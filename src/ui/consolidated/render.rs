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
//! the function-key footer off screen.

use std::collections::HashMap;
use std::io::Write;

use crossterm::{
    queue,
    style::{Color, Print},
};

use crate::app_state::ConnectionStatus;
use crate::common::config::ThemeConfig;
use crate::device::{CpuInfo, GpuInfo};
use crate::probes::HostProbes;
use crate::ui::braille::sparkline_braille;
use crate::ui::text::{display_width, print_colored_text, truncate_to_width};

use super::history::{self, ConsolidatedState, SeriesHistory};
use super::model::{ConsolidatedModel, DeviceRow, HostSection};
use super::pipeline_render;

const GIB: f64 = (1u64 << 30) as f64;
/// Width of the second column of the probe and pipeline rows (host label,
/// or host label and port).
pub(crate) const SUBJECT: usize = 22;
const LABEL: Color = Color::DarkGrey;
const VALUE: Color = Color::White;
const HEADING: Color = Color::Cyan;

/// Borrowed inputs so the renderer stays independent of `RenderSnapshot`.
pub struct ConsolidatedInputs<'a> {
    pub gpu_info: &'a [GpuInfo],
    pub cpu_info: &'a [CpuInfo],
    /// Tab strip, used for host order.
    pub tabs: &'a [String],
    pub connection_status: &'a HashMap<String, ConnectionStatus>,
    pub host_probes: &'a [HostProbes],
    pub state: &'a ConsolidatedState,
    /// Wall-clock seconds, for "held for" durations.
    pub now_unix: u64,
}

/// Render the tab body into `out` using at most `rows` terminal rows.
pub fn render_consolidated_tab<W: Write>(
    out: &mut W,
    inputs: &ConsolidatedInputs<'_>,
    cols: u16,
    rows: u16,
) {
    let model = ConsolidatedModel::build(
        inputs.gpu_info,
        inputs.cpu_info,
        inputs.tabs,
        inputs.connection_status,
    );
    let layout = Columns::for_width(cols as usize);
    let mut w = Writer {
        out,
        cols: cols as usize,
        rows_left: rows as usize,
    };

    render_title(&mut w, &model);
    render_device_header(&mut w, &layout);
    for host in &model.hosts {
        render_host(&mut w, &layout, host, &inputs.state.history);
    }
    w.rule();
    render_totals(&mut w, &layout, &model, &inputs.state.history);
    render_probes(&mut w, &layout, &model, inputs);
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn render_title<W: Write>(w: &mut Writer<'_, W>, model: &ConsolidatedModel) {
    let t = &model.totals;
    let mut line = Line::default();
    line.text("Consolidated", HEADING).text(
        &format!(
            "  {} on {} ({} up) as one system",
            plural(t.devices, "accelerator"),
            plural(t.hosts_total, "host"),
            t.hosts_up
        ),
        VALUE,
    );
    w.emit(line);
}

fn render_device_header<W: Write>(w: &mut Writer<'_, W>, c: &Columns) {
    let mut line = Line::default();
    line.cell("HOST", c.host, LABEL)
        .cell("DEVICE", c.device, LABEL)
        .rcell("UTIL", c.util, LABEL)
        .cell("MEMORY", c.mem, LABEL)
        .rcell("POWER", c.power, LABEL)
        .rcell("TEMP", c.temp, LABEL);
    if c.clock > 0 {
        line.rcell("CLOCK", c.clock, LABEL);
    }
    if c.spark_util > 0 {
        line.cell(" UTIL HISTORY", c.spark_util + 1, LABEL);
    }
    if c.spark_power > 0 {
        line.cell(" POWER HISTORY", c.spark_power + 1, LABEL);
    }
    w.emit(line);
}

fn render_host<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    host: &HostSection,
    series: &SeriesHistory,
) {
    let host_color = if host.connected { VALUE } else { Color::Red };
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
        line.text(&reason, LABEL);
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
            sub.cell("", c.host, LABEL).text(&extras, LABEL);
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
    let dim = |color: Color| if connected { color } else { LABEL };
    let room = c.device.saturating_sub(1);
    let mut name = match d.core_count {
        Some(n) if display_width(&d.name) + 12 <= room => format!("{} ({n} cores)", d.name),
        _ => d.name.clone(),
    };
    if !connected {
        name.push_str(" (stale)");
    }
    line.cell(&fit_keeping_index(&name, room), c.device, dim(VALUE));

    match d.utilization {
        Some(u) => line.rcell(
            &format!("{u:.1}%"),
            c.util,
            dim(ThemeConfig::utilization_color(u).max_contrast()),
        ),
        None => line.rcell("n/a", c.util, LABEL),
    };

    let ratio = if d.total_memory > 0 {
        d.used_memory as f64 / d.total_memory as f64
    } else {
        0.0
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
        dim(ThemeConfig::progress_bar_color(ratio).max_contrast()),
    );

    let power = match (d.power_watts, d.power_limit_watts) {
        (Some(p), Some(limit)) => format!("{}/{limit:.0} W", fmt_watts(p).trim_end_matches(" W")),
        (Some(p), None) => fmt_watts(p),
        (None, _) => "n/a".to_string(),
    };
    line.rcell(&power, c.power, dim(VALUE));
    let temp = d
        .temperature_c
        .map_or_else(|| "n/a".to_string(), |t| format!("{t}°C"));
    line.rcell(&temp, c.temp, dim(VALUE));
    if c.clock > 0 {
        let clock = d
            .frequency_mhz
            .map_or_else(|| "n/a".to_string(), |f| format!("{f} MHz"));
        line.rcell(&clock, c.clock, dim(VALUE));
    }
    if c.spark_util > 0 {
        let data = series.values(&history::util_key(&d.uuid));
        line.text(" ", VALUE).text(
            &sparkline_braille(&data, c.spark_util, Some((0.0, 100.0))),
            dim(Color::Green),
        );
    }
    if c.spark_power > 0 {
        let data = series.values(&history::power_key(&d.uuid));
        let ceiling = d
            .power_limit_watts
            .unwrap_or(0.0)
            .max(data.iter().copied().fold(1.0, f64::max));
        line.text(" ", VALUE).text(
            &sparkline_braille(&data, c.spark_power, Some((0.0, ceiling))),
            dim(Color::Yellow),
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
    line.cell("TOTAL", c.host, HEADING)
        .cell(&plural(t.devices, "accelerator"), c.device, VALUE);
    match t.avg_utilization {
        Some(u) => line.rcell(&format!("{u:.1}%"), c.util, VALUE),
        None => line.rcell("n/a", c.util, LABEL),
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
        VALUE,
    )
    .rcell(&fmt_watts(t.power_watts()), c.power, VALUE)
    .rcell("", c.temp, VALUE);
    if c.clock > 0 {
        line.rcell("", c.clock, VALUE);
    }
    if c.spark_util > 0 {
        let data = series.values(history::TOTAL_UTIL_KEY);
        line.text(" ", VALUE).text(
            &sparkline_braille(&data, c.spark_util, Some((0.0, 100.0))),
            Color::Green,
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
        line.text(" ", VALUE).text(
            &sparkline_braille(&data, c.spark_power, Some((0.0, ceiling))),
            Color::Yellow,
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
            .cell("", c.host, LABEL)
            .text(&parts.join(" · "), LABEL);
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
        // The pipeline panel (`--icculus`) groups the engine and
        // llama-swap with the lock holder and link it depends on.
        w.blank();
        pipeline_render::render_pipeline_heading(w, pipeline);
        pipeline_render::render_engine(w, c, model, pipeline, &inputs.state.history);
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
            line.cell("LINK", c.host, HEADING)
                .cell(&model.host_label(&host.host_id), SUBJECT, VALUE)
                .cell(&iface.interface, 16, VALUE)
                .text("↓ ", LABEL)
                .rcell(&fmt_rate(iface.rx_bytes_per_sec), 11, Color::Green)
                .text("  ↑ ", LABEL)
                .rcell(&fmt_rate(iface.tx_bytes_per_sec), 11, Color::Yellow);
            if spark > 0 {
                let rx = inputs
                    .state
                    .history
                    .values(&history::net_rx_key(&host.host_id, &iface.interface));
                let tx = inputs
                    .state
                    .history
                    .values(&history::net_tx_key(&host.host_id, &iface.interface));
                // One shared ceiling so rx and tx read on the same scale.
                let ceiling = rx.iter().chain(&tx).copied().fold(1_000_000.0, f64::max);
                line.text("  ", VALUE)
                    .text(
                        &sparkline_braille(&rx, spark, Some((0.0, ceiling))),
                        Color::Green,
                    )
                    .text(" ", VALUE)
                    .text(
                        &sparkline_braille(&tx, spark, Some((0.0, ceiling))),
                        Color::Yellow,
                    );
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
            line.cell("LOCK", c.host, HEADING)
                .cell(&model.host_label(&host.host_id), SUBJECT, VALUE)
                .text(&tilde_home(&lock.path), VALUE)
                .text("  ", VALUE);
            if lock.holders.is_empty() {
                line.text("free", Color::Green);
            } else {
                let who = lock
                    .holders
                    .iter()
                    .map(|h| format!("{} (pid {})", h.command, h.pid))
                    .collect::<Vec<_>>()
                    .join(", ");
                line.text("held by ", LABEL).text(&who, Color::Yellow);
                if let Some(since) = lock.since_unix {
                    line.text(
                        &format!(
                            " for {}",
                            fmt_duration(inputs.now_unix.saturating_sub(since))
                        ),
                        LABEL,
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
// Layout and line assembly
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
        let mut c = if cols >= 120 {
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

#[derive(Default)]
pub(crate) struct Line {
    segs: Vec<(String, Color)>,
}

impl Line {
    /// Display width of everything added so far.
    pub(crate) fn width(&self) -> usize {
        self.segs.iter().map(|(t, _)| display_width(t)).sum()
    }

    pub(crate) fn text(&mut self, s: &str, color: Color) -> &mut Self {
        if !s.is_empty() {
            self.segs.push((s.to_string(), color));
        }
        self
    }

    /// Left-aligned cell of exactly `width` columns (one trailing space is
    /// reserved as the column gap).
    pub(crate) fn cell(&mut self, s: &str, width: usize, color: Color) -> &mut Self {
        let body = width.saturating_sub(1);
        let t = truncate_to_width(s, body);
        let pad = width.saturating_sub(display_width(&t));
        self.segs.push((format!("{t}{}", " ".repeat(pad)), color));
        self
    }

    /// Right-aligned cell of exactly `width` columns, gap on the right.
    pub(crate) fn rcell(&mut self, s: &str, width: usize, color: Color) -> &mut Self {
        let body = width.saturating_sub(1);
        let t = truncate_to_width(s, body);
        let pad = body.saturating_sub(display_width(&t));
        self.segs.push((format!("{}{t} ", " ".repeat(pad)), color));
        self
    }
}

pub(crate) struct Writer<'w, W: Write> {
    pub out: &'w mut W,
    pub cols: usize,
    pub rows_left: usize,
}

impl<W: Write> Writer<'_, W> {
    /// Write one line clipped to the terminal width. Lines past the row
    /// budget are dropped.
    pub(crate) fn emit(&mut self, line: Line) {
        if self.rows_left == 0 {
            return;
        }
        self.rows_left -= 1;
        let mut used = 0;
        for (text, color) in line.segs {
            if used >= self.cols {
                break;
            }
            let t = truncate_to_width(&text, self.cols - used);
            used += display_width(&t);
            print_colored_text(self.out, &t, color, None, None);
        }
        queue!(self.out, Print("\r\n")).ok();
    }

    pub(crate) fn blank(&mut self) {
        self.emit(Line::default());
    }

    pub(crate) fn rule(&mut self) {
        let mut line = Line::default();
        line.text(&"─".repeat(self.cols), LABEL);
        self.emit(line);
    }
}

/// Keep dark greys readable on the value columns: the theme maps idle
/// readings to `DarkGrey`, which would make a live number look disabled.
trait MaxContrast {
    fn max_contrast(self) -> Color;
}

impl MaxContrast for Color {
    fn max_contrast(self) -> Color {
        if self == Color::DarkGrey { VALUE } else { self }
    }
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Truncate a device name to `width`, keeping a trailing ` #<index>` so
/// identical boards stay distinguishable on narrow terminals.
fn fit_keeping_index(name: &str, width: usize) -> String {
    if display_width(name) <= width {
        return name.to_string();
    }
    match name.rfind(" #") {
        Some(pos) if display_width(&name[pos..]) < width => {
            let suffix = &name[pos..];
            let head = truncate_to_width(&name[..pos], width - display_width(suffix));
            format!("{}{suffix}", head.trim_end())
        }
        _ => truncate_to_width(name, width).into_owned(),
    }
}

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

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

pub(crate) fn fmt_watts(w: f64) -> String {
    if w >= 1000.0 {
        format!("{:.2} kW", w / 1000.0)
    } else if w >= 100.0 {
        format!("{w:.0} W")
    } else if w >= 1.0 {
        format!("{w:.1} W")
    } else {
        format!("{w:.2} W")
    }
}

/// Decimal network units, as link speeds are quoted.
pub(crate) fn fmt_rate(bytes_per_sec: Option<f64>) -> String {
    let Some(b) = bytes_per_sec else {
        return "n/a".to_string();
    };
    if b >= 1e9 {
        format!("{:.2} GB/s", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.1} KB/s", b / 1e3)
    } else {
        format!("{b:.0} B/s")
    }
}

pub(crate) fn fmt_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..86400 => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {:02}h", secs / 86400, (secs % 86400) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::lock::{LockHolder, LockSample};
    use crate::probes::net::NetInterfaceSample;
    use crate::ui::consolidated::model::tests::mac_and_box;

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
        let (gpus, tabs, statuses) = mac_and_box();
        let inputs = ConsolidatedInputs {
            gpu_info: &gpus,
            cpu_info: &[],
            tabs: &tabs,
            connection_status: &statuses,
            host_probes: probes,
            state,
            now_unix: 1_000_750,
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
        assert!(out.contains("M5 Ultra GPU (80 cores)"));
        assert!(out.contains("154.0/256.0 GiB unified"));
        assert!(out.contains("87.0/96.0 GiB VRAM"));
        assert!(out.contains("RTX PRO 6000 Blackwell #1"));
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
        let mut state = ConsolidatedState::default();
        let (gpus, _, _) = mac_and_box();
        for _ in 0..50 {
            state.record_collection(&gpus, &probes());
        }
        for cols in [80u16, 100, 120, 160, 220] {
            let out = render(cols, 60, &probes(), &state);
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
            tabs: &tabs,
            connection_status: &statuses,
            host_probes: &[],
            state: &state,
            now_unix: 0,
        };
        let mut buf = Vec::new();
        render_consolidated_tab(&mut buf, &inputs, 160, 40);
        let out = strip_ansi(&String::from_utf8(buf).unwrap());
        assert!(out.contains("(1 up)"));
        assert!(out.contains("unreachable: Connection refused"), "{out}");
    }

    #[test]
    fn pipeline_panel_renders_engine_swap_lock_and_link() {
        use crate::ui::consolidated::pipeline::{
            EngineCache, EngineHealth, PipelineConfig, PipelineStatus, Probe, SwapModel,
        };
        let mut status = PipelineStatus::new(PipelineConfig::icculus(None, None));
        status.engine = Probe::Ok(EngineHealth {
            ok: Some(true),
            sessions: Some(2),
            connections: Some(1),
            gpu_job: Some(serde_json::Value::String("prefill".to_string())),
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
        assert!(out.contains("── Icculus pipeline"), "{out}");
        assert!(out.contains("vllm :10051"), "{out}");
        assert!(out.contains("ok · d11dccf · og-s4.3 · up 1h 10m"), "{out}");
        assert!(out.contains("sessions 2 · connections 1 · gpu busy: prefill 1.2s · queue 3"));
        assert!(out.contains("prefix cache 301/406 hits (74.1%) · 777 entries"));
        assert!(out.contains("ians-Mac-Studio :8080"), "{out}");
        assert!(out.contains("ds41 ready  Icculus"));
        assert!(out.contains("held by omlx-server"));
        assert!(out.contains("1.25 GB/s"));

        let mut down = state.clone();
        let p = down.pipeline.as_mut().unwrap();
        p.engine = Probe::Err("connection failed".to_string());
        p.swap = Probe::Pending;
        let out = render(160, 60, &[], &down);
        assert!(out.contains("unreachable: connection failed"));
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
    fn formats() {
        assert_eq!(fmt_watts(0.02), "0.02 W");
        assert_eq!(fmt_watts(92.4), "92.4 W");
        assert_eq!(fmt_watts(188.6), "189 W");
        assert_eq!(fmt_rate(None), "n/a");
        assert_eq!(fmt_rate(Some(999.0)), "999 B/s");
        assert_eq!(fmt_duration(59), "59s");
        assert_eq!(fmt_duration(3599), "59m 59s");
        assert_eq!(fmt_duration(3 * 3600 + 5 * 60), "3h 05m");
        assert_eq!(fmt_duration(2 * 86400 + 4 * 3600), "2d 04h");
        assert_eq!(tilde_home("/home/ian/x.lock"), "~/x.lock");
        assert_eq!(
            fit_keeping_index("RTX PRO 6000 Blackwell #1", 19),
            "RTX PRO 6000 Bla #1"
        );
        assert_eq!(fit_keeping_index("M5 Ultra GPU", 19), "M5 Ultra GPU");
        assert_eq!(tilde_home("/var/lock/x"), "/var/lock/x");
    }
}
