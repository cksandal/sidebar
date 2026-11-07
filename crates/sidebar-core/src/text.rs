//! Text measurement contract.
//!
//! Widgets need text extents for layout, but fonts are a platform concern,
//! so the core only defines the interface. `sidebar-render` provides the
//! real implementation; [`ApproxText`] keeps the core usable (and testable)
//! without any fonts.

use crate::geometry::Size;

pub trait TextMeasure {
    /// Size of `text` laid out as a single line at `font_size` logical px.
    fn measure(&self, text: &str, font_size: f32) -> Size;
}

/// Line box height for a font size. Shared by every implementation so
/// layout does not shift when switching between them.
pub fn line_height(font_size: f32) -> f32 {
    (font_size * 1.3).ceil()
}

/// Font-free estimate: every character is 0.6 em wide.
pub struct ApproxText;

impl TextMeasure for ApproxText {
    fn measure(&self, text: &str, font_size: f32) -> Size {
        Size::new(text.chars().count() as f32 * font_size * 0.6, line_height(font_size))
    }
}
