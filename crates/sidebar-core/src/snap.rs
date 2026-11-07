//! Edge snapping.
//!
//! Pure functions: given a rect being dragged and the area of the monitor it
//! is on, decide which edge (if any) it should snap to, and where a dock on
//! that edge lives.

use crate::edge::Edge;
use crate::geometry::Rect;

/// Distance from each side of `rect` to the matching side of `area`.
/// Negative when `rect` pokes outside `area`.
pub fn edge_distances(rect: Rect, area: Rect) -> [(Edge, f32); 4] {
    [
        (Edge::Left, rect.x - area.x),
        (Edge::Right, area.right() - rect.right()),
        (Edge::Top, rect.y - area.y),
        (Edge::Bottom, area.bottom() - rect.bottom()),
    ]
}

pub fn nearest_edge(rect: Rect, area: Rect) -> (Edge, f32) {
    edge_distances(rect, area)
        .into_iter()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .expect("four edges")
}

/// The edge `rect` should snap to, if it is within `threshold` of one.
pub fn snap_edge(rect: Rect, area: Rect, threshold: f32) -> Option<Edge> {
    let (edge, distance) = nearest_edge(rect, area);
    (distance <= threshold).then_some(edge)
}

/// The full-length strip of `thickness` along `edge` of `area`.
pub fn dock_rect(area: Rect, edge: Edge, thickness: f32) -> Rect {
    let t = thickness.min(match edge.orientation() {
        crate::Orientation::Vertical => area.width,
        crate::Orientation::Horizontal => area.height,
    });
    match edge {
        Edge::Left => Rect::new(area.x, area.y, t, area.height),
        Edge::Right => Rect::new(area.right() - t, area.y, t, area.height),
        Edge::Top => Rect::new(area.x, area.y, area.width, t),
        Edge::Bottom => Rect::new(area.x, area.bottom() - t, area.width, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);

    #[test]
    fn snaps_when_close_to_edge() {
        let r = Rect::new(10.0, 400.0, 100.0, 100.0);
        assert_eq!(snap_edge(r, SCREEN, 24.0), Some(Edge::Left));
        let r = Rect::new(1815.0, 400.0, 100.0, 100.0);
        assert_eq!(snap_edge(r, SCREEN, 24.0), Some(Edge::Right));
        let r = Rect::new(800.0, 1000.0, 100.0, 100.0);
        assert_eq!(snap_edge(r, SCREEN, 24.0), Some(Edge::Bottom));
    }

    #[test]
    fn does_not_snap_in_the_middle() {
        let r = Rect::new(900.0, 500.0, 100.0, 100.0);
        assert_eq!(snap_edge(r, SCREEN, 24.0), None);
    }

    #[test]
    fn snaps_when_dragged_past_edge() {
        let r = Rect::new(-50.0, 500.0, 100.0, 100.0);
        assert_eq!(snap_edge(r, SCREEN, 24.0), Some(Edge::Left));
    }

    #[test]
    fn dock_rect_hugs_edge() {
        assert_eq!(dock_rect(SCREEN, Edge::Right, 48.0), Rect::new(1872.0, 0.0, 48.0, 1080.0));
        assert_eq!(dock_rect(SCREEN, Edge::Top, 32.0), Rect::new(0.0, 0.0, 1920.0, 32.0));
    }
}
