//! Audio through PipeWire's `wpctl`, or PulseAudio's `pactl` (also served
//! by PipeWire's PulseAudio layer), whichever works.

use std::process::Command;

use sidebar_core::source::AudioSample;

use crate::audio::{self, Audio, Backend};

pub fn audio() -> Option<Audio> {
    audio::spawn("audio", || [Tool::Wpctl, Tool::Pactl].into_iter().map(Cli::new).find_map(|mut cli| cli.read().map(|_| cli)))
}

#[derive(Clone, Copy)]
enum Tool {
    Wpctl,
    Pactl,
}

struct Cli {
    tool: Tool,
    device: Option<String>,
    /// Reads since the device name was looked up.
    since_name: u32,
}

/// stdout of a successful command.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

const SINK_WP: &str = "@DEFAULT_AUDIO_SINK@";
const SINK_PA: &str = "@DEFAULT_SINK@";

impl Cli {
    fn new(tool: Tool) -> Self {
        Self { tool, device: None, since_name: u32::MAX }
    }

    fn name(&self) -> Option<String> {
        match self.tool {
            Tool::Wpctl => run("wpctl", &["inspect", SINK_WP]).and_then(|t| wpctl_description(&t)),
            Tool::Pactl => None,
        }
    }
}

impl Backend for Cli {
    fn read(&mut self) -> Option<AudioSample> {
        let (volume, muted) = match self.tool {
            Tool::Wpctl => parse_wpctl(&run("wpctl", &["get-volume", SINK_WP])?)?,
            Tool::Pactl => (
                parse_pactl_volume(&run("pactl", &["get-sink-volume", SINK_PA])?)?,
                run("pactl", &["get-sink-mute", SINK_PA])?.contains("yes"),
            ),
        };
        // The output can change (headphones); look its name up now and then.
        self.since_name = self.since_name.saturating_add(1);
        if self.since_name > 10 {
            self.since_name = 0;
            self.device = self.name();
        }
        Some(AudioSample { volume, muted, device: self.device.clone() })
    }

    fn set_volume(&mut self, volume: f32) {
        let _ = match self.tool {
            Tool::Wpctl => run("wpctl", &["set-volume", SINK_WP, &format!("{volume:.2}")]),
            Tool::Pactl => run("pactl", &["set-sink-volume", SINK_PA, &format!("{:.0}%", volume * 100.0)]),
        };
    }

    fn set_muted(&mut self, muted: bool) {
        let flag = if muted { "1" } else { "0" };
        let _ = match self.tool {
            Tool::Wpctl => run("wpctl", &["set-mute", SINK_WP, flag]),
            Tool::Pactl => run("pactl", &["set-sink-mute", SINK_PA, flag]),
        };
    }
}

/// "Volume: 0.48 [MUTED]"
fn parse_wpctl(text: &str) -> Option<(f32, bool)> {
    let rest = text.trim().strip_prefix("Volume:")?.trim();
    let volume = rest.split_whitespace().next()?.parse().ok()?;
    Some((volume, rest.contains("[MUTED]")))
}

/// The first channel's "NN%" in "Volume: front-left: 26214 /  40% / -23.88 dB, ...".
fn parse_pactl_volume(text: &str) -> Option<f32> {
    let percent = text.split_whitespace().find_map(|w| w.strip_suffix('%'))?;
    Some(percent.parse::<f32>().ok()? / 100.0)
}

/// `node.description = "Built-in Audio Analog Stereo"` from `wpctl inspect`.
fn wpctl_description(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let value = l.trim_start_matches([' ', '*']).strip_prefix("node.description = ")?;
        Some(value.trim().trim_matches('"').to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_output() {
        assert_eq!(parse_wpctl("Volume: 0.48 [MUTED]\n"), Some((0.48, true)));
        assert_eq!(parse_wpctl("Volume: 1.00\n"), Some((1.0, false)));
        assert_eq!(parse_wpctl("nonsense"), None);
        let pa = "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 26214 /  40% / -23.88 dB\n        balance 0.00\n";
        assert_eq!(parse_pactl_volume(pa), Some(0.4));
        let inspect = "id 52, type PipeWire:Interface:Node\n    alsa.card = \"0\"\n  * node.description = \"Built-in Audio Analog Stereo\"\n  * node.name = \"alsa_output.pci\"\n";
        assert_eq!(wpctl_description(inspect).as_deref(), Some("Built-in Audio Analog Stereo"));
    }
}
