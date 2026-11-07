//! Weather icons drawn from rounded rects and circles, so they scale to
//! any size and follow the theme without image assets or emoji fonts.

use sidebar_core::source::Sky;
use sidebar_core::{Color, DrawCtx, Rect};

const SUN: Color = Color::rgb(240, 214, 80);
const MOON: Color = Color::rgb(220, 223, 228);
const CLOUD: Color = Color::rgb(190, 200, 215);
const STORM_CLOUD: Color = Color::rgb(130, 140, 160);
const RAIN: Color = Color::rgb(97, 175, 239);
const SNOW: Color = Color::rgb(235, 240, 248);
const FOG: Color = Color::rgb(160, 168, 182);

/// Draws in a square, in fractions of its side.
struct Pen<'c, 'a> {
    ctx: &'c mut DrawCtx<'a>,
    rect: Rect,
}

impl Pen<'_, '_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        let s = self.rect.width;
        let r = Rect::new(self.rect.x + x * s, self.rect.y + y * s, w * s, h * s);
        self.ctx.scene.fill_rect(r, color, r.width.min(r.height) / 2.0);
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        self.rect(cx - r, cy - r, 2.0 * r, 2.0 * r, color);
    }

    fn sun(&mut self, cx: f32, cy: f32, r: f32) {
        self.circle(cx, cy, r, SUN);
        // Rays: four bars and four diagonal dots.
        let (len, w) = (r * 0.55, r * 0.22);
        let gap = r * 1.25;
        self.rect(cx - w / 2.0, cy - gap - len, w, len, SUN);
        self.rect(cx - w / 2.0, cy + gap, w, len, SUN);
        self.rect(cx - gap - len, cy - w / 2.0, len, w, SUN);
        self.rect(cx + gap, cy - w / 2.0, len, w, SUN);
        let d = (gap + len / 2.0) * std::f32::consts::FRAC_1_SQRT_2;
        for (dx, dy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            self.circle(cx + dx, cy + dy, w * 0.6, SUN);
        }
    }

    /// A crescent: a disc with a disc of the card color taken out on the
    /// `cut` side (-1 = left, 1 = right), so it opens toward the other.
    fn moon(&mut self, cx: f32, cy: f32, r: f32, cut: f32, card: Color) {
        self.circle(cx, cy, r, MOON);
        self.circle(cx + cut * r * 0.45, cy - r * 0.35, r * 0.8, card);
    }

    /// A cloud filling the box (x, y, w) with height w * 0.6.
    fn cloud(&mut self, x: f32, y: f32, w: f32, color: Color) {
        let h = w * 0.6;
        self.rect(x, y + h * 0.45, w, h * 0.55, color);
        self.circle(x + w * 0.32, y + h * 0.55, w * 0.2, color);
        self.circle(x + w * 0.6, y + h * 0.42, w * 0.28, color);
    }
}

/// The card's own color behind the icon (background with the card tint),
/// used to cut the moon's crescent.
fn card_color(ctx: &DrawCtx<'_>) -> Color {
    let (bg, accent) = (ctx.theme.background, ctx.theme.accent);
    let mix = |b: u8, a: u8| ((b as u32 * (255 - 28) + a as u32 * 28) / 255) as u8;
    Color::rgb(mix(bg.r, accent.r), mix(bg.g, accent.g), mix(bg.b, accent.b))
}

/// Draws the icon for `sky` in the square `rect`.
pub(crate) fn weather(ctx: &mut DrawCtx<'_>, rect: Rect, sky: Sky, daylight: bool) {
    let card = card_color(ctx);
    let mut pen = Pen { ctx, rect: Rect { height: rect.width, ..rect } };
    // Behind a cloud (lower left), the moon opens up and to the right.
    let light = |pen: &mut Pen<'_, '_>, cx, cy, r, cut| {
        if daylight { pen.sun(cx, cy, r) } else { pen.moon(cx, cy, r, cut, card) }
    };
    match sky {
        Sky::Clear => light(&mut pen, 0.5, 0.5, if daylight { 0.24 } else { 0.34 }, 1.0),
        Sky::PartlyCloudy => {
            light(&mut pen, 0.64, 0.34, if daylight { 0.17 } else { 0.26 }, -1.0);
            pen.cloud(0.02, 0.36, 0.78, CLOUD);
        }
        Sky::Cloudy => pen.cloud(0.04, 0.2, 0.92, CLOUD),
        Sky::Fog => {
            pen.cloud(0.1, 0.06, 0.8, CLOUD);
            for (i, y) in [0.6, 0.74, 0.88].into_iter().enumerate() {
                let inset = if i == 1 { 0.0 } else { 0.1 };
                pen.rect(0.06 + inset, y, 0.88 - 2.0 * inset, 0.07, FOG);
            }
        }
        Sky::Drizzle | Sky::Rain => {
            pen.cloud(0.06, 0.06, 0.88, CLOUD);
            let (h, w) = if sky == Sky::Rain { (0.2, 0.07) } else { (0.08, 0.08) };
            for x in [0.28, 0.48, 0.68] {
                pen.rect(x, 0.68, w, h, RAIN);
            }
        }
        Sky::Snow => {
            pen.cloud(0.06, 0.06, 0.88, CLOUD);
            for (x, y) in [(0.3, 0.74), (0.5, 0.86), (0.7, 0.74)] {
                pen.circle(x, y, 0.06, SNOW);
            }
        }
        Sky::Thunder => {
            pen.cloud(0.06, 0.06, 0.88, STORM_CLOUD);
            // A bolt from two offset bars.
            pen.rect(0.48, 0.6, 0.1, 0.2, SUN);
            pen.rect(0.4, 0.76, 0.1, 0.2, SUN);
        }
    }
}
