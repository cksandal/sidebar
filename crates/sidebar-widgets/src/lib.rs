//! Built-in widgets.
//!
//! Widgets may only use `sidebar-core`. Anything that needs system data
//! (CPU load, battery, network) gets it through a `sidebar_core::Source`
//! with per-OS implementations, never by calling OS APIs from the widget.

mod audio;
mod battery;
mod calendar;
mod clock;
mod cpu;
mod disk;
mod gpu;
mod icons;
mod label;
mod meter;
mod network;
mod stocks;
mod weather;

pub use audio::AudioMeter;
pub use battery::BatteryMeter;
pub use calendar::Calendar;
pub use clock::Clock;
pub use cpu::CpuGraph;
pub use disk::DiskGraph;
pub use gpu::GpuGraph;
pub use label::Label;
pub use network::NetGraph;
pub use stocks::StockTicker;
pub use weather::WeatherCard;

use std::time::{Duration, SystemTime};

use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, MeasureCtx, Orientation, Point, Rect, TextMeasure};

/// How often metric widgets sample their source.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
/// Inner padding of a card.
const CARD_PAD: f32 = 6.0;
const CARD_RADIUS: f32 = 4.0;
const HEADER_SIZE: f32 = 12.0;

/// Main-axis extent of a single line of text plus padding.
pub(crate) fn text_extent(ctx: &MeasureCtx<'_>, text: &str, font_size: f32, orientation: Orientation) -> f32 {
    let size = ctx.text.measure(text, font_size);
    match orientation {
        Orientation::Vertical => size.height + font_size * 0.75,
        Orientation::Horizontal => size.width + font_size,
    }
}

/// Origin that centers one line of `text` inside `bounds`.
pub(crate) fn centered(measure: &dyn TextMeasure, text: &str, font_size: f32, bounds: Rect) -> Point {
    let size = measure.measure(text, font_size);
    Point::new(
        (bounds.x + (bounds.width - size.width) / 2.0).round(),
        (bounds.y + (bounds.height - size.height) / 2.0).round(),
    )
}

/// Draws the rounded card behind a widget and returns its content area.
pub(crate) fn card(ctx: &mut DrawCtx<'_>) -> Rect {
    ctx.scene.fill_rect(ctx.bounds, ctx.theme.accent.with_alpha(28), CARD_RADIUS);
    ctx.bounds.inset(CARD_PAD)
}

/// Main-axis extent of a metric card: a header line, a gap, and a visual of
/// `visual` px (vertical docks), or a fixed width (horizontal docks).
pub(crate) fn metric_extent(orientation: Orientation, visual: f32) -> f32 {
    match orientation {
        Orientation::Vertical => line_height(HEADER_SIZE) + 4.0 + visual + 2.0 * CARD_PAD,
        Orientation::Horizontal => 120.0,
    }
}

/// Draws `title` left-aligned and `value` right-aligned on one line at the
/// top of `area`; returns the area below it.
pub(crate) fn header(ctx: &mut DrawCtx<'_>, area: Rect, title: &str, value: &str) -> Rect {
    let h = line_height(HEADER_SIZE);
    let theme = ctx.theme;
    ctx.scene.text(Point::new(area.x, area.y), title.to_string(), HEADER_SIZE, theme.foreground.with_alpha(170));
    let w = ctx.text.measure(value, HEADER_SIZE).width;
    ctx.scene.text(Point::new((area.right() - w).round(), area.y), value.to_string(), HEADER_SIZE, theme.foreground);
    Rect::new(area.x, area.y + h + 4.0, area.width, (area.height - h - 4.0).max(0.0))
}

/// A horizontal bar filled to `fraction`, vertically centered in `area`.
pub(crate) fn bar(ctx: &mut DrawCtx<'_>, area: Rect, fraction: f32, thickness: f32) {
    let track = Rect::new(area.x, area.y + ((area.height - thickness) / 2.0).max(0.0), area.width, thickness.min(area.height));
    ctx.scene.fill_rect(track, ctx.theme.foreground.with_alpha(40), thickness / 2.0);
    let fill = Rect { width: track.width * fraction.clamp(0.0, 1.0), ..track };
    if fill.width > 0.0 {
        ctx.scene.fill_rect(fill, ctx.theme.accent, thickness / 2.0);
    }
}

/// Tracks when a widget should next sample its source.
pub(crate) struct Every {
    interval: Duration,
    next: Option<SystemTime>,
}

impl Every {
    pub(crate) fn new(interval: Duration) -> Self {
        Self { interval, next: None }
    }

    /// True (and schedules the next time) when `now` is due.
    pub(crate) fn due(&mut self, now: SystemTime) -> bool {
        // A clock jumping backwards must not stall sampling.
        let due = self.next.is_none_or(|next| now >= next || next.duration_since(now).is_ok_and(|d| d > self.interval));
        if due {
            self.next = Some(now + self.interval);
        }
        due
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use sidebar_core::{ApproxText, Color, DrawCmd, DrawCtx, Rect, Scene, Size, Theme, Widget};

    /// Draws `widget` into a `w`x`h` scene and returns the text it drew.
    pub fn drawn_text(widget: &dyn Widget, w: f32, h: f32) -> Vec<String> {
        let mut scene = Scene::new(Size::new(w, h));
        let theme = Theme::default();
        widget.draw(&mut DrawCtx { scene: &mut scene, bounds: Rect::new(0.0, 0.0, w, h), theme: &theme, text: &ApproxText });
        scene
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Rects drawn in `color`.
    pub fn rects_in(widget: &dyn Widget, w: f32, h: f32, color: Color) -> Vec<Rect> {
        let mut scene = Scene::new(Size::new(w, h));
        let theme = Theme::default();
        widget.draw(&mut DrawCtx { scene: &mut scene, bounds: Rect::new(0.0, 0.0, w, h), theme: &theme, text: &ApproxText });
        scene
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::FillRect { rect, color: c, .. } if *c == color => Some(*rect),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fires_once_per_interval() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let mut every = Every::new(Duration::from_secs(1));
        assert!(every.due(t0));
        assert!(!every.due(t0 + Duration::from_millis(500)));
        assert!(every.due(t0 + Duration::from_secs(1)));
        // Clock set back an hour: sample right away instead of waiting.
        assert!(every.due(t0 - Duration::from_secs(3600)));
    }
}
