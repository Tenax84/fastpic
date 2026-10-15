//! Windows platformréteg: Win32 ablak, GDI, WIC.

mod placement;
mod render;
mod shell;
mod update;
mod wic;
mod window;

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

use crate::Args;
use crate::core::decode::{self, Target};
use crate::core::state;
use crate::core::worker::{Job, Worker};

fn ft_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

pub(crate) fn now_filetime() -> u64 {
    unsafe { ft_u64(GetSystemTimePreciseAsFileTime()) }
}

fn process_created_filetime() -> u64 {
    let (mut c, mut e, mut k, mut u) = Default::default();
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    ft_u64(c)
}

/// (working set, peak working set, private bytes)
pub(crate) fn memory_usage() -> (usize, usize, usize) {
    let mut pmc =
        PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    unsafe {
        let _ =
            K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, pmc.cb);
    }
    (pmc.WorkingSetSize, pmc.PeakWorkingSetSize, pmc.PrivateUsage)
}

/// Bench módban időbélyegek a fázisokhoz (process létrehozásától számítva).
static MARKS: std::sync::Mutex<Vec<(&'static str, u64)>> = std::sync::Mutex::new(Vec::new());
pub(crate) static BENCH_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn mark(name: &'static str) {
    if BENCH_ON.load(Ordering::Relaxed) {
        MARKS.lock().unwrap().push((name, now_filetime()));
    }
}

pub(crate) fn marks_report(created: u64) -> String {
    MARKS
        .lock()
        .unwrap()
        .iter()
        .map(|(n, t)| format!("mark_{n}={:.1}\n", t.saturating_sub(created) as f64 / 10_000.0))
        .collect()
}

/// Az exe-be ágyazott alkalmazásikon (erőforrás-azonosító: 1) a kért rendszerméretben.
unsafe fn load_icon(hinstance: HINSTANCE, cx: SYSTEM_METRICS_INDEX, cy: SYSTEM_METRICS_INDEX) -> HICON {
    unsafe {
        LoadImageW(
            Some(hinstance),
            PCWSTR(1 as _),
            IMAGE_ICON,
            GetSystemMetrics(cx),
            GetSystemMetrics(cy),
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    }
}

fn config_dir() -> Option<PathBuf> {
    // Környezeti változó: nem kell hozzá a shell32 betöltése.
    std::env::var_os("APPDATA").map(PathBuf::from)
}

pub fn run(args: Args) {
    if args.fetch_update {
        std::process::exit(update::fetch_child());
    }
    if args.register {
        let ok = shell::register();
        shell::message(
            None,
            if ok {
                "A FastPic regisztrálva. A megnyíló Beállításokban választhatod ki alapértelmezett képnézegetőnek."
            } else {
                "A regisztráció nem sikerült teljesen."
            },
            !ok,
        );
        return;
    }
    if args.unregister {
        shell::unregister();
        shell::message(None, "A FastPic fájltársításai eltávolítva.", false);
        return;
    }

    if args.bench.is_some() {
        BENCH_ON.store(true, Ordering::Relaxed);
        mark("main");
    }
    let path = args.path.clone().and_then(window::resolve_openable);
    let bench =
        args.bench.map(|out| window::Bench { out, created_filetime: process_created_filetime(), first_image_ms: None });

    // 1. Mentett ablakállapot: ebből tudjuk a dekódolás célméretét még az ablak előtt.
    let state_path = state::state_file(config_dir());
    let saved = window::initial_state(state_path.as_deref());
    let dpi_scale = unsafe { GetDpiForSystem() } as f64 / 96.0;
    let (def_w, def_h) = placement::default_size(dpi_scale);
    let client = match &saved {
        Some(s) => (s.client_w, s.client_h),
        None => ((def_w - 16).max(1) as u32, (def_h - 39).max(1) as u32),
    };

    // 2. A dekódolás azonnal indul, párhuzamosan az ablak felállásával.
    let worker = Worker::spawn(
        wic::thread_init,
        Box::new(|job: &Job, cancel: &dyn Fn() -> bool| {
            mark("decode_start");
            // A kész GDI-bitmap is itt, a háttérszálon készül el.
            let r = decode::decode_file(&job.path, job.target, cancel, &|p, t, c| wic::decode(p, t, c), &|d| {
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
            mark("decode_end");
            r
        }),
        Box::new(|| window::post_to_window(window::WM_APP_DECODED)),
    );
    let first_job = path.as_ref().map(|p| {
        let target = Target::Fit(client.0, client.1);
        worker.set_jobs(vec![Job { id: 1, path: p.clone(), target, prio: 0 }]);
        (1u64, target)
    });

    // 3. Ablak.
    let hwnd = unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(window::wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: load_icon(hinstance, SM_CXICON, SM_CYICON),
            hIconSm: load_icon(hinstance, SM_CXSMICON, SM_CYSMICON),
            hbrBackground: HBRUSH::default(),
            lpszClassName: w!("FastPicViewer"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let Ok(hwnd) = CreateWindowExW(
            // Fogd és vidd DragAcceptFiles nélkül: így a shell32 csak ejtéskor töltődik be.
            WS_EX_ACCEPTFILES,
            w!("FastPicViewer"),
            w!("FastPic"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            def_w,
            def_h,
            None,
            None,
            Some(hinstance),
            None,
        ) else {
            return;
        };
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            size_of::<BOOL>() as u32,
        );
        hwnd
    };
    window::HWND_VAL.store(hwnd.0 as isize, Ordering::Release);
    mark("window_created");

    let renderer = render::Renderer::new();
    window::install(window::App::new(hwnd, renderer, worker, path, first_job, client, state_path, bench));

    // 4. Megjelenítés a mentett helyen / méretben.
    unsafe {
        match &saved {
            Some(s) => placement::apply(hwnd, s),
            None => {
                let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            }
        }
    }
    mark("window_shown");
    // Ha a dekóder az ablak előtt végzett, az értesítése elveszett: most feldolgozzuk.
    window::post_to_window(window::WM_APP_DECODED);

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Bezárás után azonnal kilépünk: nem várunk a háttérszálakra, nem bontunk le semmit.
    std::process::exit(0);
}
