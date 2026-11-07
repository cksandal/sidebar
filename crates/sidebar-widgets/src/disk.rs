use std::collections::VecDeque;

use sidebar_core::source::DiskSample;
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, MeasureCtx, Orientation, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, COMPACT_GRAPH, GAP, GRAPH_HEIGHT, HISTORY, ROW_SIZE, SECTION_GAP};
use crate::{CARD_PAD, Every, SAMPLE_INTERVAL, card, header, metric_extent};

/// Volumes shown; more are left out (largest first is the system's order).
const MAX_VOLUMES: usize = 6;
/// Smallest full scale of the activity chart, so an idle disk reads as idle.
const MIN_SCALE: f64 = 1_000_000.0;

/// Space on each filesystem or drive (a Used / Free / Total table and a
/// bar each), and a chart of read and write rates. Compact docks show the
/// rates and the chart only.
pub struct DiskGraph {
    source: Box<dyn Source<DiskSample>>,
    every: Every,
    last: Option<DiskSample>,
    /// (read, write) rates, oldest first.
    history: VecDeque<(Option<f64>, Option<f64>)>,
}

impl DiskGraph {
    pub fn new(source: Box<dyn Source<DiskSample>>) -> Self {
        Self { source, every: Every::new(SAMPLE_INTERVAL), last: None, history: VecDeque::with_capacity(HISTORY) }
    }

    fn volumes(&self) -> usize {
        self.last.as_ref().map_or(0, |s| s.volumes.len().min(MAX_VOLUMES))
    }

    fn full_height(&self) -> f32 {
        let header = line_height(crate::HEADER_SIZE) + GAP;
        let n = self.volumes();
        let space = if n > 0 { meter::table_height(n) + SECTION_GAP + n as f32 * meter::row_height() + SECTION_GAP } else { 0.0 };
        header + space + meter::chart_height(GRAPH_HEIGHT) + 2.0 * CARD_PAD
    }

    fn peak(&self) -> f64 {
        self.history.iter().flat_map(|(r, w)| [*r, *w]).flatten().fold(0.0, f64::max)
    }

    /// Reads as an area with a line, writes as a line.
    fn draw_activity(&self, ctx: &mut DrawCtx<'_>, graph: Rect) {
        let scale = self.peak().max(MIN_SCALE);
        let fraction = |r: Option<f64>| r.map(|r| (r / scale) as f32);
        let read: Vec<Option<f32>> = self.history.iter().map(|(r, _)| fraction(*r)).collect();
        let write: Vec<Option<f32>> = self.history.iter().map(|(_, w)| fraction(*w)).collect();
        meter::load_chart(ctx, graph, &read, &[(meter::line_color(0), read.clone()), (meter::line_color(1), write)]);
    }
}

/// "R 1.2M W 30K" for the header.
fn rates(sample: Option<&DiskSample>) -> String {
    let rate = |r: Option<f64>| r.map_or("-".into(), meter::format_bytes);
    sample.map_or("n/a".into(), |s| format!("R {} W {}", rate(s.read_rate), rate(s.write_rate)))
}

impl Widget for DiskGraph {
    fn name(&self) -> &str {
        "disk"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        match orientation {
            Orientation::Vertical => self.full_height(),
            Orientation::Horizontal => metric_extent(orientation, COMPACT_GRAPH),
        }
    }

    fn update(&mut self, ctx: &UpdateCtx) -> bool {
        if !self.every.due(ctx.now) {
            return false;
        }
        self.last = self.source.sample();
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(self.last.as_ref().map_or((None, None), |s| (s.read_rate, s.write_rate)));
        true
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let compact = ctx.bounds.height + 0.5 < self.full_height();
        let area = card(ctx);
        let rest = header(ctx, area, "DISK", &rates(self.last.as_ref()));
        if compact {
            self.draw_activity(ctx, rest);
            return;
        }
        let at = |y| Rect::new(rest.x, y, rest.width, 0.0);
        let mut y = rest.y;
        let volumes = self.last.as_ref().map_or(&[][..], |s| &s.volumes[..self.volumes()]);
        if !volumes.is_empty() {
            // Wide enough for the names and the table's title, but at most
            // 40% of the width.
            let title_w = ctx.text.measure("Space", meter::DETAIL_SIZE).width;
            let widest = volumes.iter().map(|v| ctx.text.measure(&v.name, ROW_SIZE).width).fold(title_w, f32::max);
            let label_w = widest.min(rest.width * 0.4);
            let name = |ctx: &DrawCtx<'_>, s: &str| meter::fit(ctx.text, s, ROW_SIZE, label_w);
            let rows: Vec<_> = volumes
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let free = v.total_bytes.saturating_sub(v.used_bytes);
                    (name(ctx, &v.name), meter::color(i), [meter::format_gib(v.used_bytes), meter::format_gib(free), meter::format_gib(v.total_bytes)])
                })
                .collect();
            y = meter::table(ctx, at(y), label_w, "Space", ["Used", "Free", "Total"], &rows) + SECTION_GAP;
            for (i, v) in volumes.iter().enumerate() {
                let used = (v.total_bytes > 0).then(|| v.used_bytes as f32 / v.total_bytes as f32);
                meter::bar_row(ctx, meter::row(rest, y), label_w, name(ctx, &v.name), used, meter::percent(used), meter::color(i));
                y += meter::row_height();
            }
            y += SECTION_GAP;
        }
        let fg = ctx.theme.foreground;
        y = meter::title(ctx, at(y), "Activity", format!("peak {}", meter::format_rate(self.peak())), fg);
        self.draw_activity(ctx, Rect::new(rest.x, y, rest.width, GRAPH_HEIGHT));
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use sidebar_core::source::Volume;

    use super::*;
    use crate::testing::drawn_text;

    #[test]
    fn shows_volumes_and_rates() {
        let volume = |name: &str, used: u64, total: u64| Volume { name: name.into(), used_bytes: used << 30, total_bytes: total << 30 };
        let sample = DiskSample { volumes: vec![volume("/", 120, 500), volume("/home", 900, 1000)], read_rate: Some(1_500_000.0), write_rate: Some(30_000.0) };
        let mut disk = DiskGraph::new(Box::new(move || Some(sample.clone())));
        assert!(disk.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH }));
        let h = disk.full_height();
        let text = drawn_text(&disk, 148.0, h);
        assert_eq!(&text[..2], ["DISK", "R 1.4M W 29K"]);
        for want in ["/", "120G", "380G", "500G", "/home", "24%", "90%", "Activity"] {
            assert!(text.contains(&want.to_string()), "{want} missing from {text:?}");
        }
        // Compact: just the rates and the chart.
        assert_eq!(drawn_text(&disk, 148.0, 50.0), ["DISK", "R 1.4M W 29K"]);
    }
}
