//! Event loop: keeps the platform's surfaces in sync with the shell's
//! desired surfaces, routes platform events into the shell, and presents
//! frames for dirty surfaces. Also runs the app's own window (settings)
//! and the actions widgets request (reload, open settings).

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::Receiver;
use std::time::{Duration, SystemTime};

use sidebar_core::{InputEvent, Scene, Shell, Size, SurfaceKind, SurfaceSpec, SurfaceTarget};
use sidebar_platform::{Platform, PlatformEvent, Result, SurfaceId};

const TICK: Duration = Duration::from_millis(250);

/// Called after a drop changed widget placements.
pub type LayoutHook = Box<dyn FnMut(&Shell)>;

/// Requests from widgets and panels, sent over a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Restart the app, re-reading the config.
    Reload,
    OpenSettings,
    CloseSettings,
}

/// Why [`Runtime::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Quit,
    Reload,
}

/// A window the app draws itself, outside the shell (settings).
pub trait Panel {
    fn title(&self) -> String;
    fn size(&self) -> Size;
    fn render(&self) -> Scene;
    /// Returns `true` if the panel needs a redraw.
    fn input(&mut self, event: &InputEvent) -> bool;
}

/// Builds the settings panel each time it opens.
pub type PanelFactory = Box<dyn FnMut() -> Box<dyn Panel>>;
/// Called when the platform cannot open the panel's window, with why.
pub type PanelFallback = Box<dyn FnMut(&str)>;

pub struct Runtime {
    platform: Box<dyn Platform>,
    shell: Shell,
    on_layout_changed: Option<LayoutHook>,
    surfaces: HashMap<SurfaceTarget, (SurfaceId, SurfaceSpec)>,
    targets: HashMap<SurfaceId, SurfaceTarget>,
    /// Redraws requested by platform events rather than the shell.
    redraw: BTreeSet<SurfaceTarget>,
    actions: Option<Receiver<Action>>,
    make_panel: Option<(PanelFactory, PanelFallback)>,
    panel: Option<(SurfaceId, Box<dyn Panel>)>,
}

impl Runtime {
    pub fn new(platform: Box<dyn Platform>, shell: Shell) -> Self {
        Self {
            platform,
            shell,
            on_layout_changed: None,
            surfaces: HashMap::new(),
            targets: HashMap::new(),
            redraw: BTreeSet::new(),
            actions: None,
            make_panel: None,
            panel: None,
        }
    }

    /// Actions to carry out, e.g. from the clock's buttons.
    pub fn with_actions(mut self, actions: Receiver<Action>) -> Self {
        self.actions = Some(actions);
        self
    }

    /// How [`Action::OpenSettings`] builds its panel, and what to do
    /// instead when no window can be opened.
    pub fn with_settings(mut self, make: PanelFactory, fallback: PanelFallback) -> Self {
        self.make_panel = Some((make, fallback));
        self
    }

    pub fn on_layout_changed(mut self, hook: LayoutHook) -> Self {
        self.on_layout_changed = Some(hook);
        self
    }

    /// Runs until the platform asks to quit or a reload is requested, or
    /// for `max_ticks` iterations.
    pub fn run(&mut self, max_ticks: Option<u64>) -> Result<Exit> {
        let mut ticks = 0;
        loop {
            self.shell.tick(SystemTime::now());
            let mut redraw = self.shell.take_dirty();
            redraw.append(&mut self.redraw);
            redraw.extend(self.reconcile()?);
            for target in redraw {
                self.present(target)?;
            }
            if self.shell.take_layout_changed()
                && let Some(hook) = &mut self.on_layout_changed
            {
                hook(&self.shell);
            }

            if max_ticks.is_some_and(|max| ticks >= max) {
                return Ok(Exit::Quit);
            }
            ticks += 1;

            for event in self.platform.poll_events(TICK) {
                if !self.handle_event(event)? {
                    return Ok(Exit::Quit);
                }
            }
            let actions: Vec<Action> = self.actions.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
            for action in actions {
                match action {
                    Action::Reload => return Ok(Exit::Reload),
                    Action::OpenSettings => self.open_panel()?,
                    Action::CloseSettings => self.close_panel(),
                }
            }
        }
    }

    fn open_panel(&mut self) -> Result<()> {
        if self.panel.is_some() {
            return Ok(());
        }
        let Some((make, fallback)) = &mut self.make_panel else { return Ok(()) };
        let panel = make();
        let monitors = self.shell.monitors();
        let Some(monitor) = monitors.iter().find(|m| m.primary).or(monitors.first()) else { return Ok(()) };
        let spec = SurfaceSpec {
            target: SurfaceTarget::Panel,
            monitor: monitor.id,
            kind: SurfaceKind::Window { title: panel.title() },
            size: panel.size(),
            above: false,
        };
        match self.platform.create_surface(&spec) {
            Ok(id) => {
                self.platform.present(id, &panel.render())?;
                self.panel = Some((id, panel));
            }
            Err(e) => fallback(&e.to_string()),
        }
        Ok(())
    }

    fn close_panel(&mut self) {
        if let Some((id, _)) = self.panel.take() {
            self.platform.destroy_surface(id);
        }
    }

    /// Returns `false` when the app should exit.
    fn handle_event(&mut self, event: PlatformEvent) -> Result<bool> {
        match event {
            PlatformEvent::Input { surface, event, .. } if self.panel.as_ref().is_some_and(|(id, _)| *id == surface) => {
                if let Some((id, panel)) = &mut self.panel
                    && panel.input(&event)
                {
                    self.platform.present(*id, &panel.render())?;
                }
            }
            PlatformEvent::SurfaceResized { surface, .. } if self.panel.as_ref().is_some_and(|(id, _)| *id == surface) => {
                if let Some((id, panel)) = &self.panel {
                    self.platform.present(*id, &panel.render())?;
                }
            }
            PlatformEvent::CloseRequested { surface } if self.panel.as_ref().is_some_and(|(id, _)| *id == surface) => {
                self.close_panel();
            }
            PlatformEvent::MonitorsChanged => self.shell.set_monitors(self.platform.monitors()),
            PlatformEvent::SurfaceResized { surface, .. } => {
                // Layout still follows the shell's model; just redraw so the
                // backend can fit the frame to the new size.
                if let Some(&target) = self.targets.get(&surface) {
                    self.redraw.insert(target);
                }
            }
            PlatformEvent::Input { surface, event, global } => {
                if let Some(&target) = self.targets.get(&surface) {
                    self.shell.handle_input(target, &event, global);
                }
            }
            PlatformEvent::CloseRequested { .. } | PlatformEvent::Quit => return Ok(false),
        }
        Ok(true)
    }

    /// Creates, reconfigures, and destroys platform surfaces to match the
    /// shell. Returns the surfaces that were created or changed.
    fn reconcile(&mut self) -> Result<BTreeSet<SurfaceTarget>> {
        let desired = self.shell.surfaces();
        let wanted: BTreeSet<SurfaceTarget> = desired.iter().map(|s| s.target).collect();
        let mut changed = BTreeSet::new();

        let stale: Vec<SurfaceTarget> = self.surfaces.keys().filter(|t| !wanted.contains(t)).copied().collect();
        for target in stale {
            if let Some((id, _)) = self.surfaces.remove(&target) {
                self.targets.remove(&id);
                self.platform.destroy_surface(id);
            }
        }

        for spec in desired {
            let target = spec.target;
            match self.surfaces.get_mut(&target) {
                Some((_, current)) if *current == spec => {}
                Some((id, current)) => {
                    self.platform.configure_surface(*id, &spec)?;
                    *current = spec;
                    changed.insert(target);
                }
                None => {
                    let id = self.platform.create_surface(&spec)?;
                    self.targets.insert(id, target);
                    self.surfaces.insert(target, (id, spec));
                    changed.insert(target);
                }
            }
        }
        Ok(changed)
    }

    fn present(&mut self, target: SurfaceTarget) -> Result<()> {
        let Some((id, _)) = self.surfaces.get(&target) else { return Ok(()) };
        match self.shell.render(target) {
            Some(scene) => self.platform.present(*id, &scene),
            None => Ok(()),
        }
    }
}
