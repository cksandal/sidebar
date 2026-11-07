use sidebar_core::source::Weather;
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, MeasureCtx, Orientation, Point, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, DETAIL_SIZE, GAP, ROW_SIZE, SECTION_GAP};
use crate::{CARD_PAD, Every, SAMPLE_INTERVAL, card, centered, header, icons, metric_extent};

const TEMP_SIZE: f32 = 26.0;
const BIG_ICON: f32 = 34.0;
const HOUR_ICON: f32 = 16.0;
const DAY_ICON: f32 = 13.0;
/// Most columns in the hourly strip (fewer when narrow) and rows in the
/// daily list; the layout keeps room for all rows so the widget does not
/// change size.
const HOURS: usize = 5;
const DAYS: usize = 6;
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// "17°", without "-0°".
fn degrees(t: f32) -> String {
    format!("{:.0}°", t.round() + 0.0)
}

fn day_row_height() -> f32 {
    line_height(ROW_SIZE) + 4.0
}

fn hours_height() -> f32 {
    line_height(DETAIL_SIZE) + 2.0 + HOUR_ICON + 2.0 + line_height(ROW_SIZE)
}

/// Current conditions for a place, the next hours, and the coming days,
/// laid out like the other cards. Shows the place and temperature in a
/// header line on horizontal docks.
pub struct WeatherCard {
    source: Box<dyn Source<Result<Weather, String>>>,
    every: Every,
    report: Option<Result<Weather, String>>,
}

impl WeatherCard {
    pub fn new(source: Box<dyn Source<Result<Weather, String>>>) -> Self {
        Self { source, every: Every::new(SAMPLE_INTERVAL), report: None }
    }

    fn full_height(&self) -> f32 {
        let header = line_height(crate::HEADER_SIZE) + GAP;
        let now = line_height(TEMP_SIZE).max(BIG_ICON) + SECTION_GAP;
        let hours = hours_height() + 2.0 * SECTION_GAP + 1.0;
        header + now + hours + DAYS as f32 * day_row_height() + 2.0 * CARD_PAD
    }

    fn draw_now(&self, ctx: &mut DrawCtx<'_>, area: Rect, w: &Weather) -> f32 {
        let h = line_height(TEMP_SIZE).max(BIG_ICON);
        let temp = format!("{}{}", (w.temperature.round() + 0.0), w.unit);
        let th = line_height(TEMP_SIZE);
        ctx.scene.text(Point::new(area.x, (area.y + (h - th) / 2.0).round()), temp, TEMP_SIZE, ctx.theme.foreground);
        let icon = Rect::new(area.right() - BIG_ICON, area.y + (h - BIG_ICON) / 2.0, BIG_ICON, BIG_ICON);
        icons::weather(ctx, icon, w.sky, w.daylight);
        area.y + h + SECTION_GAP
    }

    /// Time, icon and temperature for each upcoming hour, in columns.
    fn draw_hours(&self, ctx: &mut DrawCtx<'_>, area: Rect, w: &Weather) -> f32 {
        let min_col = ctx.text.measure("00:00", DETAIL_SIZE).width + 2.0 * GAP;
        let columns = ((area.width / min_col) as usize).clamp(1, HOURS);
        let col = area.width / columns as f32;
        let fg = ctx.theme.foreground;
        for (i, hour) in w.hours.iter().take(columns).enumerate() {
            let x = area.x + i as f32 * col;
            let mut y = area.y;
            let time = format!("{:02}:00", hour.hour);
            let at = centered(ctx.text, &time, DETAIL_SIZE, Rect::new(x, y, col, line_height(DETAIL_SIZE)));
            ctx.scene.text(at, time, DETAIL_SIZE, fg.with_alpha(170));
            y += line_height(DETAIL_SIZE) + 2.0;
            icons::weather(ctx, Rect::new((x + (col - HOUR_ICON) / 2.0).round(), y, HOUR_ICON, HOUR_ICON), hour.sky, hour.daylight);
            y += HOUR_ICON + 2.0;
            let temp = degrees(hour.temperature);
            let at = centered(ctx.text, &temp, ROW_SIZE, Rect::new(x, y, col, line_height(ROW_SIZE)));
            ctx.scene.text(at, temp, ROW_SIZE, fg);
        }
        area.y + hours_height()
    }

    /// Day name, icon, high and (dimmer) low, one row per day.
    fn draw_days(&self, ctx: &mut DrawCtx<'_>, area: Rect, w: &Weather) {
        let fg = ctx.theme.foreground;
        let name_w = WEEKDAYS.iter().map(|d| ctx.text.measure(d, ROW_SIZE).width).fold(0.0, f32::max);
        let temp_w = ctx.text.measure("-00°", ROW_SIZE).width;
        let lh = line_height(ROW_SIZE);
        for (i, day) in w.days.iter().take(DAYS).enumerate() {
            let y = area.y + i as f32 * day_row_height();
            let name = if i == 0 { "Today" } else { WEEKDAYS[day.weekday as usize % 7] };
            ctx.scene.text(Point::new(area.x, y), name, ROW_SIZE, fg);
            let icon = Rect::new(area.x + name_w + GAP, (y + (lh - DAY_ICON) / 2.0).round(), DAY_ICON, DAY_ICON);
            icons::weather(ctx, icon, day.sky, true);
            for (text, right, color) in [
                (degrees(day.high), area.right() - temp_w - GAP, fg),
                (degrees(day.low), area.right(), fg.with_alpha(140)),
            ] {
                let tw = ctx.text.measure(&text, ROW_SIZE).width;
                ctx.scene.text(Point::new((right - tw).round(), y), text, ROW_SIZE, color);
            }
        }
    }
}

impl Widget for WeatherCard {
    fn name(&self) -> &str {
        "weather"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        match orientation {
            Orientation::Vertical => self.full_height(),
            Orientation::Horizontal => metric_extent(orientation, BIG_ICON),
        }
    }

    fn update(&mut self, ctx: &UpdateCtx) -> bool {
        if !self.every.due(ctx.now) {
            return false;
        }
        let report = self.source.sample();
        let changed = report != self.report;
        self.report = report;
        changed
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let compact = ctx.bounds.height + 0.5 < self.full_height();
        let area = card(ctx);
        let w = match &self.report {
            Some(Ok(w)) => w,
            other => {
                let message = match other {
                    Some(Err(e)) => e.as_str(),
                    _ => "Loading…",
                };
                let rest = header(ctx, area, "Weather", "");
                let text = meter::fit(ctx.text, message, DETAIL_SIZE, rest.width);
                let dim = ctx.theme.foreground.with_alpha(170);
                ctx.scene.text(rest.origin(), text, DETAIL_SIZE, dim);
                return;
            }
        };
        if compact {
            let temp = format!("{}{}", (w.temperature.round() + 0.0), w.unit);
            let rest = header(ctx, area, &w.place, &temp);
            let size = rest.height.min(BIG_ICON);
            icons::weather(ctx, Rect::new(rest.x, rest.y, size, size), w.sky, w.daylight);
            let text = Point::new(rest.x + size + GAP, (rest.y + (size - line_height(DETAIL_SIZE)) / 2.0).round());
            let dim = ctx.theme.foreground.with_alpha(170);
            ctx.scene.text(text, w.sky.describe(), DETAIL_SIZE, dim);
            return;
        }
        let place = meter::fit(ctx.text, &w.place, crate::HEADER_SIZE, area.width * 0.55);
        let rest = header(ctx, area, &place, w.sky.describe());
        let at = |y| Rect::new(rest.x, y, rest.width, 0.0);
        let mut y = self.draw_now(ctx, at(rest.y), w);
        y = self.draw_hours(ctx, at(y), w) + SECTION_GAP;
        ctx.scene.fill_rect(Rect::new(rest.x, y, rest.width, 1.0), ctx.theme.foreground.with_alpha(30), 0.0);
        y += 1.0 + SECTION_GAP;
        self.draw_days(ctx, at(y), w);
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::testing::drawn_text;
    use sidebar_core::source::{Sky, WeatherDay, WeatherHour};

    fn weather() -> Weather {
        let hour = |hour, temperature, sky| WeatherHour { hour, temperature, sky, daylight: true };
        let day = |weekday, sky, high, low| WeatherDay { weekday, sky, high, low };
        Weather {
            place: "New York".into(),
            unit: "°C",
            temperature: 16.7,
            sky: Sky::Cloudy,
            daylight: true,
            hours: vec![hour(6, 17.0, Sky::PartlyCloudy), hour(9, 15.6, Sky::Rain), hour(12, -0.2, Sky::Snow)],
            days: vec![day(2, Sky::PartlyCloudy, 17.2, 15.0), day(3, Sky::Rain, 23.0, 15.4), day(4, Sky::Thunder, 22.6, 14.6)],
        }
    }

    #[test]
    fn shows_now_hours_and_days() {
        let mut card = WeatherCard::new(Box::new(|| Some(Ok(weather()))));
        assert!(card.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH }));
        let text = drawn_text(&card, 148.0, card.full_height());
        assert_eq!(
            text,
            [
                "New York", "Cloudy", "17°C",
                "06:00", "17°", "09:00", "16°", "12:00", "0°",
                "Today", "17°", "15°", "Thursday", "23°", "15°", "Friday", "23°", "15°",
            ]
        );
    }

    #[test]
    fn loading_errors_and_compact() {
        let mut reports = [None, Some(Err("place not found: Atlantis".to_string())), Some(Ok(weather()))].into_iter();
        let mut card = WeatherCard::new(Box::new(move || reports.next().flatten()));
        let h = card.full_height();
        let t = |s| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s);
        assert!(!card.update(&UpdateCtx { now: t(0) }));
        assert_eq!(drawn_text(&card, 148.0, h), ["Weather", "", "Loading…"]);
        assert!(card.update(&UpdateCtx { now: t(1) }));
        assert_eq!(drawn_text(&card, 400.0, h)[2], "place not found: Atlantis");
        assert!(card.update(&UpdateCtx { now: t(2) }));
        assert_eq!(drawn_text(&card, 120.0, 40.0), ["New York", "17°C", "Cloudy"]);
    }
}
