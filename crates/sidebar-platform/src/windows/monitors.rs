//! Monitors: bounds, work areas and DPI, and the mapping between
//! physical pixels and the sidebar's logical coordinates.
//!
//! Windows reports everything in physical pixels (the process is per-
//! monitor DPI aware). Logical coordinates are physical ones divided by the
//! scale of the monitor they are on, origin included. With one scale on
//! all monitors this is an exact mapping; with mixed scales, monitors can
//! leave gaps or overlap in logical space, but every point still maps to
//! exactly one monitor and back.

use std::mem::{size_of, zeroed};

use sidebar_core::{MonitorId, MonitorInfo, Point, Rect};
use windows_sys::Win32::Foundation::{LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows_sys::core::BOOL;

/// A monitor as Windows describes it, in physical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub id: MonitorId,
    pub name: String,
    pub bounds: (i32, i32, i32, i32),
    pub work: (i32, i32, i32, i32),
    pub scale: f32,
    pub primary: bool,
}

impl Monitor {
    pub fn info(&self) -> MonitorInfo {
        MonitorInfo {
            id: self.id,
            name: self.name.clone(),
            bounds: to_logical(self.bounds, self.scale),
            work_area: to_logical(self.work, self.scale),
            scale: self.scale,
            primary: self.primary,
        }
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        let (l, t, r, b) = self.bounds;
        x >= l && x < r && y >= t && y < b
    }

    fn distance(&self, x: i32, y: i32) -> i64 {
        let (l, t, r, b) = self.bounds;
        let dx = (l - x).max(x - (r - 1)).max(0) as i64;
        let dy = (t - y).max(y - (b - 1)).max(0) as i64;
        dx * dx + dy * dy
    }
}

/// (left, top, right, bottom) physical to a logical rect.
fn to_logical((l, t, r, b): (i32, i32, i32, i32), scale: f32) -> Rect {
    Rect::new(l as f32 / scale, t as f32 / scale, (r - l) as f32 / scale, (b - t) as f32 / scale)
}

/// A logical rect to physical (x, y, width, height), at `scale`.
pub fn to_physical(r: Rect, scale: f32) -> (i32, i32, i32, i32) {
    let x = (r.x * scale).round() as i32;
    let y = (r.y * scale).round() as i32;
    // Round the far edges, so adjacent rects stay adjacent.
    let right = ((r.x + r.width) * scale).round() as i32;
    let bottom = ((r.y + r.height) * scale).round() as i32;
    (x, y, (right - x).max(1), (bottom - y).max(1))
}

/// A physical screen point in logical coordinates, using the scale of the
/// monitor it is on (or nearest to).
pub fn point_to_logical(monitors: &[Monitor], x: i32, y: i32) -> Point {
    let scale = monitors
        .iter()
        .find(|m| m.contains(x, y))
        .or_else(|| monitors.iter().min_by_key(|m| m.distance(x, y)))
        .map_or(1.0, |m| m.scale);
    Point::new(x as f32 / scale, y as f32 / scale)
}

/// A stable id from the GDI device name ("\\.\DISPLAY2" is 2), which
/// survives monitors being added and removed.
fn id_of(device: &str, fallback: u32) -> MonitorId {
    let digits: String = device.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    MonitorId(digits.parse().unwrap_or(1000 + fallback))
}

pub fn enumerate() -> Vec<Monitor> {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the Vec passed to EnumDisplayMonitors below,
        // which outlives the enumeration.
        unsafe { (*(data as *mut Vec<HMONITOR>)).push(monitor) };
        1
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `handles`.
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(collect), &mut handles as *mut _ as LPARAM) };

    let mut monitors: Vec<Monitor> = handles
        .into_iter()
        .enumerate()
        .filter_map(|(i, h)| {
            // SAFETY: cbSize selects the MONITORINFOEXW layout; the DPI
            // call only writes its out parameters.
            let (info, dpi) = unsafe {
                let mut info: MONITORINFOEXW = zeroed();
                info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
                if GetMonitorInfoW(h, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) == 0 {
                    return None;
                }
                let (mut dx, mut dy) = (96u32, 96u32);
                if GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) < 0 {
                    dx = 96;
                }
                (info, dx)
            };
            let device = crate::windows::from_wide(&info.szDevice);
            let r = |r: RECT| (r.left, r.top, r.right, r.bottom);
            Some(Monitor {
                id: id_of(&device, i as u32),
                name: device.trim_start_matches(r"\\.\").to_string(),
                bounds: r(info.monitorInfo.rcMonitor),
                work: r(info.monitorInfo.rcWork),
                scale: dpi as f32 / 96.0,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            })
        })
        .collect();
    monitors.sort_by_key(|m| m.id.0);
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: u32, bounds: (i32, i32, i32, i32), scale: f32) -> Monitor {
        Monitor { id: MonitorId(id), name: format!("DISPLAY{id}"), bounds, work: bounds, scale, primary: id == 1 }
    }

    #[test]
    fn maps_between_physical_and_logical() {
        let m = monitor(1, (0, 0, 2880, 1800), 1.5);
        assert_eq!(m.info().bounds, Rect::new(0.0, 0.0, 1920.0, 1200.0));
        assert_eq!(to_physical(Rect::new(1760.0, 0.0, 160.0, 1200.0), 1.5), (2640, 0, 240, 1800));
        let monitors = [m, monitor(2, (2880, 0, 4800, 1080), 1.0)];
        assert_eq!(point_to_logical(&monitors, 1500, 300), Point::new(1000.0, 200.0));
        assert_eq!(point_to_logical(&monitors, 3000, 10), Point::new(3000.0, 10.0));
        // Outside every monitor: the nearest one's scale.
        assert_eq!(point_to_logical(&monitors, -30, 0), Point::new(-20.0, 0.0));
    }

    #[test]
    fn ids_follow_device_names() {
        assert_eq!(id_of(r"\\.\DISPLAY2", 0), MonitorId(2));
        assert_eq!(id_of("odd", 3), MonitorId(1003));
    }
}
