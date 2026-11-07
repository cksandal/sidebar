#![windows_subsystem = "windows"]
//! Synthetic mouse input for `scripts/win.sh input`, run in the VM's
//! interactive session by a scheduled task. No dependencies, so it builds
//! with plain rustc:
//!
//!   rustc --target x86_64-pc-windows-gnu -C linker=x86_64-w64-mingw32-gcc scripts/win-input.rs
//!
//! Reads its commands from `input.txt` next to the executable and writes
//! output and errors to `win-input.log` there. A GUI-subsystem program, so
//! no console window opens on the desktop. It runs elevated: Windows
//! silently drops synthetic input from a normal process while an elevated
//! window (an admin terminal, say) is in the foreground.
//!
//! Commands (physical screen pixels), separated by `;` or newlines:
//!   move X Y | press [right] | release [right] | click X Y [right]
//!   drag X1 Y1 X2 Y2 | glide X1 Y1 X2 Y2 | wheel N | wait MS | where
//!   type TEXT | key NAME (enter, esc, tab, backtab, backspace, delete,
//!   left, right, up, down, home, end)
//!   monitors (log each monitor's bounds, work area and DPI)
//!   dpi N (primary monitor's scale, in steps from the recommended one:
//!   0 = recommended, 1 = one step up, e.g. 100% -> 125%; undocumented
//!   SPI_SETLOGICALDPIOVERRIDE, as used by display-scaling tools)

use std::ffi::c_void;
use std::thread::sleep;
use std::time::Duration;

#[repr(C)]
#[derive(Default)]
struct Point {
    x: i32,
    y: i32,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn SetCursorPos(x: i32, y: i32) -> i32;
    fn GetCursorPos(p: *mut Point) -> i32;
    fn WindowFromPoint(p: Point) -> *mut c_void;
    fn mouse_event(flags: u32, dx: u32, dy: u32, data: i32, extra: usize);
    fn SetProcessDpiAwarenessContext(value: isize) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
    fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
}

/// INPUT with the KEYBDINPUT member (the union is as large as MOUSEINPUT).
#[repr(C)]
struct Input {
    kind: u32,
    /// The union is 8-byte aligned.
    _pad: u32,
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
    _rest: [u8; 8],
}

const _: () = assert!(size_of::<Input>() == 40);
const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x02;
const KEYEVENTF_UNICODE: u32 = 0x04;

fn type_text(text: &str) {
    for unit in text.encode_utf16() {
        let key = |flags| Input { kind: INPUT_KEYBOARD, _pad: 0, vk: 0, scan: unit, flags, time: 0, extra: 0, _rest: [0; 8] };
        let inputs = [key(KEYEVENTF_UNICODE), key(KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)];
        unsafe { SendInput(2, inputs.as_ptr(), size_of::<Input>() as i32) };
        pause(20);
    }
}

fn press_key(name: &str) -> Result<(), String> {
    let (vk, shift) = match name {
        "enter" => (0x0D, false),
        "esc" => (0x1B, false),
        "tab" => (0x09, false),
        "backtab" => (0x09, true),
        "backspace" => (0x08, false),
        "delete" => (0x2E, false),
        "left" => (0x25, false),
        "up" => (0x26, false),
        "right" => (0x27, false),
        "down" => (0x28, false),
        "home" => (0x24, false),
        "end" => (0x23, false),
        _ => return Err(format!("unknown key {name:?}")),
    };
    const SHIFT: u8 = 0x10;
    // Arrows and friends are extended keys.
    let extended = if matches!(vk, 0x21..=0x2E) { 0x01 } else { 0 };
    unsafe {
        if shift {
            keybd_event(SHIFT, 0, 0, 0);
        }
        keybd_event(vk, 0, extended, 0);
        keybd_event(vk, 0, extended | KEYEVENTF_KEYUP, 0);
        if shift {
            keybd_event(SHIFT, 0, KEYEVENTF_KEYUP, 0);
        }
    }
    pause(30);
    Ok(())
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MonitorInfo {
    size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
    device: [u16; 32],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumDisplayMonitors(dc: *mut c_void, clip: *const Rect, proc_: extern "system" fn(*mut c_void, *mut c_void, *mut Rect, isize) -> i32, data: isize) -> i32;
    fn GetMonitorInfoW(monitor: *mut c_void, info: *mut MonitorInfo) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, pv: *mut c_void, flags: u32) -> i32;
}

#[link(name = "shcore")]
unsafe extern "system" {
    fn GetDpiForMonitor(monitor: *mut c_void, kind: u32, x: *mut u32, y: *mut u32) -> i32;
}

extern "system" fn each_monitor(monitor: *mut c_void, _dc: *mut c_void, _rect: *mut Rect, _data: isize) -> i32 {
    let mut info = MonitorInfo { size: size_of::<MonitorInfo>() as u32, monitor: Rect::default(), work: Rect::default(), flags: 0, device: [0; 32] };
    let (mut dx, mut dy) = (0, 0);
    unsafe {
        GetMonitorInfoW(monitor, &mut info);
        GetDpiForMonitor(monitor, 0, &mut dx, &mut dy);
    }
    let name = String::from_utf16_lossy(&info.device[..info.device.iter().position(|&c| c == 0).unwrap_or(32)]);
    let r = |r: Rect| format!("{},{} {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top);
    log(&format!("{name}: {} work {} dpi {dx} ({}%){}", r(info.monitor), r(info.work), dx * 100 / 96, if info.flags & 1 != 0 { " primary" } else { "" }));
    1
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

const LEFTDOWN: u32 = 0x02;
const LEFTUP: u32 = 0x04;
const RIGHTDOWN: u32 = 0x08;
const RIGHTUP: u32 = 0x10;
const WHEEL: u32 = 0x800;
const MOVE: u32 = 0x01;
const ABSOLUTE: u32 = 0x8000;
const VIRTUALDESK: u32 = 0x4000;
const SM_XVIRTUALSCREEN: i32 = 76;
const SM_CXVIRTUALSCREEN: i32 = 78;

fn pause(ms: u64) {
    sleep(Duration::from_millis(ms));
}

fn move_to(x: i32, y: i32) {
    // SetCursorPos is ignored when VirtualBox's mouse integration owns the
    // pointer; an absolute move through the input stream works either way.
    unsafe {
        if SetCursorPos(x, y) == 0 {
            log(&format!("SetCursorPos failed: error {}", GetLastError()));
        }
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_XVIRTUALSCREEN + 1));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2), GetSystemMetrics(SM_CXVIRTUALSCREEN + 1).max(2));
        let nx = ((x - vx) as i64 * 65535 / (vw - 1) as i64) as u32;
        let ny = ((y - vy) as i64 * 65535 / (vh - 1) as i64) as u32;
        mouse_event(MOVE | ABSOLUTE | VIRTUALDESK, nx, ny, 0, 0);
    }
    pause(15);
}

fn button(right: bool, down: bool) {
    let flags = match (right, down) {
        (false, true) => LEFTDOWN,
        (false, false) => LEFTUP,
        (true, true) => RIGHTDOWN,
        (true, false) => RIGHTUP,
    };
    unsafe { mouse_event(flags, 0, 0, 0, 0) };
    pause(40);
}

fn glide(x1: i32, y1: i32, x2: i32, y2: i32) {
    let steps = 25;
    for i in 1..=steps {
        move_to(x1 + (x2 - x1) * i / steps, y1 + (y2 - y1) * i / steps);
    }
}

fn run(cmd: &str) -> Result<(), String> {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let Some((&name, args)) = words.split_first() else { return Ok(()) };
    let n = |i: usize| -> Result<i32, String> { args.get(i).ok_or(format!("{name}: missing argument"))?.parse().map_err(|e| format!("{name}: {e}")) };
    let right = args.last() == Some(&"right");
    match name {
        "move" => move_to(n(0)?, n(1)?),
        "press" => button(right, true),
        "release" => button(right, false),
        "click" => {
            move_to(n(0)?, n(1)?);
            button(right, true);
            button(right, false);
        }
        "glide" => glide(n(0)?, n(1)?, n(2)?, n(3)?),
        "drag" => {
            let (x1, y1, x2, y2) = (n(0)?, n(1)?, n(2)?, n(3)?);
            move_to(x1, y1);
            button(false, true);
            glide(x1, y1, x2, y2);
            pause(100);
            button(false, false);
        }
        "wheel" => unsafe { mouse_event(WHEEL, 0, 0, 120 * n(0)?, 0) },
        "wait" => pause(n(0)? as u64),
        "type" => type_text(cmd.trim_start().strip_prefix("type").unwrap_or("").trim_start()),
        "key" => press_key(args.first().ok_or("key: missing name")?)?,
        "where" => {
            let mut p = Point::default();
            unsafe { GetCursorPos(&mut p) };
            let window = unsafe { WindowFromPoint(Point { x: p.x, y: p.y }) };
            log(&format!("cursor at {},{}; window {window:?}", p.x, p.y));
        }
        "monitors" => unsafe {
            EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), each_monitor, 0);
        },
        "dpi" => {
            // SPI_SETLOGICALDPIOVERRIDE with SPIF_UPDATEINIFILE | SPIF_SENDCHANGE.
            if unsafe { SystemParametersInfoW(0x009F, n(0)? as u32, std::ptr::null_mut(), 3) } == 0 {
                return Err(format!("dpi: failed, error {}", unsafe { GetLastError() }));
            }
            pause(1500);
        }
        _ => return Err(format!("unknown command {name:?}")),
    }
    Ok(())
}

fn dir() -> std::path::PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default()
}

fn log(line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("win-input.log")) {
        let _ = writeln!(f, "{line}");
    }
}

fn main() {
    // Per-monitor aware, so coordinates are physical pixels.
    unsafe { SetProcessDpiAwarenessContext(-4) };
    let _ = std::fs::remove_file(dir().join("win-input.log"));
    let script = std::fs::read_to_string(dir().join("input.txt")).unwrap_or_default();
    for cmd in script.split([';', '\n']) {
        if let Err(e) = run(cmd) {
            log(&e);
            std::process::exit(1);
        }
    }
    log("done");
}
