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

//! Pure view model shared by the All and Consolidated tabs.
//!
//! Groups the scraped devices by host in tab-strip order, labels each
//! device's memory as unified (Apple Silicon) or dedicated VRAM, and sums
//! memory and power across every host, counting a unified-memory host's
//! RAM once (as the GPU's pool) rather than again as host RAM. Hosts that
//! stopped answering keep a row so an outage is visible instead of
//! silently shrinking the table.

use std::collections::HashMap;

use crate::app_state::ConnectionStatus;
use crate::device::{CpuInfo, GpuInfo, MemoryInfo};
use crate::probes::HostProbes;

/// Where a device's memory lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Apple Silicon: the GPU shares the host's unified memory.
    Unified,
    /// Discrete GPU with its own VRAM.
    Dedicated,
}

impl MemoryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unified => "unified",
            Self::Dedicated => "VRAM",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceRow {
    pub uuid: String,
    /// Vendor and model, without edition suffixes ("NVIDIA RTX PRO 6000
    /// Blackwell", "Apple M5 Ultra"), with ` #<index>` when a host has
    /// several identical boards.
    pub name: String,
    pub memory_kind: MemoryKind,
    pub utilization: Option<f64>,
    pub used_memory: u64,
    pub total_memory: u64,
    pub power_watts: Option<f64>,
    pub power_limit_watts: Option<f64>,
    pub temperature_c: Option<u32>,
    /// Slowdown threshold, when the device reports one.
    pub slowdown_c: Option<u32>,
    pub frequency_mhz: Option<u32>,
    /// Apple Neural Engine power, when the device reports one.
    pub ane_watts: Option<f64>,
    pub core_count: Option<u32>,
    /// Static and slow-moving facts for the details view (`x`): thermal
    /// thresholds, P-state, driver, firmware, link.
    pub details: Vec<String>,
}

impl DeviceRow {
    /// Name plus the GPU core count for SoC GPUs:
    /// "Apple M5 Ultra · 80-core GPU".
    pub fn label(&self) -> String {
        match self.core_count {
            Some(n) if self.memory_kind == MemoryKind::Unified => {
                format!("{} · {n}-core GPU", self.name)
            }
            _ => self.name.clone(),
        }
    }

    pub fn memory_ratio(&self) -> f64 {
        if self.total_memory > 0 {
            self.used_memory as f64 / self.total_memory as f64
        } else {
            0.0
        }
    }

    pub fn power_ratio(&self) -> Option<f64> {
        match (self.power_watts, self.power_limit_watts) {
            (Some(p), Some(l)) if l > 0.0 => Some(p / l),
            _ => None,
        }
    }
}

/// Host CPU summary for the host header.
#[derive(Clone, Debug, PartialEq)]
pub struct HostCpu {
    /// "Apple M5 Ultra", "AMD EPYC 9275F".
    pub model: String,
    pub cores: u32,
    pub utilization: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostSection {
    pub host_id: String,
    pub label: String,
    pub connected: bool,
    pub last_error: Option<String>,
    pub devices: Vec<DeviceRow>,
    /// Package CPU power, reported by Apple Silicon hosts.
    pub cpu_power_watts: Option<f64>,
    /// "macOS", "Linux", from the exporter's build info.
    pub os: Option<String>,
    pub cpu: Option<HostCpu>,
    pub ram_used: u64,
    pub ram_total: u64,
}

impl HostSection {
    /// The host's RAM is the GPU's unified memory pool.
    pub fn has_unified_memory(&self) -> bool {
        self.devices
            .iter()
            .any(|d| d.memory_kind == MemoryKind::Unified)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub devices: usize,
    pub unified_devices: usize,
    pub hosts_up: usize,
    pub hosts_total: usize,
    pub avg_utilization: Option<f64>,
    pub unified_used: u64,
    pub unified_total: u64,
    pub dedicated_used: u64,
    pub dedicated_total: u64,
    /// RAM of hosts whose memory is not already counted as a unified pool.
    pub host_ram_used: u64,
    pub host_ram_total: u64,
    pub unified_power_watts: f64,
    pub dedicated_power_watts: f64,
    /// Sum of the board power limits the devices report (0 when none do).
    pub power_limit_watts: f64,
    /// Hottest device reading and that device's slowdown threshold.
    pub max_temperature: Option<(u32, Option<u32>)>,
}

impl Totals {
    pub fn memory_used(&self) -> u64 {
        self.unified_used + self.dedicated_used
    }

    pub fn memory_total(&self) -> u64 {
        self.unified_total + self.dedicated_total
    }

    pub fn power_watts(&self) -> f64 {
        self.unified_power_watts + self.dedicated_power_watts
    }
}

/// Everything the model is built from.
pub struct ModelSources<'a> {
    pub gpu_info: &'a [GpuInfo],
    pub cpu_info: &'a [CpuInfo],
    pub memory_info: &'a [MemoryInfo],
    pub host_probes: &'a [HostProbes],
    /// Tab strip, for host order; reserved tabs are skipped.
    pub host_order: &'a [String],
    pub connection_status: &'a HashMap<String, ConnectionStatus>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConsolidatedModel {
    pub hosts: Vec<HostSection>,
    pub totals: Totals,
}

impl ConsolidatedModel {
    /// Build the model. Hosts follow the tab strip; hosts that only appear
    /// in `gpu_info` are appended.
    pub fn build(src: &ModelSources<'_>) -> Self {
        let mut order: Vec<String> = src
            .host_order
            .iter()
            .filter(|t| !crate::ui::tabs::is_reserved_tab(t))
            .cloned()
            .collect();
        for gpu in src.gpu_info {
            if !order.contains(&gpu.host_id) {
                order.push(gpu.host_id.clone());
            }
        }

        let mut hosts = Vec::with_capacity(order.len());
        let mut totals = Totals {
            hosts_total: order.len(),
            ..Default::default()
        };
        let mut util_sum = 0.0;
        let mut util_count = 0usize;

        for host_id in order {
            let status = src.connection_status.get(&host_id);
            let mut gpus: Vec<&GpuInfo> = src
                .gpu_info
                .iter()
                .filter(|g| g.host_id == host_id)
                .collect();
            gpus.sort_by_key(|g| (device_index(g), g.uuid.clone()));
            let connected = status.map(|s| s.is_connected).unwrap_or(!gpus.is_empty());
            if connected {
                totals.hosts_up += 1;
            }
            let label = status
                .and_then(|s| s.actual_hostname.clone())
                .or_else(|| gpus.first().map(|g| g.instance.clone()))
                .filter(|l| !l.is_empty())
                .map(|l| short_host_label(&l))
                .unwrap_or_else(|| host_id.clone());

            let host_cpu = src.cpu_info.iter().find(|c| c.host_id == host_id);
            // Remote Apple GPUs carry their core count on the CPU series.
            let soc_gpu_cores = host_cpu
                .and_then(|c| c.apple_silicon_info.as_ref())
                .map(|a| a.gpu_core_count)
                .filter(|n| *n > 0);
            let names: Vec<String> = gpus.iter().map(|g| device_name(g)).collect();
            let devices: Vec<DeviceRow> = gpus
                .iter()
                .zip(&names)
                .map(|(g, name)| {
                    let duplicate = names.iter().filter(|n| *n == name).count() > 1;
                    let mut row = device_row(g, name, duplicate);
                    if row.memory_kind == MemoryKind::Unified && row.core_count.is_none() {
                        row.core_count = soc_gpu_cores;
                    }
                    row
                })
                .collect();

            for d in &devices {
                totals.devices += 1;
                if let Some(u) = d.utilization {
                    util_sum += u;
                    util_count += 1;
                }
                if let Some(t) = d.temperature_c
                    && totals.max_temperature.is_none_or(|(max, _)| t > max)
                {
                    totals.max_temperature = Some((t, d.slowdown_c));
                }
                let power = d.power_watts.unwrap_or(0.0);
                totals.power_limit_watts += d.power_limit_watts.unwrap_or(0.0);
                match d.memory_kind {
                    MemoryKind::Unified => {
                        totals.unified_devices += 1;
                        totals.unified_used += d.used_memory;
                        totals.unified_total += d.total_memory;
                        totals.unified_power_watts += power;
                    }
                    MemoryKind::Dedicated => {
                        totals.dedicated_used += d.used_memory;
                        totals.dedicated_total += d.total_memory;
                        totals.dedicated_power_watts += power;
                    }
                }
            }

            let unified = devices.iter().any(|d| d.memory_kind == MemoryKind::Unified);
            let cpu_power_watts = host_cpu
                .and_then(|c| c.power_consumption)
                .filter(|_| unified);
            let (ram_used, ram_total) = src
                .memory_info
                .iter()
                .filter(|m| m.host_id == host_id)
                .fold((0, 0), |(u, t), m| (u + m.used_bytes, t + m.total_bytes));
            if !unified {
                totals.host_ram_used += ram_used;
                totals.host_ram_total += ram_total;
            }
            let os = src
                .host_probes
                .iter()
                .find(|p| p.host_id == host_id)
                .and_then(|p| p.os.as_deref())
                .map(os_label);

            hosts.push(HostSection {
                host_id,
                label,
                connected,
                last_error: status.and_then(|s| s.last_error.clone()),
                devices,
                cpu_power_watts,
                os,
                cpu: host_cpu.map(|c| HostCpu {
                    model: cpu_model_label(&c.cpu_model),
                    cores: c
                        .apple_silicon_info
                        .as_ref()
                        .map(|a| a.s_core_count + a.p_core_count + a.e_core_count)
                        .filter(|n| *n > 0)
                        .unwrap_or(c.total_cores),
                    utilization: c.utilization,
                }),
                ram_used,
                ram_total,
            });
        }

        totals.avg_utilization = (util_count > 0).then(|| util_sum / util_count as f64);
        Self { hosts, totals }
    }

    /// The same model narrowed to one host (a host tab). Totals stay
    /// cluster-wide.
    pub fn only_host(&self, host_id: &str) -> Self {
        Self {
            hosts: self
                .hosts
                .iter()
                .filter(|h| h.host_id == host_id)
                .cloned()
                .collect(),
            totals: self.totals.clone(),
        }
    }

    /// Display label for a host id, falling back to the id itself.
    pub fn host_label(&self, host_id: &str) -> String {
        self.hosts
            .iter()
            .find(|h| h.host_id == host_id)
            .map(|h| h.label.clone())
            .unwrap_or_else(|| host_id.to_string())
    }
}

fn device_index(gpu: &GpuInfo) -> u32 {
    gpu.detail
        .get("index")
        .and_then(|i| i.parse().ok())
        .unwrap_or(u32::MAX)
}

fn is_unified(gpu: &GpuInfo) -> bool {
    gpu.name.contains("Apple")
        || gpu
            .detail
            .get("architecture")
            .is_some_and(|a| a == "Apple Silicon")
}

fn device_row(gpu: &GpuInfo, name: &str, duplicate: bool) -> DeviceRow {
    let unified = is_unified(gpu);
    let name = if duplicate {
        format!(
            "{name} #{}",
            gpu.detail.get("index").map_or("?", String::as_str)
        )
    } else {
        name.to_string()
    };
    DeviceRow {
        uuid: gpu.uuid.clone(),
        name,
        memory_kind: if unified {
            MemoryKind::Unified
        } else {
            MemoryKind::Dedicated
        },
        utilization: gpu.utilization_reading(),
        used_memory: gpu.used_memory,
        total_memory: gpu.total_memory,
        power_watts: gpu.power_consumption_reading(),
        power_limit_watts: gpu
            .detail
            .get("power_limit_max")
            .and_then(|p| p.parse::<f64>().ok())
            .filter(|p| *p > 0.0),
        temperature_c: gpu.temperature_reading(),
        slowdown_c: gpu.temperature_threshold_slowdown.filter(|t| *t > 0),
        frequency_mhz: gpu.frequency_reading(),
        // Apple readers carry ANE power in milliwatts in this field (the
        // exporter divides by 1000 for `all_smi_ane_power_watts`).
        ane_watts: unified
            .then(|| gpu.ane_utilization_reading())
            .flatten()
            .map(|mw| mw / 1000.0),
        core_count: gpu.gpu_core_count,
        details: device_details(gpu),
    }
}

/// The facts the old per-GPU rows printed on every refresh, collected for
/// the details view instead.
fn device_details(gpu: &GpuInfo) -> Vec<String> {
    let mut out = Vec::new();
    let thresholds = [
        ("slowdown", gpu.temperature_threshold_slowdown),
        ("shutdown", gpu.temperature_threshold_shutdown),
        ("max op", gpu.temperature_threshold_max_operating),
    ];
    for (label, value) in thresholds {
        if let Some(t) = value.filter(|t| *t > 0) {
            out.push(format!("{label} {t}°C"));
        }
    }
    if let Some(p) = gpu.performance_state {
        out.push(format!("P{p}"));
    }
    if let Some(level) = gpu.detail.get("thermal_pressure") {
        out.push(format!("thermal {level}"));
    }
    if let Some(rpm) = gpu.fan_speed_rpm {
        out.push(format!("fan {rpm} rpm"));
    }
    if let Some(v) = gpu.detail.get("driver_version") {
        out.push(if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            format!("driver {v}")
        } else {
            v.clone()
        });
    }
    if let Some(v) = gpu.detail.get("cuda_version") {
        out.push(format!("CUDA {v}"));
    }
    let gsp = match gpu.gsp_firmware_mode {
        Some(0) => Some("off"),
        Some(1) => Some("on"),
        Some(2) => Some("default"),
        _ => None,
    };
    if let Some(mode) = gsp {
        out.push(format!("GSP {mode}"));
    }
    if let (Some(gen_max), Some(width)) = (
        gpu.detail.get("pcie_gen_max"),
        gpu.detail.get("pcie_width_max"),
    ) {
        out.push(format!("PCIe {gen_max}.0 x{width}"));
    }
    if let Some(ecc) = gpu.detail.get("ecc_mode_current") {
        out.push(format!("ECC {}", ecc.to_lowercase()));
    }
    if let Some(numa) = gpu.numa_node_id.filter(|n| *n >= 0) {
        out.push(format!("NUMA {numa}"));
    }
    if !gpu.nvlink_remote_devices.is_empty() {
        out.push(format!("NVLink ×{}", gpu.nvlink_remote_devices.len()));
    }
    out
}

/// Vendor and model without edition suffixes: "NVIDIA RTX PRO 6000
/// Blackwell Workstation Edition" becomes "NVIDIA RTX PRO 6000 Blackwell",
/// "Apple M5 Ultra GPU" becomes "Apple M5 Ultra".
pub fn device_name(gpu: &GpuInfo) -> String {
    let mut s = gpu.name.trim();
    for suffix in [
        " Max-Q Workstation Edition",
        " Workstation Edition",
        " Server Edition",
    ] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest;
        }
    }
    if is_unified(gpu)
        && let Some(rest) = s.strip_suffix(" GPU")
    {
        s = rest;
    }
    s.to_string()
}

/// "AMD EPYC 9275F 24-Core Processor" → "AMD EPYC 9275F".
fn cpu_model_label(model: &str) -> String {
    let mut words: Vec<&str> = model
        .split_whitespace()
        .filter(|w| !matches!(*w, "Processor" | "CPU" | "(R)" | "(TM)"))
        .collect();
    if let Some(pos) = words
        .iter()
        .position(|w| w.to_ascii_lowercase().ends_with("-core"))
    {
        words.truncate(pos);
    }
    words.join(" ")
}

fn os_label(os: &str) -> String {
    match os {
        "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        "windows" => "Windows".to_string(),
        other => other.to_string(),
    }
}

/// "ians-Mac-Studio.local" reads better without the mDNS suffix.
pub fn short_host_label(name: &str) -> String {
    name.strip_suffix(".local").unwrap_or(name).to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn gpu(host: &str, uuid: &str, name: &str, index: u32) -> GpuInfo {
        let mut detail = HashMap::new();
        detail.insert("index".to_string(), index.to_string());
        GpuInfo {
            uuid: uuid.to_string(),
            time: String::new(),
            name: name.to_string(),
            device_type: "GPU".to_string(),
            host_id: host.to_string(),
            hostname: host.to_string(),
            instance: host.to_string(),
            utilization: 0.0,
            ane_utilization: crate::device::types::GPU_METRIC_UNAVAILABLE,
            dla_utilization: None,
            tensorcore_utilization: None,
            temperature: 40,
            used_memory: 0,
            total_memory: 0,
            frequency: 1000,
            power_consumption: 0.0,
            gpu_core_count: None,
            temperature_threshold_slowdown: None,
            temperature_threshold_shutdown: None,
            temperature_threshold_max_operating: None,
            temperature_threshold_acoustic: None,
            performance_state: None,
            fan_speed_rpm: None,
            numa_node_id: None,
            gsp_firmware_mode: None,
            gsp_firmware_version: None,
            nvlink_remote_devices: Vec::new(),
            gpm_metrics: None,
            detail,
        }
    }

    pub(crate) fn model_of(
        gpus: &[GpuInfo],
        cpus: &[CpuInfo],
        tabs: &[String],
        statuses: &HashMap<String, ConnectionStatus>,
    ) -> ConsolidatedModel {
        ConsolidatedModel::build(&ModelSources {
            gpu_info: gpus,
            cpu_info: cpus,
            memory_info: &[],
            host_probes: &[],
            host_order: tabs,
            connection_status: statuses,
        })
    }

    pub(crate) fn mac_and_box() -> (Vec<GpuInfo>, Vec<String>, HashMap<String, ConnectionStatus>) {
        const GIB: u64 = 1 << 30;
        let mut mac = gpu(
            "10.10.10.2:9090",
            "AppleSiliconGPU",
            "Apple M5 Ultra GPU",
            0,
        );
        mac.used_memory = 154 * GIB;
        mac.total_memory = 256 * GIB;
        mac.utilization = 4.0;
        mac.power_consumption = 12.0;
        mac.ane_utilization = 1500.0;
        mac.gpu_core_count = Some(80);
        mac.detail
            .insert("architecture".to_string(), "Apple Silicon".to_string());
        let name = "NVIDIA RTX PRO 6000 Blackwell Workstation Edition";
        let mut rtx1 = gpu("10.10.10.1:9090", "GPU-b", name, 1);
        let mut rtx0 = gpu("10.10.10.1:9090", "GPU-a", name, 0);
        for (g, util) in [(&mut rtx0, 50.0), (&mut rtx1, 30.0)] {
            g.used_memory = 87 * GIB;
            g.total_memory = 96 * GIB;
            g.utilization = util;
            g.power_consumption = 100.0;
            g.detail
                .insert("power_limit_max".to_string(), "600".to_string());
        }
        let tabs = vec![
            "All".to_string(),
            crate::ui::tabs::CONSOLIDATED_TAB_NAME.to_string(),
            crate::ui::tabs::USERS_TAB_NAME.to_string(),
            "10.10.10.2:9090".to_string(),
            "10.10.10.1:9090".to_string(),
        ];
        let mut statuses = HashMap::new();
        for (id, name) in [
            ("10.10.10.2:9090", "ians-Mac-Studio.local"),
            ("10.10.10.1:9090", "vllm"),
        ] {
            let mut s = ConnectionStatus::new(id.to_string(), format!("http://{id}"));
            s.mark_success();
            s.actual_hostname = Some(name.to_string());
            statuses.insert(id.to_string(), s);
        }
        (vec![rtx1, mac, rtx0], tabs, statuses)
    }

    #[test]
    fn groups_devices_by_host_in_tab_order_and_labels_memory() {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        assert_eq!(model.hosts.len(), 2);
        assert_eq!(model.hosts[0].label, "ians-Mac-Studio");
        assert_eq!(model.hosts[0].devices[0].name, "Apple M5 Ultra");
        assert_eq!(
            model.hosts[0].devices[0].label(),
            "Apple M5 Ultra · 80-core GPU"
        );
        assert_eq!(model.hosts[0].devices[0].memory_kind, MemoryKind::Unified);
        assert_eq!(model.hosts[0].devices[0].ane_watts, Some(1.5));
        assert_eq!(model.hosts[1].label, "vllm");
        let names: Vec<&str> = model.hosts[1]
            .devices
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "NVIDIA RTX PRO 6000 Blackwell #0",
                "NVIDIA RTX PRO 6000 Blackwell #1"
            ]
        );
        assert_eq!(model.hosts[1].devices[0].memory_kind, MemoryKind::Dedicated);
        assert_eq!(model.hosts[1].devices[0].power_limit_watts, Some(600.0));
        assert_eq!(model.hosts[1].devices[0].ane_watts, None);
    }

    #[test]
    fn totals_split_unified_and_dedicated() {
        const GIB: u64 = 1 << 30;
        let (gpus, tabs, statuses) = mac_and_box();
        let t = model_of(&gpus, &[], &tabs, &statuses).totals;
        assert_eq!(t.devices, 3);
        assert_eq!((t.hosts_up, t.hosts_total), (2, 2));
        assert_eq!(t.unified_used, 154 * GIB);
        assert_eq!(t.dedicated_total, 192 * GIB);
        assert_eq!(t.memory_total(), 448 * GIB);
        assert!((t.power_watts() - 212.0).abs() < 1e-9);
        assert!((t.unified_power_watts - 12.0).abs() < 1e-9);
        assert!((t.avg_utilization.unwrap() - 28.0).abs() < 1e-9);
    }

    #[test]
    fn unreachable_host_keeps_a_row() {
        let (gpus, tabs, mut statuses) = mac_and_box();
        let mac_down: Vec<GpuInfo> = gpus
            .into_iter()
            .filter(|g| g.host_id != "10.10.10.2:9090")
            .collect();
        let s = statuses.get_mut("10.10.10.2:9090").unwrap();
        s.mark_failure("Connection refused".to_string());
        let model = model_of(&mac_down, &[], &tabs, &statuses);
        let mac = &model.hosts[0];
        assert!(!mac.connected);
        assert!(mac.devices.is_empty());
        assert_eq!(mac.label, "ians-Mac-Studio");
        assert_eq!(mac.last_error.as_deref(), Some("Connection refused"));
        assert_eq!((model.totals.hosts_up, model.totals.hosts_total), (1, 2));
    }

    #[test]
    fn missing_readings_stay_absent() {
        let mut g = gpu("h:1", "u", "NVIDIA X", 0);
        g.utilization = crate::device::types::GPU_METRIC_UNAVAILABLE;
        g.power_consumption = crate::device::types::GPU_METRIC_UNAVAILABLE;
        let model = model_of(&[g], &[], &[], &HashMap::new());
        let d = &model.hosts[0].devices[0];
        assert_eq!(d.utilization, None);
        assert_eq!(d.power_watts, None);
        assert_eq!(model.totals.avg_utilization, None);
    }

    #[test]
    fn apple_gpu_core_count_falls_back_to_the_cpu_series() {
        let (mut gpus, tabs, statuses) = mac_and_box();
        for g in &mut gpus {
            g.gpu_core_count = None;
        }
        let cpu = CpuInfo {
            index: 0,
            host_id: "10.10.10.2:9090".to_string(),
            hostname: String::new(),
            instance: String::new(),
            cpu_model: "Apple M5 Ultra".to_string(),
            architecture: "arm64".to_string(),
            platform_type: crate::device::CpuPlatformType::AppleSilicon,
            socket_count: 1,
            total_cores: 36,
            total_threads: 36,
            base_frequency_mhz: 0,
            max_frequency_mhz: 0,
            cache_size_mb: 0,
            utilization: 0.0,
            temperature: None,
            power_consumption: Some(7.5),
            per_socket_info: Vec::new(),
            apple_silicon_info: Some(crate::device::AppleSiliconCpuInfo {
                s_core_count: 12,
                p_core_count: 24,
                e_core_count: 0,
                gpu_core_count: 80,
                s_core_utilization: 0.0,
                p_core_utilization: 0.0,
                e_core_utilization: 0.0,
                ane_ops_per_second: None,
                s_cluster_frequency_mhz: None,
                p_cluster_frequency_mhz: None,
                e_cluster_frequency_mhz: None,
                s_core_l2_cache_mb: None,
                p_core_l2_cache_mb: None,
                e_core_l2_cache_mb: None,
            }),
            per_core_utilization: Vec::new(),
            time: String::new(),
        };
        let model = model_of(&gpus, &[cpu], &tabs, &statuses);
        assert_eq!(model.hosts[0].devices[0].core_count, Some(80));
        assert_eq!(model.hosts[0].cpu_power_watts, Some(7.5));
        assert_eq!(model.hosts[1].devices[0].core_count, None);
        assert!((model.totals.power_limit_watts - 1200.0).abs() < 1e-9);
    }

    #[test]
    fn names_and_labels() {
        let rtx = gpu(
            "h",
            "u",
            "NVIDIA RTX PRO 6000 Blackwell Workstation Edition",
            0,
        );
        assert_eq!(device_name(&rtx), "NVIDIA RTX PRO 6000 Blackwell");
        let mac = gpu("h", "u", "Apple M5 Ultra GPU", 0);
        assert_eq!(device_name(&mac), "Apple M5 Ultra");
        assert_eq!(short_host_label("ians-Mac-Studio.local"), "ians-Mac-Studio");
        assert_eq!(short_host_label("vllm"), "vllm");
        assert_eq!(
            cpu_model_label("AMD EPYC 9275F 24-Core Processor"),
            "AMD EPYC 9275F"
        );
        assert_eq!(cpu_model_label("Apple M5 Ultra"), "Apple M5 Ultra");
        assert_eq!(os_label("macos"), "macOS");
    }

    #[test]
    fn unified_memory_is_counted_once_and_host_ram_separately() {
        const GIB: u64 = 1 << 30;
        let (gpus, tabs, statuses) = mac_and_box();
        let mem = |host: &str, used: u64, total: u64| MemoryInfo {
            index: 0,
            host_id: host.to_string(),
            hostname: host.to_string(),
            instance: host.to_string(),
            total_bytes: total * GIB,
            used_bytes: used * GIB,
            available_bytes: 0,
            free_bytes: 0,
            buffers_bytes: 0,
            cached_bytes: 0,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            swap_free_bytes: 0,
            utilization: 0.0,
            time: String::new(),
        };
        let memory = [
            mem("10.10.10.2:9090", 154, 256),
            mem("10.10.10.1:9090", 262, 503),
        ];
        let probes = [HostProbes {
            host_id: "10.10.10.2:9090".to_string(),
            os: Some("macos".to_string()),
            ..Default::default()
        }];
        let model = ConsolidatedModel::build(&ModelSources {
            gpu_info: &gpus,
            cpu_info: &[],
            memory_info: &memory,
            host_probes: &probes,
            host_order: &tabs,
            connection_status: &statuses,
        });
        let t = &model.totals;
        assert_eq!((t.unified_used, t.unified_total), (154 * GIB, 256 * GIB));
        assert_eq!((t.host_ram_used, t.host_ram_total), (262 * GIB, 503 * GIB));
        assert_eq!(t.unified_devices, 1);
        assert_eq!(t.max_temperature, Some((40, None)));
        assert!(model.hosts[0].has_unified_memory());
        assert_eq!(model.hosts[0].os.as_deref(), Some("macOS"));
        assert_eq!(model.hosts[1].ram_total, 503 * GIB);
        assert_eq!(model.hosts[1].os, None);
    }

    #[test]
    fn details_collect_thresholds_pstate_and_driver() {
        let mut g = gpu("h", "u", "NVIDIA X", 0);
        g.temperature_threshold_slowdown = Some(95);
        g.temperature_threshold_shutdown = Some(98);
        g.performance_state = Some(1);
        g.gsp_firmware_mode = Some(2);
        g.detail
            .insert("driver_version".to_string(), "595.45.04".to_string());
        g.detail.insert("pcie_gen_max".to_string(), "5".to_string());
        g.detail
            .insert("pcie_width_max".to_string(), "16".to_string());
        assert_eq!(
            device_details(&g),
            [
                "slowdown 95°C",
                "shutdown 98°C",
                "P1",
                "driver 595.45.04",
                "GSP default",
                "PCIe 5.0 x16"
            ]
        );
    }
}
