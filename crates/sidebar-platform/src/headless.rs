//! A backend with no display: fake monitors, surfaces kept in memory,
//! frames optionally rasterized to PPM files. Used for tests, CI, and
//! developing core/widget logic without a running compositor.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use sidebar_core::{MonitorId, MonitorInfo, Rect, Scene, SurfaceKind, SurfaceSpec};

use crate::{Capabilities, Platform, PlatformError, PlatformEvent, Result, SurfaceId};

pub struct HeadlessPlatform {
    monitors: Vec<MonitorInfo>,
    surfaces: BTreeMap<SurfaceId, SurfaceSpec>,
    next_id: u64,
    events: VecDeque<PlatformEvent>,
    dump_dir: Option<PathBuf>,
}

impl HeadlessPlatform {
    /// One 1920x1080 monitor at scale 1.
    pub fn new() -> Self {
        let bounds = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        Self::with_monitors(vec![MonitorInfo {
            id: MonitorId(1),
            name: "HEADLESS-1".into(),
            bounds,
            work_area: bounds,
            scale: 1.0,
            primary: true,
        }])
    }

    pub fn with_monitors(monitors: Vec<MonitorInfo>) -> Self {
        Self { monitors, surfaces: BTreeMap::new(), next_id: 1, events: VecDeque::new(), dump_dir: None }
    }

    /// Renders at `scale` physical pixels per logical pixel (e.g. 2 for
    /// sharp screenshots).
    pub fn with_scale(mut self, scale: f32) -> Self {
        for m in &mut self.monitors {
            m.scale = scale;
        }
        self
    }

    /// Write each presented frame to `<dir>/surface-<id>.ppm`.
    pub fn with_dump_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dump_dir = Some(dir.into());
        self
    }

    /// Queue an event to be returned by the next `poll_events`.
    pub fn push_event(&mut self, event: PlatformEvent) {
        self.events.push_back(event);
    }

    pub fn surfaces(&self) -> impl Iterator<Item = (&SurfaceId, &SurfaceSpec)> {
        self.surfaces.iter()
    }

    fn describe(spec: &SurfaceSpec) -> String {
        match &spec.kind {
            SurfaceKind::Dock { edge, thickness, reserve_space } => {
                format!("dock {edge} {thickness}px reserve={reserve_space} above={} on monitor {}", spec.above, spec.monitor.0)
            }
            SurfaceKind::Floating { rect } => {
                format!("floating at ({}, {}) on monitor {}", rect.x, rect.y, spec.monitor.0)
            }
            SurfaceKind::Window { title } => {
                format!("window {title:?} {}x{} on monitor {}", spec.size.width, spec.size.height, spec.monitor.0)
            }
        }
    }
}

impl Default for HeadlessPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for HeadlessPlatform {
    fn name(&self) -> &'static str {
        "headless"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { reserve_space: true, free_positioning: true, global_pointer: true, transparency: true }
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.clone()
    }

    fn create_surface(&mut self, spec: &SurfaceSpec) -> Result<SurfaceId> {
        let id = SurfaceId(self.next_id);
        self.next_id += 1;
        eprintln!("[headless] create {id}: {}", Self::describe(spec));
        self.surfaces.insert(id, spec.clone());
        Ok(id)
    }

    fn configure_surface(&mut self, surface: SurfaceId, spec: &SurfaceSpec) -> Result<()> {
        let slot = self
            .surfaces
            .get_mut(&surface)
            .ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
        eprintln!("[headless] configure {surface}: {}", Self::describe(spec));
        *slot = spec.clone();
        Ok(())
    }

    fn destroy_surface(&mut self, surface: SurfaceId) {
        eprintln!("[headless] destroy {surface}");
        self.surfaces.remove(&surface);
    }

    fn present(&mut self, surface: SurfaceId, scene: &Scene) -> Result<()> {
        let spec = self
            .surfaces
            .get(&surface)
            .ok_or_else(|| PlatformError::Backend(format!("unknown surface {surface}")))?;
        eprintln!(
            "[headless] present {surface}: {}x{} logical, {} draw commands",
            scene.size.width,
            scene.size.height,
            scene.cmds.len()
        );
        if let Some(dir) = &self.dump_dir {
            let scale = self.monitors.iter().find(|m| m.id == spec.monitor).map_or(1.0, |m| m.scale);
            let path = dir.join(format!("surface-{}.ppm", surface.0));
            sidebar_render::rasterize(scene, scale)
                .write_ppm(&path)
                .map_err(|e| PlatformError::Backend(format!("writing {}: {e}", path.display())))?;
        }
        Ok(())
    }

    fn poll_events(&mut self, timeout: Duration) -> Vec<PlatformEvent> {
        if self.events.is_empty() {
            std::thread::sleep(timeout);
        }
        self.events.drain(..).collect()
    }
}
