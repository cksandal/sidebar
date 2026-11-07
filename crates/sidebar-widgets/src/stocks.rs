use sidebar_core::source::Quote;
use sidebar_core::text::line_height;
use sidebar_core::{Color, DrawCtx, MeasureCtx, Orientation, Point, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, DETAIL_SIZE, GAP};
use crate::{CARD_PAD, Every, SAMPLE_INTERVAL, card, header, metric_extent};

const SYMBOL_SIZE: f32 = 11.0;
const ROW_PAD: f32 = 3.0;

fn row_height() -> f32 {
    line_height(SYMBOL_SIZE) + line_height(DETAIL_SIZE) + 2.0 * ROW_PAD
}

fn up_color() -> Color {
    meter::color(2)
}

fn down_color() -> Color {
    meter::color(6)
}

/// Green when up, red when down, dim when flat.
fn trend_color(q: &Quote, flat: Color) -> Color {
    match q.change() {
        c if c > 0.0 => up_color(),
        c if c < 0.0 => down_color(),
        _ => flat,
    }
}

/// "$54.50", "€12.30", or "1234.50 GBp" for other currencies.
fn price(q: &Quote) -> String {
    match q.currency.as_str() {
        "USD" => format!("${:.2}", q.price),
        "EUR" => format!("€{:.2}", q.price),
        "GBP" => format!("£{:.2}", q.price),
        "JPY" => format!("¥{:.0}", q.price),
        other => format!("{:.2} {other}", q.price),
    }
}

fn signed(v: f64, suffix: &str) -> String {
    format!("{}{v:.2}{suffix}", if v > 0.0 { "+" } else { "" })
}

/// One row per symbol: the symbol over its percent change, today's price
/// line against the previous close, and the price over its change; green
/// when up, red when down. Rows keep their order and place while loading
/// or when a symbol fails.
pub struct StockTicker {
    source: Box<dyn Source<Result<Vec<Quote>, String>>>,
    every: Every,
    symbols: Vec<String>,
    report: Option<Result<Vec<Quote>, String>>,
}

impl StockTicker {
    pub fn new(symbols: Vec<String>, source: Box<dyn Source<Result<Vec<Quote>, String>>>) -> Self {
        Self { source, every: Every::new(SAMPLE_INTERVAL), symbols, report: None }
    }

    fn full_height(&self) -> f32 {
        line_height(crate::HEADER_SIZE) + GAP + self.symbols.len().max(1) as f32 * row_height() + 2.0 * CARD_PAD
    }

    fn quote(&self, symbol: &str) -> Option<&Quote> {
        self.report.as_ref()?.as_ref().ok()?.iter().find(|q| q.symbol == symbol)
    }

    fn quotes(&self) -> impl Iterator<Item = &Quote> {
        self.symbols.iter().filter_map(|s| self.quote(s))
    }

    /// "open", "closed", or the reason nothing has loaded.
    fn status(&self) -> String {
        match &self.report {
            None => "loading…".into(),
            Some(Err(_)) => "offline".into(),
            Some(Ok(_)) if self.quotes().any(|q| q.market_open) => "open".into(),
            Some(Ok(_)) => "closed".into(),
        }
    }

    fn draw_row(&self, ctx: &mut DrawCtx<'_>, row: Rect, symbol: &str, columns: (f32, f32), shaded: bool) {
        let fg = ctx.theme.foreground;
        let dim = fg.with_alpha(140);
        if shaded {
            ctx.scene.fill_rect(row, fg.with_alpha(10), 3.0);
        }
        let (left_w, right_w) = columns;
        let inner = Rect::new(row.x + 4.0, row.y + ROW_PAD, row.width - 8.0, row.height - 2.0 * ROW_PAD);
        let lower = inner.y + line_height(SYMBOL_SIZE);
        ctx.scene.text(inner.origin(), symbol.to_string(), SYMBOL_SIZE, fg);
        let right = |ctx: &mut DrawCtx<'_>, y: f32, text: String, size: f32, color: Color| {
            let w = ctx.text.measure(&text, size).width;
            ctx.scene.text(Point::new((inner.right() - w).round(), y), text, size, color);
        };
        let Some(q) = self.quote(symbol) else {
            right(ctx, inner.y, "n/a".into(), SYMBOL_SIZE, dim);
            return;
        };
        let color = trend_color(q, dim);
        ctx.scene.text(Point::new(inner.x, lower), signed(q.change_percent(), "%"), DETAIL_SIZE, color);
        right(ctx, inner.y, price(q), SYMBOL_SIZE, fg);
        right(ctx, lower, signed(q.change(), ""), DETAIL_SIZE, color);

        let chart = Rect::new(inner.x + left_w + 2.0 * GAP, inner.y + 2.0, inner.width - left_w - right_w - 4.0 * GAP, inner.height - 4.0);
        if chart.width > 8.0 && !q.intraday.is_empty() {
            // Scaled to the day's range, including the previous close.
            let (lo, hi) = q.intraday.iter().fold((q.previous_close, q.previous_close), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
            let span = (hi - lo).max(f64::EPSILON);
            let fraction = |p: f64| ((p - lo) / span) as f32;
            let base = (chart.bottom() - fraction(q.previous_close) * chart.height).round();
            ctx.scene.fill_rect(Rect::new(chart.x, base, chart.width, 1.0), fg.with_alpha(40), 0.0);
            let values: Vec<f32> = q.intraday.iter().map(|p| fraction(*p)).collect();
            meter::sparkline(ctx, chart, &values, q.session_points, color);
        }
    }
}

impl Widget for StockTicker {
    fn name(&self) -> &str {
        "stocks"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        match orientation {
            Orientation::Vertical => self.full_height(),
            Orientation::Horizontal => metric_extent(orientation, row_height()),
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
        if compact {
            // Counts of risers and fallers.
            let (up, down) = self.quotes().fold((0, 0), |(u, d), q| (u + usize::from(q.change() > 0.0), d + usize::from(q.change() < 0.0)));
            let value = if self.quotes().next().is_some() { format!("▲{up} ▼{down}") } else { self.status() };
            header(ctx, area, "STOCKS", &value);
            return;
        }
        let rest = header(ctx, area, "STOCKS", &self.status());
        if let Some(Err(e)) = &self.report {
            let text = meter::fit(ctx.text, e, DETAIL_SIZE, rest.width);
            ctx.scene.text(rest.origin(), text, DETAIL_SIZE, ctx.theme.foreground.with_alpha(170));
            return;
        }
        // Column widths from the widest texts, so the charts line up.
        let measure = |text: &str, size| ctx.text.measure(text, size).width;
        let left_w = self
            .symbols
            .iter()
            .map(|s| measure(s, SYMBOL_SIZE))
            .chain(self.quotes().map(|q| measure(&signed(q.change_percent(), "%"), DETAIL_SIZE)))
            .fold(0.0, f32::max);
        let right_w = self
            .quotes()
            .flat_map(|q| [measure(&price(q), SYMBOL_SIZE), measure(&signed(q.change(), ""), DETAIL_SIZE)])
            .fold(measure("n/a", SYMBOL_SIZE), f32::max);
        for (i, symbol) in self.symbols.iter().enumerate() {
            let row = Rect::new(rest.x, rest.y + i as f32 * row_height(), rest.width, row_height() - 1.0);
            self.draw_row(ctx, row, symbol, (left_w, right_w), i % 2 == 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::testing::{drawn_text, rects_in};

    fn quote(symbol: &str, price: f64, previous_close: f64) -> Quote {
        Quote {
            symbol: symbol.into(),
            currency: "USD".into(),
            price,
            previous_close,
            intraday: vec![previous_close, (price + previous_close) / 2.0, price],
            session_points: 78,
            market_open: true,
        }
    }

    #[test]
    fn rows_in_configured_order() {
        let quotes = vec![quote("MSFT", 17.105, 16.98), quote("AAPL", 54.65, 58.27)];
        let symbols = vec!["AAPL".to_string(), "NOPE".to_string(), "MSFT".to_string()];
        let mut reports = [None, Some(Ok(quotes))].into_iter();
        let mut ticker = StockTicker::new(symbols, Box::new(move || reports.next().flatten()));
        let h = ticker.full_height();
        let t = |s| SystemTime::UNIX_EPOCH + Duration::from_secs(s);

        assert!(!ticker.update(&UpdateCtx { now: t(0) }));
        assert_eq!(drawn_text(&ticker, 200.0, h), ["STOCKS", "loading…", "AAPL", "n/a", "NOPE", "n/a", "MSFT", "n/a"]);

        assert!(ticker.update(&UpdateCtx { now: t(1) }));
        assert_eq!(
            drawn_text(&ticker, 200.0, h),
            ["STOCKS", "open", "AAPL", "-6.21%", "$54.65", "-3.62", "NOPE", "n/a", "MSFT", "+0.74%", "$17.11", "+0.12"]
        );
        // A chart per loaded symbol, in its trend color.
        assert!(!rects_in(&ticker, 200.0, h, down_color()).is_empty());
        assert!(!rects_in(&ticker, 200.0, h, up_color()).is_empty());
        assert_eq!(drawn_text(&ticker, 120.0, 40.0), ["STOCKS", "▲1 ▼1"]);
    }

    #[test]
    fn errors_and_currencies() {
        let mut ticker = StockTicker::new(vec!["X".into()], Box::new(|| Some(Err("X: request failed".to_string()))));
        ticker.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH });
        assert_eq!(drawn_text(&ticker, 200.0, ticker.full_height()), ["STOCKS", "offline", "X: request failed"]);

        let q = |currency: &str| Quote { currency: currency.into(), ..quote("X", 1234.5, 1200.0) };
        assert_eq!(price(&q("EUR")), "€1234.50");
        assert_eq!(price(&q("JPY")), "¥1234");
        assert_eq!(price(&q("GBp")), "1234.50 GBp");
        assert_eq!(signed(0.0, "%"), "0.00%");
    }
}
