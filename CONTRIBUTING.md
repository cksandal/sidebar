# Contributing

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Reporting bugs

Please include:

- your distribution, desktop or compositor, and whether it is X11 or
  Wayland (or your Windows version);
- the backend line the sidebar prints at start (`backend: x11 (...)`);
  run `sidebar` from a terminal to see it;
- your `config.toml`, if you have one (remove anything private, such as
  your weather location);
- what you expected and what happened, with a screenshot if it is visual.

## Building and testing

You need Rust 1.88 or newer. No system libraries are required.

```sh
cargo build
cargo test --workspace
cargo run -- --help
```

Run the app without touching your own config or saved layout:

```sh
cargo run -- --config config.example.toml --layout /tmp/layout.toml
```

The headless backend needs no display. It can write every frame to PPM
files, which is the easiest way to check how a widget looks:

```sh
cargo run -- --backend headless --scale 2 --ticks 8 --dump /tmp/frames
```

The settings window opens with `--settings`, and is dumped as a second
surface.

### Windows

The Windows code can be built and tested from Linux by cross-compiling and
running the result in a Windows VM. One-time setup:

- `rustup target add x86_64-pc-windows-gnu` and the `mingw-w64` package
  (the linker is set in `.cargo/config.toml`).
- In the VM: OpenSSH Server, with `cmd.exe` as its default shell, and your
  key in `authorized_keys` (`administrators_authorized_keys` for an admin
  account).
- An SSH host alias for the VM, `sidebar-vm` by default. With VirtualBox
  NAT, forward a local port to the guest's port 22.

Then [`scripts/win.sh`](scripts/win.sh) runs the loop:

```sh
scripts/win.sh test          # run the whole test suite in the VM
scripts/win.sh run           # build, copy and start it on the VM's desktop
scripts/win.sh log           # what it printed
scripts/win.sh stop
scripts/win.sh frames        # headless frames rendered in the VM, as PNG
scripts/win.sh shot          # screenshot of the VM screen (VirtualBox)
scripts/win.sh input 'drag 1560 70 800 500'   # synthetic mouse input
```

Programs started over SSH have no desktop, so `run`, `stop` and `input` go
through scheduled tasks that run in the logged-in user's session. Don't use
`input` while someone is working in the VM.

On Windows itself, `cargo build` and `cargo test --workspace` work as on
Linux.

## Conventions

- **Keep the core portable.** `sidebar-core` and `sidebar-widgets` must not
  depend on OS or windowing crates. System data reaches widgets through
  `Source<T>` (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)).
- **Test widgets without a display.** Widget tests feed sources with
  closures and check the drawn text and rectangles; see the `testing`
  helpers in `sidebar-widgets`. Parsers for `/proc`, `/sys` and web APIs
  are tested against sample text.
- **No secrets or personal data in fixtures.** Use example values such as
  `192.168.1.20`, `wlan0` or `Springfield, Illinois`.
- **New config options** need a default, an entry in
  `config.example.toml` (which is parsed by a test), and a line in the
  README if users will look for them.
- **Network access** only happens in `sidebar-online`, on background
  threads, and only for widgets the user enabled. Say in the README what is
  sent where.
- Match the surrounding code style, and keep `cargo build` free of warnings.

## Adding a widget

1. Add a data type to `sidebar_core::source` if it needs new data.
2. Implement the source per OS in `sidebar-sysinfo` (or `sidebar-online`):
   `linux.rs` and `windows.rs`, with a stub in `unsupported.rs` for other
   systems.
3. Write the widget in `sidebar-widgets`, using `card`, `header` and the
   helpers in `meter.rs` so it matches the others. Handle both vertical
   docks (full layout) and horizontal ones (one compact line).
4. Add a `WidgetSpec` variant in `sidebar-app/src/config_file.rs`, wire it
   in `build_widget` in `main.rs`, and add it to the settings window's
   catalog in `settings.rs`.
5. Document it in `config.example.toml` and the README's widget table.

## Pull requests

Keep pull requests focused, describe what changed and how you tested it,
and include a before/after screenshot for visual changes. By contributing
you agree that your work is dual-licensed under MIT and Apache-2.0, like the
rest of the project.
