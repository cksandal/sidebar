//! Software rasterizer.
//!
//! Every backend can display an RGBA buffer (wl_shm, MIT-SHM/XPutImage, a
//! Windows DIB section, ...), so rasterizing in software keeps rendering
//! identical everywhere and keeps GPU/toolkit choices out of the backends.
//! A GPU path can be added later behind the same `Scene` input.
//!
//! Supports filled/stroked rectangles (with anti-aliased rounded corners)
//! and text via cosmic-text (see [`text`]).

use std::io::{self, Write};
use std::path::Path;

use sidebar_core::{Color, DrawCmd, Rect, Scene};

pub mod text;

pub use text::SystemText;

/// How alpha is encoded when converting for a display server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaMode {
    /// Color scaled by alpha, for surfaces with a real alpha channel.
    Premultiplied,
    /// Alpha forced to 255, for surfaces without one.
    Opaque,
}

/// RGBA8, straight (non-premultiplied) alpha, row-major, no padding.
pub struct Pixmap {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Pixmap {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0; width as usize * height as usize * 4] }
    }

    /// Alpha-blends `color` over `rect` (physical pixels).
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        if color.a == 0 {
            return;
        }
        let clamp = |v: f32, max: u32| (v.round().max(0.0) as u32).min(max);
        let (x0, x1) = (clamp(rect.x, self.width), clamp(rect.right(), self.width));
        let (y0, y1) = (clamp(rect.y, self.height), clamp(rect.bottom(), self.height));
        for y in y0..y1 {
            for x in x0..x1 {
                self.blend(x, y, color);
            }
        }
    }

    /// Like [`Pixmap::fill_rect`] with anti-aliased corners of `radius`.
    pub fn fill_rounded_rect(&mut self, rect: Rect, color: Color, radius: f32) {
        let r = radius.min(rect.width / 2.0).min(rect.height / 2.0);
        if r < 0.5 {
            return self.fill_rect(rect, color);
        }
        let x0 = (rect.x.floor().max(0.0) as u32).min(self.width);
        let x1 = (rect.right().ceil().max(0.0) as u32).min(self.width);
        let y0 = (rect.y.floor().max(0.0) as u32).min(self.height);
        let y1 = (rect.bottom().ceil().max(0.0) as u32).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                // Distance from the pixel center to the rect shrunk by `r`;
                // inside the rounded shape when that is <= r.
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                let dx = (rect.x + r - cx).max(cx - (rect.right() - r)).max(0.0);
                let dy = (rect.y + r - cy).max(cy - (rect.bottom() - r)).max(0.0);
                let coverage = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend(x, y, color.with_alpha((f32::from(color.a) * coverage).round() as u8));
                }
            }
        }
    }

    fn blend(&mut self, x: u32, y: u32, src: Color) {
        let i = ((y * self.width + x) * 4) as usize;
        let px = &mut self.data[i..i + 4];
        let sa = src.a as u32;
        let inv = 255 - sa;
        let da = px[3] as u32;
        let out_a = sa + da * inv / 255;
        if out_a == 0 {
            return;
        }
        for (c, s) in px[..3].iter_mut().zip([src.r, src.g, src.b]) {
            *c = ((s as u32 * sa + *c as u32 * da * inv / 255) / out_a) as u8;
        }
        px[3] = out_a as u8;
    }

    /// Writes pixels as 32-bit B,G,R,A bytes (little-endian ARGB8888, the
    /// common format of wl_shm, X11 ZPixmap, and Windows DIBs) into `out`,
    /// which must be `width * height * 4` bytes.
    pub fn write_bgra(&self, out: &mut [u8], alpha: AlphaMode) {
        for (src, dst) in self.data.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
            let [r, g, b, a] = [src[0], src[1], src[2], src[3]].map(u32::from);
            let (r, g, b, a) = match alpha {
                AlphaMode::Premultiplied => (r * a / 255, g * a / 255, b * a / 255, a),
                AlphaMode::Opaque => (r, g, b, 255),
            };
            dst.copy_from_slice(&[b as u8, g as u8, r as u8, a as u8]);
        }
    }

    /// Writes a binary PPM (alpha dropped). Handy for headless debugging.
    pub fn write_ppm(&self, path: &Path) -> io::Result<()> {
        let mut out = io::BufWriter::new(std::fs::File::create(path)?);
        write!(out, "P6\n{} {}\n255\n", self.width, self.height)?;
        for px in self.data.chunks_exact(4) {
            out.write_all(&px[..3])?;
        }
        out.flush()
    }
}

/// Rasterizes `scene` at `scale` physical pixels per logical pixel.
pub fn rasterize(scene: &Scene, scale: f32) -> Pixmap {
    let w = (scene.size.width * scale).ceil().max(0.0) as u32;
    let h = (scene.size.height * scale).ceil().max(0.0) as u32;
    let mut pixmap = Pixmap::new(w, h);
    let s = |r: Rect| Rect::new(r.x * scale, r.y * scale, r.width * scale, r.height * scale);

    for cmd in &scene.cmds {
        match cmd {
            DrawCmd::FillRect { rect, color, radius } => pixmap.fill_rounded_rect(s(*rect), *color, radius * scale),
            DrawCmd::StrokeRect { rect, color, width } => {
                let r = *rect;
                let t = *width;
                for side in [
                    Rect::new(r.x, r.y, r.width, t),
                    Rect::new(r.x, r.bottom() - t, r.width, t),
                    Rect::new(r.x, r.y, t, r.height),
                    Rect::new(r.right() - t, r.y, t, r.height),
                ] {
                    pixmap.fill_rect(s(side), *color);
                }
            }
            DrawCmd::Text { origin, text, size, color } => {
                text::draw_text(&mut pixmap, *origin, text, *size, *color, scale);
            }
        }
    }
    pixmap
}

#[cfg(test)]
mod tests {
    use super::*;
    use sidebar_core::Size;

    #[test]
    fn rounded_corners_are_transparent() {
        let mut pm = Pixmap::new(20, 20);
        pm.fill_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), Color::WHITE, 8.0);
        let alpha = |x: u32, y: u32| pm.data[((y * 20 + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(10, 10), 255);
        assert_eq!(alpha(10, 0), 255);
        assert!(alpha(2, 2) > 0 && alpha(2, 2) < 255, "corner edge is anti-aliased");
    }

    #[test]
    fn renders_and_measures_text() {
        if text::font_count() == 0 {
            eprintln!("no system fonts; skipping");
            return;
        }
        let wide = text::measure("WWWW", 16.0);
        let narrow = text::measure("iiii", 16.0);
        assert!(wide.width > narrow.width && narrow.width > 0.0);

        let mut scene = Scene::new(Size::new(80.0, 24.0));
        scene.text(sidebar_core::Point::new(2.0, 2.0), "Hello", 16.0, Color::WHITE);
        let pm = rasterize(&scene, 1.0);
        assert!(pm.data.chunks_exact(4).any(|px| px[3] > 128), "glyphs drew pixels");
    }

    #[test]
    fn bgra_conversion() {
        let pm = Pixmap { width: 1, height: 1, data: vec![200, 100, 50, 128] };
        let mut out = [0; 4];
        pm.write_bgra(&mut out, AlphaMode::Premultiplied);
        assert_eq!(out, [25, 50, 100, 128]);
        pm.write_bgra(&mut out, AlphaMode::Opaque);
        assert_eq!(out, [50, 100, 200, 255]);
    }

    #[test]
    fn fills_scaled_rect() {
        let mut scene = Scene::new(Size::new(4.0, 4.0));
        scene.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::WHITE, 0.0);
        let pm = rasterize(&scene, 2.0);
        assert_eq!((pm.width, pm.height), (8, 8));
        assert_eq!(&pm.data[0..4], &[255, 255, 255, 255]);
        let outside = ((2 * 8) * 4) as usize;
        assert_eq!(&pm.data[outside..outside + 4], &[0, 0, 0, 0]);
    }
}
