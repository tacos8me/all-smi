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

//! Renderer for the Consolidated tab.
//!
//! One table row per accelerator (host, device, utilization, memory with
//! its kind, power, temperature, clock, and utilization / power
//! sparklines), a combined total, then the host probes. Columns and
//! sparklines shrink or drop with the terminal width, and output stops at
//! the row budget the frame renderer hands in, so the tab never scrolls
//! the function-key footer off screen. Colors come from [`crate::ui::theme`].

use std::collections::HashMap;
use std::io::Write;

use crate::app_state::ConnectionStatus;
use crate::device::{CpuInfo, GpuInfo, MemoryInfo};
use crate::probes::HostProbes;
use crate::ui::braille::sparkline_braille;
use crate::ui::cluster::charts;
use crate::ui::cluster::line::{
    GIB, Line, Writer, fit_keeping_index, fmt_duration, fmt_rate, fmt_watts, plural,
};
use crate::ui::theme::{self, ACCENT, CRIT, MUTED, OK, SUBTLE, TEXT};

use super::history::{self, ConsolidatedState, SeriesHistory};
use super::model::{ConsolidatedModel, DeviceRow, HostSection, ModelSources};
use super::pipeline_render;
use super::pipeline_view::PipelineView;

/// Width of the second column of the probe and pipeline rows (host label,
/// or host label and port).
pub(crate) const SUBJECT: usize = 22;

/// Borrowed inputs so the renderer stays independent of `RenderSnapshot`.
pub struct ConsolidatedInputs<'a> {
    pub gpu_info: &'a [GpuInfo],
    pub cpu_info: &'a [CpuInfo],
    pub memory_info: &'a [MemoryInfo],
    /// Tab strip, used for host order.
    pub tabs: &'a [String],
    pub connection_status: &'a HashMap<String, ConnectionStatus>,
    pub host_probes: &'a [HostProbes],
    /// Device, total and link series (`AppState::device_series`).
    pub series: &'a SeriesHistory,
    pub state: &'a ConsolidatedState,
    /// Wall-clock seconds, for "held for" durations.
    pub now_unix: u64,
    /// Collection interval, to label the history span.
    pub interval_secs: u64,
}

/// Render the tab body into `out` using at most `rows` terminal rows.
pub fn render_consolidated_tab<W: Write>(
    out: &mut W,
    inputs: &ConsolidatedInputs<'_>,
    cols: u16,
    rows: u16,
) {
    let model = ConsolidatedModel::build(&ModelSources {
        gpu_info: inputs.gpu_info,
        cpu_info: inputs.cpu_info,
        memory_info: inputs.memory_info,
        host_probes: inputs.host_probes,
        host_order: inputs.tabs,
        connection_status: inputs.connection_status,
    });
    let layout = Columns::for_width(cols as usize);
    let mut w = Writer {
        out,
        cols: cols as usize,
        rows_left: rows as usize,
    };

    render_title(&mut w, &model);
    render_device_header(&mut w, &layout);
    for host in &model.hosts {
        render_host(&mut w, &layout, host, inputs.series);
    }
    w.rule();
    render_totals(&mut w, &layout, &model, inputs.series);
    render_probes(&mut w, &layout, &model, inputs);

    // The rows left over hold the same history panel as the All tab.
    if w.rows_left > charts::MIN_ROWS {
        w.blank();
        charts::render_history(&mut w, &model, inputs.series, inputs.interval_secs);
    }
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn render_title<W: Write>(w: &mut Writer<'_, W>, model: &ConsolidatedModel) {
    let t = &model.totals;
    let mut title = Line::default();
    title.text("Consolidated", ACCENT);
    let mut note = Line::default();
    note.text(
        &format!(
            "{} on {} ({} up) as one system",
            plural(t.devices, "accelerator"),
            plural(t.hosts_total, "host"),
            t.hosts_up
        ),
        MUTED,
    );
    w.heading(&title, Some(&note));
}

fn render_device_header<W: Write>(w: &mut Writer<'_, W>, c: &Columns) {
    let mut line = Line::default();
    line.cell("HOST", c.host, MUTED)
        .cell("DEVICE", c.device, MUTED)
        .rcell("UTIL", c.util, MUTED)
        .cell("MEMORY", c.mem, MUTED)
        .rcell("POWER", c.power, MUTED)
        .rcell("TEMP", c.temp, MUTED);
    if c.clock > 0 {
        line.rcell("CLOCK", c.clock, MUTED);
    }
    if c.spark_util > 0 {
        line.cell(" UTIL HISTORY", c.spark_util + 1, MUTED);
    }
    if c.spark_power > 0 {
        line.cell(" POWER HISTORY", c.spark_power + 1, MUTED);
    }
    w.emit(line);
}

fn render_host<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    host: &HostSection,
    series: &SeriesHistory,
) {
    let host_color = if host.connected { TEXT } else { CRIT };
    if host.devices.is_empty() {
        let mut line = Line::default();
        line.cell(&host.label, c.host, host_color);
        let reason = if host.connected {
            "connected, no accelerators reported".to_string()
        } else {
            format!(
                "unreachable: {}",
                host.last_error.as_deref().unwrap_or("no response yet")
            )
        };
        line.text(&reason, MUTED);
        w.emit(line);
        return;
    }
    for (i, device) in host.devices.iter().enumerate() {
        let label = if i == 0 { host.label.as_str() } else { "" };
        let mut line = Line::default();
        line.cell(label, c.host, host_color);
        render_device_cells(&mut line, c, device, host.connected, series);
        w.emit(line);

        let extras = device_extras(device, host);
        if !extras.is_empty() {
            let mut sub = Line::default();
            sub.cell("", c.host, MUTED).text(&extras, MUTED);
            w.emit(sub);
        }
    }
}

fn render_device_cells(
    line: &mut Line,
    c: &Columns,
    d: &DeviceRow,
    connected: bool,
    series: &SeriesHistory,
) {
    let tone = |color| if connected { color } else { MUTED };
    let room = c.device.saturating_sub(1);
    let mut name = d.label();
    if !connected {
        name.push_str(" (stale)");
    }
    line.cell(&fit_keeping_index(&name, room), c.device, tone(TEXT));

    match d.utilization {
        Some(u) => line.rcell(
            &format!("{u:.1}%"),
            c.util,
            tone(theme::util_level(u).value_color()),
        ),
        None => line.rcell("n/a", c.util, MUTED),
    };

    let mem = format!(
        "{:.1}/{:.1} GiB {}",
        d.used_memory as f64 / GIB,
        d.total_memory as f64 / GIB,
        d.memory_kind.label()
    );
    line.cell(
        &mem,
        c.mem,
        tone(theme::mem_level(d.memory_ratio()).value_color()),
    );

    let power = match (d.power_watts, d.power_limit_watts) {
        (Some(p), Some(limit)) => format!("{}/{limit:.0} W", fmt_watts(p).trim_end_matches(" W")),
        (Some(p), None) => fmt_watts(p),
        (None, _) => "n/a".to_string(),
    };
    let power_color = d
        .power_ratio()
        .map_or(TEXT, |r| theme::power_level(r).value_color());
    line.rcell(&power, c.power, tone(power_color));
    match d.temperature_c {
        Some(t) => line.rcell(
            &format!("{t}°C"),
            c.temp,
            tone(theme::temp_level(t, d.slowdown_c).value_color()),
        ),
        None => line.rcell("n/a", c.temp, MUTED),
    };
    if c.clock > 0 {
        let clock = d
            .frequency_mhz
            .map_or_else(|| "n/a".to_string(), |f| format!("{f} MHz"));
        line.rcell(&clock, c.clock, tone(SUBTLE));
    }
    if c.spark_util > 0 {
        let data = series.values(&history::util_key(&d.uuid));
        line.text(" ", MUTED).text(
            &sparkline_braille(&data, c.spark_util, Some((0.0, 100.0))),
            tone(ACCENT),
        );
    }
    if c.spark_power > 0 {
        let data = series.values(&history::power_key(&d.uuid));
        let ceiling = d
            .power_limit_watts
            .unwrap_or(0.0)
            .max(data.iter().copied().fold(1.0, f64::max));
        line.text(" ", MUTED).text(
            &sparkline_braille(&data, c.spark_power, Some((0.0, ceiling))),
            tone(SUBTLE),
        );
    }
}

/// Second line under an Apple Silicon device: the SoC rails that share its
/// power budget.
fn device_extras(d: &DeviceRow, host: &HostSection) -> String {
    let mut parts = Vec::new();
    if let Some(ane) = d.ane_watts {
        parts.push(format!("ANE {}", fmt_watts(ane)));
    }
    if d.ane_watts.is_some()
        && let Some(cpu) = host.cpu_power_watts
    {
        parts.push(format!("CPU {}", fmt_watts(cpu)));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("└ {}", parts.join(" · "))
}

fn render_totals<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    series: &SeriesHistory,
) {
    let t = &model.totals;
    let mut line = Line::default();
    line.cell("TOTAL", c.host, ACCENT)
        .cell(&plural(t.devices, "accelerator"), c.device, TEXT);
    match t.avg_utilization {
        Some(u) => line.rcell(&format!("{u:.1}%"), c.util, TEXT),
        None => line.rcell("n/a", c.util, MUTED),
    };
    let pct = if t.memory_total() > 0 {
        t.memory_used() as f64 * 100.0 / t.memory_total() as f64
    } else {
        0.0
    };
    line.cell(
        &format!(
            "{:.1}/{:.1} GiB {pct:.0}%",
            t.memory_used() as f64 / GIB,
            t.memory_total() as f64 / GIB
        ),
        c.mem,
        TEXT,
    )
    .rcell(&fmt_watts(t.power_watts()), c.power, TEXT)
    .rcell("", c.temp, TEXT);
    if c.clock > 0 {
        line.rcell("", c.clock, TEXT);
    }
    if c.spark_util > 0 {
        let data = series.values(history::TOTAL_UTIL_KEY);
        line.text(" ", MUTED).text(
            &sparkline_braille(&data, c.spark_util, Some((0.0, 100.0))),
            ACCENT,
        );
    }
    if c.spark_power > 0 {
        let data = series.values(history::TOTAL_POWER_KEY);
        // Against the summed board limits when the devices report them,
        // so the total reads as a share of what the machines can draw.
        let ceiling = data
            .iter()
            .copied()
            .fold(t.power_limit_watts.max(1.0), f64::max);
        line.text(" ", MUTED).text(
            &sparkline_braille(&data, c.spark_power, Some((0.0, ceiling))),
            SUBTLE,
        );
    }
    w.emit(line);

    // Break the pool down by memory kind, naming only kinds present.
    let mut parts = Vec::new();
    if t.unified_total > 0 {
        parts.push(format!(
            "unified {:.1}/{:.1} GiB, SoC GPU {}",
            t.unified_used as f64 / GIB,
            t.unified_total as f64 / GIB,
            fmt_watts(t.unified_power_watts)
        ));
    }
    if t.dedicated_total > 0 {
        parts.push(format!(
            "VRAM {:.1}/{:.1} GiB, discrete GPUs {}",
            t.dedicated_used as f64 / GIB,
            t.dedicated_total as f64 / GIB,
            fmt_watts(t.dedicated_power_watts)
        ));
    }
    if !parts.is_empty() {
        let mut split = Line::default();
        split
            .cell("", c.host, MUTED)
            .text(&parts.join(" · "), MUTED);
        w.emit(split);
    }
}

/// Link throughput and lock holders from the opt-in exporter probes.
fn render_probes<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    inputs: &ConsolidatedInputs<'_>,
) {
    let has_links = inputs.host_probes.iter().any(|p| !p.interfaces.is_empty());
    let has_locks = inputs.host_probes.iter().any(|p| !p.locks.is_empty());
    if let Some(pipeline) = inputs.state.pipeline.as_ref() {
        // The pipeline panel (`--icculis`): the split as a diagram, then
        // the engine, llama-swap, lock holder and link it depends on.
        w.blank();
        pipeline_render::render_pipeline_heading(w, pipeline);
        let view = PipelineView::build(model, pipeline, inputs.host_probes);
        pipeline_render::render_diagram(w, pipeline, &view);
        pipeline_render::render_engine_details(w, c, model, pipeline, &inputs.state.history);
        pipeline_render::render_swap(w, c, model, pipeline);
        if has_locks {
            render_locks(w, c, model, inputs);
        }
        if has_links {
            render_links(w, c, model, inputs);
        }
        return;
    }
    if !has_links && !has_locks {
        return;
    }
    w.blank();
    if has_links {
        render_links(w, c, model, inputs);
    }
    if has_locks {
        render_locks(w, c, model, inputs);
    }
}

pub(crate) fn render_links<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    inputs: &ConsolidatedInputs<'_>,
) {
    let spark = if w.cols >= c.host + SUBJECT + 52 {
        ((w.cols - c.host - SUBJECT - 46) / 2).clamp(6, 30)
    } else {
        0
    };
    for host in ordered_probes(model, inputs.host_probes) {
        for iface in &host.interfaces {
            let mut line = Line::default();
            line.cell("LINK", c.host, MUTED)
                .cell(&model.host_label(&host.host_id), SUBJECT, TEXT)
                .cell(&iface.interface, 16, SUBTLE)
                .text("↓ ", MUTED)
                .rcell(&fmt_rate(iface.rx_bytes_per_sec), 11, TEXT)
                .text("  ↑ ", MUTED)
                .rcell(&fmt_rate(iface.tx_bytes_per_sec), 11, TEXT);
            if spark > 0 {
                let rx = inputs
                    .series
                    .values(&history::net_rx_key(&host.host_id, &iface.interface));
                let tx = inputs
                    .series
                    .values(&history::net_tx_key(&host.host_id, &iface.interface));
                // One shared ceiling so rx and tx read on the same scale.
                let ceiling = rx.iter().chain(&tx).copied().fold(1_000_000.0, f64::max);
                line.text("  ", MUTED)
                    .text(&sparkline_braille(&rx, spark, Some((0.0, ceiling))), ACCENT)
                    .text(" ", MUTED)
                    .text(&sparkline_braille(&tx, spark, Some((0.0, ceiling))), SUBTLE);
            }
            w.emit(line);
        }
    }
}

pub(crate) fn render_locks<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    inputs: &ConsolidatedInputs<'_>,
) {
    for host in ordered_probes(model, inputs.host_probes) {
        for lock in &host.locks {
            let mut line = Line::default();
            line.cell("LOCK", c.host, MUTED)
                .cell(&model.host_label(&host.host_id), SUBJECT, TEXT)
                .text(&tilde_home(&lock.path), SUBTLE)
                .text("  ", MUTED);
            if lock.holders.is_empty() {
                line.text("free", OK);
            } else {
                let who = lock
                    .holders
                    .iter()
                    .map(|h| format!("{} (pid {})", h.command, h.pid))
                    .collect::<Vec<_>>()
                    .join(", ");
                line.text("held by ", MUTED).text(&who, TEXT);
                if let Some(since) = lock.since_unix {
                    line.text(
                        &format!(
                            " for {}",
                            fmt_duration(inputs.now_unix.saturating_sub(since))
                        ),
                        MUTED,
                    );
                }
            }
            w.emit(line);
        }
    }
}

/// Probe rows follow the device table's host order; scrapes land in
/// whatever order the hosts answered.
fn ordered_probes<'a>(model: &ConsolidatedModel, probes: &'a [HostProbes]) -> Vec<&'a HostProbes> {
    let mut ordered: Vec<&HostProbes> = probes.iter().collect();
    ordered.sort_by_key(|p| {
        model
            .hosts
            .iter()
            .position(|h| h.host_id == p.host_id)
            .unwrap_or(usize::MAX)
    });
    ordered
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// Column widths for the device table at a given terminal width.
pub(crate) struct Columns {
    pub host: usize,
    pub device: usize,
    pub util: usize,
    pub mem: usize,
    pub power: usize,
    pub temp: usize,
    pub clock: usize,
    pub spark_util: usize,
    pub spark_power: usize,
}

impl Columns {
    /// Narrow terminals (80 columns) keep every value column and drop the
    /// clock and sparklines; wider ones widen the text columns first and
    /// give whatever is left to one or two sparklines.
    pub(crate) fn for_width(cols: usize) -> Self {
        let mut c = if cols >= 140 {
            Self::base(17, 34, 8, 25, 12, 10)
        } else if cols >= 120 {
            Self::base(17, 30, 8, 25, 12, 10)
        } else if cols >= 100 {
            Self::base(16, 26, 7, 24, 11, 10)
        } else {
            Self::base(12, 20, 7, 24, 11, 0)
        };
        let fixed = c.host + c.device + c.util + c.mem + c.power + c.temp + c.clock;
        let spare = cols.saturating_sub(fixed);
        if spare >= 2 * 13 {
            let each = (spare / 2 - 1).min(40);
            c.spark_util = each;
            c.spark_power = each;
        } else if spare >= 9 {
            c.spark_util = (spare - 1).min(40);
        }
        c
    }

    fn base(
        host: usize,
        device: usize,
        util: usize,
        mem: usize,
        power: usize,
        clock: usize,
    ) -> Self {
        Self {
            host,
            device,
            util,
            mem,
            power,
            temp: 6,
            clock,
            spark_util: 0,
            spark_power: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// `/Users/ian/llm/x` → `~/llm/x` (and `/home/<user>/…`): the owner is
/// implied by the host column, and the full path crowds out the holder.
fn tilde_home(path: &str) -> String {
    for root in ["/Users/", "/home/"] {
        if let Some(rest) = path.strip_prefix(root)
            && let Some((_user, tail)) = rest.split_once('/')
        {
            return format!("~/{tail}");
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::lock::{LockHolder, LockSample};
    use crate::probes::net::NetInterfaceSample;
    use crate::ui::consolidated::model::tests::mac_and_box;
    use crate::ui::text::display_width;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
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

    fn render(cols: u16, rows: u16, probes: &[HostProbes], state: &ConsolidatedState) -> String {
        render_with(cols, rows, probes, state, &SeriesHistory::default())
    }

    fn render_with(
        cols: u16,
        rows: u16,
        probes: &[HostProbes],
        state: &ConsolidatedState,
        series: &SeriesHistory,
    ) -> String {
        let (gpus, tabs, statuses) = mac_and_box();
        let inputs = ConsolidatedInputs {
            gpu_info: &gpus,
            cpu_info: &[],
            memory_info: &[],
            tabs: &tabs,
            connection_status: &statuses,
            host_probes: probes,
            series,
            state,
            now_unix: 1_000_750,
            interval_secs: 3,
        };
        let mut buf = Vec::new();
        render_consolidated_tab(&mut buf, &inputs, cols, rows);
        strip_ansi(&String::from_utf8(buf).unwrap())
    }

    fn probes() -> Vec<HostProbes> {
        vec![HostProbes {
            host_id: "10.10.10.2:9090".to_string(),
            interfaces: vec![NetInterfaceSample {
                interface: "en0".to_string(),
                rx_bytes_per_sec: Some(1_250_000_000.0),
                tx_bytes_per_sec: Some(2_500.0),
                ..Default::default()
            }],
            locks: vec![LockSample {
                path: "/Users/ian/llm/locks/gpu.lock".to_string(),
                holders: vec![LockHolder {
                    pid: 49097,
                    command: "omlx-server".to_string(),
                }],
                since_unix: Some(1_000_000),
            }],
            ..Default::default()
        }]
    }

    #[test]
    fn renders_every_device_with_labeled_memory_and_totals() {
        let out = render(160, 40, &[], &ConsolidatedState::default());
        assert!(out.contains("3 accelerators on 2 hosts (2 up)"), "{out}");
        assert!(out.contains("ians-Mac-Studio"));
        assert!(out.contains("Apple M5 Ultra · 80-core GPU"), "{out}");
        assert!(out.contains("154.0/256.0 GiB unified"));
        assert!(out.contains("87.0/96.0 GiB VRAM"));
        assert!(out.contains("NVIDIA RTX PRO 6000 Blackwell #1"), "{out}");
        assert!(out.contains("100/600 W"));
        assert!(out.contains("└ ANE 1.5 W"));
        assert!(out.contains("328.0/448.0 GiB 73%"), "{out}");
        assert!(out.contains("212 W"));
        assert!(out.contains(
            "unified 154.0/256.0 GiB, SoC GPU 12.0 W · VRAM 174.0/192.0 GiB, discrete GPUs 200 W"
        ));
    }

    #[test]
    fn lines_never_exceed_the_terminal_width() {
        let state = ConsolidatedState::default();
        let mut series = SeriesHistory::default();
        let (gpus, _, _) = mac_and_box();
        for _ in 0..50 {
            series.record_collection(&gpus, &probes());
        }
        for cols in [80u16, 100, 120, 160, 220] {
            let out = render_with(cols, 60, &probes(), &state, &series);
            for line in out.split("\r\n") {
                assert!(
                    display_width(line) <= cols as usize,
                    "{cols} cols overflowed: {line:?}"
                );
            }
        }
    }

    #[test]
    fn row_budget_is_respected() {
        let out = render(160, 5, &probes(), &ConsolidatedState::default());
        assert_eq!(out.matches("\r\n").count(), 5);
    }

    #[test]
    fn probes_render_link_rates_and_lock_holder() {
        let out = render(160, 40, &probes(), &ConsolidatedState::default());
        assert!(out.contains("LINK"));
        assert!(out.contains("en0"));
        assert!(out.contains("1.25 GB/s"));
        assert!(out.contains("2.5 KB/s"));
        assert!(out.contains("~/llm/locks/gpu.lock"));
        assert!(
            out.contains("held by omlx-server (pid 49097) for 12m 30s"),
            "{out}"
        );
    }

    #[test]
    fn down_host_renders_reason() {
        let (gpus, tabs, mut statuses) = mac_and_box();
        let gpus: Vec<GpuInfo> = gpus
            .into_iter()
            .filter(|g| g.host_id != "10.10.10.2:9090")
            .collect();
        statuses
            .get_mut("10.10.10.2:9090")
            .unwrap()
            .mark_failure("Connection refused".to_string());
        let state = ConsolidatedState::default();
        let inputs = ConsolidatedInputs {
            gpu_info: &gpus,
            cpu_info: &[],
            memory_info: &[],
            tabs: &tabs,
            connection_status: &statuses,
            host_probes: &[],
            series: &SeriesHistory::default(),
            state: &state,
            now_unix: 0,
            interval_secs: 3,
        };
        let mut buf = Vec::new();
        render_consolidated_tab(&mut buf, &inputs, 160, 40);
        let out = strip_ansi(&String::from_utf8(buf).unwrap());
        assert!(out.contains("(1 up)"));
        assert!(out.contains("unreachable: Connection refused"), "{out}");
    }

    #[test]
    fn pipeline_panel_renders_diagram_engine_swap_lock_and_link() {
        use crate::ui::consolidated::pipeline::{
            EngineCache, EngineHealth, PipelineConfig, PipelineStatus, Probe, SwapModel,
        };
        let mut status = PipelineStatus::new(PipelineConfig::icculis(None, None));
        status.engine = Probe::Ok(EngineHealth {
            ok: Some(true),
            sessions: Some(2),
            connections: Some(1),
            gpu_job: Some(serde_json::Value::String("prefill_chunk".to_string())),
            gpu_job_s: Some(1.25),
            queued_jobs: Some(3),
            version: Some("d11dccf".to_string()),
            numerics: Some("og-s4.3".to_string()),
            uptime_s: Some(4227.0),
            cache: Some(EngineCache {
                lookups: Some(406),
                hits: Some(301),
                entries: Some(777),
                ..Default::default()
            }),
            ..Default::default()
        });
        status.prefill_elapsed = Some(std::time::Duration::from_secs(8));
        status.swap = Probe::Ok(vec![SwapModel {
            model: "ds41".to_string(),
            state: "ready".to_string(),
            name: "Icculus".to_string(),
        }]);
        let state = ConsolidatedState {
            pipeline: Some(status),
            ..Default::default()
        };
        let out = render(160, 60, &probes(), &state);
        assert!(
            out.contains("Icculis · DeepSeek-V4.1-Flash · original weights"),
            "{out}"
        );
        assert!(
            out.contains("engine ok · d11dccf · og-s4.3 · up 1h 10m"),
            "{out}"
        );
        assert!(out.contains("╭ RTX box · vllm"), "{out}");
        assert!(out.contains("╭ M5 Ultra · ians-Mac-Studio"), "{out}");
        assert!(out.contains("layers 0-19"), "{out}");
        assert!(out.contains("● PREFILL  8s · prefill_chunk 1.2s"), "{out}");
        assert!(out.contains("prefill state ▶"), "{out}");
        assert!(out.contains("vllm :10051"), "{out}");
        assert!(
            out.contains("sessions 2 · connections 1 · queue 3"),
            "{out}"
        );
        assert!(out.contains("prefix cache 74% of 406 lookups"), "{out}");
        assert!(out.contains("ians-Mac-Studio :8080"), "{out}");
        assert!(out.contains("ds41 ready  Icculus"));
        assert!(out.contains("held by omlx-server"));
        assert!(out.contains("1.25 GB/s"));
        for line in out.split("\r\n") {
            assert!(display_width(line) <= 160, "{line:?}");
        }

        let mut down = state.clone();
        let p = down.pipeline.as_mut().unwrap();
        p.engine = Probe::Err("connection failed".to_string());
        p.swap = Probe::Pending;
        let out = render(160, 60, &[], &down);
        assert!(
            out.contains("engine unreachable: connection failed"),
            "{out}"
        );
        assert!(out.contains("DOWN"), "{out}");
        assert!(out.contains("waiting for the first /running poll"));
    }

    #[test]
    fn probe_rows_follow_host_order() {
        let mut both = vec![HostProbes {
            host_id: "10.10.10.1:9090".to_string(),
            interfaces: vec![NetInterfaceSample {
                interface: "enp161s0f0np0".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }];
        both.extend(probes());
        let out = render(160, 60, &both, &ConsolidatedState::default());
        let mac = out.find("en0 ").unwrap();
        let rtx = out.find("enp161s0f0np0").unwrap();
        assert!(mac < rtx, "{out}");
    }

    #[test]
    fn paths() {
        assert_eq!(tilde_home("/home/ian/x.lock"), "~/x.lock");
        assert_eq!(tilde_home("/var/lock/x"), "/var/lock/x");
    }
}
