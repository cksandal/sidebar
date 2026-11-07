//! Sidebar entry point. Contains no OS-specific code: the backend is chosen
//! at runtime through `sidebar_platform`, and system data comes from
//! `sidebar_sysinfo`.

// A GUI program on Windows, so no console window opens with it; see
// `sidebar_platform::process` for where its messages go instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config_file;
mod layout_file;
mod runtime;
mod settings;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};

use sidebar_core::{Edge, SavedSpot, Shell, Widget, WidgetId};
use sidebar_platform::headless::HeadlessPlatform;
use sidebar_platform::{Capabilities, Platform, PlatformError};
use sidebar_widgets::{BatteryMeter, Calendar, Clock, CpuGraph, GpuGraph, Label, NetGraph, StockTicker, WeatherCard};

use crate::config_file::{ConfigFile, WidgetSpec};
use crate::runtime::{Action, Exit, Panel, Runtime};

const USAGE: &str = "\
usage: sidebar [options]

  --backend <name>   auto | headless | wayland | x11 | windows   (default: auto)
  --edge <edge>      left | right | top | bottom   (default edge for new widgets)
  --config <file>    settings file   (default: <config dir>/sidebar/config.toml)
  --layout <file>    saved widget placements   (default: <state dir>/sidebar/layout.toml)
  --ticks <n>        exit after n event-loop iterations
  --dump <dir>       headless only: write frames as PPM files to <dir>
  --scale <n>        headless only: render at n× (e.g. 2 for screenshots)
  --settings         open the settings window at start
  --autostart on|off
                     start the sidebar when you log in, or stop doing so
  -h, --help         show this help

Drag a widget with the primary button to move it; drop it near a screen
edge to dock it there, elsewhere to let it float. Right-click cancels.";

struct Args {
    backend: Option<String>,
    edge: Option<Edge>,
    config: Option<PathBuf>,
    layout: Option<PathBuf>,
    ticks: Option<u64>,
    dump: Option<PathBuf>,
    scale: Option<f32>,
    settings: bool,
    autostart: Option<bool>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { backend: None, edge: None, config: None, layout: None, ticks: None, dump: None, scale: None, settings: false, autostart: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--backend" => args.backend = Some(value()?),
            "--edge" => args.edge = Some(value()?.parse()?),
            "--config" => args.config = Some(value()?.into()),
            "--layout" => args.layout = Some(value()?.into()),
            "--ticks" => args.ticks = Some(value()?.parse().map_err(|e| format!("--ticks: {e}"))?),
            "--dump" => args.dump = Some(value()?.into()),
            "--settings" => args.settings = true,
            "--autostart" => {
                args.autostart = Some(match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    other => return Err(format!("--autostart: expected on or off, not '{other}'")),
                })
            }
            "--scale" => args.scale = Some(value()?.parse().map_err(|e| format!("--scale: {e}"))?),
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument '{other}'\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn headless(args: &Args) -> Box<dyn Platform> {
    let platform = HeadlessPlatform::new().with_scale(args.scale.unwrap_or(1.0));
    Box::new(match &args.dump {
        Some(dir) => platform.with_dump_dir(dir),
        None => platform,
    })
}

fn open_platform(name: &str, args: &Args) -> Result<Box<dyn Platform>, PlatformError> {
    match name {
        "headless" => Ok(headless(args)),
        "auto" => match sidebar_platform::auto() {
            Ok(platform) => Ok(platform),
            Err(e @ (PlatformError::NotImplemented(_) | PlatformError::Unsupported(_))) => {
                eprintln!("warning: {e}; falling back to headless backend");
                Ok(headless(args))
            }
            Err(e) => Err(e),
        },
        name => sidebar_platform::create(name),
    }
}

/// Creates the widget for `spec`, or `None` if it cannot work here (no
/// battery, no such GPU).
fn build_widget(spec: &WidgetSpec, actions: &Sender<Action>) -> Option<Box<dyn Widget>> {
    let send = |action| {
        let tx = actions.clone();
        Box::new(move || {
            let _ = tx.send(action);
        })
    };
    Some(match spec {
        WidgetSpec::Label { text } => Box::new(Label::new(text.clone())),
        WidgetSpec::Clock => {
            Box::new(Clock::new(sidebar_sysinfo::utc_offset()).with_buttons(send(Action::Reload), send(Action::OpenSettings)))
        }
        WidgetSpec::Calendar { week_start } => {
            Box::new(Calendar::new(sidebar_sysinfo::utc_offset(), *week_start == config_file::WeekStart::Monday))
        }
        WidgetSpec::Cpu => Box::new(CpuGraph::new(sidebar_sysinfo::cpu(), sidebar_sysinfo::cpu_info()).with_memory(sidebar_sysinfo::memory())),
        WidgetSpec::Gpu { index } => {
            let gpu = sidebar_sysinfo::gpus().into_iter().nth(*index)?;
            Box::new(GpuGraph::new(gpu.source, gpu.info))
        }
        WidgetSpec::Network { interface } => Box::new(NetGraph::new(sidebar_sysinfo::network(interface.clone())?)),
        WidgetSpec::Weather { location, units } => {
            let fahrenheit = *units == config_file::TemperatureUnit::Fahrenheit;
            let location = sidebar_online::Location::parse(location);
            Box::new(WeatherCard::new(sidebar_online::open_meteo(location, fahrenheit)))
        }
        WidgetSpec::Stocks { symbols } => {
            let symbols: Vec<String> = symbols.iter().map(|s| s.trim().to_uppercase()).collect();
            Box::new(StockTicker::new(symbols.clone(), sidebar_online::yahoo_quotes(symbols)))
        }
        WidgetSpec::Battery => Box::new(BatteryMeter::new(sidebar_sysinfo::battery()?)),
    })
}

/// Adds the configured widgets at their saved placements. Docked widgets
/// are added in saved dock order; widgets without a saved placement go to
/// the end of the default dock. Returns each widget's layout key and id.
fn add_widgets(
    shell: &mut Shell,
    specs: &[WidgetSpec],
    saved: &layout_file::Layout,
    actions: &Sender<Action>,
) -> Vec<(String, WidgetId)> {
    let mut widgets = Vec::new();
    for (key, spec) in config_file::widget_keys(specs).into_iter().zip(specs) {
        match build_widget(spec, actions) {
            Some(widget) => widgets.push((key, widget)),
            None => eprintln!("note: skipping {key} widget (not available on this system)"),
        }
    }
    let dock_index = |key: &String| match saved.get(key).map(|p| &p.spot) {
        Some(SavedSpot::Docked { index, .. }) => *index,
        _ => usize::MAX,
    };
    widgets.sort_by_key(|(key, _)| dock_index(key));
    widgets
        .into_iter()
        .map(|(key, widget)| {
            let placement = saved.get(&key).and_then(|p| shell.restore_placement(p));
            let id = shell.add_widget(widget, placement);
            (key, id)
        })
        .collect()
}

fn default_path(dir: Option<PathBuf>, file: &str) -> Option<PathBuf> {
    dir.map(|d| d.join(file))
}

/// Builds the settings panel from the config file as it is now; saving
/// writes it back. Without a config location, saving fails with a message.
/// The General settings that `caps` say would have no effect, with why.
fn unavailable(caps: Capabilities) -> settings::Unavailable {
    settings::Unavailable {
        reserve_space: (!caps.reserve_space).then_some("Not available: ignored for X11 apps under Wayland."),
        opacity: (!caps.transparency).then_some("Not available: needs a compositor, and none is running."),
    }
}

fn settings_panel(
    path: Option<PathBuf>,
    startup: &[WidgetSpec],
    theme: sidebar_core::Theme,
    unavailable: settings::Unavailable,
    actions: Sender<Action>,
) -> Box<dyn Panel> {
    let file = path.as_deref().and_then(|p| ConfigFile::load(p, false).ok());
    let specs = file.as_ref().map_or_else(|| startup.to_vec(), ConfigFile::widgets);
    let mut general = file.as_ref().map(ConfigFile::general).unwrap_or_default();
    general.start_at_login = sidebar_platform::autostart::enabled();
    let starts_at_login = general.start_at_login;
    let save: settings::SaveFn = Box::new(move |general, specs| {
        match &path {
            Some(path) => config_file::save_settings(path, Some(general), specs)?,
            None => return Err("no config directory on this system".into()),
        }
        if general.start_at_login != starts_at_login {
            sidebar_platform::autostart::set(general.start_at_login)?;
        }
        Ok(())
    });
    Box::new(settings::Settings::new(general, &specs, theme, Box::new(sidebar_render::SystemText), actions, save).with_unavailable(unavailable))
}

/// When the backend has no settings window (Wayland for now): open the
/// config file in the desktop's editor instead, creating it if needed.
fn edit_config_file(path: Option<&Path>, specs: &[WidgetSpec], why: &str) {
    eprintln!("note: settings window unavailable ({why}); opening the config file instead");
    let Some(path) = path else { return };
    if !path.exists()
        && let Err(e) = config_file::save_settings(path, None, specs)
    {
        eprintln!("warning: could not create {}: {e}", path.display());
        return;
    }
    let opener = if cfg!(windows) { "explorer" } else if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    if let Err(e) = std::process::Command::new(opener).arg(path).spawn() {
        eprintln!("warning: could not run {opener}: {e}");
    }
}

/// Replaces this process with a fresh copy started with the same
/// arguments (minus `--settings`), so the config is read again and a
/// rebuilt binary is picked up. Only returns on failure.
fn restart() -> String {
    let args: Vec<_> = std::env::args_os().skip(1).filter(|a| a != "--settings").collect();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return format!("cannot find own executable: {e}"),
    };
    // Linux reports "<path> (deleted)" once the binary has been rebuilt.
    let exe = match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(path) if !exe.exists() => PathBuf::from(path),
        _ => exe,
    };
    eprintln!("reloading: {}", exe.display());
    let mut command = std::process::Command::new(&exe);
    command.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = command.exec();
        format!("cannot restart {}: {e}", exe.display())
    }
    #[cfg(not(unix))]
    match command.spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => format!("cannot restart {}: {e}", exe.display()),
    }
}

fn run() -> Result<Exit, String> {
    let args = parse_args()?;
    if let Some(on) = args.autostart {
        sidebar_platform::autostart::set(on)?;
        println!("{}", if on { "The sidebar will start when you log in." } else { "The sidebar won't start at login any more." });
        return Ok(Exit::Quit);
    }

    let config_path = args.config.clone().or_else(|| default_path(sidebar_platform::dirs::config_dir(), "config.toml"));
    let file = match &config_path {
        Some(path) => ConfigFile::load(path, args.config.is_some())?,
        None => ConfigFile::default(),
    };
    let mut config = file.to_config()?;

    let backend = args.backend.clone().or(file.backend.clone()).unwrap_or_else(|| "auto".into());
    let platform = open_platform(&backend, &args).map_err(|e| e.to_string())?;

    let caps = platform.capabilities();
    if let Some(edge) = args.edge {
        config.default_edge = edge;
    }
    config.reserve_space &= caps.reserve_space;
    config.allow_floating &= caps.free_positioning;
    eprintln!("backend: {} ({caps:?})", platform.name());

    let mut shell = Shell::new(config);
    shell.set_text_measure(Box::new(sidebar_render::SystemText));
    shell.set_monitors(platform.monitors());

    let layout_path = args.layout.clone().or_else(|| default_path(sidebar_platform::dirs::state_dir(), "layout.toml"));
    let mut saved = layout_path.as_deref().map(layout_file::load).unwrap_or_default();
    let specs = file.widgets();
    let (actions, received) = mpsc::channel();
    let ids = add_widgets(&mut shell, &specs, &saved, &actions);
    if args.settings {
        let _ = actions.send(Action::OpenSettings);
    }

    let theme = shell.config().theme.clone();
    let fallback_path = config_path.clone();
    let fallback_specs = specs.clone();
    let mut runtime = Runtime::new(platform, shell).with_actions(received).with_settings(
        Box::new(move || settings_panel(config_path.clone(), &specs, theme.clone(), unavailable(caps), actions.clone())),
        Box::new(move |why| edit_config_file(fallback_path.as_deref(), &fallback_specs, why)),
    );
    if let Some(path) = layout_path {
        runtime = runtime.on_layout_changed(Box::new(move |shell: &Shell| {
            for (key, id) in &ids {
                if let Some(p) = shell.saved_placement(*id) {
                    saved.insert(key.clone(), p);
                }
            }
            if let Err(e) = layout_file::save(&path, &saved) {
                eprintln!("warning: could not save layout: {e}");
            }
        }));
    }
    runtime.run(args.ticks).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    sidebar_platform::process::init_output();
    match run() {
        Ok(Exit::Quit) => ExitCode::SUCCESS,
        // Everything from the old run (windows, connection) is gone by now.
        Ok(Exit::Reload) => {
            eprintln!("error: {}", restart());
            ExitCode::FAILURE
        }
        Err(msg) if msg == USAGE => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}
