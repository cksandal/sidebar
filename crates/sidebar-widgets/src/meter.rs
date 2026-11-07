//! Building blocks shared by the detailed hardware widgets (CPU, GPU), so
//! they look alike: detail lines, titled sections, bar rows, a memory
//! table, and history charts drawn as step lines.

use sidebar_core::text::line_height;
use sidebar_core::{Color, DrawCtx, Point, Rect, TextMeasure};

use crate::centered;

/// Samples kept for the charts (one per second).
pub(crate) const HISTORY: usize = 60;
pub(crate) const GRAPH_HEIGHT: f32 = 29.0;
pub(crate) const TEMP_HEIGHT: f32 = 29.0;
/// Graph height in the compact layout's size request.
pub(crate) const COMPACT_GRAPH: f32 = 28.0;
pub(crate) const DETAIL_SIZE: f32 = 10.0;
pub(crate) const ROW_SIZE: f32 = 10.0;
pub(crate) const ROW_GAP: f32 = 1.0;
pub(crate) const GAP: f32 = 4.0;
pub(crate) const SECTION_GAP: f32 = 8.0;
const BAR: f32 = 4.0;
const LINE: f32 = 1.5;
/// Temperature chart range, °C.
const TEMP_MIN: f32 = 20.0;
const TEMP_MAX: f32 = 100.0;
const GIB: f64 = (1u64 << 30) as f64;

/// Distinct series colors, repeating when there are more series.
const PALETTE: [Color; 8] = [
    Color::rgb(97, 175, 239),
    Color::rgb(255, 159, 64),
    Color::rgb(98, 214, 112),
    Color::rgb(240, 214, 80),
    Color::rgb(224, 98, 208),
    Color::rgb(80, 214, 214),
    Color::rgb(255, 99, 99),
    Color::rgb(168, 140, 255),
];

pub(crate) fn color(i: usize) -> Color {
    PALETTE[i % PALETTE.len()]
}

/// The color series lines are drawn in.
pub(crate) fn line_color(i: usize) -> Color {
    color(i).with_alpha(210)
}

/// Green while cool, orange when warm, red when hot.
pub(crate) fn temp_color(celsius: f32) -> Color {
    match celsius {
        c if c < 70.0 => PALETTE[2],
        c if c < 85.0 => PALETTE[1],
        _ => PALETTE[6],
    }
}

/// Color of the area under a load chart.
pub(crate) fn area_color(accent: Color) -> Color {
    accent.with_alpha(60)
}

pub(crate) fn row_height() -> f32 {
    line_height(ROW_SIZE) + ROW_GAP
}

pub(crate) fn title_height() -> f32 {
    line_height(DETAIL_SIZE) + ROW_GAP
}

/// Height of [`details`] with `lines` lines.
pub(crate) fn details_height(lines: usize) -> f32 {
    lines as f32 * line_height(DETAIL_SIZE) + GAP
}

/// Height of a titled chart.
pub(crate) fn chart_height(graph: f32) -> f32 {
    title_height() + graph
}

/// "5.5G", or "26G" once there is no room for a decimal.
pub(crate) fn format_gib(bytes: u64) -> String {
    let gib = bytes as f64 / GIB;
    if gib < 10.0 { format!("{gib:.1}G") } else { format!("{gib:.0}G") }
}

/// Bytes with binary units, one decimal below 10: "980B", "2.9G", "16G".
pub(crate) fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    // Switch units before the number needs four digits.
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit > 0 && value < 10.0 { format!("{value:.1}{}", UNITS[unit]) } else { format!("{value:.0}{}", UNITS[unit]) }
}

pub(crate) fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

pub(crate) fn format_celsius(celsius: Option<f32>) -> String {
    celsius.map_or("n/a".into(), |c| format!("{c:.0}°C"))
}

pub(crate) fn percent(fraction: Option<f32>) -> String {
    fraction.map_or("-".into(), |f| format!("{:.0}%", f * 100.0))
}

/// `text`, shortened with an ellipsis if it is wider than `width`.
pub(crate) fn fit(measure: &dyn TextMeasure, text: &str, size: f32, width: f32) -> String {
    if measure.measure(text, size).width <= width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate = format!("{}…", chars.iter().collect::<String>().trim_end());
        if measure.measure(&candidate, size).width <= width {
            return candidate;
        }
    }
    String::new()
}

/// Centered lines under the header (clock, model name); returns the y
/// below them. Lines too wide for `area` are shortened.
pub(crate) fn details(ctx: &mut DrawCtx<'_>, area: Rect, lines: Vec<(String, Color)>) -> f32 {
    let h = line_height(DETAIL_SIZE);
    let count = lines.len();
    for (i, (text, color)) in lines.into_iter().enumerate() {
        let text = fit(ctx.text, &text, DETAIL_SIZE, area.width);
        let at = centered(ctx.text, &text, DETAIL_SIZE, Rect::new(area.x, area.y + i as f32 * h, area.width, h));
        ctx.scene.text(at, text, DETAIL_SIZE, color);
    }
    area.y + details_height(count)
}

/// A dim title on the left and a value on the right (shortened if it
/// does not fit beside the title); returns the y below the line.
pub(crate) fn title(ctx: &mut DrawCtx<'_>, area: Rect, title: &str, value: String, color: Color) -> f32 {
    let fg = ctx.theme.foreground;
    ctx.scene.text(area.origin(), title.to_string(), DETAIL_SIZE, fg.with_alpha(170));
    let room = area.width - ctx.text.measure(title, DETAIL_SIZE).width - GAP;
    let value = fit(ctx.text, &value, DETAIL_SIZE, room);
    let w = ctx.text.measure(&value, DETAIL_SIZE).width;
    ctx.scene.text(Point::new((area.right() - w).round(), area.y), value, DETAIL_SIZE, color);
    area.y + title_height()
}

/// `label`, a bar filled to `fraction`, and `value`, all in `color`. The
/// bar starts `label_w` px in and leaves room for a "100%"-wide value.
pub(crate) fn bar_row(ctx: &mut DrawCtx<'_>, cell: Rect, label_w: f32, label: String, fraction: Option<f32>, value: String, color: Color) {
    let value_w = ctx.text.measure("100%", ROW_SIZE).width;
    ctx.scene.text(cell.origin(), label, ROW_SIZE, color);
    let w = ctx.text.measure(&value, ROW_SIZE).width;
    ctx.scene.text(Point::new((cell.right() - w).round(), cell.y), value, ROW_SIZE, color);

    let bar_x = cell.x + label_w + GAP;
    let bar = Rect::new(bar_x, (cell.y + (cell.height - BAR) / 2.0).round(), (cell.right() - value_w - GAP - bar_x).max(0.0), BAR);
    ctx.scene.fill_rect(bar, ctx.theme.foreground.with_alpha(40), BAR / 2.0);
    let fill = bar.width * fraction.unwrap_or(0.0).clamp(0.0, 1.0);
    if fill > 0.0 {
        ctx.scene.fill_rect(Rect { width: fill, ..bar }, color, BAR / 2.0);
    }
}

/// The row rect for [`bar_row`] at `y`.
pub(crate) fn row(area: Rect, y: f32) -> Rect {
    Rect::new(area.x, y, area.width, line_height(ROW_SIZE))
}

/// Height of [`table`] with `rows` rows.
pub(crate) fn table_height(rows: usize) -> f32 {
    title_height() + rows as f32 * row_height()
}

/// A table with a dim title row (`title`, then `columns`) and rows of a
/// colored label and right-aligned values. `label_w` is the label
/// column's width. Returns the y below it.
pub(crate) fn table(ctx: &mut DrawCtx<'_>, area: Rect, label_w: f32, title: &str, columns: [&str; 3], rows: &[(String, Color, [String; 3])]) -> f32 {
    let dim = ctx.theme.foreground.with_alpha(170);
    let col_w = (area.width - label_w - GAP) / 3.0;
    // Right-aligned in column `col`.
    let cell = |ctx: &mut DrawCtx<'_>, col: usize, y: f32, text: String, size: f32, color: Color| {
        let w = ctx.text.measure(&text, size).width;
        let right = area.x + label_w + GAP + (col + 1) as f32 * col_w;
        ctx.scene.text(Point::new((right - w).round(), y), text, size, color);
    };

    let mut y = area.y;
    ctx.scene.text(Point::new(area.x, y), title.to_string(), DETAIL_SIZE, dim);
    for (col, title) in columns.into_iter().enumerate() {
        cell(ctx, col, y, title.into(), DETAIL_SIZE, dim);
    }
    y += title_height();
    let fg = ctx.theme.foreground;
    for (label, color, values) in rows {
        ctx.scene.text(Point::new(area.x, y), label.clone(), ROW_SIZE, *color);
        for (col, text) in values.iter().enumerate() {
            cell(ctx, col, y, text.clone(), ROW_SIZE, fg);
        }
        y += row_height();
    }
    y
}

/// A Used / Free / Total [`table`]; each row is a name and (used, total)
/// bytes, if known.
pub(crate) fn memory_table(ctx: &mut DrawCtx<'_>, area: Rect, label_w: f32, rows: &[(&str, Option<(u64, u64)>)]) -> f32 {
    let accent = ctx.theme.accent;
    let rows: Vec<_> = rows
        .iter()
        .map(|(name, values)| {
            let values = values.map_or(["-".into(), "-".into(), "-".into()], |(used, total)| {
                [format_gib(used), format_gib(total.saturating_sub(used)), format_gib(total)]
            });
            (name.to_string(), accent, values)
        })
        .collect();
    table(ctx, area, label_w, "Mem", ["Used", "Free", "Total"], &rows)
}

/// Chart background with horizontal grid lines at `grid` (fractions of
/// the height, from the bottom).
fn chart_background(ctx: &mut DrawCtx<'_>, graph: Rect, grid: &[f32]) {
    let fg = ctx.theme.foreground;
    ctx.scene.fill_rect(graph, fg.with_alpha(20), 2.0);
    for f in grid {
        let y = (graph.bottom() - graph.height * f).round();
        ctx.scene.fill_rect(Rect::new(graph.x, y, graph.width, 1.0), fg.with_alpha(18), 0.0);
    }
}

/// Maps sample `i` and a fraction to chart coordinates, for a chart with
/// room for `slots` samples, the first drawn in slot `start`.
struct Plot {
    graph: Rect,
    slots: usize,
    start: usize,
}

impl Plot {
    /// The last [`HISTORY`] of `len` samples, newest at the right edge.
    fn history(graph: Rect, len: usize) -> Self {
        Self { graph, slots: HISTORY, start: HISTORY - len.min(HISTORY) }
    }

    fn step(&self) -> f32 {
        self.graph.width / self.slots.max(1) as f32
    }

    fn x(&self, i: usize) -> f32 {
        self.graph.x + (self.start + i) as f32 * self.step()
    }

    fn y(&self, fraction: f32) -> f32 {
        self.graph.bottom() - fraction.clamp(0.0, 1.0) * (self.graph.height - LINE) - LINE
    }

    /// Columns from the bottom up to each value.
    fn area(&self, ctx: &mut DrawCtx<'_>, values: &[Option<f32>], color: impl Fn(usize) -> Color) {
        for (i, v) in values.iter().enumerate() {
            if let Some(v) = v {
                let top = self.y(*v);
                ctx.scene.fill_rect(Rect::new(self.x(i), top, self.step(), self.graph.bottom() - top), color(i), 0.0);
            }
        }
    }

    /// A step line: for each sample a vertical joint from the previous
    /// level, then a horizontal run. Gaps (`None`) break the line.
    fn line(&self, ctx: &mut DrawCtx<'_>, values: &[Option<f32>], color: impl Fn(usize) -> Color) {
        let mut prev: Option<f32> = None;
        for (i, v) in values.iter().enumerate() {
            let Some(v) = v else {
                prev = None;
                continue;
            };
            let cur = self.y(*v);
            if let Some(p) = prev {
                ctx.scene.fill_rect(Rect::new(self.x(i), p.min(cur), LINE, (p - cur).abs() + LINE), color(i), 0.0);
            }
            ctx.scene.fill_rect(Rect::new(self.x(i), cur, self.step(), LINE), color(i), 0.0);
            prev = Some(cur);
        }
    }
}

/// `area` (fractions) as a filled area with each of `lines` as a colored
/// step line on top, over the last [`HISTORY`] samples.
pub(crate) fn load_chart(ctx: &mut DrawCtx<'_>, graph: Rect, area: &[Option<f32>], lines: &[(Color, Vec<Option<f32>>)]) {
    chart_background(ctx, graph, &[0.25, 0.5, 0.75]);
    let plot = Plot::history(graph, area.len());
    let fill = area_color(ctx.theme.accent);
    plot.area(ctx, area, |_| fill);
    for (color, values) in lines {
        let plot = Plot::history(graph, values.len());
        plot.line(ctx, values, |_| *color);
    }
}

/// Temperatures (°C) as a line over a faint fill, colored by how hot each
/// sample was, on a fixed [`TEMP_MIN`]..[`TEMP_MAX`] scale.
pub(crate) fn temperature_chart(ctx: &mut DrawCtx<'_>, graph: Rect, celsius: &[Option<f32>]) {
    let scale = |c: f32| (c - TEMP_MIN) / (TEMP_MAX - TEMP_MIN);
    // Grid lines every 20 °C.
    chart_background(ctx, graph, &[scale(40.0), scale(60.0), scale(80.0)]);
    let fractions: Vec<Option<f32>> = celsius.iter().map(|c| c.map(scale)).collect();
    let color = |i: usize| temp_color(celsius[i].unwrap_or(TEMP_MIN));
    let plot = Plot::history(graph, celsius.len());
    plot.area(ctx, &fractions, |i| color(i).with_alpha(50));
    plot.line(ctx, &fractions, color);
}

/// `values` (fractions) as a step line over a faint fill, from the left
/// edge, in a chart with room for `slots` values (so a partial series is
/// drawn to scale). No background, for use inside other rows.
pub(crate) fn sparkline(ctx: &mut DrawCtx<'_>, graph: Rect, values: &[f32], slots: usize, color: Color) {
    let plot = Plot { graph, slots: slots.max(values.len()), start: 0 };
    let values: Vec<Option<f32>> = values.iter().map(|v| Some(*v)).collect();
    plot.area(ctx, &values, |_| color.with_alpha(40));
    plot.line(ctx, &values, |_| color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sidebar_core::ApproxText;

    #[test]
    fn formats_values() {
        assert_eq!(format_gib(5 * (1 << 30) + (1 << 29)), "5.5G");
        assert_eq!(format_gib(26 << 30), "26G");
        assert_eq!(format_celsius(Some(61.6)), "62°C");
        assert_eq!(percent(Some(0.344)), "34%");
        assert_eq!(percent(None), "-");
        assert_eq!(format_bytes(980.0), "980B");
        assert_eq!(format_bytes(1000.0), "1.0K");
        assert_eq!(format_bytes(3.1e9), "2.9G");
        assert_eq!(format_bytes(16.0 * 1024.0 * 1024.0 * 1024.0), "16G");
        assert_eq!(format_rate(50_484.0), "49K/s");
    }

    #[test]
    fn fits_text() {
        // ApproxText: 6px per char at size 10.
        assert_eq!(fit(&ApproxText, "abcdefgh", 10.0, 48.0), "abcdefgh");
        assert_eq!(fit(&ApproxText, "abcdefgh", 10.0, 31.0), "abcd…");
    }
}
