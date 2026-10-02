//! Natív dekódolás az ImageIO-val, közvetlenül célméretre.
//!
//! A `CGImageSourceCreateThumbnailAtIndex` a kért legnagyobb oldalhoz igazítva dekódol
//! (JPEG-nél a DCT-tartományban skálázva), és a `WithTransform` opcióval az EXIF-orientációt
//! is alkalmazza – ezért itt nincs szükség a `apply_orientation_into` utómunkára.

use std::ffi::c_void;
use std::path::Path;

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL, kCFBooleanTrue};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyOrientation, kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceShouldCacheImmediately, kCGImageSourceThumbnailMaxPixelSize,
};

use super::render::Texture;
use super::window::Prepared;
use crate::core::decode::{DecodeError, Target, fit_size};

/// A dekódoló szál elején fut (Windowson a COM inicializálása; itt nincs teendő).
pub fn thread_init() {}

fn cfurl(path: &Path) -> Option<CFRetained<CFURL>> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    unsafe { CFURL::from_file_system_representation(None, bytes.as_ptr(), bytes.len() as isize, false) }
}

unsafe fn dict_number(dict: &CFDictionary, key: &CFString) -> Option<f64> {
    let p = unsafe { dict.value(key as *const CFString as *const c_void) };
    if p.is_null() {
        return None;
    }
    unsafe { &*(p as *const CFNumber) }.as_f64()
}

/// `None`: az ImageIO nem ismeri a formátumot, jöhet a tartalék dekóder.
pub fn decode(
    path: &Path,
    target: Target,
    cancel: &dyn Fn() -> bool,
) -> Option<std::result::Result<Prepared, DecodeError>> {
    if !path.exists() {
        return Some(Err(DecodeError::NotFound));
    }
    let url = cfurl(path)?;
    let source = unsafe { CGImageSource::with_url(&url, None) }?;
    if unsafe { source.count() } == 0 {
        return None;
    }
    super::mark("io_source");

    // Eredeti méret és orientáció a metaadatokból: ebből számoljuk a célméretet.
    let props = unsafe { source.properties_at_index(0, None) }?;
    let (w, h) = unsafe {
        (dict_number(&props, kCGImagePropertyPixelWidth)?, dict_number(&props, kCGImagePropertyPixelHeight)?)
    };
    let orient = unsafe { dict_number(&props, kCGImagePropertyOrientation) }.unwrap_or(1.0) as u16;
    if w < 1.0 || h < 1.0 {
        return None;
    }
    // Az 5–8 orientációnál a megjelenített kép szélessége és magassága felcserélődik.
    let (ow, oh) = if (5..=8).contains(&orient) { (h as u32, w as u32) } else { (w as u32, h as u32) };
    let (tw, th) = fit_size(ow, oh, target, false);
    let max_side = tw.max(th).max(1);

    if cancel() {
        return Some(Err(DecodeError::Cancelled));
    }

    let keys: [&CFString; 4] = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceShouldCacheImmediately,
            kCGImageSourceThumbnailMaxPixelSize,
        ]
    };
    let size = CFNumber::new_i64(max_side as i64);
    let yes: &CFType = unsafe { kCFBooleanTrue }?.as_ref();
    let values: [&CFType; 4] = [yes, yes, yes, size.as_ref()];
    let options = CFDictionary::from_slices(&keys, &values);
    // A típusparaméter nélküli (opaque) szótárra hivatkozik az ImageIO API.
    let options: &CFDictionary =
        unsafe { &*(&*options as *const CFDictionary<CFString, CFType> as *const CFDictionary) };

    let img = unsafe { source.thumbnail_at_index(0, Some(options)) }?;
    super::mark("io_decoded");
    if cancel() {
        return Some(Err(DecodeError::Cancelled));
    }
    let tex = Texture::from_cgimage(img);
    let full = tex.width >= ow && tex.height >= oh;
    Some(Ok(Prepared { tex, orig_w: ow, orig_h: oh, full, vector: false }))
}
