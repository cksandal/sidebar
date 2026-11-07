//! Linux backends.
//!
//! Linux has no single "desktop API", so the session type is detected at
//! runtime and the matching protocol backend is used. Nothing outside this
//! module may know which one was picked.

use crate::{Platform, PlatformError, Result};

#[cfg(feature = "wayland")]
pub mod wayland;
#[cfg(feature = "wayland")]
mod xkb;
#[cfg(feature = "x11")]
pub mod x11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Wayland,
    X11,
    Unknown,
}

/// Detects the display session from the environment, without connecting.
pub fn detect_session() -> Session {
    let var = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => return Session::Wayland,
        Ok("x11") => return Session::X11,
        _ => {}
    }
    if var("WAYLAND_DISPLAY") {
        Session::Wayland
    } else if var("DISPLAY") {
        Session::X11
    } else {
        Session::Unknown
    }
}

/// Connects to the backend matching the current session.
///
/// On a Wayland compositor without layer-shell (e.g. GNOME/Mutter) this
/// falls back to X11 via XWayland: positioning still works there, but space
/// reservation does not (the X11 backend reports that via its capabilities).
pub fn connect() -> Result<Box<dyn Platform>> {
    match detect_session() {
        #[cfg(feature = "wayland")]
        Session::Wayland => match wayland::WaylandPlatform::connect() {
            Ok(platform) => Ok(Box::new(platform)),
            #[cfg(feature = "x11")]
            Err(PlatformError::Unsupported(why)) if std::env::var_os("DISPLAY").is_some() => {
                eprintln!("[linux] wayland: {why}; falling back to X11 via XWayland");
                Ok(Box::new(x11::X11Platform::connect()?))
            }
            Err(e) => Err(e),
        },
        #[cfg(feature = "x11")]
        Session::X11 => Ok(Box::new(x11::X11Platform::connect()?)),
        session => Err(PlatformError::Unsupported(format!(
            "no enabled backend for session {session:?} (WAYLAND_DISPLAY/DISPLAY unset or feature disabled)"
        ))),
    }
}

/// Waits until `fd` is readable or `timeout` passes. Returns true if readable.
#[cfg(any(feature = "wayland", feature = "x11"))]
pub(crate) fn wait_readable(fd: std::os::fd::RawFd, timeout: std::time::Duration) -> bool {
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: `pfd` is a valid, exclusively borrowed pollfd for the call.
    let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
    ready > 0 && pfd.revents & libc::POLLIN != 0
}
