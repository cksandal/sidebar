//! The user's config file (TOML). Every field is optional; missing ones
//! keep the defaults from `sidebar_core::Config`. See `config.example.toml`
//! at the repository root for the full format.

use std::path::Path;

use serde::Deserialize;
use sidebar_core::{Color, Config, Edge};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    /// Backend name, as for `--backend`.
    pub backend: Option<String>,
    /// Keep the sidebar above other windows (default: false).
    pub always_on_top: Option<bool>,
    pub dock: DockSection,
    pub drag: DragSection,
    pub theme: ThemeSection,
    /// Widgets in order. `None` (no `[[widgets]]` at all) means the
    /// default set.
    pub widgets: Option<Vec<WidgetSpec>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockSection {
    /// Edge for widgets without a saved placement.
    pub edge: Option<String>,
    pub thickness: Option<f32>,
    pub padding: Option<f32>,
    pub spacing: Option<f32>,
    pub reserve_space: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DragSection {
    pub snap_threshold: Option<f32>,
    pub allow_floating: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeSection {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub accent: Option<String>,
    /// Opacity of the background in percent (0 = clear, 100 = solid);
    /// overrides the alpha of `background`. Text stays solid.
    pub opacity: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum WidgetSpec {
    Label { text: String },
    Clock,
    /// A month grid; `week_start` is "sunday" (default) or "monday".
    Calendar {
        #[serde(default)]
        week_start: WeekStart,
    },
    Cpu,
    /// `index` picks among several GPUs (0 = first).
    Gpu {
        #[serde(default)]
        index: usize,
    },
    /// `interface` pins one interface (e.g. "enp4s0"); by default the
    /// one with the default route is followed.
    Network {
        #[serde(default)]
        interface: Option<String>,
    },
    /// Needs `location`: a place ("Berlin", "Springfield, Illinois") or
    /// "latitude, longitude". Sent to Open-Meteo to fetch the forecast.
    Weather {
        location: String,
        #[serde(default)]
        units: TemperatureUnit,
    },
    /// Ticker symbols as on Yahoo Finance: "AAPL", "^GSPC", "EURUSD=X".
    /// Only the symbols are sent, to Yahoo.
    Stocks {
        symbols: Vec<String>,
    },
    Battery,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekStart {
    #[default]
    Sunday,
    Monday,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemperatureUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

impl WidgetSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            WidgetSpec::Label { .. } => "label",
            WidgetSpec::Clock => "clock",
            WidgetSpec::Calendar { .. } => "calendar",
            WidgetSpec::Cpu => "cpu",
            WidgetSpec::Gpu { .. } => "gpu",
            WidgetSpec::Network { .. } => "network",
            WidgetSpec::Weather { .. } => "weather",
            WidgetSpec::Stocks { .. } => "stocks",
            WidgetSpec::Battery => "battery",
        }
    }
}

impl WidgetSpec {
    /// The `[[widgets]]` table for this spec, leaving out default values.
    fn to_table(&self) -> toml_edit::Table {
        use toml_edit::value;
        let mut t = toml_edit::Table::new();
        t["type"] = value(self.kind());
        match self {
            WidgetSpec::Label { text } => t["text"] = value(text),
            WidgetSpec::Calendar { week_start: WeekStart::Monday } => t["week_start"] = value("monday"),
            WidgetSpec::Gpu { index } if *index > 0 => t["index"] = value(*index as i64),
            WidgetSpec::Network { interface: Some(interface) } => t["interface"] = value(interface),
            WidgetSpec::Weather { location, units } => {
                t["location"] = value(location);
                t["units"] = value(match units {
                    TemperatureUnit::Celsius => "celsius",
                    TemperatureUnit::Fahrenheit => "fahrenheit",
                });
            }
            WidgetSpec::Stocks { symbols } => t["symbols"] = value(symbols.iter().collect::<toml_edit::Array>()),
            _ => {}
        }
        t
    }
}

/// The settings on the settings window's General page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct General {
    /// `dock.edge`: where widgets without a saved placement go.
    pub edge: Edge,
    /// `dock.reserve_space`.
    pub reserve_space: bool,
    pub always_on_top: bool,
    /// `theme.opacity`, in percent.
    pub opacity: u8,
    /// Not in the config file: the system's autostart entry (see
    /// `sidebar_platform::autostart`), filled in and applied by the app.
    pub start_at_login: bool,
}

impl Default for General {
    fn default() -> Self {
        let c = Config::default();
        Self {
            edge: c.default_edge,
            reserve_space: c.reserve_space,
            always_on_top: c.always_on_top,
            opacity: percent_of(c.theme.background.a),
            start_at_login: false,
        }
    }
}

/// Opacity in percent as an alpha value.
fn alpha_of(percent: f32) -> u8 {
    (percent / 100.0 * 255.0).round() as u8
}

/// An alpha value as opacity in whole percent.
fn percent_of(alpha: u8) -> u8 {
    (f32::from(alpha) * 100.0 / 255.0).round() as u8
}

/// Replaces the `[[widgets]]` list in the config file at `path` (created
/// if missing), and with `general` also `always_on_top`, `dock.edge` and
/// `dock.reserve_space`, keeping every other setting and comment, and the
/// comment above the first widget.
pub fn save_settings(path: &Path, general: Option<&General>, specs: &[WidgetSpec]) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let text = replace_settings(&text, general, specs).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Written aside and renamed, so a failure never leaves half a file.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

fn replace_settings(text: &str, general: Option<&General>, specs: &[WidgetSpec]) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    if let Some(g) = general {
        set_general(&mut doc, g)?;
    }
    let first_prefix = doc
        .get("widgets")
        .and_then(|w| w.as_array_of_tables())
        .and_then(|a| a.get(0))
        .and_then(|t| t.decor().prefix())
        .and_then(|p| p.as_str())
        .map(str::to_string);
    let mut array = toml_edit::ArrayOfTables::new();
    for (i, spec) in specs.iter().enumerate() {
        let mut table = spec.to_table();
        let prefix = match (&first_prefix, i) {
            (Some(p), 0) => p.clone(),
            (None, 0) if doc.is_empty() => String::new(),
            _ => "\n".into(),
        };
        table.decor_mut().set_prefix(prefix);
        array.push(table);
    }
    doc.remove("widgets");
    doc.insert("widgets", toml_edit::Item::ArrayOfTables(array));
    Ok(doc.to_string())
}

/// Writes the General settings. A key is written if it is already in the
/// file or differs from the default, so an untouched file stays short.
fn set_general(doc: &mut toml_edit::DocumentMut, g: &General) -> Result<(), String> {
    let d = General::default();
    if doc.contains_key("always_on_top") || g.always_on_top != d.always_on_top {
        set_value(doc.as_table_mut(), "always_on_top", g.always_on_top.into());
    }
    let has = |key: &str| doc.get("dock").and_then(|t| t.as_table_like()).is_some_and(|t| t.contains_key(key));
    let (has_edge, has_reserve) = (has("edge"), has("reserve_space"));
    let has_opacity = doc.get("theme").and_then(|t| t.as_table_like()).is_some_and(|t| t.contains_key("opacity"));
    if has_opacity || g.opacity != d.opacity {
        table(doc, "theme")?;
        let theme = doc["theme"].as_table_like_mut().ok_or("theme: expected a table")?;
        set_value(theme, "opacity", i64::from(g.opacity).into());
    }
    if !(has_edge || has_reserve || g.edge != d.edge || g.reserve_space != d.reserve_space) {
        return Ok(());
    }
    table(doc, "dock")?;
    let dock = doc["dock"].as_table_like_mut().ok_or("dock: expected a table")?;
    if has_edge || g.edge != d.edge {
        set_value(dock, "edge", g.edge.as_str().into());
    }
    if has_reserve || g.reserve_space != d.reserve_space {
        set_value(dock, "reserve_space", g.reserve_space.into());
    }
    Ok(())
}

/// Adds an empty `[name]` table unless there is one.
fn table(doc: &mut toml_edit::DocumentMut, name: &str) -> Result<(), String> {
    if !doc.contains_key(name) {
        let mut table = toml_edit::Table::new();
        table.decor_mut().set_prefix("\n");
        doc.insert(name, toml_edit::Item::Table(table));
    }
    Ok(())
}

/// Sets `key`, keeping the comment and spacing around an existing value.
fn set_value(table: &mut dyn toml_edit::TableLike, key: &str, mut new: toml_edit::Value) {
    if let Some(old) = table.get(key).and_then(|i| i.as_value()) {
        *new.decor_mut() = old.decor().clone();
    }
    table.insert(key, toml_edit::Item::Value(new));
}

pub fn default_widgets() -> Vec<WidgetSpec> {
    vec![WidgetSpec::Clock, WidgetSpec::Cpu, WidgetSpec::Gpu { index: 0 }, WidgetSpec::Network { interface: None }, WidgetSpec::Battery]
}

/// Keys that identify widgets in the saved layout: the widget type, with
/// `-2`, `-3`, ... for repeats (`label`, `label-2`).
pub fn widget_keys(specs: &[WidgetSpec]) -> Vec<String> {
    let mut seen = std::collections::HashMap::<&str, usize>::new();
    specs
        .iter()
        .map(|spec| {
            let n = seen.entry(spec.kind()).or_default();
            *n += 1;
            if *n == 1 { spec.kind().to_string() } else { format!("{}-{n}", spec.kind()) }
        })
        .collect()
}

impl ConfigFile {
    /// Reads `path`. A missing file is an error only if `required`.
    pub fn load(path: &Path, required: bool) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Applies the file on top of the defaults.
    pub fn to_config(&self) -> Result<Config, String> {
        let mut c = Config::default();
        let d = &self.dock;
        if let Some(edge) = &d.edge {
            c.default_edge = edge.parse::<Edge>().map_err(|e| format!("dock.edge: {e}"))?;
        }
        set(&mut c.thickness, d.thickness);
        set(&mut c.padding, d.padding);
        set(&mut c.spacing, d.spacing);
        set(&mut c.reserve_space, d.reserve_space);
        set(&mut c.snap_threshold, self.drag.snap_threshold);
        set(&mut c.allow_floating, self.drag.allow_floating);
        set(&mut c.always_on_top, self.always_on_top);
        let t = &self.theme;
        for (name, value, slot) in [
            ("background", &t.background, &mut c.theme.background),
            ("foreground", &t.foreground, &mut c.theme.foreground),
            ("accent", &t.accent, &mut c.theme.accent),
        ] {
            if let Some(value) = value {
                *slot = parse_color(value).ok_or_else(|| format!("theme.{name}: expected #rrggbb or #rrggbbaa, got {value:?}"))?;
            }
        }
        if let Some(percent) = t.opacity {
            if !(0.0..=100.0).contains(&percent) {
                return Err(format!("theme.opacity: must be between 0 and 100 (percent), got {percent}"));
            }
            c.theme.background.a = alpha_of(percent);
        }
        if !(c.thickness >= 16.0) {
            return Err(format!("dock.thickness: must be at least 16, got {}", c.thickness));
        }
        Ok(c)
    }

    /// The General settings in this file, with defaults for the rest.
    pub fn general(&self) -> General {
        let d = General::default();
        General {
            edge: self.dock.edge.as_deref().and_then(|e| e.parse().ok()).unwrap_or(d.edge),
            reserve_space: self.dock.reserve_space.unwrap_or(d.reserve_space),
            always_on_top: self.always_on_top.unwrap_or(d.always_on_top),
            opacity: match (self.theme.opacity, self.theme.background.as_deref().and_then(parse_color)) {
                (Some(percent), _) => percent.clamp(0.0, 100.0).round() as u8,
                (None, Some(background)) => percent_of(background.a),
                (None, None) => d.opacity,
            },
            start_at_login: false,
        }
    }

    pub fn widgets(&self) -> Vec<WidgetSpec> {
        self.widgets.clone().unwrap_or_else(default_widgets)
    }
}

fn set<T>(slot: &mut T, value: Option<T>) {
    if let Some(v) = value {
        *slot = v;
    }
}

/// `#rrggbb` or `#rrggbbaa`.
fn parse_color(s: &str) -> Option<Color> {
    let hex = s.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let a = if hex.len() == 8 { byte(6)? } else { 255 };
    Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let file = ConfigFile::parse(include_str!("../../../config.example.toml")).unwrap();
        file.to_config().unwrap();
        assert!(!file.widgets().is_empty());
    }

    #[test]
    fn empty_file_gives_defaults() {
        let file = ConfigFile::parse("").unwrap();
        assert_eq!(file.to_config().unwrap(), Config::default());
        assert_eq!(file.widgets(), default_widgets());
    }

    #[test]
    fn overrides_and_errors() {
        assert!(!ConfigFile::parse("").unwrap().to_config().unwrap().always_on_top);
        assert!(ConfigFile::parse("always_on_top = true").unwrap().to_config().unwrap().always_on_top);
        let file = ConfigFile::parse("[dock]\nedge = \"top\"\nthickness = 40\n[theme]\naccent = \"#ff000080\"").unwrap();
        let c = file.to_config().unwrap();
        assert_eq!((c.default_edge, c.thickness), (Edge::Top, 40.0));
        assert_eq!(c.theme.accent, Color::rgba(255, 0, 0, 128));

        assert!(ConfigFile::parse("[dock]\nthikness = 40").is_err(), "typos are rejected");
        assert!(ConfigFile::parse("[[widgets]]\ntype = \"label\"\ntxt = \"x\"").is_err());
        assert!(ConfigFile::parse("[dock]\nedge = \"middle\"").unwrap().to_config().is_err());
        assert!(ConfigFile::parse("[theme]\nbackground = \"red\"").unwrap().to_config().is_err());
    }

    #[test]
    fn parses_widget_options() {
        let file = ConfigFile::parse(
            "[[widgets]]\ntype = \"weather\"\nlocation = \"Berlin\"\nunits = \"fahrenheit\"\n\
             [[widgets]]\ntype = \"weather\"\nlocation = \"Oslo\"\n\
             [[widgets]]\ntype = \"network\"\ninterface = \"enp4s0\"\n[[widgets]]\ntype = \"gpu\"\nindex = 1\n\
             [[widgets]]\ntype = \"stocks\"\nsymbols = [\"AAPL\", \"^GSPC\"]\n\
             [[widgets]]\ntype = \"calendar\"\nweek_start = \"monday\"\n[[widgets]]\ntype = \"calendar\"",
        )
        .unwrap();
        assert_eq!(
            file.widgets(),
            [
                WidgetSpec::Weather { location: "Berlin".into(), units: TemperatureUnit::Fahrenheit },
                WidgetSpec::Weather { location: "Oslo".into(), units: TemperatureUnit::Celsius },
                WidgetSpec::Network { interface: Some("enp4s0".into()) },
                WidgetSpec::Gpu { index: 1 },
                WidgetSpec::Stocks { symbols: vec!["AAPL".into(), "^GSPC".into()] },
                WidgetSpec::Calendar { week_start: WeekStart::Monday },
                WidgetSpec::Calendar { week_start: WeekStart::Sunday },
            ]
        );
        assert!(ConfigFile::parse("[[widgets]]\ntype = \"weather\"").is_err(), "location is required");
        assert!(ConfigFile::parse("[[widgets]]\ntype = \"weather\"\nlocation = \"x\"\nunits = \"kelvin\"").is_err());
    }

    #[test]
    fn saves_general_settings() {
        let general = |edge, reserve_space, always_on_top| General { edge, reserve_space, always_on_top, opacity: 90, start_at_login: false };
        // Defaults on a fresh file: nothing extra is written.
        let fresh = replace_settings("", Some(&General::default()), &[WidgetSpec::Clock]).unwrap();
        assert_eq!(fresh, "[[widgets]]\ntype = \"clock\"\n");
        // Changed values are added.
        let changed = replace_settings("", Some(&general(Edge::Left, false, true)), &[WidgetSpec::Clock]).unwrap();
        let file = ConfigFile::parse(&changed).unwrap();
        assert_eq!(file.general(), general(Edge::Left, false, true));
        // Existing keys are updated in place, comments kept.
        let before = "always_on_top = true  # mine\n\n[dock]\nthickness = 200\nreserve_space = false # off\n";
        let after = replace_settings(before, Some(&General::default()), &[WidgetSpec::Clock]).unwrap();
        assert!(after.starts_with("always_on_top = false  # mine\n"), "{after}");
        assert!(after.contains("thickness = 200\nreserve_space = true # off\n"), "{after}");
        assert!(!after.contains("edge"), "{after}");
        assert_eq!(ConfigFile::parse(&after).unwrap().general(), General::default());
    }

    #[test]
    fn sets_opacity() {
        let file = ConfigFile::parse("[theme]\nbackground = \"#10203080\"\nopacity = 75\n").unwrap();
        let theme = file.to_config().unwrap().theme;
        assert_eq!((theme.background.r, theme.background.a), (0x10, 191));
        assert_eq!(file.general().opacity, 75);
        // Without opacity, the background's own alpha counts.
        assert_eq!(ConfigFile::parse("[theme]\nbackground = \"#10203080\"\n").unwrap().general().opacity, 50);
        assert!(ConfigFile::parse("[theme]\nopacity = 150\n").unwrap().to_config().unwrap_err().contains("theme.opacity"));
        // Saved under [theme], and only when changed.
        let g = General { opacity: 60, ..General::default() };
        let after = replace_settings("", Some(&g), &[WidgetSpec::Clock]).unwrap();
        assert_eq!(ConfigFile::parse(&after).unwrap().general(), g);
        assert!(after.contains("[theme]\nopacity = 60\n"), "{after}");
    }

    #[test]
    fn saving_widgets_keeps_the_rest() {
        let before = "# My sidebar\nbackend = \"x11\"\n\n[dock]\nedge = \"left\"  # keep\n\n\
                      # The widgets:\n[[widgets]]\ntype = \"clock\"\n\n[[widgets]]\ntype = \"battery\"\n\n[theme]\naccent = \"#ff0000\"\n";
        let specs = vec![
            WidgetSpec::Clock,
            WidgetSpec::Calendar { week_start: WeekStart::Monday },
            WidgetSpec::Gpu { index: 0 },
            WidgetSpec::Gpu { index: 1 },
            WidgetSpec::Network { interface: Some("enp4s0".into()) },
            WidgetSpec::Weather { location: "Springfield, Illinois".into(), units: TemperatureUnit::Fahrenheit },
            WidgetSpec::Stocks { symbols: vec!["AAPL".into(), "^GSPC".into()] },
            WidgetSpec::Label { text: "hi \"there\"".into() },
        ];
        let after = replace_settings(before, None, &specs).unwrap();
        let file = ConfigFile::parse(&after).unwrap();
        assert_eq!(file.widgets(), specs);
        assert_eq!(file.backend.as_deref(), Some("x11"));
        assert_eq!(file.to_config().unwrap().default_edge, Edge::Left);
        for kept in ["# My sidebar", "edge = \"left\"  # keep", "# The widgets:\n[[widgets]]\ntype = \"clock\"", "accent = \"#ff0000\""] {
            assert!(after.contains(kept), "{kept:?} missing from:\n{after}");
        }
        assert!(!after.contains("battery"));
        // Defaults are left out.
        assert!(after.contains("[[widgets]]\ntype = \"gpu\"\n\n[[widgets]]\ntype = \"gpu\"\nindex = 1\n"), "{after}");

        let fresh = replace_settings("", None, &[WidgetSpec::Clock, WidgetSpec::Cpu]).unwrap();
        assert_eq!(fresh, "[[widgets]]\ntype = \"clock\"\n\n[[widgets]]\ntype = \"cpu\"\n");
    }

    #[test]
    fn repeated_widgets_get_distinct_keys() {
        let label = |t: &str| WidgetSpec::Label { text: t.into() };
        let keys = widget_keys(&[label("a"), WidgetSpec::Clock, label("b")]);
        assert_eq!(keys, ["label", "clock", "label-2"]);
    }
}
