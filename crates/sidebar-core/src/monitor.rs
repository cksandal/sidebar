//! Monitor description, as reported by a backend.

use crate::geometry::Rect;

/// Backend-assigned, stable for the lifetime of a monitor connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonitorId(pub u32);

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// Human-readable connector/output name, e.g. "DP-1". Informational only.
    pub name: String,
    /// Full monitor area in global logical coordinates.
    pub bounds: Rect,
    /// Area not reserved by other panels/docks. Equal to `bounds` when the
    /// backend cannot tell.
    pub work_area: Rect,
    /// Logical-to-physical pixel ratio.
    pub scale: f32,
    pub primary: bool,
}
