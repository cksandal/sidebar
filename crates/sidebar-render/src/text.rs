//! Text shaping and glyph rasterization (cosmic-text).
//!
//! cosmic-text finds system fonts itself (fontconfig *config files* on
//! Linux, parsed in pure Rust; the Fonts directory on Windows), so there is
//! no platform code here.
//!
//! One engine per thread, created on first use: loading the font database
//! takes tens to hundreds of milliseconds, and measurement and rendering
//! must use the same fonts for layout to match pixels.

use std::cell::RefCell;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache};
use sidebar_core::text::line_height;
use sidebar_core::{Color, Point, Rect, Size, TextMeasure};

use crate::Pixmap;

struct Engine {
    fonts: FontSystem,
    glyphs: SwashCache,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

fn with_engine<R>(f: impl FnOnce(&mut Engine) -> R) -> R {
    ENGINE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let engine = slot.get_or_insert_with(|| Engine { fonts: FontSystem::new(), glyphs: SwashCache::new() });
        f(engine)
    })
}

/// Shapes `text` as one unwrapped line.
fn layout(fonts: &mut FontSystem, text: &str, font_size: f32) -> Buffer {
    let mut buffer = Buffer::new(fonts, Metrics::new(font_size, line_height(font_size)));
    buffer.set_size(fonts, None, None);
    // TODO: font family from the theme.
    buffer.set_text(fonts, text, &Attrs::new().family(Family::SansSerif), Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    buffer
}

/// Number of font faces found on this system.
pub fn font_count() -> usize {
    with_engine(|e| e.fonts.db().len())
}

pub fn measure(text: &str, font_size: f32) -> Size {
    with_engine(|e| {
        let buffer = layout(&mut e.fonts, text, font_size);
        let width = buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max);
        Size::new(width, line_height(font_size))
    })
}

/// [`TextMeasure`] backed by the system fonts, for `Shell::set_text_measure`.
pub struct SystemText;

impl TextMeasure for SystemText {
    fn measure(&self, text: &str, font_size: f32) -> Size {
        measure(text, font_size)
    }
}

/// Draws one line of text with its line box's top-left at `origin`
/// (logical px), rendering at `scale` for crisp HiDPI glyphs.
pub(crate) fn draw_text(pixmap: &mut Pixmap, origin: Point, text: &str, font_size: f32, color: Color, scale: f32) {
    with_engine(|e| {
        let buffer = layout(&mut e.fonts, text, font_size * scale);
        let ox = (origin.x * scale).round() as i32;
        let oy = (origin.y * scale).round() as i32;
        let fg = cosmic_text::Color::rgba(color.r, color.g, color.b, color.a);
        // Called per covered pixel (or pixel run); alpha carries coverage
        // only, so the text color's own alpha is applied here.
        buffer.draw(&mut e.fonts, &mut e.glyphs, fg, |x, y, w, h, c| {
            let rect = Rect::new((ox + x) as f32, (oy + y) as f32, w as f32, h as f32);
            let a = (u16::from(c.a()) * u16::from(color.a) + 127) / 255;
            pixmap.fill_rect(rect, Color::rgba(c.r(), c.g(), c.b(), a as u8));
        });
    })
}
