//! The settings window: where the sidebar sits and how it shares the
//! screen (the General page), which widgets are shown, in what order, and
//! their options. Saving rewrites those parts of the config file and
//! reloads the app.

use std::cell::RefCell;
use std::sync::mpsc::Sender;

use sidebar_core::text::line_height;
use sidebar_core::{Color, Edge, InputEvent, Key, Point, PointerButton, Rect, Scene, Size, TextMeasure, Theme};

use crate::config_file::{General, TemperatureUnit, WeekStart, WidgetSpec};
use crate::runtime::{Action, Panel};

const WIDTH: f32 = 520.0;
const HEIGHT: f32 = 450.0;
const PAD: f32 = 16.0;
const LIST_W: f32 = 180.0;
const ROW_H: f32 = 26.0;
const TEXT: f32 = 12.0;
const SMALL: f32 = 10.0;
const TITLE: f32 = 15.0;
const FIELD_H: f32 = 28.0;
const BUTTON_H: f32 = 28.0;

/// Saves the General settings and the widget list (the config file in the
/// app; a stub in tests).
pub type SaveFn = Box<dyn FnMut(&General, &[WidgetSpec]) -> Result<(), String>>;

/// General settings this backend cannot apply, each with the reason shown
/// in place of its hint. Their rows are grayed out and can't be changed;
/// the configured value is kept.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Unavailable {
    pub reserve_space: Option<&'static str>,
    pub opacity: Option<&'static str>,
}

const EDGES: [Edge; 4] = [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom];

/// The General page's choice rows: label, hint, options, and the chosen
/// index.
const OPACITIES: [u8; 5] = [60, 70, 80, 90, 100];

fn general_rows(g: &General) -> [(&'static str, &'static str, &'static [&'static str], usize); 4] {
    // The nearest step, for opacities set by hand in the config file.
    let opacity = (0..OPACITIES.len()).min_by_key(|&i| OPACITIES[i].abs_diff(g.opacity)).unwrap_or(3);
    [
        (
            "Reserve screen space",
            "Maximized windows stop at the sidebar instead of covering it.",
            &["On", "Off"],
            (!g.reserve_space).into(),
        ),
        ("Always on top", "Off: other windows can be moved over the sidebar.", &["Off", "On"], g.always_on_top.into()),
        (
            "Default edge",
            "For widgets that haven't been dragged somewhere else.",
            &["Left", "Right", "Top", "Bottom"],
            EDGES.iter().position(|e| *e == g.edge).unwrap_or(1),
        ),
        ("Opacity", "How solid the sidebar's background is; text stays solid.", &["60%", "70%", "80%", "90%", "100%"], opacity),
    ]
}

/// One widget in the list. `text` is the widget's text option while being
/// edited (location, symbols, interface, label).
struct Entry {
    enabled: bool,
    spec: WidgetSpec,
    text: String,
}

/// Every kind once, with default options, in list order.
fn catalog() -> Vec<WidgetSpec> {
    vec![
        WidgetSpec::Clock,
        WidgetSpec::Calendar { week_start: WeekStart::Sunday },
        WidgetSpec::Cpu,
        WidgetSpec::Gpu { index: 0 },
        WidgetSpec::Network { interface: None },
        WidgetSpec::Weather { location: String::new(), units: TemperatureUnit::Celsius },
        WidgetSpec::Stocks { symbols: Vec::new() },
        WidgetSpec::Battery,
        WidgetSpec::Label { text: String::new() },
    ]
}

fn display_name(spec: &WidgetSpec) -> &'static str {
    match spec {
        WidgetSpec::Label { .. } => "Label",
        WidgetSpec::Clock => "Clock",
        WidgetSpec::Calendar { .. } => "Calendar",
        WidgetSpec::Cpu => "CPU & memory",
        WidgetSpec::Gpu { .. } => "GPU",
        WidgetSpec::Network { .. } => "Network",
        WidgetSpec::Weather { .. } => "Weather",
        WidgetSpec::Stocks { .. } => "Stocks",
        WidgetSpec::Battery => "Battery",
    }
}

fn description(spec: &WidgetSpec) -> &'static str {
    match spec {
        WidgetSpec::Label { .. } => "A line of fixed text.",
        WidgetSpec::Clock => "Time and date, with the reload and settings buttons.",
        WidgetSpec::Calendar { .. } => "Month grid with ISO week numbers.",
        WidgetSpec::Cpu => "Per-core load, temperature, RAM and swap.",
        WidgetSpec::Gpu { .. } => "NVIDIA load, memory, power and temperature.",
        WidgetSpec::Network { .. } => "Addresses, signal and traffic.",
        WidgetSpec::Weather { .. } => "Forecast from Open-Meteo (location is sent there).",
        WidgetSpec::Stocks { .. } => "Quotes from Yahoo Finance, every 5 minutes.",
        WidgetSpec::Battery => "Charge level (skipped without a battery).",
    }
}

/// The text field of a kind: label and hint.
fn field_of(spec: &WidgetSpec) -> Option<(&'static str, &'static str)> {
    match spec {
        WidgetSpec::Weather { .. } => Some(("Location", "\"Springfield, Illinois\", \"Berlin\" or \"52.52, 13.41\"")),
        WidgetSpec::Stocks { .. } => Some(("Symbols", "Separated by commas or spaces: AAPL, ^GSPC, EURUSD=X")),
        WidgetSpec::Network { .. } => Some(("Interface", "Empty: follow the default route")),
        WidgetSpec::Label { .. } => Some(("Text", "")),
        _ => None,
    }
}

/// The choice row of a kind: label, options, and the chosen index.
fn choice_of(spec: &WidgetSpec) -> Option<(&'static str, &'static [&'static str], usize)> {
    match spec {
        WidgetSpec::Weather { units, .. } => Some(("Units", &["°C", "°F"], (*units == TemperatureUnit::Fahrenheit).into())),
        WidgetSpec::Calendar { week_start } => Some(("Week starts", &["Sunday", "Monday"], (*week_start == WeekStart::Monday).into())),
        WidgetSpec::Gpu { index } => Some(("GPU", &["1st", "2nd", "3rd", "4th"], (*index).min(3))),
        _ => None,
    }
}

fn text_of(spec: &WidgetSpec) -> String {
    match spec {
        WidgetSpec::Weather { location, .. } => location.clone(),
        WidgetSpec::Stocks { symbols } => symbols.join(", "),
        WidgetSpec::Network { interface } => interface.clone().unwrap_or_default(),
        WidgetSpec::Label { text } => text.clone(),
        _ => String::new(),
    }
}

/// The spec with the edited text applied, or what is wrong with it.
fn finished(entry: &Entry) -> Result<WidgetSpec, String> {
    let text = entry.text.trim();
    let name = display_name(&entry.spec);
    Ok(match &entry.spec {
        WidgetSpec::Weather { .. } if text.is_empty() => return Err(format!("{name}: enter a location")),
        WidgetSpec::Weather { units, .. } => WidgetSpec::Weather { location: text.into(), units: *units },
        WidgetSpec::Stocks { .. } => {
            let symbols: Vec<String> =
                text.split([',', ' ', ';']).filter(|s| !s.is_empty()).map(str::to_uppercase).collect();
            if symbols.is_empty() {
                return Err(format!("{name}: add at least one symbol"));
            }
            WidgetSpec::Stocks { symbols }
        }
        WidgetSpec::Network { .. } => WidgetSpec::Network { interface: (!text.is_empty()).then(|| text.into()) },
        WidgetSpec::Label { .. } if text.is_empty() => return Err(format!("{name}: enter the text")),
        WidgetSpec::Label { .. } => WidgetSpec::Label { text: entry.text.clone() },
        other => other.clone(),
    })
}

/// Clickable things, recorded while drawing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    /// The General page, above the widgets.
    General,
    Row(usize),
    Check(usize),
    Up,
    Down,
    Copy,
    /// The text field; `start` is the first visible char, `x` where it is.
    Field { start: usize, x: f32 },
    /// Option `.1` of choice row `.0`.
    Choice(usize, usize),
    Cancel,
    Save,
}

pub struct Settings {
    general: General,
    unavailable: Unavailable,
    /// The General page is shown instead of the selected widget.
    on_general: bool,
    entries: Vec<Entry>,
    selected: usize,
    /// Typing goes to the text field.
    editing: bool,
    /// In chars.
    cursor: usize,
    /// An error from the last save attempt.
    status: Option<String>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    theme: Theme,
    text: Box<dyn TextMeasure>,
    hits: RefCell<Vec<(Rect, Hit)>>,
    actions: Sender<Action>,
    save: SaveFn,
}

impl Settings {
    /// `specs` are the configured widgets; kinds not among them are listed
    /// unticked after them.
    pub fn new(general: General, specs: &[WidgetSpec], theme: Theme, text: Box<dyn TextMeasure>, actions: Sender<Action>, save: SaveFn) -> Self {
        let entry = |enabled, spec: WidgetSpec| Entry { enabled, text: text_of(&spec), spec };
        let mut entries: Vec<Entry> = specs.iter().map(|s| entry(true, s.clone())).collect();
        for spec in catalog() {
            if !specs.iter().any(|s| s.kind() == spec.kind()) {
                entries.push(entry(false, spec));
            }
        }
        Self {
            general,
            unavailable: Unavailable::default(),
            on_general: false,
            entries,
            selected: 0,
            editing: false,
            cursor: 0,
            status: None,
            hover: None,
            pressed: None,
            theme,
            text,
            hits: RefCell::new(Vec::new()),
            actions,
            save,
        }
    }

    /// Grays out the General settings this backend can't apply.
    pub fn with_unavailable(mut self, unavailable: Unavailable) -> Self {
        self.unavailable = unavailable;
        self
    }

    /// Why General row `row` can't be changed, if it can't.
    fn unavailable(&self, row: usize) -> Option<&'static str> {
        match row {
            0 => self.unavailable.reserve_space,
            3 => self.unavailable.opacity,
            _ => None,
        }
    }

    fn entry(&self) -> &Entry {
        &self.entries[self.selected]
    }

    /// "Weather", or "Weather 2" for the second of a kind.
    fn name(&self, i: usize) -> String {
        let kind = self.entries[i].spec.kind();
        let n = self.entries[..i].iter().filter(|e| e.spec.kind() == kind).count();
        let name = display_name(&self.entries[i].spec);
        if n == 0 { name.into() } else { format!("{name} {}", n + 1) }
    }

    fn select(&mut self, i: usize) {
        if i < self.entries.len() && (i != self.selected || self.on_general) {
            self.selected = i;
            self.on_general = false;
            self.editing = false;
        }
    }

    fn select_general(&mut self) {
        self.on_general = true;
        self.editing = false;
    }

    fn start_editing(&mut self) {
        if !self.on_general && field_of(&self.entry().spec).is_some() {
            self.editing = true;
            self.cursor = self.entry().text.chars().count();
        }
    }

    fn move_selected(&mut self, down: bool) {
        if self.on_general {
            return;
        }
        let i = self.selected;
        let j = if down { i + 1 } else { i.wrapping_sub(1) };
        if j < self.entries.len() {
            self.entries.swap(i, j);
            self.selected = j;
        }
    }

    /// Adds a copy of the selected widget below it (e.g. a second weather
    /// place).
    fn copy_selected(&mut self) {
        if self.on_general {
            return;
        }
        let e = self.entry();
        let copy = Entry { enabled: true, spec: e.spec.clone(), text: e.text.clone() };
        self.entries.insert(self.selected + 1, copy);
        self.selected += 1;
        self.start_editing();
    }

    fn choose(&mut self, row: usize, option: usize) {
        if self.on_general {
            if self.unavailable(row).is_some() {
                return;
            }
            let g = &mut self.general;
            match row {
                0 => g.reserve_space = option == 0,
                1 => g.always_on_top = option == 1,
                2 => g.edge = EDGES.get(option).copied().unwrap_or(g.edge),
                _ => g.opacity = OPACITIES.get(option).copied().unwrap_or(g.opacity),
            }
            return;
        }
        let spec = &mut self.entries[self.selected].spec;
        match spec {
            WidgetSpec::Weather { units, .. } => {
                *units = if option == 1 { TemperatureUnit::Fahrenheit } else { TemperatureUnit::Celsius };
            }
            WidgetSpec::Calendar { week_start } => *week_start = if option == 1 { WeekStart::Monday } else { WeekStart::Sunday },
            WidgetSpec::Gpu { index } => *index = option,
            _ => {}
        }
    }

    fn save(&mut self) {
        self.editing = false;
        let mut specs = Vec::new();
        for (i, entry) in self.entries.iter().enumerate().filter(|(_, e)| e.enabled) {
            match finished(entry) {
                Ok(spec) => specs.push(spec),
                Err(e) => {
                    self.status = Some(e);
                    self.select(i);
                    self.start_editing();
                    return;
                }
            }
        }
        if specs.is_empty() {
            self.status = Some("Tick at least one widget".into());
            return;
        }
        match (self.save)(&self.general, &specs) {
            Ok(()) => {
                let _ = self.actions.send(Action::Reload);
            }
            Err(e) => self.status = Some(format!("Could not save: {e}")),
        }
    }

    fn close(&self) {
        let _ = self.actions.send(Action::CloseSettings);
    }

    fn click(&mut self, hit: Hit, pos: Point) {
        self.status = None;
        if !matches!(hit, Hit::Field { .. }) {
            self.editing = false;
        }
        match hit {
            Hit::General => self.select_general(),
            Hit::Row(i) => self.select(i),
            Hit::Check(i) => {
                self.select(i);
                self.entries[i].enabled = !self.entries[i].enabled;
            }
            Hit::Up => self.move_selected(false),
            Hit::Down => self.move_selected(true),
            Hit::Copy => self.copy_selected(),
            Hit::Field { start, x } => {
                self.editing = true;
                // The char boundary nearest the click.
                let chars: Vec<char> = self.entry().text.chars().collect();
                let mut cursor = start;
                while cursor < chars.len() {
                    let before: String = chars[start..cursor].iter().collect();
                    let next: String = chars[start..=cursor].iter().collect();
                    let mid = (self.width(&before, TEXT) + self.width(&next, TEXT)) / 2.0;
                    if x + mid > pos.x {
                        break;
                    }
                    cursor += 1;
                }
                self.cursor = cursor;
            }
            Hit::Choice(row, i) => self.choose(row, i),
            Hit::Cancel => self.close(),
            Hit::Save => self.save(),
        }
    }

    fn key(&mut self, key: Key) {
        if self.editing {
            self.edit(key);
            return;
        }
        match key {
            Key::Up if self.on_general => {}
            Key::Up if self.selected == 0 => self.select_general(),
            Key::Up => self.select(self.selected - 1),
            Key::Down if self.on_general => self.select(0),
            Key::Down => self.select(self.selected + 1),
            Key::Char(' ') if !self.on_general => self.entries[self.selected].enabled ^= true,
            Key::Tab | Key::Right => self.start_editing(),
            Key::Enter => self.save(),
            Key::Escape => self.close(),
            // Typing goes straight into the field.
            Key::Char(_) if !self.on_general && field_of(&self.entry().spec).is_some() => {
                self.start_editing();
                self.edit(key);
            }
            _ => {}
        }
    }

    fn edit(&mut self, key: Key) {
        let text = &mut self.entries[self.selected].text;
        let len = text.chars().count();
        let byte = |text: &str, i: usize| text.char_indices().nth(i).map_or(text.len(), |(b, _)| b);
        match key {
            Key::Char(c) => {
                text.insert(byte(text, self.cursor), c);
                self.cursor += 1;
            }
            Key::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                text.remove(byte(text, self.cursor));
            }
            Key::Delete if self.cursor < len => {
                text.remove(byte(text, self.cursor));
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(len),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = len,
            Key::Tab | Key::BackTab | Key::Escape | Key::Up | Key::Down => self.editing = false,
            Key::Enter => self.save(),
            _ => {}
        }
        if matches!(key, Key::Up | Key::Down) {
            self.key(key);
        }
    }

    fn width(&self, text: &str, size: f32) -> f32 {
        self.text.measure(text, size).width
    }

    // Drawing ------------------------------------------------------------

    fn label(&self, scene: &mut Scene, at: Point, text: &str, size: f32, color: Color) {
        scene.text(Point::new(at.x.round(), at.y.round()), text, size, color);
    }

    /// Text centered in `rect`.
    fn centered(&self, scene: &mut Scene, rect: Rect, text: &str, size: f32, color: Color) {
        let w = self.width(text, size);
        let at = Point::new(rect.x + (rect.width - w) / 2.0, rect.y + (rect.height - line_height(size)) / 2.0);
        self.label(scene, at, text, size, color);
    }

    fn button(&self, scene: &mut Scene, rect: Rect, text: &str, hit: Hit, primary: bool) {
        let fg = self.theme.foreground;
        let accent = self.theme.accent.with_alpha(255);
        let hovered = self.hover == Some(hit);
        let fill = match (primary, hovered) {
            (true, false) => accent.with_alpha(200),
            (true, true) => accent,
            (false, false) => fg.with_alpha(22),
            (false, true) => fg.with_alpha(45),
        };
        scene.fill_rect(rect, fill, 5.0);
        self.centered(scene, rect, text, TEXT, fg);
        self.hits.borrow_mut().push((rect, hit));
    }

    /// The General row, at the top of the list.
    fn draw_general_row(&self, scene: &mut Scene, row: Rect) {
        let fg = self.theme.foreground;
        if self.on_general {
            scene.fill_rect(row, self.theme.accent.with_alpha(60), 5.0);
        } else if self.hover == Some(Hit::General) {
            scene.fill_rect(row, fg.with_alpha(14), 5.0);
        }
        let icon = Rect::new(row.x + 8.0, row.y + (row.height - 14.0) / 2.0, 14.0, 14.0);
        self.centered(scene, icon, "⚙", 12.0, fg.with_alpha(200));
        self.label(scene, Point::new(row.x + 30.0, row.y + (row.height - line_height(TEXT)) / 2.0), "General", TEXT, fg);
        self.hits.borrow_mut().push((row, Hit::General));
    }

    fn draw_list(&self, scene: &mut Scene, area: Rect) {
        let fg = self.theme.foreground;
        let accent = self.theme.accent.with_alpha(255);
        for i in 0..self.entries.len() {
            let row = Rect::new(area.x, area.y + i as f32 * ROW_H, area.width, ROW_H - 2.0);
            if i == self.selected && !self.on_general {
                scene.fill_rect(row, accent.with_alpha(60), 5.0);
            } else if self.hover == Some(Hit::Row(i)) {
                scene.fill_rect(row, fg.with_alpha(14), 5.0);
            }
            self.hits.borrow_mut().push((row, Hit::Row(i)));
            // The tick box, with a larger click target than it looks.
            let target = Rect::new(row.x, row.y, 28.0, row.height);
            let check = Rect::new(row.x + 8.0, row.y + (row.height - 14.0) / 2.0, 14.0, 14.0);
            let on = self.entries[i].enabled;
            if on {
                scene.fill_rect(check, accent, 3.0);
                self.centered(scene, check, "✓", 11.0, Color::rgba(255, 255, 255, 255));
            } else {
                scene.fill_rect(check, fg.with_alpha(if self.hover == Some(Hit::Check(i)) { 90 } else { 60 }), 3.0);
                scene.fill_rect(check.inset(1.5), self.theme.background.with_alpha(255), 2.0);
            }
            self.hits.borrow_mut().push((target, Hit::Check(i)));
            let color = if on { fg } else { fg.with_alpha(130) };
            self.label(scene, Point::new(row.x + 30.0, row.y + (row.height - line_height(TEXT)) / 2.0), &self.name(i), TEXT, color);
        }
        // Reorder and copy, under the list.
        let y = area.y + self.entries.len() as f32 * ROW_H + 6.0;
        let small = 34.0;
        self.button(scene, Rect::new(area.x, y, small, 24.0), "▲", Hit::Up, false);
        self.button(scene, Rect::new(area.x + small + 4.0, y, small, 24.0), "▼", Hit::Down, false);
        self.button(scene, Rect::new(area.x + 2.0 * (small + 4.0), y, area.width - 2.0 * (small + 4.0), 24.0), "Add another", Hit::Copy, false);
    }

    /// A label and a segmented choice; returns the y below it. With `row`
    /// `None` it is grayed out and not clickable.
    fn draw_choice(&self, scene: &mut Scene, at: Point, label: &str, options: &[&str], chosen: usize, row: Option<usize>) -> f32 {
        let fg = self.theme.foreground;
        let (x, mut y) = (at.x, at.y);
        self.label(scene, Point::new(x, y), label, SMALL, fg.with_alpha(if row.is_some() { 170 } else { 90 }));
        y += line_height(SMALL) + 4.0;
        // Within the options column.
        let column = WIDTH - 2.0 * PAD - (LIST_W + PAD + 4.0);
        let w = (options.iter().map(|o| self.width(o, TEXT)).fold(0.0, f32::max) + 28.0).max(56.0).min(column / options.len() as f32);
        let track = Rect::new(x, y, w * options.len() as f32, FIELD_H);
        scene.fill_rect(track, fg.with_alpha(if row.is_some() { 18 } else { 10 }), 6.0);
        for (i, option) in options.iter().enumerate() {
            let rect = Rect::new(track.x + i as f32 * w, y, w, FIELD_H).inset(2.0);
            let Some(row) = row else {
                // Disabled: the configured value stays visible, without the accent.
                if i == chosen {
                    scene.fill_rect(rect, fg.with_alpha(30), 5.0);
                }
                self.centered(scene, rect, option, TEXT, fg.with_alpha(if i == chosen { 110 } else { 70 }));
                continue;
            };
            if i == chosen {
                scene.fill_rect(rect, self.theme.accent.with_alpha(220), 5.0);
            } else if self.hover == Some(Hit::Choice(row, i)) {
                scene.fill_rect(rect, fg.with_alpha(25), 5.0);
            }
            self.centered(scene, rect, option, TEXT, if i == chosen { fg } else { fg.with_alpha(170) });
            self.hits.borrow_mut().push((rect, Hit::Choice(row, i)));
        }
        y + FIELD_H
    }

    fn draw_general(&self, scene: &mut Scene, area: Rect) {
        let fg = self.theme.foreground;
        let mut y = area.y;
        self.label(scene, Point::new(area.x, y), "General", TITLE, fg);
        y += line_height(TITLE) + 2.0;
        self.label(scene, Point::new(area.x, y), "Where the sidebar sits and how it shares the screen.", SMALL, fg.with_alpha(150));
        y += line_height(SMALL) + 16.0;
        for (row, (label, hint, options, chosen)) in general_rows(&self.general).into_iter().enumerate() {
            let why = self.unavailable(row);
            let clickable = if why.is_none() { Some(row) } else { None };
            y = self.draw_choice(scene, Point::new(area.x, y), label, options, chosen, clickable) + 4.0;
            self.label(scene, Point::new(area.x, y), why.unwrap_or(hint), SMALL, fg.with_alpha(120));
            y += line_height(SMALL) + 14.0;
        }
    }

    fn draw_options(&self, scene: &mut Scene, area: Rect) {
        if self.on_general {
            self.draw_general(scene, area);
            return;
        }
        let fg = self.theme.foreground;
        let entry = self.entry();
        let mut y = area.y;
        self.label(scene, Point::new(area.x, y), &self.name(self.selected), TITLE, fg);
        y += line_height(TITLE) + 2.0;
        self.label(scene, Point::new(area.x, y), description(&entry.spec), SMALL, fg.with_alpha(150));
        y += line_height(SMALL) + 4.0;
        if !entry.enabled {
            self.label(scene, Point::new(area.x, y), "Not shown. Tick it in the list to add it.", SMALL, fg.with_alpha(150));
        }
        y += line_height(SMALL) + 12.0;

        if let Some((label, hint)) = field_of(&entry.spec) {
            self.label(scene, Point::new(area.x, y), label, SMALL, fg.with_alpha(170));
            y += line_height(SMALL) + 4.0;
            let field = Rect::new(area.x, y, area.width, FIELD_H);
            self.draw_field(scene, field, &entry.text);
            y += FIELD_H + 4.0;
            if !hint.is_empty() {
                self.label(scene, Point::new(area.x, y), hint, SMALL, fg.with_alpha(120));
            }
            y += line_height(SMALL) + 14.0;
        }

        if let Some((label, options, chosen)) = choice_of(&entry.spec) {
            self.draw_choice(scene, Point::new(area.x, y), label, options, chosen, Some(0));
        }
        if field_of(&entry.spec).is_none() && choice_of(&entry.spec).is_none() {
            self.label(scene, Point::new(area.x, y), "No options.", TEXT, fg.with_alpha(130));
        }
    }

    /// A one-line text field, scrolled so the cursor stays visible.
    fn draw_field(&self, scene: &mut Scene, field: Rect, text: &str) {
        let fg = self.theme.foreground;
        let accent = self.theme.accent.with_alpha(255);
        if self.editing {
            scene.fill_rect(field, accent, 6.0);
        } else {
            scene.fill_rect(field, fg.with_alpha(if matches!(self.hover, Some(Hit::Field { .. })) { 70 } else { 45 }), 6.0);
        }
        scene.fill_rect(field.inset(1.5), self.theme.background.with_alpha(255), 5.0);
        let inner = field.inset(8.0);
        let chars: Vec<char> = text.chars().collect();
        let span = |a: usize, b: usize| -> String { chars[a..b].iter().collect() };
        let cursor = if self.editing { self.cursor.min(chars.len()) } else { 0 };
        let mut start = 0;
        while start < cursor && self.width(&span(start, cursor), TEXT) > inner.width - 2.0 {
            start += 1;
        }
        let mut end = start;
        while end < chars.len() && self.width(&span(start, end + 1), TEXT) <= inner.width {
            end += 1;
        }
        let y = field.y + (field.height - line_height(TEXT)) / 2.0;
        self.label(scene, Point::new(inner.x, y), &span(start, end), TEXT, fg);
        if self.editing {
            let x = (inner.x + self.width(&span(start, cursor), TEXT)).round();
            scene.fill_rect(Rect::new(x, y + 1.0, 1.5, line_height(TEXT) - 2.0), fg, 0.0);
        }
        self.hits.borrow_mut().push((field, Hit::Field { start, x: inner.x }));
    }

    fn draw_footer(&self, scene: &mut Scene, area: Rect) {
        let y = area.bottom() - BUTTON_H;
        scene.fill_rect(Rect::new(area.x, y - 12.0, area.width, 1.0), self.theme.foreground.with_alpha(25), 0.0);
        let save_w = 120.0;
        let cancel_w = 84.0;
        let save = Rect::new(area.right() - save_w, y, save_w, BUTTON_H);
        self.button(scene, save, "Save & reload", Hit::Save, true);
        self.button(scene, Rect::new(save.x - 8.0 - cancel_w, y, cancel_w, BUTTON_H), "Cancel", Hit::Cancel, false);
        let (message, color) = match &self.status {
            Some(e) => (e.as_str(), Color::rgba(240, 110, 100, 255)),
            None => ("Changes apply after saving.", self.theme.foreground.with_alpha(110)),
        };
        let y = y + (BUTTON_H - line_height(SMALL)) / 2.0;
        self.label(scene, Point::new(area.x, y), message, SMALL, color);
    }

    fn hit(&self, pos: Point) -> Option<Hit> {
        // Last drawn wins: buttons over the rows they sit on.
        self.hits.borrow().iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, h)| *h)
    }
}

impl Panel for Settings {
    fn title(&self) -> String {
        "Sidebar Settings".into()
    }

    fn size(&self) -> Size {
        let list = PAD + ROW_H + 10.0 + line_height(TEXT) + 10.0 + self.entries.len().max(9) as f32 * ROW_H + 30.0;
        Size::new(WIDTH, HEIGHT.max(list + 12.0 + BUTTON_H + 12.0 + PAD))
    }

    fn render(&self) -> Scene {
        let size = self.size();
        let mut scene = Scene::new(size);
        scene.clear(self.theme.background.with_alpha(255));
        // Tinted like the sidebar's cards.
        scene.fill_rect(Rect::new(0.0, 0.0, size.width, size.height), self.theme.accent.with_alpha(18), 0.0);
        self.hits.borrow_mut().clear();
        let area = Rect::new(0.0, 0.0, size.width, size.height).inset(PAD);
        let fg = self.theme.foreground;
        self.draw_general_row(&mut scene, Rect::new(area.x, area.y, LIST_W, ROW_H - 2.0));
        let heading = area.y + ROW_H + 10.0;
        self.label(&mut scene, Point::new(area.x, heading), "WIDGETS", SMALL, fg.with_alpha(140));
        let top = heading + line_height(TEXT) + 6.0;
        self.draw_list(&mut scene, Rect::new(area.x, top, LIST_W, area.height));
        let right = area.x + LIST_W + PAD + 4.0;
        scene.fill_rect(Rect::new(right - PAD / 2.0 - 2.0, area.y, 1.0, area.height - BUTTON_H - 24.0), fg.with_alpha(25), 0.0);
        self.draw_options(&mut scene, Rect::new(right, area.y, area.right() - right, area.height));
        self.draw_footer(&mut scene, area);
        scene
    }

    fn input(&mut self, event: &InputEvent) -> bool {
        match *event {
            InputEvent::PointerMoved { pos } => {
                let hover = self.hit(pos);
                let changed = hover != self.hover;
                self.hover = hover;
                changed
            }
            InputEvent::PointerLeft => {
                self.pressed = None;
                self.hover.take().is_some()
            }
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true } => {
                self.pressed = self.hit(pos);
                if self.pressed.is_none() && self.editing {
                    self.editing = false;
                    return true;
                }
                false
            }
            InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false } => {
                let Some(hit) = self.pressed.take() else { return false };
                // Same target (a field's scroll may differ; position decides).
                let same = match (hit, self.hit(pos)) {
                    (Hit::Field { .. }, Some(Hit::Field { .. })) => true,
                    (a, b) => Some(a) == b,
                };
                if same {
                    self.click(hit, pos);
                }
                same
            }
            InputEvent::Key(key) => {
                self.key(key);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;

    use sidebar_core::{ApproxText, DrawCmd};

    use super::*;

    type Saved = Rc<RefCell<Option<Vec<WidgetSpec>>>>;

    fn settings(specs: &[WidgetSpec]) -> (Settings, mpsc::Receiver<Action>, Saved) {
        let (s, rx, saved, _) = settings_with_general(specs);
        (s, rx, saved)
    }

    fn settings_with_general(specs: &[WidgetSpec]) -> (Settings, mpsc::Receiver<Action>, Saved, Rc<RefCell<Option<General>>>) {
        let (tx, rx) = mpsc::channel();
        let saved: Saved = Rc::default();
        let general: Rc<RefCell<Option<General>>> = Rc::default();
        let (sink, general_sink) = (saved.clone(), general.clone());
        let save: SaveFn = Box::new(move |general, specs| {
            *sink.borrow_mut() = Some(specs.to_vec());
            *general_sink.borrow_mut() = Some(*general);
            Ok(())
        });
        let s = Settings::new(General::default(), specs, Theme::default(), Box::new(ApproxText), tx, save);
        s.render();
        (s, rx, saved, general)
    }

    fn texts(s: &Settings) -> Vec<String> {
        s.render()
            .cmds
            .iter()
            .filter_map(|c| match c {
                DrawCmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Clicks the first target matching `want`.
    fn click(s: &mut Settings, want: impl Fn(&Hit) -> bool) {
        s.render();
        let rect = s.hits.borrow().iter().find(|(_, h)| want(h)).map(|(r, _)| *r).expect("no such target");
        let pos = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        s.input(&InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: true });
        s.input(&InputEvent::PointerButton { pos, button: PointerButton::Primary, pressed: false });
    }

    fn type_text(s: &mut Settings, text: &str) {
        for c in text.chars() {
            s.input(&InputEvent::Key(Key::Char(c)));
        }
    }

    #[test]
    fn lists_configured_then_the_rest() {
        let (s, _, _) = settings(&[WidgetSpec::Cpu, WidgetSpec::Clock]);
        let names: Vec<String> = (0..s.entries.len()).map(|i| s.name(i)).collect();
        assert_eq!(names, ["CPU & memory", "Clock", "Calendar", "GPU", "Network", "Weather", "Stocks", "Battery", "Label"]);
        assert_eq!(s.entries.iter().filter(|e| e.enabled).count(), 2);
        assert!(texts(&s).contains(&"Per-core load, temperature, RAM and swap.".to_string()));
    }

    #[test]
    fn edits_and_saves() {
        let weather = WidgetSpec::Weather { location: "Springfield".into(), units: TemperatureUnit::Celsius };
        let (mut s, rx, saved) = settings(&[WidgetSpec::Clock, weather]);
        // Weather: click the field (cursor lands near the end), fix the
        // location, switch to °F.
        click(&mut s, |h| *h == Hit::Row(1));
        s.input(&InputEvent::Key(Key::End));
        type_text(&mut s, ", Illinoisx");
        s.input(&InputEvent::Key(Key::Backspace));
        click(&mut s, |h| *h == Hit::Choice(0, 1));
        assert!(!s.editing, "clicking elsewhere ends editing");
        // Add stocks and move them to the top.
        let stocks = s.entries.iter().position(|e| e.spec.kind() == "stocks").unwrap();
        click(&mut s, |h| *h == Hit::Check(stocks));
        type_text(&mut s, "aapl msft");
        s.input(&InputEvent::Key(Key::Escape));
        for _ in 0..stocks {
            click(&mut s, |h| *h == Hit::Up);
        }
        // Untick the clock (now second).
        click(&mut s, |h| *h == Hit::Check(1));
        click(&mut s, |h| *h == Hit::Save);
        assert_eq!(
            saved.borrow().as_deref(),
            Some(
                &[
                    WidgetSpec::Stocks { symbols: vec!["AAPL".into(), "MSFT".into()] },
                    WidgetSpec::Weather { location: "Springfield, Illinois".into(), units: TemperatureUnit::Fahrenheit },
                ][..]
            )
        );
        assert_eq!(rx.try_recv(), Ok(Action::Reload));
    }

    #[test]
    fn invalid_options_are_reported() {
        let (mut s, rx, saved) = settings(&[WidgetSpec::Clock]);
        let weather = s.entries.iter().position(|e| e.spec.kind() == "weather").unwrap();
        click(&mut s, |h| *h == Hit::Check(weather));
        s.input(&InputEvent::Key(Key::Enter));
        assert_eq!(s.status.as_deref(), Some("Weather: enter a location"));
        assert!(s.editing && s.selected == weather);
        assert!(saved.borrow().is_none() && rx.try_recv().is_err());
        assert!(texts(&s).contains(&"Weather: enter a location".to_string()));

        // Escape twice: leave the field, then close.
        s.input(&InputEvent::Key(Key::Escape));
        s.input(&InputEvent::Key(Key::Escape));
        assert_eq!(rx.try_recv(), Ok(Action::CloseSettings));
    }

    #[test]
    fn changes_general_settings() {
        let (mut s, rx, _, general) = settings_with_general(&[WidgetSpec::Clock]);
        // Up from the first widget reaches the General page.
        s.input(&InputEvent::Key(Key::Up));
        assert!(s.on_general);
        assert!(texts(&s).contains(&"Reserve screen space".to_string()));
        click(&mut s, |h| *h == Hit::Choice(0, 1));
        click(&mut s, |h| *h == Hit::Choice(1, 1));
        click(&mut s, |h| *h == Hit::Choice(2, 0));
        click(&mut s, |h| *h == Hit::Choice(3, 4));
        // Widget-only actions do nothing here.
        s.input(&InputEvent::Key(Key::Char(' ')));
        click(&mut s, |h| *h == Hit::Copy);
        assert_eq!(s.entries.iter().filter(|e| e.enabled).count(), 1);
        s.input(&InputEvent::Key(Key::Down));
        assert!(!s.on_general && s.selected == 0);
        click(&mut s, |h| *h == Hit::General);
        click(&mut s, |h| *h == Hit::Save);
        assert_eq!(*general.borrow(), Some(General { edge: Edge::Left, reserve_space: false, always_on_top: true, opacity: 100 }));
        assert_eq!(rx.try_recv(), Ok(Action::Reload));
    }

    #[test]
    fn grays_out_what_the_backend_cannot_do() {
        let (s, rx, _, general) = settings_with_general(&[WidgetSpec::Clock]);
        let why = "Not available: no compositor is running.";
        let mut s = s.with_unavailable(Unavailable { reserve_space: None, opacity: Some(why) });
        s.input(&InputEvent::Key(Key::Up));
        let texts = texts(&s);
        assert!(texts.contains(&why.to_string()));
        assert!(!texts.iter().any(|t| t.starts_with("How solid")), "the reason replaces the hint");
        // The opacity options can't be clicked; the others still can.
        s.render();
        assert!(!s.hits.borrow().iter().any(|(_, h)| matches!(h, Hit::Choice(3, _))));
        s.choose(3, 0);
        click(&mut s, |h| *h == Hit::Choice(0, 1));
        click(&mut s, |h| *h == Hit::Save);
        let saved = general.borrow().expect("saved");
        assert_eq!((saved.reserve_space, saved.opacity), (false, General::default().opacity));
        assert_eq!(rx.try_recv(), Ok(Action::Reload));
    }

    #[test]
    fn copies_and_edits_in_the_middle() {
        let weather = WidgetSpec::Weather { location: "Oslo".into(), units: TemperatureUnit::Celsius };
        let (mut s, _, saved) = settings(&[weather]);
        click(&mut s, |h| *h == Hit::Copy);
        assert_eq!(s.name(1), "Weather 2");
        assert!(s.editing);
        s.input(&InputEvent::Key(Key::Home));
        s.input(&InputEvent::Key(Key::Delete));
        s.input(&InputEvent::Key(Key::Right));
        s.input(&InputEvent::Key(Key::Right));
        type_text(&mut s, "€");
        s.input(&InputEvent::Key(Key::Enter));
        let saved = saved.borrow().clone().unwrap();
        assert_eq!(saved[1], WidgetSpec::Weather { location: "sl€o".into(), units: TemperatureUnit::Celsius });
    }
}
