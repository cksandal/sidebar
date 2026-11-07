use sidebar_core::source::Battery;
use sidebar_core::{DrawCtx, MeasureCtx, Orientation, Source, UpdateCtx, Widget};

use crate::{Every, SAMPLE_INTERVAL, bar, card, header, metric_extent};

const BAR: f32 = 6.0;

/// Battery charge with a bar; the title says whether it is charging.
pub struct BatteryMeter {
    source: Box<dyn Source<Battery>>,
    every: Every,
    last: Option<Battery>,
}

impl BatteryMeter {
    pub fn new(source: Box<dyn Source<Battery>>) -> Self {
        // Battery state changes slowly; once per interval is plenty.
        Self { source, every: Every::new(SAMPLE_INTERVAL * 5), last: None }
    }
}

impl Widget for BatteryMeter {
    fn name(&self) -> &str {
        "battery"
    }

    fn measure(&self, _ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        metric_extent(orientation, BAR)
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

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let area = card(ctx);
        let title = if self.last.is_some_and(|b| b.charging) { "BAT +" } else { "BAT" };
        let value = self.last.map_or("n/a".into(), |b| format!("{:.0}%", b.level * 100.0));
        let rest = header(ctx, area, title, &value);
        bar(ctx, rest, self.last.map_or(0.0, |b| b.level), BAR);
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::testing::drawn_text;

    #[test]
    fn shows_level_and_charging() {
        let mut meter = BatteryMeter::new(Box::new(|| Some(Battery { level: 0.874, charging: true })));
        assert!(meter.update(&UpdateCtx { now: SystemTime::UNIX_EPOCH }));
        assert_eq!(drawn_text(&meter, 148.0, 40.0), ["BAT +", "87%"]);
    }
}
