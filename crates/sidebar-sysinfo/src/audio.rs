//! The default audio output, read and changed on a background thread (the
//! system calls can block, and on Windows the COM objects belong to the
//! thread that made them).

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sidebar_core::Source;
use sidebar_core::source::{AudioControl, AudioSample};

/// Volume and mute of the default output, and a way to change them.
pub struct Audio {
    pub source: Box<dyn Source<AudioSample>>,
    pub control: Box<dyn AudioControl + Send>,
}

/// One way of talking to the system's audio.
pub(crate) trait Backend {
    fn read(&mut self) -> Option<AudioSample>;
    fn set_volume(&mut self, volume: f32);
    fn set_muted(&mut self, muted: bool);
}

enum Command {
    Volume(f32),
    Muted(bool),
}

type Latest = Arc<Mutex<Option<AudioSample>>>;

fn lock(latest: &Latest) -> std::sync::MutexGuard<'_, Option<AudioSample>> {
    latest.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the thread with the backend `make` creates there; `None` if it
/// can't read the output.
pub(crate) fn spawn<B: Backend>(name: &str, make: impl FnOnce() -> Option<B> + Send + 'static) -> Option<Audio> {
    let latest: Latest = Arc::default();
    let (commands, received) = mpsc::channel();
    let (ready, started) = mpsc::channel();
    let shared = Arc::clone(&latest);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let Some(mut backend) = make() else { return };
            let first = backend.read();
            let ok = first.is_some();
            *lock(&shared) = first;
            let _ = ready.send(ok);
            if !ok {
                return;
            }
            loop {
                match received.recv_timeout(Duration::from_secs(1)) {
                    Ok(command) => {
                        // Apply only the newest of a burst (a fast wheel).
                        let (mut volume, mut muted) = (None, None);
                        for command in std::iter::once(command).chain(received.try_iter()) {
                            match command {
                                Command::Volume(v) => volume = Some(v),
                                Command::Muted(m) => muted = Some(m),
                            }
                        }
                        if let Some(v) = volume {
                            backend.set_volume(v);
                        }
                        if let Some(m) = muted {
                            backend.set_muted(m);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if let Some(sample) = backend.read() {
                    *lock(&shared) = Some(sample);
                }
            }
        })
        .ok()?;
    if !started.recv().unwrap_or(false) {
        return None;
    }
    let source = Shared(Arc::clone(&latest));
    Some(Audio { source: Box::new(source), control: Box::new(Control { commands: Mutex::new(commands), latest }) })
}

struct Shared(Latest);

impl Source<AudioSample> for Shared {
    fn sample(&mut self) -> Option<AudioSample> {
        lock(&self.0).clone()
    }
}

/// Sends changes to the thread, and shows them right away.
struct Control {
    commands: Mutex<Sender<Command>>,
    latest: Latest,
}

impl Control {
    fn send(&self, command: Command) {
        if let Some(s) = lock(&self.latest).as_mut() {
            match command {
                Command::Volume(v) => s.volume = v,
                Command::Muted(m) => s.muted = m,
            }
        }
        let _ = self.commands.lock().unwrap_or_else(|e| e.into_inner()).send(command);
    }
}

impl AudioControl for Control {
    fn set_volume(&self, volume: f32) {
        self.send(Command::Volume(volume.clamp(0.0, 1.0)));
    }

    fn set_muted(&self, muted: bool) {
        self.send(Command::Muted(muted));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Arc<Mutex<AudioSample>>);

    impl Backend for Fake {
        fn read(&mut self) -> Option<AudioSample> {
            Some(self.0.lock().unwrap().clone())
        }
        fn set_volume(&mut self, volume: f32) {
            self.0.lock().unwrap().volume = volume;
        }
        fn set_muted(&mut self, muted: bool) {
            self.0.lock().unwrap().muted = muted;
        }
    }

    #[test]
    fn applies_changes_on_the_thread() {
        let system = Arc::new(Mutex::new(AudioSample { volume: 0.5, muted: false, device: Some("Speakers".into()) }));
        let shared = Arc::clone(&system);
        let mut audio = spawn("test-audio", move || Some(Fake(shared))).expect("started");
        assert_eq!(audio.source.sample().map(|s| s.volume), Some(0.5));
        audio.control.set_volume(1.5);
        audio.control.set_muted(true);
        // Shown at once, before the thread has applied it.
        assert_eq!(audio.source.sample().map(|s| (s.volume, s.muted)), Some((1.0, true)));
        for _ in 0..200 {
            if system.lock().unwrap().muted {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let now = system.lock().unwrap().clone();
        assert_eq!((now.volume, now.muted), (1.0, true));
        assert!(spawn::<Fake>("test-none", || None).is_none());
    }
}
