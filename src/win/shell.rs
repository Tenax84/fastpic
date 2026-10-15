//! Shell-integráció: lomtár, megnyitás-párbeszéd, új példány indítása, fájltársítás,
//! természetes rendezés, dátumformázás.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{ERROR_SUCCESS, FILETIME, HWND, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Time::FileTimeToSystemTime;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::core::formats::EXTENSIONS;

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Az Intéző rendezése (pl. „kep2” a „kep10” előtt).
pub fn logical_cmp(a: &OsStr, b: &OsStr) -> Ordering {
    let (a, b) = (wide(a), wide(b));
    let r = unsafe { StrCmpLogicalW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr())) };
    r.cmp(&0)
}

/// Minden kép külön processt kap.
pub fn spawn_viewer(path: &Path) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(path).spawn();
    }
}

/// Webcím megnyitása az alapértelmezett böngészőben.
pub fn open_url(url: &str) {
    ensure_sta();
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
}

/// Program indítása a shellen keresztül (így a UAC-emelést kérő telepítő is elindul).
pub fn launch(file: &Path, params: &str) -> bool {
    ensure_sta();
    let r = unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(file.as_os_str()), &HSTRING::from(params), None, SW_SHOWNORMAL)
    };
    r.0 as isize > 32
}

fn ensure_sta() {
    // A UI-szálon csak akkor inicializáljuk a COM-ot, ha tényleg kell (indulási idő).
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    DONE.with(|d| {
        if !d.get() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            d.set(true);
        }
    });
}

pub fn confirm(hwnd: HWND, text: &str, caption: &str) -> bool {
    let r = unsafe {
        MessageBoxW(
            Some(hwnd),
            &HSTRING::from(text),
            &HSTRING::from(caption),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON1,
        )
    };
    r == IDYES
}

pub fn message(hwnd: Option<HWND>, text: &str, error: bool) {
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    unsafe {
        MessageBoxW(hwnd, &HSTRING::from(text), w!("FastPic"), MB_OK | icon);
    }
}

/// Lomtárba helyezés (visszavonható). A megerősítést a hívó kérte be.
pub fn recycle(hwnd: HWND, path: &Path) -> Result<()> {
    ensure_sta();
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        op.SetOwnerWindow(hwnd)?;
        op.SetOperationFlags(FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOFX_RECYCLEONDELETE)?;
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        op.DeleteItem(&item, None)?;
        op.PerformOperations()?;
        if op.GetAnyOperationsAborted()?.as_bool() {
            return Err(windows::Win32::Foundation::E_ABORT.into());
        }
    }
    Ok(())
}

pub fn open_dialog(hwnd: HWND, start_dir: Option<&Path>) -> Option<PathBuf> {
    ensure_sta();
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let pattern: String = EXTENSIONS.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
        let pattern = HSTRING::from(pattern);
        let filters = [
            COMDLG_FILTERSPEC { pszName: w!("Képek"), pszSpec: PCWSTR(pattern.as_ptr()) },
            COMDLG_FILTERSPEC { pszName: w!("Minden fájl"), pszSpec: w!("*.*") },
        ];
        dlg.SetFileTypes(&filters).ok()?;
        if let Some(dir) = start_dir
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None)
        {
            let _ = dlg.SetFolder(&item);
        }
        dlg.Show(Some(hwnd)).ok()?;
        let item = dlg.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = PathBuf::from(std::ffi::OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0 as *const _));
        Some(path)
    }
}

/// Képszerkesztők a fájltípus kezelői között, preferencia-sorrendben (kisbetűs részszöveg a
/// kezelő nevében vagy útvonalában).
const EDITORS: &[&str] = &[
    "photoshop",
    "affinity photo",
    "affinity",
    "paint.net",
    "paintdotnet",
    "gimp",
    "krita",
    "paintshop",
    "corel",
    "photopea",
    "pixlr",
    "luminar",
    "mspaint",
    "paint",
];

/// Megnyitás képszerkesztővel, ebben a sorrendben:
/// 1. a Windows „Szerkesztés” művelete (amit a felhasználó vagy a rendszer beállított);
/// 2. a fájltípus regisztrált kezelői közül egy ismert képszerkesztő (Store-appok is, pl. paint.net);
/// 3. a „Társítás ezzel” ablak, ahol a felhasználó választ (az alapértelmezett társítás nem változik).
pub fn open_in_editor(hwnd: HWND, path: &Path) -> bool {
    ensure_sta();
    let file = HSTRING::from(path.as_os_str());
    unsafe {
        let mut sei = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
            hwnd,
            lpVerb: w!("edit"),
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if ShellExecuteExW(&mut sei).is_ok() {
            return true;
        }
    }
    if open_with_known_editor(path, &file).is_some() {
        return true;
    }
    let info = OPENASINFO {
        pcszFile: PCWSTR(file.as_ptr()),
        pcszClass: PCWSTR::null(),
        oaifInFlags: OAIF_EXEC | OAIF_HIDE_REGISTRATION,
    };
    unsafe { SHOpenWithDialog(Some(hwnd), &info).is_ok() }
}

fn take_pwstr(p: windows::core::PWSTR) -> String {
    let s = unsafe { p.to_string().unwrap_or_default() };
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}

fn open_with_known_editor(path: &Path, file: &HSTRING) -> Option<()> {
    let ext = format!(".{}", path.extension()?.to_string_lossy());
    unsafe {
        let handlers = SHAssocEnumHandlers(&HSTRING::from(ext), ASSOC_FILTER_NONE).ok()?;
        let mut best: Option<(usize, IAssocHandler)> = None;
        loop {
            let mut slot = [None];
            let mut fetched = 0u32;
            if handlers.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(h) = slot[0].take() else { break };
            let name = h.GetName().map(take_pwstr).unwrap_or_default().to_lowercase();
            let ui = h.GetUIName().map(take_pwstr).unwrap_or_default().to_lowercase();
            if name.contains("fastpic") || ui.contains("fastpic") {
                continue;
            }
            if let Some(rank) = EDITORS.iter().position(|e| ui.contains(e) || name.contains(e))
                && best.as_ref().is_none_or(|(r, _)| rank < *r)
            {
                best = Some((rank, h));
            }
        }
        let (_, handler) = best?;
        let item: IShellItem = SHCreateItemFromParsingName(file, None).ok()?;
        let data: IDataObject = item.BindToHandler(None, &BHID_DataObject).ok()?;
        handler.Invoke(&data).ok()
    }
}

/// FILETIME (100 ns egységben, 1601-től) → helyi idő, „2026.09.21. 14:05”.
pub fn format_filetime(ft: u64) -> Option<String> {
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let mut local = FILETIME::default();
    let mut st = SYSTEMTIME::default();
    unsafe {
        FileTimeToLocalFileTime(&ft, &mut local).ok()?;
        FileTimeToSystemTime(&local, &mut st).ok()?;
    }
    Some(format!("{}.{:02}.{:02}. {:02}:{:02}", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute))
}

// ---------------------------------------------------------------------------------------------
// Fájltársítás (HKCU, rendszergazdai jog nélkül). Windows 10/11 alatt az alapértelmezett
// alkalmazást a felhasználónak kell kiválasztania a Beállításokban; mi csak felkínáljuk magunkat.

const PROG_ID: &str = "FastPic.Image";
const CAPS: &str = r"Software\FastPic\Capabilities";

fn reg_set(root: HKEY, subkey: &str, name: Option<&str>, value: &str) -> bool {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyExW(
            root,
            &HSTRING::from(subkey),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        ) != ERROR_SUCCESS
        {
            return false;
        }
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let name = name.map(HSTRING::from);
        let pname = name.as_ref().map(|n| PCWSTR(n.as_ptr())).unwrap_or(PCWSTR::null());
        let ok = RegSetValueExW(key, pname, None, REG_SZ, Some(bytes)) == ERROR_SUCCESS;
        let _ = RegCloseKey(key);
        ok
    }
}

fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "fastpic.exe".into())
}

pub fn register() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = exe.to_string_lossy().into_owned();
    let cmd = format!("\"{exe}\" \"%1\"");
    let classes = r"Software\Classes";
    let app_key = format!(r"{classes}\Applications\{}", exe_name());
    let mut ok = true;
    ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{classes}\{PROG_ID}"), None, "FastPic kép");
    ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{classes}\{PROG_ID}\shell\open\command"), None, &cmd);
    ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{classes}\{PROG_ID}\DefaultIcon"), None, &format!("\"{exe}\",0"));
    ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{app_key}\shell\open\command"), None, &cmd);
    ok &= reg_set(HKEY_CURRENT_USER, &app_key, Some("FriendlyAppName"), "FastPic");
    ok &= reg_set(HKEY_CURRENT_USER, CAPS, Some("ApplicationName"), "FastPic");
    ok &= reg_set(HKEY_CURRENT_USER, CAPS, Some("ApplicationDescription"), "Gyors képnézegető");
    for ext in EXTENSIONS {
        ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{classes}\.{ext}\OpenWithProgids"), Some(PROG_ID), "");
        ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{app_key}\SupportedTypes"), Some(&format!(".{ext}")), "");
        ok &= reg_set(HKEY_CURRENT_USER, &format!(r"{CAPS}\FileAssociations"), Some(&format!(".{ext}")), PROG_ID);
    }
    ok &= reg_set(HKEY_CURRENT_USER, r"Software\RegisteredApplications", Some("FastPic"), CAPS);
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
        if ok {
            ShellExecuteW(
                None,
                w!("open"),
                w!("ms-settings:defaultapps?registeredAppUser=FastPic"),
                None,
                None,
                SW_SHOWNORMAL,
            );
        }
    }
    ok
}

pub fn unregister() {
    let classes = r"Software\Classes";
    unsafe {
        let del_tree = |sub: &str| {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(sub));
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(sub));
        };
        del_tree(&format!(r"{classes}\{PROG_ID}"));
        del_tree(&format!(r"{classes}\Applications\{}", exe_name()));
        del_tree(r"Software\FastPic");
        for ext in EXTENSIONS {
            let _ = RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(format!(r"{classes}\.{ext}\OpenWithProgids")),
                &HSTRING::from(PROG_ID),
            );
        }
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, w!(r"Software\RegisteredApplications"), w!("FastPic"));
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}
