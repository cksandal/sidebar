use std::collections::VecDeque;

use sidebar_core::source::{CpuInfo, CpuSample, Memory};
use sidebar_core::text::line_height;
use sidebar_core::{DrawCtx, InputEvent, MeasureCtx, Orientation, Point, PointerButton, Rect, Source, UpdateCtx, Widget};

use crate::meter::{self, COMPACT_GRAPH, GAP, GRAPH_HEIGHT, HISTORY, ROW_SIZE, SECTION_GAP, TEMP_HEIGHT};
use crate::{CARD_PAD, Every, SAMPLE_INTERVAL, card, header, metric_extent};

/// More cores than this are listed in two columns.
const ONE_COLUMN_MAX: usize = 8;

/// Cores shown together: all of them, or one core type on hybrid CPUs.
struct Block {
    /// "P-cores" / "E-cores"; `None` when there is a single block.
    name: Option<String>,
    cores: Vec<usize>,
    /// Whether the core rows are shown; named blocks start collapsed and
    /// toggle when their title is clicked.
    expanded: bool,
}

/// CPU load, clock and model; optionally RAM and swap use; a bar per core,
/// in blocks by core type on hybrid CPUs (P-cores, E-cores; collapsed to
/// their title and average until clicked); a chart of
/// every core's recent load; and a chart of the package temperature. Falls
/// back to a header and the load chart when there is not enough room
/// (horizontal docks).
pub struct CpuGraph {
    source: Box<dyn Source<CpuSample>>,
    every: Every,
    model: Option<String>,
    cores: usize,
    blocks: Vec<Block>,
    temperature: bool,
    history: VecDeque<CpuSample>,
    memory: Option<Box<dyn Source<Memory>>>,
    last_memory: Option<Memory>,
    /// Block whose title the primary button went down on.
    pressed: Option<usize>,
}

impl CpuGraph {
    pub fn new(source: Box<dyn Source<CpuSample>>, info: CpuInfo) -> Self {
        let blocks = if info.groups.len() > 1 {
            info.groups.into_iter().map(|g| Block { name: Some(g.name), cores: g.cores, expanded: false }).collect()
        } else {
            vec![Block { name: None, cores: (0..info.cores).collect(), expanded: true }]
        };
        Self {
            source,
            every: Every::new(SAMPLE_INTERVAL),
            model: info.model.as_deref().map(short_model),
            cores: info.cores,
            blocks,
            temperature: info.temperature,
            history: VecDeque::with_capacity(HISTORY),
            memory: None,
            last_memory: None,
            pressed: None,
        }
    }

    /// Also shows RAM and swap use, sampled with the CPU.
    pub fn with_memory(mut self, source: Box<dyn Source<Memory>>) -> Self {
        self.memory = Some(source);
        self
    }

    /// Table, then a bar each for RAM and swap.
    fn memory_height(&self) -> f32 {
        if self.memory.is_some() { meter::table_height(2) + 2.0 * meter::row_height() + SECTION_GAP } else { 0.0 }
    }

    /// The same for every block, so the bars line up across blocks.
    fn columns(&self) -> usize {
        if self.cores > ONE_COLUMN_MAX { 2 } else { 1 }
    }

    fn rows(&self, block: &Block) -> usize {
        block.cores.len().div_ceil(self.columns())
    }

    fn block_height(&self, block: &Block) -> f32 {
        let title = if block.name.is_some() { meter::title_height() } else { 0.0 };
        let rows = if block.expanded { self.rows(block) as f32 * meter::row_height() } else { 0.0 };
        title + rows
    }

    /// Top of the first block, for a widget drawn in `bounds`; must match
    /// the layout in `draw`.
    fn blocks_top(&self, bounds: Rect) -> f32 {
        bounds.y + CARD_PAD + line_height(crate::HEADER_SIZE) + GAP + meter::details_height(2) + self.memory_height()
    }

    /// The block whose title is at `pos`, if it can be toggled.
    fn title_at(&self, bounds: Rect, pos: Point) -> Option<usize> {
        if bounds.height + 0.5 < self.full_height() || !bounds.contains(pos) {
            return None;
        }
        let mut y = self.blocks_top(bounds);
        for (i, block) in self.blocks.iter().enumerate() {
            if block.name.is_some() && (y..y + meter::title_height()).contains(&pos.y) {
                return Some(i);
            }
            y += self.block_height(block) + SECTION_GAP;
        }
        None
    }

    /// Height of the detailed layout, card padding included.
    fn full_height(&self) -> f32 {
        let header = line_height(crate::HEADER_SIZE) + GAP;
        let blocks: f32 = self.blocks.iter().map(|b| self.block_height(b) + SECTION_GAP).sum();
        let temp = if self.temperature { SECTION_GAP + meter::chart_height(TEMP_HEIGHT) } else { 0.0 };
        header + meter::details_height(2) + self.memory_height() + blocks + meter::chart_height(GRAPH_HEIGHT) + temp + 2.0 * CARD_PAD
    }

    fn celsius(&self) -> Option<f32> {
        self.history.back().and_then(|s| s.celsius)
    }

    fn label(&self, core: usize) -> String {
        if self.columns() == 1 { format!("Core {}", core + 1) } else { (core + 1).to_string() }
    }

    fn draw_memory(&self, ctx: &mut DrawCtx<'_>, area: Rect) {
        let rows = match self.last_memory {
            Some(m) => [("RAM", Some((m.used_bytes, m.total_bytes))), ("Page", Some((m.swap_used_bytes, m.swap_total_bytes)))],
            None => [("RAM", None), ("Page", None)],
        };
        let label_w = ctx.text.measure("Page", ROW_SIZE).width;
        let mut y = meter::memory_table(ctx, area, label_w, &rows);
        let accent = ctx.theme.accent;
        for (name, values) in rows {
            let fraction = values.filter(|(_, total)| *total > 0).map(|(used, total)| used as f32 / total as f32);
            meter::bar_row(ctx, meter::row(area, y), label_w, name.into(), fraction, meter::percent(fraction), accent);
            y += meter::row_height();
        }
    }

    /// Title line with the block's average load (hybrid CPUs), core rows.
    fn draw_block(&self, ctx: &mut DrawCtx<'_>, block: &Block, area: Rect) {
        let mut y = area.y;
        if let Some(name) = &block.name {
            let value = meter::percent(self.history.back().map(|s| average(s, &block.cores)));
            let marker = if block.expanded { "▾" } else { "▸" };
            y = meter::title(ctx, area, &format!("{marker} {name}"), value, ctx.theme.foreground);
            if !block.expanded {
                return;
            }
        }
        let columns = self.columns();
        let rows = self.rows(block);
        let col_gap = 8.0;
        let col_width = (area.width - col_gap * (columns - 1) as f32) / columns as f32;
        // Fixed label width so the bars line up.
        let label_w = ctx.text.measure(&self.label(self.cores.saturating_sub(1)), ROW_SIZE).width;
        let loads = self.history.back().map_or(&[][..], |s| &s.cores[..]);
        for (k, &core) in block.cores.iter().enumerate() {
            let (row, col) = (k % rows, k / rows);
            let cell = Rect::new(area.x + col as f32 * (col_width + col_gap), y + row as f32 * meter::row_height(), col_width, line_height(ROW_SIZE));
            let load = loads.get(core).copied();
            meter::bar_row(ctx, cell, label_w, self.label(core), load, meter::percent(load), meter::color(core));
        }
    }

    /// Total load as an area, every core as a line.
    fn draw_load(&self, ctx: &mut DrawCtx<'_>, graph: Rect) {
        let total: Vec<Option<f32>> = self.history.iter().map(|s| Some(s.total)).collect();
        let lines: Vec<_> = (0..self.cores)
            .map(|core| (meter::line_color(core), self.history.iter().map(|s| s.cores.get(core).copied()).collect()))
            .collect();
        meter::load_chart(ctx, graph, &total, &lines);
    }
}

/// Mean load of `cores` in `sample`.
fn average(sample: &CpuSample, cores: &[usize]) -> f32 {
    let loads: Vec<f32> = cores.iter().filter_map(|&c| sample.cores.get(c).copied()).collect();
    if loads.is_empty() { 0.0 } else { loads.iter().sum::<f32>() / loads.len() as f32 }
}

/// Drops trademark marks and filler words from a CPU model name:
/// "Intel(R) Core(TM) i7-2670QM CPU @ 2.20GHz" -> "Intel Core i7-2670QM 2.20GHz".
fn short_model(model: &str) -> String {
    let mut name = model.to_string();
    for mark in ["(R)", "(r)", "(TM)", "(tm)"] {
        name = name.replace(mark, "");
    }
    name.split_whitespace()
        // "AMD Ryzen 5 5600G with Radeon Graphics"
        .take_while(|w| *w != "with")
        .filter(|w| !matches!(*w, "CPU" | "Processor" | "@") && !w.ends_with("-Core"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Widget for CpuGraph {
    fn name(&self) -> &str {
        "cpu"
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
        let memory = self.memory.as_mut().and_then(|m| m.sample());
        let memory_changed = memory != self.last_memory;
        self.last_memory = memory;
        let Some(sample) = self.source.sample() else { return memory_changed };
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(sample);
        true
    }

    fn on_input(&mut self, event: &InputEvent, bounds: Rect) -> bool {
        match *event {
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true } => {
                self.pressed = self.title_at(bounds, pos);
                false
            }
            // A click: released on the title it was pressed on. A drag sends
            // PointerLeft first, so it never toggles.
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false } => {
                match self.pressed.take() {
                    Some(i) if self.title_at(bounds, pos) == Some(i) => {
                        self.blocks[i].expanded = !self.blocks[i].expanded;
                        true
                    }
                    _ => false,
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
        let compact = ctx.bounds.height + 0.5 < self.full_height();
        let area = card(ctx);
        let load = self.history.back().map(|s| s.total);
        if compact {
            // Temperature goes in the header, next to the load.
            let value = match (load, self.celsius()) {
                (Some(load), Some(c)) => format!("{}  {}", meter::percent(Some(load)), meter::format_celsius(Some(c))),
                (Some(load), None) => meter::percent(Some(load)),
                (None, _) => "n/a".into(),
            };
            let rest = header(ctx, area, "CPU", &value);
            self.draw_load(ctx, rest);
            return;
        }
        let rest = header(ctx, area, "CPU", &load.map_or("n/a".into(), |l| meter::percent(Some(l))));
        let clock = match self.history.back().and_then(|s| s.mhz) {
            Some(mhz) => format!("Clock {mhz:.0} MHz"),
            None => "Clock n/a".into(),
        };
        let model = self.model.clone().unwrap_or_else(|| "Unknown CPU".into());
        let fg = ctx.theme.foreground;
        let mut y = meter::details(ctx, rest, vec![(clock, meter::color(2)), (model, fg.with_alpha(200))]);
        let at = |y| Rect::new(rest.x, y, rest.width, 0.0);
        if self.memory.is_some() {
            self.draw_memory(ctx, at(y));
            y += self.memory_height();
        }
        debug_assert_eq!(y, self.blocks_top(ctx.bounds));
        for block in &self.blocks {
            self.draw_block(ctx, block, at(y));
            y += self.block_height(block) + SECTION_GAP;
        }
        y = meter::title(ctx, at(y), "Load", meter::percent(load), fg);
        self.draw_load(ctx, Rect::new(rest.x, y, rest.width, GRAPH_HEIGHT));
        if self.temperature {
            y += GRAPH_HEIGHT + SECTION_GAP;
            let celsius = self.celsius();
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
    use crate::testing::{drawn_text, rects_in};
    use crate::meter::{area_color, line_color, temp_color};
    use sidebar_core::Theme;

    fn sample(total: f32, cores: &[f32]) -> CpuSample {
        CpuSample { total, cores: cores.to_vec(), mhz: Some(2201.4), celsius: None }
    }

    fn info(cores: usize) -> CpuInfo {
        CpuInfo { model: Some("Intel(R) Core(TM) i7-2670QM CPU @ 2.20GHz".into()), cores, groups: Vec::new(), temperature: false }
    }

    #[test]
    fn samples_once_per_interval_and_lists_cores() {
        let mut samples = [sample(0.25, &[0.5, 0.0]), sample(0.5, &[1.0, 0.0])].into_iter();
        let mut cpu = CpuGraph::new(Box::new(move || samples.next()), info(2));
        let h = cpu.full_height();
        assert_eq!(drawn_text(&cpu, 148.0, h), ["CPU", "n/a", "Clock n/a", "Intel Core i7-2670QM…", "Core 1", "-", "Core 2", "-", "Load", "-"]);

        let t0 = SystemTime::UNIX_EPOCH;
        assert!(cpu.update(&UpdateCtx { now: t0 }));
        assert!(!cpu.update(&UpdateCtx { now: t0 + Duration::from_millis(250) }));
        assert!(cpu.update(&UpdateCtx { now: t0 + Duration::from_secs(1) }));
        assert_eq!(drawn_text(&cpu, 148.0, h)[..6], ["CPU", "50%", "Clock 2201 MHz", "Intel Core i7-2670QM…", "Core 1", "100%"]);

        // Total load is drawn as a filled area, one column per sample.
        let area = rects_in(&cpu, 148.0, h, area_color(Theme::default().accent));
        assert_eq!(area.len(), 2);
        assert!(area[1].height > area[0].height && area[1].x > area[0].x);
    }

    #[test]
    fn hybrid_cpus_get_a_block_per_core_type() {
        use sidebar_core::source::CoreGroup;
        let groups = vec![
            CoreGroup { name: "P-cores".into(), cores: vec![0, 1] },
            CoreGroup { name: "E-cores".into(), cores: vec![2, 3] },
        ];
        let mut cpu = CpuGraph::new(Box::new(|| Some(sample(0.5, &[0.8, 0.6, 0.2, 0.4]))), CpuInfo { groups, ..info(4) });
        let flat = CpuGraph::new(Box::new(|| None), info(4));
        cpu.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH });
        // Collapsed by default: titles with averages only.
        let text = drawn_text(&cpu, 148.0, cpu.full_height());
        assert_eq!(text[4..], ["▸ P-cores", "70%", "▸ E-cores", "30%", "Load", "50%"]);
        for block in &mut cpu.blocks {
            block.expanded = true;
        }
        // Expanded, the block titles add to the flat layout.
        assert!(cpu.full_height() > flat.full_height());
        let text = drawn_text(&cpu, 148.0, cpu.full_height());
        assert_eq!(text[4..], ["▾ P-cores", "70%", "Core 1", "80%", "Core 2", "60%", "▾ E-cores", "30%", "Core 3", "20%", "Core 4", "40%", "Load", "50%"]);
        // One load chart for all cores.
        let area = rects_in(&cpu, 148.0, cpu.full_height(), area_color(Theme::default().accent));
        assert_eq!(area.len(), 1);
        for core in 0..4 {
            assert!(!rects_in(&cpu, 148.0, cpu.full_height(), line_color(core)).is_empty());
        }
    }

    fn hybrid(memory: bool) -> CpuGraph {
        use sidebar_core::source::CoreGroup;
        let groups = vec![
            CoreGroup { name: "P-cores".into(), cores: vec![0, 1] },
            CoreGroup { name: "E-cores".into(), cores: vec![2, 3] },
        ];
        let cpu = CpuGraph::new(Box::new(|| Some(sample(0.5, &[0.8, 0.6, 0.2, 0.4]))), CpuInfo { groups, ..info(4) });
        if memory { cpu.with_memory(Box::new(|| None)) } else { cpu }
    }

    /// Where `text` was drawn, in a widget drawn at `bounds`.
    fn text_origin(widget: &dyn Widget, bounds: Rect, text: &str) -> Point {
        use sidebar_core::{ApproxText, DrawCmd, Scene, Size};
        let mut scene = Scene::new(Size::new(bounds.right(), bounds.bottom()));
        let theme = Theme::default();
        widget.draw(&mut DrawCtx { scene: &mut scene, bounds, theme: &theme, text: &ApproxText });
        scene
            .cmds
            .iter()
            .find_map(|c| match c {
                DrawCmd::Text { origin, text: t, .. } if t == text => Some(*origin),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{text:?} not drawn"))
    }

    /// Not at the origin, as in a dock below other widgets.
    fn at(cpu: &CpuGraph) -> Rect {
        Rect::new(6.0, 100.0, 148.0, cpu.full_height())
    }

    /// Press and release at `pos`, with the widget at [`at`].
    fn click(cpu: &mut CpuGraph, pos: Point) -> bool {
        let bounds = at(cpu);
        let press = InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true };
        let release = InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false };
        cpu.on_input(&press, bounds) | cpu.on_input(&release, bounds)
    }

    #[test]
    fn clicking_a_title_toggles_its_block() {
        for memory in [false, true] {
            let mut cpu = hybrid(memory);
            let collapsed = cpu.full_height();
            let title = text_origin(&cpu, at(&cpu), "▸ E-cores") + Point::new(4.0, 4.0);
            assert!(click(&mut cpu, title));
            assert!(cpu.full_height() > collapsed);
            assert_eq!((cpu.blocks[0].expanded, cpu.blocks[1].expanded), (false, true));

            // P-cores is above E-cores, so its title did not move.
            let title = text_origin(&cpu, at(&cpu), "▸ P-cores") + Point::new(4.0, 4.0);
            assert!(click(&mut cpu, title));
            assert!(click(&mut cpu, title));
            assert!(!cpu.blocks[0].expanded);

            // Elsewhere: no toggle.
            let below = Point::new(20.0, at(&cpu).bottom() - 10.0);
            assert!(!click(&mut cpu, below));
        }
    }

    #[test]
    fn dragging_from_a_title_does_not_toggle() {
        let mut cpu = hybrid(false);
        let bounds = Rect::new(0.0, 0.0, 148.0, cpu.full_height());
        let pos = text_origin(&cpu, bounds, "▸ P-cores") + Point::new(4.0, 4.0);
        cpu.on_input(&InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true }, bounds);
        cpu.on_input(&InputEvent::PointerLeft, bounds);
        assert!(!cpu.on_input(&InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false }, bounds));
        assert!(!cpu.blocks[0].expanded);
    }

    #[test]
    fn charts_temperature() {
        let mut temps = [45.0, 90.0].into_iter();
        let mut cpu = CpuGraph::new(
            Box::new(move || Some(CpuSample { celsius: Some(temps.next()?), ..sample(0.2, &[0.2, 0.2]) })),
            CpuInfo { temperature: true, ..info(2) },
        );
        let flat = CpuGraph::new(Box::new(|| None), info(2));
        assert!(cpu.full_height() > flat.full_height());
        let t0 = SystemTime::UNIX_EPOCH;
        cpu.update(&UpdateCtx { now: t0 });
        cpu.update(&UpdateCtx { now: t0 + Duration::from_secs(1) });
        let h = cpu.full_height();
        assert_eq!(drawn_text(&cpu, 148.0, h)[8..], ["Load", "20%", "Temp", "90°C"]);
        // Each sample is colored by its temperature.
        let cool = rects_in(&cpu, 148.0, h, temp_color(45.0).with_alpha(50));
        let hot = rects_in(&cpu, 148.0, h, temp_color(90.0).with_alpha(50));
        assert_eq!((cool.len(), hot.len()), (1, 1));
        assert!(hot[0].height > cool[0].height);
        // Compact: temperature next to the load.
        assert_eq!(drawn_text(&cpu, 120.0, 40.0), ["CPU", "20%  90°C"]);
    }

    #[test]
    fn shows_memory_table() {
        let gib = 1u64 << 30;
        let memory = Memory { used_bytes: 5 * gib + gib / 2, total_bytes: 32 * gib, swap_used_bytes: 0, swap_total_bytes: 8 * gib };
        let mut cpu = CpuGraph::new(Box::new(|| None), info(2)).with_memory(Box::new(move || Some(memory)));
        let without = CpuGraph::new(Box::new(|| None), info(2));
        assert!(cpu.full_height() > without.full_height());
        // Memory still updates while the CPU source has nothing yet.
        assert!(cpu.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH }));
        let text = drawn_text(&cpu, 148.0, cpu.full_height());
        assert_eq!(
            text[4..20],
            ["Mem", "Used", "Free", "Total", "RAM", "5.5G", "26G", "32G", "Page", "0.0G", "8.0G", "8.0G", "RAM", "17%", "Page", "0%"]
        );
    }

    #[test]
    fn compact_when_short() {
        let mut cpu = CpuGraph::new(Box::new(|| Some(sample(0.1, &[0.1; 4]))), info(4));
        cpu.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH });
        assert_eq!(drawn_text(&cpu, 120.0, 40.0), ["CPU", "10%"]);
        // Core lines are still graphed.
        assert!(!rects_in(&cpu, 120.0, 40.0, line_color(3)).is_empty());
    }

    #[test]
    fn many_cores_use_two_columns() {
        let one = CpuGraph::new(Box::new(|| None), info(8));
        let two = CpuGraph::new(Box::new(|| None), info(16));
        assert_eq!(one.rows(&one.blocks[0]), 8);
        assert_eq!(two.rows(&two.blocks[0]), 8);
        assert_eq!(drawn_text(&two, 148.0, two.full_height())[4..6], ["1", "-"]);
    }

    #[test]
    fn shortens_model_names() {
        assert_eq!(short_model("13th Gen Intel(R) Core(TM) i7-13700"), "13th Gen Intel Core i7-13700");
        assert_eq!(short_model("AMD Ryzen 9 7950X 16-Core Processor"), "AMD Ryzen 9 7950X");
        assert_eq!(short_model("AMD Ryzen 5 5600G with Radeon Graphics"), "AMD Ryzen 5 5600G");
    }
}
