//! Dekódolt kép típusai és a pure Rust tartalék dekóderek (JPEG XL, SVG, `image` crate).

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use super::formats::{Sniffed, sniff};

/// Egy dekódolt kép. A pixelek premultiplied BGRA sorrendben, stride = `width * 4`.
/// A képkockák listája a későbbi animációtámogatásnak készült; most mindig egy elemű.
pub struct Decoded {
    pub frames: Vec<Frame>,
    /// A teljes felbontású, EXIF szerint elforgatott méret. Vektoros képnél az intrinsic méret.
    pub orig_width: u32,
    pub orig_height: u32,
    /// Nincs nagyobb felbontás, amit érdemes lenne kérni.
    pub full: bool,
    pub vector: bool,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Férjen bele ebbe a dobozba. Rasztert nem nagyít, vektort igen.
    Fit(u32, u32),
    /// Teljes felbontás (rasztereknél).
    Full,
}

#[derive(Debug, Clone)]
pub enum DecodeError {
    NotFound,
    Cancelled,
    /// A formátumhoz nincs dekóder (pl. hiányzó HEIF-kodek). A szöveg a felhasználónak szól.
    Unsupported(String),
    Failed(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NotFound => f.write_str("A fájl nem található."),
            DecodeError::Cancelled => f.write_str("Megszakítva."),
            DecodeError::Unsupported(s) | DecodeError::Failed(s) => f.write_str(s),
        }
    }
}

pub type DecodeResult = Result<Decoded, DecodeError>;

/// A cél-dobozhoz tartozó méret adott forrásmérethez.
pub fn fit_size(w: u32, h: u32, target: Target, allow_upscale: bool) -> (u32, u32) {
    match target {
        Target::Full => (w, h),
        Target::Fit(tw, th) => {
            if w == 0 || h == 0 || tw == 0 || th == 0 {
                return (w.max(1), h.max(1));
            }
            let mut s = (tw as f64 / w as f64).min(th as f64 / h as f64);
            if !allow_upscale {
                s = s.min(1.0);
            }
            (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
        }
    }
}

/// A platform natív dekódere. `None`: nem ismeri a formátumot.
pub type NativeFn<'a, T> = &'a dyn Fn(&Path, Target, &dyn Fn() -> bool) -> Option<Result<T, DecodeError>>;

/// Megkeresi a fájl típusát, és a natív vagy a tartalék dekódert futtatja.
/// `T` a platform megjelenítésre kész eredménye: a natív dekóder közvetlenül ezt adja
/// (így nincs fölösleges másolat), a tartalék dekóderek kimenetét `convert` alakítja át.
pub fn decode_file<T>(
    path: &Path,
    target: Target,
    cancel: &dyn Fn() -> bool,
    native: NativeFn<T>,
    convert: &dyn Fn(Decoded) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
    let mut head = [0u8; 512];
    let n = match File::open(path) {
        Ok(mut f) => f.read(&mut head).unwrap_or(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(DecodeError::NotFound),
        Err(e) => return Err(DecodeError::Failed(format!("A fájl nem olvasható: {e}"))),
    };
    let head = &head[..n];
    if n == 0 {
        return Err(DecodeError::Failed("A fájl üres.".into()));
    }

    match sniff(path, head) {
        Sniffed::Svg => decode_svg(path, target).and_then(convert),
        Sniffed::Jxl => {
            // Újabb Windows / macOS natívan is tudja; ha nem, jön a jxl-oxide.
            if let Some(Ok(d)) = native(path, target, cancel) {
                return Ok(d);
            }
            decode_jxl(path).and_then(convert)
        }
        // Windowson a WebP-kodek Store-kiegészítő, aminek már az aktiválása ~75 ms;
        // a pure Rust dekóder ennél gyorsabban végez. Ha mégis hibázik, jöhet a natív.
        Sniffed::WebP => match decode_with_image_crate(path) {
            Ok(d) => convert(d),
            Err(e) => native(path, target, cancel).unwrap_or(Err(e)),
        },
        Sniffed::Heif => match native(path, target, cancel) {
            Some(r) => r.map_err(|e| match e {
                DecodeError::Failed(_) | DecodeError::Unsupported(_) => heif_missing(),
                other => other,
            }),
            None => Err(heif_missing()),
        },
        Sniffed::Other => {
            let native_err = match native(path, target, cancel) {
                Some(Ok(d)) => return Ok(d),
                Some(Err(e @ (DecodeError::Cancelled | DecodeError::NotFound))) => return Err(e),
                Some(Err(e)) => Some(e),
                None => None,
            };
            if cancel() {
                return Err(DecodeError::Cancelled);
            }
            // Ha a natív dekóder ismerte a formátumot, de hibát adott (pl. sérült JPEG),
            // és a tartalék nem is tudja a formátumot, a natív hibaüzenet a hasznosabb.
            let r = match (decode_with_image_crate(path), native_err) {
                (Ok(d), _) => Ok(d),
                (Err(e), _) if head.starts_with(&[0, 0, 1, 0]) => decode_ico_png(path).ok_or(e),
                (Err(DecodeError::Unsupported(_)), Some(e)) => Err(e),
                (r, _) => r,
            };
            r.and_then(convert)
        }
    }
}

fn heif_missing() -> DecodeError {
    DecodeError::Unsupported(
        "HEIC/AVIF képhez telepítsd a Microsoft Store-ból a „HEIF Image Extensions” \
         és az „AV1 Video Extension” kiegészítőt."
            .into(),
    )
}

/// EXIF Orientation (1–8) alkalmazása egy 4 bájt/pixeles pufferre. Az 5–8 esetben
/// a szélesség és a magasság felcserélődik. Visszaadja az új puffert és méretet.
#[cfg(test)]
pub fn apply_orientation(src: Vec<u8>, w: u32, h: u32, orient: u16) -> (Vec<u8>, u32, u32) {
    if !(2..=8).contains(&orient) {
        return (src, w, h);
    }
    let mut out = vec![0u8; src.len()];
    let (ow, oh) = apply_orientation_into(&src, w, h, orient, &mut out);
    (out, ow, oh)
}

/// Mint az `apply_orientation`, de a hívó által adott (azonos méretű) pufferbe ír.
pub fn apply_orientation_into(src: &[u8], w: u32, h: u32, orient: u16, out: &mut [u8]) -> (u32, u32) {
    let (w, h) = (w as usize, h as usize);
    assert!(src.len() >= w * h * 4 && out.len() >= w * h * 4);
    let swaps = orient >= 5;
    let (ow, oh) = if swaps { (h, w) } else { (w, h) };
    if !(2..=8).contains(&orient) {
        out[..w * h * 4].copy_from_slice(&src[..w * h * 4]);
        return (ow as u32, oh as u32);
    }
    let s: &[[u8; 4]] = unsafe { std::slice::from_raw_parts(src.as_ptr() as *const [u8; 4], w * h) };
    let d: &mut [[u8; 4]] = unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr() as *mut [u8; 4], w * h) };
    for y in 0..h {
        let row = &s[y * w..(y + 1) * w];
        for (x, px) in row.iter().enumerate() {
            let (dx, dy) = match orient {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x), // 8
            };
            d[dy * ow + dx] = *px;
        }
    }
    (ow as u32, oh as u32)
}

/// RGBA8 (nem premultiplied) → premultiplied BGRA, helyben.
pub fn rgba_to_pbgra(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        let (r, g, b) = (p[0] as u32, p[1] as u32, p[2] as u32);
        if a == 255 {
            p[0] = b as u8;
            p[2] = r as u8;
        } else {
            p[0] = ((b * a + 127) / 255) as u8;
            p[1] = ((g * a + 127) / 255) as u8;
            p[2] = ((r * a + 127) / 255) as u8;
        }
    }
}

fn from_dynamic(img: image::DynamicImage) -> Decoded {
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    rgba_to_pbgra(&mut pixels);
    Decoded {
        frames: vec![Frame { width: w, height: h, pixels }],
        orig_width: w,
        orig_height: h,
        full: true,
        vector: false,
    }
}

fn decode_with_image_crate(path: &Path) -> DecodeResult {
    use image::ImageDecoder;
    let reader = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| DecodeError::Failed(format!("A fájl nem olvasható: {e}")))?;
    if reader.format().is_none() {
        return Err(DecodeError::Unsupported("Ismeretlen vagy nem támogatott képformátum.".into()));
    }
    let mut decoder = reader.into_decoder().map_err(image_err)?;
    let orientation = decoder.orientation().ok();
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(image_err)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(from_dynamic(img))
}

fn image_err(e: image::ImageError) -> DecodeError {
    match e {
        image::ImageError::Unsupported(u) => DecodeError::Unsupported(format!("Nem támogatott: {u}")),
        other => DecodeError::Failed(format!("Hibás vagy sérült kép: {other}")),
    }
}

/// Szabványon kívüli ICO (pl. a Pillow RGB PNG-t ment bele, amit a WIC és az `image` crate
/// is elutasít): a legnagyobb PNG-bejegyzést közvetlenül dekódoljuk.
fn decode_ico_png(path: &Path) -> Option<Decoded> {
    let data = std::fs::read(path).ok()?;
    let count = u16::from_le_bytes([*data.get(4)?, *data.get(5)?]) as usize;
    let mut best: Option<(u32, &[u8])> = None;
    for i in 0..count {
        let e = data.get(6 + i * 16..6 + i * 16 + 16)?;
        let side = |b: u8| if b == 0 { 256 } else { b as u32 };
        let area = side(e[0]) * side(e[1]);
        let size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        let off = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        let img = data.get(off..off.checked_add(size)?)?;
        if img.starts_with(b"\x89PNG") && best.is_none_or(|(a, _)| area > a) {
            best = Some((area, img));
        }
    }
    let img = image::load_from_memory_with_format(best?.1, image::ImageFormat::Png).ok()?;
    Some(from_dynamic(img))
}

fn decode_jxl(path: &Path) -> DecodeResult {
    let file = File::open(path).map_err(|e| DecodeError::Failed(e.to_string()))?;
    let decoder = jxl_oxide::integration::JxlDecoder::new(BufReader::new(file))
        .map_err(|e| DecodeError::Failed(format!("Hibás JPEG XL: {e}")))?;
    let img =
        image::DynamicImage::from_decoder(decoder).map_err(|e| DecodeError::Failed(format!("Hibás JPEG XL: {e}")))?;
    Ok(from_dynamic(img))
}

/// A leghosszabb oldal, amire egy SVG-t renderelünk (memória- és időkorlát).
pub const MAX_VECTOR_SIDE: u32 = 16384;

fn decode_svg(path: &Path, target: Target) -> DecodeResult {
    use resvg::{tiny_skia, usvg};
    let data = std::fs::read(path).map_err(|e| DecodeError::Failed(e.to_string()))?;
    let mut opt = usvg::Options { resources_dir: path.parent().map(|p| p.to_path_buf()), ..usvg::Options::default() };
    // A rendszerfontok betöltése több száz ms is lehet, ezért csak akkor, ha van szöveg.
    // (Tömörített .svgz-nél nem látjuk előre, ott mindig betöltjük.)
    let compressed = data.starts_with(&[0x1f, 0x8b]);
    if compressed || data.windows(5).any(|w| w == b"<text") {
        opt.fontdb_mut().load_system_fonts();
    }
    let tree = usvg::Tree::from_data(&data, &opt).map_err(|e| DecodeError::Failed(format!("Hibás SVG: {e}")))?;
    let size = tree.size();
    let (iw, ih) = (size.width().max(1.0), size.height().max(1.0));
    let (ow, oh) = (iw.ceil() as u32, ih.ceil() as u32);

    let (mut w, mut h) = fit_size(ow, oh, target, true);
    let long = w.max(h);
    if long > MAX_VECTOR_SIDE {
        let s = MAX_VECTOR_SIDE as f64 / long as f64;
        w = ((w as f64 * s) as u32).max(1);
        h = ((h as f64 * s) as u32).max(1);
    }
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or_else(|| DecodeError::Failed("Túl nagy SVG-méret.".into()))?;
    let transform = tiny_skia::Transform::from_scale(w as f32 / iw, h as f32 / ih);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // A tiny-skia premultiplied RGBA-t ad; csak R és B cseréje kell.
    let mut pixels = pixmap.take();
    for p in pixels.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Ok(Decoded {
        frames: vec![Frame { width: w, height: h, pixels }],
        orig_width: ow,
        orig_height: oh,
        full: false,
        vector: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit() {
        assert_eq!(fit_size(4000, 3000, Target::Fit(800, 600), false), (800, 600));
        assert_eq!(fit_size(4000, 2000, Target::Fit(800, 600), false), (800, 400));
        assert_eq!(fit_size(100, 50, Target::Fit(800, 600), false), (100, 50));
        assert_eq!(fit_size(100, 50, Target::Fit(800, 600), true), (800, 400));
        assert_eq!(fit_size(100, 50, Target::Full, false), (100, 50));
    }

    #[test]
    fn orientation() {
        // 3×2 kép, a pixelek értéke az (x, y) pozíciót kódolja.
        let px = |x: u8, y: u8| [x, y, 0, 255];
        let mut src = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                src.extend_from_slice(&px(x, y));
            }
        }
        let at = |buf: &[u8], w: u32, x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [buf[i], buf[i + 1]]
        };
        // 6: 90° az óramutató szerint → a bal felső sarok a jobb felsőbe kerül.
        let (o, w, h) = apply_orientation(src.clone(), 3, 2, 6);
        assert_eq!((w, h), (2, 3));
        assert_eq!(at(&o, w, 1, 0), [0, 0]);
        assert_eq!(at(&o, w, 0, 2), [2, 1]);
        // 8: 270° → a bal felső sarok a bal alsóba.
        let (o, w, h) = apply_orientation(src.clone(), 3, 2, 8);
        assert_eq!((w, h), (2, 3));
        assert_eq!(at(&o, w, 0, 2), [0, 0]);
        // 3: 180°.
        let (o, w, _) = apply_orientation(src.clone(), 3, 2, 3);
        assert_eq!(at(&o, w, 2, 1), [0, 0]);
        // 5: transzponálás.
        let (o, w, _) = apply_orientation(src.clone(), 3, 2, 5);
        assert_eq!(at(&o, w, 1, 2), [2, 1]);
        // 1: változatlan.
        let (o, _, _) = apply_orientation(src.clone(), 3, 2, 1);
        assert_eq!(o, src);
    }

    #[test]
    fn premultiply() {
        let mut px = [255, 0, 0, 128, 10, 20, 30, 255];
        rgba_to_pbgra(&mut px);
        assert_eq!(px, [0, 0, 128, 128, 30, 20, 10, 255]);
    }
}
