//! Megjelenítés CoreAnimation-nel: a kép egy `CALayer` tartalma (`CGImage`), az overlay
//! gombok és az infópanel további rétegek.
//!
//! Nincs saját rajzolási ciklus és hátsó puffer: csak a rétegek tulajdonságait állítjuk,
//! a komponálást a window server végzi. A képet kijelzőméretre dekódoljuk, így a réteg
//! tartalma jellemzően 1:1-ben kerül a képernyőre.

use std::ffi::c_void;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName,
};
use objc2_core_foundation::{CFRetained, CGAffineTransform, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapInfo, CGColor, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString};
use objc2_quartz_core::{CALayer, CATextLayer, CATransaction, kCAFilterNearest, kCAFilterTrilinear, kCAGravityResize};

use crate::core::decode::Frame;
use crate::core::exif_info::InfoLines;

/// Háttérszín (R, G, B) – a Windows-verzióval azonos.
const BG: (f64, f64, f64) = (0x1e as f64 / 255.0, 0x1e as f64 / 255.0, 0x1e as f64 / 255.0);

/// Egy megjelenítésre kész kép. A `CGImage` immutable, a dekódoló szálon készül el.
pub struct Texture {
    pub img: CFRetained<CGImage>,
    pub width: u32,
    pub height: u32,
}

// A CGImage immutable és szálfüggetlen; a dekódoló szálról adjuk át a UI-szálnak.
unsafe impl Send for Texture {}

impl Texture {
    /// Premultiplied BGRA pixelekből (a tartalék dekóderek kimenete).
    pub fn from_frame(frame: &Frame) -> Option<Texture> {
        let img = cgimage_from_pbgra(&frame.pixels, frame.width, frame.height)?;
        Some(Texture { img, width: frame.width, height: frame.height })
    }

    pub fn from_cgimage(img: CFRetained<CGImage>) -> Texture {
        let (width, height) = (CGImage::width(Some(&img)) as u32, CGImage::height(Some(&img)) as u32);
        Texture { img, width, height }
    }

    fn as_contents(&self) -> &AnyObject {
        // A CGImageRef toll-free módon átadható a CALayer `contents` tulajdonságának.
        unsafe { &*(CFRetained::as_ptr(&self.img).as_ptr() as *const AnyObject) }
    }
}

/// A puffer tulajdonjogát átvevő `CGDataProvider` felszabadítója.
unsafe extern "C-unwind" fn release_boxed(_info: *mut c_void, data: std::ptr::NonNull<c_void>, size: usize) {
    drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(data.as_ptr() as *mut u8, size)) });
}

/// Premultiplied BGRA puffer → `CGImage`, másolat nélkül (a puffert a provider veszi át).
fn cgimage_from_pbgra(pixels: &[u8], w: u32, h: u32) -> Option<CFRetained<CGImage>> {
    let stride = w as usize * 4;
    if w == 0 || h == 0 || pixels.len() < stride * h as usize {
        return None;
    }
    let boxed: Box<[u8]> = pixels[..stride * h as usize].into();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    let release: unsafe extern "C-unwind" fn(*mut c_void, std::ptr::NonNull<c_void>, usize) = release_boxed;
    let provider =
        unsafe { CGDataProvider::with_data(std::ptr::null_mut(), ptr as *const c_void, len, Some(release)) }?;
    let space = CGColorSpace::new_device_rgb()?;
    let info = CGBitmapInfo(CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0);
    unsafe {
        CGImage::new(
            w as usize,
            h as usize,
            8,
            32,
            stride,
            Some(&space),
            info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
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
    /// Kép-tér (eredeti pixel) → képernyő (pont).
    pub matrix: [f64; 6],
    pub orig_w: u32,
    pub orig_h: u32,
}

pub struct Scene<'a> {
    pub image: Option<ImageDraw<'a>>,
    pub message: Option<&'a str>,
    pub buttons: Option<ButtonBar>,
    pub info: Option<&'a InfoLines>,
    /// Kliensterület pontban.
    pub width: f64,
    pub height: f64,
}

fn button_radius(which: Button) -> f64 {
    if which == Button::Edit { 20.0 } else { 26.0 }
}

/// Az overlay gombok középpontja és sugara (pontban).
pub fn button_geometry(which: Button, w: f64, h: f64) -> (f64, f64, f64) {
    let r = button_radius(which);
    let margin = 16.0;
    match which {
        Button::Prev => (margin + r, (h / 2.0).round(), r),
        Button::Next => (w - margin - r, (h / 2.0).round(), r),
        Button::Edit => (w - margin - r, margin + r, r),
    }
}

pub fn hit_button(x: f64, y: f64, w: f64, h: f64, bar: &ButtonBar) -> Option<Button> {
    for b in bar.visible() {
        let (cx, cy, r) = button_geometry(b, w, h);
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
/// `s` a raszterizálás skálája (backing scale).
fn button_sprite(s: f32, which: Button, hover: bool) -> Option<CFRetained<CGImage>> {
    let r = (button_radius(which) as f32 * s).round();
    let size = (2.0 * r).ceil() as u32 + 2;
    let mut px = vec![0u8; size as usize * size as usize * 4];
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
            // Fehér ikon a fekete körön („over” komponálás, premultiplied).
            let a = line + circle * (1.0 - line);
            let v = (line * 255.0).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = (a * 255.0).round() as u8;
        }
    }
    cgimage_from_pbgra(&px, size, size)
}

fn color(r: f64, g: f64, b: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_srgb(r, g, b, a)
}

fn attributed(
    text: &str,
    size: f64,
    bold: bool,
    gray: f64,
    line_height: f64,
    center: bool,
) -> Retained<NSAttributedString> {
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    let style = NSMutableParagraphStyle::new();
    if line_height > 0.0 {
        style.setMinimumLineHeight(line_height);
        style.setMaximumLineHeight(line_height);
    }
    if center {
        style.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    }
    let fg = NSColor::colorWithSRGBRed_green_blue_alpha(gray, gray, gray, 1.0);
    let keys: [&NSString; 3] =
        unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName] };
    let vals: [&AnyObject; 3] = [
        &font as &NSFont as *const NSFont as *const AnyObject,
        &fg as &NSColor as *const NSColor as *const AnyObject,
        &style as &NSMutableParagraphStyle as *const NSMutableParagraphStyle as *const AnyObject,
    ]
    .map(|p| unsafe { &*p });
    let attrs = NSDictionary::from_slices(&keys, &vals);
    let s = NSString::from_str(text);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &s, Some(&attrs)) }
}

/// Az összes réteg, amit a nézet megjelenít.
pub struct Renderer {
    pub root: Retained<CALayer>,
    image: Retained<CALayer>,
    buttons: Vec<(Button, Retained<CALayer>)>,
    info_bg: Retained<CALayer>,
    info_label: Retained<CATextLayer>,
    info_value: Retained<CATextLayer>,
    message: Retained<CATextLayer>,
    /// (skála, [prev, prev_hover, next, next_hover, edit, edit_hover])
    sprites: Option<(f32, Vec<CFRetained<CGImage>>)>,
    backing: f64,
}

impl Renderer {
    pub fn new(backing: f64) -> Renderer {
        let root = CALayer::new();
        let image = CALayer::new();
        let info_bg = CALayer::new();
        let info_label = CATextLayer::new();
        let info_value = CATextLayer::new();
        let message = CATextLayer::new();
        root.setBackgroundColor(Some(&color(BG.0, BG.1, BG.2, 1.0)));
        // A rétegek koordinátája így bal felső sarokból indul, mint a nézeté.
        root.setGeometryFlipped(true);
        root.setMasksToBounds(true);
        image.setAnchorPoint(CGPoint::new(0.5, 0.5));
        image.setAllowsEdgeAntialiasing(false);
        image.setHidden(true);
        info_bg.setBackgroundColor(Some(&color(0.0, 0.0, 0.0, 0.72)));
        info_bg.setCornerRadius(8.0);
        info_bg.setHidden(true);
        for t in [&info_label, &info_value, &message] {
            t.setContentsScale(backing);
            t.setHidden(true);
            t.setWrapped(false);
        }
        message.setWrapped(true);
        root.addSublayer(&image);
        root.addSublayer(&info_bg);
        root.addSublayer(&info_label);
        root.addSublayer(&info_value);
        root.addSublayer(&message);
        let buttons = [Button::Prev, Button::Next, Button::Edit]
            .into_iter()
            .map(|b| {
                let l = CALayer::new();
                l.setContentsScale(backing);
                l.setHidden(true);
                root.addSublayer(&l);
                (b, l)
            })
            .collect();
        Renderer { root, image, buttons, info_bg, info_label, info_value, message, sprites: None, backing }
    }

    fn sprite(&mut self, which: Button, hover: bool) -> Option<&CFRetained<CGImage>> {
        let s = self.backing as f32;
        if self.sprites.as_ref().is_none_or(|(sc, _)| *sc != s) {
            let v: Option<Vec<_>> = [Button::Prev, Button::Next, Button::Edit]
                .into_iter()
                .flat_map(|b| [(b, false), (b, true)])
                .map(|(b, h)| button_sprite(s, b, h))
                .collect();
            self.sprites = Some((s, v?));
        }
        let idx = which as usize * 2 + hover as usize;
        self.sprites.as_ref().map(|(_, v)| &v[idx])
    }

    /// A rétegek frissítése a jelenet szerint. Az implicit animációkat kikapcsoljuk,
    /// különben a lapozás és a zoom „becsúszna”.
    pub fn draw(&mut self, scene: &Scene) {
        unsafe {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.root.setFrame(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(scene.width, scene.height)));
            self.draw_image(scene);
            self.draw_buttons(scene);
            self.draw_info(scene);
            self.draw_message(scene);
            CATransaction::commit();
        }
    }

    unsafe fn draw_image(&mut self, scene: &Scene) {
        let Some(img) = &scene.image else {
            self.image.setHidden(true);
            return;
        };
        let m = img.matrix;
        // A réteg a forgatás nélküli méretet veszi fel, a forgatás a transzformációban van.
        let scale = (m[0] * m[0] + m[1] * m[1]).sqrt();
        let (w, h) = (img.orig_w as f64 * scale, img.orig_h as f64 * scale);
        // A kép közepének képernyő-koordinátája (a réteg horgonypontja a közepe).
        let (icx, icy) = (img.orig_w as f64 / 2.0, img.orig_h as f64 / 2.0);
        let cx = icx * m[0] + icy * m[2] + m[4];
        let cy = icx * m[1] + icy * m[3] + m[5];
        let rot = m[1].atan2(m[0]);
        unsafe {
            self.image.setContents(Some(img.tex.as_contents()));
            self.image.setContentsScale(img.tex.width as f64 / w.max(1.0));
            self.image.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)));
            self.image.setPosition(CGPoint::new(cx, cy));
            self.image.setAffineTransform(CGAffineTransform {
                a: rot.cos(),
                b: rot.sin(),
                c: -rot.sin(),
                d: rot.cos(),
                tx: 0.0,
                ty: 0.0,
            });
            // 100% fölött éles pixelek, kicsinyítésnél simítás. A még nem teljes felbontású
            // textúrát (gyors előnézet, zoom közbeni köztes állapot) mindig simítva nagyítjuk.
            let up = w >= img.tex.width as f64 && img.tex.width >= img.orig_w;
            self.image.setMagnificationFilter(if up { kCAFilterNearest } else { kCAFilterTrilinear });
            self.image.setMinificationFilter(kCAFilterTrilinear);
            self.image.setContentsGravity(kCAGravityResize);
            self.image.setHidden(false);
        }
    }

    unsafe fn draw_buttons(&mut self, scene: &Scene) {
        for i in 0..self.buttons.len() {
            let (b, _) = self.buttons[i];
            let show = scene.buttons.as_ref().map(|bar| bar.visible().any(|v| v == b)).unwrap_or(false);
            if !show {
                self.buttons[i].1.setHidden(true);
                continue;
            }
            let hover = scene.buttons.as_ref().and_then(|bar| bar.hover) == Some(b);
            let sprite = self.sprite(b, hover).map(|s| s.clone());
            let (cx, cy, _) = button_geometry(b, scene.width, scene.height);
            let layer = &self.buttons[i].1;
            unsafe {
                if let Some(s) = sprite {
                    let side = CGImage::width(Some(&s)) as f64 / self.backing;
                    let contents = &*(CFRetained::as_ptr(&s).as_ptr() as *const AnyObject);
                    layer.setContents(Some(contents));
                    layer
                        .setFrame(CGRect::new(CGPoint::new(cx - side / 2.0, cy - side / 2.0), CGSize::new(side, side)));
                    layer.setHidden(false);
                }
            }
        }
    }

    unsafe fn draw_info(&mut self, scene: &Scene) {
        let Some(info) = scene.info else {
            self.info_bg.setHidden(true);
            self.info_label.setHidden(true);
            self.info_value.setHidden(true);
            return;
        };
        let line_h = 21.0;
        let pad = 12.0;
        let label_w = 118.0;
        let panel_w = (430.0f64).min(scene.width - 24.0).max(100.0);
        let panel_h = pad * 2.0 + line_h * info.len() as f64;
        let (x0, y0) = (12.0, 12.0);
        let labels: String = info.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join("\n");
        let values: String = info.iter().map(|(_, v)| v.as_str()).collect::<Vec<_>>().join("\n");
        unsafe {
            self.info_bg.setFrame(CGRect::new(CGPoint::new(x0, y0), CGSize::new(panel_w, panel_h)));
            self.info_bg.setHidden(false);
            let text_h = line_h * info.len() as f64 + 2.0;
            self.info_label.setString(Some(
                &*(Retained::as_ptr(&attributed(&labels, 12.5, true, 0.65, line_h, false)) as *const AnyObject),
            ));
            self.info_label.setFrame(CGRect::new(CGPoint::new(x0 + pad, y0 + pad - 2.0), CGSize::new(label_w, text_h)));
            self.info_label.setHidden(false);
            self.info_value.setString(Some(
                &*(Retained::as_ptr(&attributed(&values, 12.5, false, 1.0, line_h, false)) as *const AnyObject),
            ));
            self.info_value.setFrame(CGRect::new(
                CGPoint::new(x0 + pad + label_w, y0 + pad - 2.0),
                CGSize::new(panel_w - pad * 2.0 - label_w, text_h),
            ));
            self.info_value.setHidden(false);
        }
    }

    unsafe fn draw_message(&mut self, scene: &Scene) {
        let Some(msg) = scene.message else {
            self.message.setHidden(true);
            return;
        };
        let attr = attributed(msg, 13.5, false, 0.8, 0.0, true);
        let pad = 32.0;
        let width = (scene.width - pad * 2.0).max(50.0);
        let size = attr.size();
        // A méret becslése a tördelt szöveghez: a magasságot a sortörések számából pótoljuk.
        let lines = (size.width / width).ceil().max(1.0);
        let height = (size.height * lines).min(scene.height);
        unsafe {
            self.message.setString(Some(&*(Retained::as_ptr(&attr) as *const AnyObject)));
            self.message.setFrame(CGRect::new(
                CGPoint::new(pad, ((scene.height - height) / 2.0).max(0.0)),
                CGSize::new(width, height),
            ));
            self.message.setHidden(false);
        }
    }
}
