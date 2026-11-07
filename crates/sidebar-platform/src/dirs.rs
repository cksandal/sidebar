//! Per-user directories for the sidebar, following each OS's conventions.

use std::env;
use std::path::PathBuf;

const APP: &str = "sidebar";

fn env_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Where the user's settings live: `$XDG_CONFIG_HOME/sidebar` (default
/// `~/.config/sidebar`), or `%APPDATA%\sidebar` on Windows.
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        env_dir("APPDATA")
    } else {
        env_dir("XDG_CONFIG_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join(APP))
}

/// Where state the app writes itself (saved widget placements) lives:
/// `$XDG_STATE_HOME/sidebar` (default `~/.local/state/sidebar`), or
/// `%LOCALAPPDATA%\sidebar` on Windows.
pub fn state_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        env_dir("LOCALAPPDATA")
    } else {
        env_dir("XDG_STATE_HOME").or_else(|| env_dir("HOME").map(|h| h.join(".local/state")))
    };
    base.map(|b| b.join(APP))
}
