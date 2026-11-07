# Architecture

The sidebar is a Cargo workspace. Most of the code does not depend on the
operating system, window manager or display protocol; per-platform code is
kept in two crates behind small traits.

## Crates

```
crates/
  sidebar-core      model: geometry, snapping, dock layout, Shell, Widget trait,
                    Source<T> data trait, Scene display lists
  sidebar-render    software rasterizer: Scene -> RGBA pixels; text via cosmic-text
  sidebar-widgets   built-in widgets (depend on sidebar-core only)
  sidebar-platform  Platform trait and backends
    headless.rs       no display; for tests and CI, can dump frames as PPM
    linux/            session detection (picks Wayland or X11)
      wayland.rs        smithay-client-toolkit: wlr-layer-shell, xdg-output, wl_shm
      x11.rs            x11rb: EWMH dock hints, _NET_WM_STRUT_PARTIAL, RandR, keyboard
    windows/          Win32: layered popup windows (UpdateLayeredWindow), per-monitor DPI v2
      monitors.rs       EnumDisplayMonitors, physical <-> logical coordinates
      wndproc.rs        window procedure: input, keyboard, stacking, display changes
      appbar.rs         reserving screen space as an AppBar (SHAppBarMessage)
    dirs.rs           per-user config and state directories
    autostart.rs      starting at login (XDG autostart entry, or the Run registry key)
    process.rs        where messages go on Windows (parent console or sidebar.log)
  sidebar-sysinfo   per-OS data sources (CPU, memory, GPU, network, battery, time zone)
    linux.rs          /proc, /sys (hwmon, thermal, power_supply), localtime_r
    net_linux.rs      /proc/net, getifaddrs, `iw` for Wi-Fi details
    nvml.rs           NVIDIA GPUs through NVML, loaded at run time (dlopen / LoadLibrary)
    drm_linux.rs      AMD GPUs through amdgpu's sysfs files; names from pci.ids
    disk_linux.rs     /proc/self/mounts, statvfs, /proc/diskstats
    audio.rs          audio thread shared by audio_linux.rs (wpctl, pactl) and
                      audio_windows.rs (Core Audio)
    gpu_windows.rs    other GPUs through DXGI and the "GPU Engine" counters
    com.rs            calling COM interfaces windows-sys leaves out
    windows.rs        NtQuerySystemInformation, PDH, GlobalMemoryStatusEx, power and time zone APIs
    net_windows.rs    IP Helper (GetAdaptersAddresses, GetIfEntry2), WLAN API for Wi-Fi
    traffic.rs        byte counters to rates and session totals (shared)
  sidebar-online    weather (Open-Meteo) and quotes (Yahoo), polled on background threads
  sidebar-app       the `sidebar` binary
    main.rs           arguments, wiring, reload (re-exec)
    runtime.rs        event loop, actions, the settings window's surface
    config_file.rs    config.toml: parsing, and rewriting the widget list
    layout_file.rs    saved widget placements
    settings.rs       the settings window (drawn with the same Scene API)
```

Dependencies go one way only:

```
widgets ──► core ◄── render ◄── platform ◄── app
              ▲                                │
              ├──────────── sysinfo ◄──────────┤
              └──────────── online  ◄──────────┘
```

## Rules for staying platform-agnostic

1. **`sidebar-core` and `sidebar-widgets` never depend on OS or windowing
   crates.** If something is needed there, add an abstraction to
   `sidebar-platform` or a `Source` to `sidebar-sysinfo`.
2. **The `Platform` trait is the only seam to the display.** Surfaces are
   requested as a `SurfaceSpec` (edge and thickness for docks, a rect for
   floating widgets, a size for windows). Frames go out as `Scene` display
   lists, and input comes back as `InputEvent`s.
3. **Check the backend's `Capabilities` before relying on a feature.**
   Wayland clients can't position windows freely, some compositors (GNOME)
   don't reserve space, and X11 without a compositor can't show
   transparency. The app adjusts its settings to match, for example
   `allow_floating = config && caps.free_positioning`, and the settings
   window grays out options that would have no effect.
4. **Pick the backend at run time.** `cfg` only decides whether a backend is
   compiled in at all (`target_os`, and the `wayland` and `x11` features).
5. **Render in software**, so every backend gets identical pixels and only
   has to blit an RGBA buffer. Widgets measure text through the core's
   `TextMeasure` trait; `sidebar-render` implements it with cosmic-text,
   which finds system fonts by itself.
6. **System data goes through `sidebar_core::Source<T>`**, implemented per
   OS in `sidebar-sysinfo` (or `sidebar-online` for network data). Widgets
   never call OS APIs, and tests feed them closures.

## How it runs

`Shell` (core) owns the widgets and their placements, and describes the
surfaces it needs: one per occupied dock, one per floating widget, and a
preview while dragging. `Runtime` (app) creates, reconfigures and destroys
platform surfaces until they match, routes platform events into the shell,
and presents a fresh `Scene` for every surface that changed.

Widgets implement `measure`, `update`, `on_input` and `draw`. `update` runs
every tick (about four times a second); widgets sample their sources on
their own schedule and return whether they need a redraw.

Widgets ask the app to do things (reload, open settings) by sending an
`Action` over a channel. Reloading re-executes the binary with the same
arguments, so a rebuilt binary is picked up as well.

The settings window is a `Panel`: the runtime gives it an ordinary
top-level window (`SurfaceKind::Window`) and forwards its input. It is drawn
with the same `Scene` API as the widgets. Saving rewrites only the
`[[widgets]]` tables and the General settings it shows, with `toml_edit`,
keeping the rest of the file and its comments. A General setting is only
written when it is already in the file or differs from the default.

### Docks that don't fit

A dock is as long as its monitor edge unless the platform reports a
shorter surface (`PlatformEvent::SurfaceResized`): the Windows backend
keeps docks clear of the taskbar, and layer-shell compositors shorten them
for their own panels. The shell then lays the dock out at that size. When
its widgets are longer than the dock, the mouse wheel scrolls it and a thin
bar is drawn along its inner side.

### Dragging

A primary-button press followed by a move of more than a few pixels starts
a drag. Dropping within `snap_threshold` of an edge docks the widget there,
between two docked widgets inserts it, and anywhere else leaves it floating
(or snaps it to the nearest edge if floating isn't allowed). A right-click
cancels.

The core drives this from `InputEvent`s plus a global pointer position that
every backend supplies: X11 reports root coordinates, Windows converts
screen coordinates with the scale of the monitor under the pointer, and
Wayland adds the surface's known position to the surface-local one. During a drag a separate
preview surface follows the pointer. The surface that was pressed doesn't
move or go away until the drop, because it holds the pointer grab, and
compositors disagree about the coordinates they report for a grabbed surface
that moves.

### Stacking

With `always_on_top = false` (the default), X11 surfaces get
`_NET_WM_STATE_BELOW`, Wayland surfaces use the layer-shell `bottom`
layer, and Windows surfaces are held at the bottom of the z-order by
rewriting `WM_WINDOWPOSCHANGING`. Floating widgets on X11 are then managed dock-type windows, because
override-redirect windows can't be stacked under others. The drag preview is
always on top.

### Keyboard input

Only the settings window takes keyboard input. The X11 backend maps key
codes with the core keyboard mapping (Shift and Caps Lock columns), which
covers Latin text. The Windows backend takes text from `WM_CHAR`, so any
keyboard layout and input method works. The Wayland backend has no keyboard support yet (it would
need xkbcommon), so on Wayland the settings button opens the config file in
the default editor instead.

## Saved state

- `config.toml` is the user's file; the app only writes it from the settings
  window.
- `layout.toml` (state directory) maps each widget key (`weather`,
  `weather-2`, ...) to its dock edge and position, or its floating rect.
  Monitors are stored by name, so placements survive restarts and monitor
  rearrangement; a widget on a missing monitor falls back to the primary
  one.
