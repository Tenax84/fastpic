//! GDI kirajzolás: kép, lapozó gombok, infópanel, üzenetek.
//!
//! Szándékosan nem Direct2D: a D3D eszköz létrehozása mérten ~90 ms-ot és több tíz MB
//! memóriát vitt el. A képet eleve kijelzőméretre dekódoljuk, így a megjelenítés jellemzően
//! 1:1-es BitBlt, amihez nem kell GPU. Zoomnál StretchBlt, forgatásnál PlgBlt.

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::w;

use crate::core::decode::Frame;
use crate::core::exif_info::InfoLines;

/// Háttérszín (B, G, R). Az átlátszó képeket feltöltéskor erre komponáljuk.
const BG: (u8, u8, u8) = (0x1e, 0x1e, 0x1e);

fn bg_colorref() -> COLORREF {
    COLORREF(BG.2 as u32 | (BG.1 as u32) << 8 | (BG.0 as u32) << 16)
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Egy kép GDI DIB-szekcióban (32 bit, felülről lefelé, átlátszóság a háttérre komponálva).
/// A háttérszálon készül el, így a UI-szálnak már csak ki kell rajzolnia.
pub struct Texture {
    bmp: HBITMAP,
    pub width: u32,
    pub height: u32,
}

// A GDI-bitmap folyamatszintű objektum; egyszerre csak egy szál használja.
unsafe impl Send for Texture {}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

fn dib_info(w: u32, h: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Üres DIB-szekció és a pixeleire mutató szelet.
fn create_dib(w: u32, h: u32) -> Option<(HBITMAP, &'static mut [u8])> {
    let info = dib_info(w, h);
    let mut bits = std::ptr::null_mut();
    let bmp = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()? };
    if bits.is_null() {
        return None;
    }
    let len = w as usize * h as usize * 4;
    Some((bmp, unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, len) }))
}

impl Texture {
    /// Üres textúra és az írható pixelei (premultiplied BGRA). A dekóder közvetlenül ide
    /// írhat, így nincs külön CPU-oldali másolat. A szelet nem élheti túl a textúrát.
    pub fn alloc(w: u32, h: u32) -> Option<(Texture, &'static mut [u8])> {
        let (bmp, px) = create_dib(w, h)?;
        Some((Texture { bmp, width: w, height: h }, px))
    }

    pub fn from_frame(frame: &Frame) -> Option<Texture> {
        let (tex, dst) = Texture::alloc(frame.width, frame.height)?;
        dst.copy_from_slice(&frame.pixels);
        composite_on_background(dst);
        Some(tex)
    }
}

/// Premultiplied BGRA → átlátszatlan, a háttérszínre komponálva: out = src + háttér * (1 - alfa).
pub fn composite_on_background(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3];
        if a != 255 {
            let inv = 255 - a as u32;
            p[0] = (p[0] as u32 + (BG.0 as u32 * inv + 127) / 255).min(255) as u8;
            p[1] = (p[1] as u32 + (BG.1 as u32 * inv + 127) / 255).min(255) as u8;
            p[2] = (p[2] as u32 + (BG.2 as u32 * inv + 127) / 255).min(255) as u8;
            p[3] = 255;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Prev,
    Next,
    /// Megnyitás képszerkesztővel (jobb felső sarok).
    Edit,
}

/// A látható overlay gombok és az, amelyik fölött az egér áll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ButtonBar {
    pub nav: bool,
    pub edit: bool,
    pub hover: Option<Button>,
}

impl ButtonBar {
    fn visible(&self) -> impl Iterator<Item = Button> + '_ {
        [Button::Prev, Button::Next, Button::Edit]
            .into_iter()
            .filter(|b| if *b == Button::Edit { self.edit } else { self.nav })
    }
}

pub struct ImageDraw<'a> {
    pub tex: &'a Texture,
    /// Kép-tér (eredeti pixel) → képernyő.
    pub matrix: [f64; 6],
    pub orig_w: u32,
    pub orig_h: u32,
}

pub struct Scene<'a> {
    pub image: Option<ImageDraw<'a>>,
    pub message: Option<&'a str>,
    /// A látható overlay gombok (lapozás, szerkesztés), ha vannak.
    pub buttons: Option<ButtonBar>,
    pub info: Option<&'a InfoLines>,
    /// Rövid értesítés az ablak alján (pl. elérhető frissítés).
    pub notice: Option<&'a str>,
    pub dpi_scale: f32,
}

pub fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    ((rc.right - rc.left).max(0) as u32, (rc.bottom - rc.top).max(0) as u32)
}

fn button_radius(which: Button, s: f32) -> f32 {
    if which == Button::Edit { (20.0 * s).round() } else { (26.0 * s).round() }
}

/// Az overlay gombok középpontja és sugara.
pub fn button_geometry(which: Button, w: f32, h: f32, s: f32) -> (f32, f32, f32) {
    let r = button_radius(which, s);
    let margin = (16.0 * s).round();
    match which {
        Button::Prev => (margin + r, (h / 2.0).round(), r),
        Button::Next => (w - margin - r, (h / 2.0).round(), r),
        Button::Edit => (w - margin - r, margin + r, r),
    }
}

pub fn hit_button(x: f32, y: f32, w: f32, h: f32, s: f32, bar: &ButtonBar) -> Option<Button> {
    for b in bar.visible() {
        let (cx, cy, r) = button_geometry(b, w, h, s);
        // A kattintható terület kicsit nagyobb a rajzolt körnél.
        let reach = r * 1.6;
        if (x - cx).abs() <= reach && (y - cy).abs() <= reach {
            return Some(b);
        }
    }
    None
}

/// Egy szakasz lefedettsége az (x, y) pontban: a vastagság `w0`-ról `w1`-re változik
/// (így kúpos hegy is rajzolható), élsimítva.
fn segment_cover(x: f32, y: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32), w0: f32, w1: f32) -> f32 {
    let (vx, vy) = (bx - ax, by - ay);
    let t = (((x - ax) * vx + (y - ay) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    let d = ((x - ax - t * vx).powi(2) + (y - ay - t * vy).powi(2)).sqrt();
    (w0 + (w1 - w0) * t - d + 0.5).clamp(0.0, 1.0)
}

/// Élsimított, félátlátszó kör fehér ikonnal (nyíl vagy ceruza), premultiplied BGRA-ban.
fn button_sprite(s: f32, which: Button, hover: bool) -> Option<(HBITMAP, i32)> {
    let r = button_radius(which, s);
    let size = (2.0 * r).ceil() as u32 + 2;
    let (bmp, px) = create_dib(size, size)?;
    let c = size as f32 / 2.0;
    let fill_a = if hover { 0.75 } else { 0.45 };
    let half_stroke = 1.5 * s;
    // Az ikon szakaszai a kör középpontjához képest: (kezdet, vég, vastagság a kezdetnél, a végén).
    let segments: Vec<((f32, f32), (f32, f32), f32, f32)> = match which {
        Button::Prev | Button::Next => {
            let d = r * 0.3;
            let dir = if which == Button::Prev { 1.0 } else { -1.0 };
            let tip = (-dir * d * 0.6, 0.0);
            [(dir * d * 0.6, -d * 1.2), (dir * d * 0.6, d * 1.2)]
                .into_iter()
                .map(|e| (e, tip, half_stroke, half_stroke))
                .collect()
        }
        Button::Edit => {
            // Ceruza 45°-ban: test, kúpos hegy, és egy rés a radír előtt.
            let body = 0.15 * r;
            vec![
                ((0.40 * r, -0.40 * r), (0.26 * r, -0.26 * r), body, body),
                ((0.20 * r, -0.20 * r), (-0.22 * r, 0.22 * r), body, body),
                ((-0.22 * r, 0.22 * r), (-0.42 * r, 0.42 * r), body, 0.0),
            ]
        }
    };
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let circle = (r - (fx * fx + fy * fy).sqrt() + 0.5).clamp(0.0, 1.0) * fill_a;
            let line =
                segments.iter().map(|(a, b, w0, w1)| segment_cover(fx, fy, *a, *b, *w0, *w1)).fold(0.0f32, f32::max)
                    * 0.95;
            // Fehér nyíl a fekete körön („over” komponálás, premultiplied).
            let a = line + circle * (1.0 - line);
            let v = (line * 255.0).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = (a * 255.0).round() as u8;
        }
    }
    Some((bmp, size as i32))
}

struct Fonts {
    scale: f32,
    normal: HFONT,
    bold: HFONT,
}

struct BackBuffer {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    w: u32,
    h: u32,
}

pub struct Renderer {
    back: Option<BackBuffer>,
    fonts: Option<Fonts>,
    /// (skála, [prev, prev_hover, next, next_hover, edit, edit_hover])
    sprites: Option<(f32, Vec<(HBITMAP, i32)>)>,
    bg_brush: HBRUSH,
    black_dib: Option<HBITMAP>,
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer {
            back: None,
            fonts: None,
            sprites: None,
            bg_brush: unsafe { CreateSolidBrush(bg_colorref()) },
            black_dib: None,
        }
    }

    fn back_buffer(&mut self, wnd_dc: HDC, w: u32, h: u32) -> HDC {
        if let Some(b) = &self.back
            && b.w == w
            && b.h == h
        {
            return b.dc;
        }
        self.release_back();
        unsafe {
            let dc = CreateCompatibleDC(Some(wnd_dc));
            let bmp = CreateCompatibleBitmap(wnd_dc, w as i32, h as i32);
            let old = SelectObject(dc, bmp.into());
            self.back = Some(BackBuffer { dc, bmp, old, w, h });
            dc
        }
    }

    fn release_back(&mut self) {
        if let Some(b) = self.back.take() {
            unsafe {
                SelectObject(b.dc, b.old);
                let _ = DeleteObject(b.bmp.into());
                let _ = DeleteDC(b.dc);
            }
        }
    }

    fn fonts(&mut self, s: f32) -> (HFONT, HFONT) {
        if let Some(f) = &self.fonts {
            if f.scale == s {
                return (f.normal, f.bold);
            }
            unsafe {
                let _ = DeleteObject(f.normal.into());
                let _ = DeleteObject(f.bold.into());
            }
        }
        let mk = |weight: i32| unsafe {
            CreateFontW(
                -(15.0 * s).round() as i32,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            )
        };
        let f = Fonts { scale: s, normal: mk(400), bold: mk(600) };
        let r = (f.normal, f.bold);
        self.fonts = Some(f);
        r
    }

    fn sprite(&mut self, s: f32, which: Button, hover: bool) -> Option<(HBITMAP, i32)> {
        if self.sprites.as_ref().is_none_or(|(sc, _)| *sc != s) {
            if let Some((_, old)) = self.sprites.take() {
                for (b, _) in old {
                    unsafe {
                        let _ = DeleteObject(b.into());
                    }
                }
            }
            let v: Option<Vec<_>> = [Button::Prev, Button::Next, Button::Edit]
                .into_iter()
                .flat_map(|b| [(b, false), (b, true)])
                .map(|(b, h)| button_sprite(s, b, h))
                .collect();
            self.sprites = Some((s, v?));
        }
        let idx = which as usize * 2 + hover as usize;
        self.sprites.as_ref().map(|(_, v)| v[idx])
    }

    pub fn draw(&mut self, wnd_dc: HDC, w: u32, h: u32, scene: &Scene) {
        if w == 0 || h == 0 {
            return;
        }
        let full = RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 };
        let overlays =
            scene.message.is_some() || scene.buttons.is_some() || scene.info.is_some() || scene.notice.is_some();
        if !overlays {
            // Csak kép: közvetlenül az ablakba rajzolunk, a háttérrel csak a kép körüli
            // részt töltjük ki (nincs villogás), és nem kell kliensméretű hátsó puffer.
            self.release_back();
            unsafe {
                let plan = scene.image.as_ref().and_then(|img| plan_image(img, w, h));
                let saved = SaveDC(wnd_dc);
                if let Some(p) = &plan {
                    let r = p.bounds();
                    ExcludeClipRect(wnd_dc, r.left, r.top, r.right, r.bottom);
                }
                FillRect(wnd_dc, &full, self.bg_brush);
                let _ = RestoreDC(wnd_dc, saved);
                if let (Some(img), Some(p)) = (&scene.image, &plan) {
                    exec_image(wnd_dc, img, p);
                }
            }
            return;
        }

        let s = scene.dpi_scale;
        let dc = self.back_buffer(wnd_dc, w, h);
        unsafe {
            FillRect(dc, &full, self.bg_brush);

            if let Some(img) = &scene.image
                && let Some(p) = plan_image(img, w, h)
            {
                exec_image(dc, img, &p);
            }

            if let Some(msg) = scene.message {
                let (normal, _) = self.fonts(s);
                let old = SelectObject(dc, normal.into());
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, rgb(0xcc, 0xcc, 0xcc));
                let pad = (32.0 * s) as i32;
                let mut text: Vec<u16> = msg.encode_utf16().collect();
                let mut rc = RECT { left: pad, top: 0, right: w as i32 - pad, bottom: h as i32 };
                let flags = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
                DrawTextW(dc, &mut text, &mut rc, flags | DT_CALCRECT);
                let text_h = rc.bottom - rc.top;
                let mut rc = RECT {
                    left: pad,
                    top: (h as i32 - text_h) / 2,
                    right: w as i32 - pad,
                    bottom: (h as i32 + text_h) / 2 + 1,
                };
                DrawTextW(dc, &mut text, &mut rc, flags);
                SelectObject(dc, old);
            }

            if let Some(bar) = &scene.buttons {
                for b in bar.visible() {
                    let (cx, cy, _) = button_geometry(b, w as f32, h as f32, s);
                    if let Some((bmp, size)) = self.sprite(s, b, bar.hover == Some(b)) {
                        alpha_blit(dc, bmp, cx as i32 - size / 2, cy as i32 - size / 2, size, size, size, size, None);
                    }
                }
            }

            if let Some(info) = scene.info {
                let (normal, bold) = self.fonts(s);
                let line_h = (22.0 * s) as i32;
                let pad = (14.0 * s) as i32;
                let label_w = (135.0 * s) as i32;
                let panel_w = ((460.0 * s) as i32).min(w as i32 - (24.0 * s) as i32).max(100);
                let panel_h = pad * 2 + line_h * info.len() as i32;
                let (x0, y0) = ((12.0 * s) as i32, (12.0 * s) as i32);
                self.dim_rect(dc, x0, y0, panel_w, panel_h);
                SetBkMode(dc, TRANSPARENT);
                let old = SelectObject(dc, bold.into());
                for (i, (label, value)) in info.iter().enumerate() {
                    let y = y0 + pad + i as i32 * line_h;
                    let flags = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS;
                    SelectObject(dc, bold.into());
                    SetTextColor(dc, rgb(0xa6, 0xa6, 0xa6));
                    let mut l: Vec<u16> = label.encode_utf16().collect();
                    let mut rc = RECT { left: x0 + pad, top: y, right: x0 + pad + label_w, bottom: y + line_h };
                    DrawTextW(dc, &mut l, &mut rc, flags);
                    SelectObject(dc, normal.into());
                    SetTextColor(dc, rgb(0xff, 0xff, 0xff));
                    let mut v: Vec<u16> = value.encode_utf16().collect();
                    let mut rc =
                        RECT { left: x0 + pad + label_w, top: y, right: x0 + panel_w - pad, bottom: y + line_h };
                    DrawTextW(dc, &mut v, &mut rc, flags);
                }
                SelectObject(dc, old);
            }

            if let Some(notice) = scene.notice {
                let (normal, _) = self.fonts(s);
                let old = SelectObject(dc, normal.into());
                let (pad_x, pad_y, margin) = ((14.0 * s) as i32, (8.0 * s) as i32, (16.0 * s) as i32);
                let mut text: Vec<u16> = notice.encode_utf16().collect();
                let flags = DT_SINGLELINE | DT_NOPREFIX;
                let mut rc = RECT::default();
                DrawTextW(dc, &mut text, &mut rc, flags | DT_CALCRECT);
                let box_w = (rc.right + 2 * pad_x).min(w as i32 - 2 * margin).max(1);
                let box_h = rc.bottom + 2 * pad_y;
                let (x0, y0) = ((w as i32 - box_w) / 2, h as i32 - margin - box_h);
                self.dim_rect(dc, x0, y0, box_w, box_h);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, rgb(0xff, 0xff, 0xff));
                let mut rc = RECT { left: x0 + pad_x, top: y0, right: x0 + box_w - pad_x, bottom: y0 + box_h };
                DrawTextW(dc, &mut text, &mut rc, flags | DT_CENTER | DT_VCENTER | DT_END_ELLIPSIS);
                SelectObject(dc, old);
            }

            let _ = BitBlt(wnd_dc, 0, 0, w as i32, h as i32, Some(dc), 0, 0, SRCCOPY);
        }
    }

    /// Félátlátszó fekete háttér a szöveges overlay-ek alá.
    unsafe fn dim_rect(&mut self, dc: HDC, x: i32, y: i32, w: i32, h: i32) {
        if self.black_dib.is_none() {
            self.black_dib = create_dib(1, 1).map(|(b, px)| {
                px.copy_from_slice(&[0, 0, 0, 255]);
                b
            });
        }
        if let Some(black) = self.black_dib {
            unsafe { alpha_blit(dc, black, x, y, w, h, 1, 1, Some(184)) };
        }
    }
}

/// Bitmap ráhúzása alfával. `constant`: egységes átlátszóság (per-pixel alfa helyett).
#[allow(clippy::too_many_arguments)]
unsafe fn alpha_blit(dc: HDC, bmp: HBITMAP, x: i32, y: i32, w: i32, h: i32, sw: i32, sh: i32, constant: Option<u8>) {
    unsafe {
        let src = CreateCompatibleDC(Some(dc));
        let old = SelectObject(src, bmp.into());
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: constant.unwrap_or(255),
            AlphaFormat: if constant.is_some() { 0 } else { AC_SRC_ALPHA as u8 },
        };
        let _ = GdiAlphaBlend(dc, x, y, w, h, src, 0, 0, sw, sh, blend);
        SelectObject(src, old);
        let _ = DeleteDC(src);
    }
}

/// Hogyan kerül a kép a képernyőre.
enum Blit {
    /// 1:1 másolás (a leggyakoribb: a kép eleve kijelzőméretre van dekódolva).
    Copy { dst: RECT, sx: i32, sy: i32 },
    /// Nyújtás/zsugorítás, csak a látható részre.
    Stretch { dst: RECT, src: RECT, mode: STRETCH_BLT_MODE },
    /// Forgatott kép: a textúra három sarkának képernyőpontja.
    Rotated { pts: [POINT; 3], bounds: RECT, mode: STRETCH_BLT_MODE },
}

impl Blit {
    fn bounds(&self) -> RECT {
        match self {
            Blit::Copy { dst, .. } | Blit::Stretch { dst, .. } => *dst,
            Blit::Rotated { bounds, .. } => *bounds,
        }
    }
}

fn plan_image(img: &ImageDraw, cw: u32, ch: u32) -> Option<Blit> {
    let m = img.matrix;
    let (tw, th) = (img.tex.width as f64, img.tex.height as f64);
    // Textúrapixel → képernyő: a kép-tér mátrixa, megszorozva a textúra/eredeti aránnyal.
    let kx = img.orig_w as f64 / tw;
    let ky = img.orig_h as f64 / th;
    let rotated = m[1].abs() > 1e-9 || m[0] < 0.0;
    if !rotated {
        let sx = m[0] * kx; // képernyőpixel / textúrapixel
        let sy = m[3] * ky;
        let (dx, dy) = (m[4], m[5]);
        let vis_x0 = (-dx / sx).floor().max(0.0);
        let vis_y0 = (-dy / sy).floor().max(0.0);
        let vis_x1 = ((cw as f64 - dx) / sx).ceil().min(tw);
        let vis_y1 = ((ch as f64 - dy) / sy).ceil().min(th);
        if vis_x1 <= vis_x0 || vis_y1 <= vis_y0 {
            return None;
        }
        let dst = RECT {
            left: (dx + vis_x0 * sx).round() as i32,
            top: (dy + vis_y0 * sy).round() as i32,
            right: (dx + vis_x1 * sx).round() as i32,
            bottom: (dy + vis_y1 * sy).round() as i32,
        };
        let src = RECT { left: vis_x0 as i32, top: vis_y0 as i32, right: vis_x1 as i32, bottom: vis_y1 as i32 };
        if dst.right - dst.left == src.right - src.left && dst.bottom - dst.top == src.bottom - src.top {
            return Some(Blit::Copy { dst, sx: src.left, sy: src.top });
        }
        // Kicsinyítésnél szép (HALFTONE), nagyításnál éles pixelek (COLORONCOLOR).
        let mode = if sx < 1.0 { HALFTONE } else { COLORONCOLOR };
        return Some(Blit::Stretch { dst, src, mode });
    }
    let map = |x: f64, y: f64| {
        let (ix, iy) = (x * kx, y * ky);
        POINT { x: (ix * m[0] + iy * m[2] + m[4]).round() as i32, y: (ix * m[1] + iy * m[3] + m[5]).round() as i32 }
    };
    let pts = [map(0.0, 0.0), map(tw, 0.0), map(0.0, th)];
    let far = map(tw, th);
    let xs = [pts[0].x, pts[1].x, pts[2].x, far.x];
    let ys = [pts[0].y, pts[1].y, pts[2].y, far.y];
    let bounds = RECT {
        left: *xs.iter().min().unwrap(),
        top: *ys.iter().min().unwrap(),
        right: *xs.iter().max().unwrap(),
        bottom: *ys.iter().max().unwrap(),
    };
    let scale = (m[0] * m[0] + m[1] * m[1]).sqrt() * kx;
    let mode = if scale < 1.0 { HALFTONE } else { COLORONCOLOR };
    Some(Blit::Rotated { pts, bounds, mode })
}

unsafe fn exec_image(dc: HDC, img: &ImageDraw, plan: &Blit) {
    unsafe {
        let src = CreateCompatibleDC(Some(dc));
        let old = SelectObject(src, img.tex.bmp.into());
        match plan {
            Blit::Copy { dst, sx, sy } => {
                let (w, h) = (dst.right - dst.left, dst.bottom - dst.top);
                let _ = BitBlt(dc, dst.left, dst.top, w, h, Some(src), *sx, *sy, SRCCOPY);
            }
            Blit::Stretch { dst, src: s, mode } => {
                SetStretchBltMode(dc, *mode);
                let _ = SetBrushOrgEx(dc, 0, 0, None);
                let _ = StretchBlt(
                    dc,
                    dst.left,
                    dst.top,
                    dst.right - dst.left,
                    dst.bottom - dst.top,
                    Some(src),
                    s.left,
                    s.top,
                    s.right - s.left,
                    s.bottom - s.top,
                    SRCCOPY,
                );
            }
            Blit::Rotated { pts, mode, .. } => {
                SetStretchBltMode(dc, *mode);
                let _ = SetBrushOrgEx(dc, 0, 0, None);
                let (w, h) = (img.tex.width as i32, img.tex.height as i32);
                let _ = PlgBlt(dc, pts, src, 0, 0, w, h, None, 0, 0);
            }
        }
        SelectObject(src, old);
        let _ = DeleteDC(src);
    }
}
