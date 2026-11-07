//! Stacking widgets inside a dock.

use crate::edge::Orientation;
use crate::geometry::Rect;

/// Stacks items with the given main-axis `extents` inside `area`.
///
/// Items that overflow the dock are still laid out; the shell scrolls
/// overfull docks by shifting these rects, and the renderer clips.
pub fn stack(
    area: Rect,
    orientation: Orientation,
    extents: &[f32],
    padding: f32,
    spacing: f32,
) -> Vec<Rect> {
    let inner = area.inset(padding);
    let mut cursor = 0.0;
    extents
        .iter()
        .map(|&extent| {
            let rect = match orientation {
                Orientation::Vertical => Rect::new(inner.x, inner.y + cursor, inner.width, extent),
                Orientation::Horizontal => Rect::new(inner.x + cursor, inner.y, extent, inner.height),
            };
            cursor += extent + spacing;
            rect
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_stack() {
        let rects = stack(Rect::new(0.0, 0.0, 48.0, 500.0), Orientation::Vertical, &[20.0, 30.0], 4.0, 2.0);
        assert_eq!(rects, vec![Rect::new(4.0, 4.0, 40.0, 20.0), Rect::new(4.0, 26.0, 40.0, 30.0)]);
    }

    #[test]
    fn horizontal_stack() {
        let rects = stack(Rect::new(0.0, 0.0, 500.0, 32.0), Orientation::Horizontal, &[50.0, 10.0], 0.0, 5.0);
        assert_eq!(rects, vec![Rect::new(0.0, 0.0, 50.0, 32.0), Rect::new(55.0, 0.0, 10.0, 32.0)]);
    }
}
