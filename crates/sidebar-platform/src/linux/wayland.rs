//! Wayland backend (smithay-client-toolkit + wlr-layer-shell).
//!
//! Uses only standard and widely implemented protocols, so no
//! compositor-specific code is needed:
//! - Docks: `zwlr_layer_shell_v1` surfaces on the `top` layer, anchored to
//!   their edge plus both adjacent edges, with `exclusive_zone = thickness`
//!   to reserve space. Supported by wlroots compositors (Sway, Hyprland,
//!   river, labwc, ...), KDE/KWin, COSMIC, niri. Not GNOME/Mutter:
//!   `connect` returns `Unsupported` there and `linux::connect` falls back
//!   to X11 via XWayland.
//! - Floating widgets: layer surfaces anchored top-left of their output and
//!   offset with margins. That is the only client-side positioning Wayland
//!   allows, and it is limited to one output.
//! - Monitors: `wl_output` + `zxdg_output_manager_v1` (logical geometry).
//! - Pixels: `sidebar_render` output copied into `wl_shm` ARGB8888 buffers.
//! - Windows (the settings window): `xdg_toplevel`, with server-side
//!   decorations where offered. A fixed size, so tiling compositors float
//!   it.
//! - Input: `wl_pointer` events are surface-local. Global positions are
//!   derived from where the surface was placed, so `global_pointer` is
//!   false. That holds during a drag because the shell never moves the
//!   surface holding the grab. Docks are assumed to start at their output's
//!   corner, which is off by the size of any other panel on an adjacent
//!   edge. `wl_keyboard` keys go through libxkbcommon (see `xkb`), with
//!   key repeat done here, as Wayland expects of clients.
//! - Scale: `wp_fractional_scale_v1` with `wp_viewporter` where offered
//!   (render at 1.25x, say, and have the compositor show it at logical
//!   size), else the integer `wl_surface` buffer scale.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use sidebar_core::{
    Edge, InputEvent, MonitorId, MonitorInfo, Point, PointerButton, Rect, Scene, Size, SurfaceKind,
    SurfaceSpec, snap,
};
use sidebar_render::AlphaMode;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputInfo, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, Proxy, QueueHandle, WEnum};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{AxisScroll, PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};

use super::xkb::Xkb;
use crate::{Capabilities, Platform, PlatformError, PlatformEvent, Result, SurfaceId};

const NAMESPACE: &str = "sidebar";
/// Initial shm pool size; the pool grows on demand.
const INITIAL_POOL_BYTES: usize = 4 * 1024 * 1024;

// Linux input event codes (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

fn be(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Backend(e.to_string())
}

/// What a surface is to the compositor.
enum Role {
    Layer(LayerSurface),
    Window(Window),
}

impl Role {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Role::Layer(l) => l.wl_surface(),
            Role::Window(w) => w.wl_surface(),
        }
    }

    fn commit(&self) {
        self.wl_surface().commit();
    }
}

struct Surface {
    role: Role,
    spec: SurfaceSpec,
    /// False until the compositor's first configure; no buffer may be
    /// attached before that.
    configured: bool,
    /// The compositor closed the layer surface (e.g. its output went away).
    /// It is recreated on the next `configure_surface`.
    closed: bool,
    /// Logical size from the last configure.
    size: (u32, u32),
    scale: i32,
    /// The compositor's preferred fractional scale, when it offers one
    /// (and a viewport to show the larger buffer at logical size).
    fraction: Option<f32>,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    /// Last presented scene, redrawn on configure and scale changes.
    scene: Option<Scene>,
    /// Keeps the attached buffer alive until replaced.
    buffer: Option<Buffer>,
    /// Global logical position of the surface's top-left corner.
    origin: Point,
}

/// Everything protocol event handlers need to touch.
struct State {
    registry: RegistryState,
    outputs: OutputState,
    seat: SeatState,
    shm: Shm,
    pool: SlotPool,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    xkb: Option<Xkb>,
    /// The surface with keyboard focus.
    focus: Option<SurfaceId>,
    /// Keys per second and delay before repeating, from the compositor.
    repeat_rate: (u32, Duration),
    /// The held key being repeated: its code, key, and next repeat.
    repeat: Option<(u32, sidebar_core::Key, Instant)>,
    surfaces: HashMap<SurfaceId, Surface>,
    events: Vec<PlatformEvent>,
    /// SIDEBAR_TRACE=1: log keyboard events.
    trace: bool,
}

impl State {
    fn surface_id(&self, wl_surface: &wl_surface::WlSurface) -> Option<SurfaceId> {
        self.surfaces.iter().find(|(_, s)| s.role.wl_surface() == wl_surface).map(|(id, _)| *id)
    }

    fn monitors_changed(&mut self) {
        if !self.events.contains(&PlatformEvent::MonitorsChanged) {
            self.events.push(PlatformEvent::MonitorsChanged);
        }
    }

    /// Rasterizes the surface's last scene at its configured size and
    /// commits it. No-op until the surface is configured.
    fn draw(&mut self, id: SurfaceId) -> Result<()> {
        let Some(s) = self.surfaces.get_mut(&id) else { return Ok(()) };
        if !s.configured || s.closed || s.size.0 == 0 || s.size.1 == 0 {
            return Ok(());
        }
        let Some(scene) = &mut s.scene else { return Ok(()) };
        // The compositor's size wins over the size the shell laid out for.
        scene.size = Size::new(s.size.0 as f32, s.size.1 as f32);
        let fractional = s.fraction.filter(|_| s.viewport.is_some());
        let scale = fractional.unwrap_or(s.scale.max(1) as f32);
        let pixmap = sidebar_render::rasterize(scene, scale);
        let (w, h) = (pixmap.width as i32, pixmap.height as i32);

        let (buffer, canvas) =
            self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).map_err(be)?;
        pixmap.write_bgra(canvas, AlphaMode::Premultiplied);

        let wl_surface = s.role.wl_surface();
        match (&s.viewport, fractional) {
            // The buffer is larger than the logical size; the viewport maps
            // it back.
            (Some(viewport), Some(_)) => {
                wl_surface.set_buffer_scale(1);
                viewport.set_destination(s.size.0 as i32, s.size.1 as i32);
            }
            _ => wl_surface.set_buffer_scale(scale as i32),
        }
        wl_surface.damage_buffer(0, 0, w, h);
        buffer.attach_to(wl_surface).map_err(be)?;
        s.role.commit();
        s.buffer = Some(buffer);
        Ok(())
    }

    fn redraw(&mut self, id: SurfaceId) {
        if let Err(e) = self.draw(id) {
            eprintln!("[wayland] draw {id}: {e}");
        }
    }
}

pub struct WaylandPlatform {
    _conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    compositor: CompositorState,
    layer_shell: LayerShell,
    xdg_shell: Option<XdgShell>,
    fractional_scale: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    state: State,
    next_id: u64,
}

impl WaylandPlatform {
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env()
            .map_err(|e| PlatformError::Unsupported(format!("cannot connect to Wayland display: {e}")))?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(be)?;
        let qh = queue.handle();

        let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| {
            PlatformError::Unsupported("compositor does not support zwlr_layer_shell_v1".into())
        })?;
        let compositor = CompositorState::bind(&globals, &qh).map_err(be)?;
        let shm = Shm::bind(&globals, &qh).map_err(be)?;
        let pool = SlotPool::new(INITIAL_POOL_BYTES, &shm).map_err(be)?;
        // Optional: without xdg-shell there is no settings window, and
        // without these two, integer scaling.
        let xdg_shell = XdgShell::bind(&globals, &qh).ok();
        let fractional_scale = globals.bind(&qh, 1..=1, Unit).ok();
        let viewporter = globals.bind(&qh, 1..=1, Unit).ok();

        let mut state = State {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seat: SeatState::new(&globals, &qh),
            shm,
            pool,
            pointer: None,
            keyboard: None,
            xkb: None,
            focus: None,
            repeat_rate: (25, Duration::from_millis(600)),
            repeat: None,
            surfaces: HashMap::new(),
            events: Vec::new(),
            trace: std::env::var_os("SIDEBAR_TRACE").is_some_and(|v| v == "1"),
        };
        // First roundtrip binds outputs (and xdg-output); second delivers
        // their geometry.
        queue.roundtrip(&mut state).map_err(be)?;
        queue.roundtrip(&mut state).map_err(be)?;
        state.events.clear();

        Ok(Self { _conn: conn, queue, qh, compositor, layer_shell, xdg_shell, fractional_scale, viewporter, state, next_id: 1 })
    }

    fn outputs(&self) -> Vec<(wl_output::WlOutput, OutputInfo)> {
        let outputs = &self.state.outputs;
        outputs.outputs().filter_map(|o| outputs.info(&o).map(|info| (o, info))).collect()
    }

    fn output_for(&self, monitor: MonitorId) -> Result<(wl_output::WlOutput, OutputInfo)> {
        self.outputs()
            .into_iter()
            .find(|(_, info)| MonitorId(info.id) == monitor)
            .ok_or_else(|| PlatformError::Backend(format!("unknown monitor {}", monitor.0)))
    }

    fn new_surface(&self, id: SurfaceId, spec: &SurfaceSpec) -> Result<Surface> {
        let (output, info) = self.output_for(spec.monitor)?;
        let bounds = monitor_info(&info, false).bounds;
        let wl_surface = self.compositor.create_surface(&self.qh);
        let fractional = self.fractional_scale.as_ref().map(|m| m.get_fractional_scale(&wl_surface, &self.qh, FractionalData(id)));
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&wl_surface, &self.qh, Unit));
        let (role, size) = match &spec.kind {
            SurfaceKind::Window { title } => {
                let xdg = self.xdg_shell.as_ref().ok_or(PlatformError::NotImplemented("windows without xdg-shell on Wayland"))?;
                let window = xdg.create_window(wl_surface, WindowDecorations::RequestServer, &self.qh);
                let size = (spec.size.width.round() as u32, spec.size.height.round() as u32);
                window.set_title(title.clone());
                window.set_app_id("sidebar");
                // A fixed size: tiling compositors float such windows.
                window.set_min_size(Some(size));
                window.set_max_size(Some(size));
                (Role::Window(window), size)
            }
            _ => {
                let layer =
                    self.layer_shell.create_layer_surface(&self.qh, wl_surface, layer_of(spec), Some(NAMESPACE), Some(&output));
                configure_layer(&layer, spec, bounds);
                (Role::Layer(layer), (0, 0))
            }
        };
        // Initial commit without a buffer; the compositor answers with a
        // configure carrying the real size.
        role.commit();
        Ok(Surface {
            role,
            spec: spec.clone(),
            configured: false,
            closed: false,
            size,
            scale: info.scale_factor.max(1),
            fraction: None,
            fractional,
            viewport,
            scene: None,
            buffer: None,
            origin: surface_origin(spec, bounds),
        })
    }

    /// Repeats the held key when it is due.
    fn repeat_keys(&mut self) {
        let Some((code, key, next)) = self.state.repeat else { return };
        let now = Instant::now();
        if now < next {
            return;
        }
        if let Some(surface) = self.state.focus {
            self.state.events.push(PlatformEvent::Input { surface, event: InputEvent::Key(key), global: None });
        }
        let interval = Duration::from_secs(1) / self.state.repeat_rate.0.max(1);
        self.state.repeat = Some((code, key, (next + interval).max(now)));
    }

    fn pump(&mut self, timeout: Duration) -> Result<()> {
        // Wake up in time for a key repeat.
        let timeout = match self.state.repeat {
            Some((_, _, next)) => timeout.min(next.saturating_duration_since(Instant::now())),
            None => timeout,
        };
        self.queue.dispatch_pending(&mut self.state).map_err(be)?;
        if self.state.events.is_empty() {
            self.queue.flush().map_err(be)?;
            // Dropping an unread guard cancels the read.
            if let Some(guard) = self.queue.prepare_read()
                && super::wait_readable(guard.connection_fd().as_raw_fd(), timeout)
            {
                guard.read().map_err(be)?;
            }
            self.queue.dispatch_pending(&mut self.state).map_err(be)?;
        }
        self.queue.flush().map_err(be)
    }
}

fn monitor_info(info: &OutputInfo, primary: bool) -> MonitorInfo {
    let scale = info.scale_factor.max(1);
    let mode = info.modes.iter().find(|m| m.current).map_or((0, 0), |m| m.dimensions);
    let (x, y) = info.logical_position.unwrap_or(info.location);
    let (w, h) = info.logical_size.unwrap_or((mode.0 / scale, mode.1 / scale));
    let bounds = Rect::new(x as f32, y as f32, w as f32, h as f32);
    MonitorInfo {
        id: MonitorId(info.id),
        name: info.name.clone().unwrap_or_else(|| format!("{} {}", info.make, info.model)),
        bounds,
        work_area: bounds,
        scale: scale as f32,
        primary,
    }
}

/// Where a surface for `spec` lands on an output with `monitor` bounds.
fn surface_origin(spec: &SurfaceSpec, monitor: Rect) -> Point {
    match spec.kind {
        SurfaceKind::Dock { edge, thickness, .. } => snap::dock_rect(monitor, edge, thickness).origin(),
        SurfaceKind::Floating { rect } => rect.origin(),
        // Placed by the compositor; only its own input is used.
        SurfaceKind::Window { .. } => monitor.origin(),
    }
}

/// Top: over ordinary windows. Bottom: under them, over the wallpaper.
fn layer_of(spec: &SurfaceSpec) -> Layer {
    if spec.above { Layer::Top } else { Layer::Bottom }
}

/// Translates a spec into layer-shell anchor, size, margins, and zone.
fn configure_layer(layer: &LayerSurface, spec: &SurfaceSpec, monitor: Rect) {
    match spec.kind {
        SurfaceKind::Dock { edge, thickness, reserve_space } => {
            let t = thickness.round().max(1.0) as u32;
            // Size 0 on an axis anchored at both ends = stretch to fill.
            let (anchor, (w, h)) = match edge {
                Edge::Left => (Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM, (t, 0)),
                Edge::Right => (Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM, (t, 0)),
                Edge::Top => (Anchor::TOP | Anchor::LEFT | Anchor::RIGHT, (0, t)),
                Edge::Bottom => (Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT, (0, t)),
            };
            layer.set_anchor(anchor);
            layer.set_size(w, h);
            layer.set_margin(0, 0, 0, 0);
            // 0 = don't reserve, but still avoid other panels' zones.
            layer.set_exclusive_zone(if reserve_space { t as i32 } else { 0 });
        }
        SurfaceKind::Floating { rect } => {
            layer.set_anchor(Anchor::TOP | Anchor::LEFT);
            layer.set_size(rect.width.round().max(1.0) as u32, rect.height.round().max(1.0) as u32);
            let top = (rect.y - monitor.y).round() as i32;
            let left = (rect.x - monitor.x).round() as i32;
            layer.set_margin(top, 0, 0, left);
            // -1 = position relative to the output edge, ignoring other panels.
            layer.set_exclusive_zone(-1);
        }
        // Windows are xdg toplevels, not layer surfaces.
        SurfaceKind::Window { .. } => {}
    }
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
}

fn axis_steps(axis: &AxisScroll) -> f32 {
    if axis.discrete != 0 {
        axis.discrete as f32
    } else if axis.value120 != 0 {
        axis.value120 as f32 / 120.0
    } else {
        // Touchpads: continuous pixels; ~10px per step.
        axis.absolute as f32 / 10.0
    }
}

impl Platform for WaylandPlatform {
    fn name(&self) -> &'static str {
        "wayland"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { reserve_space: true, free_positioning: true, global_pointer: false, transparency: true }
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        // Wayland has no "primary output"; treat the first one as primary.
        self.outputs().iter().enumerate().map(|(i, (_, info))| monitor_info(info, i == 0)).collect()
    }

    fn create_surface(&mut self, spec: &SurfaceSpec) -> Result<SurfaceId> {
        let id = SurfaceId(self.next_id);
        self.next_id += 1;
        let surface = self.new_surface(id, spec)?;
        self.state.surfaces.insert(id, surface);
        self.queue.flush().map_err(be)?;
        Ok(id)
    }

    fn configure_surface(&mut self, surface: SurfaceId, spec: &SurfaceSpec) -> Result<()> {
        let current = self
            .state
            .surfaces
            .get(&surface)
            .ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
        if matches!(current.role, Role::Window(_)) {
            // The compositor places and keeps windows.
            if let Some(s) = self.state.surfaces.get_mut(&surface) {
                s.spec = spec.clone();
            }
        } else if current.closed || current.spec.monitor != spec.monitor || current.spec.above != spec.above {
            // A layer surface is bound to one output (and, before layer-shell
            // v2, one layer) for life: recreate it.
            let mut fresh = self.new_surface(surface, spec)?;
            fresh.scene = current.scene.clone();
            self.state.surfaces.insert(surface, fresh);
        } else {
            let bounds = self.output_for(spec.monitor).map(|(_, info)| monitor_info(&info, false).bounds)?;
            if let Some(s) = self.state.surfaces.get_mut(&surface)
                && let Role::Layer(layer) = &s.role
            {
                configure_layer(layer, spec, bounds);
                layer.commit();
                s.origin = surface_origin(spec, bounds);
                s.spec = spec.clone();
            }
        }
        self.queue.flush().map_err(be)
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        // Dropping the LayerSurface or Window destroys the role objects.
        if let Some(s) = self.state.surfaces.remove(&surface) {
            if let Some(f) = s.fractional {
                f.destroy();
            }
            if let Some(v) = s.viewport {
                v.destroy();
            }
        }
        if self.state.focus == Some(surface) {
            self.state.focus = None;
            self.state.repeat = None;
        }
        let _ = self.queue.flush();
    }

    fn present(&mut self, surface: SurfaceId, scene: &Scene) -> Result<()> {
        let s = self
            .state
            .surfaces
            .get_mut(&surface)
            .ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
        s.scene = Some(scene.clone());
        self.state.draw(surface)?;
        self.queue.flush().map_err(be)
    }

    fn poll_events(&mut self, timeout: Duration) -> Vec<PlatformEvent> {
        if let Err(e) = self.pump(timeout) {
            eprintln!("[wayland] connection lost: {e}");
            self.state.events.push(PlatformEvent::Quit);
        }
        self.repeat_keys();
        std::mem::take(&mut self.state.events)
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if let Some(id) = self.surface_id(surface)
            && let Some(s) = self.surfaces.get_mut(&id)
        {
            s.scale = new_factor;
            self.redraw(id);
        }
    }

    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}

    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.monitors_changed();
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.monitors_changed();
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.monitors_changed();
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        if let Some(surface) = self.surface_id(window.wl_surface()) {
            self.events.push(PlatformEvent::CloseRequested { surface });
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, _: WindowConfigure, _serial: u32) {
        // The window keeps its own size; sctk acknowledges the configure.
        let Some(id) = self.surface_id(window.wl_surface()) else { return };
        if let Some(s) = self.surfaces.get_mut(&id) {
            s.configured = true;
        }
        self.redraw(id);
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(id) = self.surface_id(layer.wl_surface())
            && let Some(s) = self.surfaces.get_mut(&id)
        {
            s.closed = true;
            s.configured = false;
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(id) = self.surface_id(layer.wl_surface()) else { return };
        let Some(s) = self.surfaces.get_mut(&id) else { return };
        let (w, h) = configure.new_size;
        let resized = s.configured && s.size != (w, h);
        s.size = (w, h);
        s.configured = true;
        if resized {
            self.events.push(PlatformEvent::SurfaceResized { surface: id, size: Size::new(w as f32, h as f32) });
        }
        self.redraw(id);
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(e) => eprintln!("[wayland] no pointer: {e}"),
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(seat.get_keyboard(qh, KeyboardData));
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
            self.repeat = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(surface) = self.surface_id(&event.surface) else { continue };
            let pos = Point::new(event.position.0 as f32, event.position.1 as f32);
            let global = self.surfaces.get(&surface).map(|s| s.origin + pos);
            let button = |code| match code {
                BTN_LEFT => PointerButton::Primary,
                BTN_RIGHT => PointerButton::Secondary,
                BTN_MIDDLE => PointerButton::Middle,
                other => PointerButton::Other(other as u16),
            };
            let input = match &event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => InputEvent::PointerMoved { pos },
                PointerEventKind::Leave { .. } => InputEvent::PointerLeft,
                PointerEventKind::Press { button: code, .. } => {
                    InputEvent::PointerButton { pos, button: button(*code), pressed: true }
                }
                PointerEventKind::Release { button: code, .. } => {
                    InputEvent::PointerButton { pos, button: button(*code), pressed: false }
                }
                PointerEventKind::Axis { horizontal, vertical, .. } => {
                    let (dx, dy) = (axis_steps(horizontal), axis_steps(vertical));
                    if dx == 0.0 && dy == 0.0 {
                        continue;
                    }
                    InputEvent::Scroll { pos, dx, dy }
                }
            };
            self.events.push(PlatformEvent::Input { surface, event: input, global });
        }
    }
}

/// User data for protocol objects without events this backend needs.
struct Unit;

impl<I: Proxy> Dispatch2<I, State> for Unit {
    fn event(&self, _: &mut State, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<State>) {}
}

/// A surface's fractional scale object.
struct FractionalData(SurfaceId);

impl Dispatch2<wp_fractional_scale_v1::WpFractionalScaleV1, State> for FractionalData {
    fn event(
        &self,
        state: &mut State,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(s) = state.surfaces.get_mut(&self.0)
        {
            // In 120ths.
            s.fraction = Some(scale as f32 / 120.0);
            state.redraw(self.0);
        }
    }
}

struct KeyboardData;

/// The compositor's keymap, from the shared memory it sends.
fn read_keymap(fd: &OwnedFd, size: u32) -> Option<std::ffi::CString> {
    let len = size as usize;
    // SAFETY: a read-only private mapping of the fd the compositor sent,
    // copied out and unmapped before returning.
    unsafe {
        let map = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, fd.as_raw_fd(), 0);
        if map == libc::MAP_FAILED {
            return None;
        }
        let bytes = std::slice::from_raw_parts(map as *const u8, len);
        let text = bytes.split(|&b| b == 0).next().map(|t| t.to_vec());
        libc::munmap(map, len);
        std::ffi::CString::new(text?).ok()
    }
}

impl Dispatch2<wl_keyboard::WlKeyboard, State> for KeyboardData {
    fn event(&self, state: &mut State, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &Connection, _: &QueueHandle<State>) {
        use wl_keyboard::Event;
        if state.trace {
            eprintln!("[wayland] {event:?}");
        }
        match event {
            Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                state.xkb = read_keymap(&fd, size).and_then(|text| Xkb::new(&text));
                if state.xkb.is_none() {
                    eprintln!("[wayland] keyboard unusable: libxkbcommon missing or keymap rejected");
                }
            }
            Event::Enter { surface, .. } => state.focus = state.surface_id(&surface),
            Event::Leave { .. } => {
                state.focus = None;
                state.repeat = None;
            }
            Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(xkb) = &mut state.xkb {
                    xkb.update_modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            Event::RepeatInfo { rate, delay } => {
                state.repeat_rate = (rate.max(0) as u32, Duration::from_millis(delay.max(0) as u64));
            }
            Event::Key { key: code, state: key_state, .. } => {
                let (Some(xkb), Some(surface)) = (&state.xkb, state.focus) else { return };
                match key_state {
                    WEnum::Value(wl_keyboard::KeyState::Pressed) => {
                        let Some(key) = xkb.key(code) else { return };
                        state.events.push(PlatformEvent::Input { surface, event: InputEvent::Key(key), global: None });
                        // Rate 0 means the compositor wants no repeating.
                        state.repeat = (xkb.repeats(code) && state.repeat_rate.0 > 0)
                            .then(|| (code, key, Instant::now() + state.repeat_rate.1));
                    }
                    WEnum::Value(wl_keyboard::KeyState::Released) if state.repeat.is_some_and(|(held, _, _)| held == code) => {
                        state.repeat = None;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);
