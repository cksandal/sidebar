//! Platform-independent core of the sidebar.
//!
//! Everything here is pure data and logic: geometry, edge snapping, dock
//! layout, the widget trait, and the display list widgets draw into. Nothing
//! in this crate knows about Linux, Windows, X11, Wayland, or any window
//! manager. Backends in `sidebar-platform` consume what this crate produces
//! ([`shell::SurfaceSpec`]s and [`render::Scene`]s) and feed back
//! [`widget::InputEvent`]s and [`monitor::MonitorInfo`]s.

pub mod config;
pub mod edge;
pub mod geometry;
pub mod layout;
pub mod monitor;
pub mod render;
pub mod shell;
pub mod snap;
pub mod source;
pub mod text;
pub mod widget;

pub use config::{Config, Theme};
pub use edge::{Edge, Orientation};
pub use geometry::{Point, Rect, Size};
pub use monitor::{MonitorId, MonitorInfo};
pub use render::{Color, DrawCmd, Scene};
pub use shell::{DockKey, Placement, SavedPlacement, SavedSpot, Shell, SurfaceKind, SurfaceSpec, SurfaceTarget};
pub use source::Source;
pub use text::{ApproxText, TextMeasure};
pub use widget::{DrawCtx, InputEvent, Key, MeasureCtx, PointerButton, UpdateCtx, Widget, WidgetId};
