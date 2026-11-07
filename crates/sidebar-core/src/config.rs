//! User-facing settings. Plain data: the app fills it from a TOML file,
//! but the core doesn't know or care where it came from.

use crate::edge::Edge;
use crate::render::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub accent: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: Color::rgba(24, 26, 32, 230),
            foreground: Color::rgb(220, 223, 228),
            accent: Color::rgb(97, 175, 239),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Cross-axis size of a dock, in logical pixels.
    pub thickness: f32,
    pub padding: f32,
    pub spacing: f32,
    /// How close (logical px) a dragged widget must get to an edge to snap.
    pub snap_threshold: f32,
    /// Ask the platform to keep other windows out of the dock area.
    pub reserve_space: bool,
    /// Allow widgets to float when dropped away from an edge. When false (or
    /// when the platform cannot position windows freely) they snap to the
    /// nearest edge instead.
    pub allow_floating: bool,
    /// Keep docks and floating widgets above other windows. When false
    /// they sit below ordinary windows (above the desktop).
    pub always_on_top: bool,
    pub default_edge: Edge,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            thickness: 160.0,
            padding: 6.0,
            spacing: 6.0,
            snap_threshold: 32.0,
            reserve_space: true,
            allow_floating: true,
            always_on_top: false,
            default_edge: Edge::Right,
            theme: Theme::default(),
        }
    }
}
