# Changelog

## 0.1.0 (2026-11-07)

First public release.

**Platforms**

- Linux on X11, including GNOME through XWayland, and on Wayland
  compositors with layer-shell (KDE Plasma, Sway, Hyprland, COSMIC and
  others).
- Windows 10 and 11: docks reserve screen space like the taskbar does (as
  AppBars). Tested with two monitors and display scales of 100%, 125% and
  150%, mixed across monitors.
- Display scaling: per monitor on Windows, fractional on Wayland, and from
  `Xft.dpi` (or `SIDEBAR_SCALE`) on X11.

**Widgets**

- Clock, calendar, CPU (per-core load, P-/E-cores, temperature, RAM and
  swap), GPU, network (with Wi-Fi details), disks, audio volume, weather
  (Open-Meteo), stocks (Yahoo Finance), battery and a text label.
- GPUs: NVIDIA through NVML; AMD on Linux through `amdgpu`; any vendor on
  Windows through the system's GPU counters.
- The audio widget changes the volume with the mouse wheel and mutes with
  a click.

**Using it**

- Docks on any edge of any monitor. Widgets can be dragged between edges
  or left floating, and their placements are saved.
- A dock whose widgets don't fit scrolls with the mouse wheel.
- Settings window to choose, order and configure widgets, with a General
  page for reserving screen space, staying on top, the default edge,
  opacity and starting at login. Settings the desktop can't apply are
  grayed out with the reason.
- Start at login from the settings window or with `--autostart on|off`.
- On Windows there is no console window; messages go to the terminal it
  was started from, or to `sidebar.log`.

**Configuration**

- A TOML file with theme colors, background opacity, dock size,
  `reserve_space` and `always_on_top`. Misspelled options are reported.
- Changes to the file are picked up automatically, as long as the file is
  valid.
- Weather refreshes every 15 minutes, and soon after the computer wakes
  from sleep.
