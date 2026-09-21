//! Ablak helye/mérete/maximalizált állapota, és a teljes képernyős mód.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, MonitorFromRect, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::core::state::WindowState;

/// Alapértelmezett ablakméret (fizikai pixel), ha nincs mentett állapot.
pub fn default_size(dpi_scale: f64) -> (i32, i32) {
    let work = primary_work_area();
    let w = ((1200.0 * dpi_scale) as i32).min(((work.right - work.left) as f64 * 0.9) as i32);
    let h = ((800.0 * dpi_scale) as i32).min(((work.bottom - work.top) as f64 * 0.9) as i32);
    (w.max(320), h.max(240))
}

fn primary_work_area() -> RECT {
    unsafe {
        let mon = MonitorFromPoint(Default::default(), MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi.rcWork
    }
}

/// Használható-e még a mentett állapot (létezik-e a monitor, amin az ablak volt).
pub fn is_on_screen(s: &WindowState) -> bool {
    let rc = RECT { left: s.left, top: s.top, right: s.right, bottom: s.bottom };
    unsafe { !MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_invalid() }
}

/// Megjeleníti az ablakot a mentett állapot szerint.
pub fn apply(hwnd: HWND, s: &WindowState) {
    let wp = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        flags: WINDOWPLACEMENT_FLAGS(0),
        showCmd: if s.maximized { SW_SHOWMAXIMIZED.0 as u32 } else { SW_SHOWNORMAL.0 as u32 },
        rcNormalPosition: RECT { left: s.left, top: s.top, right: s.right, bottom: s.bottom },
        ..Default::default()
    };
    unsafe {
        let _ = SetWindowPlacement(hwnd, &wp);
    }
}

pub fn get(hwnd: HWND) -> Option<WINDOWPLACEMENT> {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut wp).ok().map(|_| wp) }
}

pub fn to_state(wp: &WINDOWPLACEMENT, client: (u32, u32)) -> WindowState {
    let r = wp.rcNormalPosition;
    let maximized = if wp.showCmd == SW_SHOWMINIMIZED.0 as u32 {
        wp.flags.0 & WPF_RESTORETOMAXIMIZED.0 != 0
    } else {
        wp.showCmd == SW_SHOWMAXIMIZED.0 as u32
    };
    WindowState {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
        maximized,
        client_w: client.0,
        client_h: client.1,
    }
}

/// Teljes képernyő be: keret nélküli ablak a monitor teljes területén.
/// Visszaadja a korábbi elhelyezést, amivel később visszaállítható.
pub fn enter_fullscreen(hwnd: HWND) -> Option<WINDOWPLACEMENT> {
    let saved = get(hwnd)?;
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SetWindowLongW(hwnd, GWL_STYLE, (style & !WS_OVERLAPPEDWINDOW.0) as i32);
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let r = mi.rcMonitor;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOP),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
    Some(saved)
}

pub fn exit_fullscreen(hwnd: HWND, saved: &WINDOWPLACEMENT) {
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        SetWindowLongW(hwnd, GWL_STYLE, (style | WS_OVERLAPPEDWINDOW.0) as i32);
        let _ = SetWindowPlacement(hwnd, saved);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
        );
    }
}
