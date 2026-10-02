//! Rendszerintegráció: Kuka, megnyitás-párbeszéd, új példány indítása, szerkesztő,
//! Finder-szerű rendezés, dátumformázás.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSAlert, NSAlertStyle, NSOpenPanel, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSFileManager, NSString, NSURL};

fn ns(path: &Path) -> Retained<NSString> {
    NSString::from_str(&path.to_string_lossy())
}

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&ns(path))
}

/// A Finder rendezése (pl. „kep2” a „kep10” előtt), kis-nagybetűtől függetlenül.
pub fn logical_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    let (a, b) = (NSString::from_str(&a.to_string_lossy()), NSString::from_str(&b.to_string_lossy()));
    let r = a.localizedStandardCompare(&b);
    (r as isize).cmp(&0)
}

/// Minden kép külön processt kap.
pub fn spawn_viewer(path: &Path) {
    // Fejlesztői fék: teszt közben egy hibás ág se tudjon példányokat szaporítani.
    if std::env::var("FASTPIC_NO_SPAWN").is_ok_and(|v| v != "0" && !v.is_empty()) {
        eprintln!("[fastpic] spawn_viewer kihagyva (FASTPIC_NO_SPAWN): {}", path.display());
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    // Alkalmazáscsomagból a LaunchServices külön példányt indít; enélkül a bináris
    // közvetlen indítása is működik.
    if let Some(app) = bundle_path(&exe) {
        let ok = std::process::Command::new("/usr/bin/open").args(["-n", "-a"]).arg(app).arg(path).spawn().is_ok();
        if ok {
            return;
        }
    }
    let _ = std::process::Command::new(exe).arg(path).spawn();
}

/// `.../FastPic.app/Contents/MacOS/fastpic` → `.../FastPic.app`
pub fn bundle_path(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app")
        .then(|| app.to_path_buf())
}

pub fn confirm(mtm: MainThreadMarker, text: &str, ok_title: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(text));
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.addButtonWithTitle(&NSString::from_str(ok_title));
    alert.addButtonWithTitle(&NSString::from_str("Mégse"));
    // Az első gomb válasza NSAlertFirstButtonReturn (1000).
    alert.runModal() == 1000
}

pub fn message(mtm: MainThreadMarker, text: &str, error: bool) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(text));
    alert.setAlertStyle(if error { NSAlertStyle::Critical } else { NSAlertStyle::Informational });
    alert.addButtonWithTitle(&NSString::from_str("OK"));
    alert.runModal();
}

/// Kukába helyezés (visszavonható). A megerősítést a hívó kérte be.
pub fn trash(path: &Path) -> Result<(), String> {
    let fm = NSFileManager::defaultManager();
    fm.trashItemAtURL_resultingItemURL_error(&url(path), None).map_err(|e| e.localizedDescription().to_string())
}

pub fn open_dialog(mtm: MainThreadMarker, start_dir: Option<&Path>) -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setMessage(Some(&NSString::from_str("Nyiss meg egy képet")));
    if let Some(dir) = start_dir {
        panel.setDirectoryURL(Some(&url(dir)));
    }
    if panel.runModal() != 1 {
        return None;
    }
    let u = panel.URLs().firstObject()?;
    u.path().map(|p| PathBuf::from(p.to_string()))
}

/// Ismert képszerkesztők, preferencia-sorrendben (kisbetűs részszöveg az alkalmazás nevében).
/// Az Előnézet a végén áll: alapszintű szerkesztésre (jelölés, forgatás) az is alkalmas.
const EDITORS: &[&str] = &[
    "photoshop",
    "affinity photo",
    "pixelmator",
    "photomator",
    "gimp",
    "krita",
    "acorn",
    "aseprite",
    "graphicconverter",
    "preview",
    "előnézet",
];

/// Megnyitás képszerkesztővel: a fájltípust kezelő alkalmazások közül a legelőrébb való
/// ismert képszerkesztő. macOS-en nincs „Szerkesztés” ige, ezért a listából választunk.
pub fn open_in_editor(path: &Path) -> bool {
    let ws = NSWorkspace::sharedWorkspace();
    let file = url(path);
    let apps = ws.URLsForApplicationsToOpenURL(&file);
    let mut best: Option<(usize, Retained<NSURL>)> = None;
    for app in apps.iter() {
        let name = app.lastPathComponent().map(|s| s.to_string()).unwrap_or_default().to_lowercase();
        if name.contains("fastpic") {
            continue;
        }
        if let Some(rank) = EDITORS.iter().position(|e| name.contains(e))
            && best.as_ref().is_none_or(|(r, _)| rank < *r)
        {
            best = Some((rank, app));
        }
    }
    let Some((_, app)) = best else { return false };
    let config = NSWorkspaceOpenConfiguration::configuration();
    let urls = NSArray::from_slice(&[&*file]);
    ws.openURLs_withApplicationAtURL_configuration_completionHandler(&urls, &app, &config, None);
    true
}

/// Fájl módosítási ideje helyi időben, „2026.09.21. 14:05”.
pub fn format_time(t: std::time::SystemTime) -> Option<String> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    Some(format!("{}.{:02}.{:02}. {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min))
}

/// A `--register` Windowson registry-bejegyzéseket ír; macOS-en a támogatott típusokat
/// az alkalmazáscsomag `Info.plist`-je hirdeti, az alapértelmezést a felhasználó állítja.
pub fn register_hint() -> &'static str {
    "macOS-en a fájltársítást a Finderben állíthatod be:\n\n\
     jelöld ki a képet, ⌘I (Adatok), „Társítás ezzel” → FastPic → „Összes módosítása”."
}
