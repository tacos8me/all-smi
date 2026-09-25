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

//! Remote-mode cluster views: the header and overview shown above every
//! tab, and the body of the All tab (device table grouped by host, the
//! optional Icculis pipeline strip, and the history panel).
//!
//! Submodules:
//!
//! * [`line`] — line assembly, clipping and number formatting.
//! * [`header`] — title line and overview stat blocks.
//! * [`devices`] — host groups and aligned device rows.
//! * [`charts`] — the history panel.

pub mod charts;
pub mod devices;
pub mod header;
pub mod line;

use std::collections::HashSet;
use std::io::Write;

use crate::app_state::SortCriteria;
use crate::probes::HostProbes;
use crate::storage::info::StorageInfo;
use crate::ui::consolidated::history::SeriesHistory;
use crate::ui::consolidated::model::{ConsolidatedModel, DeviceRow};
use crate::ui::consolidated::pipeline::PipelineStatus;
use crate::ui::theme::MUTED;

use devices::Columns;
use line::{Line, Writer};

/// Everything the All tab body reads.
pub struct AllTabInputs<'a> {
    pub model: &'a ConsolidatedModel,
    pub series: &'a SeriesHistory,
    pub host_probes: &'a [HostProbes],
    /// Icculis pipeline state (`--icculis`), for the compact strip.
    pub pipeline: Option<&'a PipelineStatus>,
    pub show_details: bool,
    pub sort: SortCriteria,
    /// Devices to skip from the top (↑/↓ scrolling).
    pub scroll: usize,
    /// Device UUIDs outside the active filter: dimmed, or hidden when
    /// `hide_filtered` is set.
    pub filtered_out: &'a HashSet<String>,
    pub hide_filtered: bool,
    /// Collection interval, to label the history span.
    pub interval_secs: u64,
    pub now_unix: u64,
    /// A host tab: only this host's devices, their details, and its
    /// disks. `None` for the All tab.
    pub host: Option<&'a str>,
    pub storage: &'a [StorageInfo],
}

/// The shared host / device model for the current remote state.
pub fn model_for(state: &crate::app_state::AppState) -> ConsolidatedModel {
    ConsolidatedModel::build(&crate::ui::consolidated::model::ModelSources {
        gpu_info: &state.gpu_info,
        cpu_info: &state.cpu_info,
        memory_info: &state.memory_info,
        host_probes: &state.host_probes,
        host_order: &state.tabs,
        connection_status: &state.connection_status,
    })
}

/// Rows above the tab body in remote mode: the header, the tab strip and
/// its rule.
pub fn remote_header_rows(state: &crate::app_state::AppState, cols: u16) -> u16 {
    header::header_rows(&model_for(state), cols) + 2
}

/// Render the All tab body (or, with `inputs.host`, a host tab body) into
/// `out` using at most `rows` rows.
pub fn render_all_tab<W: Write>(out: &mut W, inputs: &AllTabInputs<'_>, cols: u16, rows: u16) {
    let mut w = Writer {
        out,
        cols: cols as usize,
        rows_left: rows as usize,
    };
    let host_model;
    let model = match inputs.host {
        Some(host) => {
            host_model = inputs.model.only_host(host);
            &host_model
        }
        None => inputs.model,
    };

    let mut table = device_table(inputs, model, w.cols);
    if inputs.host.is_some() {
        let disks = storage_lines(inputs, w.cols);
        if !disks.is_empty() {
            table.push(Line::default());
            table.extend(disks);
        }
    }
    let pipeline: Vec<Line> = inputs
        .pipeline
        .filter(|_| inputs.host.is_none())
        .map(|p| {
            crate::ui::consolidated::pipeline_render::compact_lines(
                model,
                p,
                inputs.host_probes,
                w.cols,
                inputs.now_unix,
            )
        })
        .unwrap_or_default();
    let pipeline_rows = if pipeline.is_empty() {
        0
    } else {
        pipeline.len() + 1
    };

    // The table gets the rows it needs, less what the pipeline strip
    // takes; a table taller than that scrolls.
    let table_budget = w.rows_left.saturating_sub(pipeline_rows).max(1);
    if table.len() > table_budget {
        let shown = table_budget.saturating_sub(1);
        let hidden = table.len() - shown;
        for line in table.into_iter().take(shown) {
            w.emit(line);
        }
        let mut more = Line::default();
        more.text(&format!("   ↓ {hidden} more lines (↑/↓ to scroll)"), MUTED);
        w.emit(more);
    } else {
        for line in table {
            w.emit(line);
        }
    }

    if !pipeline.is_empty() {
        w.blank();
        for line in pipeline {
            w.emit(line);
        }
    }

    let has_devices = model.hosts.iter().any(|h| !h.devices.is_empty());
    if has_devices && w.rows_left > charts::MIN_ROWS {
        w.blank();
        charts::render_history(&mut w, model, inputs.series, inputs.interval_secs);
    }
}

/// A host tab's disks: mount point, a usage bar, used / total.
fn storage_lines(inputs: &AllTabInputs<'_>, cols: usize) -> Vec<Line> {
    let Some(host) = inputs.host else {
        return Vec::new();
    };
    let disks: Vec<&StorageInfo> = inputs
        .storage
        .iter()
        .filter(|s| s.host_id == host && s.total_bytes > 0)
        .collect();
    if disks.is_empty() {
        return Vec::new();
    }
    let name_w = disks
        .iter()
        .map(|d| d.mount_point.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(12, 32);
    let bar_w = cols.saturating_sub(3 + name_w + 2 + 24).clamp(0, 40);
    let mut out = Vec::with_capacity(disks.len() + 1);
    let mut header = Line::default();
    header
        .text("   ", MUTED)
        .cell("DISK", name_w + 2, MUTED)
        .cell("USED", bar_w + 24, MUTED);
    out.push(header);
    for d in disks {
        let used = d.total_bytes.saturating_sub(d.available_bytes);
        let ratio = used as f64 / d.total_bytes as f64;
        let level = crate::ui::theme::mem_level(ratio);
        let mut line = Line::default();
        line.text("   ", MUTED)
            .cell(&d.mount_point, name_w + 2, crate::ui::theme::TEXT);
        if bar_w >= 8 {
            line.bar(bar_w, ratio, level.color()).text(" ", MUTED);
        }
        line.rcell(
            &format!("{} / {}", fmt_bytes(used), fmt_bytes(d.total_bytes)),
            18,
            crate::ui::theme::SUBTLE,
        )
        .rcell(&format!("{:.0}%", ratio * 100.0), 5, level.value_color());
        out.push(line);
    }
    out
}

fn fmt_bytes(b: u64) -> String {
    let g = b as f64 / line::GIB;
    if g >= 1024.0 {
        format!("{:.1} TiB", g / 1024.0)
    } else {
        format!("{g:.0} GiB")
    }
}

/// Column header, then per host: host line, device rows and (with `x`, or
/// always on a host tab) details lines, with a blank line between hosts.
fn device_table(inputs: &AllTabInputs<'_>, model: &ConsolidatedModel, cols: usize) -> Vec<Line> {
    let show_details = inputs.show_details || inputs.host.is_some();
    let longest = model
        .hosts
        .iter()
        .flat_map(|h| &h.devices)
        .map(|d| d.label().chars().count())
        .max()
        .unwrap_or(0);
    let c = Columns::for_width(cols, longest);
    let mut lines = vec![devices::column_header(&c)];

    let mut skip = inputs.scroll;
    for (i, host) in model.hosts.iter().enumerate() {
        let mut shown: Vec<&DeviceRow> = host
            .devices
            .iter()
            .filter(|d| !(inputs.hide_filtered && inputs.filtered_out.contains(&d.uuid)))
            .collect();
        sort_devices(&mut shown, inputs.sort);
        // Scrolling skips whole devices from the top; a host whose devices
        // all scrolled away drops out with them.
        let skipped = skip.min(shown.len());
        skip -= skipped;
        if skipped > 0 && skipped == shown.len() {
            continue;
        }
        if i > 0 && lines.len() > 1 {
            lines.push(Line::default());
        }
        lines.push(devices::host_line(host, cols));
        for d in shown.into_iter().skip(skipped) {
            let dim = !host.connected || inputs.filtered_out.contains(&d.uuid);
            lines.push(devices::device_line(d, &c, dim));
            if show_details && let Some(details) = devices::details_line(d, host) {
                lines.push(details);
            }
        }
    }
    lines
}

fn sort_devices(devices: &mut [&DeviceRow], sort: SortCriteria) {
    let key = |d: &DeviceRow| -> f64 {
        match sort {
            SortCriteria::Utilization => d.utilization.unwrap_or(-1.0),
            SortCriteria::GpuMemory => d.used_memory as f64,
            SortCriteria::Power => d.power_watts.unwrap_or(-1.0),
            SortCriteria::Temperature => d.temperature_c.map_or(-1.0, f64::from),
            _ => 0.0,
        }
    };
    // Stable: equal keys (and the default order) keep device-index order.
    devices.sort_by(|a, b| key(b).total_cmp(&key(a)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consolidated::model::tests::{mac_and_box, model_of};
    use crate::ui::text::display_width;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn render(cols: u16, rows: u16, details: bool, scroll: usize) -> String {
        render_tab(cols, rows, details, scroll, None)
    }

    fn render_tab(
        cols: u16,
        rows: u16,
        details: bool,
        scroll: usize,
        host: Option<&str>,
    ) -> String {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let mut series = SeriesHistory::default();
        for _ in 0..20 {
            series.record_collection(&gpus, &[]);
        }
        let empty = HashSet::new();
        let storage = [StorageInfo {
            mount_point: "/models".to_string(),
            total_bytes: 1 << 40,
            available_bytes: 1 << 38,
            host_id: "10.10.10.1:9090".to_string(),
            hostname: "vllm".to_string(),
            index: 0,
        }];
        let inputs = AllTabInputs {
            host,
            storage: &storage,
            model: &model,
            series: &series,
            host_probes: &[],
            pipeline: None,
            show_details: details,
            sort: SortCriteria::Default,
            scroll,
            filtered_out: &empty,
            hide_filtered: false,
            interval_secs: 3,
            now_unix: 0,
        };
        let mut buf = Vec::new();
        render_all_tab(&mut buf, &inputs, cols, rows);
        strip(&String::from_utf8(buf).unwrap())
    }

    #[test]
    fn host_tab_shows_one_host_with_details_and_disks() {
        let out = render_tab(160, 40, false, 0, Some("10.10.10.1:9090"));
        assert!(!out.contains("ians-Mac-Studio"), "{out}");
        assert!(out.contains("● vllm"), "{out}");
        assert!(out.contains("/models"), "{out}");
        assert!(out.contains("768 GiB / 1.0 TiB"), "{out}");
        assert!(out.contains("75%"), "{out}");
        assert!(out.contains("History"), "{out}");
        assert!(!out.contains("disks: "), "{out}");
        assert!(!render(160, 40, false, 0).contains("/models"));
    }

    #[test]
    fn groups_devices_under_their_host_and_fills_the_rest_with_history() {
        let out = render(160, 38, false, 0);
        let mac = out.find("● ians-Mac-Studio").unwrap();
        let apple = out.find("Apple M5 Ultra · 80-core GPU").unwrap();
        let vllm = out.find("● vllm").unwrap();
        let rtx = out.find("NVIDIA RTX PRO 6000 Blackwell #0").unwrap();
        assert!(mac < apple && apple < vllm && vllm < rtx, "{out}");
        assert!(out.contains("History"), "{out}");
        assert!(out.matches("\r\n").count() <= 38);
        // The history panel takes what the table leaves, give or take the
        // rounding of chart heights.
        assert!(out.matches("\r\n").count() >= 35, "{out}");
        for line in out.split("\r\n") {
            assert!(display_width(line) <= 160, "{line:?}");
        }
        assert!(!out.contains("slowdown"), "{out}");
    }

    #[test]
    fn details_toggle_adds_a_line_per_device() {
        let out = render(160, 38, true, 0);
        assert!(out.contains("ANE 1.5 W"), "{out}");
        assert!(!out.contains("unreachable"), "{out}");
    }

    #[test]
    fn scrolling_skips_devices_from_the_top() {
        let out = render(160, 38, false, 1);
        assert!(!out.contains("● ians-Mac-Studio"), "{out}");
        assert!(out.contains("● vllm"), "{out}");
    }

    #[test]
    fn short_terminals_keep_the_budget() {
        for rows in [3u16, 6, 10, 20] {
            for cols in [60u16, 80, 100, 120] {
                let out = render(cols, rows, true, 0);
                assert!(
                    out.matches("\r\n").count() <= rows as usize,
                    "{cols}x{rows}"
                );
                for line in out.split("\r\n") {
                    assert!(display_width(line) <= cols as usize, "{cols}: {line:?}");
                }
            }
        }
    }
}
