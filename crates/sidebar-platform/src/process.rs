//! Where the process's messages go.
//!
//! On Windows the sidebar is a GUI program (no console window opens with
//! it), so it starts without anywhere for `eprintln!` to write. Started
//! from a terminal, it attaches to that terminal's console; started from
//! Explorer or at login, it writes to `sidebar.log` in the state
//! directory. Output that was already redirected (to a file or a pipe) is
//! left alone. Elsewhere nothing needs doing.

use std::path::PathBuf;

/// Sets up stdout and stderr. Returns the log file's path when output goes
/// to one.
pub fn init_output() -> Option<PathBuf> {
    imp::init_output()
}

#[cfg(not(windows))]
mod imp {
    pub fn init_output() -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::io::IntoRawHandle;
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };

    /// Output to a file or a pipe, which works without a console.
    fn redirected(h: HANDLE) -> bool {
        // SAFETY: GetFileType accepts any handle value.
        !h.is_null() && h != INVALID_HANDLE_VALUE && matches!(unsafe { GetFileType(h) }, FILE_TYPE_DISK | FILE_TYPE_PIPE)
    }

    /// Points stderr, and stdout unless it is redirected, at `h`.
    fn use_handle(h: HANDLE, out_redirected: bool) {
        // SAFETY: plain handle values; the handle stays open for the
        // life of the process.
        unsafe {
            SetStdHandle(STD_ERROR_HANDLE, h);
            if !out_redirected {
                SetStdHandle(STD_OUTPUT_HANDLE, h);
            }
        }
    }

    pub fn init_output() -> Option<PathBuf> {
        // SAFETY: no preconditions.
        let (out, err) = unsafe { (GetStdHandle(STD_OUTPUT_HANDLE), GetStdHandle(STD_ERROR_HANDLE)) };
        if redirected(err) {
            return None;
        }
        let out_redirected = redirected(out);
        // SAFETY: no preconditions; fails when the parent has no console.
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0 {
            let name: Vec<u16> = "CONOUT$".encode_utf16().chain([0]).collect();
            // SAFETY: a NUL-terminated name; null security attributes and
            // template are allowed.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                use_handle(h, out_redirected);
                return None;
            }
        }
        let path = crate::dirs::state_dir()?.join("sidebar.log");
        std::fs::create_dir_all(path.parent()?).ok()?;
        let file = std::fs::File::create(&path).ok()?;
        use_handle(file.into_raw_handle(), out_redirected);
        Some(path)
    }
}
