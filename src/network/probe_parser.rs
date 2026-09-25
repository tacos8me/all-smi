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

//! Viewer-side parser for the opt-in host probe families
//! (`all_smi_network_*`, `all_smi_lock_*`) exported by
//! [`crate::api::metrics::probes`]. Kept out of `metrics_parser.rs`, which
//! only routes the lines here.

use std::collections::{BTreeMap, HashMap};

use crate::probes::HostProbes;
use crate::probes::lock::{LockHolder, LockSample};
use crate::probes::net::NetInterfaceSample;

/// Per-scrape caps so a hostile exporter cannot grow the viewer's memory
/// by advertising unbounded interface names, lock paths, or holders.
const MAX_INTERFACES: usize = 64;
const MAX_LOCKS: usize = 64;
const MAX_HOLDERS_PER_LOCK: usize = 16;

/// True for metric names (without the `all_smi_` prefix) this parser owns.
pub(crate) fn is_probe_metric(metric_name: &str) -> bool {
    metric_name.starts_with("network_") || metric_name.starts_with("lock_")
}

#[derive(Default)]
pub(crate) struct ProbeParseState {
    hostname: Option<String>,
    instance: Option<String>,
    interfaces: BTreeMap<String, NetInterfaceSample>,
    locks: BTreeMap<String, LockSample>,
}

impl ProbeParseState {
    pub(crate) fn process(
        &mut self,
        metric_name: &str,
        labels: &HashMap<String, String>,
        value: f64,
    ) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        if self.hostname.is_none() {
            self.hostname = labels.get("hostname").cloned();
        }
        if self.instance.is_none() {
            self.instance = labels.get("instance").cloned();
        }
        if let Some(field) = metric_name.strip_prefix("network_") {
            self.process_network(field, labels, value);
        } else if let Some(field) = metric_name.strip_prefix("lock_") {
            self.process_lock(field, labels, value);
        }
    }

    fn process_network(&mut self, field: &str, labels: &HashMap<String, String>, value: f64) {
        let Some(name) = labels.get("interface").filter(|n| !n.is_empty()) else {
            return;
        };
        if !self.interfaces.contains_key(name) && self.interfaces.len() >= MAX_INTERFACES {
            return;
        }
        let iface = self
            .interfaces
            .entry(name.clone())
            .or_insert_with(|| NetInterfaceSample {
                interface: name.clone(),
                ..Default::default()
            });
        match field {
            "receive_bytes_total" => iface.rx_bytes = value as u64,
            "transmit_bytes_total" => iface.tx_bytes = value as u64,
            "receive_bytes_per_second" => iface.rx_bytes_per_sec = Some(value),
            "transmit_bytes_per_second" => iface.tx_bytes_per_sec = Some(value),
            _ => {}
        }
    }

    fn process_lock(&mut self, field: &str, labels: &HashMap<String, String>, value: f64) {
        let Some(path) = labels.get("path").filter(|p| !p.is_empty()) else {
            return;
        };
        if !self.locks.contains_key(path) && self.locks.len() >= MAX_LOCKS {
            return;
        }
        let lock = self
            .locks
            .entry(path.clone())
            .or_insert_with(|| LockSample {
                path: path.clone(),
                ..Default::default()
            });
        match field {
            "holder_info" => {
                let Some(pid) = labels.get("pid").and_then(|p| p.parse::<u32>().ok()) else {
                    return;
                };
                if lock.holders.len() < MAX_HOLDERS_PER_LOCK
                    && !lock.holders.iter().any(|h| h.pid == pid)
                {
                    lock.holders.push(LockHolder {
                        pid,
                        command: labels.get("command").cloned().unwrap_or_default(),
                    });
                    lock.holders.sort_by_key(|h| h.pid);
                }
            }
            "held_since_seconds" => lock.since_unix = Some(value as u64),
            // `lock_held` only establishes the entry; the holder rows are
            // what the viewer renders.
            _ => {}
        }
    }

    /// `None` when the scrape carried no probe series at all.
    pub(crate) fn finish(self, host: &str) -> Option<HostProbes> {
        if self.interfaces.is_empty() && self.locks.is_empty() {
            return None;
        }
        let hostname = self.hostname.unwrap_or_else(|| host.to_string());
        Some(HostProbes {
            host_id: host.to_string(),
            instance: self.instance.unwrap_or_else(|| hostname.clone()),
            hostname,
            interfaces: self.interfaces.into_values().collect(),
            locks: self.locks.into_values().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::api::metrics::MetricExporter;
    use crate::api::metrics::probes::ProbeMetricExporter;
    use crate::network::metrics_parser::MetricsParser;
    use crate::probes::HostProbes;
    use crate::probes::lock::{LockHolder, LockSample};
    use crate::probes::net::NetInterfaceSample;

    fn regex() -> regex::Regex {
        regex::Regex::new(r"^all_smi_([^\{]+)\{([^}]+)\} ([\d\.]+)$").unwrap()
    }

    fn exported() -> HostProbes {
        HostProbes {
            host_id: "ignored".to_string(),
            hostname: "mac".to_string(),
            instance: "mac".to_string(),
            interfaces: vec![NetInterfaceSample {
                interface: "en0".to_string(),
                rx_bytes: 275_619_451_663,
                tx_bytes: 551_826_449_812,
                rx_bytes_per_sec: Some(1_250_000.5),
                tx_bytes_per_sec: Some(0.0),
            }],
            locks: vec![LockSample {
                path: "/Users/ian/llm/locks/gpu.lock".to_string(),
                holders: vec![LockHolder {
                    pid: 49097,
                    command: "omlx-server".to_string(),
                }],
                since_unix: Some(1_790_370_904),
            }],
        }
    }

    #[test]
    fn probe_series_round_trip_through_the_exporter() {
        let text = ProbeMetricExporter::new(&[exported()]).export_metrics();
        let parsed = MetricsParser::new().parse_metrics(&text, "10.10.10.2:9090", &regex());
        let probes = parsed.host_probes.expect("probe series present");
        assert_eq!(probes.host_id, "10.10.10.2:9090");
        assert_eq!(probes.hostname, "mac");
        assert_eq!(probes.interfaces, exported().interfaces);
        assert_eq!(probes.locks, exported().locks);
    }

    #[test]
    fn free_lock_parses_without_holders() {
        let text = "all_smi_lock_held{instance=\"mac\", hostname=\"mac\", path=\"/l\"} 0\n";
        let parsed = MetricsParser::new().parse_metrics(text, "h:9090", &regex());
        let probes = parsed.host_probes.unwrap();
        assert_eq!(probes.locks.len(), 1);
        assert!(!probes.locks[0].is_held());
        assert_eq!(probes.locks[0].since_unix, None);
    }

    #[test]
    fn scrape_without_probe_series_has_none() {
        let text = "all_smi_gpu_utilization{gpu=\"g\", instance=\"i\", gpu_uuid=\"u\", gpu_index=\"0\"} 5\n";
        let parsed = MetricsParser::new().parse_metrics(text, "h:9090", &regex());
        assert!(parsed.host_probes.is_none());
    }

    #[test]
    fn interface_count_is_capped() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!(
                "all_smi_network_receive_bytes_total{{instance=\"i\", hostname=\"h\", interface=\"if{i}\"}} 1\n"
            ));
        }
        let parsed = MetricsParser::new().parse_metrics(&text, "h:9090", &regex());
        assert_eq!(parsed.host_probes.unwrap().interfaces.len(), 64);
    }
}
