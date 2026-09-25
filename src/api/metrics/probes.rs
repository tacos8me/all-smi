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

//! Prometheus families for the opt-in host probes (`--net-iface`,
//! `--watch-lock`). Emits nothing when no probe is configured, so a stock
//! exporter's `/metrics` output is unchanged.

use super::{MetricBuilder, MetricExporter};
use crate::probes::HostProbes;

pub const NET_RX_TOTAL: &str = "all_smi_network_receive_bytes_total";
pub const NET_TX_TOTAL: &str = "all_smi_network_transmit_bytes_total";
pub const NET_RX_RATE: &str = "all_smi_network_receive_bytes_per_second";
pub const NET_TX_RATE: &str = "all_smi_network_transmit_bytes_per_second";
pub const LOCK_HELD: &str = "all_smi_lock_held";
pub const LOCK_HOLDER_INFO: &str = "all_smi_lock_holder_info";
pub const LOCK_HELD_SINCE: &str = "all_smi_lock_held_since_seconds";

pub struct ProbeMetricExporter<'a> {
    probes: &'a [HostProbes],
}

impl<'a> ProbeMetricExporter<'a> {
    pub fn new(probes: &'a [HostProbes]) -> Self {
        Self { probes }
    }

    fn export_network(&self, builder: &mut MetricBuilder) {
        if self.probes.iter().all(|p| p.interfaces.is_empty()) {
            return;
        }
        let families = [
            (NET_RX_TOTAL, "counter", "Bytes received on the interface"),
            (
                NET_TX_TOTAL,
                "counter",
                "Bytes transmitted on the interface",
            ),
            (
                NET_RX_RATE,
                "gauge",
                "Receive rate in bytes per second over the last collection interval",
            ),
            (
                NET_TX_RATE,
                "gauge",
                "Transmit rate in bytes per second over the last collection interval",
            ),
        ];
        for (name, kind, help) in families {
            builder.help(name, help).type_(name, kind);
            for host in self.probes {
                for iface in &host.interfaces {
                    let labels = [
                        ("instance", host.instance.as_str()),
                        ("hostname", host.hostname.as_str()),
                        ("interface", iface.interface.as_str()),
                    ];
                    match name {
                        NET_RX_TOTAL => {
                            builder.metric(name, &labels, iface.rx_bytes);
                        }
                        NET_TX_TOTAL => {
                            builder.metric(name, &labels, iface.tx_bytes);
                        }
                        NET_RX_RATE => {
                            if let Some(r) = iface.rx_bytes_per_sec {
                                builder.metric(name, &labels, format!("{r:.1}"));
                            }
                        }
                        _ => {
                            if let Some(r) = iface.tx_bytes_per_sec {
                                builder.metric(name, &labels, format!("{r:.1}"));
                            }
                        }
                    }
                }
            }
        }
    }

    fn export_locks(&self, builder: &mut MetricBuilder) {
        if self.probes.iter().all(|p| p.locks.is_empty()) {
            return;
        }
        builder
            .help(
                LOCK_HELD,
                "1 when some process has the watched lock file open",
            )
            .type_(LOCK_HELD, "gauge");
        for host in self.probes {
            for lock in &host.locks {
                let labels = [
                    ("instance", host.instance.as_str()),
                    ("hostname", host.hostname.as_str()),
                    ("path", lock.path.as_str()),
                ];
                builder.metric(LOCK_HELD, &labels, u8::from(lock.is_held()));
            }
        }

        builder
            .help(
                LOCK_HOLDER_INFO,
                "Process holding the watched lock file open",
            )
            .type_(LOCK_HOLDER_INFO, "gauge");
        for host in self.probes {
            for lock in &host.locks {
                for holder in &lock.holders {
                    let pid = holder.pid.to_string();
                    let labels = [
                        ("instance", host.instance.as_str()),
                        ("hostname", host.hostname.as_str()),
                        ("path", lock.path.as_str()),
                        ("pid", pid.as_str()),
                        ("command", holder.command.as_str()),
                    ];
                    builder.metric(LOCK_HOLDER_INFO, &labels, 1);
                }
            }
        }

        builder
            .help(
                LOCK_HELD_SINCE,
                "Unix time the current holder of the watched lock file was first seen",
            )
            .type_(LOCK_HELD_SINCE, "gauge");
        for host in self.probes {
            for lock in &host.locks {
                if let Some(since) = lock.since_unix {
                    let labels = [
                        ("instance", host.instance.as_str()),
                        ("hostname", host.hostname.as_str()),
                        ("path", lock.path.as_str()),
                    ];
                    builder.metric(LOCK_HELD_SINCE, &labels, since);
                }
            }
        }
    }
}

impl MetricExporter for ProbeMetricExporter<'_> {
    fn export_metrics(&self) -> String {
        let mut builder = MetricBuilder::new();
        self.export_network(&mut builder);
        self.export_locks(&mut builder);
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::NetInterfaceSample;
    use crate::probes::lock::{LockHolder, LockSample};

    fn sample_probes() -> HostProbes {
        HostProbes {
            host_id: "mac".to_string(),
            hostname: "mac".to_string(),
            instance: "mac".to_string(),
            interfaces: vec![NetInterfaceSample {
                interface: "en0".to_string(),
                rx_bytes: 1_000,
                tx_bytes: 2_000,
                rx_bytes_per_sec: Some(125.0),
                tx_bytes_per_sec: None,
            }],
            locks: vec![
                LockSample {
                    path: "/locks/gpu.lock".to_string(),
                    holders: vec![LockHolder {
                        pid: 42,
                        command: "omlx-server".to_string(),
                    }],
                    since_unix: Some(1_700_000_000),
                },
                LockSample {
                    path: "/locks/free.lock".to_string(),
                    holders: Vec::new(),
                    since_unix: None,
                },
            ],
        }
    }

    #[test]
    fn empty_probe_set_exports_nothing() {
        assert!(ProbeMetricExporter::new(&[]).export_metrics().is_empty());
        let empty = HostProbes::default();
        assert!(
            ProbeMetricExporter::new(std::slice::from_ref(&empty))
                .export_metrics()
                .is_empty()
        );
    }

    #[test]
    fn exports_counters_rates_and_lock_state() {
        let out = ProbeMetricExporter::new(&[sample_probes()]).export_metrics();
        assert!(out.contains(
            "all_smi_network_receive_bytes_total{instance=\"mac\", hostname=\"mac\", interface=\"en0\"} 1000\n"
        ));
        assert!(out.contains(
            "all_smi_network_receive_bytes_per_second{instance=\"mac\", hostname=\"mac\", interface=\"en0\"} 125.0\n"
        ));
        // No transmit rate on a first sample.
        assert!(!out.contains("all_smi_network_transmit_bytes_per_second{"));
        assert!(out.contains(
            "all_smi_lock_held{instance=\"mac\", hostname=\"mac\", path=\"/locks/gpu.lock\"} 1\n"
        ));
        assert!(out.contains(
            "all_smi_lock_held{instance=\"mac\", hostname=\"mac\", path=\"/locks/free.lock\"} 0\n"
        ));
        assert!(out.contains("pid=\"42\", command=\"omlx-server\"} 1\n"));
        assert!(out.contains(
            "all_smi_lock_held_since_seconds{instance=\"mac\", hostname=\"mac\", path=\"/locks/gpu.lock\"} 1700000000\n"
        ));
    }
}
