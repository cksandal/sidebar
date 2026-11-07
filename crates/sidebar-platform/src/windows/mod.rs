//! Windows backend (Win32 through windows-sys).
//!
//! - Docks, floating widgets and the drag preview are layered popup windows
//!   (`WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`): no taskbar
//!   button, never activated by a click, and drawn with per-pixel alpha
//!   through `UpdateLayeredWindow` from a DIB section.
//! - The settings window is an ordinary captioned window painted from a
//!   stored frame on `WM_PAINT`.
//! - The process is per-monitor DPI aware (v2); see [`monitors`] for how
//!   physical pixels map to logical coordinates.
//! - Stacking: `HWND_TOPMOST` when `above`, otherwise kept at the bottom of
//!   the z-order by rewriting `WM_WINDOWPOSCHANGING`.
//!
//! - Docks that reserve space are AppBars; see [`appbar`].

mod appbar;
mod monitors;
mod wndproc;

use std::collections::BTreeMap;
use std::time::Duration;

use sidebar_core::{Edge, InputEvent, MonitorInfo, Point, Scene, SurfaceKind, SurfaceSpec, SurfaceTarget, snap};
use sidebar_render::AlphaMode;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, InvalidateRect, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, HWND_BOTTOM, HWND_TOPMOST, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx,
    PM_REMOVE, PeekMessageW, QS_ALLINPUT, SW_SHOW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, SetWindowTextW,
    ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CAPTION, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_POPUP, WS_SYSMENU,
};

use windows_sys::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};

use self::monitors::Monitor;
use self::wndproc::Raw;
use crate::{Capabilities, Platform, PlatformError, PlatformEvent, Result, SurfaceId};

/// Null-terminated UTF-16.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// UTF-16 up to the first NUL.
fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

fn id_of(hwnd: HWND) -> SurfaceId {
    SurfaceId(hwnd as usize as u64)
}

pub struct WindowsPlatform {
    monitors: Vec<Monitor>,
    surfaces: BTreeMap<SurfaceId, Surface>,
    /// A hidden top-level window, so display changes are heard even with
    /// no surfaces (message-only windows don't get broadcasts).
    listener: HWND,
    /// SIDEBAR_TRACE=1: log every message the window procedure records.
    trace: bool,
}

struct Surface {
    hwnd: HWND,
    spec: SurfaceSpec,
    /// Layered surfaces draw through this; ordinary windows on WM_PAINT.
    dib: Option<Dib>,
    /// Physical top-left as last placed (docks can be inset by the taskbar).
    origin: Option<(i32, i32)>,
    /// Registered as an AppBar, and the strip it holds.
    appbar: bool,
    reserved: Option<(Edge, RECT)>,
    /// A full-screen app is on this dock's monitor: give up topmost.
    fullscreen: bool,
}

impl Surface {
    fn layered(&self) -> bool {
        !matches!(self.spec.kind, SurfaceKind::Window { .. })
    }
}

/// A memory DC with a 32-bit top-down DIB section selected into it.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
}

impl Dib {
    fn new(width: u32, height: u32) -> Option<Self> {
        let info = wndproc::bitmap_info(width, height);
        // SAFETY: a memory DC compatible with the screen; the DIB's bits
        // pointer stays valid until the bitmap is deleted in Drop.
        unsafe {
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return None;
            }
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap);
            Some(Self { dc, bitmap, old, bits: bits.cast(), width, height })
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the DIB section is width * height 32-bit pixels.
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.width as usize * self.height as usize * 4) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: objects created in `new`, released once.
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

impl WindowsPlatform {
    pub fn connect() -> Result<Self> {
        // SAFETY: plain Win32 calls; fails harmlessly if a manifest already
        // set the awareness.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        wndproc::register_class();
        let monitors = monitors::enumerate();
        if monitors.is_empty() {
            return Err(PlatformError::Unsupported("no monitors found".into()));
        }
        let listener = create_window(WS_EX_TOOLWINDOW, WS_POPUP, "sidebar", (0, 0, 0, 0))?;
        let trace = std::env::var_os("SIDEBAR_TRACE").is_some_and(|v| v == "1");
        Ok(Self { monitors, surfaces: BTreeMap::new(), listener, trace })
    }

    fn monitor(&self, spec: &SurfaceSpec) -> &Monitor {
        self.monitors
            .iter()
            .find(|m| m.id == spec.monitor)
            .or_else(|| self.monitors.iter().find(|m| m.primary))
            .unwrap_or(&self.monitors[0])
    }

    /// Outer window rect in physical pixels: (x, y, width, height).
    fn placement(&self, spec: &SurfaceSpec) -> (i32, i32, i32, i32) {
        let monitor = self.monitor(spec);
        let info = monitor.info();
        let scale = monitor.scale;
        match &spec.kind {
            SurfaceKind::Dock { edge, thickness, .. } => monitors::to_physical(snap::dock_rect(info.bounds, *edge, *thickness), scale),
            SurfaceKind::Floating { rect } => monitors::to_physical(*rect, scale),
            SurfaceKind::Window { .. } => {
                let (w, h) = ((spec.size.width * scale).round() as i32, (spec.size.height * scale).round() as i32);
                let mut outer = RECT { left: 0, top: 0, right: w, bottom: h };
                let (ex, style) = styles(spec);
                // SAFETY: valid in/out rect.
                unsafe { AdjustWindowRectExForDpi(&mut outer, style, 0, ex, (scale * 96.0).round() as u32) };
                let (ow, oh) = (outer.right - outer.left, outer.bottom - outer.top);
                let (l, t, r, b) = monitor.work;
                ((l + r - ow) / 2, (t + b - oh) / 2, ow, oh)
            }
        }
    }

    /// Moves, resizes and restacks a surface to match its spec, and
    /// reserves (or releases) its screen space.
    fn apply(&mut self, id: SurfaceId) {
        let Some(s) = self.surfaces.get(&id) else { return };
        let (mut x, mut y, w, h) = self.placement(&s.spec);
        let hwnd = s.hwnd;
        let window = !s.layered();
        let reserve = match s.spec.kind {
            SurfaceKind::Dock { edge, reserve_space: true, .. } => Some(edge),
            _ => None,
        };
        let bounds = self.monitor(&s.spec).bounds;
        let (mut registered, mut reserved) = (s.appbar, s.reserved);
        match reserve {
            Some(edge) => {
                if !registered {
                    registered = appbar::register(hwnd);
                    reserved = None;
                }
                if registered {
                    let thickness = if matches!(edge, Edge::Left | Edge::Right) { w } else { h };
                    let rc = appbar::query(hwnd, edge, bounds, thickness);
                    if reserved.is_none_or(|(e, r)| e != edge || !same_rect(&r, &rc)) {
                        appbar::set(hwnd, edge, rc);
                        reserved = Some((edge, rc));
                    }
                    // Across the dock, stay clear of the taskbar; along it,
                    // keep the full length the scene was laid out for.
                    match edge {
                        Edge::Left | Edge::Right => x = rc.left,
                        Edge::Top | Edge::Bottom => y = rc.top,
                    }
                }
            }
            None if registered => {
                appbar::remove(hwnd);
                registered = false;
                reserved = None;
            }
            None => {}
        }
        let Some(s) = self.surfaces.get_mut(&id) else { return };
        s.appbar = registered;
        s.reserved = reserved;
        s.origin = Some((x, y));
        let on_top = (s.spec.above || matches!(s.spec.target, SurfaceTarget::DragPreview(_))) && !s.fullscreen;
        // Ordinary windows stack like any other window.
        if !window {
            wndproc::set_bottom(hwnd, !on_top);
        }
        let after = if window { std::ptr::null_mut() } else if on_top { HWND_TOPMOST } else { HWND_BOTTOM };
        let flags = SWP_NOACTIVATE | if window { SWP_NOZORDER } else { 0 };
        // SAFETY: a window this platform created.
        unsafe { SetWindowPos(hwnd, after, x, y, w, h, flags) };
    }

    /// Re-reads monitors; true if anything changed.
    fn refresh_monitors(&mut self) -> bool {
        let now = monitors::enumerate();
        if now.is_empty() || now == self.monitors {
            return false;
        }
        self.monitors = now;
        let ids: Vec<SurfaceId> = self.surfaces.keys().copied().collect();
        for id in ids {
            self.apply(id);
        }
        true
    }

    fn pump(&self) {
        // SAFETY: standard message loop on the thread that owns the windows.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn scale_of(&self, s: &Surface) -> f32 {
        self.monitor(&s.spec).scale
    }

    fn translate(&mut self, raw: Raw, out: &mut Vec<PlatformEvent>) {
        let local = |s: &Surface, (x, y): (i32, i32)| {
            let scale = self.scale_of(s);
            Point::new(x as f32 / scale, y as f32 / scale)
        };
        let global = |(x, y): (i32, i32)| Some(monitors::point_to_logical(&self.monitors, x, y));
        match raw {
            Raw::Moved { hwnd, client, screen } => {
                let Some(s) = self.surfaces.get(&id_of(hwnd)) else { return };
                let event = InputEvent::PointerMoved { pos: local(s, client) };
                out.push(PlatformEvent::Input { surface: id_of(hwnd), event, global: global(screen) });
            }
            Raw::Button { hwnd, client, screen, button, pressed } => {
                let Some(s) = self.surfaces.get(&id_of(hwnd)) else { return };
                let event = InputEvent::PointerButton { pos: local(s, client), button, pressed };
                out.push(PlatformEvent::Input { surface: id_of(hwnd), event, global: global(screen) });
            }
            Raw::Scroll { hwnd, client, screen, dx, dy } => {
                let Some(s) = self.surfaces.get(&id_of(hwnd)) else { return };
                let event = InputEvent::Scroll { pos: local(s, client), dx, dy };
                out.push(PlatformEvent::Input { surface: id_of(hwnd), event, global: global(screen) });
            }
            Raw::Left { hwnd } if self.surfaces.contains_key(&id_of(hwnd)) => {
                out.push(PlatformEvent::Input { surface: id_of(hwnd), event: InputEvent::PointerLeft, global: None });
            }
            Raw::Key { hwnd, key } if self.surfaces.contains_key(&id_of(hwnd)) => {
                out.push(PlatformEvent::Input { surface: id_of(hwnd), event: InputEvent::Key(key), global: None });
            }
            Raw::Close { hwnd } if self.surfaces.contains_key(&id_of(hwnd)) => {
                out.push(PlatformEvent::CloseRequested { surface: id_of(hwnd) });
            }
            // `taskkill` without /f closes every top-level window, including
            // the hidden one.
            Raw::Close { hwnd } if hwnd == self.listener => out.push(PlatformEvent::Quit),
            Raw::Display => {
                if self.refresh_monitors() && !out.contains(&PlatformEvent::MonitorsChanged) {
                    out.push(PlatformEvent::MonitorsChanged);
                }
            }
            Raw::AppBar { hwnd, notify, lparam } => {
                let id = id_of(hwnd);
                match notify {
                    // The taskbar or another AppBar moved: claim again.
                    ABN_POSCHANGED => self.apply(id),
                    ABN_FULLSCREENAPP => {
                        if let Some(s) = self.surfaces.get_mut(&id) {
                            s.fullscreen = lparam != 0;
                        }
                        self.apply(id);
                    }
                    _ => {}
                }
            }
            Raw::Quit => out.push(PlatformEvent::Quit),
            _ => {}
        }
    }
}

fn same_rect(a: &RECT, b: &RECT) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}

fn styles(spec: &SurfaceSpec) -> (WINDOW_EX_STYLE, WINDOW_STYLE) {
    let popup = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    match (&spec.kind, spec.target) {
        (SurfaceKind::Window { .. }, _) => (0, WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX),
        // The preview follows the pointer; clicks go through it.
        (_, SurfaceTarget::DragPreview(_)) => (popup | WS_EX_TRANSPARENT | WS_EX_TOPMOST, WS_POPUP),
        _ if spec.above => (popup | WS_EX_TOPMOST, WS_POPUP),
        _ => (popup, WS_POPUP),
    }
}

fn create_window(ex: WINDOW_EX_STYLE, style: WINDOW_STYLE, title: &str, (x, y, w, h): (i32, i32, i32, i32)) -> Result<HWND> {
    let class = wide(wndproc::CLASS);
    let title = wide(title);
    // SAFETY: the class is registered; strings outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(
            ex,
            class.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(PlatformError::Backend(format!("CreateWindowExW failed: {}", std::io::Error::last_os_error())));
    }
    Ok(hwnd)
}

impl Drop for WindowsPlatform {
    fn drop(&mut self) {
        let ids: Vec<SurfaceId> = self.surfaces.keys().copied().collect();
        for id in ids {
            self.destroy_surface(id);
        }
        // SAFETY: created in `connect`.
        unsafe { DestroyWindow(self.listener) };
    }
}

impl Platform for WindowsPlatform {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { reserve_space: true, free_positioning: true, global_pointer: true, transparency: true }
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.iter().map(Monitor::info).collect()
    }

    fn create_surface(&mut self, spec: &SurfaceSpec) -> Result<SurfaceId> {
        let rect = self.placement(spec);
        let (ex, style) = styles(spec);
        let title = match &spec.kind {
            SurfaceKind::Window { title } => title.as_str(),
            _ => "sidebar",
        };
        let hwnd = create_window(ex, style, title, rect)?;
        let id = id_of(hwnd);
        if matches!(spec.kind, SurfaceKind::Window { .. }) {
            wndproc::set_window(hwnd);
        }
        self.surfaces.insert(id, Surface { hwnd, spec: spec.clone(), dib: None, origin: None, appbar: false, reserved: None, fullscreen: false });
        self.apply(id);
        // SAFETY: a window just created.
        unsafe {
            if matches!(spec.kind, SurfaceKind::Window { .. }) {
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
            } else {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        Ok(id)
    }

    fn configure_surface(&mut self, surface: SurfaceId, spec: &SurfaceSpec) -> Result<()> {
        let s = self.surfaces.get_mut(&surface).ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
        if let (SurfaceKind::Window { title }, SurfaceKind::Window { title: old }) = (&spec.kind, &s.spec.kind)
            && title != old
        {
            let t = wide(title);
            // SAFETY: our window; NUL-terminated title.
            unsafe { SetWindowTextW(s.hwnd, t.as_ptr()) };
        }
        s.spec = spec.clone();
        self.apply(surface);
        Ok(())
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        if let Some(s) = self.surfaces.remove(&surface) {
            if s.appbar {
                appbar::remove(s.hwnd);
            }
            wndproc::forget(s.hwnd);
            drop(s.dib);
            // SAFETY: a window this platform created, destroyed once.
            unsafe { DestroyWindow(s.hwnd) };
        }
    }

    fn present(&mut self, surface: SurfaceId, scene: &Scene) -> Result<()> {
        let (x, y) = {
            let s = self.surfaces.get(&surface).ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
            s.origin.unwrap_or_else(|| {
                let (x, y, _, _) = self.placement(&s.spec);
                (x, y)
            })
        };
        let scale = {
            let s = &self.surfaces[&surface];
            self.scale_of(s)
        };
        let pixmap = sidebar_render::rasterize(scene, scale);
        if pixmap.width == 0 || pixmap.height == 0 {
            return Ok(());
        }
        let s = self.surfaces.get_mut(&surface).expect("checked above");
        if !s.layered() {
            let mut bgra = vec![0; pixmap.data.len()];
            pixmap.write_bgra(&mut bgra, AlphaMode::Opaque);
            wndproc::set_frame(s.hwnd, wndproc::Frame { width: pixmap.width, height: pixmap.height, bgra });
            // SAFETY: our window.
            unsafe { InvalidateRect(s.hwnd, std::ptr::null(), 0) };
            return Ok(());
        }
        if s.dib.as_ref().is_none_or(|d| (d.width, d.height) != (pixmap.width, pixmap.height)) {
            s.dib = Some(Dib::new(pixmap.width, pixmap.height).ok_or_else(|| PlatformError::Backend("CreateDIBSection failed".into()))?);
        }
        let dib = s.dib.as_mut().expect("just set");
        pixmap.write_bgra(dib.pixels(), AlphaMode::Premultiplied);
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let (pos, size, origin) = (POINT { x, y }, SIZE { cx: pixmap.width as i32, cy: pixmap.height as i32 }, POINT { x: 0, y: 0 });
        // SAFETY: our layered window and a DC holding a DIB of `size`.
        let ok = unsafe { UpdateLayeredWindow(s.hwnd, std::ptr::null_mut(), &pos, &size, dib.dc, &origin, 0, &blend, ULW_ALPHA) };
        if ok == 0 {
            return Err(PlatformError::Backend(format!("UpdateLayeredWindow failed: {}", std::io::Error::last_os_error())));
        }
        Ok(())
    }

    fn poll_events(&mut self, timeout: Duration) -> Vec<PlatformEvent> {
        self.pump();
        let mut raw = wndproc::take_events();
        if raw.is_empty() {
            let ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
            // SAFETY: no handles; wakes on any input or the timeout.
            unsafe { MsgWaitForMultipleObjectsEx(0, std::ptr::null(), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
            self.pump();
            raw = wndproc::take_events();
        }
        let mut out = Vec::new();
        for r in raw {
            if self.trace {
                eprintln!("[windows] {r:?}");
            }
            self.translate(r, &mut out);
        }
        out
    }
}
