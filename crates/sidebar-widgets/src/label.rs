use sidebar_core::{DrawCtx, MeasureCtx, Orientation, Widget};

use crate::{centered, text_extent};

const FONT_SIZE: f32 = 13.0;

/// Static text.
pub struct Label {
    text: String,
}

impl Label {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl Widget for Label {
    fn name(&self) -> &str {
        "label"
    }

    fn measure(&self, ctx: &MeasureCtx<'_>, orientation: Orientation, _cross_extent: f32) -> f32 {
        text_extent(ctx, &self.text, FONT_SIZE, orientation)
    }

    fn draw(&self, ctx: &mut DrawCtx<'_>) {
        let origin = centered(ctx.text, &self.text, FONT_SIZE, ctx.bounds);
        ctx.scene.text(origin, self.text.clone(), FONT_SIZE, ctx.theme.foreground);
    }
}
