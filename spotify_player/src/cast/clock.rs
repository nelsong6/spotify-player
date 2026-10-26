//! This PC's clock and its offset from the server's, measured the way snapclient does: half the
//! difference of the two one-way latencies of a time request, taken as the median of the last
//! 200 so that a delayed packet doesn't move it.

use super::protocol::Micros;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SAMPLES: usize = 200;
/// Samples older than this describe a connection that was lost; they are thrown away
const STALE_AFTER: Duration = Duration::from_mins(1);

/// Microseconds on this PC's monotonic clock; only differences between its values mean anything
pub fn now() -> Micros {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as Micros
}

#[derive(Default)]
pub struct Clock {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    diffs: VecDeque<Micros>,
    median: Micros,
    last: Option<Instant>,
}

impl Clock {
    /// Take in a time response: `c2s` is the latency the server measured, `s2c` the one measured
    /// here (this PC's receive time minus the server's send time)
    pub fn add(&self, c2s: Micros, s2c: Micros) {
        let mut inner = self.inner.lock();
        if inner.last.is_some_and(|t| t.elapsed() > STALE_AFTER) {
            inner.diffs.clear();
        }
        inner.last = Some(Instant::now());
        if inner.diffs.len() == SAMPLES {
            inner.diffs.pop_front();
        }
        inner.diffs.push_back((c2s - s2c) / 2);
        let mut sorted: Vec<_> = inner.diffs.iter().copied().collect();
        sorted.sort_unstable();
        inner.median = sorted[sorted.len() / 2];
    }

    /// Whether any time response has come in
    pub fn is_synced(&self) -> bool {
        !self.inner.lock().diffs.is_empty()
    }

    /// The server's clock now
    pub fn server_now(&self) -> Micros {
        now() + self.inner.lock().median
    }

    pub fn reset(&self) {
        *self.inner.lock() = Inner::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_the_offset_from_symmetric_latency() {
        let clock = Clock::default();
        // the server is 5 s ahead and each direction takes 2 ms
        let offset = 5_000_000;
        clock.add(offset + 2_000, -offset + 2_000);
        let diff = clock.server_now() - now();
        assert!((diff - offset).abs() < 1_000, "{diff}");
    }

    #[test]
    fn ignores_a_few_delayed_responses() {
        let clock = Clock::default();
        let offset = -3_000_000;
        for i in 0..50 {
            // every fifth response sat 40 ms in a queue on the way back
            let back = if i % 5 == 0 { 40_000 } else { 1_000 };
            clock.add(offset + 1_000, -offset + back);
        }
        let diff = clock.server_now() - now();
        assert!((diff - offset).abs() < 1_000, "{diff}");
    }
}
