//! The sidebar model: which widgets exist, where they are placed, and which
//! surfaces the platform needs to provide for them.
//!
//! The shell never creates windows. It *describes* the surfaces it wants
//! ([`Shell::surfaces`]) and renders [`Scene`]s for them; the runtime
//! reconciles those descriptions against a `Platform` backend.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use crate::config::Config;
use crate::edge::{Edge, Orientation};
use crate::geometry::{Point, Rect, Size};
use crate::layout;
use crate::monitor::{MonitorId, MonitorInfo};
use crate::render::Scene;
use crate::snap;
use crate::text::{ApproxText, TextMeasure};
use crate::widget::{DrawCtx, InputEvent, MeasureCtx, PointerButton, UpdateCtx, Widget, WidgetId};

/// How far (logical px) the pointer must move with the primary button held
/// before a press on a widget turns into a drag.
pub const DRAG_THRESHOLD: f32 = 6.0;

/// One dock = one edge of one monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DockKey {
    pub monitor: MonitorId,
    pub edge: Edge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    Docked(DockKey),
    /// `rect` is in global logical coordinates.
    Floating { monitor: MonitorId, rect: Rect },
}

/// A placement that survives restarts and backend changes: the monitor is
/// identified by name (backend ids are not stable), floating rects are
/// relative to their monitor, and docked widgets remember their position
/// in the dock.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedPlacement {
    pub monitor: String,
    pub spot: SavedSpot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SavedSpot {
    Docked { edge: Edge, index: usize },
    /// Relative to the monitor's top-left corner.
    Floating { rect: Rect },
}

/// What a platform surface is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceTarget {
    Dock(DockKey),
    Floating(WidgetId),
    /// The widget being dragged, following the pointer.
    DragPreview(WidgetId),
    /// A window the app opens itself (e.g. settings); never in the shell's
    /// surface list.
    Panel,
}

/// How the platform should place a surface. Docks are described by edge
/// and thickness rather than absolute coordinates, because some platforms
/// (e.g. Wayland layer-shell) only let clients *anchor* to edges.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceKind {
    Dock { edge: Edge, thickness: f32, reserve_space: bool },
    Floating { rect: Rect },
    /// An ordinary top-level window of the spec's size, managed and
    /// decorated by the window manager, centered on its monitor, and able
    /// to take keyboard focus.
    Window { title: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceSpec {
    pub target: SurfaceTarget,
    pub monitor: MonitorId,
    pub kind: SurfaceKind,
    pub size: Size,
    /// Stacked above other windows; otherwise below ordinary windows.
    pub above: bool,
}

struct Entry {
    widget: Box<dyn Widget>,
    placement: Placement,
    /// Position within its dock; lower comes first.
    order: u64,
}

struct Drag {
    id: WidgetId,
    /// Pointer position relative to the widget's top-left at drag start.
    grab: Point,
    /// Current global rect of the dragged widget.
    rect: Rect,
}

/// A primary-button press on a widget that may become a drag.
#[derive(Clone, Copy)]
struct Press {
    id: WidgetId,
    target: SurfaceTarget,
    /// Global pointer position at the press.
    at: Point,
}

pub struct Shell {
    config: Config,
    text: Box<dyn TextMeasure>,
    monitors: Vec<MonitorInfo>,
    entries: BTreeMap<WidgetId, Entry>,
    next_id: u64,
    next_order: u64,
    press: Option<Press>,
    drag: Option<Drag>,
    dirty: BTreeSet<SurfaceTarget>,
    layout_changed: bool,
    /// Dock sizes the platform reported where they differ from the
    /// monitor-long strip asked for (shortened by a taskbar or panel).
    dock_sizes: BTreeMap<DockKey, Size>,
    /// How far each dock is scrolled, when its widgets don't fit.
    scroll: BTreeMap<DockKey, f32>,
}

/// How far one wheel notch scrolls an overfull dock.
const SCROLL_STEP: f32 = 48.0;

impl Shell {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            text: Box::new(ApproxText),
            monitors: Vec::new(),
            entries: BTreeMap::new(),
            next_id: 1,
            next_order: 0,
            press: None,
            drag: None,
            dirty: BTreeSet::new(),
            layout_changed: false,
            dock_sizes: BTreeMap::new(),
            scroll: BTreeMap::new(),
        }
    }

    /// Replace the text measurer (the default is [`ApproxText`]). Call
    /// before adding widgets, or layouts computed so far will be stale.
    pub fn set_text_measure(&mut self, text: Box<dyn TextMeasure>) {
        self.text = text;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn monitors(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    pub fn monitor(&self, id: MonitorId) -> Option<&MonitorInfo> {
        self.monitors.iter().find(|m| m.id == id)
    }

    fn primary(&self) -> Option<&MonitorInfo> {
        self.monitors.iter().find(|m| m.primary).or_else(|| self.monitors.first())
    }

    fn monitor_at(&self, p: Point) -> Option<&MonitorInfo> {
        self.monitors.iter().find(|m| m.bounds.contains(p))
    }

    /// Replace the monitor set. Widgets on monitors that disappeared move to
    /// the primary monitor, keeping their edge.
    pub fn set_monitors(&mut self, monitors: Vec<MonitorInfo>) {
        self.monitors = monitors;
        let Some(fallback) = self.primary().map(|m| m.id) else { return };
        let known: BTreeSet<MonitorId> = self.monitors.iter().map(|m| m.id).collect();
        for entry in self.entries.values_mut() {
            match &mut entry.placement {
                Placement::Docked(key) if !known.contains(&key.monitor) => key.monitor = fallback,
                Placement::Floating { monitor, .. } if !known.contains(monitor) => *monitor = fallback,
                _ => {}
            }
        }
        self.dirty.extend(self.surfaces().into_iter().map(|s| s.target));
    }

    /// Adds a widget. `None` docks it to the configured default edge of the
    /// primary monitor.
    pub fn add_widget(&mut self, widget: Box<dyn Widget>, placement: Option<Placement>) -> WidgetId {
        let id = WidgetId(self.next_id);
        self.next_id += 1;
        let placement = placement.unwrap_or_else(|| {
            let monitor = self.primary().map(|m| m.id).unwrap_or(MonitorId(0));
            Placement::Docked(DockKey { monitor, edge: self.config.default_edge })
        });
        let order = self.take_order();
        self.entries.insert(id, Entry { widget, placement, order });
        self.dirty.insert(target_of(id, placement));
        id
    }

    pub fn placement(&self, id: WidgetId) -> Option<Placement> {
        self.entries.get(&id).map(|e| e.placement)
    }

    fn take_order(&mut self) -> u64 {
        self.next_order += 1;
        self.next_order
    }

    fn is_dragging(&self, id: WidgetId) -> bool {
        self.drag.as_ref().is_some_and(|d| d.id == id)
    }

    /// Global rect of a dock.
    pub fn dock_area(&self, key: DockKey) -> Option<Rect> {
        let monitor = self.monitor(key.monitor)?;
        Some(snap::dock_rect(monitor.bounds, key.edge, self.config.thickness))
    }

    /// Every surface the platform should currently provide.
    ///
    /// While a widget is dragged, its own surface stays exactly where it was
    /// (a dock reflows without it, a floating surface shows a ghost) and a
    /// separate preview follows the pointer. The surface that was pressed
    /// holds the pointer grab; moving or destroying it mid-drag would make
    /// its pointer coordinates compositor-dependent or end the grab.
    pub fn surfaces(&self) -> Vec<SurfaceSpec> {
        let mut docks = BTreeSet::new();
        let mut specs = Vec::new();
        if let Some(drag) = &self.drag {
            let monitor = self.monitor_at(drag.rect.center()).or_else(|| self.primary()).map(|m| m.id);
            if let Some(monitor) = monitor {
                specs.push(SurfaceSpec {
                    target: SurfaceTarget::DragPreview(drag.id),
                    monitor,
                    kind: SurfaceKind::Floating { rect: drag.rect },
                    size: drag.rect.size(),
                    // Whatever the setting, a dragged widget must stay visible.
                    above: true,
                });
            }
        }
        for (&id, entry) in &self.entries {
            match entry.placement {
                Placement::Docked(key) => {
                    docks.insert(key);
                }
                Placement::Floating { monitor, rect } => specs.push(SurfaceSpec {
                    target: SurfaceTarget::Floating(id),
                    monitor,
                    kind: SurfaceKind::Floating { rect },
                    size: rect.size(),
                    above: self.config.always_on_top,
                }),
            }
        }
        for key in docks {
            let Some(size) = self.surface_size(SurfaceTarget::Dock(key)) else { continue };
            specs.push(SurfaceSpec {
                target: SurfaceTarget::Dock(key),
                monitor: key.monitor,
                kind: SurfaceKind::Dock {
                    edge: key.edge,
                    thickness: self.config.thickness,
                    reserve_space: self.config.reserve_space,
                },
                size,
                above: self.config.always_on_top,
            });
        }
        specs
    }

    /// The platform gave a surface a different size than asked for. Only
    /// docks follow it: the platform may shorten them to stay clear of a
    /// taskbar or another panel, and their widgets are laid out to fit.
    pub fn surface_resized(&mut self, target: SurfaceTarget, size: Size) {
        if let SurfaceTarget::Dock(key) = target
            && self.surface_size(target) != Some(size)
        {
            self.dock_sizes.insert(key, size);
            self.dirty.insert(target);
        }
    }

    fn surface_size(&self, target: SurfaceTarget) -> Option<Size> {
        match target {
            SurfaceTarget::Dock(key) => self.dock_sizes.get(&key).copied().or_else(|| self.dock_area(key).map(|r| r.size())),
            SurfaceTarget::Floating(id) => match self.entries.get(&id)?.placement {
                Placement::Floating { rect, .. } => Some(rect.size()),
                Placement::Docked(_) => None,
            },
            SurfaceTarget::DragPreview(id) => self.drag.as_ref().filter(|d| d.id == id).map(|d| d.rect.size()),
            SurfaceTarget::Panel => None,
        }
    }

    /// Widget rects in the surface's local coordinates.
    pub fn layout(&self, target: SurfaceTarget) -> Vec<(WidgetId, Rect)> {
        let Some(size) = self.surface_size(target) else { return Vec::new() };
        let local = Rect::from_origin_size(Point::default(), size);
        match target {
            SurfaceTarget::Floating(id) | SurfaceTarget::DragPreview(id) => vec![(id, local)],
            SurfaceTarget::Panel => Vec::new(),
            SurfaceTarget::Dock(key) => {
                let (members, extents) = self.dock_members(key);
                let offset = self.scroll_offset(key, &extents);
                let shift = match key.edge.orientation() {
                    Orientation::Vertical => Point::new(0.0, -offset),
                    Orientation::Horizontal => Point::new(-offset, 0.0),
                };
                let rects = layout::stack(local, key.edge.orientation(), &extents, self.config.padding, self.config.spacing);
                members.into_iter().zip(rects.into_iter().map(|r| r.translate(shift))).collect()
            }
        }
    }

    /// The widgets in dock `key`, in order, with their lengths along it.
    fn dock_members(&self, key: DockKey) -> (Vec<WidgetId>, Vec<f32>) {
        let orientation = key.edge.orientation();
        let cross = (self.config.thickness - 2.0 * self.config.padding).max(0.0);
        let mut members: Vec<(WidgetId, &Entry)> = self
            .entries
            .iter()
            .filter(|(id, e)| !self.is_dragging(**id) && e.placement == Placement::Docked(key))
            .map(|(id, e)| (*id, e))
            .collect();
        members.sort_by_key(|(_, e)| e.order);
        let ctx = MeasureCtx { text: &*self.text, theme: &self.config.theme };
        members.into_iter().map(|(id, e)| (id, e.widget.measure(&ctx, orientation, cross))).unzip()
    }

    /// The dock's length and the length of its contents.
    fn dock_lengths(&self, key: DockKey, extents: &[f32]) -> (f32, f32) {
        let size = self.surface_size(SurfaceTarget::Dock(key)).unwrap_or_default();
        let length = match key.edge.orientation() {
            Orientation::Vertical => size.height,
            Orientation::Horizontal => size.width,
        };
        let gaps = extents.len().saturating_sub(1) as f32 * self.config.spacing;
        (length, extents.iter().sum::<f32>() + gaps + 2.0 * self.config.padding)
    }

    /// The dock's scroll position, kept within its contents (which may
    /// have shrunk since it was scrolled).
    fn scroll_offset(&self, key: DockKey, extents: &[f32]) -> f32 {
        let (length, content) = self.dock_lengths(key, extents);
        self.scroll.get(&key).copied().unwrap_or(0.0).clamp(0.0, (content - length).max(0.0))
    }

    /// Scrolls an overfull dock by `notches` wheel steps; false if it
    /// fits and so can't scroll.
    fn scroll_dock(&mut self, key: DockKey, notches: f32) -> bool {
        let (_, extents) = self.dock_members(key);
        let (length, content) = self.dock_lengths(key, &extents);
        if content <= length {
            return false;
        }
        let old = self.scroll_offset(key, &extents);
        let new = (old + notches * SCROLL_STEP).clamp(0.0, content - length);
        if new != old {
            self.scroll.insert(key, new);
            self.dirty.insert(SurfaceTarget::Dock(key));
        }
        true
    }

    /// A thin scroll bar along the inner side of an overfull dock.
    fn draw_scroll_bar(&self, scene: &mut Scene, key: DockKey) {
        let (_, extents) = self.dock_members(key);
        let (length, content) = self.dock_lengths(key, &extents);
        if content <= length {
            return;
        }
        let offset = self.scroll_offset(key, &extents);
        let (start, len) = (offset / content * length, (length / content * length).max(24.0));
        let (w, side) = (3.0, 1.5);
        let rect = match key.edge {
            Edge::Left => Rect::new(scene.size.width - w - side, start, w, len),
            Edge::Right => Rect::new(side, start, w, len),
            Edge::Top => Rect::new(start, scene.size.height - w - side, len, w),
            Edge::Bottom => Rect::new(start, side, len, w),
        };
        scene.fill_rect(rect, self.config.theme.foreground.with_alpha(90), w / 2.0);
    }

    pub fn render(&self, target: SurfaceTarget) -> Option<Scene> {
        let mut scene = Scene::new(self.surface_size(target)?);
        let theme = &self.config.theme;
        scene.clear(theme.background);
        for (id, bounds) in self.layout(target) {
            let Some(entry) = self.entries.get(&id) else { continue };
            entry.widget.draw(&mut DrawCtx { scene: &mut scene, bounds, theme, text: &*self.text });
        }
        if let SurfaceTarget::Dock(key) = target {
            self.draw_scroll_bar(&mut scene, key);
        }
        if let SurfaceTarget::Floating(id) = target
            && self.is_dragging(id)
        {
            // The ghost left behind by a dragged floating widget.
            scene.fill_rect(Rect::from_origin_size(Point::default(), scene.size), theme.background.with_alpha(200), 0.0);
        }
        Some(scene)
    }

    /// Advance all widgets; widgets that changed mark their surface dirty.
    pub fn tick(&mut self, now: SystemTime) {
        let ctx = UpdateCtx { now };
        let mut changed = Vec::new();
        for (&id, entry) in self.entries.iter_mut() {
            if entry.widget.update(&ctx) {
                changed.push(id);
            }
        }
        for id in changed {
            if let Some(entry) = self.entries.get(&id) {
                self.dirty.insert(target_of(id, entry.placement));
            }
            if self.is_dragging(id) {
                self.dirty.insert(SurfaceTarget::DragPreview(id));
            }
        }
    }

    /// Surfaces that need a redraw since the last call.
    pub fn take_dirty(&mut self) -> BTreeSet<SurfaceTarget> {
        std::mem::take(&mut self.dirty)
    }

    /// Handles input from a surface: drives dragging and routes everything
    /// else to the widget under the pointer. `event` is surface-local;
    /// `global` is the same position in global coordinates when the
    /// platform knows it exactly (dragging only uses global positions).
    ///
    /// A primary press on a widget followed by a move of more than
    /// [`DRAG_THRESHOLD`] starts a drag; releasing drops the widget, and a
    /// secondary click during the drag cancels it.
    pub fn handle_input(&mut self, target: SurfaceTarget, event: &InputEvent, global: Option<Point>) {
        if self.drag.is_some() {
            match *event {
                InputEvent::PointerMoved { .. } => {
                    if let Some(g) = global {
                        self.drag_to(g);
                    }
                }
                InputEvent::PointerButton { button: PointerButton::Primary, pressed: false, .. } => {
                    if let Some(g) = global {
                        self.drag_to(g);
                    }
                    self.end_drag();
                }
                InputEvent::PointerButton { button: PointerButton::Secondary, pressed: true, .. } => self.cancel_drag(),
                // Widgets don't see input while something is being dragged.
                _ => {}
            }
            return;
        }

        match *event {
            InputEvent::PointerMoved { .. } => {
                if let (Some(press), Some(g)) = (self.press, global)
                    && distance(press.at, g) > DRAG_THRESHOLD
                {
                    self.press = None;
                    // Let the widget forget the press it saw.
                    self.route(press.target, &InputEvent::PointerLeft, Some(press.id));
                    if self.begin_drag(press.id, press.at) {
                        self.drag_to(g);
                    }
                    return;
                }
            }
            // The widget under the pointer gets the wheel first (a volume
            // control); otherwise it scrolls a dock whose widgets don't fit.
            InputEvent::Scroll { dx, dy, .. } => {
                if !self.route(target, event, None)
                    && let SurfaceTarget::Dock(key) = target
                {
                    self.scroll_dock(key, if dy != 0.0 { dy } else { dx });
                }
                return;
            }
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed } => {
                self.press = match (pressed, global, self.hit(target, pos)) {
                    (true, Some(at), Some((id, _))) => Some(Press { id, target, at }),
                    _ => None,
                };
            }
            _ => {}
        }
        self.route(target, event, None);
    }

    fn hit(&self, target: SurfaceTarget, pos: Point) -> Option<(WidgetId, Rect)> {
        self.layout(target).into_iter().find(|(_, r)| r.contains(pos))
    }

    /// Delivers `event` to the widget under its position, or to `only`;
    /// true if the widget used it.
    fn route(&mut self, target: SurfaceTarget, event: &InputEvent, only: Option<WidgetId>) -> bool {
        let hit = match (only, event.position()) {
            (Some(id), _) => self.layout(target).into_iter().find(|(w, _)| *w == id),
            (None, Some(pos)) => self.hit(target, pos),
            (None, None) => None,
        };
        let Some((id, bounds)) = hit else { return false };
        let before = self.floating_extent(id);
        if let Some(entry) = self.entries.get_mut(&id)
            && entry.widget.on_input(event, bounds)
        {
            self.dirty.insert(target);
            self.refit_floating(id, before);
            return true;
        }
        false
    }

    /// For a floating widget: its rect and its measured height.
    fn floating_extent(&self, id: WidgetId) -> Option<(Rect, f32)> {
        let entry = self.entries.get(&id)?;
        let Placement::Floating { rect, .. } = entry.placement else { return None };
        let ctx = MeasureCtx { text: &*self.text, theme: &self.config.theme };
        Some((rect, entry.widget.measure(&ctx, Orientation::Vertical, rect.width)))
    }

    /// A floating widget keeps its rect, so when it changes its own height
    /// (e.g. expands a section) the rect follows, if it was the widget's
    /// full height before. Rects of another height (dropped from a
    /// horizontal dock) are left alone. Stays on its monitor.
    fn refit_floating(&mut self, id: WidgetId, before: Option<(Rect, f32)>) {
        let (Some((rect, old)), Some((_, new))) = (before, self.floating_extent(id)) else { return };
        if (rect.height - old).abs() > 0.5 || (new - old).abs() <= 0.5 {
            return;
        }
        let Some(entry) = self.entries.get_mut(&id) else { return };
        let Placement::Floating { monitor, rect } = &mut entry.placement else { return };
        let bounds = self.monitors.iter().find(|m| m.id == *monitor).map(|m| m.bounds);
        let grown = Rect { height: new, ..*rect };
        *rect = bounds.map_or(grown, |b| grown.clamp_within(b));
        self.layout_changed = true;
    }

    /// Global rect a widget currently occupies (the preview's while dragged).
    pub fn global_rect(&self, id: WidgetId) -> Option<Rect> {
        if let Some(drag) = self.drag.as_ref().filter(|d| d.id == id) {
            return Some(drag.rect);
        }
        let entry = self.entries.get(&id)?;
        match entry.placement {
            Placement::Floating { rect, .. } => Some(rect),
            Placement::Docked(key) => {
                let area = self.dock_area(key)?;
                let (_, local) = self.layout(SurfaceTarget::Dock(key)).into_iter().find(|(w, _)| *w == id)?;
                Some(local.translate(area.origin()))
            }
        }
    }

    /// Start dragging `id`; `pointer` is in global logical coordinates.
    pub fn begin_drag(&mut self, id: WidgetId, pointer: Point) -> bool {
        let Some(rect) = self.global_rect(id) else { return false };
        self.cancel_drag();
        self.dirty.insert(target_of(id, self.entries[&id].placement));
        self.drag = Some(Drag { id, grab: pointer - rect.origin(), rect });
        self.dirty.insert(SurfaceTarget::DragPreview(id));
        true
    }

    pub fn drag_to(&mut self, pointer: Point) {
        if let Some(drag) = &mut self.drag {
            drag.rect = Rect::from_origin_size(pointer - drag.grab, drag.rect.size());
            self.dirty.insert(SurfaceTarget::DragPreview(drag.id));
        }
    }

    /// Drop the dragged widget: snap it to an edge if close enough,
    /// otherwise leave it floating (or force-snap if floating is disallowed).
    /// Dropped on a dock, it is inserted at the pointer's position.
    pub fn end_drag(&mut self) -> Option<Placement> {
        let (id, rect) = self.drag.as_ref().map(|d| (d.id, d.rect))?;
        let Some(monitor) = self.monitor_at(rect.center()).or_else(|| self.primary()) else {
            self.cancel_drag();
            return None;
        };
        let (monitor_id, bounds) = (monitor.id, monitor.bounds);

        let placement = match snap::snap_edge(rect, bounds, self.config.snap_threshold) {
            Some(edge) => Placement::Docked(DockKey { monitor: monitor_id, edge }),
            None if self.config.allow_floating => {
                Placement::Floating { monitor: monitor_id, rect: rect.clamp_within(bounds) }
            }
            None => {
                let (edge, _) = snap::nearest_edge(rect, bounds);
                Placement::Docked(DockKey { monitor: monitor_id, edge })
            }
        };
        // Computed while the drag is active, so the layout excludes `id`.
        let order = match placement {
            Placement::Docked(key) => self.dock_order_with(key, id, rect.center()),
            Placement::Floating { .. } => Vec::new(),
        };

        self.drag = None;
        let entry = self.entries.get_mut(&id)?;
        let old = std::mem::replace(&mut entry.placement, placement);
        self.dirty.insert(target_of(id, old));
        self.dirty.insert(target_of(id, placement));
        for member in order {
            let order = self.take_order();
            if let Some(entry) = self.entries.get_mut(&member) {
                entry.order = order;
            }
        }
        self.layout_changed = true;
        Some(placement)
    }

    /// Abandon the drag; the widget goes back where it was.
    pub fn cancel_drag(&mut self) {
        if let Some(drag) = self.drag.take()
            && let Some(entry) = self.entries.get(&drag.id)
        {
            self.dirty.insert(target_of(drag.id, entry.placement));
        }
    }

    /// Members of dock `key` in order, with `id` inserted where `at` falls
    /// along the dock's main axis.
    fn dock_order_with(&self, key: DockKey, id: WidgetId, at: Point) -> Vec<WidgetId> {
        let origin = self.dock_area(key).map_or(Point::default(), |r| r.origin());
        let along = |p: Point| match key.edge.orientation() {
            Orientation::Vertical => p.y,
            Orientation::Horizontal => p.x,
        };
        let mut members: Vec<WidgetId> = Vec::new();
        let mut index = 0;
        for (member, rect) in self.layout(SurfaceTarget::Dock(key)) {
            if along(rect.translate(origin).center()) < along(at) {
                index += 1;
            }
            members.push(member);
        }
        members.insert(index, id);
        members
    }

    /// Whether a drop changed any placement since the last call.
    pub fn take_layout_changed(&mut self) -> bool {
        std::mem::take(&mut self.layout_changed)
    }

    /// Describes `id`'s placement in a form that can be saved and restored
    /// with [`Shell::restore_placement`].
    pub fn saved_placement(&self, id: WidgetId) -> Option<SavedPlacement> {
        let entry = self.entries.get(&id)?;
        let monitor = self.monitor(placement_monitor(entry.placement))?;
        let spot = match entry.placement {
            Placement::Docked(key) => {
                let index = self.layout(SurfaceTarget::Dock(key)).iter().position(|(w, _)| *w == id)?;
                SavedSpot::Docked { edge: key.edge, index }
            }
            Placement::Floating { rect, .. } => {
                SavedSpot::Floating { rect: rect.translate(Point::default() - monitor.bounds.origin()) }
            }
        };
        Some(SavedPlacement { monitor: monitor.name.clone(), spot })
    }

    /// Maps a saved placement onto the current monitors. A missing monitor
    /// falls back to the primary one; floating rects are kept on screen, and
    /// become docks if floating is not allowed. Dock positions are not
    /// applied here: add widgets in saved `index` order instead.
    pub fn restore_placement(&self, saved: &SavedPlacement) -> Option<Placement> {
        let monitor = self.monitors.iter().find(|m| m.name == saved.monitor).or_else(|| self.primary())?;
        Some(match saved.spot {
            SavedSpot::Docked { edge, .. } => Placement::Docked(DockKey { monitor: monitor.id, edge }),
            SavedSpot::Floating { rect } => {
                let rect = rect.translate(monitor.bounds.origin()).clamp_within(monitor.bounds);
                if self.config.allow_floating {
                    Placement::Floating { monitor: monitor.id, rect }
                } else {
                    let (edge, _) = snap::nearest_edge(rect, monitor.bounds);
                    Placement::Docked(DockKey { monitor: monitor.id, edge })
                }
            }
        })
    }
}

fn distance(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn target_of(id: WidgetId, placement: Placement) -> SurfaceTarget {
    match placement {
        Placement::Docked(key) => SurfaceTarget::Dock(key),
        Placement::Floating { .. } => SurfaceTarget::Floating(id),
    }
}

fn placement_monitor(placement: Placement) -> MonitorId {
    match placement {
        Placement::Docked(key) => key.monitor,
        Placement::Floating { monitor, .. } => monitor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Block;

    impl Widget for Block {
        fn name(&self) -> &str {
            "block"
        }
        fn measure(&self, _: &MeasureCtx<'_>, _: Orientation, _: f32) -> f32 {
            40.0
        }
        fn draw(&self, ctx: &mut DrawCtx<'_>) {
            ctx.scene.fill_rect(ctx.bounds, ctx.theme.accent, 0.0);
        }
    }

    fn shell(allow_floating: bool) -> Shell {
        let mut shell = Shell::new(Config { allow_floating, ..Config::default() });
        shell.set_monitors(vec![MonitorInfo {
            id: MonitorId(1),
            name: "TEST-1".into(),
            bounds: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            work_area: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            primary: true,
        }]);
        shell
    }

    /// 40px tall, 100px once clicked.
    struct Grows(bool);

    impl Widget for Grows {
        fn name(&self) -> &str {
            "grows"
        }
        fn measure(&self, _: &MeasureCtx<'_>, _: Orientation, _: f32) -> f32 {
            if self.0 { 100.0 } else { 40.0 }
        }
        fn on_input(&mut self, event: &InputEvent, _: Rect) -> bool {
            let released = matches!(event, InputEvent::PointerButton { pressed: false, .. });
            self.0 |= released;
            released
        }
        fn draw(&self, _: &mut DrawCtx<'_>) {}
    }

    #[test]
    fn floating_widget_follows_its_height() {
        let mut shell = shell(true);
        let id = shell.add_widget(Box::new(Grows(false)), None);
        let Placement::Floating { rect, .. } = drag(&mut shell, id, Point::new(900.0, 1010.0)) else { panic!("not floating") };
        assert_eq!(rect.height, 40.0);
        shell.take_layout_changed();

        let pos = Point::new(5.0, 5.0);
        let global = Some(rect.origin() + pos);
        let target = SurfaceTarget::Floating(id);
        for pressed in [true, false] {
            let event = InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed };
            shell.handle_input(target, &event, global);
        }
        let grown = shell.global_rect(id).unwrap();
        assert_eq!(grown.height, 100.0);
        // Kept on screen: it was near the bottom edge.
        assert!(grown.bottom() <= 1080.0 && grown.x == rect.x);
        assert!(shell.take_layout_changed());
    }

    #[test]
    fn overfull_docks_scroll_and_follow_their_reported_size() {
        let mut shell = shell(true);
        let ids: Vec<WidgetId> = (0..30).map(|_| shell.add_widget(Box::new(Block), None)).collect();
        let Some(Placement::Docked(key)) = shell.placement(ids[0]) else { panic!("not docked") };
        let target = SurfaceTarget::Dock(key);
        // A taskbar shortens the dock: 30 blocks need 30*40 + 29*6 + 2*6 = 1386px.
        let size = Size::new(160.0, 1032.0);
        shell.surface_resized(target, size);
        assert_eq!(shell.surfaces().iter().find(|s| s.target == target).map(|s| s.size), Some(size));
        let last = |shell: &Shell| shell.layout(target).last().map(|(_, r)| r.bottom());
        assert_eq!(last(&shell), Some(1380.0));

        let wheel = |shell: &mut Shell, dy| shell.handle_input(target, &InputEvent::Scroll { pos: Point::new(80.0, 500.0), dx: 0.0, dy }, None);
        wheel(&mut shell, 100.0);
        // Scrolled to the end, and no further.
        assert_eq!(last(&shell), Some(1032.0 - 6.0));
        assert!(shell.take_dirty().contains(&target));
        assert!(shell.render(target).is_some());
        wheel(&mut shell, -100.0);
        assert_eq!(shell.layout(target)[0].1.y, 6.0);

        // A dock that fits doesn't scroll.
        let mut small = self::shell(true);
        let id = small.add_widget(Box::new(Block), None);
        let Some(Placement::Docked(key)) = small.placement(id) else { panic!("not docked") };
        assert!(!small.scroll_dock(key, 3.0));
    }

    fn drag(shell: &mut Shell, id: WidgetId, to: Point) -> Placement {
        let start = shell.global_rect(id).unwrap().center();
        assert!(shell.begin_drag(id, start));
        shell.drag_to(to);
        shell.end_drag().unwrap()
    }

    #[test]
    fn widgets_dock_to_default_edge() {
        let mut shell = shell(true);
        let a = shell.add_widget(Box::new(Block), None);
        let b = shell.add_widget(Box::new(Block), None);
        let key = DockKey { monitor: MonitorId(1), edge: Edge::Right };
        assert_eq!(shell.placement(a), Some(Placement::Docked(key)));
        assert_eq!(shell.surfaces().len(), 1);
        let layout = shell.layout(SurfaceTarget::Dock(key));
        assert_eq!(layout.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![a, b]);
    }

    #[test]
    fn dragging_near_edge_snaps() {
        let mut shell = shell(true);
        let id = shell.add_widget(Box::new(Block), None);
        let placed = drag(&mut shell, id, Point::new(20.0, 500.0));
        assert_eq!(placed, Placement::Docked(DockKey { monitor: MonitorId(1), edge: Edge::Left }));
    }

    #[test]
    fn dragging_to_middle_floats() {
        let mut shell = shell(true);
        let id = shell.add_widget(Box::new(Block), None);
        assert!(matches!(drag(&mut shell, id, Point::new(960.0, 540.0)), Placement::Floating { .. }));
        assert!(shell.surfaces().iter().any(|s| s.target == SurfaceTarget::Floating(id)));
    }

    #[test]
    fn floating_disallowed_snaps_to_nearest() {
        let mut shell = shell(false);
        let id = shell.add_widget(Box::new(Block), None);
        let placed = drag(&mut shell, id, Point::new(900.0, 100.0));
        assert_eq!(placed, Placement::Docked(DockKey { monitor: MonitorId(1), edge: Edge::Top }));
    }

    fn press(shell: &mut Shell, target: SurfaceTarget, local: Point, global: Point, pressed: bool) {
        let event = InputEvent::PointerButton { pos: local, button: PointerButton::Primary, pressed };
        shell.handle_input(target, &event, Some(global));
    }

    fn motion(shell: &mut Shell, target: SurfaceTarget, local: Point, global: Point) {
        shell.handle_input(target, &InputEvent::PointerMoved { pos: local }, Some(global));
    }

    const RIGHT: DockKey = DockKey { monitor: MonitorId(1), edge: Edge::Right };

    /// Presses on `id` in the right dock and returns (target, local, global).
    fn press_on(shell: &mut Shell, id: WidgetId) -> (SurfaceTarget, Point, Point) {
        let target = SurfaceTarget::Dock(RIGHT);
        let global = shell.global_rect(id).unwrap().center();
        let local = global - shell.dock_area(RIGHT).unwrap().origin();
        press(shell, target, local, global, true);
        (target, local, global)
    }

    #[test]
    fn small_moves_do_not_start_a_drag() {
        let mut shell = shell(true);
        let id = shell.add_widget(Box::new(Block), None);
        let (target, local, global) = press_on(&mut shell, id);
        let nudge = Point::new(3.0, 2.0);
        motion(&mut shell, target, local + nudge, global + nudge);
        press(&mut shell, target, local + nudge, global + nudge, false);
        assert_eq!(shell.placement(id), Some(Placement::Docked(RIGHT)));
        assert!(!shell.take_layout_changed());
    }

    #[test]
    fn pointer_drag_moves_widget_and_keeps_source_dock() {
        let mut shell = shell(true);
        let a = shell.add_widget(Box::new(Block), None);
        let (target, local, global) = press_on(&mut shell, a);
        let to = Point::new(10.0, 300.0);
        motion(&mut shell, target, local + (to - global), to);

        // Mid-drag: a preview follows the pointer and the (now empty) source
        // dock is still wanted, because it holds the pointer grab.
        let targets: Vec<SurfaceTarget> = shell.surfaces().iter().map(|s| s.target).collect();
        assert!(targets.contains(&SurfaceTarget::DragPreview(a)));
        assert!(targets.contains(&target));
        assert!(shell.layout(target).is_empty());

        press(&mut shell, target, local + (to - global), to, false);
        assert_eq!(shell.placement(a), Some(Placement::Docked(DockKey { monitor: MonitorId(1), edge: Edge::Left })));
        assert!(shell.take_layout_changed());
        let targets: Vec<SurfaceTarget> = shell.surfaces().iter().map(|s| s.target).collect();
        assert_eq!(targets, vec![SurfaceTarget::Dock(DockKey { monitor: MonitorId(1), edge: Edge::Left })]);
    }

    #[test]
    fn dragged_floating_surface_stays_put() {
        let mut shell = shell(true);
        let a = shell.add_widget(Box::new(Block), None);
        drag(&mut shell, a, Point::new(500.0, 400.0));
        let spec = |shell: &Shell, t| shell.surfaces().into_iter().find(|s| s.target == t);
        let before = spec(&shell, SurfaceTarget::Floating(a)).unwrap();

        let start = shell.global_rect(a).unwrap().center();
        assert!(shell.begin_drag(a, start));
        shell.drag_to(start + Point::new(300.0, 100.0));
        assert_eq!(spec(&shell, SurfaceTarget::Floating(a)), Some(before));
        let preview = spec(&shell, SurfaceTarget::DragPreview(a)).unwrap();
        assert_eq!(preview.kind, SurfaceKind::Floating { rect: shell.global_rect(a).unwrap() });
        assert!(shell.take_dirty().contains(&SurfaceTarget::Floating(a)), "ghost is redrawn");
    }

    #[test]
    fn drop_inserts_at_pointer_position() {
        let mut shell = shell(true);
        let a = shell.add_widget(Box::new(Block), None);
        let b = shell.add_widget(Box::new(Block), None);
        let c = shell.add_widget(Box::new(Block), None);
        // Drag `c` between `a` (centered at y=26) and `b` (y=72).
        let (target, local, global) = press_on(&mut shell, c);
        let to = Point::new(global.x, 50.0);
        motion(&mut shell, target, local + (to - global), to);
        press(&mut shell, target, local + (to - global), to, false);
        let order: Vec<WidgetId> = shell.layout(SurfaceTarget::Dock(RIGHT)).into_iter().map(|(id, _)| id).collect();
        assert_eq!(order, vec![a, c, b]);
    }

    #[test]
    fn secondary_click_cancels_drag() {
        let mut shell = shell(true);
        let a = shell.add_widget(Box::new(Block), None);
        let (target, local, _) = press_on(&mut shell, a);
        let to = Point::new(960.0, 540.0);
        motion(&mut shell, target, local, to);
        let cancel = InputEvent::PointerButton { pos: local, button: PointerButton::Secondary, pressed: true };
        shell.handle_input(target, &cancel, Some(to));
        assert_eq!(shell.placement(a), Some(Placement::Docked(RIGHT)));
        assert_eq!(shell.surfaces().len(), 1);
    }

    #[test]
    fn saved_placements_round_trip() {
        let mut shell = shell(true);
        let a = shell.add_widget(Box::new(Block), None);
        let b = shell.add_widget(Box::new(Block), None);
        drag(&mut shell, b, Point::new(500.0, 400.0));

        let saved_a = shell.saved_placement(a).unwrap();
        assert_eq!(saved_a, SavedPlacement { monitor: "TEST-1".into(), spot: SavedSpot::Docked { edge: Edge::Right, index: 0 } });
        let saved_b = shell.saved_placement(b).unwrap();
        assert_eq!(shell.restore_placement(&saved_b), shell.placement(b));

        // Unknown monitors fall back to the primary one.
        let elsewhere = SavedPlacement { monitor: "GONE".into(), ..saved_a };
        assert_eq!(shell.restore_placement(&elsewhere), Some(Placement::Docked(RIGHT)));
    }
}
