//! Ablak helye, mérete és „zoomolt” (maximalizált) állapota.
//!
//! A mentett téglalap macOS globális koordinátában van (origó a főképernyő bal alsó sarka,
//! y felfelé nő), ezért a `WindowState` `top`/`bottom` mezői itt az alsó/felső élt jelentik.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSScreen, NSWindow};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::core::state::WindowState;

/// Alapértelmezett ablakméret, ha nincs mentett állapot.
pub fn default_size(mtm: MainThreadMarker) -> (f64, f64) {
    let vis = NSScreen::mainScreen(mtm)
        .map(|s| s.visibleFrame())
        .unwrap_or(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1280.0, 800.0)));
    let w = 1200.0f64.min(vis.size.width * 0.9).max(320.0);
    let h = 800.0f64.min(vis.size.height * 0.9).max(240.0);
    (w, h)
}

fn to_rect(s: &WindowState) -> CGRect {
    CGRect::new(
        CGPoint::new(s.left as f64, s.top as f64),
        CGSize::new((s.right - s.left) as f64, (s.bottom - s.top) as f64),
    )
}

/// Látszik-e még a mentett ablak valamelyik képernyőn (monitor lecsatlakozhatott).
pub fn is_on_screen(s: &WindowState, mtm: MainThreadMarker) -> bool {
    let r = to_rect(s);
    NSScreen::screens(mtm).iter().any(|sc| {
        let f = sc.frame();
        // Legalább egy kis átfedés kell a látható területtel.
        r.origin.x < f.origin.x + f.size.width
            && r.origin.x + r.size.width > f.origin.x
            && r.origin.y < f.origin.y + f.size.height
            && r.origin.y + r.size.height > f.origin.y
    })
}

pub fn apply(window: &NSWindow, s: &WindowState) {
    window.setFrame_display(to_rect(s), false);
    if s.maximized && !window.isZoomed() {
        window.zoom(None);
    }
}

pub fn to_state(window: &NSWindow, client: (u32, u32)) -> WindowState {
    let f = window.frame();
    WindowState {
        left: f.origin.x.round() as i32,
        top: f.origin.y.round() as i32,
        right: (f.origin.x + f.size.width).round() as i32,
        bottom: (f.origin.y + f.size.height).round() as i32,
        maximized: window.isZoomed(),
        client_w: client.0,
        client_h: client.1,
    }
}
