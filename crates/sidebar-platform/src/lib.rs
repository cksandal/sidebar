//! The boundary between the sidebar and the operating system.
//!
//! [`Platform`] is the *only* interface the rest of the program uses to talk
//! to a display server. It is deliberately small and expressed in
//! `sidebar-core` types, so that:
//!
//! - backends (Wayland, X11, Windows, headless) are interchangeable at runtime;
//! - no protocol, window-manager, or toolkit type leaks above this crate;
//! - differences between platforms are surfaced as [`Capabilities`] the
//!   application can adapt to, instead of `cfg` checks scattered around.

use std::fmt;
use std::time::Duration;

use sidebar_core::{InputEvent, MonitorInfo, Point, Scene, Size, SurfaceSpec};

pub mod autostart;
pub mod dirs;
pub mod headless;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod process;
#[cfg(windows)]
pub mod windows;

/// Backend-assigned handle for a surface (window / layer surface / appbar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceId(pub u64);

impl fmt::Display for SurfaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// What a backend can actually do. The app must degrade gracefully when a
/// capability is missing rather than assume a particular platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Can keep other windows out of a dock area (layer-shell exclusive
    /// zone, `_NET_WM_STRUT_PARTIAL`, Windows AppBar).
    pub reserve_space: bool,
    /// Can place a floating surface at arbitrary global coordinates.
    /// False on Wayland, where clients cannot position their own toplevels.
    pub free_positioning: bool,
    /// Global pointer positions come from the display server. When false
    /// they are derived from where the backend believes its surface is,
    /// which is only reliable on the surface's own monitor.
    pub global_pointer: bool,
    /// Translucent backgrounds show what is behind them. False on X11
    /// without a compositing manager, where alpha is drawn as solid.
    pub transparency: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    /// Monitors were added, removed, or changed; call [`Platform::monitors`].
    MonitorsChanged,
    /// The platform resized a surface (e.g. compositor configure).
    SurfaceResized { surface: SurfaceId, size: Size },
    /// Pointer input. `event` is surface-local; `global` is the same
    /// position in global logical coordinates, or `None` when the backend
    /// cannot tell exactly (e.g. while the surface is being moved).
    Input { surface: SurfaceId, event: InputEvent, global: Option<Point> },
    CloseRequested { surface: SurfaceId },
    /// The session is ending or the display connection was lost.
    Quit,
}

#[derive(Debug)]
pub enum PlatformError {
    /// The current environment cannot run this backend (wrong OS, no
    /// display, missing compositor protocol, ...).
    Unsupported(String),
    /// The backend exists as a stub only.
    NotImplemented(&'static str),
    /// A runtime failure inside the backend.
    Backend(String),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlatformError::Unsupported(why) => write!(f, "unsupported: {why}"),
            PlatformError::NotImplemented(what) => write!(f, "{what} backend is not implemented yet"),
            PlatformError::Backend(msg) => write!(f, "backend error: {msg}"),
        }
    }
}

impl std::error::Error for PlatformError {}

pub type Result<T> = std::result::Result<T, PlatformError>;

pub trait Platform {
    fn name(&self) -> &'static str;

    fn capabilities(&self) -> Capabilities;

    fn monitors(&self) -> Vec<MonitorInfo>;

    fn create_surface(&mut self, spec: &SurfaceSpec) -> Result<SurfaceId>;

    /// Apply a changed spec (edge, thickness, floating rect) to a surface.
    fn configure_surface(&mut self, surface: SurfaceId, spec: &SurfaceSpec) -> Result<()>;

    fn destroy_surface(&mut self, surface: SurfaceId);

    /// Display a frame. Backends typically rasterize with `sidebar_render`
    /// and blit the pixels; scenes are in logical pixels.
    fn present(&mut self, surface: SurfaceId, scene: &Scene) -> Result<()>;

    /// Wait up to `timeout` for events and return everything pending.
    fn poll_events(&mut self, timeout: Duration) -> Vec<PlatformEvent>;
}

/// Backend names accepted by [`create`].
pub const BACKENDS: &[&str] = &["auto", "headless", "wayland", "x11", "windows"];

/// Opens a backend by name. `"auto"` picks the best one for this session.
pub fn create(name: &str) -> Result<Box<dyn Platform>> {
    match name {
        "auto" => auto(),
        "headless" => Ok(Box::new(headless::HeadlessPlatform::new())),
        #[cfg(all(target_os = "linux", feature = "wayland"))]
        "wayland" => Ok(Box::new(linux::wayland::WaylandPlatform::connect()?)),
        #[cfg(all(target_os = "linux", feature = "x11"))]
        "x11" => Ok(Box::new(linux::x11::X11Platform::connect()?)),
        #[cfg(windows)]
        "windows" => Ok(Box::new(windows::WindowsPlatform::connect()?)),
        other => Err(PlatformError::Unsupported(format!(
            "backend '{other}' is not available in this build (known: {})",
            BACKENDS.join(", ")
        ))),
    }
}

/// Picks a backend for the current OS/session.
pub fn auto() -> Result<Box<dyn Platform>> {
    #[cfg(target_os = "linux")]
    {
        linux::connect()
    }
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsPlatform::connect()?))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(PlatformError::Unsupported(format!("no backend for {}", std::env::consts::OS)))
    }
}
