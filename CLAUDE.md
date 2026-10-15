# FastPic

Gyors, minimális memóriájú képnézegető Windowsra és macOS-re.

**Állapot:** a Windows- (1–3. mérföldkő) és a macOS-verzió (4.) is kész, ikonnal és telepítővel (Inno Setup, illetve DMG). Hátravan: CI + kódaláírás (5.) – a macOS-bináris egyelőre csak ad-hoc aláírt, nincs notarizálva, és arm64-only.

## Alapkövetelmények (nem alkuképesek)

1. **Leggyorsabb indulás** – cél: < 100 ms a látható képig (12 MP JPEG, SSD).
2. **Minimális memória** – cél: < 30 MB egy képpel előtöltéssel (< 20 MB nélküle).
3. **Ablakállapot megjegyzése** – pozíció, méret, maximalizált állapot. Többmonitoros környezetben is helyes.
4. **Bezárás után a process kilép** – nincs tálcaikon, háttérszolgáltatás, rezidens rész.
5. **Minden megnyitott kép külön process** – nincs single-instance zár.
6. **Lapozás** – ◀ ▶ gombok a megnyitott kép mappájának többi képe között.

Minden döntésnél ezek élveznek elsőbbséget. Új függőség, init-lépés vagy háttérfeladat csak akkor kerülhet be, ha nem rontja az indulási időt és a memóriát – ezt mérni kell (`scripts/measure.ps1`, illetve `scripts/measure-mac.sh`), nem feltételezni.

## Technológia

- **Nyelv:** Rust (stable, edition 2024).
- **Nincs cross-platform GUI keretrendszer** (nincs winit, Qt, Tauri, egui stb.). Közvetlen natív API-k:
  - **Windows:** Win32 ablak + **GDI** kirajzolás + WIC dekódolás – `windows` crate.
  - **macOS:** AppKit (`NSApplication`, `NSWindow`, saját `NSView`) + `CALayer` (`CGImage` tartalom) + ImageIO dekódolás – `objc2`, `objc2-app-kit`, `objc2-foundation`, `objc2-core-graphics`, `objc2-image-io`, `objc2-quartz-core`, `objc2-core-foundation`, `libc`.
- **Egyéb crate-ek:** `image` (csak a szükséges feature-ökkel!), `jxl-oxide`, `resvg`, `kamadak-exif`.
- **Nincs async runtime.** `std::thread` + csatornák. macOS-en a szálak közti átadás `dispatch_async_f` a fő sorra (nincs `NSRunLoop`-forrás, nincs `objc` autorelease-ciklus a kritikus úton).
- Tartalék dekóderek pure Rust, statikusan linkelve – nincs külön DLL / dylib.

### Miért GDI és nem Direct2D (mért döntés)

Az eredeti terv Direct2D volt. Mérve a `CreateHwndRenderTarget` (D3D eszköz) **~90 ms**-ot vitt el a UI-szálon, és a GPU-driver miatt a private memória 76 MB volt. GDI-vel: első kép 160 → ~65 ms, working set 40 → ~19 MB, private 76 → 12 MB. Mivel a képet eleve kijelzőméretre dekódoljuk, a megjelenítés jellemzően 1:1-es `BitBlt`, amihez nem kell GPU. **Ne vezess vissza D2D-t/D3D-t mérés nélkül.**

### Miért van előnézet macOS-en (mért döntés)

macOS-en az ablak felállása sokkal drágább, mint Windowson: az `NSWindow` első létrehozása ~50 ms, az `NSApplication::run` belső inicializálása további ~45 ms – ezalatt a dekódolás rég kész van, de a kép nem látszik. Ezért az első képnél előbb egy **negyedméretű előnézet** megy ki (JPEG-nél 1/4-es DCT-skálázás: ~40 ms, szemben a végleges felbontás ~115 ms-ával), és a végleges felbontás rögtön mögé kerül a sorba. Mérve (12 MP JPEG, 7–7 futás mediánja): látható kép **173,6 → 105,4 ms**, ára 8 ms a végleges képen és ~1 MB. A `FASTPIC_NO_PREVIEW=1` környezeti változóval ugyanazzal a binárissal végigmérhető az előnézet nélküli út is – az A/B-t így kell újramérni.

## Projektstruktúra

Egyetlen crate, platformmodulok `cfg(windows)` / `cfg(target_os = "macos")` alatt. A `core/` platformfüggetlen, nem hivatkozhat `win/` vagy `mac/` modulra. A két platformréteg ugyanazt a `core`-t használja, és ugyanazt a szerkezetet követi (`mod.rs` = indulás, `window.rs` = App, `render.rs` = megjelenítés, `placement.rs` = ablakállapot, `shell.rs` = rendszerintegráció) – új funkciónál tartsd a párhuzamot.

```
src/
  main.rs            argumentumok, panic hook (a temp könyvtár fastpic-crash.txt fájlja)
  core/
    formats.rs       támogatott kiterjesztések, formátumfelismerés fájlfejléc alapján
    decode.rs        Decoded/Frame/Target, decode_file (natív → tartalék sorrend), EXIF-orientáció,
                     tartalék dekóderek: image crate, jxl-oxide, resvg, szabványon kívüli ICO
    worker.rs        egyetlen dekódoló szál, megszakítható prioritásos feladatsor (generikus eredménytípus)
    folder.rs        mappalista + Nav (lapozás, körbefordulás, törölt fájl kezelése)
    view.rs          zoom/pan/forgatás matek (kép-tér → képernyő mátrix)
    state.rs         ablakállapot (state.ini), atomi írás
    exif_info.rs     infópanel sorai
    update.rs        frissítés: verzió-összehasonlítás, releases/latest JSON-elemzés, napi ütemezés, SHA-256
  win/
    mod.rs           run(): indulási sorrend, bench mód, memóriamérés
    window.rs        App: üzenetkezelés, entry-cache, előtöltés, nagyfelbontás, billentyűk/egér
    render.rs        GDI: Texture (DIB-szekció), Renderer (hátsó puffer csak overlay-nél), gombok, infópanel
    wic.rs           WIC dekódolás közvetlenül DIB-be, JPEG DCT-prescale
    placement.rs     Get/SetWindowPlacement, teljes képernyő
    shell.rs         lomtár (IFileOperation), Ctrl+O párbeszéd, új példány, fájltársítás, StrCmpLogicalW
    update.rs        frissítés-ellenőrzés (--fetch-update gyerekprocess, WinHTTP), telepítő letöltése
  mac/
    mod.rs           run(): indulási sorrend, főmenü, bench mód, memóriamérés (mach task_info + phys_footprint)
    window.rs        App + FastPicView (NSView) + Delegate (NSApplicationDelegate): események, entry-cache,
                     előnézet + előtöltés, nagyfelbontás, billentyűk/egér/trackpad, fogd és vidd
    render.rs        CoreAnimation: Texture (CGImage), Renderer (CALayer-fa), gomb-sprite-ok, infópanel
    imageio.rs       ImageIO dekódolás célméretre (CGImageSourceCreateThumbnailAtIndex, WithTransform)
    placement.rs     NSWindow frame mentése/visszaállítása, NSScreen-ellenőrzés, zoomolt állapot
    shell.rs         Kuka (NSFileManager), ⌘O panel, új példány (open -n -a), szerkesztő, localizedStandardCompare
scripts/measure.ps1        indulási idő és memória mérése (Windows)
scripts/build-release.ps1  tesztek + release build + telepítő (dist\FastPic-Setup-<verzió>.exe)
scripts/measure-mac.sh     ugyanaz macOS-en (bash 3.2-kompatibilis, a guardon keresztül indít)
scripts/build-mac.sh       tesztek + release build + dist/FastPic.app + dist/FastPic-<verzió>.dmg
scripts/mac-run-guard.sh   GUI-teszt védőburok: saját folyamatcsoport, időkorlát, kóbor példányok leállítása
installer/fastpic.iss      Inno Setup 6 szkript (felhasználói vagy gépszintű telepítés, fájltársítások)
macos/Info.plist           csomag-metaadatok, támogatott fájltípusok (UTI + kiterjesztés), LSMinimumSystemVersion 11.0
assets/                    ikon: fastpic.svg (forrás), fastpic.ico (16–256 px), fastpic.png (256 px, macOS .icns forrása),
                           make_icon.py (újragenerálás)
fastpic.rc                 erőforrások: ikon (ID 1) + verzióinfó (build.rs fordítja rc.exe-vel)
fastpic.manifest           PerMonitorV2 DPI, Common Controls v6, hosszú útvonalak (build.rs ágyazza be)
```

## Formátumok

A dekódolás sorrendje közös (`core::decode::decode_file`): fejlécfelismerés → **natív** dekóder (WIC, illetve ImageIO) → **tartalék** (pure Rust). Csak a natív réteg különbözik.

| Formátum | Windows (natív: WIC) | macOS (natív: ImageIO) |
|---|---|---|
| JPEG, PNG, BMP, TIFF, ICO, GIF (első kocka) | WIC, közvetlenül célméretre; tartalék: `image` crate | ImageIO, célméretre; tartalék: `image` crate |
| HEIC, AVIF | csak telepített WIC-kodekkel; ha nincs, érthető hibaüzenet a Store-kiegészítőkről | natívan, kodek nélkül |
| WebP | **Először `image` crate** (a WIC WebP-kodek Store-kiegészítő, már az aktiválása ~75 ms), tartalék: WIC | szintén először `image` crate (közös ág), tartalék: ImageIO |
| JPEG XL | WIC, ha van kodek; különben `jxl-oxide` | ImageIO, ha tudja; különben `jxl-oxide` |
| SVG / SVGZ | `resvg`, célméretre renderelve, zoomkor újrarenderelve (max. 16384 px); rendszerfontok csak `<text>` esetén | ugyanaz (platformfüggetlen) |
| QOI, TGA, PNM, HDR, DDS, JPEG-XR | `image` crate (a JPEG-XR / DDS WIC-ből is) | `image` crate |
| RAW, PSD, EXR | **Kimarad** – sok munka / nagy függőség. Ne add hozzá kérés nélkül. | **Kimarad** (az ImageIO tudná, de a kiterjesztéslista közös) |

- Formátumfelismerés fájlfejléc (magic bytes) alapján, nem csak kiterjesztésből.
- EXIF Orientation:
  - **Windows:** a WIC-ből forgatás nélkül másolunk, az orientációt Rustban alkalmazzuk (`apply_orientation_into`). A WIC flip-rotatora skálázott forráson oszloponként újraszámolja a skálázást – nagyságrendekkel lassabb, ne használd. HEIF-nél az EXIF-et figyelmen kívül hagyjuk (a kodek már forgat).
  - **macOS:** a `kCGImageSourceCreateThumbnailWithTransform` már forgatva adja vissza a képet, itt nincs utómunka. A célméretet viszont a **forgatás utáni** oldalakra kell számolni (5–8 orientációnál a szélesség és a magasság felcserélődik).
- Hibás natív dekódolásnál jön a tartalék; ha az sem ismeri a formátumot, a natív hibaüzenet jelenik meg.
- **Animáció (GIF/WebP/APNG) most nem kell, de később igen:** a `Decoded` képkockák listáját tartalmazza (most mindig 1 elem).

## Funkciók és kezelés

A kezelés a két platformon azonos, csak a módosítóbillentyű és néhány rendszerszokás tér el (a macOS-eltérés a harmadik oszlopban).

| Funkció | Kezelés (Windows) | macOS-eltérés |
|---|---|---|
| Lapozás | ◀ ▶ overlay gombok (egérmozgásra jelennek meg, 1,5 mp után eltűnnek), ← → (ha nincs mit pannolni), PgUp/PgDn, Szóköz/Backspace, Home/End, egér vissza/előre gombok | ugyanaz; trackpad-görgetés csak akkor lapoz, ha nincs mit pannolni |
| Zoom | Egérgörgő (a kurzor körül), + / −, `0` = ablakhoz igazítás, `1` = 100% | + csippentés (`magnifyWithEvent:`); a görgő csak „nem precíz” eszköznél zoomol, trackpaden a natív görgetés/pan a viselkedés |
| Pan | Egérhúzás, nyilak (ha a kép nagyobb az ablaknál) | + trackpad-görgetés; nyitott/zárt kéz kurzor |
| Teljes képernyő | F11, Enter, dupla kattintás; Esc = kilépés a teljes képernyőből, egyébként bezárás | natív `toggleFullScreen:` (Mission Control-szerű) |
| Forgatás | R / Shift+R (csak nézet, fájlt nem ír) | ugyanaz |
| Törlés | Del → megerősítés → **Lomtárba** (`IFileOperation`, visszavonható) → következő kép | ⌦ vagy ⌘⌫ → megerősítés → **Kukába** (`NSFileManager.trashItemAtURL`) |
| EXIF-infó | `I` | ugyanaz |
| Szerkesztés | Ceruza gomb (jobb felső sarok, a lapozó gombokkal együtt jelenik meg) vagy `E`. Sorrend: Windows „Szerkesztés” ige → a fájltípus kezelői közül ismert képszerkesztő (`shell::EDITORS`, Store-appok is, pl. paint.net) → „Társítás ezzel” ablak. | macOS-en nincs „Szerkesztés” ige: a fájltípust kezelő appok közül (`NSWorkspace.URLsForApplicationsToOpenURL`) a legelőrébb való ismert szerkesztő (`mac::shell::EDITORS`, a lista végén az Előnézet). Ha egy sincs, hibaüzenet. |
| Megnyitás | Ctrl+O, fogd és vidd (mappát is elfogad: az első képét nyitja). Ha már van kép az ablakban, **új processt** indít. | ⌘O; ugyanez, plusz a Finderből / LaunchServicesből érkező megnyitás (lásd [Multi-process](#multi-process)) |
| Bezárás | Ctrl+W, Esc, ablak bezárása | ⌘W, Esc, ablak bezárása, ⌘Q (a minimális főmenüből) |
| Frissítés | Új kiadásnál értesítés az ablak alján (10 mp-ig, utána a gombokkal együtt), `U` = frissítés. Telepített példány: a telepítő letöltése és csendes futtatása, utána ugyanaz a kép nyílik újra. Hordozható: a kiadás oldala nyílik meg. | **Nincs** (csak Windows) |

Visszaváltáskor (ablakaktiválás) mindkét platformon frissül a mappalista, és ha az aktuális fájl módosult (mtime), újratöltődik; addig a régi kép látszik.

## Kulcs-megvalósítási részletek

### Indulás (sorrend a `win::run`-ban)
1. `state.ini` beolvasása → ebből a dekódolás célmérete még az ablak előtt.
2. Dekódoló szál indul az első feladattal – párhuzamosan az ablak létrehozásával.
3. Ablak létrehozása (`WS_EX_ACCEPTFILES`, sötét címsor), App telepítése, majd `SetWindowPlacement` → látható.
4. Mappalistázás csak az első kép megjelenése **után**, háttérszálon (`StrCmpLogicalW` rendezés).
5. Delay-load: shell32, shlwapi, ole32, oleaut32, combase, advapi32, winhttp – induláskor csak kernel32/user32/gdi32/dwmapi töltődik be. A COM-ot a dekódoló szál tölti be, párhuzamosan.

### Indulás (sorrend a `mac::run`-ban)
1. `state.ini` beolvasása (`~/Library/Application Support/FastPic/`). A **kliensméretet pixelben** mentjük, így a célméret az `NSScreen` (és így az AppKit betöltése) **nélkül** is megvan.
2. Dekódoló szál indul: előbb az előnézet (ha érdemes), mögötte rögtön a végleges felbontás – így a szál az előnézet után nem áll meg, nem vár az ablakra.
3. Innentől kell az AppKit: `NSScreen` (backing scale), `NSApplication`, `setActivationPolicy(Regular)`, delegate, `NSWindow` + `FastPicView`, `CALayer`-fa, App telepítése.
4. `SetFrame` a mentett helyre (`placement::apply`) → `makeKeyAndOrderFront` → `activate` → minimális főmenü (enélkül nem működne a ⌘Q).
5. `drain_decoded_now()` + `CATransaction::flush()` **még az `app.run()` előtt**: ha a dekóder már végzett, a `dispatch_async` munkája csak a futási hurokban indulna el, az `NSApplication::run` inicializálása viszont ~45 ms – ennyit várna a kész kép.
6. Mappalistázás csak az első kép megjelenése **után**, háttérszálon (`localizedStandardCompare` = Finder-rendezés).

### Dekódolás
- Mindig a **képernyőn látható méretre** dekódolunk (Fit: ablakméret; zoomnál a zoomolt méret; ≥100% esetén teljes felbontás). Így a kirajzolás 1:1. macOS-en a célméret pontban mért ablakméret × `backingScaleFactor` (Retinán kétszeres pixelméret – ez a memóriában is látszik).
- JPEG-skálázás a DCT-tartományban: Windowson `IWICBitmapSourceTransform` (1/2, 1/4, 1/8) **5% tűréssel**; macOS-en a `CGImageSourceCreateThumbnailAtIndex` + `kCGImageSourceThumbnailMaxPixelSize` ugyanezt intézi (a `…ShouldCacheImmediately` kell, különben a dekódolás a rajzoláskor, a fő szálon történne meg).
- Nincs fölösleges másolat: a WIC közvetlenül a GDI DIB-szekcióba ír, macOS-en a `CGImage` közvetlenül a réteg tartalma lesz. Az átlátszóságot a háttérszínre komponáljuk.
- A textúra (DIB / `CGImage`) a dekódoló szálon készül el; a UI-szál csak megjelenít.
- Nagyfelbontás/visszaváltás (`hires_target`): ha a látható méret > textúra × 1,05 → nagyobb; ha a textúra > látható × 2 → kisebb (memória). Debounce: 150–200 ms. Lapozáskor az elhagyott kép nagy textúrája eldobódik.
- **Előnézet (macOS):** csak az első képnél, és csak ha érdemes – `preview_target`: a célméret nagyobb oldala ≥ 1200 px **és** a fájl ≥ 400 KB. Az előnézet a célméret negyede. Amint megjelent, a végleges felbontást a `hires_target` kéri, 0-s prioritással (nem várhat az előtöltésre).

### Szálak
UI-szál + egy dekódoló szál (prioritás: aktuális kép 0 > aktuális nagyfelbontás 1 > következő 2 > előző 3; a `set_jobs` mindig a teljes kívánt listát kapja, a kimaradó futó feladat sávonként megszakad) + rövid életű listázó szál (első képnél és minden aktiváláskor).

A `Worker` **nyilvántartja az elkészült, de át nem vett feladatokat** (`Queue::done`): a UI addig is a kívánt listában tartja őket, amíg az eredményt fel nem dolgozza, és enélkül ugyanazt a képet még egyszer lefuttatná (ez a hiba mérhető volt: +11,7 MB working set). Megszakított feladat nem kerül a listára, az újra kérhető.

### Kirajzolás (GDI)
- Csak kép: közvetlenül az ablakba, a háttérrel csak a kép körüli részt töltjük ki (`ExcludeClipRect`) – nincs hátsó puffer.
- Overlay (gombok, infó, üzenet) esetén kliensméretű hátsó puffer, ami az overlay eltűnésekor felszabadul.
- Zoom: `StretchBlt` (kicsinyítés HALFTONE, nagyítás COLORONCOLOR), csak a látható részre. Forgatás: `PlgBlt`.
- Gombok: élsimított sprite-ok (CPU-n generálva), `GdiAlphaBlend`.

### Megjelenítés (macOS, CoreAnimation)
- **Nincs saját rajzolási ciklus és hátsó puffer.** A kép egy `CALayer` tartalma (`CGImage`), az overlay gombok és az infópanel további rétegek; a komponálást a window server végzi. A „rajzolás” csak réteg-tulajdonságok állítása.
- Az implicit animációkat ki kell kapcsolni (`CATransaction::setDisableActions`), különben a lapozás és a zoom „becsúszna”.
- Zoom: a réteg `bounds` + `contentsScale` (a `view.rs` mátrixából számolva), forgatás: `setAffineTransform`. Szűrő: 100% fölött, **teljes felbontású** textúránál `kCAFilterNearest` (éles pixelek), egyébként `kCAFilterTrilinear` – az előnézetet és a zoom közbeni köztes állapotot mindig simítva nagyítjuk.
- A gombok ugyanazok a CPU-n generált, élsimított sprite-ok, mint Windowson, `backingScaleFactor` szerinti skálán raszterizálva.

### Frissítés (csak Windows)
- **Nem a kritikus úton:** az első megjelenített kép után indul egy rövid életű szál, 3 mp-et vár, és csak akkor kérdez, ha esedékes. Bench módban nem fut.
- **Ütemezés:** `%APPDATA%\FastPic\update.ini` (`check=0` → soha; `last=` Unix-idő). Naponta legfeljebb egyszer; az időpontot a lekérés **előtt** írja be, így a párhuzamos példányok közül csak egy kérdez. Külön fájl, mert a `state.ini`-t bezáráskor egészében felülírja az utoljára záródó példány.
- **A lekérés külön processben fut** (`fastpic.exe --fetch-update`, a JSON-t a stdout-ra írja). Mért döntés: processen belül a WinHTTP a TLS-sel és a tanúsítványkezeléssel együtt a process végéig bent maradt (+5,3 MB working set, +1 MB private, +4 állandó szál). Gyerekprocesszel a nézegetőn mérve +0,1 MB. **Ne vidd vissza a processen belülre mérés nélkül.** A winhttp.dll emellett delay-load.
- `api.github.com/repos/Tenax84/fastpic/releases/latest` → `tag_name` (`vX.Y.Z`), `html_url`, és a `FastPic-Setup-` kezdetű `.exe` asset `browser_download_url` + `digest` (sha256) mezője. Saját mini JSON-elemző, serde nélkül. Letöltés csak a `github.com/Tenax84/fastpic/releases/download/` alól.
- **Telepített vagy hordozható:** az exe mellett van-e `unins000.exe`. Telepítettnél `U` → letöltés `%TEMP%\FastPic-update\` alá (ez processen belül fut, utána úgyis kilépünk), SHA-256-ellenőrzés, majd `ShellExecute`: `/SILENT /SP- /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS "/RELAUNCH=<kép>"`, és az ablak bezárul. A többi futó példányt az Inno restart managere zárja be, a végén az `.iss` `[Run]`-ja (`ShouldRelaunch`) nyitja újra a képet. Hordozhatónál `U` = a kiadás oldala a böngészőben.
- **Biztonság:** a lenyomat ugyanarról az API-ról jön, így csak a sérült letöltés ellen véd. Kódaláírás után (5. mérföldkő) kell mellé `WinVerifyTrust`.
- **Kiadáskor:** a tag `vX.Y.Z` legyen (a `CARGO_PKG_VERSION`-höz hasonlítunk), és a telepítő `FastPic-Setup-*.exe` néven kerüljön a kiadásba.

### Ablakállapot
- **Windows:** `GetWindowPlacement` / `SetWindowPlacement`; indításkor `MonitorFromRect` ellenőrzés – ha a monitor már nincs, alapértelmezett ablak.
- **macOS:** `NSWindow.frame` mentése/visszaállítása; a maximalizált állapot a `isZoomed` / `zoom(None)`. Indításkor `placement::is_on_screen`: ha a mentett téglalap egyik `NSScreen`-nel sem fed át, alapértelmezett méret + `center()`. A koordináta macOS-globális (origó a főképernyő **bal alsó** sarka), ezért a `WindowState` `top`/`bottom` mezői ott az alsó/felső élt jelentik.
- Tárolás: `%APPDATA%\FastPic\state.ini`, illetve `~/Library/Application Support/FastPic/state.ini` (`rect`, `maximized`, `client`). Atomi írás (temp + rename), az utoljára bezárt példány nyer. A `client` **pixelben** van (Retinán az ablakméret kétszerese) – ez indítja a dekódolást az ablak előtt.
- Teljes képernyőben bezárva a teljes képernyő előtti állapot mentődik. Bench módban nem ment.

### Kilépés
- **Windows:** `WM_DESTROY` → állapotmentés → `PostQuitMessage` → `std::process::exit(0)` (nem várunk a háttérszálakra).
- **macOS:** `windowWillClose:` → állapotmentés; `applicationShouldTerminateAfterLastWindowClosed` → `true`, és az `app.run()` után `std::process::exit(0)`. Nincs dokkban maradó, ablak nélküli példány.

### Multi-process
- **Windows:** nincs single-instance zár. Fogd és vidd / Ctrl+O foglalt ablakban → `spawn_viewer` új processt indít.
- **macOS:** a LaunchServices a futó példánynak küldi az új fájlt (`application:openFile:`). Az induló példány az első fájlt maga nyitja meg; minden később érkező fájlra **új processt indít** (`open -n -a <bundle>`; csomagon kívül futtatva közvetlenül a binárist). `applicationShouldTerminateAfterLastWindowClosed` → `true`.
- **Csapda (volt is belőle végtelen processlánc):** az AppKit az indulás során a *parancssori argumentumot is* visszaadja `openFile:`-ként. Ha ezt valódi megnyitási kérésnek vennénk, az új példány ugyanígy járna el – vég nélkül. Ezért a `LAUNCH_PATH`/`LAUNCH_DONE` páros az indulás alatt érkező, az argumentummal azonos megnyitást elnyeli (`is_launch_echo`). **Ezt az ágat ne írd át óvatlanul**, és GUI-teszthez használd a `scripts/mac-run-guard.sh`-t (a `FASTPIC_NO_SPAWN=1` a spawnolást is letiltja).

## Mért eredmények (Windows 11, release build, 12 MP JPEG)

| Mérőszám | Cél | Mért |
|---|---|---|
| Indulás → látható kép, 1184×761 ablak | < 100 ms | ~65 ms (közvetlen indítás), ~85 ms (`Start-Process`) |
| Indulás → látható kép, maximalizált 1920×1017 | < 100 ms | ~80–84 ms |
| Working set, 1184×761 ablak, előtöltéssel | < 30 MB | ~25 MB |
| Working set, maximalizált 1920×1017, előtöltéssel | < 30 MB | ~34 MB (3 × kijelzőméretű kép ≈ 16 MB + alap ~13 MB) |
| Exe méret | < 6 MB | 5,2 MB |

A maximalizált 1080p-s eset kicsit a cél felett van. A kritikus út a WIC JPEG-dekódolás (1/2-es DCT-skálázás ~50 ms); a gyorsabb út lehetne egy saját SIMD JPEG-dekóder, de ez nagy munka.

## Mért eredmények (macOS 26.6, Apple Silicon, release build, 12 MP JPEG, 1200×762 pt ablak Retinán = 2400×1524 px)

7–7 futás mediánja, `scripts/measure-mac.sh`.

| Mérőszám | Cél | Mért |
|---|---|---|
| Indulás → látható kép (előnézet) | < 100 ms | ~105 ms |
| Indulás → végleges felbontás | – | ~182 ms |
| `phys_footprint`, előtöltéssel | < 30 MB | ~40 MB |
| `resident_size` („working set”) | – | ~138 MB (ebben a megosztott rendszerkönyvtárak lapjai is benne vannak) |
| `FastPic.app` / DMG | – | 4,4 MB / 2,4 MB |

- **A memóriánál az `Activity Monitor` „Memory” oszlopa a `phys_footprint`** – ezt hasonlítsd a Windows-számokhoz, ne a `resident_size`-t. A ~40 MB a 30 MB-os cél felett van: Retinán a célméret kétszeres pixelben értendő (2400×1524 px ≈ 14,6 MB/textúra), és három kép van a memóriában (aktuális + előző + következő).
- **A fenti számok felső becslések:** a mérés bekapcsolt Low Power Mode mellett, nem üresjáratban futó gépen készült. Újramérés előtt: `pmset -g | grep lowpowermode`, és nézd meg a `load average`-et.
- A kritikus úton a legnagyobb egyben maradt tétel az **ablakteremtés, ~50 ms** (`mark_activation_policy` → `mark_window_created`): az első `NSWindow` behúzza az AppKit fél világát. Ha ezen kell javítani, ide tegyél almérőket először.

## Build és használat

### Közös

- `cargo build --release` → `target/release/fastpic[.exe]`. `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`; Windowson ezen felül statikus CRT (`.cargo/config.toml`).
- A dekóderek debug buildben is `opt-level = 3` (`profile.dev.package."*"`), különben a fejlesztés közbeni mérések félrevezetők.
- `fastpic [kép vagy mappa]` – megnyitás.
- `fastpic --bench <kimenet.txt> <kép>` – első látható kép (`preview_ms`) és első végleges felbontású kép (`first_image_ms`) ideje, memória, fázisidők (`mark_*`), majd kilép. Bench módban nem ment ablakállapotot.
- `cargo test` – a `core` unit tesztjei (formátum, lapozás, állapot, zoom/forgatás matek, orientáció, feladatsor, frissítés).
- `cargo test --release -- --ignored network` (Windows) – valódi lekérés a GitHubról, a telepítő letöltése és a lenyomat ellenőrzése.

### Windows
- `build.rs`: manifest beágyazása, `fastpic.rc` fordítása a Windows SDK `rc.exe`-jével (ikon + verzióinfó), és delay-load (`delayimp.lib` + `/DELAYLOAD`) – külön build-függőség nélkül.
- `.\scripts\build-release.ps1` – tesztek, release build, telepítő. Kell hozzá: Inno Setup 6. A verziószám az exe verzióinfójából jön (`fastpic.rc` → `FILEVERSION`; a `Cargo.toml` verziójával együtt emeld).
- A telepítő a `--register`-rel azonos registry-bejegyzéseket írja (HKCU vagy HKLM a telepítési módtól függően), eltávolításkor mindent töröl, a `%APPDATA%\FastPic`-et is. Új kiterjesztésnél a `formats.rs` `EXTENSIONS` listája mellett az `installer/fastpic.iss` `Exts` listáját is frissítsd.
- Ikon: az `assets/make_icon.py` generálja (Codex készítette); módosítás után futtasd újra.
- `fastpic.exe --fetch-update` – belső kapcsoló: a frissítés-ellenőrzés gyerekprocessze (a kiadás JSON-ját a stdout-ra írja).
- `fastpic.exe --register` / `--unregister` – fájltársítás HKCU alatt (rendszergazda nélkül), utána megnyílik a Beállítások „Alapértelmezett alkalmazások” oldala. Windows 11-en az alapértelmezést a felhasználónak kell kiválasztania.
- `.\scripts\measure.ps1 -Image <kép> [-Runs 5]` – ismételt mérés, medián.

### macOS

- `./scripts/build-mac.sh` – tesztek, release build, `.icns` (az `assets/fastpic.png`-ből, `sips` + `iconutil`), `dist/FastPic.app`, majd `dist/FastPic-<verzió>.dmg` (app + `Applications` symlink, a megszokott „húzd át” telepítés). Kapcsolók: `--no-dmg` (csak a csomag), `--install` (másolás `/Applications` alá + `lsregister -f`).
- A verzió a `macos/Info.plist` `CFBundleShortVersionString` mezőjéből jön – a `Cargo.toml` és a `fastpic.rc` verziójával együtt emeld.
- **Aláírás:** ad-hoc (`codesign --force --sign -`), mert Apple Siliconon aláírás nélkül nem futna a bináris. Más gépre átvitt DMG-nél ezért Gatekeeper-figyelmeztetés jön (jobb gomb → Megnyitás). Valódi Developer ID + notarizálás: 5. mérföldkő.
- A build **arm64-only**. Univerzális binárishoz `cargo build --release --target x86_64-apple-darwin` + `lipo -create` kellene; Intel Macen nincs tesztelve.
- `fastpic --register` / `--unregister`: macOS-en nincs registry, csak egy útmutató üzenet (a támogatott típusokat az `Info.plist` hirdeti, az alapértelmezettet a felhasználó állítja a Finderben: ⌘I → „Társítás ezzel” → „Összes módosítása”). Új kiterjesztésnél a `formats.rs` `EXTENSIONS` mellett a `macos/Info.plist` `CFBundleDocumentTypes` listáját is frissítsd.
- `scripts/measure-mac.sh <kép> [futások] [bináris]` – ismételt mérés, medián. `FASTPIC_NO_PREVIEW=1`-gyel az előnézet nélküli út is mérhető (A/B).
- `scripts/mac-run-guard.sh <mp> <parancs…>` – GUI-indítás védőburokkal.

## Munkaszabályok

- A felhasználóval magyarul kommunikálunk.
- Scope szűken tartva: sok munkát / nagy függőséget hozó formátumot vagy funkciót csak egyeztetés után.
- Minden teljesítményt érintő változásnál mérés előtte/utána (`--bench`, `measure.ps1` / `measure-mac.sh`). macOS-en mérés előtt ellenőrizd a Low Power Mode-ot (`pmset -g`) és a gép terheltségét – mindkettő meghamisítja az eredményt.
- Fájlokat ne PowerShell `Get-Content`/`Set-Content`-tel szerkessz: ANSI kódlappal olvas, és elrontja az ékezeteket. Használd az Edit/Write eszközt vagy Pythont UTF-8-cal.
- Futó `fastpic.exe` példányt csak a parancssora alapján állíts le (`Get-CimInstance Win32_Process` + `CommandLine` szűrés), név szerint soha – ezt egy hook is tiltja.
- **A FastPicet GUI-módban (nem `--bench`) csak a `scripts/mac-run-guard.sh`-n keresztül indítsd.** Egyszer már indított végtelen processláncot (lásd [Multi-process](#multi-process)); a guard saját folyamatcsoportba teszi, időkorláttal leállítja, és `FASTPIC_NO_SPAWN=1`-gyel letiltja az új példányok indítását.
- A `core/` közös: ha ott javítasz, az a másik platformot is érinti – írd meg a válaszban, hogy a Windows-ág mit kap.
