//! The window class and its window procedure.
//!
//! Windows calls the window procedure from inside `DispatchMessageW`, and
//! also re-entrantly from calls such as `SetWindowPos`. It only records
//! what happened in a thread-local queue, which `poll_events` turns into
//! `PlatformEvent`s with access to the platform. The few messages that need
//! an answer straight away (stacking, activation, painting) are answered
//! here from small per-window flags.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Once;

use sidebar_core::{Key, PointerButton};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, ClientToScreen, DIB_RGB_COLORS, EndPaint, PAINTSTRUCT, ScreenToClient, SetDIBitsToDevice,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, HWND_BOTTOM, IDC_ARROW, LoadCursorW, MA_NOACTIVATE, RegisterClassExW, SPI_SETWORKAREA, SWP_NOZORDER,
    WINDOWPOS, WM_CHAR, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_QUERYENDSESSION,
    WM_APP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WNDCLASSEXW,
};

use windows_sys::Win32::UI::Shell::{ABM_WINDOWPOSCHANGED, APPBARDATA, SHAppBarMessage};

use super::wide;

pub const CLASS: &str = "sidebar-surface";
/// From CommCtrl.h; not worth a windows-sys feature.
const WM_MOUSELEAVE: u32 = 0x02A3;
const WHEEL_DELTA: f32 = 120.0;
/// The message the shell sends AppBar notifications with.
pub const WM_APPBAR: u32 = WM_APP + 1;

/// Something the window procedure saw. Positions are physical pixels:
/// `client` relative to the window, `screen` on the desktop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Raw {
    Moved { hwnd: HWND, client: (i32, i32), screen: (i32, i32) },
    Button { hwnd: HWND, client: (i32, i32), screen: (i32, i32), button: PointerButton, pressed: bool },
    Scroll { hwnd: HWND, client: (i32, i32), screen: (i32, i32), dx: f32, dy: f32 },
    Left { hwnd: HWND },
    Key { hwnd: HWND, key: Key },
    Close { hwnd: HWND },
    /// Monitors, DPI or work areas may have changed.
    Display,
    /// An AppBar notification (`ABN_*` in `notify`).
    AppBar { hwnd: HWND, notify: u32, lparam: LPARAM },
    Quit,
}

/// A frame for an ordinary window, painted on WM_PAINT.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// B, G, R, A rows, top-down.
    pub bgra: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    events: Vec<Raw>,
    /// Windows kept at the bottom of the z-order.
    bottom: HashSet<isize>,
    /// Ordinary windows: activate on click, take the keyboard.
    windows: HashSet<isize>,
    /// Windows with a pending WM_MOUSELEAVE request.
    tracking: HashSet<isize>,
    /// Windows registered as AppBars.
    appbars: HashSet<isize>,
    frames: HashMap<isize, Frame>,
    /// First half of a UTF-16 surrogate pair from WM_CHAR.
    high_surrogate: Option<u16>,
}

thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}

fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    SHARED.with(|s| f(&mut s.borrow_mut()))
}

fn push(raw: Raw) {
    with(|s| s.events.push(raw));
}

pub fn take_events() -> Vec<Raw> {
    with(|s| std::mem::take(&mut s.events))
}

pub fn set_bottom(hwnd: HWND, bottom: bool) {
    with(|s| if bottom { s.bottom.insert(hwnd as isize) } else { s.bottom.remove(&(hwnd as isize)) });
}

pub fn set_window(hwnd: HWND) {
    with(|s| s.windows.insert(hwnd as isize));
}

pub fn set_frame(hwnd: HWND, frame: Frame) {
    with(|s| s.frames.insert(hwnd as isize, frame));
}

pub fn set_appbar(hwnd: HWND, registered: bool) {
    with(|s| if registered { s.appbars.insert(hwnd as isize) } else { s.appbars.remove(&(hwnd as isize)) });
}

pub fn forget(hwnd: HWND) {
    let key = hwnd as isize;
    with(|s| {
        s.bottom.remove(&key);
        s.windows.remove(&key);
        s.tracking.remove(&key);
        s.frames.remove(&key);
        s.appbars.remove(&key);
    });
}

pub fn register_class() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let name = wide(CLASS);
        // SAFETY: the class struct is fully initialized; the class name is
        // copied by RegisterClassExW.
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&class);
        }
    });
}

/// Signed client coordinates from a mouse message's LPARAM.
fn point_of(lparam: LPARAM) -> (i32, i32) {
    ((lparam & 0xffff) as i16 as i32, ((lparam >> 16) & 0xffff) as i16 as i32)
}

/// The key for a WM_KEYDOWN virtual key; printable keys come as WM_CHAR.
fn key_of(vk: u16, shift: bool) -> Option<Key> {
    Some(match vk {
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_TAB if shift => Key::BackTab,
        VK_TAB => Key::Tab,
        _ => return None,
    })
}

/// A character from WM_CHAR, joining surrogate pairs. Control characters
/// (from Ctrl combinations, and Enter/Tab/Backspace, which WM_KEYDOWN
/// already reported) are dropped.
fn char_of(unit: u16, pending: &mut Option<u16>) -> Option<char> {
    match unit {
        0xD800..=0xDBFF => {
            *pending = Some(unit);
            None
        }
        0xDC00..=0xDFFF => {
            let high = pending.take()?;
            char::decode_utf16([high, unit]).next()?.ok()
        }
        _ => {
            *pending = None;
            char::from_u32(unit as u32).filter(|c| !c.is_control())
        }
    }
}

unsafe fn screen_of(hwnd: HWND, client: (i32, i32)) -> (i32, i32) {
    let mut p = POINT { x: client.0, y: client.1 };
    // SAFETY: valid window and out pointer.
    unsafe { ClientToScreen(hwnd, &mut p) };
    (p.x, p.y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let key = hwnd as isize;
    // SAFETY (whole function): `hwnd` is the window this message is for;
    // the pointers dereferenced are the ones Windows documents for each
    // message.
    unsafe {
        match msg {
            WM_MOUSEMOVE => {
                if with(|s| s.tracking.insert(key)) {
                    let mut t = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    TrackMouseEvent(&mut t);
                }
                let client = point_of(lparam);
                push(Raw::Moved { hwnd, client, screen: screen_of(hwnd, client) });
                0
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
                let (button, pressed) = match msg {
                    WM_LBUTTONDOWN => (PointerButton::Primary, true),
                    WM_LBUTTONUP => (PointerButton::Primary, false),
                    WM_RBUTTONDOWN => (PointerButton::Secondary, true),
                    WM_RBUTTONUP => (PointerButton::Secondary, false),
                    WM_MBUTTONDOWN => (PointerButton::Middle, true),
                    _ => (PointerButton::Middle, false),
                };
                // Keep getting the pointer while a button is down, even
                // outside the window: dragging relies on it.
                const ANY_BUTTON: usize = 0x0001 | 0x0002 | 0x0010;
                if pressed {
                    SetCapture(hwnd);
                } else if wparam & ANY_BUTTON == 0 && GetCapture() == hwnd {
                    ReleaseCapture();
                }
                let client = point_of(lparam);
                push(Raw::Button { hwnd, client, screen: screen_of(hwnd, client), button, pressed });
                0
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                // Wheel messages carry screen coordinates.
                let screen = point_of(lparam);
                let mut p = POINT { x: screen.0, y: screen.1 };
                ScreenToClient(hwnd, &mut p);
                let notches = ((wparam >> 16) & 0xffff) as i16 as f32 / WHEEL_DELTA;
                // Up is negative dy, as on X11 and Wayland; right is positive dx.
                let (dx, dy) = if msg == WM_MOUSEWHEEL { (0.0, -notches) } else { (notches, 0.0) };
                push(Raw::Scroll { hwnd, client: (p.x, p.y), screen, dx, dy });
                0
            }
            WM_MOUSELEAVE => {
                with(|s| s.tracking.remove(&key));
                // During a drag the window keeps the pointer; Windows sends
                // the leave again after the button is released outside.
                if GetCapture() != hwnd {
                    push(Raw::Left { hwnd });
                }
                0
            }
            WM_MOUSEACTIVATE if !with(|s| s.windows.contains(&key)) => MA_NOACTIVATE as LRESULT,
            WM_WINDOWPOSCHANGING => {
                if with(|s| s.bottom.contains(&key)) {
                    let pos = &mut *(lparam as *mut WINDOWPOS);
                    pos.hwndInsertAfter = HWND_BOTTOM;
                    pos.flags &= !SWP_NOZORDER;
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_KEYDOWN => {
                let shift = GetKeyState(VK_SHIFT as i32) < 0;
                if let Some(k) = key_of(wparam as u16, shift) {
                    push(Raw::Key { hwnd, key: k });
                }
                0
            }
            WM_CHAR => {
                if let Some(c) = with(|s| char_of(wparam as u16, &mut s.high_surrogate)) {
                    push(Raw::Key { hwnd, key: Key::Char(c) });
                }
                0
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                with(|s| {
                    if let Some(f) = s.frames.get(&key) {
                        let info = bitmap_info(f.width, f.height);
                        SetDIBitsToDevice(dc, 0, 0, f.width, f.height, 0, 0, 0, f.height, f.bgra.as_ptr().cast(), &info, DIB_RGB_COLORS);
                    }
                });
                EndPaint(hwnd, &ps);
                0
            }
            WM_ERASEBKGND => 1,
            WM_CLOSE => {
                push(Raw::Close { hwnd });
                0
            }
            WM_WINDOWPOSCHANGED => {
                // The shell asks AppBars to report every move.
                if with(|s| s.appbars.contains(&key)) {
                    let mut abd = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, hWnd: hwnd, ..Default::default() };
                    SHAppBarMessage(ABM_WINDOWPOSCHANGED, &mut abd);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_APPBAR => {
                push(Raw::AppBar { hwnd, notify: wparam as u32, lparam });
                0
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => {
                push(Raw::Display);
                0
            }
            WM_SETTINGCHANGE => {
                if wparam == SPI_SETWORKAREA as usize {
                    push(Raw::Display);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_QUERYENDSESSION => 1,
            WM_ENDSESSION => {
                if wparam != 0 {
                    push(Raw::Quit);
                }
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// A top-down 32-bit DIB header.
pub fn bitmap_info(width: u32, height: u32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: rows go top to bottom.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        bmiColors: [Default::default()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_signed_points() {
        assert_eq!(point_of(0x0020_0010), (16, 32));
        // (-5, -1) while the pointer is captured outside the window.
        assert_eq!(point_of(0xffff_fffb_u32 as isize), (-5, -1));
    }

    #[test]
    fn maps_keys_and_chars() {
        assert_eq!(key_of(VK_TAB, true), Some(Key::BackTab));
        assert_eq!(key_of(VK_RETURN, false), Some(Key::Enter));
        assert_eq!(key_of(0x41, false), None);
        let mut pending = None;
        assert_eq!(char_of('ñ' as u16, &mut pending), Some('ñ'));
        assert_eq!(char_of(0x08, &mut pending), None);
        // U+1F600 as a surrogate pair.
        assert_eq!(char_of(0xD83D, &mut pending), None);
        assert_eq!(char_of(0xDE00, &mut pending), Some('😀'));
        assert_eq!(char_of(0xDE00, &mut pending), None);
    }
}
