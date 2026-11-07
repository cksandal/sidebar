//! X11 backend (x11rb, pure Rust, no Xlib/xcb C libraries).
//!
//! Uses only core protocol + EWMH/ICCCM hints + RandR, so it works with any
//! EWMH-compliant window manager without WM-specific code:
//! - Docks: managed windows with `_NET_WM_WINDOW_TYPE_DOCK`, sticky/above
//!   state, and `_NET_WM_STRUT_PARTIAL` (+ legacy `_NET_WM_STRUT`) to
//!   reserve space.
//! - Floating widgets: override-redirect windows when kept on top. The WM
//!   leaves them alone, so they go exactly where the shell puts them
//!   (essential while dragging) and show on every workspace. When not on
//!   top they are undecorated managed dock-type windows (no strut), since
//!   the WM cannot stack override-redirect windows under others.
//! - Stacking: `_NET_WM_STATE_ABOVE` when on top, else `_BELOW`, which puts
//!   docks under ordinary windows.
//! - Windows (settings): ordinary managed top-levels the WM decorates and
//!   focuses; key presses are mapped with the core keyboard mapping
//!   (keysym columns for Shift / Caps Lock), enough for Latin text.
//! - Input: pointer events carry root coordinates, so global positions are
//!   exact even while the window under the pointer is moving.
//! - Monitors: RandR 1.5 `GetMonitors` (falls back to the whole screen);
//!   RandR change notifications become `MonitorsChanged`.
//! - Pixels: `sidebar_render` output uploaded with `PutImage`, split into
//!   chunks that fit the maximum request size. A 32-bit ARGB visual is used
//!   when a compositing manager is running, so translucency works.
//!
//! Limitations / TODO:
//! - Scale is always 1.0 (logical == physical). Read `Xft.dpi` for HiDPI.
//! - Struts are screen-relative (an EWMH limitation): a dock on an inner
//!   monitor edge reserves that band across monitors it spans.
//! - MIT-SHM would avoid copying frames through the socket.
//! - Under XWayland the Wayland compositor ignores struts, so space
//!   reservation is reported as unsupported there.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::time::Duration;

use sidebar_core::{
    Edge, InputEvent, Key, MonitorId, MonitorInfo, Point, PointerButton, Rect, Scene, Size, SurfaceKind,
    SurfaceSpec, snap,
};
use sidebar_render::{AlphaMode, Pixmap};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::properties::{WmHints, WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ButtonPressEvent, ColormapAlloc, KeyButMask, KeyPressEvent, Keycode, Keysym, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
    CreateWindowAux, EventMask, Gcontext, ImageFormat, ImageOrder, PropMode, VisualClass, Visualid,
    Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::{Capabilities, Platform, PlatformError, PlatformEvent, Result, SurfaceId};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        UTF8_STRING,
        _NET_WM_NAME,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_UTILITY,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_STATE,
        _NET_WM_STATE_STICKY,
        _NET_WM_STATE_ABOVE,
        _NET_WM_STATE_BELOW,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_DESKTOP,
        _NET_WM_STRUT,
        _NET_WM_STRUT_PARTIAL,
    }
}

const WINDOW_NAME: &[u8] = b"sidebar";
const WINDOW_CLASS: &[u8] = b"sidebar\0Sidebar\0";
const ALL_DESKTOPS: u32 = 0xFFFF_FFFF;

struct VisualChoice {
    id: Visualid,
    depth: u8,
    colormap: u32,
    /// 32-bit visual with a real alpha channel (premultiplied ARGB).
    argb: bool,
}

struct Frame {
    width: u16,
    height: u16,
    /// Pixels already converted to the server's image format.
    data: Vec<u8>,
}

struct Surface {
    gc: Gcontext,
    spec: SurfaceSpec,
    size: (u16, u16),
    /// Last presented frame, kept to repaint on `Expose`.
    frame: Option<Frame>,
}

pub struct X11Platform {
    conn: RustConnection,
    root: Window,
    root_size: (u16, u16),
    atoms: Atoms,
    visual: VisualChoice,
    lsb_first: bool,
    randr: bool,
    reserve_space: bool,
    monitors: Vec<MonitorInfo>,
    surfaces: HashMap<Window, Surface>,
    keymap: Keymap,
}

/// The core protocol keyboard mapping: `per` keysyms for each keycode
/// from `min`.
#[derive(Default)]
struct Keymap {
    min: Keycode,
    per: usize,
    keysyms: Vec<Keysym>,
}

impl Keymap {
    fn load(conn: &RustConnection) -> Result<Self> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn.get_keyboard_mapping(min, max - min + 1).map_err(be)?.reply().map_err(be)?;
        Ok(Self { min, per: reply.keysyms_per_keycode.into(), keysyms: reply.keysyms })
    }

    /// The keysym for `code` with the given modifier state: column 1 with
    /// Shift (when set), letters flipped by Caps Lock.
    fn keysym(&self, code: Keycode, state: KeyButMask) -> Keysym {
        let Some(base) = usize::from(code).checked_sub(self.min.into()).map(|i| i * self.per) else { return 0 };
        let column = |i: usize| if i < self.per { self.keysyms.get(base + i).copied().unwrap_or(0) } else { 0 };
        let (lower, upper) = (column(0), column(1));
        let shift = state.contains(KeyButMask::SHIFT);
        let caps = state.contains(KeyButMask::LOCK);
        let letter = |k: Keysym| char::from_u32(k).is_some_and(|c| c.is_ascii_alphabetic());
        // Single-column letters ('a' only) get their uppercase implied.
        let upper = if upper == 0 && letter(lower) { lower & !0x20 } else if upper == 0 { lower } else { upper };
        let shifted = if letter(lower) { shift != caps } else { shift };
        if shifted { upper } else { lower }
    }
}

/// Maps a keysym to a [`Key`]; Ctrl/Alt chords are not text.
fn key_of(keysym: Keysym, state: KeyButMask) -> Option<Key> {
    let chord = state.intersects(KeyButMask::CONTROL | KeyButMask::MOD1);
    Some(match keysym {
        0xff08 => Key::Backspace,
        0xff09 if state.contains(KeyButMask::SHIFT) => Key::BackTab,
        0xff09 => Key::Tab,
        0xfe20 => Key::BackTab,
        0xff0d | 0xff8d => Key::Enter,
        0xff1b => Key::Escape,
        0xffff | 0xff9f => Key::Delete,
        0xff50 | 0xff95 => Key::Home,
        0xff57 | 0xff9c => Key::End,
        0xff51 | 0xff96 => Key::Left,
        0xff52 | 0xff97 => Key::Up,
        0xff53 | 0xff98 => Key::Right,
        0xff54 | 0xff99 => Key::Down,
        _ if chord => return None,
        // Keypad digits and operators.
        0xffb0..=0xffb9 => Key::Char(char::from(b'0' + (keysym - 0xffb0) as u8)),
        0xffaa..=0xffaf => Key::Char(char::from((keysym - 0xff80) as u8)),
        0xff80 => Key::Char(' '),
        // Latin-1 keysyms are their code points; 0x01xxxxxx is Unicode.
        0x20..=0x7e | 0xa0..=0xff => Key::Char(char::from_u32(keysym)?),
        0x0100_0000..=0x0110_ffff => Key::Char(char::from_u32(keysym - 0x0100_0000)?),
        _ => return None,
    })
}

fn be(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Backend(e.to_string())
}

fn window_of(surface: SurfaceId) -> Window {
    surface.0 as Window
}

impl X11Platform {
    pub fn connect() -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| PlatformError::Unsupported(format!("cannot connect to X server: {e}")))?;

        let setup = conn.setup();
        let screen = &setup.roots[screen_num];
        let root = screen.root;
        let root_size = (screen.width_in_pixels, screen.height_in_pixels);
        let lsb_first = setup.image_byte_order == ImageOrder::LSB_FIRST;

        let atoms = Atoms::new(&conn).map_err(be)?.reply().map_err(be)?;
        let compositor = has_compositor(&conn, screen_num)?;
        let visual = choose_visual(&conn, screen_num, compositor)?;
        let bpp = setup.pixmap_formats.iter().find(|f| f.depth == visual.depth).map(|f| f.bits_per_pixel);
        if bpp != Some(32) {
            return Err(PlatformError::Unsupported(format!(
                "depth {} uses {bpp:?} bits per pixel; only 32 is supported",
                visual.depth
            )));
        }

        // Fails with UnsupportedExtension when RandR is absent.
        let randr = conn
            .randr_query_version(1, 5)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|v| v.major_version > 1 || v.minor_version >= 5);
        if randr {
            let mask = randr::NotifyMask::SCREEN_CHANGE
                | randr::NotifyMask::CRTC_CHANGE
                | randr::NotifyMask::OUTPUT_CHANGE;
            conn.randr_select_input(root, mask).map_err(be)?;
        }

        let xwayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
        if xwayland {
            eprintln!("[x11] running under XWayland: struts are ignored by the compositor, space will not be reserved");
        }

        let mut platform = Self {
            conn,
            root,
            root_size,
            atoms,
            visual,
            lsb_first,
            randr,
            reserve_space: !xwayland,
            monitors: Vec::new(),
            surfaces: HashMap::new(),
            keymap: Keymap::default(),
        };
        platform.keymap = Keymap::load(&platform.conn)?;
        platform.refresh_monitors()?;
        Ok(platform)
    }

    fn refresh_monitors(&mut self) -> Result<()> {
        let geometry = self.conn.get_geometry(self.root).map_err(be)?.reply().map_err(be)?;
        self.root_size = (geometry.width, geometry.height);

        let mut monitors = Vec::new();
        if self.randr {
            let reply = self.conn.randr_get_monitors(self.root, true).map_err(be)?.reply().map_err(be)?;
            for m in reply.monitors {
                let name = self
                    .conn
                    .get_atom_name(m.name)
                    .map_err(be)?
                    .reply()
                    .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                    .unwrap_or_default();
                let bounds = Rect::new(m.x.into(), m.y.into(), m.width.into(), m.height.into());
                monitors.push(MonitorInfo {
                    // The name atom is stable across reconfiguration.
                    id: MonitorId(m.name),
                    name,
                    bounds,
                    work_area: bounds,
                    scale: 1.0,
                    primary: m.primary,
                });
            }
        }
        if monitors.is_empty() {
            let bounds = Rect::new(0.0, 0.0, self.root_size.0.into(), self.root_size.1.into());
            monitors.push(MonitorInfo {
                id: MonitorId(0),
                name: "screen".into(),
                bounds,
                work_area: bounds,
                scale: 1.0,
                primary: true,
            });
        }
        self.monitors = monitors;
        Ok(())
    }

    fn monitor(&self, id: MonitorId) -> Result<&MonitorInfo> {
        self.monitors
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| PlatformError::Backend(format!("unknown monitor {}", id.0)))
    }

    fn scale_of(&self, window: Window) -> f32 {
        self.surfaces
            .get(&window)
            .and_then(|s| self.monitor(s.spec.monitor).ok())
            .map_or(1.0, |m| m.scale)
    }

    /// Physical, root-relative rect for a spec. With scale fixed at 1.0,
    /// global logical coordinates are already physical.
    fn surface_rect(&self, spec: &SurfaceSpec) -> Result<(i32, i32, u32, u32)> {
        let monitor = self.monitor(spec.monitor)?;
        let r = match spec.kind {
            SurfaceKind::Dock { edge, thickness, .. } => snap::dock_rect(monitor.bounds, edge, thickness),
            SurfaceKind::Floating { rect } => rect,
            SurfaceKind::Window { .. } => {
                let area = monitor.work_area;
                let size = spec.size;
                Rect::new(area.x + (area.width - size.width) / 2.0, area.y + (area.height - size.height) / 2.0, size.width, size.height)
            }
        };
        Ok((
            r.x.round() as i32,
            r.y.round() as i32,
            r.width.round().max(1.0) as u32,
            r.height.round().max(1.0) as u32,
        ))
    }

    /// Applies geometry, size hints, and struts for `spec` to `window`.
    fn apply(&self, window: Window, spec: &SurfaceSpec) -> Result<(i32, i32, u32, u32)> {
        let (x, y, w, h) = self.surface_rect(spec)?;
        let fixed = matches!(spec.kind, SurfaceKind::Dock { .. } | SurfaceKind::Window { .. });

        let mut hints = WmSizeHints::new();
        hints.position = Some((WmSizeHintsSpecification::ProgramSpecified, x, y));
        hints.size = Some((WmSizeHintsSpecification::ProgramSpecified, w as i32, h as i32));
        if fixed {
            hints.min_size = Some((w as i32, h as i32));
            hints.max_size = Some((w as i32, h as i32));
        }
        hints.set_normal_hints(&self.conn, window).map_err(be)?;
        self.conn
            .configure_window(window, &ConfigureWindowAux::new().x(x).y(y).width(w).height(h))
            .map_err(be)?;

        match spec.kind {
            SurfaceKind::Dock { edge, reserve_space: true, .. } if self.reserve_space => {
                let partial = strut_partial(edge, (x, y, w, h), self.root_size);
                let atoms = &self.atoms;
                self.conn
                    .change_property32(PropMode::REPLACE, window, atoms._NET_WM_STRUT_PARTIAL, AtomEnum::CARDINAL, &partial)
                    .map_err(be)?;
                self.conn
                    .change_property32(PropMode::REPLACE, window, atoms._NET_WM_STRUT, AtomEnum::CARDINAL, &partial[..4])
                    .map_err(be)?;
            }
            _ => {
                self.conn.delete_property(window, self.atoms._NET_WM_STRUT_PARTIAL).map_err(be)?;
                self.conn.delete_property(window, self.atoms._NET_WM_STRUT).map_err(be)?;
            }
        }
        Ok((x, y, w, h))
    }

    /// Sets the EWMH/ICCCM properties that make a window a dock/utility,
    /// or an ordinary focusable window.
    fn set_window_properties(&self, window: Window, spec: &SurfaceSpec) -> Result<()> {
        let kind = &spec.kind;
        let atoms = &self.atoms;
        let c = &self.conn;
        let replace = PropMode::REPLACE;
        if let SurfaceKind::Window { title } = kind {
            let title = title.as_bytes();
            c.change_property8(replace, window, AtomEnum::WM_NAME, AtomEnum::STRING, title).map_err(be)?;
            c.change_property8(replace, window, atoms._NET_WM_NAME, atoms.UTF8_STRING, title).map_err(be)?;
            c.change_property8(replace, window, AtomEnum::WM_CLASS, AtomEnum::STRING, WINDOW_CLASS).map_err(be)?;
            c.change_property32(replace, window, atoms.WM_PROTOCOLS, AtomEnum::ATOM, &[atoms.WM_DELETE_WINDOW])
                .map_err(be)?;
            c.change_property32(replace, window, atoms._NET_WM_WINDOW_TYPE, AtomEnum::ATOM, &[atoms._NET_WM_WINDOW_TYPE_NORMAL])
                .map_err(be)?;
            // ICCCM: the client wants the WM to give it keyboard focus.
            let mut hints = WmHints::new();
            hints.input = Some(true);
            hints.set(c, window).map_err(be)?;
            return Ok(());
        }
        let window_type = match kind {
            SurfaceKind::Floating { .. } if spec.above => atoms._NET_WM_WINDOW_TYPE_UTILITY,
            _ => atoms._NET_WM_WINDOW_TYPE_DOCK,
        };
        let state = [
            atoms._NET_WM_STATE_STICKY,
            if spec.above { atoms._NET_WM_STATE_ABOVE } else { atoms._NET_WM_STATE_BELOW },
            atoms._NET_WM_STATE_SKIP_TASKBAR,
            atoms._NET_WM_STATE_SKIP_PAGER,
        ];
        c.change_property8(replace, window, AtomEnum::WM_NAME, AtomEnum::STRING, WINDOW_NAME).map_err(be)?;
        c.change_property8(replace, window, atoms._NET_WM_NAME, atoms.UTF8_STRING, WINDOW_NAME).map_err(be)?;
        c.change_property8(replace, window, AtomEnum::WM_CLASS, AtomEnum::STRING, WINDOW_CLASS).map_err(be)?;
        c.change_property32(replace, window, atoms.WM_PROTOCOLS, AtomEnum::ATOM, &[atoms.WM_DELETE_WINDOW])
            .map_err(be)?;
        c.change_property32(replace, window, atoms._NET_WM_WINDOW_TYPE, AtomEnum::ATOM, &[window_type])
            .map_err(be)?;
        c.change_property32(replace, window, atoms._NET_WM_STATE, AtomEnum::ATOM, &state).map_err(be)?;
        c.change_property32(replace, window, atoms._NET_WM_DESKTOP, AtomEnum::CARDINAL, &[ALL_DESKTOPS])
            .map_err(be)?;
        Ok(())
    }

    /// Uploads the surface's last frame, split to fit the max request size.
    fn blit(&self, window: Window) -> Result<()> {
        let Some(surface) = self.surfaces.get(&window) else { return Ok(()) };
        let Some(frame) = &surface.frame else { return Ok(()) };
        let stride = frame.width as usize * 4;
        let height = frame.height as usize;
        if stride == 0 || height == 0 {
            return Ok(());
        }
        // Leave room for the PutImage request header.
        let budget = self.conn.maximum_request_bytes().saturating_sub(64);
        let rows = (budget / stride).clamp(1, height);
        for y in (0..height).step_by(rows) {
            let n = rows.min(height - y);
            self.conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    window,
                    surface.gc,
                    frame.width,
                    n as u16,
                    0,
                    y as i16,
                    0,
                    self.visual.depth,
                    &frame.data[y * stride..(y + n) * stride],
                )
                .map_err(be)?;
        }
        Ok(())
    }

    fn reapply_all(&self) {
        for (&window, surface) in &self.surfaces {
            // Surfaces on vanished monitors are re-homed by the shell, which
            // then reconfigures them.
            let _ = self.apply(window, &surface.spec);
        }
    }

    /// Handles everything already queued. Returns whether any event was seen.
    fn drain(&mut self, out: &mut Vec<PlatformEvent>) -> Result<bool> {
        self.conn.flush().map_err(be)?;
        let mut handled = false;
        while let Some(event) = self.conn.poll_for_event().map_err(be)? {
            handled = true;
            self.translate(event, out)?;
        }
        self.conn.flush().map_err(be)?;
        Ok(handled)
    }

    fn translate(&mut self, event: Event, out: &mut Vec<PlatformEvent>) -> Result<()> {
        match event {
            Event::Expose(e) if e.count == 0 => self.blit(e.window)?,
            Event::ConfigureNotify(e) => {
                let scale = self.scale_of(e.window);
                if let Some(surface) = self.surfaces.get_mut(&e.window)
                    && surface.size != (e.width, e.height)
                {
                    surface.size = (e.width, e.height);
                    out.push(PlatformEvent::SurfaceResized {
                        surface: SurfaceId(e.window.into()),
                        size: Size::new(f32::from(e.width) / scale, f32::from(e.height) / scale),
                    });
                }
            }
            Event::ButtonPress(e) => self.button(e, true, out),
            Event::ButtonRelease(e) => self.button(e, false, out),
            Event::MotionNotify(e) => {
                let pos = self.local_point(e.event, e.event_x, e.event_y);
                let global = self.local_point(e.event, e.root_x, e.root_y);
                self.input(e.event, InputEvent::PointerMoved { pos }, Some(global), out);
            }
            Event::LeaveNotify(e) => self.input(e.event, InputEvent::PointerLeft, None, out),
            Event::KeyPress(e) => self.key(e, out),
            Event::MappingNotify(_) => self.keymap = Keymap::load(&self.conn)?,
            Event::ClientMessage(e)
                if e.format == 32 && e.data.as_data32()[0] == self.atoms.WM_DELETE_WINDOW =>
            {
                out.push(PlatformEvent::CloseRequested { surface: SurfaceId(e.window.into()) });
            }
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                if !out.contains(&PlatformEvent::MonitorsChanged) {
                    self.refresh_monitors()?;
                    self.reapply_all();
                    out.push(PlatformEvent::MonitorsChanged);
                }
            }
            Event::Error(e) => eprintln!("[x11] protocol error: {e:?}"),
            _ => {}
        }
        Ok(())
    }

    /// Converts physical coordinates to logical ones using the scale of
    /// `window`'s monitor.
    fn local_point(&self, window: Window, x: i16, y: i16) -> Point {
        let scale = self.scale_of(window);
        Point::new(f32::from(x) / scale, f32::from(y) / scale)
    }

    fn input(&self, window: Window, event: InputEvent, global: Option<Point>, out: &mut Vec<PlatformEvent>) {
        if self.surfaces.contains_key(&window) {
            out.push(PlatformEvent::Input { surface: SurfaceId(window.into()), event, global });
        }
    }

    fn key(&self, e: KeyPressEvent, out: &mut Vec<PlatformEvent>) {
        if let Some(key) = key_of(self.keymap.keysym(e.detail, e.state), e.state) {
            self.input(e.event, InputEvent::Key(key), None, out);
        }
    }

    fn button(&self, e: ButtonPressEvent, pressed: bool, out: &mut Vec<PlatformEvent>) {
        let pos = self.local_point(e.event, e.event_x, e.event_y);
        let global = self.local_point(e.event, e.root_x, e.root_y);
        let button = |button| InputEvent::PointerButton { pos, button, pressed };
        let scroll = |dx, dy| InputEvent::Scroll { pos, dx, dy };
        let event = match e.detail {
            // Buttons 4-7 are wheel clicks: one press + release per notch.
            4..=7 if !pressed => return,
            1 => button(PointerButton::Primary),
            2 => button(PointerButton::Middle),
            3 => button(PointerButton::Secondary),
            4 => scroll(0.0, -1.0),
            5 => scroll(0.0, 1.0),
            6 => scroll(-1.0, 0.0),
            7 => scroll(1.0, 0.0),
            n => button(PointerButton::Other(n.into())),
        };
        self.input(e.event, event, Some(global), out);
    }
}

fn has_compositor(conn: &RustConnection, screen_num: usize) -> Result<bool> {
    let name = format!("_NET_WM_CM_S{screen_num}");
    let atom = conn.intern_atom(false, name.as_bytes()).map_err(be)?.reply().map_err(be)?.atom;
    let owner = conn.get_selection_owner(atom).map_err(be)?.reply().map_err(be)?.owner;
    Ok(owner != x11rb::NONE)
}

fn choose_visual(conn: &RustConnection, screen_num: usize, want_argb: bool) -> Result<VisualChoice> {
    let screen = &conn.setup().roots[screen_num];
    if want_argb {
        let argb = screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| &d.visuals)
            .find(|v| {
                v.class == VisualClass::TRUE_COLOR
                    && (v.red_mask, v.green_mask, v.blue_mask) == (0xff_0000, 0xff00, 0xff)
            });
        if let Some(v) = argb {
            let colormap = conn.generate_id().map_err(be)?;
            conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, v.visual_id).map_err(be)?;
            return Ok(VisualChoice { id: v.visual_id, depth: 32, colormap, argb: true });
        }
    }
    Ok(VisualChoice { id: screen.root_visual, depth: screen.root_depth, colormap: screen.default_colormap, argb: false })
}

/// `_NET_WM_STRUT_PARTIAL` for a dock at `(x, y, w, h)` on a root window of
/// `root` size: left, right, top, bottom, then start/end pairs for each.
fn strut_partial(edge: Edge, (x, y, w, h): (i32, i32, u32, u32), root: (u16, u16)) -> [u32; 12] {
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x0 + w).saturating_sub(1);
    let y1 = (y0 + h).saturating_sub(1);
    let mut s = [0u32; 12];
    match edge {
        Edge::Left => (s[0], s[4], s[5]) = (x1 + 1, y0, y1),
        Edge::Right => (s[1], s[6], s[7]) = (u32::from(root.0).saturating_sub(x0), y0, y1),
        Edge::Top => (s[2], s[8], s[9]) = (y1 + 1, x0, x1),
        Edge::Bottom => (s[3], s[10], s[11]) = (u32::from(root.1).saturating_sub(y0), x0, x1),
    }
    s
}

/// Converts to the server's 32bpp layout: B,G,R,A in memory on
/// LSB-first servers, reversed on (rare) MSB-first ones.
fn to_x11_pixels(pixmap: &Pixmap, argb: bool, lsb_first: bool) -> Vec<u8> {
    let alpha = if argb { AlphaMode::Premultiplied } else { AlphaMode::Opaque };
    let mut out = vec![0; pixmap.data.len()];
    pixmap.write_bgra(&mut out, alpha);
    if !lsb_first {
        out.chunks_exact_mut(4).for_each(<[u8]>::reverse);
    }
    out
}

impl Platform for X11Platform {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { reserve_space: self.reserve_space, free_positioning: true, global_pointer: true, transparency: self.visual.argb }
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.clone()
    }

    fn create_surface(&mut self, spec: &SurfaceSpec) -> Result<SurfaceId> {
        let (x, y, w, h) = self.surface_rect(spec)?;
        let window = self.conn.generate_id().map_err(be)?;
        let events = EventMask::EXPOSURE
            | EventMask::STRUCTURE_NOTIFY
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::LEAVE_WINDOW;
        let events = if matches!(spec.kind, SurfaceKind::Window { .. }) { events | EventMask::KEY_PRESS } else { events };
        // border_pixel + colormap are required for a non-default visual.
        let floating = matches!(spec.kind, SurfaceKind::Floating { .. }) && spec.above;
        let aux = CreateWindowAux::new()
            .background_pixel(0)
            .border_pixel(0)
            .colormap(self.visual.colormap)
            .override_redirect(u32::from(floating))
            .event_mask(events);
        self.conn
            .create_window(
                self.visual.depth,
                window,
                self.root,
                x as i16,
                y as i16,
                w as u16,
                h as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                self.visual.id,
                &aux,
            )
            .map_err(be)?;
        let gc = self.conn.generate_id().map_err(be)?;
        self.conn.create_gc(gc, window, &CreateGCAux::new()).map_err(be)?;

        self.set_window_properties(window, spec)?;
        self.apply(window, spec)?;
        self.conn.map_window(window).map_err(be)?;
        // Some WMs ignore pre-map positions; ask again once mapped.
        self.conn.configure_window(window, &ConfigureWindowAux::new().x(x).y(y)).map_err(be)?;
        self.conn.flush().map_err(be)?;

        self.surfaces.insert(window, Surface { gc, spec: spec.clone(), size: (w as u16, h as u16), frame: None });
        Ok(SurfaceId(window.into()))
    }

    fn configure_surface(&mut self, surface: SurfaceId, spec: &SurfaceSpec) -> Result<()> {
        let window = window_of(surface);
        if !self.surfaces.contains_key(&window) {
            return Err(PlatformError::Backend(format!("unknown surface {surface}")));
        }
        let (_, _, w, h) = self.apply(window, spec)?;
        self.conn.flush().map_err(be)?;
        if let Some(s) = self.surfaces.get_mut(&window) {
            s.spec = spec.clone();
            s.size = (w as u16, h as u16);
        }
        Ok(())
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        let window = window_of(surface);
        if let Some(s) = self.surfaces.remove(&window) {
            let _ = self.conn.free_gc(s.gc);
            let _ = self.conn.destroy_window(window);
            let _ = self.conn.flush();
        }
    }

    fn present(&mut self, surface: SurfaceId, scene: &Scene) -> Result<()> {
        let window = window_of(surface);
        let pixmap = sidebar_render::rasterize(scene, self.scale_of(window));
        let data = to_x11_pixels(&pixmap, self.visual.argb, self.lsb_first);
        let s = self
            .surfaces
            .get_mut(&window)
            .ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
        s.frame = Some(Frame { width: pixmap.width as u16, height: pixmap.height as u16, data });
        self.blit(window)?;
        self.conn.flush().map_err(be)
    }

    fn poll_events(&mut self, timeout: Duration) -> Vec<PlatformEvent> {
        let mut out = Vec::new();
        let result = self.drain(&mut out).and_then(|handled| {
            if !handled {
                super::wait_readable(self.conn.stream().as_raw_fd(), timeout);
                self.drain(&mut out)?;
            }
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("[x11] connection lost: {e}");
            out.push(PlatformEvent::Quit);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strut_right_dock() {
        let s = strut_partial(Edge::Right, (1760, 0, 160, 1080), (1920, 1080));
        assert_eq!(s[1], 160);
        assert_eq!((s[6], s[7]), (0, 1079));
        assert_eq!(s.iter().filter(|&&v| v != 0).count(), 2);
    }

    #[test]
    fn strut_top_dock_on_second_monitor() {
        let s = strut_partial(Edge::Top, (1920, 0, 2560, 32), (4480, 1440));
        assert_eq!(s[2], 32);
        assert_eq!((s[8], s[9]), (1920, 4479));
    }

    #[test]
    fn strut_bottom_dock() {
        let s = strut_partial(Edge::Bottom, (0, 1048, 1920, 32), (1920, 1080));
        assert_eq!(s[3], 32);
        assert_eq!((s[10], s[11]), (0, 1919));
    }

    #[test]
    fn maps_keys() {
        // Keycode 10: a/A; 11: 1/!; 12: Tab.
        let keymap = Keymap { min: 10, per: 2, keysyms: vec![0x61, 0x41, 0x31, 0x21, 0xff09, 0xfe20] };
        let none = KeyButMask::default();
        let key = |code, state| key_of(keymap.keysym(code, state), state);
        assert_eq!(key(10, none), Some(Key::Char('a')));
        assert_eq!(key(10, KeyButMask::SHIFT), Some(Key::Char('A')));
        assert_eq!(key(10, KeyButMask::LOCK), Some(Key::Char('A')));
        assert_eq!(key(10, KeyButMask::LOCK | KeyButMask::SHIFT), Some(Key::Char('a')));
        assert_eq!(key(11, KeyButMask::LOCK), Some(Key::Char('1')), "Caps Lock only affects letters");
        assert_eq!(key(11, KeyButMask::SHIFT), Some(Key::Char('!')));
        assert_eq!(key(10, KeyButMask::CONTROL), None);
        assert_eq!(key(12, KeyButMask::SHIFT), Some(Key::BackTab));
        assert_eq!(key(9, none), None, "below the first keycode");
        assert_eq!(key_of(0x0100_20ac, none), Some(Key::Char('€')));
    }

    #[test]
    fn msb_first_reverses_bytes() {
        let pixmap = Pixmap { width: 1, height: 1, data: vec![200, 100, 50, 255] };
        assert_eq!(to_x11_pixels(&pixmap, false, false), vec![255, 200, 100, 50]);
    }
}
