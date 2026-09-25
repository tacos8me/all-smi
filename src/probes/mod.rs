// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Host-level probes exported next to the device metrics.
//!
//! Two opt-in probes, both read-only:
//!
//! * [`net`]: cumulative byte counters (and the rate since the previous
//!   cycle) for named network interfaces, e.g. a direct link between two
//!   machines that serve one model together.
//! * [`lock`]: which processes hold a named lock file open, found with
//!   `lsof`, so the lock itself is never touched.
//!
//! `all-smi api --net-iface <IF> --watch-lock <PATH>` enables them; the
//! remote viewer parses the resulting series back into [`HostProbes`].

pub mod lock;
pub mod net;

use std::path::PathBuf;

pub use lock::{LockSample, LockWatcher};
pub use net::{NetInterfaceSample, NetSampler};

/// Everything the probes observed on one host in one collection cycle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostProbes {
    /// Host identifier (the scraped `host:port` on the viewer side).
    pub host_id: String,
    pub hostname: String,
    pub instance: String,
    pub interfaces: Vec<NetInterfaceSample>,
    pub locks: Vec<LockSample>,
}

impl HostProbes {
    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty() && self.locks.is_empty()
    }
}

/// Owns the state the probes carry between cycles (previous counters,
/// when each lock holder was first seen).
pub struct ProbeSampler {
    net: Option<NetSampler>,
    locks: Option<LockWatcher>,
}

impl ProbeSampler {
    /// Returns `None` when no probe was requested, so callers skip the
    /// whole probe path on a stock exporter.
    pub fn new(interfaces: Vec<String>, lock_paths: Vec<PathBuf>) -> Option<Self> {
        if interfaces.is_empty() && lock_paths.is_empty() {
            return None;
        }
        Some(Self {
            net: (!interfaces.is_empty()).then(|| NetSampler::new(interfaces)),
            locks: (!lock_paths.is_empty()).then(|| LockWatcher::new(lock_paths)),
        })
    }

    pub fn sample(&mut self, hostname: &str) -> HostProbes {
        HostProbes {
            host_id: hostname.to_string(),
            hostname: hostname.to_string(),
            instance: hostname.to_string(),
            interfaces: self
                .net
                .as_mut()
                .map(NetSampler::sample)
                .unwrap_or_default(),
            locks: self
                .locks
                .as_mut()
                .map(LockWatcher::sample)
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_is_absent_when_nothing_requested() {
        assert!(ProbeSampler::new(Vec::new(), Vec::new()).is_none());
        assert!(ProbeSampler::new(vec!["lo".to_string()], Vec::new()).is_some());
    }

    #[test]
    fn sample_reports_requested_interface_even_when_missing() {
        let mut sampler =
            ProbeSampler::new(vec!["definitely-not-an-iface0".to_string()], Vec::new()).unwrap();
        let probes = sampler.sample("host-a");
        assert_eq!(probes.hostname, "host-a");
        // A missing interface is skipped rather than reported as zero.
        assert!(probes.interfaces.is_empty());
        assert!(probes.locks.is_empty());
    }
}
