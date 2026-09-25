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

//! Network interface byte counters.
//!
//! Counters come from `sysinfo`, which reads `/sys/class/net/*/statistics`
//! on Linux and the 64-bit `NET_RT_IFLIST2` interface table on macOS, so
//! neither platform wraps at 4 GiB. The rate is computed here, between two
//! of the exporter's own samples, rather than by the viewer: a viewer that
//! scrapes on a different cadence than the exporter collects would
//! otherwise see alternating zero and double-sized deltas.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sysinfo::Networks;

/// One interface's cumulative counters and the rate since the previous
/// sample.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetInterfaceSample {
    pub interface: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Bytes per second since the previous sample. `None` on the first
    /// sample and after a counter reset.
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
}

struct Previous {
    rx: u64,
    tx: u64,
    at: Instant,
}

pub struct NetSampler {
    interfaces: Vec<String>,
    networks: Networks,
    previous: HashMap<String, Previous>,
}

impl NetSampler {
    pub fn new(interfaces: Vec<String>) -> Self {
        Self {
            interfaces,
            networks: Networks::new_with_refreshed_list(),
            previous: HashMap::new(),
        }
    }

    /// Sample every requested interface that currently exists. An
    /// interface that is missing (unplugged, renamed) is skipped rather
    /// than reported as zero traffic.
    pub fn sample(&mut self) -> Vec<NetInterfaceSample> {
        self.networks.refresh(true);
        let now = Instant::now();
        let mut out = Vec::with_capacity(self.interfaces.len());
        for name in &self.interfaces {
            let Some(data) = self.networks.get(name) else {
                self.previous.remove(name);
                continue;
            };
            let rx = data.total_received();
            let tx = data.total_transmitted();
            let (rx_rate, tx_rate) = match self.previous.get(name) {
                Some(prev) => {
                    let dt = now.duration_since(prev.at);
                    (rate(prev.rx, rx, dt), rate(prev.tx, tx, dt))
                }
                None => (None, None),
            };
            self.previous
                .insert(name.clone(), Previous { rx, tx, at: now });
            out.push(NetInterfaceSample {
                interface: name.clone(),
                rx_bytes: rx,
                tx_bytes: tx,
                rx_bytes_per_sec: rx_rate,
                tx_bytes_per_sec: tx_rate,
            });
        }
        out
    }
}

/// Bytes per second between two cumulative readings. `None` when no time
/// elapsed or the counter went backwards (interface reset), because either
/// would render as a nonsense spike.
pub(crate) fn rate(previous: u64, current: u64, elapsed: Duration) -> Option<f64> {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 || current < previous {
        return None;
    }
    Some((current - previous) as f64 / secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_divides_delta_by_elapsed_time() {
        let r = rate(1_000, 4_000, Duration::from_millis(1_500)).unwrap();
        assert!((r - 2_000.0).abs() < 1e-9);
    }

    #[test]
    fn rate_is_absent_on_reset_or_zero_interval() {
        assert_eq!(rate(5_000, 10, Duration::from_secs(1)), None);
        assert_eq!(rate(1, 2, Duration::ZERO), None);
    }

    #[test]
    fn loopback_counters_are_sampled_with_a_rate_on_the_second_pass() {
        let lo = if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        };
        let mut sampler = NetSampler::new(vec![lo.to_string()]);
        let first = sampler.sample();
        if first.is_empty() {
            // Sandboxed CI without a loopback interface: nothing to assert.
            return;
        }
        assert_eq!(first[0].interface, lo);
        assert!(first[0].rx_bytes_per_sec.is_none());
        std::thread::sleep(Duration::from_millis(20));
        let second = sampler.sample();
        assert!(second[0].rx_bytes_per_sec.is_some());
        assert!(second[0].rx_bytes >= first[0].rx_bytes);
    }
}
