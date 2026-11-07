use std::time::UNIX_EPOCH;

use sidebar_core::source::UtcOffset;
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, InputEvent, MeasureCtx, Orientation, Point, PointerButton, Rect, Source, UpdateCtx, Widget};

use crate::clock::{civil_from_days, format_date};
use crate::meter::{DETAIL_SIZE, GAP};
use crate::{CARD_PAD, Every, HEADER_SIZE, SAMPLE_INTERVAL, card, centered, header, metric_extent};

const DAY_SIZE: f32 = 10.0;
/// Always six week rows, so the card keeps its size from month to month.
const ROWS: usize = 6;
/// Width of the ‹ and › click targets beside the month name.
const ARROW_W: f32 = 22.0;
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn row_height() -> f32 {
    line_height(DAY_SIZE) + 6.0
}

fn title_height() -> f32 {
    line_height(HEADER_SIZE) + GAP
}

fn weekday_row_height() -> f32 {
    line_height(DETAIL_SIZE) + 4.0
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// algorithm, the inverse of `civil_from_days`).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = Monday ... 6 = Sunday (1970-01-01 was a Thursday).
fn iso_weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

/// ISO 8601 week number: the week belongs to the year of its Thursday.
fn iso_week(days: i64) -> u32 {
    let thursday = days - iso_weekday(days) + 3;
    let (year, _, _) = civil_from_days(thursday);
    ((thursday - days_from_civil(year, 1, 1)) / 7 + 1) as u32
}

/// (year, month) moved by `shift` months.
fn add_months(year: i64, month: u32, shift: i32) -> (i64, u32) {
    let index = year * 12 + i64::from(month) - 1 + i64::from(shift);
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Previous,
    Next,
    Today,
}

/// A month grid with ISO week numbers, today highlighted and its week
/// marked. Click ‹ or › to page through months and the month name to come
/// back. Shows the week and date on horizontal docks.
pub struct Calendar {
    offset: Box<dyn Source<UtcOffset>>,
    every: Every,
    monday_first: bool,
    /// Today, as local days since 1970-01-01.
    today: Option<i64>,
    /// Months away from today's month that are shown.
    shift: i32,
    pressed: Option<Target>,
}

impl Calendar {
    /// `offset` supplies the local time zone (UTC if it reports `None`);
    /// weeks start on Monday if `monday_first`, else Sunday.
    pub fn new(offset: Box<dyn Source<UtcOffset>>, monday_first: bool) -> Self {
        Self { offset, every: Every::new(SAMPLE_INTERVAL), monday_first, today: None, shift: 0, pressed: None }
    }

    fn full_height(&self) -> f32 {
        title_height() + weekday_row_height() + 1.0 + ROWS as f32 * row_height() + 2.0 * CARD_PAD
    }

    /// The shown (year, month).
    fn month(&self) -> (i64, u32) {
        let (year, month, _) = civil_from_days(self.today.unwrap_or(0));
        add_months(year, month, self.shift)
    }

    /// The day in the top-left cell: the first of the month moved back to
    /// the start of its week.
    fn grid_start(&self) -> i64 {
        let (year, month) = self.month();
        let first = days_from_civil(year, month, 1);
        let column = if self.monday_first { iso_weekday(first) } else { (iso_weekday(first) + 1) % 7 };
        first - column
    }

    fn target_at(&self, bounds: Rect, pos: Point) -> Option<Target> {
        let area = bounds.inset(CARD_PAD);
        if !area.contains(pos) || pos.y >= area.y + title_height() {
            return None;
        }
        Some(if pos.x < area.x + ARROW_W {
            Target::Previous
        } else if pos.x > area.right() - ARROW_W {
            Target::Next
        } else {
            Target::Today
        })
    }

    fn draw_title(&self, ctx: &mut DrawCtx<'_>, area: Rect) {
        let fg = ctx.theme.foreground;
        let h = line_height(HEADER_SIZE);
        let (year, month) = self.month();
        let title = format!("{} {year}", MONTHS[month as usize - 1]);
        let at = centered(ctx.text, &title, HEADER_SIZE, Rect::new(area.x, area.y, area.width, h));
        // Brighter when paged away, as a hint that clicking comes back.
        ctx.scene.text(at, title, HEADER_SIZE, if self.shift == 0 { fg } else { ctx.theme.accent.with_alpha(255) });
        for (arrow, x) in [("‹", area.x), ("›", area.right() - ARROW_W)] {
            let at = centered(ctx.text, arrow, HEADER_SIZE, Rect::new(x, area.y, ARROW_W, h));
            ctx.scene.text(at, arrow, HEADER_SIZE, fg.with_alpha(140));
        }
    }

    fn draw_grid(&self, ctx: &mut DrawCtx<'_>, area: Rect) {
        let fg = ctx.theme.foreground;
        let accent = ctx.theme.accent;
        let week_w = ctx.text.measure("00", DETAIL_SIZE).width + GAP;
        let col = (area.width - week_w) / 7.0;
        let cell = |i: usize, y: f32, h: f32| Rect::new(area.x + week_w + i as f32 * col, y, col, h);

        let letters: [&str; 7] = if self.monday_first {
            ["M", "T", "W", "T", "F", "S", "S"]
        } else {
            ["S", "M", "T", "W", "T", "F", "S"]
        };
        let mut y = area.y;
        let wh = weekday_row_height();
        let at = centered(ctx.text, "W", DETAIL_SIZE, Rect::new(area.x, y, week_w, wh));
        ctx.scene.text(at, "W", DETAIL_SIZE, fg.with_alpha(90));
        for (i, letter) in letters.iter().enumerate() {
            let at = centered(ctx.text, letter, DETAIL_SIZE, cell(i, y, wh));
            ctx.scene.text(at, *letter, DETAIL_SIZE, fg.with_alpha(140));
        }
        y += wh;
        ctx.scene.fill_rect(Rect::new(area.x, y, area.width, 1.0), fg.with_alpha(30), 0.0);
        y += 1.0;

        let (_, month) = self.month();
        let rh = row_height();
        let mut day = self.grid_start();
        for _ in 0..ROWS {
            let week = (0..7).map(|i| day + i);
            if let Some(today) = self.today.filter(|t| week.clone().any(|d| d == *t)) {
                // This week's row, then a pill behind today's date.
                let band = Rect::new(area.x + week_w - 2.0, y + 1.0, area.width - week_w + 2.0, rh - 2.0);
                ctx.scene.fill_rect(band, accent.with_alpha(45), 4.0);
                let c = cell((today - day) as usize, y, rh);
                let size = (rh - 2.0).min(col - 2.0);
                let pill = Rect::new((c.x + (c.width - size) / 2.0).round(), y + 1.0, size, size);
                ctx.scene.fill_rect(pill, accent.with_alpha(255), size / 2.0);
            }
            // The week number of the row's Thursday, as ISO counts it.
            let thursday = if self.monday_first { day + 3 } else { day + 4 };
            let number = iso_week(thursday).to_string();
            let at = centered(ctx.text, &number, DETAIL_SIZE, Rect::new(area.x, y, week_w, rh));
            ctx.scene.text(at, number, DETAIL_SIZE, fg.with_alpha(90));
            for (i, d) in week.enumerate() {
                let (_, m, n) = civil_from_days(d);
                let text = n.to_string();
                let color = if Some(d) == self.today {
                    fg
                } else if m != month {
                    fg.with_alpha(70)
                } else {
                    fg.with_alpha(220)
                };
                let at = centered(ctx.text, &text, DAY_SIZE, cell(i, y, rh));
                ctx.scene.text(at, text, DAY_SIZE, color);
            }
            day += 7;
            y += rh;
        }
    }
}

impl Widget for Calendar {
    fn name(&self) -> &str {
        "calendar"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        match orientation {
            Orientation::Vertical => self.full_height(),
            Orientation::Horizontal => metric_extent(orientation, line_height(DAY_SIZE)),
        }
    }

    fn update(&mut self, ctx: &UpdateCtx) -> bool {
        if !self.every.due(ctx.now) {
            return false;
        }
        let utc = ctx.now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let local = utc + i64::from(self.offset.sample().map_or(0, |o| o.0));
        let today = Some(local.div_euclid(86_400));
        let changed = today != self.today;
        self.today = today;
        changed
    }

    fn on_input(&mut self, event: &InputEvent, bounds: Rect) -> bool {
        match *event {
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true } => {
                self.pressed = self.target_at(bounds, pos);
                false
            }
            // A click: released where it was pressed. A drag sends
            // PointerLeft first, so it never pages.
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false } => {
                let Some(target) = self.pressed.take().filter(|t| self.target_at(bounds, pos) == Some(*t)) else {
                    return false;
                };
                let shift = match target {
                    Target::Previous => self.shift - 1,
                    Target::Next => self.shift + 1,
                    Target::Today => 0,
                };
                let changed = shift != self.shift;
                self.shift = shift;
                changed
            }
            InputEvent::PointerLeft => {
                self.pressed = None;
                false
            }
            _ => false,
        }
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let compact = ctx.bounds.height + 0.5 < self.full_height();
        let area = card(ctx);
        let Some(today) = self.today else {
            header(ctx, area, "Calendar", "");
            return;
        };
        if compact {
            header(ctx, area, &format!("Week {}", iso_week(today)), &format_date(today * 86_400));
            return;
        }
        self.draw_title(ctx, area);
        let grid = Rect::new(area.x, area.y + title_height(), area.width, area.height - title_height());
        self.draw_grid(ctx, grid);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::testing::{drawn_text, rects_in};
    use sidebar_core::Theme;

    /// 2026-11-05, a Thursday.
    const NOV_5_2026: i64 = 20_762;

    fn calendar(monday_first: bool) -> Calendar {
        let mut c = Calendar::new(Box::new(|| Some(UtcOffset(-8 * 3600))), monday_first);
        // 07:00 UTC is still the 4th at UTC-8, so the 5th is 08:00 UTC.
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(NOV_5_2026 as u64 * 86_400 + 9 * 3600);
        assert!(c.update(&UpdateCtx { now }));
        c
    }

    fn click(c: &mut Calendar, pos: Point) -> bool {
        let bounds = Rect::new(0.0, 0.0, 200.0, c.full_height());
        let press = InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true };
        let release = InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false };
        c.on_input(&press, bounds) | c.on_input(&release, bounds)
    }

    #[test]
    fn date_math() {
        assert_eq!(days_from_civil(2026, 11, 5), NOV_5_2026);
        for d in [-800_000, -1, 0, 11_016, NOV_5_2026, 900_000] {
            let (y, m, n) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, n), d);
        }
        assert_eq!(iso_week(NOV_5_2026), 45);
        // 2021-01-01 (Friday) is in week 53 of 2020; 2024-12-30 in week 1 of 2025.
        assert_eq!(iso_week(days_from_civil(2021, 1, 1)), 53);
        assert_eq!(iso_week(days_from_civil(2024, 12, 30)), 1);
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 11, 14), (2028, 1));
    }

    #[test]
    fn sunday_first_grid() {
        let c = calendar(false);
        let text = drawn_text(&c, 200.0, c.full_height());
        assert_eq!(&text[..12], ["November 2026", "‹", "›", "W", "S", "M", "T", "W", "T", "F", "S", "45"]);
        // Nov 2026 starts on a Sunday; six rows run into December.
        assert_eq!(&text[12..19], ["1", "2", "3", "4", "5", "6", "7"]);
        assert_eq!(text.last().map(String::as_str), Some("12"));
        // Today's pill.
        assert_eq!(rects_in(&c, 200.0, c.full_height(), Theme::default().accent.with_alpha(255)).len(), 1);
        assert_eq!(drawn_text(&c, 160.0, 40.0), ["Week 45", "Thu 5 Nov"]);
    }

    #[test]
    fn monday_first_grid() {
        let c = calendar(true);
        let text = drawn_text(&c, 200.0, c.full_height());
        assert_eq!(&text[4..11], ["M", "T", "W", "T", "F", "S", "S"]);
        // Week 44 starts on Monday 26 October.
        assert_eq!(&text[11..19], ["44", "26", "27", "28", "29", "30", "31", "1"]);
    }

    #[test]
    fn clicks_page_months() {
        let mut c = calendar(false);
        let title_y = CARD_PAD + 4.0;
        assert!(click(&mut c, Point::new(190.0, title_y)));
        assert!(click(&mut c, Point::new(190.0, title_y)));
        assert_eq!(drawn_text(&c, 200.0, c.full_height())[0], "January 2027");
        assert!(click(&mut c, Point::new(10.0, title_y)));
        assert_eq!(drawn_text(&c, 200.0, c.full_height())[0], "December 2026");
        assert!(click(&mut c, Point::new(100.0, title_y)));
        assert_eq!(c.shift, 0);
        assert!(!click(&mut c, Point::new(100.0, title_y)), "already on today's month");
        assert!(!click(&mut c, Point::new(100.0, 100.0)), "grid clicks do nothing");
    }
}
