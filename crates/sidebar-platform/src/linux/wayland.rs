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
//! - Input: `wl_pointer` events are surface-local. Global positions are
//!   derived from where the surface was placed, so `global_pointer` is
//!   false. That holds during a drag because the shell never moves the
//!   surface holding the grab. Docks are assumed to start at their output's
//!   corner, which is off by the size of any other panel on an adjacent
//!   edge.
//!
//! TODO: `wp_fractional_scale_v1` (scale is the integer output scale now).

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::time::Duration;

use sidebar_core::{
    Edge, InputEvent, MonitorId, MonitorInfo, Point, PointerButton, Rect, Scene, Size, SurfaceKind,
    SurfaceSpec, snap,
};
use sidebar_render::AlphaMode;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputInfo, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{AxisScroll, PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};

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

struct Surface {
    layer: LayerSurface,
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
    surfaces: HashMap<SurfaceId, Surface>,
    events: Vec<PlatformEvent>,
}

impl State {
    fn surface_id(&self, wl_surface: &wl_surface::WlSurface) -> Option<SurfaceId> {
        self.surfaces.iter().find(|(_, s)| s.layer.wl_surface() == wl_surface).map(|(id, _)| *id)
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
        let scale = s.scale.max(1);
        let pixmap = sidebar_render::rasterize(scene, scale as f32);
        let (w, h) = (pixmap.width as i32, pixmap.height as i32);

        let (buffer, canvas) =
            self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888).map_err(be)?;
        pixmap.write_bgra(canvas, AlphaMode::Premultiplied);

        let wl_surface = s.layer.wl_surface();
        wl_surface.set_buffer_scale(scale);
        wl_surface.damage_buffer(0, 0, w, h);
        buffer.attach_to(wl_surface).map_err(be)?;
        s.layer.commit();
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

        let mut state = State {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seat: SeatState::new(&globals, &qh),
            shm,
            pool,
            pointer: None,
            surfaces: HashMap::new(),
            events: Vec::new(),
        };
        // First roundtrip binds outputs (and xdg-output); second delivers
        // their geometry.
        queue.roundtrip(&mut state).map_err(be)?;
        queue.roundtrip(&mut state).map_err(be)?;
        state.events.clear();

        Ok(Self { _conn: conn, queue, qh, compositor, layer_shell, state, next_id: 1 })
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

    fn new_surface(&self, spec: &SurfaceSpec) -> Result<Surface> {
        let (output, info) = self.output_for(spec.monitor)?;
        let bounds = monitor_info(&info, false).bounds;
        let wl_surface = self.compositor.create_surface(&self.qh);
        let layer =
            self.layer_shell.create_layer_surface(&self.qh, wl_surface, layer_of(spec), Some(NAMESPACE), Some(&output));
        configure_layer(&layer, spec, bounds);
        // Initial commit without a buffer; the compositor answers with a
        // configure carrying the real size.
        layer.commit();
        Ok(Surface {
            layer,
            spec: spec.clone(),
            configured: false,
            closed: false,
            size: (0, 0),
            scale: info.scale_factor.max(1),
            scene: None,
            buffer: None,
            origin: surface_origin(spec, bounds),
        })
    }

    fn pump(&mut self, timeout: Duration) -> Result<()> {
        self.queue.dispatch_pending(&mut self.state).map_err(be)?;
        if self.state.events.is_empty() {
            self.queue.flush().map_err(be)?;
            if let Some(guard) = self.queue.prepare_read() {
                if super::wait_readable(guard.connection_fd().as_raw_fd(), timeout) {
                    guard.read().map_err(be)?;
                }
                // Dropping an unread guard cancels the read.
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
        // Rejected in create_surface.
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
        if matches!(spec.kind, SurfaceKind::Window { .. }) {
            // Needs xdg-shell plus keyboard input (xkbcommon keymaps).
            return Err(PlatformError::NotImplemented("top-level windows on the Wayland"));
        }
        let id = SurfaceId(self.next_id);
        self.next_id += 1;
        let surface = self.new_surface(spec)?;
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
        if current.closed || current.spec.monitor != spec.monitor || current.spec.above != spec.above {
            // A layer surface is bound to one output (and, before layer-shell
            // v2, one layer) for life: recreate it.
            let mut fresh = self.new_surface(spec)?;
            fresh.scene = current.scene.clone();
            self.state.surfaces.insert(surface, fresh);
        } else {
            let bounds = self.output_for(spec.monitor).map(|(_, info)| monitor_info(&info, false).bounds)?;
            if let Some(s) = self.state.surfaces.get_mut(&surface) {
                configure_layer(&s.layer, spec, bounds);
                s.layer.commit();
                s.origin = surface_origin(spec, bounds);
                s.spec = spec.clone();
            }
        }
        self.queue.flush().map_err(be)
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        // Dropping the LayerSurface destroys the protocol objects.
        self.state.surfaces.remove(&surface);
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
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, capability: Capability) {
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
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
