//! Reserving screen space: docks register as Application Desktop Toolbars
//! (AppBars), like the taskbar. Windows then keeps maximized windows out of
//! the reserved strip and lowers the monitor's work area.
//!
//! The protocol: `ABM_NEW` once; then, whenever the dock or the screen
//! changes, propose the full edge strip with `ABM_QUERYPOS` (the shell
//! moves it clear of the taskbar and other AppBars), cut it to the dock's
//! thickness, and claim it with `ABM_SETPOS`. `ABM_REMOVE` gives it back.

use sidebar_core::Edge;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA, SHAppBarMessage,
};

use super::wndproc::{self, WM_APPBAR};

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: hwnd, uCallbackMessage: WM_APPBAR, ..Default::default() }
}

pub fn register(hwnd: HWND) -> bool {
    let mut abd = data(hwnd);
    // SAFETY: a valid APPBARDATA for our window.
    let ok = unsafe { SHAppBarMessage(ABM_NEW, &mut abd) } != 0;
    if ok {
        wndproc::set_appbar(hwnd, true);
    }
    ok
}

pub fn remove(hwnd: HWND) {
    let mut abd = data(hwnd);
    wndproc::set_appbar(hwnd, false);
    // SAFETY: as above.
    unsafe { SHAppBarMessage(ABM_REMOVE, &mut abd) };
}

fn abe(edge: Edge) -> u32 {
    match edge {
        Edge::Left => ABE_LEFT,
        Edge::Right => ABE_RIGHT,
        Edge::Top => ABE_TOP,
        Edge::Bottom => ABE_BOTTOM,
    }
}

/// The strip `thickness` physical pixels deep along `edge` of `monitor`
/// (left, top, right, bottom) that the shell would grant: inset from the
/// monitor edge by the taskbar or other AppBars on that side.
pub fn query(hwnd: HWND, edge: Edge, monitor: (i32, i32, i32, i32), thickness: i32) -> RECT {
    let mut abd = data(hwnd);
    abd.uEdge = abe(edge);
    let (l, t, r, b) = monitor;
    abd.rc = RECT { left: l, top: t, right: r, bottom: b };
    // SAFETY: as above; the shell adjusts `rc` in place.
    unsafe { SHAppBarMessage(ABM_QUERYPOS, &mut abd) };
    fit(edge, abd.rc, thickness)
}

/// Claims a strip from [`query`]. Every claim makes the shell recompute
/// work areas and notify the other AppBars, so only claim changes.
pub fn set(hwnd: HWND, edge: Edge, rc: RECT) {
    let mut abd = data(hwnd);
    abd.uEdge = abe(edge);
    abd.rc = rc;
    // SAFETY: as above.
    unsafe { SHAppBarMessage(ABM_SETPOS, &mut abd) };
}

/// Cuts a proposed strip to `thickness`, measured from its outer side.
fn fit(edge: Edge, mut rc: RECT, thickness: i32) -> RECT {
    match edge {
        Edge::Left => rc.right = rc.left + thickness,
        Edge::Right => rc.left = rc.right - thickness,
        Edge::Top => rc.bottom = rc.top + thickness,
        Edge::Bottom => rc.top = rc.bottom - thickness,
    }
    rc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_strips_to_thickness() {
        // A 1920x1080 monitor with a 48 px taskbar at the bottom.
        let proposed = RECT { left: 0, top: 0, right: 1920, bottom: 1032 };
        let r = fit(Edge::Right, proposed, 160);
        assert_eq!((r.left, r.top, r.right, r.bottom), (1760, 0, 1920, 1032));
        let r = fit(Edge::Left, proposed, 160);
        assert_eq!((r.left, r.right), (0, 160));
        let r = fit(Edge::Bottom, proposed, 40);
        assert_eq!((r.top, r.bottom), (992, 1032));
        let r = fit(Edge::Top, proposed, 40);
        assert_eq!((r.top, r.bottom), (0, 40));
    }
}
