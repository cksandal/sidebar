use std::time::UNIX_EPOCH;

use sidebar_core::source::UtcOffset;
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, InputEvent, MeasureCtx, Orientation, Point, PointerButton, Rect, Source, UpdateCtx, Widget};

use crate::{card, centered};

const TIME_SIZE: f32 = 16.0;
const DATE_SIZE: f32 = 11.0;
const ICON_SIZE: f32 = 15.0;
/// Width of each button beside the time.
const BUTTON_W: f32 = 26.0;
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A button beside the time: an icon and what a click does.
struct Button {
    icon: &'static str,
    action: Box<dyn FnMut()>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Right,
}

/// Local time (HH:MM:SS) with the date underneath, optionally between two
/// buttons (reload and settings in the app).
pub struct Clock {
    offset: Box<dyn Source<UtcOffset>>,
    last_secs: Option<i64>,
    time: String,
    date: String,
    buttons: Option<[Button; 2]>,
    hover: Option<Side>,
    pressed: Option<Side>,
}

impl Clock {
    /// `offset` supplies the local time zone; UTC if it reports `None`.
    pub fn new(offset: Box<dyn Source<UtcOffset>>) -> Self {
        Self { offset, last_secs: None, time: "--:--:--".into(), date: String::new(), buttons: None, hover: None, pressed: None }
    }

    /// Adds a ⟳ button on the left and a ⚙ button on the right; clicking
    /// one calls its function.
    pub fn with_buttons(mut self, on_reload: Box<dyn FnMut()>, on_settings: Box<dyn FnMut()>) -> Self {
        self.buttons = Some([Button { icon: "⟳", action: on_reload }, Button { icon: "⚙", action: on_settings }]);
        self
    }

    fn button_rect(bounds: Rect, side: Side) -> Rect {
        let x = match side {
            Side::Left => bounds.x,
            Side::Right => bounds.right() - BUTTON_W,
        };
        Rect::new(x, bounds.y, BUTTON_W, bounds.height)
    }

    fn side_at(&self, bounds: Rect, pos: Point) -> Option<Side> {
        self.buttons.as_ref()?;
        [Side::Left, Side::Right].into_iter().find(|s| Self::button_rect(bounds, *s).contains(pos))
    }
}

fn format_hms(secs: i64) -> String {
    let s = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// "Sun 1 Nov" for local seconds since the epoch.
pub(crate) fn format_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let (_, month, day) = civil_from_days(days);
    // 1970-01-01 was a Thursday.
    let weekday = WEEKDAYS[(days + 3).rem_euclid(7) as usize];
    format!("{weekday} {day} {}", MONTHS[month as usize - 1])
}

/// (year, month 1-12, day 1-31) for days since 1970-01-01, proleptic
/// Gregorian (Howard Hinnant's algorithm).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl Widget for Clock {
    fn name(&self) -> &str {
        "clock"
    }

    fn measure(&self, ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        // Fixed samples, so the widget doesn't resize as the time changes.
        let time = ctx.text.measure("00:00:00", TIME_SIZE);
        let date = ctx.text.measure("Wed 30 Sep", DATE_SIZE);
        match orientation {
            Orientation::Vertical => time.height + date.height + TIME_SIZE * 0.75,
            Orientation::Horizontal => {
                let buttons = if self.buttons.is_some() { 2.0 * BUTTON_W } else { 0.0 };
                time.width.max(date.width) + TIME_SIZE + buttons
            }
        }
    }

    fn on_input(&mut self, event: &InputEvent, bounds: Rect) -> bool {
        let before = self.hover;
        match *event {
            InputEvent::PointerMoved { pos } => self.hover = self.side_at(bounds, pos),
            InputEvent::PointerLeft => {
                self.hover = None;
                self.pressed = None;
            }
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true } => {
                self.pressed = self.side_at(bounds, pos);
            }
            // A click: released on the button it was pressed on. A drag
            // sends PointerLeft first, so it never clicks.
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false } => {
                let side = self.pressed.take().filter(|s| self.side_at(bounds, pos) == Some(*s));
                if let (Some(side), Some(buttons)) = (side, &mut self.buttons) {
                    (buttons[side as usize].action)();
                }
            }
            _ => {}
        }
        self.hover != before
    }

    fn update(&mut self, ctx: &UpdateCtx) -> bool {
        let utc = ctx.now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let local = utc + i64::from(self.offset.sample().map_or(0, |o| o.0));
        if self.last_secs == Some(local) {
            return false;
        }
        self.last_secs = Some(local);
        self.time = format_hms(local);
        self.date = format_date(local);
        true
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let b = ctx.bounds;
        card(ctx);
        let (th, dh) = (line_height(TIME_SIZE), line_height(DATE_SIZE));
        // Center the two lines as a block.
        let top = (b.y + (b.height - th - dh) / 2.0).round();
        let time_at = centered(ctx.text, &self.time, TIME_SIZE, Rect::new(b.x, top, b.width, th));
        let date_at = centered(ctx.text, &self.date, DATE_SIZE, Rect::new(b.x, top + th, b.width, dh));
        let fg = ctx.theme.foreground;
        ctx.scene.text(time_at, self.time.clone(), TIME_SIZE, fg);
        ctx.scene.text(Point::new(date_at.x, date_at.y), self.date.clone(), DATE_SIZE, fg.with_alpha(170));
        let Some(buttons) = &self.buttons else { return };
        for (side, button) in [Side::Left, Side::Right].into_iter().zip(buttons) {
            let area = Self::button_rect(b, side).inset(4.0);
            let hovered = self.hover == Some(side);
            if hovered {
                ctx.scene.fill_rect(area, ctx.theme.accent.with_alpha(70), 4.0);
            }
            let at = centered(ctx.text, button.icon, ICON_SIZE, area);
            ctx.scene.text(at, button.icon, ICON_SIZE, fg.with_alpha(if hovered { 255 } else { 130 }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::testing::drawn_text;

    #[test]
    fn formats_time_of_day() {
        assert_eq!(format_hms(0), "00:00:00");
        assert_eq!(format_hms(86_400 + 3_661), "01:01:01");
        assert_eq!(format_hms(-1), "23:59:59");
    }

    #[test]
    fn formats_dates() {
        assert_eq!(format_date(0), "Thu 1 Jan");
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // 2000-02-29 (leap day) and 2026-11-01.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(format_date(20_758 * 86_400), "Sun 1 Nov");
    }

    #[test]
    fn buttons_click() {
        use std::cell::Cell;
        use std::rc::Rc;
        let clicks = Rc::new(Cell::new((0, 0)));
        let (a, b) = (clicks.clone(), clicks.clone());
        let mut clock = Clock::new(Box::new(|| None)).with_buttons(
            Box::new(move || a.set((a.get().0 + 1, a.get().1))),
            Box::new(move || b.set((b.get().0, b.get().1 + 1))),
        );
        let bounds = Rect::new(0.0, 0.0, 148.0, 50.0);
        let press = |pos, pressed| InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed };
        let click = |clock: &mut Clock, x| {
            clock.on_input(&press(Point::new(x, 25.0), true), bounds);
            clock.on_input(&press(Point::new(x, 25.0), false), bounds);
        };
        click(&mut clock, 10.0);
        click(&mut clock, 140.0);
        click(&mut clock, 140.0);
        click(&mut clock, 74.0);
        assert_eq!(clicks.get(), (1, 2));
        // Pressed on one button, released on the other: nothing.
        clock.on_input(&press(Point::new(10.0, 25.0), true), bounds);
        clock.on_input(&press(Point::new(140.0, 25.0), false), bounds);
        assert_eq!(clicks.get(), (1, 2));
        assert!(clock.on_input(&InputEvent::PointerMoved { pos: Point::new(10.0, 25.0) }, bounds), "hover redraws");
        assert!(drawn_text(&clock, 148.0, 50.0).ends_with(&["⟳".to_string(), "⚙".to_string()]));
    }

    #[test]
    fn applies_utc_offset() {
        let mut clock = Clock::new(Box::new(|| Some(UtcOffset(-5 * 3600))));
        let noon_utc = SystemTime::UNIX_EPOCH + Duration::from_secs(20_758 * 86_400 + 12 * 3600);
        assert!(clock.update(&UpdateCtx { now: noon_utc }));
        assert_eq!(drawn_text(&clock, 148.0, 50.0), ["07:00:00", "Sun 1 Nov"]);
    }
}
