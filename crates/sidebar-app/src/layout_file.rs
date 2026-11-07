//! Saved widget placements (TOML), written after every drop and read at
//! startup. Keyed by `config_file::widget_keys`; entries for widgets not
//! in the current config are kept, so removing a widget for a while does
//! not forget where it was.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sidebar_core::{Edge, Rect, SavedPlacement, SavedSpot};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Entry {
    Docked { monitor: String, edge: String, index: usize },
    /// Position relative to the monitor's top-left corner.
    Floating { monitor: String, x: f32, y: f32, width: f32, height: f32 },
}

pub type Layout = BTreeMap<String, SavedPlacement>;

impl From<&SavedPlacement> for Entry {
    fn from(p: &SavedPlacement) -> Self {
        let monitor = p.monitor.clone();
        match p.spot {
            SavedSpot::Docked { edge, index } => Entry::Docked { monitor, edge: edge.as_str().into(), index },
            SavedSpot::Floating { rect } => {
                Entry::Floating { monitor, x: rect.x, y: rect.y, width: rect.width, height: rect.height }
            }
        }
    }
}

impl TryFrom<Entry> for SavedPlacement {
    type Error = String;

    fn try_from(e: Entry) -> Result<Self, String> {
        Ok(match e {
            Entry::Docked { monitor, edge, index } => {
                SavedPlacement { monitor, spot: SavedSpot::Docked { edge: edge.parse::<Edge>()?, index } }
            }
            Entry::Floating { monitor, x, y, width, height } => {
                SavedPlacement { monitor, spot: SavedSpot::Floating { rect: Rect::new(x, y, width, height) } }
            }
        })
    }
}

/// Reads the layout. A missing file is an empty layout; bad entries are
/// skipped with a warning rather than failing startup.
pub fn load(path: &Path) -> Layout {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Layout::new(),
        Err(e) => {
            eprintln!("warning: {}: {e}", path.display());
            return Layout::new();
        }
    };
    parse(&text).unwrap_or_else(|e| {
        eprintln!("warning: ignoring saved layout {}: {e}", path.display());
        Layout::new()
    })
}

fn parse(text: &str) -> Result<Layout, String> {
    let entries: BTreeMap<String, Entry> = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut layout = Layout::new();
    for (key, entry) in entries {
        match SavedPlacement::try_from(entry) {
            Ok(p) => {
                layout.insert(key, p);
            }
            Err(e) => eprintln!("warning: saved layout entry '{key}': {e}"),
        }
    }
    Ok(layout)
}

fn render(layout: &Layout) -> Result<String, String> {
    let entries: BTreeMap<&String, Entry> = layout.iter().map(|(k, p)| (k, Entry::from(p))).collect();
    let body = toml::to_string(&entries).map_err(|e| e.to_string())?;
    Ok(format!("# Written by sidebar when widgets are dragged. Safe to delete.\n\n{body}"))
}

/// Writes atomically (temp file + rename), creating the directory.
pub fn save(path: &Path, layout: &Layout) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, render(layout)?).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let mut layout = Layout::new();
        layout.insert(
            "clock".into(),
            SavedPlacement { monitor: "DP-1".into(), spot: SavedSpot::Docked { edge: Edge::Left, index: 1 } },
        );
        layout.insert(
            "label-2".into(),
            SavedPlacement {
                monitor: "HDMI-A-1".into(),
                spot: SavedSpot::Floating { rect: Rect::new(10.0, 20.5, 148.0, 33.0) },
            },
        );
        assert_eq!(parse(&render(&layout).unwrap()).unwrap(), layout);
    }

    #[test]
    fn bad_entries_are_skipped() {
        let text = "[a]\nkind = \"docked\"\nmonitor = \"X\"\nedge = \"sideways\"\nindex = 0\n\
                    [b]\nkind = \"docked\"\nmonitor = \"X\"\nedge = \"top\"\nindex = 2\n";
        let layout = parse(text).unwrap();
        assert_eq!(layout.keys().collect::<Vec<_>>(), ["b"]);
    }
}
