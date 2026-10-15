//! A nézegető ablak: üzenetkezelés, lapozás, előtöltés, zoom/pan, teljes képernyő.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::HSTRING;

use super::placement;
use super::render::{self, Button, ButtonBar, ImageDraw, Renderer, Scene, Texture};
use super::shell;
use super::update;
use crate::core::decode::{DecodeError, MAX_VECTOR_SIDE, Target};
use crate::core::exif_info::{self, InfoLines};
use crate::core::folder::{self, Nav};
use crate::core::state::WindowState;
use crate::core::update::Release;
use crate::core::view::{Mode, View};
use crate::core::worker::{Job, Worker};

pub const WM_APP_DECODED: u32 = WM_APP + 1;
pub const WM_APP_LISTED: u32 = WM_APP + 2;
pub const WM_APP_UPDATE: u32 = WM_APP + 3;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_HIRES: usize = 1;
const TIMER_HIDE_BUTTONS: usize = 2;
const TIMER_BENCH: usize = 3;
const TIMER_NOTICE: usize = 4;

/// Az ablak azonosítója a háttérszálak számára (0, amíg nincs ablak).
pub static HWND_VAL: AtomicIsize = AtomicIsize::new(0);

pub fn post_to_window(msg: u32) {
    let h = HWND_VAL.load(Ordering::Acquire);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn install(app: App) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// A háttérszál kész, megjelenítésre előkészített eredménye.
pub struct Prepared {
    pub tex: Texture,
    pub orig_w: u32,
    pub orig_h: u32,
    pub full: bool,
    pub vector: bool,
}

struct Loaded {
    tex: Texture,
    orig_w: u32,
    orig_h: u32,
    full: bool,
    vector: bool,
    /// A cél, amivel ez a textúra készült – ugyanarra nem kérünk újra dekódolást.
    target: Target,
    /// A fájl módosítási ideje a betöltéskor: ebből látjuk, ha közben szerkesztették.
    mtime: Option<std::time::SystemTime>,
    /// A fájl megváltozott (pl. szerkesztés után): újra kell tölteni.
    stale: bool,
}

enum Slot {
    Pending,
    Ready(Loaded),
    Failed(String),
}

struct Entry {
    path: PathBuf,
    job: Option<(u64, Target)>,
    slot: Slot,
}

/// Modális művelet: az `App` kölcsönzésén kívül kell futnia, mert saját üzenethurkot pörget.
pub enum Modal {
    ConfirmDelete(PathBuf),
    Open,
    Edit(PathBuf),
    OpenUrl(String),
    /// A letöltött telepítő indítása, utána kilépés: (telepítő, újranyitandó kép).
    Install(PathBuf, Option<PathBuf>),
}

pub struct Bench {
    pub out: PathBuf,
    pub created_filetime: u64,
    pub first_image_ms: Option<f64>,
}

enum NavOp {
    Step(isize),
    First,
    Last,
}

pub struct App {
    hwnd: HWND,
    renderer: Renderer,
    worker: Worker<Prepared>,
    nav: Nav,
    entries: Vec<Entry>,
    next_job_id: u64,
    view: View,
    view_path: Option<PathBuf>,
    list_slot: Arc<Mutex<Option<(PathBuf, Vec<PathBuf>)>>>,
    listing_started: bool,
    windowed_client: (u32, u32),
    drag: Option<(i32, i32)>,
    tracking_mouse: bool,
    buttons_visible: bool,
    hover: Option<Button>,
    show_info: bool,
    info_cache: Option<(PathBuf, InfoLines)>,
    fullscreen: Option<WINDOWPLACEMENT>,
    state_path: Option<PathBuf>,
    bench: Option<Bench>,
    update_checked: bool,
    /// Elérhető újabb kiadás, és hogy telepítővel frissíthető-e (különben a kiadás oldala).
    update: Option<(Release, bool)>,
    /// Fut a telepítő letöltése.
    update_busy: bool,
    update_error: Option<String>,
    /// Az értesítés néhány másodpercig magától is látszik, utána csak a gombokkal együtt.
    notice_pinned: bool,
}

impl App {
    pub fn new(
        hwnd: HWND,
        renderer: Renderer,
        worker: Worker<Prepared>,
        path: Option<PathBuf>,
        first_job: Option<(u64, Target)>,
        client: (u32, u32),
        state_path: Option<PathBuf>,
        bench: Option<Bench>,
    ) -> App {
        let mut entries = Vec::new();
        if let (Some(p), Some(job)) = (&path, first_job) {
            entries.push(Entry { path: p.clone(), job: Some(job), slot: Slot::Pending });
        }
        App {
            hwnd,
            renderer,
            worker,
            nav: path.map(Nav::new).unwrap_or_default(),
            entries,
            next_job_id: first_job.map(|j| j.0 + 1).unwrap_or(1),
            view: View::new(client.0 as f64, client.1 as f64),
            view_path: None,
            list_slot: Arc::new(Mutex::new(None)),
            listing_started: false,
            windowed_client: client,
            drag: None,
            tracking_mouse: false,
            buttons_visible: false,
            hover: None,
            show_info: false,
            info_cache: None,
            fullscreen: None,
            state_path,
            bench,
            update_checked: false,
            update: None,
            update_busy: false,
            update_error: None,
            notice_pinned: false,
        }
    }

    fn dpi_scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd).max(96) as f32 / 96.0 }
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn set_timer(&self, id: usize, ms: u32) {
        unsafe {
            SetTimer(Some(self.hwnd), id, ms, None);
        }
    }

    fn kill_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }

    fn entry_idx(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }

    fn current_loaded(&self) -> Option<&Loaded> {
        let i = self.entry_idx(self.nav.current())?;
        match &self.entries[i].slot {
            Slot::Ready(l) => Some(l),
            _ => None,
        }
    }

    fn update_title(&self) {
        let mut title = String::new();
        if !self.nav.is_empty() {
            let name = self.nav.current().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            title.push_str(&name);
            if let Some((i, n)) = self.nav.position() {
                title.push_str(&format!("  —  {i} / {n}"));
            }
            if let Some(l) = self.current_loaded() {
                title.push_str(&format!("  —  {} × {}", l.orig_w, l.orig_h));
                if self.view.has_image() {
                    title.push_str(&format!("  —  {:.0}%", self.view.scale * 100.0));
                }
            }
            title.push_str("  —  ");
        }
        title.push_str("FastPic");
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(title));
        }
    }

    /// Kijelzőméretű dekódolás célja. Az aktuális képnél figyelembe vesszük a forgatást.
    fn display_target(&self, rotated: bool) -> Target {
        let (w, h) = (self.view.win_w.max(1.0) as u32, self.view.win_h.max(1.0) as u32);
        if rotated { Target::Fit(h, w) } else { Target::Fit(w, h) }
    }

    /// Kell-e más felbontás az aktuális képhez? Nagyobb, ha a zoom vagy az ablak nőtt;
    /// kisebb, ha a meglévő jóval nagyobb a szükségesnél (memória és rajzolási sebesség).
    /// A cél mindig a képernyőn látható méret, így a kirajzolás 1:1-es másolás marad.
    fn hires_target(&self, l: &Loaded) -> Option<Target> {
        if !self.view.has_image() {
            return None;
        }
        let (dw, dh) = self.view.displayed_size();
        let (tw, th) = (l.tex.width as f64, l.tex.height as f64);
        let upgrade = dw > tw * 1.05 || dh > th * 1.05;
        let downgrade = tw > dw * 2.0 && th > dh * 2.0;
        if (upgrade && l.full && !l.vector) || (!upgrade && !downgrade) {
            return None;
        }
        let t = if l.vector {
            if upgrade && l.tex.width.max(l.tex.height) >= MAX_VECTOR_SIDE {
                return None;
            }
            Target::Fit(dw.ceil() as u32, dh.ceil() as u32)
        } else if self.view.mode == Mode::Fit {
            self.display_target(self.view.rot % 2 == 1)
        } else if self.view.scale >= 1.0 {
            Target::Full
        } else {
            Target::Fit(dw.ceil() as u32, dh.ceil() as u32)
        };
        if t == l.target { None } else { Some(t) }
    }

    /// Összeállítja a kívánt dekódolási feladatokat: aktuális kép, nagyfelbontás, előtöltés.
    fn refresh_jobs(&mut self) {
        if self.nav.is_empty() {
            self.entries.clear();
            self.worker.set_jobs(Vec::new());
            return;
        }
        let cur = self.nav.current().to_path_buf();
        let mut wanted: Vec<(PathBuf, u8)> = vec![(cur.clone(), 0)];
        for (delta, prio) in [(1, 2), (-1, 3)] {
            if let Some(p) = self.nav.peek(delta)
                && !wanted.iter().any(|(w, _)| w == p)
            {
                wanted.push((p.to_path_buf(), prio));
            }
        }
        self.entries.retain(|e| wanted.iter().any(|(w, _)| *w == e.path));
        for (p, _) in &wanted {
            if self.entry_idx(p).is_none() {
                self.entries.push(Entry { path: p.clone(), job: None, slot: Slot::Pending });
            }
        }

        let mut jobs = Vec::new();
        for (p, prio) in &wanted {
            let i = self.entry_idx(p).unwrap();
            let is_current = *prio == 0;
            let want: Option<(Target, u8)> = match &self.entries[i].slot {
                Slot::Pending => Some((self.display_target(is_current && self.view.rot % 2 == 1), *prio)),
                // Szerkesztés után a meglévő kép látszik, amíg az új el nem készül.
                Slot::Ready(l) if is_current && l.stale => Some((self.hires_target(l).unwrap_or(l.target), 0)),
                Slot::Ready(l) if is_current => self.hires_target(l).map(|t| (t, 1)),
                _ => None,
            };
            let e = &mut self.entries[i];
            match want {
                Some((target, prio)) => {
                    // Futó feladatot nem cserélünk le, ha a célja még jó.
                    let id = match e.job {
                        Some((id, t)) if t == target || matches!(e.slot, Slot::Pending) => id,
                        _ => {
                            let id = self.next_job_id;
                            self.next_job_id += 1;
                            e.job = Some((id, target));
                            id
                        }
                    };
                    let target = e.job.unwrap().1;
                    jobs.push(Job { id, path: p.clone(), target, prio });
                }
                None => e.job = None,
            }
        }
        self.worker.set_jobs(jobs);
    }

    fn on_decoded(&mut self) {
        while let Ok((id, result)) = self.worker.results.try_recv() {
            let Some(i) = self.entries.iter().position(|e| e.job.map(|j| j.0) == Some(id)) else {
                continue;
            };
            let target = self.entries[i].job.take().unwrap().1;
            let is_current = self.entries[i].path == self.nav.current();
            match result {
                Ok(p) => {
                    let mtime = std::fs::metadata(&self.entries[i].path).and_then(|m| m.modified()).ok();
                    if let Slot::Ready(old) = &self.entries[i].slot
                        && is_current
                        && (old.orig_w, old.orig_h, old.mtime) != (p.orig_w, p.orig_h, mtime)
                    {
                        // Szerkesztés után megváltozhatott a méret: a nézetet újra beállítjuk.
                        self.view_path = None;
                        self.info_cache = None;
                    }
                    // A régi (pl. kisebb felbontású) textúra itt szabadul fel.
                    self.entries[i].slot = Slot::Ready(Loaded {
                        tex: p.tex,
                        orig_w: p.orig_w,
                        orig_h: p.orig_h,
                        full: p.full,
                        vector: p.vector,
                        target,
                        mtime,
                        stale: false,
                    });
                }
                Err(DecodeError::Cancelled) => {}
                Err(DecodeError::NotFound) if is_current && self.nav.len() > 1 => {
                    // Közben törölték vagy átnevezték: átugorjuk.
                    self.entries.remove(i);
                    self.nav.remove_current();
                    self.view_path = None;
                    self.view.clear_image();
                }
                Err(e) => match &mut self.entries[i].slot {
                    // A nagyfelbontású újradekódolás hibája: marad a meglévő kép, nem próbáljuk újra.
                    Slot::Ready(l) => l.full = true,
                    slot => *slot = Slot::Failed(e.to_string()),
                },
            }
        }
        self.sync_view();
        if !self.listing_started && !self.nav.is_empty() {
            self.listing_started = true;
            self.start_listing();
        }
        if !self.update_checked && self.bench.is_none() && self.view.has_image() {
            self.update_checked = true;
            update::spawn_check();
        }
        self.refresh_jobs();
        self.update_title();
        self.invalidate();
    }

    /// Ha az aktuális kép elkészült, és még nem ahhoz van beállítva a nézet, beállítja.
    fn sync_view(&mut self) {
        let cur = self.nav.current().to_path_buf();
        if self.view_path.as_deref() == Some(cur.as_path()) {
            return;
        }
        let ready = self.entry_idx(&cur).and_then(|i| match &self.entries[i].slot {
            Slot::Ready(l) => Some((l.orig_w, l.orig_h, l.vector)),
            _ => None,
        });
        match ready {
            Some((w, h, vector)) => {
                self.view.set_image(w, h, vector);
                self.view_path = Some(cur);
            }
            None => {
                self.view.clear_image();
                self.view_path = None;
            }
        }
    }

    fn start_listing(&mut self) {
        let Some(dir) = self.nav.current().parent().map(Path::to_path_buf) else { return };
        let slot = self.list_slot.clone();
        let _ = std::thread::Builder::new().name("lister".into()).spawn(move || {
            let files = folder::list(&dir, &shell::logical_cmp);
            *slot.lock().unwrap() = Some((dir, files));
            post_to_window(WM_APP_LISTED);
        });
    }

    fn on_listed(&mut self) {
        let Some((dir, files)) = self.list_slot.lock().unwrap().take() else { return };
        if self.nav.is_empty() || self.nav.current().parent() != Some(dir.as_path()) {
            return;
        }
        let old = self.nav.current().to_path_buf();
        self.nav.set_files(files, &shell::logical_cmp);
        let new = self.nav.current().to_path_buf();
        if old != new {
            // Csak az írásmód változott (kis/nagybetű): a meglévő dekódolást megtartjuk.
            for e in &mut self.entries {
                if e.path == old {
                    e.path = new.clone();
                }
            }
            if self.view_path.as_ref() == Some(&old) {
                self.view_path = Some(new);
            }
        }
        self.refresh_jobs();
        self.update_title();
        self.invalidate();
    }

    fn navigate(&mut self, op: NavOp) {
        let old = self.nav.current().to_path_buf();
        let moved = match op {
            NavOp::Step(d) => self.nav.step(d),
            NavOp::First => self.nav.go_to(0),
            NavOp::Last => self.nav.go_last(),
        };
        if !moved {
            return;
        }
        // Az elhagyott kép nagyfelbontású textúráját eldobjuk; ha kell, kijelzőméretben
        // újra betöltődik előtöltésként.
        let side = self.view.win_w.max(self.view.win_h) * 1.05;
        if let Some(i) = self.entry_idx(&old)
            && let Slot::Ready(l) = &self.entries[i].slot
            && (l.tex.width as f64 > side || l.tex.height as f64 > side)
        {
            self.entries[i].slot = Slot::Pending;
        }
        self.after_current_changed();
    }

    fn after_current_changed(&mut self) {
        self.view_path = None;
        self.info_cache = None;
        self.sync_view();
        self.refresh_jobs();
        self.update_title();
        self.invalidate();
    }

    /// Első kép ebben az ablakban (üres ablakba megnyitás, fogd és vidd, Ctrl+O).
    fn open_here(&mut self, path: PathBuf) {
        self.nav = Nav::new(path);
        self.entries.clear();
        self.listing_started = false;
        self.after_current_changed();
    }

    /// Minden kép külön processt kap: ha már van kép az ablakban, új példányt indítunk.
    pub fn open_or_spawn(&mut self, path: PathBuf) {
        let Some(path) = resolve_openable(path) else { return };
        if self.nav.is_empty() {
            self.open_here(path);
        } else {
            shell::spawn_viewer(&path);
        }
    }

    pub fn current_dir(&self) -> Option<PathBuf> {
        self.nav.current().parent().map(Path::to_path_buf)
    }

    pub fn after_delete(&mut self, path: &Path) {
        if self.nav.current() != path {
            return;
        }
        if let Some(i) = self.entry_idx(path) {
            self.entries.remove(i);
        }
        if !self.nav.remove_current() {
            self.nav = Nav::default();
            self.entries.clear();
        }
        self.after_current_changed();
    }

    fn pin_notice(&mut self) {
        self.notice_pinned = true;
        self.set_timer(TIMER_NOTICE, 10_000);
        self.invalidate();
    }

    fn on_update(&mut self) -> Option<Modal> {
        match update::take_event()? {
            update::Event::Available(r) => {
                let installable = r.installer.is_some() && update::is_installed();
                self.update = Some((r, installable));
                self.pin_notice();
            }
            update::Event::Ready(setup) => {
                let reopen = (!self.nav.is_empty()).then(|| self.nav.current().to_path_buf());
                return Some(Modal::Install(setup, reopen));
            }
            update::Event::Failed(e) => self.update_failed(e),
        }
        None
    }

    pub fn update_failed(&mut self, e: String) {
        self.update_busy = false;
        self.update_error = Some(format!("A frissítés nem sikerült: {e}"));
        self.pin_notice();
    }

    /// U: telepített példánynál letöltés és telepítés, különben a kiadás oldala.
    fn start_update(&mut self) -> Option<Modal> {
        let (r, installable) = self.update.as_ref()?;
        if self.update_busy {
            return None;
        }
        match (&r.installer, installable) {
            (Some(asset), true) => {
                update::spawn_download(asset.clone());
                self.update_busy = true;
                self.update_error = None;
                self.invalidate();
                None
            }
            _ => Some(Modal::OpenUrl(r.page.clone())),
        }
    }

    fn notice_text(&self) -> Option<String> {
        if self.notice_pinned
            && let Some(e) = &self.update_error
        {
            return Some(e.clone());
        }
        let (r, installable) = self.update.as_ref()?;
        if self.update_busy {
            return Some(format!("A FastPic {} letöltése…", r.version));
        }
        if !self.notice_pinned && !self.buttons_visible {
            return None;
        }
        let action = if *installable { "frissítés" } else { "letöltési oldal" };
        Some(format!("Elérhető a FastPic {}   ·   U: {action}", r.version))
    }

    fn ensure_info(&mut self) {
        let cur = self.nav.current().to_path_buf();
        if self.info_cache.as_ref().is_some_and(|(p, _)| *p == cur) {
            return;
        }
        let dims = self.current_loaded().map(|l| (l.orig_w, l.orig_h));
        let modified = std::fs::metadata(&cur).ok().and_then(|m| {
            use std::os::windows::fs::MetadataExt;
            shell::format_filetime(m.last_write_time())
        });
        let lines = exif_info::read(&cur, dims, modified);
        self.info_cache = Some((cur, lines));
    }

    fn paint(&mut self) {
        super::mark("paint");
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if self.show_info && !self.nav.is_empty() {
            self.ensure_info();
        }
        let s = self.dpi_scale();
        let cur = self.nav.current().to_path_buf();
        let entry = self.entries.iter().find(|e| e.path == cur);

        let mut failed_msg = None;
        let image = match entry.map(|e| &e.slot) {
            Some(Slot::Ready(l)) if self.view.has_image() => {
                Some(ImageDraw { tex: &l.tex, matrix: self.view.matrix(), orig_w: l.orig_w, orig_h: l.orig_h })
            }
            Some(Slot::Failed(msg)) => {
                let name = cur.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                failed_msg = Some(format!("Nem sikerült megnyitni:\n{name}\n\n{msg}"));
                None
            }
            _ => None,
        };
        let has_image = image.is_some();
        let notice = self.notice_text();
        let message = if self.nav.is_empty() { Some(EMPTY_HELP) } else { failed_msg.as_deref() };
        let scene = Scene {
            image,
            message,
            buttons: if self.buttons_visible { self.button_bar() } else { None },
            info: if self.show_info { self.info_cache.as_ref().map(|(_, l)| l) } else { None },
            notice: notice.as_deref(),
            dpi_scale: s,
        };
        let (w, h) = render::client_size(self.hwnd);
        self.renderer.draw(hdc, w, h, &scene);
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
        if has_image {
            self.bench_first_image();
        }
    }

    fn bench_first_image(&mut self) {
        let Some(b) = &mut self.bench else { return };
        if b.first_image_ms.is_some() {
            return;
        }
        let now = super::now_filetime();
        b.first_image_ms = Some(now.saturating_sub(b.created_filetime) as f64 / 10_000.0);
        // Megvárjuk, hogy az előtöltés is lefusson, és csak utána mérünk memóriát.
        self.set_timer(TIMER_BENCH, 2000);
    }

    fn bench_finish(&mut self) {
        self.kill_timer(TIMER_BENCH);
        if let Some(b) = &self.bench {
            let (ws, peak, private) = super::memory_usage();
            let report = format!(
                "first_image_ms={:.1}\nworking_set_mb={:.1}\npeak_working_set_mb={:.1}\nprivate_mb={:.1}\n{}",
                b.first_image_ms.unwrap_or(-1.0),
                ws as f64 / 1048576.0,
                peak as f64 / 1048576.0,
                private as f64 / 1048576.0,
                super::marks_report(b.created_filetime)
            );
            let _ = std::fs::write(&b.out, report);
        }
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    fn save_state(&self) {
        if self.bench.is_some() {
            return;
        }
        let Some(path) = &self.state_path else { return };
        let wp = match &self.fullscreen {
            Some(saved) => Some(*saved),
            None => placement::get(self.hwnd),
        };
        if let Some(wp) = wp {
            let _ = placement::to_state(&wp, self.windowed_client).save(path);
        }
    }

    fn toggle_fullscreen(&mut self) {
        match self.fullscreen.take() {
            Some(saved) => placement::exit_fullscreen(self.hwnd, &saved),
            None => self.fullscreen = placement::enter_fullscreen(self.hwnd),
        }
    }

    fn after_zoom(&mut self) {
        self.update_title();
        self.invalidate();
        self.set_timer(TIMER_HIRES, 150);
    }

    fn zoom_center(&mut self, factor: f64) {
        let (cx, cy) = (self.view.win_w / 2.0, self.view.win_h / 2.0);
        self.view.zoom_at(factor, cx, cy);
        self.after_zoom();
    }

    fn pannable(&self) -> bool {
        self.view.mode == Mode::Manual && self.view.can_pan()
    }

    fn key_down(&mut self, vk: u16) -> Option<Modal> {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let step_x = self.view.win_w * 0.15;
        let step_y = self.view.win_h * 0.15;
        match VIRTUAL_KEY(vk) {
            VK_LEFT if self.pannable() => self.pan(step_x, 0.0),
            VK_RIGHT if self.pannable() => self.pan(-step_x, 0.0),
            VK_UP if self.pannable() => self.pan(0.0, step_y),
            VK_DOWN if self.pannable() => self.pan(0.0, -step_y),
            VK_LEFT | VK_PRIOR | VK_BACK => self.navigate(NavOp::Step(-1)),
            VK_RIGHT | VK_NEXT | VK_SPACE => self.navigate(NavOp::Step(1)),
            VK_HOME => self.navigate(NavOp::First),
            VK_END => self.navigate(NavOp::Last),
            VK_ADD | VK_OEM_PLUS => self.zoom_center(1.25),
            VK_SUBTRACT | VK_OEM_MINUS => self.zoom_center(0.8),
            VK_0 | VK_NUMPAD0 => {
                self.view.fit();
                self.after_zoom();
            }
            VK_1 | VK_NUMPAD1 => {
                self.view.set_scale(1.0);
                self.after_zoom();
            }
            VK_F11 | VK_RETURN => self.toggle_fullscreen(),
            VK_ESCAPE => {
                if self.fullscreen.is_some() {
                    self.toggle_fullscreen();
                } else {
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            VK_R if self.view.has_image() => {
                self.view.rotate(!shift);
                self.after_zoom();
                // A forgatás megváltoztatja a kijelzőméretű dekódolás célját.
                self.refresh_jobs();
            }
            VK_U => return self.start_update(),
            VK_I => {
                self.show_info = !self.show_info;
                self.invalidate();
            }
            VK_E if !self.nav.is_empty() => return Some(Modal::Edit(self.nav.current().to_path_buf())),
            VK_DELETE if !self.nav.is_empty() => return Some(Modal::ConfirmDelete(self.nav.current().to_path_buf())),
            VK_O if ctrl => return Some(Modal::Open),
            VK_W if ctrl => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            _ => {}
        }
        None
    }

    fn pan(&mut self, dx: f64, dy: f64) {
        self.view.pan(dx, dy);
        self.invalidate();
    }

    fn client_xy(lp: LPARAM) -> (i32, i32) {
        ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
    }

    /// A megjeleníthető overlay gombok (az egér pozíciójától függetlenül).
    fn button_bar(&self) -> Option<ButtonBar> {
        if self.nav.is_empty() {
            return None;
        }
        Some(ButtonBar { nav: self.nav.len() > 1, edit: true, hover: self.hover })
    }

    fn hit(&self, x: i32, y: i32) -> Option<Button> {
        let bar = self.button_bar()?;
        render::hit_button(x as f32, y as f32, self.view.win_w as f32, self.view.win_h as f32, self.dpi_scale(), &bar)
    }

    /// Visszatéréskor: ha az aktuális képet közben módosították (pl. szerkesztőben), újratöltjük.
    fn check_current_modified(&mut self) {
        let Some(i) = self.entry_idx(self.nav.current()) else { return };
        let mtime = std::fs::metadata(&self.entries[i].path).and_then(|m| m.modified()).ok();
        if let Slot::Ready(l) = &mut self.entries[i].slot
            && mtime.is_some()
            && l.mtime != mtime
        {
            l.stale = true;
            self.refresh_jobs();
        }
    }

    fn mouse_move(&mut self, x: i32, y: i32) {
        if let Some((lx, ly)) = self.drag {
            self.drag = Some((x, y));
            self.pan((x - lx) as f64, (y - ly) as f64);
        }
        if !self.tracking_mouse {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking_mouse = true;
        }
        let hover = self.hit(x, y);
        if !self.buttons_visible || hover != self.hover {
            self.buttons_visible = true;
            self.hover = hover;
            self.invalidate();
        }
        self.set_timer(TIMER_HIDE_BUTTONS, 1500);
    }

    fn button_click(&mut self, b: Button) -> Option<Modal> {
        match b {
            Button::Prev => self.navigate(NavOp::Step(-1)),
            Button::Next => self.navigate(NavOp::Step(1)),
            Button::Edit => return Some(Modal::Edit(self.nav.current().to_path_buf())),
        }
        None
    }

    fn drop_files(&mut self, hdrop: HDROP) {
        let mut paths = Vec::new();
        unsafe {
            let n = DragQueryFileW(hdrop, u32::MAX, None);
            for i in 0..n {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                buf.truncate(len);
                use std::os::windows::ffi::OsStringExt;
                paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
            }
            DragFinish(hdrop);
        }
        for p in paths {
            self.open_or_spawn(p);
        }
    }

    fn set_cursor(&self) -> bool {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        let id = if self.hit(pt.x, pt.y).is_some() {
            IDC_HAND
        } else if self.pannable() {
            IDC_SIZEALL
        } else {
            IDC_ARROW
        };
        unsafe {
            if let Ok(c) = LoadCursorW(None, id) {
                SetCursor(Some(c));
            }
        }
        true
    }

    /// `None` → DefWindowProc.
    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> (Option<LRESULT>, Option<Modal>) {
        let done = (Some(LRESULT(0)), None);
        match msg {
            WM_PAINT => {
                self.paint();
                done
            }
            WM_ERASEBKGND => (Some(LRESULT(1)), None),
            WM_SIZE => {
                if wp.0 as u32 != SIZE_MINIMIZED {
                    let (w, h) = ((lp.0 & 0xFFFF) as u32, ((lp.0 >> 16) & 0xFFFF) as u32);
                    if w > 0 && h > 0 {
                        if self.fullscreen.is_none() {
                            self.windowed_client = (w, h);
                        }
                        self.view.set_window(w as f64, h as f64);
                        self.update_title();
                        self.set_timer(TIMER_HIRES, 200);
                        self.invalidate();
                    }
                }
                done
            }
            WM_DPICHANGED => {
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                done
            }
            WM_GETMINMAXINFO => {
                let mmi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                mmi.ptMinTrackSize = POINT { x: 320, y: 240 };
                done
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_HIRES => {
                        self.kill_timer(TIMER_HIRES);
                        self.refresh_jobs();
                    }
                    TIMER_HIDE_BUTTONS if self.hover.is_none() && self.drag.is_none() => {
                        self.kill_timer(TIMER_HIDE_BUTTONS);
                        self.buttons_visible = false;
                        self.invalidate();
                    }
                    TIMER_BENCH => self.bench_finish(),
                    TIMER_NOTICE => {
                        self.kill_timer(TIMER_NOTICE);
                        self.notice_pinned = false;
                        self.update_error = None;
                        self.invalidate();
                    }
                    _ => {}
                }
                done
            }
            WM_APP_DECODED => {
                self.on_decoded();
                done
            }
            WM_APP_LISTED => {
                self.on_listed();
                done
            }
            WM_APP_UPDATE => (Some(LRESULT(0)), self.on_update()),
            WM_ACTIVATEAPP => {
                // Visszatéréskor frissítjük a mappalistát (új, törölt, átnevezett fájlok),
                // és újratöltjük az aktuális képet, ha közben szerkesztették.
                if wp.0 != 0 && self.listing_started && !self.nav.is_empty() {
                    self.start_listing();
                    self.check_current_modified();
                }
                (None, None)
            }
            WM_KEYDOWN => (Some(LRESULT(0)), self.key_down(wp.0 as u16)),
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f64;
                let (sx, sy) = Self::client_xy(lp);
                let mut pt = POINT { x: sx, y: sy };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                self.view.zoom_at(1.2f64.powf(delta / 120.0), pt.x as f64, pt.y as f64);
                self.after_zoom();
                done
            }
            WM_MOUSEMOVE => {
                let (x, y) = Self::client_xy(lp);
                self.mouse_move(x, y);
                done
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                if self.drag.is_none() && (self.buttons_visible || self.hover.is_some()) {
                    self.buttons_visible = false;
                    self.hover = None;
                    self.invalidate();
                }
                done
            }
            WM_LBUTTONDOWN => {
                let (x, y) = Self::client_xy(lp);
                if let Some(b) = self.hit(x, y) {
                    return (Some(LRESULT(0)), self.button_click(b));
                } else if self.pannable() {
                    self.drag = Some((x, y));
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                done
            }
            WM_LBUTTONUP => {
                if self.drag.take().is_some() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                done
            }
            WM_CAPTURECHANGED => {
                self.drag = None;
                done
            }
            WM_LBUTTONDBLCLK => {
                let (x, y) = Self::client_xy(lp);
                match self.hit(x, y) {
                    // A szerkesztő gombra adott dupla kattintás ne nyissa meg kétszer.
                    Some(Button::Edit) => {}
                    Some(b) => return (Some(LRESULT(0)), self.button_click(b)),
                    None => self.toggle_fullscreen(),
                }
                done
            }
            WM_XBUTTONDOWN => {
                let which = ((wp.0 >> 16) & 0xFFFF) as u16;
                self.navigate(NavOp::Step(if which == XBUTTON1 { -1 } else { 1 }));
                (Some(LRESULT(1)), None)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                self.set_cursor();
                (Some(LRESULT(1)), None)
            }
            WM_DROPFILES => {
                self.drop_files(HDROP(wp.0 as *mut _));
                done
            }
            WM_DESTROY => {
                self.save_state();
                unsafe { PostQuitMessage(0) };
                done
            }
            _ => (None, None),
        }
    }
}

const EMPTY_HELP: &str = "Nyiss meg egy képet (Ctrl+O), vagy húzd ide a fájlt.\n\n\
    ← →  lapozás   ·   görgő  zoom   ·   0  ablakhoz   ·   1  100%\n\
    F11 / dupla kattintás  teljes képernyő   ·   R  forgatás   ·   I  infó   ·   E  szerkesztés   ·   Del  törlés";

/// Mappát is elfogad: ilyenkor a mappa első képét nyitja meg.
pub fn resolve_openable(path: PathBuf) -> Option<PathBuf> {
    if path.is_dir() {
        return folder::list(&path, &shell::logical_cmp).into_iter().next();
    }
    Some(path)
}

pub fn run_modal(hwnd: HWND, modal: Modal) {
    match modal {
        Modal::ConfirmDelete(path) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !shell::confirm(hwnd, &format!("Biztosan a Lomtárba helyezed?\n\n{name}"), "Törlés") {
                return;
            }
            match shell::recycle(hwnd, &path) {
                Ok(()) => {
                    with_app(|a| a.after_delete(&path));
                }
                Err(e) => shell::message(Some(hwnd), &format!("Nem sikerült törölni:\n{}", e.message()), true),
            }
        }
        Modal::Edit(path) => {
            if !shell::open_in_editor(hwnd, &path) {
                shell::message(Some(hwnd), "Nem található képszerkesztő alkalmazás ehhez a fájltípushoz.", true);
            }
        }
        Modal::OpenUrl(url) => shell::open_url(&url),
        Modal::Install(setup, reopen) => {
            if shell::launch(&setup, &update::installer_params(reopen.as_deref())) {
                // A telepítő lecseréli az exe-t: kilépünk, a végén ő nyitja újra a képet.
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            } else {
                with_app(|a| a.update_failed("a telepítő nem indult el".into()));
            }
        }
        Modal::Open => {
            let dir = with_app(|a| a.current_dir()).flatten();
            if let Some(p) = shell::open_dialog(hwnd, dir.as_deref()) {
                with_app(|a| a.open_or_spawn(p));
            }
        }
    }
}

pub extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // A modális párbeszédek (MessageBox, fájlválasztó) az App kölcsönzésén kívül futnak,
    // így közben is kiszolgálható minden üzenet. Az ablak létrehozása alatti üzenetek
    // (még nincs App) a rendszer alapkezelésére mennek.
    match with_app(|app| app.handle(msg, wp, lp)) {
        Some((result, modal)) => {
            if let Some(m) = modal {
                run_modal(hwnd, m);
            }
            match result {
                Some(r) => r,
                None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
        }
        None => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

pub fn initial_state(state_path: Option<&Path>) -> Option<WindowState> {
    let s = WindowState::load(state_path?)?;
    placement::is_on_screen(&s).then_some(s)
}
