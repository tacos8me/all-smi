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

//! Sparkline history for the Consolidated tab.
//!
//! The cluster-wide histories in `AppState` are averages; this tab needs a
//! series per device and per link, so it keeps its own keyed ring buffers.
//! One sample is appended per collection cycle.

use std::collections::{HashMap, VecDeque};

use crate::device::GpuInfo;
use crate::probes::HostProbes;

/// Samples kept per series. Enough to fill a 120-column braille sparkline
/// (two samples per cell), i.e. 8 minutes at the default 2 s interval.
pub const HISTORY_LEN: usize = 240;

/// Upper bound on distinct series so a host that churns device UUIDs
/// cannot grow the map without bound.
const MAX_SERIES: usize = 256;

/// Series keys. Device series are keyed by UUID, link series by
/// `host_id/interface`.
pub fn util_key(uuid: &str) -> String {
    format!("util:{uuid}")
}
pub fn power_key(uuid: &str) -> String {
    format!("power:{uuid}")
}
pub fn net_rx_key(host_id: &str, iface: &str) -> String {
    format!("net_rx:{host_id}/{iface}")
}
pub fn net_tx_key(host_id: &str, iface: &str) -> String {
    format!("net_tx:{host_id}/{iface}")
}
pub const TOTAL_POWER_KEY: &str = "total_power";
pub const TOTAL_UTIL_KEY: &str = "total_util";

#[derive(Clone, Debug, Default)]
pub struct SeriesHistory {
    series: HashMap<String, VecDeque<f64>>,
}

impl SeriesHistory {
    pub fn push(&mut self, key: String, value: f64) {
        if !value.is_finite() {
            return;
        }
        if !self.series.contains_key(&key) && self.series.len() >= MAX_SERIES {
            return;
        }
        let buf = self.series.entry(key).or_default();
        if buf.len() == HISTORY_LEN {
            buf.pop_front();
        }
        buf.push_back(value);
    }

    /// Samples oldest-first, or an empty vector for an unknown series.
    pub fn values(&self, key: &str) -> Vec<f64> {
        self.series
            .get(key)
            .map(|b| b.iter().copied().collect())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.series.len()
    }

    pub fn is_empty(&self) -> bool {
        self.series.is_empty()
    }
}

/// Everything the Consolidated tab keeps between frames. `AppState` holds
/// `Some` only when the tab was requested.
#[derive(Clone, Debug, Default)]
pub struct ConsolidatedState {
    pub history: SeriesHistory,
}

impl ConsolidatedState {
    /// Append one sample per series from a finished collection cycle.
    /// Devices with no reading contribute nothing rather than a zero, so a
    /// gap in the source never renders as an idle dip.
    pub fn record_collection(&mut self, gpu_info: &[GpuInfo], host_probes: &[HostProbes]) {
        let mut total_power = 0.0;
        let mut util_sum = 0.0;
        let mut util_count = 0usize;
        for gpu in gpu_info {
            if let Some(util) = gpu.utilization_reading() {
                self.history.push(util_key(&gpu.uuid), util);
                util_sum += util;
                util_count += 1;
            }
            if let Some(watts) = gpu.power_consumption_reading() {
                self.history.push(power_key(&gpu.uuid), watts);
                total_power += watts;
            }
        }
        if !gpu_info.is_empty() {
            self.history.push(TOTAL_POWER_KEY.to_string(), total_power);
        }
        if util_count > 0 {
            self.history
                .push(TOTAL_UTIL_KEY.to_string(), util_sum / util_count as f64);
        }
        for host in host_probes {
            for iface in &host.interfaces {
                if let Some(rx) = iface.rx_bytes_per_sec {
                    self.history
                        .push(net_rx_key(&host.host_id, &iface.interface), rx);
                }
                if let Some(tx) = iface.tx_bytes_per_sec {
                    self.history
                        .push(net_tx_key(&host.host_id, &iface.interface), tx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::net::NetInterfaceSample;
    use crate::ui::consolidated::model::tests::gpu;

    #[test]
    fn push_caps_each_series_at_history_len() {
        let mut h = SeriesHistory::default();
        for i in 0..(HISTORY_LEN + 10) {
            h.push("a".to_string(), i as f64);
        }
        let v = h.values("a");
        assert_eq!(v.len(), HISTORY_LEN);
        assert_eq!(v[0], 10.0);
        assert_eq!(*v.last().unwrap(), (HISTORY_LEN + 9) as f64);
    }

    #[test]
    fn non_finite_samples_are_dropped() {
        let mut h = SeriesHistory::default();
        h.push("a".to_string(), f64::NAN);
        assert!(h.values("a").is_empty());
    }

    #[test]
    fn series_count_is_capped() {
        let mut h = SeriesHistory::default();
        for i in 0..(MAX_SERIES + 5) {
            h.push(format!("k{i}"), 1.0);
        }
        assert_eq!(h.len(), MAX_SERIES);
    }

    #[test]
    fn record_collection_skips_missing_readings_and_sums_power() {
        let mut state = ConsolidatedState::default();
        let mut idle = gpu("h1:9090", "gpu-a", "NVIDIA RTX", 0);
        idle.power_consumption = 90.0;
        idle.utilization = 10.0;
        let mut unavailable = gpu("h2:9090", "gpu-b", "Apple M5 Ultra GPU", 0);
        unavailable.power_consumption = crate::device::types::GPU_METRIC_UNAVAILABLE;
        unavailable.utilization = 30.0;
        let probes = vec![HostProbes {
            host_id: "h2:9090".to_string(),
            interfaces: vec![NetInterfaceSample {
                interface: "en0".to_string(),
                rx_bytes_per_sec: Some(5.0),
                ..Default::default()
            }],
            ..Default::default()
        }];
        state.record_collection(&[idle, unavailable], &probes);
        assert_eq!(state.history.values(&power_key("gpu-a")), vec![90.0]);
        assert!(state.history.values(&power_key("gpu-b")).is_empty());
        assert_eq!(state.history.values(TOTAL_POWER_KEY), vec![90.0]);
        assert_eq!(state.history.values(TOTAL_UTIL_KEY), vec![20.0]);
        assert_eq!(
            state.history.values(&net_rx_key("h2:9090", "en0")),
            vec![5.0]
        );
        assert!(
            state
                .history
                .values(&net_tx_key("h2:9090", "en0"))
                .is_empty()
        );
    }
}
