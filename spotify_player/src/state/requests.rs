//! The client requests the app is handling and the ones that failed recently, so the running
//! app's state can be read from the command line (`get key app`).

use std::collections::{BTreeMap, VecDeque};
use std::time::{Instant, SystemTime};

use serde::Serialize;

const MAX_FAILURES: usize = 20;

#[derive(Debug, Default)]
pub struct RequestTracker {
    next_id: u64,
    in_flight: BTreeMap<u64, (String, Instant)>,
    failures: VecDeque<RequestFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestFailure {
    pub request: String,
    pub error: String,
    /// Seconds since the Unix epoch
    pub at: u64,
}

#[derive(Debug, Serialize)]
pub struct InFlightRequest {
    pub request: String,
    pub running_ms: u128,
}

impl RequestTracker {
    pub fn start(&mut self, request: String) -> u64 {
        self.next_id += 1;
        self.in_flight
            .insert(self.next_id, (request, Instant::now()));
        self.next_id
    }

    pub fn finish(&mut self, id: u64, error: Option<String>) {
        let Some((request, _)) = self.in_flight.remove(&id) else {
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
            .map(|(request, started)| InFlightRequest {
                request: request.clone(),
                running_ms: started.elapsed().as_millis(),
            })
            .collect()
    }

    pub fn failures(&self) -> Vec<RequestFailure> {
        self.failures.iter().cloned().collect()
    }
}
