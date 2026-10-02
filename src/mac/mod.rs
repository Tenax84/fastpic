//! macOS platformréteg: AppKit ablak, CoreAnimation megjelenítés, ImageIO dekódolás.

mod imageio;
mod placement;
mod render;
mod shell;
mod window;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy,
    NSBackingStoreType, NSColor, NSMenu, NSMenuItem, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::NSString;

use crate::Args;
use crate::core::decode::{self, Target};
use crate::core::state::{self, WindowState};
use crate::core::worker::{Job, Worker};

/// Bench módban időbélyegek a fázisokhoz (az indulástól számítva).
static MARKS: Mutex<Vec<(std::borrow::Cow<'static, str>, f64)>> = Mutex::new(Vec::new());
static BENCH_ON: AtomicBool = AtomicBool::new(false);
static START: Mutex<Option<Instant>> = Mutex::new(None);

fn elapsed_ms() -> f64 {
    START.lock().unwrap().map(|s| s.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// Az indulás óta eltelt idő ms-ban (bench nélkül is).
pub(crate) fn uptime_ms() -> f64 {
    elapsed_ms()
}

pub(crate) fn mark(name: &'static str) {
    if BENCH_ON.load(Ordering::Relaxed) {
        let t = elapsed_ms();
        MARKS.lock().unwrap().push((name.into(), t));
    }
}

/// Bench-jelölő futásidőben összeállított névvel (pl. feladatazonosító, célméret).
pub(crate) fn mark_owned(name: String) {
    if BENCH_ON.load(Ordering::Relaxed) {
        let t = elapsed_ms();
        MARKS.lock().unwrap().push((name.into(), t));
    }
}

fn target_desc(t: Target) -> String {
    match t {
        Target::Fit(w, h) => format!("fit{w}x{h}"),
        Target::Full => "full".into(),
    }
}

pub(crate) fn marks_report() -> String {
    MARKS.lock().unwrap().iter().map(|(n, t)| format!("mark_{n}={t:.1}\n")).collect()
}

/// (rezidens méret, csúcs, fizikai lábnyom) bájtban. A `phys_footprint` felel meg az
/// Activity Monitor „Memory” oszlopának: a megosztott rendszerkönyvtárak lapjait nem számolja.
#[allow(deprecated)]
pub(crate) fn memory_usage() -> (usize, usize, usize) {
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = (size_of::<libc::mach_task_basic_info>() / size_of::<u32>()) as u32;
        let r = libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        );
        let (ws, peak) = if r == 0 { (info.resident_size as usize, info.resident_size_max as usize) } else { (0, 0) };
        let mut ri: libc::rusage_info_v4 = std::mem::zeroed();
        let ok = libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            &mut ri as *mut _ as *mut libc::rusage_info_t,
        );
        let footprint = if ok == 0 { ri.ri_phys_footprint as usize } else { ws };
        (ws, peak, footprint)
    }
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
}

/// Minimális főmenü: enélkül a ⌘Q és a szokásos alkalmazásmenü sem működne.
fn install_menu(app: &NSApplication, mtm: MainThreadMarker) {
    let menubar = NSMenu::new(mtm);
    let app_item = NSMenuItem::new(mtm);
    let app_menu = NSMenu::new(mtm);
    unsafe {
        let hide = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("FastPic elrejtése"),
            Some(objc2::sel!(hide:)),
            &NSString::from_str("h"),
        );
        let quit = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Kilépés"),
            Some(objc2::sel!(terminate:)),
            &NSString::from_str("q"),
        );
        app_menu.addItem(&hide);
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&quit);
        app_item.setSubmenu(Some(&app_menu));
        menubar.addItem(&app_item);
    }
    app.setMainMenu(Some(&menubar));
}

fn create_window(
    mtm: MainThreadMarker,
    size: (f64, f64),
    delegate: &window::Delegate,
) -> (Retained<NSWindow>, Retained<window::FastPicView>) {
    let rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(size.0, size.1));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let win = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    let view: Retained<window::FastPicView> =
        unsafe { objc2::msg_send![window::FastPicView::alloc(mtm), initWithFrame: rect] };
    unsafe {
        win.setAppearance(NSAppearance::appearanceNamed(NSAppearanceNameDarkAqua).as_deref());
        win.setBackgroundColor(Some(&NSColor::blackColor()));
        win.setContentMinSize(CGSize::new(320.0, 240.0));
        win.setAcceptsMouseMovedEvents(true);
        win.setContentView(Some(&view));
        win.setDelegate(Some(ProtocolObject::from_ref(delegate)));
        win.setReleasedWhenClosed(false);
    }
    view.install_tracking();
    (win, view)
}

pub fn run(args: Args) {
    *START.lock().unwrap() = Some(Instant::now());
    if args.bench.is_some() {
        BENCH_ON.store(true, Ordering::Relaxed);
        mark("main");
    }
    let mtm = MainThreadMarker::new().expect("a fő szálon kell futnia");

    if args.register || args.unregister {
        shell::message(mtm, shell::register_hint(), false);
        return;
    }

    let path = args.path.clone().and_then(window::resolve_openable);
    // Az AppKit ezt az utat az indulás során `application:openFile:`-ként visszaadja.
    window::set_launch_path(path.clone());
    let bench = args.bench.map(|out| window::Bench {
        out,
        started: START.lock().unwrap().unwrap(),
        preview_ms: None,
        first_image_ms: None,
    });

    // 1. Mentett ablakállapot: ebből tudjuk a dekódolás célméretét még az ablak előtt.
    //    A mentett kliensméret pixelben van, így az AppKit betöltése (NSScreen) nélkül is
    //    elindíthatjuk a dekódolást – az teszi ki az indulás első ~100 ms-át.
    let state_path = state::state_file(config_dir());
    let saved = WindowState::load_if(state_path.as_deref());
    mark("state_read");

    // 2. A dekódolás azonnal indul, párhuzamosan az ablak felállásával.
    let worker = Worker::spawn(
        imageio::thread_init,
        Box::new(|job: &Job, cancel: &dyn Fn() -> bool| {
            mark_owned(format!(
                "decode_start_{}_{}_{}",
                job.id,
                target_desc(job.target),
                job.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
            let r = decode::decode_file(&job.path, job.target, cancel, &|p, t, c| imageio::decode(p, t, c), &|d| {
                let tex = render::Texture::from_frame(&d.frames[0])
                    .ok_or_else(|| decode::DecodeError::Failed("Nincs elég memória a képhez.".into()))?;
                Ok(window::Prepared {
                    tex,
                    orig_w: d.orig_width,
                    orig_h: d.orig_height,
                    full: d.full,
                    vector: d.vector,
                })
            });
            mark_owned(format!("decode_end_{}", job.id));
            r
        }),
        Box::new(window::post_decoded),
    );
    let first_target = match &saved {
        Some(s) => Target::Fit(s.client_w, s.client_h),
        // Első indításnál még nem tudjuk a méretet: a főképernyőé a felső becslés.
        None => Target::Fit(3840, 2160),
    };
    // Nagy képnél előbb egy negyedméretű előnézetet kérünk: ez töredék idő alatt megvan.
    // A végleges felbontás rögtön mögé kerül a sorba – így a dekódoló szál az előnézet után
    // azonnal folytatja, nem vár az ablakra és az első kirajzolásra.
    let preview = path.as_ref().and_then(|p| window::preview_target(p, first_target));
    let queued = preview.map(|_| (2u64, first_target));
    let first_target = preview.unwrap_or(first_target);
    let first_job = path.as_ref().map(|p| {
        let mut jobs = vec![Job { id: 1, path: p.clone(), target: first_target, prio: 0 }];
        if let Some((id, target)) = queued {
            jobs.push(Job { id, path: p.clone(), target, prio: 1 });
        }
        worker.set_jobs(jobs);
        (1u64, first_target)
    });
    let preview_job = preview.and(first_job).map(|j| j.0);
    let queued_job = first_job.and(queued);

    // 3. Ablak. Innentől kell az AppKit: a képernyők és a Retina-szorzó lekérdezése.
    let saved = saved.filter(|s| placement::is_on_screen(s, mtm));
    let backing = objc2_app_kit::NSScreen::mainScreen(mtm).map(|s| s.backingScaleFactor()).unwrap_or(1.0).max(1.0);
    let (def_w, def_h) = placement::default_size(mtm);
    let client = match &saved {
        Some(s) => ((s.right - s.left) as f64, (s.bottom - s.top) as f64),
        None => (def_w, def_h),
    };
    mark("screen_read");
    let app = NSApplication::sharedApplication(mtm);
    mark("nsapp");
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    mark("activation_policy");
    let delegate = window::Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let (win, view) = create_window(mtm, client, &delegate);
    mark("window_created");

    let renderer = render::Renderer::new(backing);
    view.setLayer(Some(&renderer.root));
    view.setWantsLayer(true);
    // A tényleges kliensméretet a nézet adja (az ablakkeret levonása után).
    let client = {
        let f = view.frame().size;
        (f.width.max(1.0), f.height.max(1.0))
    };
    window::install(window::App::new(
        win.clone(),
        view.clone(),
        renderer,
        worker,
        path,
        first_job,
        preview_job,
        queued_job,
        client,
        backing,
        state_path,
        bench,
    ));

    // 4. Megjelenítés a mentett helyen / méretben.
    match &saved {
        Some(s) => placement::apply(&win, s),
        None => win.center(),
    }
    win.makeKeyAndOrderFront(None);
    mark("order_front");
    app.activate();
    mark("window_shown");

    install_menu(&app, mtm);
    mark("menu");
    // Ha a dekóder az ablak előtt végzett, az értesítése még nem futott le: most pótoljuk.
    // Közvetlenül hívjuk (nem a fő sorba téve), és a CA-tranzakciót is kiürítjük, hogy a
    // kép még az `app.run()` inicializálása előtt a képernyőre kerüljön.
    window::drain_decoded_now();
    objc2_quartz_core::CATransaction::flush();

    mark("run_enter");
    app.run();
    std::process::exit(0);
}
