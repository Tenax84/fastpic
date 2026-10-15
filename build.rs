// Windows-specifikus linkelés közvetlenül az MSVC eszközökkel, külön build-függőség nélkül:
// manifest (PerMonitorV2 DPI, Common Controls v6, hosszú útvonalak), erőforrások (ikon,
// verzióinfó) és delay-load.
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=fastpic.manifest");
    println!("cargo:rerun-if-changed=fastpic.rc");
    println!("cargo:rerun-if-changed=assets/fastpic.ico");
    let target = std::env::var("TARGET").unwrap_or_default();
    if !(target.contains("windows") && target.contains("msvc")) {
        return;
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", root.join("fastpic.manifest").display());

    let res = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("fastpic.res");
    let rc = find_rc().expect("rc.exe nem található (Windows SDK kell)");
    let status = Command::new(rc)
        .current_dir(&root)
        .args(["/nologo", "/fo"])
        .arg(&res)
        .arg("fastpic.rc")
        .status()
        .expect("rc.exe futtatása");
    assert!(status.success(), "rc.exe hiba");
    println!("cargo:rustc-link-arg-bins={}", res.display());

    // Az induláshoz nem szükséges DLL-ek csak első használatkor töltődnek be
    // (a COM-ot pl. a dekódoló szál tölti be, párhuzamosan az ablakkal; a winhttp.dll-t a
    // frissítés-ellenőrzés, az első kép után).
    println!("cargo:rustc-link-arg-bins=delayimp.lib");
    for dll in ["shell32.dll", "shlwapi.dll", "ole32.dll", "oleaut32.dll", "combase.dll", "advapi32.dll", "winhttp.dll"]
    {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
}

/// A legújabb Windows SDK rc.exe-je (a PATH-on lévőt részesíti előnyben, pl. Developer Promptban).
fn find_rc() -> Option<PathBuf> {
    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return Some(PathBuf::from("rc.exe"));
    }
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let base = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    versions.sort();
    versions.into_iter().rev().map(|v| v.join(arch).join("rc.exe")).find(|p| p.exists())
}
