//! Runs a fetch function on a background thread at an interval.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use sidebar_core::Source;

/// How often the thread looks at the clock while waiting.
const TICK: Duration = Duration::from_secs(60);

/// Calls `fetch` now and then every `refresh` (every `retry` after an
/// error) on a thread named `name`. Waits are measured in wall-clock time,
/// which keeps running while the computer sleeps, so the data is refreshed
/// within a minute of waking up rather than up to `refresh` later.
///
/// The source yields `None` until the first call finishes, then the latest
/// success; errors are only shown until the first success, so a failed
/// refresh keeps the last data. The thread stops when the source is
/// dropped.
pub(crate) fn spawn<T: Clone + Send + 'static>(
    name: &str,
    refresh: Duration,
    retry: Duration,
    mut fetch: impl FnMut() -> Result<T, String> + Send + 'static,
) -> Box<dyn Source<Result<T, String>>> {
    let latest = Arc::new(Mutex::new(None));
    let (stop, stopped) = mpsc::channel::<()>();
    let shared = Arc::clone(&latest);
    let label = name.to_string();
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        loop {
            let result = fetch();
            let wait = if result.is_ok() { refresh } else { retry };
            if let Err(e) = &result {
                eprintln!("{label}: {e}");
            }
            {
                let mut latest = shared.lock().unwrap_or_else(|e| e.into_inner());
                if result.is_ok() || !matches!(*latest, Some(Ok(_))) {
                    *latest = Some(result);
                }
            }
            // Sleep until the next fetch; exit once the source is dropped.
            let fetched = SystemTime::now();
            while !due(fetched, SystemTime::now(), wait) {
                if let Err(RecvTimeoutError::Disconnected) | Ok(()) = stopped.recv_timeout(wait.min(TICK)) {
                    return;
                }
            }
        }
    });
    if let Err(e) = spawned {
        *latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(format!("could not start: {e}")));
    }
    Box::new(Polled { latest, _stop: stop })
}

/// Whether `wait` has passed since `fetched`, by the wall clock. A clock
/// that went backwards (changed by hand or by time sync) counts as due.
fn due(fetched: SystemTime, now: SystemTime, wait: Duration) -> bool {
    now.duration_since(fetched).is_ok_and(|elapsed| elapsed >= wait) || now < fetched
}

struct Polled<T> {
    latest: Arc<Mutex<Option<Result<T, String>>>>,
    /// Dropping this stops the thread.
    _stop: mpsc::Sender<()>,
}

impl<T: Clone> Source<Result<T, String>> for Polled<T> {
    fn sample(&mut self) -> Option<Result<T, String>> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_for<T: Clone>(source: &mut Box<dyn Source<Result<T, String>>>, done: impl Fn(&Result<T, String>) -> bool) -> Result<T, String> {
        let start = Instant::now();
        loop {
            if let Some(r) = source.sample().filter(|r| done(r)) {
                return r;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn waits_by_the_wall_clock() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let min = Duration::from_secs(60);
        assert!(!due(t, t + 14 * min, 15 * min));
        assert!(due(t, t + 15 * min, 15 * min));
        // Woke up after a night's sleep.
        assert!(due(t, t + 600 * min, 15 * min));
        // The clock was set back.
        assert!(due(t, t - min, 15 * min));
    }

    #[test]
    fn keeps_last_success_through_errors() {
        let mut calls = 0;
        let mut source = spawn("test", Duration::from_millis(10), Duration::from_millis(10), move || {
            calls += 1;
            match calls {
                1 => Err("offline".to_string()),
                2 => Ok(calls),
                _ => Err("flaky".to_string()),
            }
        });
        assert_eq!(wait_for(&mut source, |r| r.is_err()), Err("offline".into()));
        assert_eq!(wait_for(&mut source, |r| r.is_ok()), Ok(2));
        thread::sleep(Duration::from_millis(50));
        assert_eq!(source.sample(), Some(Ok(2)));
    }
}
