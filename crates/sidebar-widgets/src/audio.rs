use sidebar_core::source::{AudioControl, AudioSample};
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, InputEvent, MeasureCtx, Orientation, PointerButton, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, DETAIL_SIZE, GAP};
use crate::{CARD_PAD, Every, HEADER_SIZE, card, header};

const BAR: f32 = 6.0;
/// Volume change per wheel notch.
const STEP: f32 = 0.05;

/// What a click lands on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    /// The bar: sets the volume there.
    Bar,
    /// Anywhere else: mutes or unmutes.
    Mute,
}

/// The default audio output's volume, and its name. The wheel changes the
/// volume, a click on the bar sets it, and a click elsewhere mutes.
pub struct AudioMeter {
    source: Box<dyn Source<AudioSample>>,
    control: Box<dyn AudioControl>,
    every: Every,
    last: Option<AudioSample>,
    pressed: Option<Target>,
}

impl AudioMeter {
    pub fn new(source: Box<dyn Source<AudioSample>>, control: Box<dyn AudioControl>) -> Self {
        // Volume keys and other apps change it too; check often.
        let every = Every::new(std::time::Duration::from_millis(500));
        Self { source, control, every, last: None, pressed: None }
    }

    fn has_device(&self) -> bool {
        self.last.as_ref().is_some_and(|s| s.device.is_some())
    }

    /// The bar's row: below the header and the device name.
    fn bar_row(&self, bounds: Rect) -> Rect {
        let area = bounds.inset(CARD_PAD);
        let mut y = area.y + line_height(HEADER_SIZE) + GAP;
        if self.has_device() {
            y += line_height(DETAIL_SIZE) + GAP;
        }
        Rect::new(area.x, y, area.width, BAR.max(area.bottom() - y))
    }

    fn target_at(&self, bounds: Rect, pos: sidebar_core::Point) -> Option<Target> {
        let bar = self.bar_row(bounds);
        if bar.inset(-3.0).contains(pos) {
            Some(Target::Bar)
        } else {
            bounds.contains(pos).then_some(Target::Mute)
        }
    }

    fn set_volume(&mut self, volume: f32) -> bool {
        let Some(last) = &mut self.last else { return false };
        let volume = (volume * 100.0).round().clamp(0.0, 100.0) / 100.0;
        self.control.set_volume(volume);
        if last.muted {
            self.control.set_muted(false);
            last.muted = false;
        }
        last.volume = volume;
        true
    }
}

impl Widget for AudioMeter {
    fn name(&self) -> &str {
        "audio"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        match orientation {
            Orientation::Vertical => {
                let device = if self.has_device() { line_height(DETAIL_SIZE) + GAP } else { 0.0 };
                line_height(HEADER_SIZE) + GAP + device + BAR + 2.0 + 2.0 * CARD_PAD
            }
            Orientation::Horizontal => 120.0,
        }
    }

    fn update(&mut self, ctx: &UpdateCtx) -> bool {
        if !self.every.due(ctx.now) {
            return false;
        }
        let sample = self.source.sample();
        let changed = sample != self.last;
        self.last = sample;
        changed
    }

    fn on_input(&mut self, event: &InputEvent, bounds: Rect) -> bool {
        match *event {
            InputEvent::Scroll { dy, .. } if dy != 0.0 => {
                let volume = self.last.as_ref().map_or(0.0, |s| s.volume);
                self.set_volume(volume - dy * STEP)
            }
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true } => {
                self.pressed = self.target_at(bounds, pos);
                false
            }
            // A click: released where it was pressed. A drag sends
            // PointerLeft first, so it never changes anything.
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false } => {
                let Some(target) = self.pressed.take().filter(|t| self.target_at(bounds, pos) == Some(*t)) else { return false };
                match target {
                    Target::Bar => {
                        let bar = self.bar_row(bounds);
                        self.set_volume((pos.x - bar.x) / bar.width.max(1.0))
                    }
                    Target::Mute => {
                        let Some(last) = &mut self.last else { return false };
                        last.muted = !last.muted;
                        self.control.set_muted(last.muted);
                        true
                    }
                }
            }
            InputEvent::PointerLeft => {
                self.pressed = None;
                false
            }
            _ => false,
        }
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let bounds = ctx.bounds;
        let area = card(ctx);
        let value = match &self.last {
            None => "n/a".to_string(),
            Some(s) if s.muted => "muted".to_string(),
            Some(s) => format!("{:.0}%", s.volume * 100.0),
        };
        let rest = header(ctx, area, "VOL", &value);
        if let Some(device) = self.last.as_ref().and_then(|s| s.device.as_deref()) {
            let fg = ctx.theme.foreground;
            meter::details(ctx, Rect::new(rest.x, rest.y, rest.width, line_height(DETAIL_SIZE)), vec![(device.to_string(), fg.with_alpha(170))]);
        }
        let row = self.bar_row(bounds);
        let track = Rect::new(row.x, row.y, row.width, BAR);
        let level = self.last.as_ref().map_or(0.0, |s| s.volume.clamp(0.0, 1.0));
        ctx.scene.fill_rect(track, ctx.theme.foreground.with_alpha(40), BAR / 2.0);
        let muted = self.last.as_ref().is_some_and(|s| s.muted);
        let color = if muted { ctx.theme.foreground.with_alpha(90) } else { ctx.theme.accent };
        if level > 0.0 {
            ctx.scene.fill_rect(Rect { width: track.width * level, ..track }, color, BAR / 2.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::SystemTime;

    use sidebar_core::Point;

    use super::*;
    use crate::testing::drawn_text;

    #[derive(Default)]
    struct Recorder(Rc<RefCell<Vec<String>>>);

    impl AudioControl for Recorder {
        fn set_volume(&self, volume: f32) {
            self.0.borrow_mut().push(format!("volume {volume:.2}"));
        }
        fn set_muted(&self, muted: bool) {
            self.0.borrow_mut().push(format!("muted {muted}"));
        }
    }

    fn click(w: &mut AudioMeter, bounds: Rect, pos: Point) -> bool {
        w.on_input(&InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true }, bounds);
        w.on_input(&InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false }, bounds)
    }

    #[test]
    fn shows_and_changes_the_volume() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let sample = AudioSample { volume: 0.48, muted: true, device: Some("Speakers".into()) };
        let mut w = AudioMeter::new(Box::new(move || Some(sample.clone())), Box::new(Recorder(calls.clone())));
        assert!(w.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH }));
        let ctx_measure = |w: &AudioMeter| {
            let theme = sidebar_core::Theme::default();
            w.measure(&MeasureCtx { text: &sidebar_core::ApproxText, theme: &theme }, Orientation::Vertical, 148.0)
        };
        let bounds = Rect::new(0.0, 0.0, 148.0, ctx_measure(&w));
        assert_eq!(drawn_text(&w, bounds.width, bounds.height), ["VOL", "muted", "Speakers"]);

        // Wheel up: louder, and unmuted.
        assert!(w.on_input(&InputEvent::Scroll { pos: Point::new(70.0, 10.0), dx: 0.0, dy: -1.0 }, bounds));
        assert_eq!(*calls.borrow(), ["volume 0.53", "muted false"]);
        assert_eq!(drawn_text(&w, bounds.width, bounds.height)[1], "53%");

        // A click on the header mutes; on the bar's middle, half volume.
        calls.borrow_mut().clear();
        assert!(click(&mut w, bounds, Point::new(20.0, 10.0)));
        let bar = w.bar_row(bounds);
        assert!(click(&mut w, bounds, Point::new(bar.x + bar.width / 2.0, bar.y + 2.0)));
        assert_eq!(*calls.borrow(), ["muted true", "volume 0.50", "muted false"]);

        // A press that turns into a drag does nothing.
        w.on_input(&InputEvent::PointerButton { pos: Point::new(20.0, 10.0), button: PointerButton::Primary, pressed: true }, bounds);
        w.on_input(&InputEvent::PointerLeft, bounds);
        assert!(!w.on_input(&InputEvent::PointerButton { pos: Point::new(20.0, 10.0), button: PointerButton::Primary, pressed: false }, bounds));
    }
}
