# FastPic

Gyors, minimális memóriájú képnézegető Windowsra és macOS-re.

**Állapot:** a Windows-verzió kész (1–3. mérföldkő), ikonnal és Inno Setup telepítővel. Hátravan: macOS port (4.), CI + kódaláírás (5.).

## Alapkövetelmények (nem alkuképesek)

1. **Leggyorsabb indulás** – cél: < 100 ms a látható képig (12 MP JPEG, SSD).
2. **Minimális memória** – cél: < 30 MB egy képpel előtöltéssel (< 20 MB nélküle).
3. **Ablakállapot megjegyzése** – pozíció, méret, maximalizált állapot. Többmonitoros környezetben is helyes.
4. **Bezárás után a process kilép** – nincs tálcaikon, háttérszolgáltatás, rezidens rész.
5. **Minden megnyitott kép külön process** – nincs single-instance zár.
6. **Lapozás** – ◀ ▶ gombok a megnyitott kép mappájának többi képe között.

Minden döntésnél ezek élveznek elsőbbséget. Új függőség, init-lépés vagy háttérfeladat csak akkor kerülhet be, ha nem rontja az indulási időt és a memóriát – ezt mérni kell (`scripts/measure.ps1`), nem feltételezni.

## Technológia

- **Nyelv:** Rust (stable, edition 2024).
- **Nincs cross-platform GUI keretrendszer** (nincs winit, Qt, Tauri, egui stb.). Közvetlen natív API-k:
  - **Windows:** Win32 ablak + **GDI** kirajzolás + WIC dekódolás – `windows` crate.
  - **macOS (tervezett):** AppKit (`NSApplication`, `NSWindow`) + `CALayer` (`CGImage` tartalom) + ImageIO – `objc2`, `objc2-app-kit`, `objc2-foundation`.
- **Egyéb crate-ek:** `image` (csak a szükséges feature-ökkel!), `jxl-oxide`, `resvg`, `kamadak-exif`.
- **Nincs async runtime.** `std::thread` + csatornák.
- Tartalék dekóderek pure Rust, statikusan linkelve – nincs külön DLL.

### Miért GDI és nem Direct2D (mért döntés)

Az eredeti terv Direct2D volt. Mérve a `CreateHwndRenderTarget` (D3D eszköz) **~90 ms**-ot vitt el a UI-szálon, és a GPU-driver miatt a private memória 76 MB volt. GDI-vel: első kép 160 → ~65 ms, working set 40 → ~19 MB, private 76 → 12 MB. Mivel a képet eleve kijelzőméretre dekódoljuk, a megjelenítés jellemzően 1:1-es `BitBlt`, amihez nem kell GPU. **Ne vezess vissza D2D-t/D3D-t mérés nélkül.**

## Projektstruktúra

Egyetlen crate, platformmodulok `cfg(windows)` alatt. A `core/` platformfüggetlen, nem hivatkozhat `win/` vagy `mac/` modulra.

```
src/
  main.rs            argumentumok, panic hook (%TEMP%\fastpic-crash.txt)
  core/
    formats.rs       támogatott kiterjesztések, formátumfelismerés fájlfejléc alapján
    decode.rs        Decoded/Frame/Target, decode_file (natív → tartalék sorrend), EXIF-orientáció,
                     tartalék dekóderek: image crate, jxl-oxide, resvg, szabványon kívüli ICO
    worker.rs        egyetlen dekódoló szál, megszakítható prioritásos feladatsor (generikus eredménytípus)
    folder.rs        mappalista + Nav (lapozás, körbefordulás, törölt fájl kezelése)
    view.rs          zoom/pan/forgatás matek (kép-tér → képernyő mátrix)
    state.rs         ablakállapot (state.ini), atomi írás
    exif_info.rs     infópanel sorai
  win/
    mod.rs           run(): indulási sorrend, bench mód, memóriamérés
    window.rs        App: üzenetkezelés, entry-cache, előtöltés, nagyfelbontás, billentyűk/egér
    render.rs        GDI: Texture (DIB-szekció), Renderer (hátsó puffer csak overlay-nél), gombok, infópanel
    wic.rs           WIC dekódolás közvetlenül DIB-be, JPEG DCT-prescale
    placement.rs     Get/SetWindowPlacement, teljes képernyő
    shell.rs         lomtár (IFileOperation), Ctrl+O párbeszéd, új példány, fájltársítás, StrCmpLogicalW
scripts/measure.ps1        indulási idő és memória mérése
scripts/build-release.ps1  tesztek + release build + telepítő (dist\FastPic-Setup-<verzió>.exe)
installer/fastpic.iss      Inno Setup 6 szkript (felhasználói vagy gépszintű telepítés, fájltársítások)
assets/                    ikon: fastpic.svg (forrás), fastpic.ico (16–256 px), make_icon.py (újragenerálás)
fastpic.rc                 erőforrások: ikon (ID 1) + verzióinfó (build.rs fordítja rc.exe-vel)
fastpic.manifest           PerMonitorV2 DPI, Common Controls v6, hosszú útvonalak (build.rs ágyazza be)
```

## Formátumok

| Formátum | Dekóder |
|---|---|
| JPEG, PNG, BMP, TIFF, ICO, GIF (első kocka), JPEG-XR, DDS | WIC, közvetlenül célméretre; tartalék: `image` crate |
| WebP | **Először `image` crate** (a WIC WebP-kodek Store-kiegészítő, már az aktiválása ~75 ms), tartalék: WIC |
| JPEG XL | WIC, ha van kodek; különben `jxl-oxide` |
| SVG / SVGZ | `resvg`, célméretre renderelve, zoomkor újrarenderelve (max. 16384 px); rendszerfontok csak `<text>` esetén |
| QOI, TGA, PNM, HDR | `image` crate |
| HEIC, AVIF | csak WIC-kodekkel; ha nincs, érthető hibaüzenet a Store-kiegészítőkről |
| RAW, PSD, EXR | **Kimarad** – sok munka / nagy függőség. Ne add hozzá kérés nélkül. |

- Formátumfelismerés fájlfejléc (magic bytes) alapján, nem csak kiterjesztésből.
- EXIF Orientation: a WIC-ből forgatás nélkül másolunk, az orientációt Rustban alkalmazzuk (`apply_orientation_into`). A WIC flip-rotatora skálázott forráson oszloponként újraszámolja a skálázást – nagyságrendekkel lassabb, ne használd. HEIF-nél az EXIF-et figyelmen kívül hagyjuk (a kodek már forgat).
- Hibás natív dekódolásnál jön a tartalék; ha az sem ismeri a formátumot, a natív hibaüzenet jelenik meg.
- **Animáció (GIF/WebP/APNG) most nem kell, de később igen:** a `Decoded` képkockák listáját tartalmazza (most mindig 1 elem).

## Funkciók és kezelés

| Funkció | Kezelés |
|---|---|
| Lapozás | ◀ ▶ overlay gombok (egérmozgásra jelennek meg, 1,5 mp után eltűnnek), ← → (ha nincs mit pannolni), PgUp/PgDn, Szóköz/Backspace, Home/End, egér vissza/előre gombok |
| Zoom | Egérgörgő (a kurzor körül), + / −, `0` = ablakhoz igazítás, `1` = 100% |
| Pan | Egérhúzás, nyilak (ha a kép nagyobb az ablaknál) |
| Teljes képernyő | F11, Enter, dupla kattintás; Esc = kilépés a teljes képernyőből, egyébként bezárás |
| Forgatás | R / Shift+R (csak nézet, fájlt nem ír) |
| Törlés | Del → megerősítés → **Lomtárba** (`IFileOperation`, visszavonható) → következő kép |
| EXIF-infó | `I` |
| Szerkesztés | Ceruza gomb (jobb felső sarok, a lapozó gombokkal együtt jelenik meg) vagy `E`. Sorrend: Windows „Szerkesztés” ige → a fájltípus kezelői közül ismert képszerkesztő (`shell::EDITORS`, Store-appok is, pl. paint.net) → „Társítás ezzel” ablak. Visszaváltáskor, ha a fájl módosult (mtime), az aktuális kép újratöltődik; addig a régi látszik. |
| Megnyitás | Ctrl+O, fogd és vidd (mappát is elfogad: az első képét nyitja). Ha már van kép az ablakban, **új processt** indít. |
| Bezárás | Ctrl+W, Esc, ablak bezárása |

## Kulcs-megvalósítási részletek

### Indulás (sorrend a `win::run`-ban)
1. `state.ini` beolvasása → ebből a dekódolás célmérete még az ablak előtt.
2. Dekódoló szál indul az első feladattal – párhuzamosan az ablak létrehozásával.
3. Ablak létrehozása (`WS_EX_ACCEPTFILES`, sötét címsor), App telepítése, majd `SetWindowPlacement` → látható.
4. Mappalistázás csak az első kép megjelenése **után**, háttérszálon (`StrCmpLogicalW` rendezés).
5. Delay-load: shell32, shlwapi, ole32, oleaut32, combase, advapi32 – induláskor csak kernel32/user32/gdi32/dwmapi töltődik be. A COM-ot a dekódoló szál tölti be, párhuzamosan.

### Dekódolás
- Mindig a **képernyőn látható méretre** dekódolunk (Fit: ablakméret; zoomnál a zoomolt méret; ≥100% esetén teljes felbontás). Így a kirajzolás 1:1 `BitBlt`.
- JPEG: `IWICBitmapSourceTransform` DCT-skálázás (1/2, 1/4, 1/8), **5% tűréssel** (a legfeljebb 5%-kal kisebb méret is jó – akár kétszer gyorsabb). A prescale közvetlenül WIC-bitmap memóriájába dekódol.
- A WIC közvetlenül a GDI DIB-szekcióba ír (nincs köztes `Vec` + másolat); az átlátszóságot a háttérszínre komponáljuk.
- A textúra (DIB) a dekódoló szálon készül el; a UI-szál csak kirajzol.
- Nagyfelbontás/visszaváltás (`hires_target`): ha a látható méret > textúra × 1,05 → nagyobb; ha a textúra > látható × 2 → kisebb (memória). Debounce: 150–200 ms. Lapozáskor az elhagyott kép nagy textúrája eldobódik.

### Szálak
UI-szál + egy dekódoló szál (prioritás: aktuális kép 0 > aktuális nagyfelbontás 1 > következő 2 > előző 3; a `set_jobs` mindig a teljes kívánt listát kapja, a kimaradó futó feladat sávonként megszakad) + rövid életű listázó szál (első képnél és minden aktiváláskor).

### Kirajzolás (GDI)
- Csak kép: közvetlenül az ablakba, a háttérrel csak a kép körüli részt töltjük ki (`ExcludeClipRect`) – nincs hátsó puffer.
- Overlay (gombok, infó, üzenet) esetén kliensméretű hátsó puffer, ami az overlay eltűnésekor felszabadul.
- Zoom: `StretchBlt` (kicsinyítés HALFTONE, nagyítás COLORONCOLOR), csak a látható részre. Forgatás: `PlgBlt`.
- Gombok: élsimított sprite-ok (CPU-n generálva), `GdiAlphaBlend`.

### Ablakállapot
- `GetWindowPlacement` / `SetWindowPlacement`; indításkor `MonitorFromRect` ellenőrzés – ha a monitor már nincs, alapértelmezett ablak.
- Tárolás: `%APPDATA%\FastPic\state.ini` (`rect`, `maximized`, `client`). Atomi írás (temp + rename), az utoljára bezárt példány nyer.
- Teljes képernyőben bezárva a teljes képernyő előtti állapot mentődik. Bench módban nem ment.

### Kilépés
`WM_DESTROY` → állapotmentés → `PostQuitMessage` → `std::process::exit(0)` (nem várunk a háttérszálakra).

### Multi-process
- **Windows:** nincs single-instance zár. Fogd és vidd / Ctrl+O foglalt ablakban → `spawn_viewer` új processt indít.
- **macOS (tervezett):** a LaunchServices a futó példánynak küldi az új fájlt (`application:openURLs:`). Az induló példány az első fájlt maga nyitja meg; minden később érkező fájlra **új processt indít** (`NSWorkspace.openApplication` + `createsNewApplicationInstance = true`). `applicationShouldTerminateAfterLastWindowClosed` → `true`.

## Mért eredmények (Windows 11, release build, 12 MP JPEG)

| Mérőszám | Cél | Mért |
|---|---|---|
| Indulás → látható kép, 1184×761 ablak | < 100 ms | ~65 ms (közvetlen indítás), ~85 ms (`Start-Process`) |
| Indulás → látható kép, maximalizált 1920×1017 | < 100 ms | ~80–84 ms |
| Working set, 1184×761 ablak, előtöltéssel | < 30 MB | ~25 MB |
| Working set, maximalizált 1920×1017, előtöltéssel | < 30 MB | ~34 MB (3 × kijelzőméretű kép ≈ 16 MB + alap ~13 MB) |
| Exe méret | < 6 MB | 5,2 MB |

A maximalizált 1080p-s eset kicsit a cél felett van. A kritikus út a WIC JPEG-dekódolás (1/2-es DCT-skálázás ~50 ms); a gyorsabb út lehetne egy saját SIMD JPEG-dekóder, de ez nagy munka.

## Build és használat

- `cargo build --release` → `target/release/fastpic.exe`. Statikus CRT (`.cargo/config.toml`), `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`.
- `build.rs`: manifest beágyazása, `fastpic.rc` fordítása a Windows SDK `rc.exe`-jével (ikon + verzióinfó), és delay-load (`delayimp.lib` + `/DELAYLOAD`) – külön build-függőség nélkül.
- `.\scripts\build-release.ps1` – tesztek, release build, telepítő. Kell hozzá: Inno Setup 6. A verziószám az exe verzióinfójából jön (`fastpic.rc` → `FILEVERSION`; a `Cargo.toml` verziójával együtt emeld).
- A telepítő a `--register`-rel azonos registry-bejegyzéseket írja (HKCU vagy HKLM a telepítési módtól függően), eltávolításkor mindent töröl, a `%APPDATA%\FastPic`-et is. Új kiterjesztésnél a `formats.rs` `EXTENSIONS` listája mellett az `installer/fastpic.iss` `Exts` listáját is frissítsd.
- Ikon: az `assets/make_icon.py` generálja (Codex készítette); módosítás után futtasd újra.
- `fastpic.exe [kép vagy mappa]` – megnyitás.
- `fastpic.exe --register` / `--unregister` – fájltársítás HKCU alatt (rendszergazda nélkül), utána megnyílik a Beállítások „Alapértelmezett alkalmazások” oldala. Windows 11-en az alapértelmezést a felhasználónak kell kiválasztania.
- `fastpic.exe --bench <kimenet.txt> <kép>` – első kép ideje, memória, fázisidők (`mark_*`), majd kilép.
- `.\scripts\measure.ps1 -Image <kép> [-Runs 5]` – ismételt mérés, medián.
- `cargo test` – a `core` unit tesztjei (formátum, lapozás, állapot, zoom/forgatás matek, orientáció).

## Munkaszabályok

- A felhasználóval magyarul kommunikálunk.
- Scope szűken tartva: sok munkát / nagy függőséget hozó formátumot vagy funkciót csak egyeztetés után.
- Minden teljesítményt érintő változásnál mérés előtte/utána (`--bench`, `measure.ps1`).
- Fájlokat ne PowerShell `Get-Content`/`Set-Content`-tel szerkessz: ANSI kódlappal olvas, és elrontja az ékezeteket. Használd az Edit/Write eszközt vagy Pythont UTF-8-cal.
- Futó `fastpic.exe` példányt csak a parancssora alapján állíts le (`Get-CimInstance Win32_Process` + `CommandLine` szűrés), név szerint soha – ezt egy hook is tiltja.
