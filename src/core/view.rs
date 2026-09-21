//! Zoom, pan és forgatás matematikája. Platformfüggetlen, csak számol.
//!
//! Koordináták:
//! - kép-tér: a (EXIF szerint már elforgatott) eredeti kép pixelei, `img_w × img_h`;
//! - forgatott tér: a felhasználói forgatás (`rot` × 90° az óramutató szerint) utáni kép;
//! - képernyő: a kliensterület pixelei.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Fit,
    Manual,
}

pub const MAX_SCALE: f64 = 64.0;

#[derive(Debug, Clone)]
pub struct View {
    pub img_w: f64,
    pub img_h: f64,
    pub vector: bool,
    pub rot: u8,
    pub mode: Mode,
    pub scale: f64,
    /// A forgatott tér azon pontja, ami a kliensterület közepén van.
    cx: f64,
    cy: f64,
    pub win_w: f64,
    pub win_h: f64,
}

impl View {
    pub fn new(win_w: f64, win_h: f64) -> View {
        View {
            img_w: 0.0,
            img_h: 0.0,
            vector: false,
            rot: 0,
            mode: Mode::Fit,
            scale: 1.0,
            cx: 0.0,
            cy: 0.0,
            win_w,
            win_h,
        }
    }

    pub fn has_image(&self) -> bool {
        self.img_w > 0.0 && self.img_h > 0.0
    }

    /// Új kép: alaphelyzet (ablakhoz igazítva, forgatás nélkül).
    pub fn set_image(&mut self, w: u32, h: u32, vector: bool) {
        self.img_w = w as f64;
        self.img_h = h as f64;
        self.vector = vector;
        self.rot = 0;
        self.fit();
    }

    pub fn clear_image(&mut self) {
        self.img_w = 0.0;
        self.img_h = 0.0;
    }

    fn rotated_size(&self) -> (f64, f64) {
        if self.rot % 2 == 1 { (self.img_h, self.img_w) } else { (self.img_w, self.img_h) }
    }

    pub fn fit_scale(&self) -> f64 {
        let (rw, rh) = self.rotated_size();
        if rw <= 0.0 || rh <= 0.0 || self.win_w <= 0.0 || self.win_h <= 0.0 {
            return 1.0;
        }
        let s = (self.win_w / rw).min(self.win_h / rh);
        if self.vector { s } else { s.min(1.0) }
    }

    pub fn fit(&mut self) {
        self.mode = Mode::Fit;
        self.scale = self.fit_scale();
        let (rw, rh) = self.rotated_size();
        self.cx = rw / 2.0;
        self.cy = rh / 2.0;
    }

    pub fn set_window(&mut self, w: f64, h: f64) {
        self.win_w = w;
        self.win_h = h;
        if self.mode == Mode::Fit {
            self.fit();
        } else {
            self.clamp();
        }
    }

    /// Nagyítás `factor`-ral úgy, hogy a (px, py) képernyőpont alatti képpont a helyén maradjon.
    pub fn zoom_at(&mut self, factor: f64, px: f64, py: f64) {
        if !self.has_image() {
            return;
        }
        let fit = self.fit_scale();
        let new_scale = (self.scale * factor).clamp(fit.min(1.0), MAX_SCALE);
        if new_scale <= fit * 1.0001 && factor < 1.0 {
            self.fit();
            return;
        }
        let ix = self.cx + (px - self.win_w / 2.0) / self.scale;
        let iy = self.cy + (py - self.win_h / 2.0) / self.scale;
        self.mode = Mode::Manual;
        self.scale = new_scale;
        self.cx = ix - (px - self.win_w / 2.0) / new_scale;
        self.cy = iy - (py - self.win_h / 2.0) / new_scale;
        self.clamp();
    }

    /// Adott méretarány a kliensterület közepéhez igazítva (pl. 100% = 1.0).
    pub fn set_scale(&mut self, scale: f64) {
        if !self.has_image() {
            return;
        }
        self.mode = Mode::Manual;
        self.scale = scale.clamp(1.0 / MAX_SCALE, MAX_SCALE);
        self.clamp();
    }

    /// Eltolás képernyőpixelben (a kép a kurzorral együtt mozog).
    pub fn pan(&mut self, dx: f64, dy: f64) {
        if self.mode == Mode::Fit {
            return;
        }
        self.cx -= dx / self.scale;
        self.cy -= dy / self.scale;
        self.clamp();
    }

    /// Nagyobb-e a kép a kliensterületnél (van-e mit pannolni).
    pub fn can_pan(&self) -> bool {
        let (rw, rh) = self.rotated_size();
        rw * self.scale > self.win_w + 0.5 || rh * self.scale > self.win_h + 0.5
    }

    pub fn rotate(&mut self, clockwise: bool) {
        self.rot = if clockwise { (self.rot + 1) % 4 } else { (self.rot + 3) % 4 };
        self.fit();
    }

    fn clamp(&mut self) {
        let (rw, rh) = self.rotated_size();
        let half_w = self.win_w / 2.0 / self.scale;
        let half_h = self.win_h / 2.0 / self.scale;
        self.cx = if rw <= 2.0 * half_w { rw / 2.0 } else { self.cx.clamp(half_w, rw - half_w) };
        self.cy = if rh <= 2.0 * half_h { rh / 2.0 } else { self.cy.clamp(half_h, rh - half_h) };
    }

    /// Kép-tér → képernyő affin mátrix, D2D / CoreGraphics sorrendben:
    /// `[m11, m12, m21, m22, dx, dy]`, ahol `x' = x*m11 + y*m21 + dx`, `y' = x*m12 + y*m22 + dy`.
    pub fn matrix(&self) -> [f64; 6] {
        let (w, h) = (self.img_w, self.img_h);
        // Forgatás az óramutató szerint, majd visszatolás a pozitív negyedbe.
        let (m11, m12, m21, m22, tx, ty) = match self.rot {
            1 => (0.0, 1.0, -1.0, 0.0, h, 0.0),
            2 => (-1.0, 0.0, 0.0, -1.0, w, h),
            3 => (0.0, -1.0, 1.0, 0.0, 0.0, w),
            _ => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        };
        let s = self.scale;
        let mut dx = (tx - self.cx) * s + self.win_w / 2.0;
        let mut dy = (ty - self.cy) * s + self.win_h / 2.0;
        // Egész pixelre illesztés: 100%-on éles marad a kép.
        dx = dx.round();
        dy = dy.round();
        [m11 * s, m12 * s, m21 * s, m22 * s, dx, dy]
    }

    /// A kép képernyőn látható mérete pixelben (forgatás nélkül értve: szélesség az eredeti
    /// kép szélességéhez tartozik). Ebből dönthető el, kell-e nagyobb felbontású dekódolás.
    pub fn displayed_size(&self) -> (f64, f64) {
        (self.img_w * self.scale, self.img_h * self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
        (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
    }

    #[test]
    fn fit_centers_and_does_not_upscale_raster() {
        let mut v = View::new(800.0, 600.0);
        v.set_image(400, 300, false);
        assert_eq!(v.scale, 1.0);
        let m = v.matrix();
        assert_eq!(apply(m, 0.0, 0.0), (200.0, 150.0));
        assert_eq!(apply(m, 400.0, 300.0), (600.0, 450.0));
    }

    #[test]
    fn fit_upscales_vector() {
        let mut v = View::new(800.0, 600.0);
        v.set_image(40, 30, true);
        assert!((v.scale - 20.0).abs() < 1e-9);
    }

    #[test]
    fn fit_downscales() {
        let mut v = View::new(800.0, 600.0);
        v.set_image(4000, 1000, false);
        assert!((v.scale - 0.2).abs() < 1e-9);
    }

    #[test]
    fn rotation_maps_corners() {
        let mut v = View::new(1000.0, 1000.0);
        v.set_image(400, 200, false);
        v.rotate(true);
        let m = v.matrix();
        // 90°-kal elforgatva a bal felső sarok a jobb felsőbe kerül.
        let (x0, y0) = apply(m, 0.0, 0.0);
        let (x1, y1) = apply(m, 400.0, 200.0);
        assert_eq!((x0, y0), (600.0, 300.0));
        assert_eq!((x1, y1), (400.0, 700.0));
        v.rotate(true);
        v.rotate(true);
        v.rotate(true);
        assert_eq!(v.rot, 0);
    }

    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut v = View::new(800.0, 600.0);
        v.set_image(4000, 3000, false);
        let before = v.matrix();
        // Melyik képpont van a (100, 100) képernyőpont alatt?
        let ix = (100.0 - before[4]) / before[0];
        let iy = (100.0 - before[5]) / before[3];
        v.zoom_at(2.0, 100.0, 100.0);
        assert_eq!(v.mode, Mode::Manual);
        let after = v.matrix();
        let (sx, sy) = apply(after, ix, iy);
        assert!((sx - 100.0).abs() <= 1.0 && (sy - 100.0).abs() <= 1.0);
    }

    #[test]
    fn zoom_out_snaps_to_fit_and_pan_is_clamped() {
        let mut v = View::new(800.0, 600.0);
        v.set_image(4000, 3000, false);
        v.set_scale(1.0);
        assert!(v.can_pan());
        v.pan(1e6, 1e6);
        let m = v.matrix();
        // A bal felső sarok nem jöhet beljebb a kliensterület bal felső sarkánál.
        assert_eq!(apply(m, 0.0, 0.0), (0.0, 0.0));
        v.zoom_at(0.01, 400.0, 300.0);
        assert_eq!(v.mode, Mode::Fit);
    }
}
