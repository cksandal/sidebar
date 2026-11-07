//! Starting the sidebar when the user logs in: an entry under the per-user
//! `Run` registry key on Windows, or an XDG autostart entry
//! (`~/.config/autostart/sidebar.desktop`) on Linux. Both point at the
//! running executable.

use std::path::PathBuf;

/// Whether the sidebar is set to start at login.
pub fn enabled() -> bool {
    imp::enabled()
}

/// Turns starting at login on or off.
pub fn set(on: bool) -> Result<(), String> {
    imp::set(on)
}

/// The running executable. Linux reports "<path> (deleted)" once the
/// binary has been replaced, e.g. by a rebuild.
fn exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own executable: {e}"))?;
    Ok(match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) if !exe.exists() => PathBuf::from(path),
        _ => exe,
    })
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Sidebar";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    pub fn enabled() -> bool {
        let (key, name) = (wide(RUN), wide(NAME));
        // SAFETY: NUL-terminated strings; null outputs only test existence.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        status == ERROR_SUCCESS
    }

    pub fn set(on: bool) -> Result<(), String> {
        let (key, name) = (wide(RUN), wide(NAME));
        let status = if on {
            let value = wide(&format!("\"{}\"", super::exe()?.display()));
            // SAFETY: as above; `value` is REG_SZ data including its NUL.
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    value.as_ptr().cast(),
                    (value.len() * 2) as u32,
                )
            }
        } else {
            // SAFETY: as above.
            match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
                ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
                status => status,
            }
        };
        if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("registry error {status}")) }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    fn path() -> Result<PathBuf, String> {
        crate::dirs::xdg_config_home()
            .map(|d| d.join("autostart").join("sidebar.desktop"))
            .ok_or_else(|| "no config directory (HOME is not set)".into())
    }

    pub fn enabled() -> bool {
        path().ok().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|text| is_enabled(&text))
    }

    pub fn set(on: bool) -> Result<(), String> {
        let path = path()?;
        if on {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            }
            std::fs::write(&path, entry(&super::exe()?)).map_err(|e| format!("cannot write {}: {e}", path.display()))
        } else {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("cannot remove {}: {e}", path.display())),
                _ => Ok(()),
            }
        }
    }

    /// An entry the desktop hides with `Hidden=true` (or GNOME's own
    /// switch) does not start.
    pub(super) fn is_enabled(text: &str) -> bool {
        !text.lines().any(|l| matches!(l.trim(), "Hidden=true" | "X-GNOME-Autostart-enabled=false"))
    }

    pub(super) fn entry(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Sidebar\nComment=Desktop sidebar with system and information widgets\n\
             Exec={}\nIcon=utilities-system-monitor\nTerminal=false\nNoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=3\n",
            quote(&exe.to_string_lossy())
        )
    }

    /// Quotes a path for `Exec=` when needed (Desktop Entry spec: inside
    /// double quotes, `"`, `` ` ``, `$` and `\` are escaped with `\`).
    fn quote(s: &str) -> String {
        if !s.chars().any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c)) {
            return s.to_string();
        }
        let mut out = String::from("\"");
        for c in s.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::imp::*;
    use std::path::Path;

    #[test]
    fn writes_desktop_entries() {
        let e = entry(Path::new("/home/you/.cargo/bin/sidebar"));
        assert!(e.contains("\nExec=/home/you/.cargo/bin/sidebar\n"));
        assert!(is_enabled(&e));
        let e = entry(Path::new("/opt/my apps/side$bar"));
        assert!(e.contains("\nExec=\"/opt/my apps/side\\$bar\"\n"), "{e}");
        assert!(!is_enabled(&format!("{e}Hidden=true\n")));
    }
}
