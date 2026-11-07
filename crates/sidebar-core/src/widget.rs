//! The widget contract.

use std::time::SystemTime;

use crate::config::Theme;
use crate::edge::Orientation;
use crate::geometry::{Point, Rect};
use crate::render::Scene;
use crate::text::TextMeasure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(pub u64);

pub struct UpdateCtx {
    pub now: SystemTime,
}

pub struct MeasureCtx<'a> {
    pub text: &'a dyn TextMeasure,
    pub theme: &'a Theme,
}

pub struct DrawCtx<'a> {
    pub scene: &'a mut Scene,
    /// Where the widget lives, in surface-local coordinates.
    pub bounds: Rect,
    pub theme: &'a Theme,
    pub text: &'a dyn TextMeasure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
    Other(u16),
}

/// Input in surface-local logical coordinates. Backends translate native
/// events (wl_pointer, XInput, WM_* messages) into these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    PointerMoved { pos: Point },
    PointerButton { pos: Point, button: PointerButton, pressed: bool },
    Scroll { pos: Point, dx: f32, dy: f32 },
    PointerLeft,
    /// A key press (repeats while held) on a surface with keyboard focus.
    Key(Key),
}

/// Keys, already mapped through the keyboard layout. Presses with Ctrl or
/// Alt held are not reported as `Char`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Escape,
    Tab,
    /// Shift+Tab.
    BackTab,
}

impl InputEvent {
    pub fn position(&self) -> Option<Point> {
        match *self {
            InputEvent::PointerMoved { pos }
            | InputEvent::PointerButton { pos, .. }
            | InputEvent::Scroll { pos, .. } => Some(pos),
            InputEvent::PointerLeft | InputEvent::Key(_) => None,
        }
    }
}

pub trait Widget {
    fn name(&self) -> &str;

    /// Length along the dock's main axis, given the available cross-axis
    /// extent (the dock thickness minus padding).
    fn measure(&self, ctx: &MeasureCtx<'_>, orientation: Orientation, cross_extent: f32) -> f32;

    /// Advance internal state. Return `true` if the widget needs a redraw.
    fn update(&mut self, _ctx: &UpdateCtx) -> bool {
        false
    }

    /// Handle input that hit this widget. Return `true` if it needs a redraw.
    fn on_input(&mut self, _event: &InputEvent, _bounds: Rect) -> bool {
        false
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>);
}
