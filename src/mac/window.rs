//! A nézegető ablak és nézete: eseménykezelés, lapozás, előtöltés, zoom/pan, teljes képernyő.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSEvent,
    NSEventModifierFlags, NSPasteboardTypeFileURL, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowDelegate,
};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSString, NSTimer, NSURL};

use super::placement;
use super::render::{self, Button, ButtonBar, ImageDraw, Renderer, Scene, Texture};
use super::shell;
use crate::core::decode::{DecodeError, MAX_VECTOR_SIDE, Target};
use crate::core::exif_info::{self, InfoLines};
use crate::core::folder::{self, Nav};
use crate::core::view::{Mode, View};
use crate::core::worker::{Job, Worker};

// Az AppKit funkcióbillentyűi (NSUpArrowFunctionKey és társai).
const K_UP: u16 = 0xF700;
const K_DOWN: u16 = 0xF701;
const K_LEFT: u16 = 0xF702;
const K_RIGHT: u16 = 0xF703;
const K_DELETE: u16 = 0xF728;
const K_HOME: u16 = 0xF729;
const K_END: u16 = 0xF72B;
const K_PAGEUP: u16 = 0xF72C;
const K_PAGEDOWN: u16 = 0xF72D;
const K_F11: u16 = 0xF70E;
const K_ESC: u16 = 0x1B;
const K_RETURN: u16 = 0x0D;
const K_ENTER: u16 = 0x03;
const K_BACKSPACE: u16 = 0x7F;
const K_SPACE: u16 = b' ' as u16;
const K_PLUS: u16 = b'+' as u16;
const K_EQUALS: u16 = b'=' as u16;
const K_MINUS: u16 = b'-' as u16;
const K_UNDERSCORE: u16 = b'_' as u16;
const K_0: u16 = b'0' as u16;
const K_1: u16 = b'1' as u16;
const K_R: u16 = b'r' as u16;
const K_I: u16 = b'i' as u16;
const K_E: u16 = b'e' as u16;
const K_O: u16 = b'o' as u16;
const K_W: u16 = b'w' as u16;

const TIMER_HIRES: usize = 0;
const TIMER_HIDE_BUTTONS: usize = 1;
const TIMER_BENCH: usize = 2;

unsafe extern "C" {
    static _dispatch_main_q: [u8; 0];
    fn dispatch_async_f(queue: *mut c_void, context: *mut c_void, work: extern "C-unwind" fn(*mut c_void));
}

fn on_main(work: extern "C-unwind" fn(*mut c_void)) {
    unsafe { dispatch_async_f(&raw const _dispatch_main_q as *mut c_void, std::ptr::null_mut(), work) }
}

extern "C-unwind" fn decoded_on_main(_: *mut c_void) {
    super::mark("main_q");
    with_app(|a| a.on_decoded());
}

extern "C-unwind" fn listed_on_main(_: *mut c_void) {
    with_app(|a| a.on_listed());
}

/// A dekódoló szál értesítése: az eredményt a fő szálon dolgozzuk fel.
pub fn post_decoded() {
    on_main(decoded_on_main);
}

/// Ugyanez, de azonnal, a fő szálon. Induláskor kell: a fő futási hurok csak az
/// `NSApplication::run`-ban indul el, addig a `dispatch_async` munkája állna –
/// az `run` belső inicializálása viszont ~45 ms, ennyit várna a kész kép.
pub fn drain_decoded_now() {
    decoded_on_main(std::ptr::null_mut());
}

pub fn post_listed() {
    on_main(listed_on_main);
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn install(app: App) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

/// Az indításkor a parancssorból megnyitott kép, és hogy az indulás lezárult-e.
///
/// Az AppKit a parancssori argumentumot az indulás során `application:openFile:`-ként
/// is átadja. Mivel a képet ilyenkor már megnyitottuk, a „minden további kép külön
/// process” szabály új példányt indítana – az új példány pedig ugyanígy járna el:
/// végtelen processlánc. Ezért az induláskor érkező, az argumentummal azonos
/// megnyitást elnyeljük.
static LAUNCH_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static LAUNCH_DONE: AtomicBool = AtomicBool::new(false);

pub fn set_launch_path(path: Option<PathBuf>) {
    *LAUNCH_PATH.lock().unwrap() = path;
}

/// Ugyanaz a fájl? A parancssori út lehet relatív, az AppKité mindig abszolút.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::path::absolute(a), std::path::absolute(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Az induláskor visszakapott saját argumentum (nem valódi megnyitási kérés).
fn is_launch_echo(path: &Path) -> bool {
    !LAUNCH_DONE.load(Ordering::Relaxed) && LAUNCH_PATH.lock().unwrap().as_deref().is_some_and(|p| same_file(p, path))
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// Az előnézet ennyiedére kicsinyített oldalhosszal dekódol. JPEG-nél így az ImageIO
/// 1/4-es DCT-skálázással dolgozik: a végleges kép töredéke alatt kész.
const PREVIEW_DIV: u32 = 4;
/// Ez alatti célméretnél a teljes dekódolás is gyors, nincs értelme előnézetnek.
const PREVIEW_MIN_SIDE: u32 = 1200;
/// Kis fájlnál a dekódolás amúgy is gyors: a dupla munka többe kerülne, mint amennyit ér.
const PREVIEW_MIN_BYTES: u64 = 400 * 1024;

/// A gyorsan megjelenő előnézet célmérete, ha van értelme előnézetet kérni.
pub fn preview_target(path: &Path, final_target: Target) -> Option<Target> {
    // Méréshez: ugyanazzal a binárissal az előnézet nélküli út is végigmérhető.
    if std::env::var_os("FASTPIC_NO_PREVIEW").is_some() {
        return None;
    }
    let Target::Fit(w, h) = final_target else { return None };
    if w.max(h) < PREVIEW_MIN_SIDE {
        return None;
    }
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) < PREVIEW_MIN_BYTES {
        return None;
    }
    Some(Target::Fit((w / PREVIEW_DIV).max(1), (h / PREVIEW_DIV).max(1)))
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
    stale: bool,
    /// Gyors előnézet: a végleges felbontás még úton van.
    preview: bool,
}

enum Slot {
    Pending,
    Ready(Loaded),
    Failed(String),
}

struct Entry {
    path: PathBuf,
    job: Option<(u64, Target)>,
    /// Az előnézet mögé előre beütemezett, végleges felbontású feladat: már a sorban áll,
    /// amíg az ablak felépül, így a dekódoló szál nem áll meg az előnézet után.
    queued: Option<(u64, Target)>,
    slot: Slot,
}

/// Modális művelet: az `App` kölcsönzésén kívül kell futnia, mert saját eseményhurkot pörget.
pub enum Modal {
    ConfirmDelete(PathBuf),
    Open,
    Edit(PathBuf),
}

pub struct Bench {
    pub out: PathBuf,
    pub started: std::time::Instant,
    /// Az első látható kép (akár előnézet).
    pub preview_ms: Option<f64>,
    /// Az első végleges felbontású kép.
    pub first_image_ms: Option<f64>,
}

enum NavOp {
    Step(isize),
    First,
    Last,
}

pub struct App {
    window: Retained<NSWindow>,
    nsview: Retained<FastPicView>,
    renderer: Renderer,
    worker: Worker<Prepared>,
    nav: Nav,
    entries: Vec<Entry>,
    next_job_id: u64,
    /// Az utoljára előnézetként kiadott feladat azonosítója.
    preview_job: Option<u64>,
    view: View,
    view_path: Option<PathBuf>,
    list_slot: Arc<Mutex<Option<(PathBuf, Vec<PathBuf>)>>>,
    listing_started: bool,
    windowed_client: (u32, u32),
    drag: Option<(f64, f64)>,
    buttons_visible: bool,
    hover: Option<Button>,
    show_info: bool,
    info_cache: Option<(PathBuf, InfoLines)>,
    fullscreen: bool,
    state_path: Option<PathBuf>,
    bench: Option<Bench>,
    timers: [Option<Retained<NSTimer>>; 3],
    /// Retina-szorzó: ennyivel nagyobb pixelméretre dekódolunk, mint a pontban mért ablak.
    backing: f64,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        window: Retained<NSWindow>,
        nsview: Retained<FastPicView>,
        renderer: Renderer,
        worker: Worker<Prepared>,
        path: Option<PathBuf>,
        first_job: Option<(u64, Target)>,
        preview_job: Option<u64>,
        queued_job: Option<(u64, Target)>,
        client: (f64, f64),
        backing: f64,
        state_path: Option<PathBuf>,
        bench: Option<Bench>,
    ) -> App {
        let mut entries = Vec::new();
        if let (Some(p), Some(job)) = (&path, first_job) {
            entries.push(Entry { path: p.clone(), job: Some(job), queued: queued_job, slot: Slot::Pending });
        }
        App {
            window,
            nsview,
            renderer,
            worker,
            nav: path.map(Nav::new).unwrap_or_default(),
            entries,
            next_job_id: queued_job.or(first_job).map(|j| j.0 + 1).unwrap_or(1),
            preview_job,
            view: View::new(client.0, client.1),
            view_path: None,
            list_slot: Arc::new(Mutex::new(None)),
            listing_started: false,
            windowed_client: ((client.0 * backing) as u32, (client.1 * backing) as u32),
            drag: None,
            buttons_visible: false,
            hover: None,
            show_info: false,
            info_cache: None,
            fullscreen: false,
            state_path,
            bench,
            timers: [None, None, None],
            backing,
        }
    }

    fn set_timer(&mut self, id: usize, ms: u64) {
        if let Some(t) = self.timers[id].take() {
            t.invalidate();
        }
        let sel = match id {
            TIMER_HIRES => sel!(onHiresTimer:),
            TIMER_HIDE_BUTTONS => sel!(onHideButtonsTimer:),
            _ => sel!(onBenchTimer:),
        };
        let t = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                ms as f64 / 1000.0,
                &*self.nsview,
                sel,
                None,
                false,
            )
        };
        self.timers[id] = Some(t);
    }

    fn kill_timer(&mut self, id: usize) {
        if let Some(t) = self.timers[id].take() {
            t.invalidate();
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
        self.window.setTitle(&NSString::from_str(&title));
        // A címsorban a fájl ikonja és a jobb kattintásos útvonalmenü is megjelenik.
        if !self.nav.is_empty() {
            self.window.setRepresentedFilename(&NSString::from_str(&self.nav.current().to_string_lossy()));
        }
    }

    /// Kijelzőméretű (pixel) dekódolás célja. Az aktuális képnél a forgatást is figyelembe vesszük.
    fn display_target(&self, rotated: bool) -> Target {
        let w = (self.view.win_w.max(1.0) * self.backing).round() as u32;
        let h = (self.view.win_h.max(1.0) * self.backing).round() as u32;
        if rotated { Target::Fit(h, w) } else { Target::Fit(w, h) }
    }

    /// Kell-e más felbontás az aktuális képhez? A textúra mérete pixelben értendő,
    /// a nézeté pontban – ezért a `backing` szorzó.
    fn hires_target(&self, l: &Loaded) -> Option<Target> {
        if !self.view.has_image() {
            return None;
        }
        let (dw, dh) = self.view.displayed_size();
        let (dw, dh) = (dw * self.backing, dh * self.backing);
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
        } else if self.view.scale * self.backing >= 1.0 {
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
                self.entries.push(Entry { path: p.clone(), job: None, queued: None, slot: Slot::Pending });
            }
        }

        let mut jobs = Vec::new();
        for (p, prio) in &wanted {
            let i = self.entry_idx(p).unwrap();
            let is_current = *prio == 0;
            let want: Option<(Target, u8, bool)> = match &self.entries[i].slot {
                Slot::Pending => {
                    let t = self.display_target(is_current && self.view.rot % 2 == 1);
                    // Az aktuális képnél előbb egy gyors előnézet megy ki; a végleges
                    // felbontást a `hires_target` kéri, amint ez megjelent.
                    match if is_current { preview_target(p, t) } else { None } {
                        Some(pt) => Some((pt, *prio, true)),
                        None => Some((t, *prio, false)),
                    }
                }
                // Szerkesztés után a meglévő kép látszik, amíg az új el nem készül.
                Slot::Ready(l) if is_current && l.stale => Some((self.hires_target(l).unwrap_or(l.target), 0, false)),
                // Előnézet után a végleges felbontás sürgős: nem várhat az előtöltésre.
                Slot::Ready(l) if is_current => {
                    self.hires_target(l).map(|t| (t, if l.preview { 0 } else { 1 }, false))
                }
                _ => None,
            };
            let e = &mut self.entries[i];
            // Ha még az előnézet előtt lapoztak, az előre beütemezett feladat okafogyott.
            if !is_current {
                e.queued = None;
            }
            match want {
                Some((target, prio, is_preview)) => {
                    // Futó feladatot nem cserélünk le, ha a célja még jó.
                    let id = match e.job {
                        Some((id, t)) if t == target || matches!(e.slot, Slot::Pending) => id,
                        _ => {
                            let id = self.next_job_id;
                            self.next_job_id += 1;
                            e.job = Some((id, target));
                            if is_preview {
                                self.preview_job = Some(id);
                            }
                            id
                        }
                    };
                    let target = e.job.unwrap().1;
                    jobs.push(Job { id, path: p.clone(), target, prio });
                    // Az előre beütemezett végleges felbontás az előnézet mögött marad a sorban.
                    if let Some((qid, qt)) = e.queued {
                        jobs.push(Job { id: qid, path: p.clone(), target: qt, prio: 1 });
                    }
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
            let was_preview = self.preview_job == Some(id);
            // Az előnézet helyére a mögé beütemezett, végleges felbontású feladat lép:
            // az már fut (vagy a sorban áll), az eredményét ehhez az entryhez kell kötni.
            if was_preview {
                self.entries[i].job = self.entries[i].queued.take();
            }
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
                        preview: was_preview,
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
                    // A nagyfelbontású újradekódolás hibája: marad a meglévő kép.
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
        self.refresh_jobs();
        self.update_title();
        self.render();
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
            post_listed();
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
        self.render();
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
        let side = self.view.win_w.max(self.view.win_h) * self.backing * 1.05;
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
        self.render();
    }

    /// Első kép ebben az ablakban (üres ablakba megnyitás, fogd és vidd, ⌘O).
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
            return;
        }
        // Második fék az `is_launch_echo` mellett: az indulás első pillanataiban a már
        // látható képre érkező megnyitás csak az AppKit visszhangja lehet.
        if super::uptime_ms() < 2000.0 && same_file(&path, self.nav.current()) {
            return;
        }
        shell::spawn_viewer(&path);
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

    fn ensure_info(&mut self) {
        let cur = self.nav.current().to_path_buf();
        if self.info_cache.as_ref().is_some_and(|(p, _)| *p == cur) {
            return;
        }
        let dims = self.current_loaded().map(|l| (l.orig_w, l.orig_h));
        let modified = std::fs::metadata(&cur).and_then(|m| m.modified()).ok().and_then(shell::format_time);
        let lines = exif_info::read(&cur, dims, modified);
        self.info_cache = Some((cur, lines));
    }

    /// A rétegek frissítése. A Windows-verzió `WM_PAINT`-jének megfelelője, de itt nincs
    /// rajzolási ciklus: csak a réteg-tulajdonságokat állítjuk.
    fn render(&mut self) {
        super::mark("render");
        if self.show_info && !self.nav.is_empty() {
            self.ensure_info();
        }
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
        let showing_preview = matches!(entry.map(|e| &e.slot), Some(Slot::Ready(l)) if l.preview);
        let message = if self.nav.is_empty() { Some(EMPTY_HELP) } else { failed_msg.as_deref() };
        let scene = Scene {
            image,
            message,
            buttons: if self.buttons_visible { self.button_bar() } else { None },
            info: if self.show_info { self.info_cache.as_ref().map(|(_, l)| l) } else { None },
            width: self.view.win_w,
            height: self.view.win_h,
        };
        self.renderer.draw(&scene);
        if has_image {
            self.bench_image_shown(showing_preview);
        }
    }

    /// Bench: az első látható kép és az első végleges felbontású kép ideje.
    fn bench_image_shown(&mut self, preview: bool) {
        let Some(b) = &mut self.bench else { return };
        let now = b.started.elapsed().as_secs_f64() * 1000.0;
        if b.preview_ms.is_none() {
            b.preview_ms = Some(now);
        }
        if preview {
            // Ha a végleges kép valamiért nem érkezik meg, ne ragadjon be a mérés.
            if b.first_image_ms.is_none() {
                self.set_timer(TIMER_BENCH, 3000);
            }
            return;
        }
        if b.first_image_ms.is_some() {
            return;
        }
        b.first_image_ms = Some(now);
        // Megvárjuk, hogy az előtöltés is lefusson, és csak utána mérünk memóriát.
        self.set_timer(TIMER_BENCH, 2000);
    }

    fn bench_finish(&mut self) {
        self.kill_timer(TIMER_BENCH);
        if let Some(b) = &self.bench {
            let (ws, peak, footprint) = super::memory_usage();
            let report = format!(
                "first_image_ms={:.1}\npreview_ms={:.1}\nworking_set_mb={:.1}\npeak_working_set_mb={:.1}\nprivate_mb={:.1}\n{}",
                b.first_image_ms.unwrap_or(-1.0),
                b.preview_ms.unwrap_or(-1.0),
                ws as f64 / 1048576.0,
                peak as f64 / 1048576.0,
                footprint as f64 / 1048576.0,
                super::marks_report()
            );
            let _ = std::fs::write(&b.out, report);
        }
        std::process::exit(0);
    }

    pub fn save_state(&self) {
        if self.bench.is_some() {
            return;
        }
        let Some(path) = &self.state_path else { return };
        // Teljes képernyőn a belépés előtti ablakállapot a mentendő; onnan tér vissza.
        if self.fullscreen {
            return;
        }
        let _ = placement::to_state(&self.window, self.windowed_client).save(path);
    }

    fn toggle_fullscreen(&mut self) {
        self.window.toggleFullScreen(None);
    }

    fn after_zoom(&mut self) {
        self.update_title();
        self.render();
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

    fn key_down(&mut self, key: u16, cmd: bool, shift: bool) -> Option<Modal> {
        // A betűket kisbetűssé normalizáljuk (Shift+R is „r”-ként érkezik be).
        let key = if (b'A' as u16..=b'Z' as u16).contains(&key) { key + 32 } else { key };
        let step_x = self.view.win_w * 0.15;
        let step_y = self.view.win_h * 0.15;
        match key {
            K_LEFT if self.pannable() => self.pan(step_x, 0.0),
            K_RIGHT if self.pannable() => self.pan(-step_x, 0.0),
            K_UP if self.pannable() => self.pan(0.0, step_y),
            K_DOWN if self.pannable() => self.pan(0.0, -step_y),
            K_BACKSPACE if cmd && !self.nav.is_empty() => {
                return Some(Modal::ConfirmDelete(self.nav.current().to_path_buf()));
            }
            K_LEFT | K_PAGEUP | K_BACKSPACE => self.navigate(NavOp::Step(-1)),
            K_RIGHT | K_PAGEDOWN | K_SPACE => self.navigate(NavOp::Step(1)),
            K_HOME => self.navigate(NavOp::First),
            K_END => self.navigate(NavOp::Last),
            K_PLUS | K_EQUALS => self.zoom_center(1.25),
            K_MINUS | K_UNDERSCORE => self.zoom_center(0.8),
            K_0 => {
                self.view.fit();
                self.after_zoom();
            }
            K_1 => {
                self.view.set_scale(1.0);
                self.after_zoom();
            }
            K_F11 | K_RETURN | K_ENTER => self.toggle_fullscreen(),
            K_ESC => {
                if self.fullscreen {
                    self.toggle_fullscreen();
                } else {
                    self.window.close();
                }
            }
            K_R if self.view.has_image() => {
                self.view.rotate(!shift);
                self.after_zoom();
                // A forgatás megváltoztatja a kijelzőméretű dekódolás célját.
                self.refresh_jobs();
            }
            K_I => {
                self.show_info = !self.show_info;
                self.render();
            }
            K_E if !self.nav.is_empty() => {
                return Some(Modal::Edit(self.nav.current().to_path_buf()));
            }
            K_DELETE if !self.nav.is_empty() => {
                return Some(Modal::ConfirmDelete(self.nav.current().to_path_buf()));
            }
            K_O if cmd => return Some(Modal::Open),
            K_W if cmd => self.window.close(),
            _ => {}
        }
        None
    }

    fn pan(&mut self, dx: f64, dy: f64) {
        self.view.pan(dx, dy);
        self.render();
    }

    /// A megjeleníthető overlay gombok (az egér pozíciójától függetlenül).
    fn button_bar(&self) -> Option<ButtonBar> {
        if self.nav.is_empty() {
            return None;
        }
        Some(ButtonBar { nav: self.nav.len() > 1, edit: true, hover: self.hover })
    }

    fn hit(&self, x: f64, y: f64) -> Option<Button> {
        let bar = self.button_bar()?;
        render::hit_button(x, y, self.view.win_w, self.view.win_h, &bar)
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

    fn mouse_move(&mut self, x: f64, y: f64) {
        let hover = self.hit(x, y);
        if !self.buttons_visible || hover != self.hover {
            self.buttons_visible = true;
            self.hover = hover;
            self.render();
        }
        self.update_cursor();
        self.set_timer(TIMER_HIDE_BUTTONS, 1500);
    }

    fn update_cursor(&self) {
        use objc2_app_kit::NSCursor;
        if self.hover.is_some() {
            NSCursor::pointingHandCursor().set();
        } else if self.pannable() {
            NSCursor::openHandCursor().set();
        } else {
            NSCursor::arrowCursor().set();
        }
    }

    fn button_click(&mut self, b: Button) -> Option<Modal> {
        match b {
            Button::Prev => self.navigate(NavOp::Step(-1)),
            Button::Next => self.navigate(NavOp::Step(1)),
            Button::Edit => return Some(Modal::Edit(self.nav.current().to_path_buf())),
        }
        None
    }

    fn resized(&mut self, w: f64, h: f64) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if !self.fullscreen {
            // Pixelben mentjük: így a következő induláskor a dekódolás célmérete
            // a képernyők lekérdezése nélkül is megvan.
            self.windowed_client = ((w * self.backing).round() as u32, (h * self.backing).round() as u32);
        }
        self.view.set_window(w, h);
        self.update_title();
        self.set_timer(TIMER_HIRES, 200);
        self.render();
    }

    fn activated(&mut self) {
        // Visszatéréskor frissítjük a mappalistát (új, törölt, átnevezett fájlok),
        // és újratöltjük az aktuális képet, ha közben szerkesztették.
        if self.listing_started && !self.nav.is_empty() {
            self.start_listing();
            self.check_current_modified();
        }
    }
}

const EMPTY_HELP: &str = "Nyiss meg egy képet (⌘O), vagy húzd ide a fájlt.\n\n\
    ← →  lapozás   ·   görgő / csippentés  zoom   ·   0  ablakhoz   ·   1  100%\n\
    F11 / dupla kattintás  teljes képernyő   ·   R  forgatás   ·   I  infó   ·   E  szerkesztés   ·   ⌦  törlés";

/// Mappát is elfogad: ilyenkor a mappa első képét nyitja meg.
pub fn resolve_openable(path: PathBuf) -> Option<PathBuf> {
    if path.is_dir() {
        return folder::list(&path, &shell::logical_cmp).into_iter().next();
    }
    Some(path)
}

pub fn run_modal(mtm: MainThreadMarker, modal: Modal) {
    match modal {
        Modal::ConfirmDelete(path) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !shell::confirm(mtm, &format!("Biztosan a Kukába helyezed?\n\n{name}"), "Kukába") {
                return;
            }
            match shell::trash(&path) {
                Ok(()) => {
                    with_app(|a| a.after_delete(&path));
                }
                Err(e) => shell::message(mtm, &format!("Nem sikerült törölni:\n{e}"), true),
            }
        }
        Modal::Edit(path) => {
            if !shell::open_in_editor(&path) {
                shell::message(mtm, "Nem található képszerkesztő alkalmazás ehhez a fájltípushoz.", true);
            }
        }
        Modal::Open => {
            let dir = with_app(|a| a.current_dir()).flatten();
            if let Some(p) = shell::open_dialog(mtm, dir.as_deref()) {
                with_app(|a| a.open_or_spawn(p));
            }
        }
    }
}

fn handle(modal: Option<Modal>, mtm: MainThreadMarker) {
    if let Some(m) = modal {
        run_modal(mtm, m);
    }
}

define_class!(
    // SAFETY:
    // - Az NSView-nak nincs külön alosztályozási megkötése.
    // - A típus nem implementál Drue-t.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastPicView"]
    pub struct FastPicView;

    impl FastPicView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            false
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: CGSize) {
            unsafe { msg_send![super(self), setFrameSize: size] }
            with_app(|a| a.resized(size.width, size.height));
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let flags = event.modifierFlags();
            let cmd = flags.contains(NSEventModifierFlags::Command);
            let shift = flags.contains(NSEventModifierFlags::Shift);
            let key = event.charactersIgnoringModifiers()
                .and_then(|s| s.to_string().encode_utf16().next())
                .unwrap_or(0);
            let modal = with_app(|a| a.key_down(key, cmd, shift)).flatten();
            handle(modal, self.mtm());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            let double = event.clickCount() >= 2;
            let modal = with_app(|a| {
                match a.hit(x, y) {
                    // A szerkesztő gombra adott dupla kattintás ne nyissa meg kétszer.
                    Some(Button::Edit) if double => None,
                    Some(b) => a.button_click(b),
                    None if double => {
                        a.toggle_fullscreen();
                        None
                    }
                    None => {
                        if a.pannable() {
                            a.drag = Some((x, y));
                            objc2_app_kit::NSCursor::closedHandCursor().set();
                        }
                        None
                    }
                }
            })
            .flatten();
            handle(modal, self.mtm());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            with_app(|a| {
                if let Some((lx, ly)) = a.drag {
                    a.drag = Some((x, y));
                    a.pan(x - lx, y - ly);
                }
            });
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            with_app(|a| {
                if a.drag.take().is_some() {
                    a.update_cursor();
                }
            });
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            with_app(|a| a.mouse_move(x, y));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            with_app(|a| {
                if a.drag.is_none() && (a.buttons_visible || a.hover.is_some()) {
                    a.buttons_visible = false;
                    a.hover = None;
                    a.render();
                }
            });
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            let (dx, dy) = (event.scrollingDeltaX(), event.scrollingDeltaY());
            let precise = event.hasPreciseScrollingDeltas();
            with_app(|a| {
                if precise {
                    // Trackpad: a natív viselkedés a görgetés; ha nincs mit pannolni, lapozunk.
                    if a.pannable() {
                        a.pan(dx, dy);
                    }
                } else {
                    a.view.zoom_at(1.2f64.powf(dy / 3.0), x, y);
                    a.after_zoom();
                }
            });
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify(&self, event: &NSEvent) {
            let (x, y) = self.point(event);
            let m = event.magnification();
            with_app(|a| {
                a.view.zoom_at(1.0 + m, x, y);
                a.after_zoom();
            });
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            let which = event.buttonNumber();
            with_app(|a| a.navigate(if which == 3 { NavOp::Step(-1) } else { NavOp::Step(1) }));
        }

        #[unsafe(method(onHiresTimer:))]
        fn on_hires_timer(&self, _t: *mut AnyObject) {
            with_app(|a| {
                a.kill_timer(TIMER_HIRES);
                a.refresh_jobs();
            });
        }

        #[unsafe(method(onHideButtonsTimer:))]
        fn on_hide_buttons_timer(&self, _t: *mut AnyObject) {
            with_app(|a| {
                a.kill_timer(TIMER_HIDE_BUTTONS);
                if a.hover.is_none() && a.drag.is_none() {
                    a.buttons_visible = false;
                    a.render();
                }
            });
        }

        #[unsafe(method(onBenchTimer:))]
        fn on_bench_timer(&self, _t: *mut AnyObject) {
            with_app(|a| a.bench_finish());
        }
    }

    unsafe impl NSObjectProtocol for FastPicView {}

    unsafe impl NSDraggingDestination for FastPicView {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let paths = dropped_paths(sender);
            let empty = paths.is_empty();
            with_app(|a| {
                for p in paths {
                    a.open_or_spawn(p);
                }
            });
            !empty
        }
    }
);

impl FastPicView {
    /// Az egérpozíció a nézet koordinátáiban, bal felső sarokból számolva.
    fn point(&self, event: &NSEvent) -> (f64, f64) {
        let p = event.locationInWindow();
        let local = self.convertPoint_fromView(p, None);
        let h = self.frame().size.height;
        (local.x, h - local.y)
    }

    pub fn install_tracking(&self) {
        let opts = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveInKeyWindow
            | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                objc2_core_foundation::CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)),
                opts,
                Some(self),
                None,
            )
        };
        self.addTrackingArea(&area);
        unsafe { self.registerForDraggedTypes(&NSArray::from_slice(&[NSPasteboardTypeFileURL])) };
    }
}

fn dropped_paths(sender: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<PathBuf> {
    let pb = sender.draggingPasteboard();
    let classes = NSArray::from_slice(&[<NSURL as ClassType>::class()]);
    let Some(objs) = (unsafe { pb.readObjectsForClasses_options(&classes, None) }) else {
        return Vec::new();
    };
    objs.iter()
        .filter_map(|o| {
            let url: &NSURL = unsafe { &*(&*o as *const AnyObject as *const NSURL) };
            url.path().map(|p| PathBuf::from(p.to_string()))
        })
        .collect()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastPicDelegate"]
    pub struct Delegate;

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidBecomeActive:))]
        fn did_become_active(&self, _n: &NSNotification) {
            super::mark("active");
            with_app(|a| a.activated());
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate(&self, _app: &NSApplication) -> bool {
            true
        }

        /// Az indulás lezárult: innentől minden `openFile:` valódi felhasználói kérés.
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            super::mark("did_finish");
            LAUNCH_DONE.store(true, Ordering::Relaxed);
        }

        /// A LaunchServices a futó példánynak küldi az új fájlt; minden továbbit külön process kap.
        /// Kivéve az indulás közben visszakapott saját argumentumot – lásd [`is_launch_echo`].
        #[unsafe(method(application:openFile:))]
        fn open_file(&self, _app: &NSApplication, file: &NSString) -> bool {
            let path = PathBuf::from(file.to_string());
            if is_launch_echo(&path) {
                return true.into();
            }
            with_app(|a| a.open_or_spawn(path)).is_some()
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _n: &NSNotification) {
            with_app(|a| {
                let size = a.nsview.frame().size;
                a.resized(size.width, size.height);
            });
        }

        #[unsafe(method(windowWillEnterFullScreen:))]
        fn will_enter_fullscreen(&self, _n: &NSNotification) {
            // A belépés előtti ablakállapotot mentjük: onnan tér vissza, és ezt írjuk ki kilépéskor.
            with_app(|a| {
                a.save_state();
                a.fullscreen = true;
            });
        }

        #[unsafe(method(windowDidExitFullScreen:))]
        fn did_exit_fullscreen(&self, _n: &NSNotification) {
            with_app(|a| a.fullscreen = false);
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            with_app(|a| a.save_state());
            // Nem várunk a háttérszálakra, nem bontunk le semmit.
            std::process::exit(0);
        }
    }

    unsafe impl NSObjectProtocol for Delegate {}
);

impl Delegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Delegate> {
        unsafe { msg_send![Delegate::alloc(mtm), init] }
    }
}
