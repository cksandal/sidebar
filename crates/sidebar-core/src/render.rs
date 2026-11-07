//! Retained display list.
//!
//! Widgets never touch a graphics API. They append [`DrawCmd`]s to a
//! [`Scene`]; a backend (or `sidebar-render`) turns the scene into pixels.
//! This is the seam that keeps widgets independent of the rendering stack.

use crate::geometry::{Point, Rect, Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self::rgba(self.r, self.g, self.b, a)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DrawCmd {
    FillRect { rect: Rect, color: Color, radius: f32 },
    StrokeRect { rect: Rect, color: Color, width: f32 },
    /// `origin` is the top-left of the text's line box.
    Text { origin: Point, text: String, size: f32, color: Color },
}

/// Draw commands for one surface, in that surface's local logical coordinates.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub size: Size,
    pub cmds: Vec<DrawCmd>,
}

impl Scene {
    pub fn new(size: Size) -> Self {
        Self { size, cmds: Vec::new() }
    }

    pub fn clear(&mut self, color: Color) {
        self.cmds.clear();
        self.fill_rect(Rect::from_origin_size(Point::default(), self.size), color, 0.0);
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color, radius: f32) {
        self.cmds.push(DrawCmd::FillRect { rect, color, radius });
    }

    pub fn stroke_rect(&mut self, rect: Rect, color: Color, width: f32) {
        self.cmds.push(DrawCmd::StrokeRect { rect, color, width });
    }

    pub fn text(&mut self, origin: Point, text: impl Into<String>, size: f32, color: Color) {
        self.cmds.push(DrawCmd::Text { origin, text: text.into(), size, color });
    }
}
