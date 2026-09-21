//! Natív dekódolás a Windows Imaging Component-tel, közvetlenül célméretre.

use std::cell::RefCell;
use std::path::Path;

use windows::Win32::Foundation::{
    E_ABORT, E_OUTOFMEMORY, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ, WINCODEC_ERR_COMPONENTNOTFOUND,
    WINCODEC_ERR_UNKNOWNIMAGEFORMAT,
};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Variant::VT_UI2;
use windows::core::{Error, HSTRING, Interface, Result, w};

use super::render::{Texture, composite_on_background};
use super::window::Prepared;
use crate::core::decode::{DecodeError, Target, apply_orientation_into, fit_size};

thread_local! {
    static FACTORY: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

/// A dekódoló szál elején fut.
pub fn thread_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn factory() -> Result<IWICImagingFactory> {
    FACTORY.with(|f| {
        if let Some(fac) = &*f.borrow() {
            return Ok(fac.clone());
        }
        let fac: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        *f.borrow_mut() = Some(fac.clone());
        Ok(fac)
    })
}

/// `None`: a WIC nem ismeri a formátumot, jöhet a tartalék dekóder.
/// Az eredmény közvetlenül GDI-bitmapbe kerül, köztes CPU-oldali másolat nélkül.
pub fn decode(
    path: &Path,
    target: Target,
    cancel: &dyn Fn() -> bool,
) -> Option<std::result::Result<Prepared, DecodeError>> {
    match unsafe { decode_inner(path, target, cancel) } {
        Ok(d) => Some(Ok(d)),
        Err(e) => {
            let code = e.code();
            if code == WINCODEC_ERR_COMPONENTNOTFOUND || code == WINCODEC_ERR_UNKNOWNIMAGEFORMAT {
                None
            } else if code == E_ABORT {
                Some(Err(DecodeError::Cancelled))
            } else if code == ERROR_FILE_NOT_FOUND.to_hresult() || code == ERROR_PATH_NOT_FOUND.to_hresult() {
                Some(Err(DecodeError::NotFound))
            } else {
                Some(Err(DecodeError::Failed(format!("Hibás vagy sérült kép ({:#010x}).", code.0 as u32))))
            }
        }
    }
}

unsafe fn decode_inner(path: &Path, target: Target, cancel: &dyn Fn() -> bool) -> Result<Prepared> {
    unsafe {
        let f = factory()?;
        let decoder = f.CreateDecoderFromFilename(
            &HSTRING::from(path.as_os_str()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )?;
        super::mark("wic_decoder");
        let container = decoder.GetContainerFormat().unwrap_or_default();
        let frame = pick_frame(&decoder, container)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        if w == 0 || h == 0 {
            return Err(Error::empty());
        }

        // HEIF-nél a kodek már alkalmazza a konténer saját forgatását; az EXIF-et figyelmen
        // kívül kell hagyni, különben duplán forgatnánk.
        let orient = if container == GUID_ContainerFormatHeif { 1 } else { orientation(&frame) };
        let swaps = matches!(orient, 5..=8);
        let (ow, oh) = if swaps { (h, w) } else { (w, h) };
        let (tw, th) = fit_size(ow, oh, target, false);
        let (mut uw, mut uh) = if swaps { (th, tw) } else { (tw, th) };

        let mut source: IWICBitmapSource = frame.cast()?;
        if uw < w || uh < h {
            // JPEG: a DCT-tartományban 1/2, 1/4, 1/8 méretre dekódol – töredék idő.
            if let Some(pre) = prescale(&f, &frame, w, h, uw, uh)? {
                source = pre;
            }
            super::mark("wic_prescaled");
            let (mut sw, mut sh) = (0u32, 0u32);
            source.GetSize(&mut sw, &mut sh)?;
            if sw < uw || sh < uh {
                // Legfeljebb 5%-kal kisebb előskálázott méret: azt használjuk közvetlenül.
                (uw, uh) = (sw, sh);
            }
            if (sw, sh) != (uw, uh) {
                let scaler = f.CreateBitmapScaler()?;
                scaler.Initialize(&source, uw, uh, WICBitmapInterpolationModeHighQualityCubic)?;
                source = scaler.cast()?;
            }
        }
        if cancel() {
            return Err(E_ABORT.into());
        }

        let conv = f.CreateFormatConverter()?;
        conv.Initialize(
            &source,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;

        // Forgatás nélkül, sávokban másolunk (közben figyeljük a megszakítást); az EXIF
        // orientációt utána, a kész pixeleken alkalmazzuk. A WIC flip-rotatora skálázott
        // forráson oszloponként újraszámolná a skálázást, ami nagyságrendekkel lassabb.
        let (tw, th) = if swaps { (uh, uw) } else { (uw, uh) };
        let (tex, dst) = Texture::alloc(tw, th).ok_or_else(|| Error::from(E_OUTOFMEMORY))?;
        let copy_into = |buf: &mut [u8]| -> Result<()> {
            let stride = uw as usize * 4;
            let rows = ((4 << 20) / stride).max(1) as u32;
            let mut y = 0u32;
            while y < uh {
                if cancel() {
                    return Err(E_ABORT.into());
                }
                let rh = rows.min(uh - y);
                let rc = WICRect { X: 0, Y: y as i32, Width: uw as i32, Height: rh as i32 };
                let start = y as usize * stride;
                conv.CopyPixels(&rc, stride as u32, &mut buf[start..start + rh as usize * stride])?;
                y += rh;
            }
            Ok(())
        };
        if orient == 1 {
            copy_into(dst)?;
        } else {
            let mut tmp = vec![0u8; uw as usize * uh as usize * 4];
            copy_into(&mut tmp)?;
            apply_orientation_into(&tmp, uw, uh, orient, dst);
        }
        composite_on_background(dst);

        Ok(Prepared { tex, orig_w: ow, orig_h: oh, full: (tw, th) == (ow, oh), vector: false })
    }
}

/// ICO-nál a legnagyobb méretű ikont választjuk, egyébként az elsőt.
unsafe fn pick_frame(decoder: &IWICBitmapDecoder, container: windows::core::GUID) -> Result<IWICBitmapFrameDecode> {
    unsafe {
        if container == GUID_ContainerFormatIco {
            let n = decoder.GetFrameCount().unwrap_or(1);
            let mut best: Option<(u64, IWICBitmapFrameDecode)> = None;
            for i in 0..n {
                let Ok(fr) = decoder.GetFrame(i) else { continue };
                let (mut w, mut h) = (0u32, 0u32);
                if fr.GetSize(&mut w, &mut h).is_ok() {
                    let area = w as u64 * h as u64;
                    if best.as_ref().is_none_or(|(a, _)| area > *a) {
                        best = Some((area, fr));
                    }
                }
            }
            if let Some((_, fr)) = best {
                return Ok(fr);
            }
        }
        decoder.GetFrame(0)
    }
}

unsafe fn prescale(
    f: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    w: u32,
    h: u32,
    uw: u32,
    uh: u32,
) -> Result<Option<IWICBitmapSource>> {
    unsafe {
        let Ok(t) = frame.cast::<IWICBitmapSourceTransform>() else {
            return Ok(None);
        };
        // Legfeljebb 5%-kal kisebb méret is jó: szemmel nem látható, de akár kétszer gyorsabb
        // (pl. 12 MP-es JPEG 1015 px-es célra: 1/4 = 1000 px az 1/2 = 2000 px helyett).
        let ok = |cw: u32, ch: u32| cw as f64 >= uw as f64 * 0.95 && ch as f64 >= uh as f64 * 0.95;
        for d in [8u32, 4, 2] {
            let (mut cw, mut ch) = (w.div_ceil(d), h.div_ceil(d));
            if !ok(cw, ch) {
                continue;
            }
            t.GetClosestSize(&mut cw, &mut ch)?;
            if !ok(cw, ch) || cw >= w {
                continue;
            }
            let mut fmt = frame.GetPixelFormat()?;
            t.GetClosestPixelFormat(&mut fmt)?;
            // Közvetlenül egy WIC-bitmap memóriájába dekódolunk (nincs külön puffer + másolat).
            let bmp = f.CreateBitmap(cw, ch, &fmt, WICBitmapCacheOnLoad)?;
            {
                let rc = WICRect { X: 0, Y: 0, Width: cw as i32, Height: ch as i32 };
                let lock = bmp.Lock(&rc, WICBitmapLockWrite.0 as u32)?;
                let stride = lock.GetStride()?;
                let (mut size, mut ptr) = (0u32, std::ptr::null_mut());
                lock.GetDataPointer(&mut size, &mut ptr)?;
                let buf = std::slice::from_raw_parts_mut(ptr, size as usize);
                t.CopyPixels(std::ptr::null(), cw, ch, &fmt, WICBitmapTransformRotate0, stride, buf)?;
            }
            return Ok(Some(bmp.cast()?));
        }
        Ok(None)
    }
}

/// EXIF Orientation (1–8), a formátumtól független WIC metaadat-policyn keresztül.
unsafe fn orientation(frame: &IWICBitmapFrameDecode) -> u16 {
    unsafe {
        let Ok(q) = frame.GetMetadataQueryReader() else { return 1 };
        let mut pv = PROPVARIANT::default();
        if q.GetMetadataByName(w!("System.Photo.Orientation"), &mut pv).is_err() {
            return 1;
        }
        let inner = &pv.Anonymous.Anonymous;
        let v = if inner.vt == VT_UI2 { inner.Anonymous.uiVal } else { 1 };
        let _ = PropVariantClear(&mut pv);
        if (1..=8).contains(&v) { v } else { 1 }
    }
}
