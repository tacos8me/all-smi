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

/// Render the All tab body into `out` using at most `rows` rows.
pub fn render_all_tab<W: Write>(out: &mut W, inputs: &AllTabInputs<'_>, cols: u16, rows: u16) {
    let mut w = Writer {
        out,
        cols: cols as usize,
        rows_left: rows as usize,
    };

    let table = device_table(inputs, w.cols);
    let pipeline: Vec<Line> = inputs
        .pipeline
        .map(|p| {
            crate::ui::consolidated::pipeline_render::compact_lines(
                inputs.model,
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

    if w.rows_left > charts::MIN_ROWS {
        w.blank();
        charts::render_history(&mut w, inputs.model, inputs.series, inputs.interval_secs);
    }
}

/// Column header, then per host: host line, device rows and (with `x`)
/// details lines, with a blank line between hosts.
fn device_table(inputs: &AllTabInputs<'_>, cols: usize) -> Vec<Line> {
    let longest = inputs
        .model
        .hosts
        .iter()
        .flat_map(|h| &h.devices)
        .map(|d| d.label().chars().count())
        .max()
        .unwrap_or(0);
    let c = Columns::for_width(cols, longest);
    let mut lines = vec![devices::column_header(&c)];

    let mut skip = inputs.scroll;
    for (i, host) in inputs.model.hosts.iter().enumerate() {
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
            if inputs.show_details
                && let Some(details) = devices::details_line(d, host)
            {
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
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let mut series = SeriesHistory::default();
        for _ in 0..20 {
            series.record_collection(&gpus, &[]);
        }
        let empty = HashSet::new();
        let inputs = AllTabInputs {
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
