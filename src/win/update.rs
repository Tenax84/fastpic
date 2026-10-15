//! Frissítés a GitHub-kiadásokból: napi egy ellenőrzés egy rövid életű szálon, az első kép
//! megjelenése után, és a felhasználó jóváhagyására a telepítő letöltése. A hálózat WinHTTP
//! (delay-load: a winhttp.dll csak az első kéréskor töltődik be, az indulást nem terheli).
//!
//! Az ellenőrző lekérés külön, rövid életű processben fut (`--fetch-update`): a WinHTTP a
//! TLS-sel és a tanúsítványkezeléssel együtt a process végéig bent maradna, mérve +5,3 MB
//! working set, +1 MB private és 4 állandó szál – ezt a nézegető nem viheti magával.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, w};

use super::window::{WM_APP_UPDATE, post_to_window};
use crate::core::update::{self, Asset, Release};

const INSTALLER_PREFIX: &str = "FastPic-Setup-";
/// Ennyivel az első kép után kérdezünk: addig fut az előtöltés és a mappalistázás.
const CHECK_DELAY: Duration = Duration::from_secs(3);
const MAX_JSON: usize = 1 << 20;
const MAX_INSTALLER: usize = 64 << 20;

pub enum Event {
    Available(Release),
    /// A letöltött, ellenőrzött telepítő.
    Ready(PathBuf),
    Failed(String),
}

static EVENT: Mutex<Option<Event>> = Mutex::new(None);

fn send(e: Event) {
    *EVENT.lock().unwrap() = Some(e);
    post_to_window(WM_APP_UPDATE);
}

pub fn take_event() -> Option<Event> {
    EVENT.lock().unwrap().take()
}

/// Inno Setuppal telepített példány (az eltávolító az exe mellett van)? Ha nem, hordozható:
/// annál csak a kiadás oldalát nyitjuk meg.
pub fn is_installed() -> bool {
    std::env::current_exe().ok().and_then(|p| Some(p.parent()?.join("unins000.exe").exists())).unwrap_or(false)
}

fn update_dir() -> PathBuf {
    std::env::temp_dir().join("FastPic-update")
}

pub fn spawn_check() {
    let _ = std::thread::Builder::new().name("update-check".into()).spawn(|| {
        std::thread::sleep(CHECK_DELAY);
        let Some(file) = update::schedule_file(super::config_dir()) else { return };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        if !update::claim_check(&file, now) {
            return;
        }
        // Egy korábbi frissítés telepítője.
        let _ = std::fs::remove_dir_all(update_dir());
        let Ok(exe) = std::env::current_exe() else { return };
        let Ok(out) =
            Command::new(exe).arg(FETCH_ARG).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).output()
        else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let body = out.stdout;
        let Some(r) = std::str::from_utf8(&body).ok().and_then(|s| update::parse_release(s, INSTALLER_PREFIX)) else {
            return;
        };
        if update::is_newer(&r.version, env!("CARGO_PKG_VERSION")) {
            send(Event::Available(r));
        }
    });
}

pub const FETCH_ARG: &str = "--fetch-update";

/// A `--fetch-update` gyerekprocess: a legutóbbi kiadás JSON-ját a standard kimenetre írja.
pub fn fetch_child() -> i32 {
    match http_get(update::LATEST_URL, MAX_JSON) {
        Ok(body) if std::io::stdout().write_all(&body).is_ok() => 0,
        _ => 1,
    }
}

pub fn spawn_download(asset: Asset) {
    let _ = std::thread::Builder::new().name("update-download".into()).spawn(move || {
        send(match download(&asset) {
            Ok(p) => Event::Ready(p),
            Err(e) => Event::Failed(e),
        });
    });
}

fn download(asset: &Asset) -> Result<PathBuf, String> {
    let data = http_get(&asset.url, MAX_INSTALLER)?;
    if update::sha256(&data) != asset.sha256 {
        return Err("a letöltött fájl ellenőrzőösszege nem egyezik".into());
    }
    let name = Path::new(&asset.name).file_name().ok_or("hibás fájlnév")?;
    let dir = update_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    std::fs::write(&path, &data).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Csendes telepítés (folyamatjelzővel). A futó példányokat az Inno restart managere zárja be,
/// a végén pedig a telepítő újranyitja a `reopen` képet (lásd `installer/fastpic.iss`).
pub fn installer_params(reopen: Option<&Path>) -> String {
    let reopen = reopen.map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "-".into());
    format!("/SILENT /SP- /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS \"/RELAUNCH={reopen}\"")
}

struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(h: *mut core::ffi::c_void) -> Result<Handle, String> {
        if h.is_null() { Err(windows::core::Error::from_thread().message()) } else { Ok(Handle(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// Egyszerű HTTPS GET, átirányításokkal (a kiadások fájljai más gépről jönnek).
fn http_get(url: &str, max: usize) -> Result<Vec<u8>, String> {
    let (host, path) = update::split_https_url(url).ok_or("nem HTTPS cím")?;
    let err = |e: windows::core::Error| e.message();
    let agent = HSTRING::from(format!("FastPic/{}", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = Handle::new(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0))?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000).map_err(err)?;
        let conn = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0))?;
        let req = Handle::new(WinHttpOpenRequest(
            conn.0,
            w!("GET"),
            &HSTRING::from(path),
            None,
            None,
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let headers: Vec<u16> = "Accept: application/vnd.github+json\r\n".encode_utf16().collect();
        WinHttpSendRequest(req.0, Some(&headers), None, 0, 0, 0).map_err(err)?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(err)?;
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            None,
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(err)?;
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut body = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).map_err(err)?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buf[..read as usize]);
            if body.len() > max {
                return Err("túl nagy válasz".into());
            }
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valódi hálózati kérés: `cargo test --release -- --ignored network`
    #[test]
    #[ignore]
    fn network_latest_release_and_download() {
        let body = http_get(update::LATEST_URL, MAX_JSON).unwrap();
        let r = update::parse_release(std::str::from_utf8(&body).unwrap(), INSTALLER_PREFIX).unwrap();
        let asset = r.installer.expect("telepítő a kiadásban");
        let path = download(&asset).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        let _ = std::fs::remove_dir_all(update_dir());
        let bad = Asset { sha256: [0; 32], ..asset };
        assert!(download(&bad).is_err());
    }
}
