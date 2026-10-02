#![cfg_attr(not(test), windows_subsystem = "windows")]

mod core;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;

use std::path::PathBuf;

/// Parancssor: `fastpic [kép]`, `--register`, `--unregister`, `--bench <kimeneti fájl> <kép>`.
pub struct Args {
    pub path: Option<PathBuf>,
    pub register: bool,
    pub unregister: bool,
    pub bench: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = Args { path: None, register: false, unregister: false, bench: None };
    let mut it = std::env::args_os().skip(1);
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--register") => args.register = true,
            Some("--unregister") => args.unregister = true,
            Some("--bench") => args.bench = it.next().map(PathBuf::from),
            _ if args.path.is_none() => {
                let p = PathBuf::from(a);
                args.path = Some(std::path::absolute(&p).unwrap_or(p));
            }
            _ => {}
        }
    }
    args
}

fn main() {
    // GUI-alkalmazásnak nincs konzolja: egy esetleges pánik okát fájlba írjuk.
    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::write(std::env::temp_dir().join("fastpic-crash.txt"), info.to_string());
    }));
    let args = parse_args();
    #[cfg(windows)]
    win::run(args);
    #[cfg(target_os = "macos")]
    mac::run(args);
}
