//! Turns an interface's byte counters into rates and session totals. Shared
//! by the per-OS network sources.

use std::time::Instant;

#[derive(Default)]
pub struct Traffic {
    /// Interface, (rx, tx) counters, and time of the previous sample.
    prev: Option<(String, (u64, u64), Instant)>,
    /// Bytes (down, up) since the source was created.
    session: (u64, u64),
}

/// What [`Traffic::update`] works out from one reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// Bytes per second (down, up) since the previous reading; `None` on
    /// the first reading and after the interface changed.
    pub down: Option<f64>,
    pub up: Option<f64>,
    /// Bytes (down, up) since the source was created.
    pub session: (u64, u64),
}

impl Traffic {
    pub fn update(&mut self, interface: &str, counters: (u64, u64)) -> Rates {
        self.update_at(interface, counters, Instant::now())
    }

    fn update_at(&mut self, interface: &str, counters: (u64, u64), now: Instant) -> Rates {
        let (mut down, mut up) = (None, None);
        if let Some((prev_interface, prev, at)) = self.prev.replace((interface.to_string(), counters, now))
            && prev_interface == interface
        {
            let (d, u) = (delta(prev.0, counters.0), delta(prev.1, counters.1));
            self.session.0 += d;
            self.session.1 += u;
            let secs = now.duration_since(at).as_secs_f64();
            if secs > 0.0 {
                (down, up) = (Some(d as f64 / secs), Some(u as f64 / secs));
            }
        }
        Rates { down, up, session: self.session }
    }
}

/// Bytes between two counter readings; a counter that went backwards was
/// reset (interface re-created), so everything since counts.
fn delta(prev: u64, now: u64) -> u64 {
    if now >= prev { now - prev } else { now }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn computes_rates_and_session() {
        let mut t = Traffic::default();
        let start = Instant::now();
        let first = t.update_at("eth0", (1000, 500), start);
        assert_eq!(first, Rates { down: None, up: None, session: (0, 0) });
        let second = t.update_at("eth0", (3000, 1500), start + Duration::from_secs(2));
        assert_eq!(second, Rates { down: Some(1000.0), up: Some(500.0), session: (2000, 1000) });
        // A reset counter counts from zero.
        let reset = t.update_at("eth0", (100, 0), start + Duration::from_secs(3));
        assert_eq!(reset.session, (2100, 1000));
        // Another interface starts over for rates, but keeps the session.
        let other = t.update_at("wlan0", (9, 9), start + Duration::from_secs(4));
        assert_eq!(other, Rates { down: None, up: None, session: (2100, 1000) });
    }
}
