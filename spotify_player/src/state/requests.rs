//! The client requests the app is handling and the ones that failed recently, so the running
//! app's state can be read from the command line (`get key app`).

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

const MAX_FAILURES: usize = 20;

#[derive(Debug, Default)]
pub struct RequestTracker {
    next_id: u64,
    in_flight: BTreeMap<u64, InFlight>,
    failures: VecDeque<RequestFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestFailure {
    pub request: String,
    pub error: String,
    /// Seconds since the Unix epoch
    pub at: u64,
}

#[derive(Debug)]
struct InFlight {
    request: String,
    /// What it does, for one a person asked for
    action: Option<&'static str>,
    started: Instant,
}

#[derive(Debug, Serialize)]
pub struct InFlightRequest {
    pub request: String,
    pub running_ms: u128,
}

impl RequestTracker {
    pub fn start(&mut self, request: String, action: Option<&'static str>) -> u64 {
        self.next_id += 1;
        self.in_flight.insert(
            self.next_id,
            InFlight {
                request,
                action,
                started: Instant::now(),
            },
        );
        self.next_id
    }

    pub fn finish(&mut self, id: u64, error: Option<String>) {
        let Some(InFlight { request, .. }) = self.in_flight.remove(&id) else {
            return;
        };
        if let Some(error) = error {
            let at = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            self.failures
                .push_back(RequestFailure { request, error, at });
            if self.failures.len() > MAX_FAILURES {
                self.failures.pop_front();
            }
        }
    }

    pub fn in_flight(&self) -> Vec<InFlightRequest> {
        self.in_flight
            .values()
            .map(|in_flight| InFlightRequest {
                request: in_flight.request.clone(),
                running_ms: in_flight.started.elapsed().as_millis(),
            })
            .collect()
    }

    /// The longest-running request a person asked for, once it has run for `after`
    #[cfg_attr(not(feature = "tui"), allow(dead_code))]
    pub fn slowest_action(&self, after: Duration) -> Option<(&'static str, Duration)> {
        self.in_flight
            .values()
            .filter_map(|in_flight| Some((in_flight.action?, in_flight.started.elapsed())))
            .filter(|(_, running)| *running >= after)
            .max_by_key(|(_, running)| *running)
    }

    pub fn failures(&self) -> Vec<RequestFailure> {
        self.failures.iter().cloned().collect()
    }
}
