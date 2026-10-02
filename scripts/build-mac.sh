#!/bin/bash
# FastPic release build, alkalmazáscsomag és telepítő macOS-re.
#   ./scripts/build-mac.sh            – tesztek, release build, dist/FastPic.app, dist/FastPic-<verzió>.dmg
#   ./scripts/build-mac.sh --install  – utána telepítés is /Applications alá
#   ./scripts/build-mac.sh --no-dmg   – csak az alkalmazáscsomag, telepítő nélkül
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

APP="dist/FastPic.app"
INSTALL_DIR="/Applications"
MAKE_DMG=1
DO_INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --install) DO_INSTALL=1 ;;
    --no-dmg)  MAKE_DMG=0 ;;
    *) echo "ismeretlen kapcsoló: $arg" >&2; exit 2 ;;
  esac
done

echo "== tesztek =="
cargo test --quiet

echo "== release build =="
cargo build --release

echo "== ikon =="
ICONSET="target/fastpic.iconset"
rm -rf "$ICONSET" && mkdir -p "$ICONSET"
# A forrás 256 px-es; csak lefelé méretezünk, hogy ne legyen elmosódott.
sips -z 16 16   assets/fastpic.png --out "$ICONSET/icon_16x16.png"      >/dev/null
sips -z 32 32   assets/fastpic.png --out "$ICONSET/icon_16x16@2x.png"   >/dev/null
cp "$ICONSET/icon_16x16@2x.png" "$ICONSET/icon_32x32.png"
sips -z 64 64   assets/fastpic.png --out "$ICONSET/icon_32x32@2x.png"   >/dev/null
sips -z 128 128 assets/fastpic.png --out "$ICONSET/icon_128x128.png"    >/dev/null
cp assets/fastpic.png "$ICONSET/icon_128x128@2x.png"
cp assets/fastpic.png "$ICONSET/icon_256x256.png"
iconutil -c icns "$ICONSET" -o target/fastpic.icns

echo "== csomag =="
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp macos/Info.plist "$APP/Contents/Info.plist"
cp target/release/fastpic "$APP/Contents/MacOS/fastpic"
cp target/fastpic.icns "$APP/Contents/Resources/fastpic.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc aláírás: Apple Silicon alatt aláírás nélkül nem futna a bináris.
codesign --force --sign - --timestamp=none "$APP" >/dev/null 2>&1
codesign --verify --deep "$APP"

SIZE=$(du -sh "$APP" | cut -f1)
echo "kész: $APP ($SIZE)"

if [ "$MAKE_DMG" = 1 ]; then
  echo "== telepítő =="
  # A verzió az Info.plist-ből jön, ahogy Windowson az exe verzióinfójából.
  VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")
  DMG="dist/FastPic-$VERSION.dmg"
  STAGE="target/dmg-stage"
  rm -rf "$STAGE" && mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/FastPic.app"
  # A szokásos macOS telepítési mozdulat: húzd az appot az Applications mappára.
  ln -s /Applications "$STAGE/Applications"
  rm -f "$DMG"
  hdiutil create -volname "FastPic $VERSION" -srcfolder "$STAGE" \
    -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
  rm -rf "$STAGE"
  echo "kész: $DMG ($(du -sh "$DMG" | cut -f1))"
fi

if [ "$DO_INSTALL" = 1 ]; then
  echo "== telepítés =="
  # A futó példányokat a telepítés nem állítja le; azok a régi binárist használják tovább.
  rm -rf "$INSTALL_DIR/FastPic.app"
  cp -R "$APP" "$INSTALL_DIR/FastPic.app"
  # A LaunchServices csak akkor ismeri fel az új verziót, ha újraregisztráljuk.
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -f "$INSTALL_DIR/FastPic.app" || true
  echo "telepítve: $INSTALL_DIR/FastPic.app"
fi
