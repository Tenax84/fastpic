# Release build + telepítő.
# Használat: .\scripts\build-release.ps1
# Eredmény: target\release\fastpic.exe és dist\FastPic-Setup-<verzió>.exe
# A natív parancsok (cargo, ISCC) sikerét a kilépési kódjukból ellenőrizzük.
$root = Resolve-Path "$PSScriptRoot\.."
Push-Location $root
try {
    cargo test --release
    if ($LASTEXITCODE -ne 0) { throw "A tesztek nem futottak le sikeresen." }
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "A release build nem sikerült." }

    $iscc = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $iscc) { throw "Az Inno Setup 6 nincs telepítve (https://jrsoftware.org/isdl.php)." }
    & $iscc /Qp "installer\fastpic.iss"
    if ($LASTEXITCODE -ne 0) { throw "A telepítő fordítása nem sikerült." }

    Get-ChildItem dist\*.exe | Sort-Object LastWriteTime | Select-Object -Last 1 |
        ForEach-Object { "Telepítő: {0}  ({1:N2} MB)" -f $_.FullName, ($_.Length / 1MB) }
} finally {
    Pop-Location
}
