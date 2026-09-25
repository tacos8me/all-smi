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
//! Three opt-in probes, all read-only:
//!
//! * [`net`]: cumulative byte counters (and the rate since the previous
//!   cycle) for named network interfaces, e.g. a direct link between two
//!   machines that serve one model together.
//! * [`lock`]: which processes hold a named lock file open, found with
//!   `lsof`, so the lock itself is never touched.
//! * [`json`]: numeric fields (and their rates) of a loopback JSON status
//!   endpoint, e.g. a model server's `/health`.
//!
//! `all-smi api --net-iface <IF> --watch-lock <PATH> --json-probe NAME=URL`
//! enables them; the remote viewer parses the resulting series back into
//! [`HostProbes`].

pub mod json;
pub mod lock;
pub mod net;

use std::path::PathBuf;

pub use json::{JsonProbeSample, JsonProbeSpec, JsonProber};
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
    pub json: Vec<JsonProbeSample>,
    /// Exporter OS from `all_smi_build_info` (viewer side only).
    pub os: Option<String>,
}

impl HostProbes {
    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty() && self.locks.is_empty() && self.json.is_empty()
    }

    /// The JSON probe exported under `name`, if any.
    pub fn json_probe(&self, name: &str) -> Option<&JsonProbeSample> {
        self.json.iter().find(|p| p.name == name)
    }
}

/// Owns the state the probes carry between cycles (previous counters,
/// when each lock holder was first seen).
pub struct ProbeSampler {
    net: Option<NetSampler>,
    locks: Option<LockWatcher>,
    json: Option<JsonProber>,
}

impl ProbeSampler {
    /// Returns `None` when no probe was requested, so callers skip the
    /// whole probe path on a stock exporter.
    pub fn new(
        interfaces: Vec<String>,
        lock_paths: Vec<PathBuf>,
        json_probes: Vec<JsonProbeSpec>,
    ) -> Option<Self> {
        if interfaces.is_empty() && lock_paths.is_empty() && json_probes.is_empty() {
            return None;
        }
        Some(Self {
            net: (!interfaces.is_empty()).then(|| NetSampler::new(interfaces)),
            locks: (!lock_paths.is_empty()).then(|| LockWatcher::new(lock_paths)),
            json: (!json_probes.is_empty()).then(|| JsonProber::new(json_probes)),
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
            json: self
                .json
                .as_mut()
                .map(JsonProber::sample)
                .unwrap_or_default(),
            os: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampler_is_absent_when_nothing_requested() {
        assert!(ProbeSampler::new(Vec::new(), Vec::new(), Vec::new()).is_none());
        assert!(ProbeSampler::new(vec!["lo".to_string()], Vec::new(), Vec::new()).is_some());
    }

    #[test]
    fn sample_reports_requested_interface_even_when_missing() {
        let mut sampler = ProbeSampler::new(
            vec!["definitely-not-an-iface0".to_string()],
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let probes = sampler.sample("host-a");
        assert_eq!(probes.hostname, "host-a");
        // A missing interface is skipped rather than reported as zero.
        assert!(probes.interfaces.is_empty());
        assert!(probes.locks.is_empty());
    }
}
