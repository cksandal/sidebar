# Changelog

## 0.1.0 (unreleased)

First public release.

- Docks on any screen edge of any monitor; widgets can be dragged between
  edges or left floating, and placements are saved.
- Backends: X11 (including GNOME through XWayland), Wayland (layer-shell)
  and Windows (layered windows; docks reserve space as AppBars); a headless
  backend for tests.
- Data sources for Linux and Windows: CPU (with P-/E-cores), memory,
  NVIDIA GPU, network with Wi-Fi details, battery, time zone.
- Widgets: clock, calendar, CPU (per-core, P-/E-cores, temperature, RAM and
  swap), NVIDIA GPU, network, weather (Open-Meteo), stocks (Yahoo Finance),
  battery, label.
- Settings window (X11 and Windows) to choose, order and configure widgets,
  with a General page for reserving screen space, staying on top, the
  default edge and opacity. Settings the desktop can't apply are grayed
  out with the reason. Reload button.
- TOML configuration with theme colors, background `opacity`, dock size,
  `reserve_space` and `always_on_top`.
- Weather refreshes every 15 minutes, and soon after the computer wakes
  from sleep.
- Start at login, from the settings window or `--autostart on|off`.
- Disk widget (space per filesystem or drive, read and write rates) and
  audio widget (output volume: scroll to change, click to mute).
- GPUs beyond NVIDIA: AMD on Linux through `amdgpu`; any vendor on Windows
  through the GPU performance counters. Rows a driver doesn't report are
  left out.
- The config file is reloaded automatically when it changes and is
  valid.
- Docks whose widgets don't fit scroll with the mouse wheel, and stay
  clear of the Windows taskbar.
- Windows: no console window; messages go to the terminal it was started
  from, or to `sidebar.log`. Tested with two monitors and display scales
  of 100%, 125% and 150%, mixed across monitors.
