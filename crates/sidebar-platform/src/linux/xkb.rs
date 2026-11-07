//! Keyboard layouts for the Wayland backend: libxkbcommon turns the
//! compositor's keymap and modifier state into keysyms and text. It is
//! loaded at run time (every Wayland desktop has it), so building needs no
//! system libraries. Compose sequences (dead keys) are not handled.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use sidebar_core::Key;

type Ptr = *mut c_void;

struct Api {
    context_new: unsafe extern "C" fn(c_int) -> Ptr,
    context_unref: unsafe extern "C" fn(Ptr),
    keymap_new_from_string: unsafe extern "C" fn(Ptr, *const c_char, c_int, c_int) -> Ptr,
    keymap_unref: unsafe extern "C" fn(Ptr),
    keymap_key_repeats: unsafe extern "C" fn(Ptr, u32) -> c_int,
    state_new: unsafe extern "C" fn(Ptr) -> Ptr,
    state_unref: unsafe extern "C" fn(Ptr),
    state_update_mask: unsafe extern "C" fn(Ptr, u32, u32, u32, u32, u32, u32) -> c_int,
    state_key_get_one_sym: unsafe extern "C" fn(Ptr, u32) -> u32,
    state_key_get_utf32: unsafe extern "C" fn(Ptr, u32) -> u32,
    state_mod_name_is_active: unsafe extern "C" fn(Ptr, *const c_char, c_int) -> c_int,
}

const KEYMAP_FORMAT_TEXT_V1: c_int = 1;
const STATE_MODS_EFFECTIVE: c_int = 1 << 3;
/// evdev key codes are XKB key codes minus 8.
const EVDEV_OFFSET: u32 = 8;

// The transmutes' targets are the `Api` field types.
#[allow(clippy::missing_transmute_annotations)]
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: dlopen/dlsym with NUL-terminated names; each symbol is
        // cast to its signature from xkbcommon.h.
        unsafe {
            let lib = libc::dlopen(c"libxkbcommon.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if lib.is_null() {
                return None;
            }
            macro_rules! sym {
                ($name:literal) => {{
                    let p = libc::dlsym(lib, $name.as_ptr());
                    if p.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, _>(p)
                }};
            }
            Some(Api {
                context_new: sym!(c"xkb_context_new"),
                context_unref: sym!(c"xkb_context_unref"),
                keymap_new_from_string: sym!(c"xkb_keymap_new_from_string"),
                keymap_unref: sym!(c"xkb_keymap_unref"),
                keymap_key_repeats: sym!(c"xkb_keymap_key_repeats"),
                state_new: sym!(c"xkb_state_new"),
                state_unref: sym!(c"xkb_state_unref"),
                state_update_mask: sym!(c"xkb_state_update_mask"),
                state_key_get_one_sym: sym!(c"xkb_state_key_get_one_sym"),
                state_key_get_utf32: sym!(c"xkb_state_key_get_utf32"),
                state_mod_name_is_active: sym!(c"xkb_state_mod_name_is_active"),
            })
        }
    })
    .as_ref()
}

/// A compiled keymap and its modifier state.
pub struct Xkb {
    api: &'static Api,
    context: Ptr,
    keymap: Ptr,
    state: Ptr,
}

impl Xkb {
    /// From the compositor's XKB text keymap; `None` if libxkbcommon is
    /// missing or rejects it.
    pub fn new(keymap: &CStr) -> Option<Self> {
        let api = api()?;
        // SAFETY: valid pointers from libxkbcommon, each checked; on
        // failure what was created is released.
        unsafe {
            let context = (api.context_new)(0);
            if context.is_null() {
                return None;
            }
            let map = (api.keymap_new_from_string)(context, keymap.as_ptr(), KEYMAP_FORMAT_TEXT_V1, 0);
            let state = if map.is_null() { std::ptr::null_mut() } else { (api.state_new)(map) };
            if state.is_null() {
                if !map.is_null() {
                    (api.keymap_unref)(map);
                }
                (api.context_unref)(context);
                return None;
            }
            Some(Self { api, context, keymap: map, state })
        }
    }

    pub fn update_modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        // SAFETY: a live state.
        unsafe { (self.api.state_update_mask)(self.state, depressed, latched, locked, 0, 0, group) };
    }

    /// Whether holding the key repeats it.
    pub fn repeats(&self, code: u32) -> bool {
        // SAFETY: a live keymap.
        unsafe { (self.api.keymap_key_repeats)(self.keymap, code + EVDEV_OFFSET) != 0 }
    }

    /// The key an evdev `code` gives with the current modifiers.
    pub fn key(&self, code: u32) -> Option<Key> {
        let code = code + EVDEV_OFFSET;
        // SAFETY: a live state; modifier names are NUL-terminated.
        let (sym, text, held) = unsafe {
            let held = |name: &CStr| (self.api.state_mod_name_is_active)(self.state, name.as_ptr(), STATE_MODS_EFFECTIVE) > 0;
            (
                (self.api.state_key_get_one_sym)(self.state, code),
                char::from_u32((self.api.state_key_get_utf32)(self.state, code)),
                held(c"Control") || held(c"Mod1"),
            )
        };
        key_from(sym, text, held)
    }
}

impl Drop for Xkb {
    fn drop(&mut self) {
        // SAFETY: each was created by this value and is released once.
        unsafe {
            (self.api.state_unref)(self.state);
            (self.api.keymap_unref)(self.keymap);
            (self.api.context_unref)(self.context);
        }
    }
}

/// Maps a keysym (and the text it types) to a [`Key`]. Presses with Ctrl
/// or Alt held type nothing.
pub fn key_from(sym: u32, text: Option<char>, ctrl_or_alt: bool) -> Option<Key> {
    // Keysym values from xkbcommon-keysyms.h.
    Some(match sym {
        0xff08 => Key::Backspace,
        0xff09 => Key::Tab,
        0xfe20 => Key::BackTab,
        0xff0d | 0xff8d => Key::Enter,
        0xff1b => Key::Escape,
        0xffff | 0xff9f => Key::Delete,
        0xff50 | 0xff95 => Key::Home,
        0xff57 | 0xff9c => Key::End,
        0xff51 | 0xff96 => Key::Left,
        0xff52 | 0xff97 => Key::Up,
        0xff53 | 0xff98 => Key::Right,
        0xff54 | 0xff99 => Key::Down,
        _ => match text {
            Some(c) if !ctrl_or_alt && !c.is_control() => Key::Char(c),
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keysyms() {
        assert_eq!(key_from(0xff08, Some('\u{8}'), false), Some(Key::Backspace));
        assert_eq!(key_from(0xfe20, None, false), Some(Key::BackTab));
        assert_eq!(key_from(0xff8d, Some('\r'), false), Some(Key::Enter));
        assert_eq!(key_from(0x00e9, Some('é'), false), Some(Key::Char('é')));
        assert_eq!(key_from(0x0061, Some('a'), true), None, "Ctrl+A types nothing");
        assert_eq!(key_from(0xffe1, None, false), None, "Shift alone");
    }

    #[test]
    fn compiles_a_keymap() {
        // Skipped where libxkbcommon isn't installed (e.g. minimal CI).
        if api().is_none() {
            return;
        }
        let keymap = c"xkb_keymap {
            xkb_keycodes { minimum = 8; maximum = 255; <AC01> = 38; <BKSP> = 22; };
            xkb_types { include \"basic\" };
            xkb_compat { include \"basic\" };
            xkb_symbols { key <AC01> { [ a, A ] }; key <BKSP> { [ BackSpace ] }; };
        };";
        let xkb = Xkb::new(keymap).expect("keymap");
        assert_eq!(xkb.key(38 - 8), Some(Key::Char('a')));
        assert_eq!(xkb.key(22 - 8), Some(Key::Backspace));
    }
}
