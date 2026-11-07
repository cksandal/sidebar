use std::collections::VecDeque;

use sidebar_core::source::{GpuInfo, GpuSample};
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, MeasureCtx, Orientation, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, COMPACT_GRAPH, GAP, GRAPH_HEIGHT, HISTORY, ROW_SIZE, SECTION_GAP, TEMP_HEIGHT};
use crate::{CARD_PAD, Every, SAMPLE_INTERVAL, card, header, metric_extent};

/// One bar row, and the matching line in the load chart.
struct Series {
    label: &'static str,
    /// Bar fill, `0.0..=1.0`.
    fraction: fn(&GpuSample) -> Option<f32>,
    /// Text at the end of the row; the percentage when `None`.
    value: Option<fn(&GpuSample) -> Option<String>>,
}

const SERIES: [Series; 7] = [
    Series { label: "GPU", fraction: |s| Some(s.load), value: None },
    Series { label: "VRAM", fraction: memory_fraction, value: None },
    Series { label: "Bus", fraction: |s| s.memory_load, value: None },
    Series { label: "Enc", fraction: |s| s.encoder_load, value: None },
    Series { label: "Dec", fraction: |s| s.decoder_load, value: None },
    Series { label: "Fan", fraction: |s| s.fan, value: None },
    Series {
        label: "Power",
        fraction: |s| Some(s.power_watts? / s.power_limit_watts.filter(|l| *l > 0.0)?),
        value: Some(|s| s.power_watts.map(|w| format!("{w:.0} W"))),
    },
];

fn memory_fraction(s: &GpuSample) -> Option<f32> {
    (s.memory_total_bytes > 0).then(|| s.memory_used_bytes as f32 / s.memory_total_bytes as f32)
}

/// GPU load, clocks and name; a VRAM table; a bar each for the engine,
/// VRAM, memory bus, video encoder and decoder, fan and power; a chart of
/// all of them; and a temperature chart. Laid out like [`crate::CpuGraph`],
/// with the same compact fallback.
pub struct GpuGraph {
    source: Box<dyn Source<GpuSample>>,
    every: Every,
    name: String,
    temperature: bool,
    history: VecDeque<GpuSample>,
}

impl GpuGraph {
    pub fn new(source: Box<dyn Source<GpuSample>>, info: GpuInfo) -> Self {
        Self {
            source,
            every: Every::new(SAMPLE_INTERVAL),
            name: short_name(&info.name),
            temperature: info.temperature,
            history: VecDeque::with_capacity(HISTORY),
        }
    }

    fn full_height(&self) -> f32 {
        let header = line_height(crate::HEADER_SIZE) + GAP;
        let rows = meter::table_height(1) + SERIES.len() as f32 * meter::row_height() + SECTION_GAP;
        let temp = if self.temperature { SECTION_GAP + meter::chart_height(TEMP_HEIGHT) } else { 0.0 };
        header + meter::details_height(2) + rows + meter::chart_height(GRAPH_HEIGHT) + temp + 2.0 * CARD_PAD
    }

    fn last(&self) -> Option<&GpuSample> {
        self.history.back()
    }

    fn draw_load(&self, ctx: &mut DrawCtx<'_>, graph: Rect) {
        let area: Vec<Option<f32>> = self.history.iter().map(|s| Some(s.load)).collect();
        let lines: Vec<_> = SERIES
            .iter()
            .enumerate()
            .map(|(i, series)| (meter::line_color(i), self.history.iter().map(series.fraction).collect()))
            .collect();
        meter::load_chart(ctx, graph, &area, &lines);
    }
}

/// "NVIDIA GeForce RTX 5060 Ti" -> "GeForce RTX 5060 Ti".
fn short_name(name: &str) -> String {
    name.strip_prefix("NVIDIA ").unwrap_or(name).to_string()
}

impl Widget for GpuGraph {
    fn name(&self) -> &str {
        "gpu"
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
        let Some(sample) = self.source.sample() else { return false };
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        true
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let compact = ctx.bounds.height + 0.5 < self.full_height();
        let area = card(ctx);
        let load = self.last().map(|s| s.load);
        let celsius = self.last().and_then(|s| s.celsius);
        if compact {
            let value = match (load, celsius) {
                (Some(load), Some(c)) => format!("{}  {}", meter::percent(Some(load)), meter::format_celsius(Some(c))),
                (Some(load), None) => meter::percent(Some(load)),
                (None, _) => "n/a".into(),
            };
            let rest = header(ctx, area, "GPU", &value);
            self.draw_load(ctx, rest);
            return;
        }
        let rest = header(ctx, area, "GPU", &load.map_or("n/a".into(), |l| meter::percent(Some(l))));
        let mhz = |v: Option<f32>| v.map_or("-".into(), |m| format!("{m:.0}"));
        let clocks = format!("Core {} · Mem {} MHz", mhz(self.last().and_then(|s| s.core_mhz)), mhz(self.last().and_then(|s| s.memory_mhz)));
        let fg = ctx.theme.foreground;
        let mut y = meter::details(ctx, rest, vec![(clocks, meter::color(2)), (self.name.clone(), fg.with_alpha(200))]);

        let label_w = SERIES.iter().map(|s| ctx.text.measure(s.label, ROW_SIZE).width).fold(0.0, f32::max);
        let at = |y| Rect::new(rest.x, y, rest.width, 0.0);
        let vram = self.last().map(|s| (s.memory_used_bytes, s.memory_total_bytes));
        y = meter::memory_table(ctx, at(y), label_w, &[("VRAM", vram)]);
        for (i, series) in SERIES.iter().enumerate() {
            let fraction = self.last().and_then(series.fraction);
            let value = match series.value {
                Some(text) => self.last().and_then(text).unwrap_or_else(|| "-".into()),
                None => meter::percent(fraction),
            };
            meter::bar_row(ctx, meter::row(rest, y), label_w, series.label.into(), fraction, value, meter::color(i));
            y += meter::row_height();
        }
        y += SECTION_GAP;

        y = meter::title(ctx, at(y), "Load", meter::percent(load), fg);
        self.draw_load(ctx, Rect::new(rest.x, y, rest.width, GRAPH_HEIGHT));
        if self.temperature {
            y += GRAPH_HEIGHT + SECTION_GAP;
            y = meter::title(ctx, at(y), "Temp", meter::format_celsius(celsius), celsius.map_or(fg, meter::temp_color));
            let temps: Vec<Option<f32>> = self.history.iter().map(|s| s.celsius).collect();
            meter::temperature_chart(ctx, Rect::new(rest.x, y, rest.width, TEMP_HEIGHT), &temps);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::meter::{area_color, line_color, temp_color};
    use crate::testing::{drawn_text, rects_in};
    use sidebar_core::Theme;

    const GIB: u64 = 1 << 30;

    fn sample(load: f32) -> GpuSample {
        GpuSample {
            load,
            memory_load: Some(0.1),
            memory_used_bytes: GIB + GIB / 5,
            memory_total_bytes: 16 * GIB,
            encoder_load: Some(0.0),
            decoder_load: None,
            fan: Some(0.3),
            power_watts: Some(14.2),
            power_limit_watts: Some(180.0),
            core_mhz: Some(1282.0),
            memory_mhz: Some(7001.0),
            celsius: Some(48.0),
        }
    }

    fn info() -> GpuInfo {
        GpuInfo { name: "NVIDIA GeForce RTX 5060 Ti".into(), temperature: true }
    }

    #[test]
    fn shows_engines_memory_and_power() {
        let mut loads = [0.02, 0.5].into_iter();
        let mut gpu = GpuGraph::new(Box::new(move || loads.next().map(sample)), info());
        let h = gpu.full_height();
        assert_eq!(drawn_text(&gpu, 200.0, h)[..4], ["GPU", "n/a", "Core - · Mem - MHz", "GeForce RTX 5060 Ti"]);

        let t0 = SystemTime::UNIX_EPOCH;
        assert!(gpu.update(&UpdateCtx { now: t0 }));
        assert!(gpu.update(&UpdateCtx { now: t0 + Duration::from_secs(1) }));
        let text = drawn_text(&gpu, 200.0, h);
        assert_eq!(text[..4], ["GPU", "50%", "Core 1282 · Mem 7001 MHz", "GeForce RTX 5060 Ti"]);
        assert_eq!(text[8..12], ["VRAM", "1.2G", "15G", "16G"]);
        assert_eq!(
            text[12..26],
            ["GPU", "50%", "VRAM", "8%", "Bus", "10%", "Enc", "0%", "Dec", "-", "Fan", "30%", "Power", "14 W"]
        );
        assert_eq!(text[26..], ["Load", "50%", "Temp", "48°C"]);

        let h = gpu.full_height();
        assert_eq!(rects_in(&gpu, 200.0, h, area_color(Theme::default().accent)).len(), 2);
        // Every series with data is charted in its row's color; the decoder has none.
        assert!(!rects_in(&gpu, 200.0, h, line_color(6)).is_empty());
        assert!(rects_in(&gpu, 200.0, h, line_color(4)).is_empty());
        assert_eq!(rects_in(&gpu, 200.0, h, temp_color(48.0).with_alpha(50)).len(), 2);
    }

    #[test]
    fn compact_when_short() {
        let mut gpu = GpuGraph::new(Box::new(|| Some(sample(0.25))), info());
        gpu.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH });
        assert_eq!(drawn_text(&gpu, 120.0, 40.0), ["GPU", "25%  48°C"]);
    }

    #[test]
    fn no_temperature_chart_without_sensor() {
        let with = GpuGraph::new(Box::new(|| None), info());
        let without = GpuGraph::new(Box::new(|| None), GpuInfo { temperature: false, ..info() });
        assert!(without.full_height() < with.full_height());
        assert!(!drawn_text(&without, 148.0, without.full_height()).contains(&"Temp".to_string()));
    }
}
