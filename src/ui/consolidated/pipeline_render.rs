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

//! The pipeline panel (`--icculis`): a heading, a diagram of the split
//! with a live phase on each half and the link rates between them, then
//! the engine, llama-swap, lock and link detail rows. [`compact_lines`] is
//! the one-line version the All tab shows.

use std::io::Write;

use crossterm::style::Color;

use crate::probes::HostProbes;
use crate::ui::braille::sparkline_braille;
use crate::ui::cluster::line::{Line, Writer, fmt_duration, fmt_rate};
use crate::ui::text::display_width;
use crate::ui::theme::{ACCENT, CRIT, MUTED, OK, RULE, SUBTLE, TEXT, WARN};

use super::history::{PIPE_BUSY_KEY, PIPE_QUEUE_KEY, SeriesHistory};
use super::model::ConsolidatedModel;
use super::pipeline::{EngineHealth, PipelineStatus, Probe, Stage, SwapModel};
use super::pipeline_view::{LinkUse, Phase, PipelineView};
use super::render::Columns;

const GIB: f64 = (1u64 << 30) as f64;
use super::render::SUBJECT as ENDPOINT;

/// Link traffic above this (bytes/s) draws the arrow in the accent color.
const LINK_ACTIVE: f64 = 1e6;
/// Width of the link column between the two halves of the diagram.
const LINK_W: usize = 24;
/// Widest box of the diagram.
const MAX_BOX_W: usize = 50;

/// `Icculis · DeepSeek-V4.1-Flash · original weights ───── ok · f1b5ebe …`
pub(crate) fn render_pipeline_heading<W: Write>(w: &mut Writer<'_, W>, p: &PipelineStatus) {
    let mut title = Line::default();
    title
        .text(&p.config.title, ACCENT)
        .text(&format!(" · {}", p.config.subtitle), SUBTLE);
    let mut right = Line::default();
    match &p.engine {
        Probe::Ok(h) => {
            engine_status(&mut right, h);
        }
        Probe::Err(e) => {
            right.text(&format!("engine unreachable: {e}"), CRIT);
        }
        Probe::Pending => {
            right.text("waiting for the engine", MUTED);
        }
    }
    let right = (w.cols >= title.width() + right.width() + 12).then_some(&right);
    w.heading(&title, right);
}

fn engine_status(line: &mut Line, h: &EngineHealth) {
    match h.ok {
        Some(true) => line.text("engine ok", OK),
        Some(false) => line.text("engine NOT OK", CRIT),
        None => line.text("engine ?", MUTED),
    };
    for (name, state) in [("encoder", &h.encoder), ("step api", &h.step_api)] {
        if let Some(s) = state
            && s != "up"
        {
            line.text(&format!(" · {name} {s}"), CRIT);
        }
    }
    if let Some(v) = &h.version {
        line.text(" · ", MUTED).text(v, SUBTLE);
    }
    if let Some(n) = &h.numerics {
        line.text(" · ", MUTED).text(n, SUBTLE);
    }
    if let Some(up) = h.uptime_s {
        line.text(
            &format!(" · up {}", fmt_duration(up.max(0.0) as u64)),
            MUTED,
        );
    }
    if h.restart_pending == Some(true) {
        line.text(" · restart pending", WARN);
    }
}

/// The two halves as boxes, the link between them:
///
/// ```text
/// ╭ RTX box · vllm ─────────────╮          10GbE          ╭ M5 Ultra · ians-Mac-Studio ──╮
/// │ layers 0-19 · prefill + …   │ ━━━━ ▶ 1.12 GB/s ━━━━▶ │ layers 20-39 · head · …      │
/// │ ● PREFILL  8s · prefill_…   │      ◀ 3.1 KB/s         │ ○ waiting  for the prefill…  │
/// ╰─────────────────────────────╯     prefill state       ╰──────────────────────────────╯
/// ```
pub(crate) fn render_diagram<W: Write>(
    w: &mut Writer<'_, W>,
    p: &PipelineStatus,
    view: &PipelineView,
) {
    let inner = w.cols.saturating_sub(2);
    let box_w = (inner.saturating_sub(LINK_W) / 2).min(MAX_BOX_W);
    if box_w < 24 {
        // Too narrow for boxes: the compact strip instead.
        for line in strip(p, view, w.cols) {
            w.emit(line);
        }
        return;
    }
    // Centered: the diagram is a figure, not a table.
    let indent = " ".repeat(1 + (inner - 2 * box_w - LINK_W) / 2);
    let front_title = format!("{} · {}", p.config.front.name, view.front_host);
    let back_title = format!("{} · {}", p.config.back.name, view.back_host);

    let mut top = Line::default();
    top.text(&indent, RULE);
    box_top(&mut top, &front_title, box_w);
    top.text(&center(&p.config.link, LINK_W), MUTED);
    box_top(&mut top, &back_title, box_w);
    w.emit(top);

    let mut roles = Line::default();
    roles.text(&indent, RULE);
    box_row(&mut roles, box_w, |l, width| {
        l.cell(&p.config.front.role, width, SUBTLE);
    });
    link_arrow(&mut roles, view.to_back, true);
    box_row(&mut roles, box_w, |l, width| {
        l.cell(&p.config.back.role, width, SUBTLE);
    });
    w.emit(roles);

    let mut phases = Line::default();
    phases.text(&indent, RULE);
    box_row(&mut phases, box_w, |l, width| {
        phase_cell(l, &view.front, width)
    });
    link_arrow(&mut phases, view.to_front, false);
    box_row(&mut phases, box_w, |l, width| {
        phase_cell(l, &view.back, width)
    });
    w.emit(phases);

    let mut bottom = Line::default();
    bottom.text(&indent, RULE);
    box_bottom(&mut bottom, box_w);
    let use_label = match view.link {
        LinkUse::PrefillState => "prefill state ▶",
        LinkUse::Steps => "◀ decode steps ▶",
        LinkUse::Idle => "idle",
    };
    let use_color = if view.link == LinkUse::Idle {
        MUTED
    } else {
        ACCENT
    };
    bottom.text(&center(use_label, LINK_W), use_color);
    box_bottom(&mut bottom, box_w);
    w.emit(bottom);
}

fn box_top(line: &mut Line, title: &str, width: usize) {
    let title = crate::ui::text::truncate_to_width(title, width.saturating_sub(5));
    line.text("╭ ", RULE).text(&title, TEXT).text(" ", RULE);
    let used = display_width(&title) + 3;
    line.text(
        &format!("{}╮", "─".repeat(width.saturating_sub(used + 1))),
        RULE,
    );
}

fn box_bottom(line: &mut Line, width: usize) {
    line.text(&format!("╰{}╯", "─".repeat(width.saturating_sub(2))), RULE);
}

fn box_row(line: &mut Line, width: usize, body: impl FnOnce(&mut Line, usize)) {
    line.text("│ ", RULE);
    let start = line.width();
    let inner = width.saturating_sub(4);
    body(line, inner);
    let used = line.width() - start;
    if used < inner {
        line.text(&" ".repeat(inner - used), RULE);
    }
    line.text(" │", RULE);
}

fn phase_cell(line: &mut Line, phase: &Phase, width: usize) {
    let (word, detail) = phase.words();
    let (dot, color) = match phase {
        Phase::Down(_) => ("● ", CRIT),
        p if p.is_active() => ("● ", ACCENT),
        _ => ("○ ", MUTED),
    };
    let text_color = match phase {
        Phase::Down(_) => CRIT,
        p if p.is_active() => ACCENT,
        _ => SUBTLE,
    };
    line.text(dot, color);
    if !word.is_empty() {
        line.text(word, text_color).text("  ", MUTED);
    }
    let room = width.saturating_sub(display_width(word) + 4);
    line.text(&crate::ui::text::truncate_to_width(&detail, room), SUBTLE);
}

/// One direction of the link: `━━━ ▶ 1.12 GB/s ━━━▶` towards the back
/// host, `◀ 3.1 KB/s` towards the front.
fn link_arrow(line: &mut Line, rate: Option<f64>, to_back: bool) {
    let active = rate.is_some_and(|r| r >= LINK_ACTIVE);
    let color = if active { ACCENT } else { MUTED };
    let text = match (rate, to_back) {
        (Some(r), true) => format!(" {} ▶ ", fmt_rate(Some(r))),
        (Some(r), false) => format!(" ◀ {} ", fmt_rate(Some(r))),
        (None, _) => String::new(),
    };
    let t = display_width(&text);
    let pad = LINK_W.saturating_sub(t + 2);
    let (left, right) = (pad / 2, pad - pad / 2);
    let rule = if active { "━" } else { "─" };
    line.text(" ", RULE)
        .text(&rule.repeat(left), if active { ACCENT } else { RULE })
        .text(&text, color)
        .text(&rule.repeat(right), if active { ACCENT } else { RULE })
        .text(" ", RULE);
}

fn center(text: &str, width: usize) -> String {
    let t = display_width(text);
    let left = width.saturating_sub(t) / 2;
    let right = width.saturating_sub(t + left);
    format!("{}{text}{}", " ".repeat(left), " ".repeat(right))
}

/// One line: `RTX box ● PREFILL 8s ━▶ 1.1 GB/s ━▶ M5 Ultra ○ waiting …`.
fn strip(p: &PipelineStatus, view: &PipelineView, cols: usize) -> Vec<Line> {
    // Roles go first when space runs out; a detail that still does not
    // fit is clipped at the edge.
    let line = strip_line(p, view, true);
    if line.width() <= cols {
        return vec![line];
    }
    let line = strip_line(p, view, false);
    if cols >= 80 {
        return vec![line];
    }
    // Very narrow: the phase words only.
    let mut short = Line::default();
    short.text(" ", RULE);
    half_word(&mut short, &p.config.front.name, None, &view.front);
    short.text(" ▶ ", MUTED);
    half_word(&mut short, &p.config.back.name, None, &view.back);
    vec![short]
}

fn strip_line(p: &PipelineStatus, view: &PipelineView, roles: bool) -> Line {
    let mut line = Line::default();
    line.text(" ", RULE);
    half(&mut line, &p.config.front, roles, &view.front);
    let rate = match (view.link.clone(), view.to_back, view.to_front) {
        (LinkUse::PrefillState, r, _) => r,
        (_, Some(a), Some(b)) => Some(a.max(b)),
        (_, a, b) => a.or(b),
    };
    let active = rate.is_some_and(|r| r >= LINK_ACTIVE);
    let arrow = match view.link {
        LinkUse::Steps => "◀▶",
        _ => "▶",
    };
    line.text("   ", RULE)
        .text(
            &format!("── {} {arrow} ──", fmt_rate(rate)),
            if active { ACCENT } else { MUTED },
        )
        .text("   ", RULE);
    half(&mut line, &p.config.back, roles, &view.back);
    line
}

fn half(line: &mut Line, stage: &Stage, role: bool, phase: &Phase) {
    half_word(
        line,
        &stage.name,
        role.then_some(stage.short.as_str()),
        phase,
    );
    let (_, detail) = phase.words();
    if !detail.is_empty() {
        line.text("  ", MUTED).text(&detail, SUBTLE);
    }
}

fn half_word(line: &mut Line, name: &str, role: Option<&str>, phase: &Phase) {
    let (word, _) = phase.words();
    let (dot, color) = match phase {
        Phase::Down(_) => ("●", CRIT),
        p if p.is_active() => ("●", ACCENT),
        _ => ("○", MUTED),
    };
    line.text(name, TEXT);
    if let Some(role) = role {
        line.text(" ", RULE).text(role, MUTED);
    }
    line.text("  ", RULE).text(dot, color);
    if !word.is_empty() {
        line.text(" ", RULE).text(word, color);
    }
}

/// The All tab's version: a heading line with the model and llama-swap
/// state, and the one-line split.
pub fn compact_lines(
    model: &ConsolidatedModel,
    p: &PipelineStatus,
    probes: &[HostProbes],
    cols: usize,
    _now_unix: u64,
) -> Vec<Line> {
    let view = PipelineView::build(model, p, probes);
    let mut head = Line::default();
    head.text(" ", RULE)
        .text(&p.config.title, ACCENT)
        .text(&format!("  {}", p.config.subtitle), MUTED);
    match &p.swap {
        Probe::Ok(models) if !models.is_empty() => {
            head.text("  ·  ", MUTED);
            for (i, m) in models.iter().enumerate() {
                if i > 0 {
                    head.text(", ", MUTED);
                }
                head.text(&m.model, SUBTLE)
                    .text(" ", RULE)
                    .text(&m.state, swap_color(&m.state));
            }
        }
        Probe::Ok(_) => {
            head.text("  ·  no model loaded", WARN);
        }
        Probe::Err(e) => {
            head.text(&format!("  ·  llama-swap unreachable: {e}"), CRIT);
        }
        Probe::Pending => {}
    }
    if let Probe::Err(e) = &p.engine {
        head.text(&format!("  ·  engine unreachable: {e}"), CRIT);
    }
    let mut out = vec![head];
    out.extend(strip(p, &view, cols));
    out
}

fn swap_color(state: &str) -> Color {
    match state {
        "ready" => OK,
        "starting" | "stopping" => WARN,
        _ => CRIT,
    }
}

/// Sessions, connections, the queue and the prefix cache, with the
/// busy / queue sparklines.
pub(crate) fn render_engine_details<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    p: &PipelineStatus,
    series: &SeriesHistory,
) {
    let Probe::Ok(h) = &p.engine else {
        return;
    };
    let mut line = Line::default();
    line.cell("ENGINE", c.host, MUTED).cell(
        &endpoint_label(model, &p.config.health_url),
        ENDPOINT,
        TEXT,
    );
    line.text("sessions ", MUTED)
        .text(&opt(h.sessions), TEXT)
        .text(" · connections ", MUTED)
        .text(&opt(h.connections), TEXT)
        .text(" · queue ", MUTED);
    let queued = h.queued_jobs.unwrap_or(0);
    line.text(&opt(h.queued_jobs), if queued > 0 { WARN } else { TEXT });
    if let Some(cache) = &h.cache {
        line.text(" · prefix cache ", MUTED);
        match cache.hit_ratio() {
            Some(r) => line.text(
                &format!("{:.0}% of {} lookups", r * 100.0, opt(cache.lookups)),
                TEXT,
            ),
            None => line.text("no lookups yet", MUTED),
        };
        if let (Some(b), Some(budget)) = (cache.bytes, cache.budget) {
            line.text(
                &format!(" · {:.1}/{:.1} GiB", b as f64 / GIB, budget as f64 / GIB),
                MUTED,
            );
        }
    }
    let spare = w.cols.saturating_sub(line.width() + 15);
    let spark = (spare / 2).min(30);
    if spark >= 6 {
        let busy = series.values(PIPE_BUSY_KEY);
        let queue = series.values(PIPE_QUEUE_KEY);
        let queue_max = queue.iter().copied().fold(1.0, f64::max);
        line.text("  busy ", MUTED)
            .text(&sparkline_braille(&busy, spark, Some((0.0, 1.0))), ACCENT)
            .text("  queue ", MUTED)
            .text(
                &sparkline_braille(&queue, spark, Some((0.0, queue_max))),
                SUBTLE,
            );
    }
    w.emit(line);
}

pub(crate) fn render_swap<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    p: &PipelineStatus,
) {
    let mut line = Line::default();
    line.cell("SWAP", c.host, MUTED).cell(
        &endpoint_label(model, &p.config.swap_url),
        ENDPOINT,
        TEXT,
    );
    match &p.swap {
        Probe::Pending => {
            line.text("waiting for the first /running poll", MUTED);
        }
        Probe::Err(e) => {
            line.text(&format!("unreachable: {e}"), CRIT);
        }
        Probe::Ok(models) if models.is_empty() => {
            line.text("no model loaded", WARN);
        }
        Probe::Ok(models) => {
            for (i, m) in models.iter().enumerate() {
                if i > 0 {
                    line.text(" · ", MUTED);
                }
                swap_model(&mut line, m, models.len() == 1);
            }
        }
    }
    w.emit(line);
}

fn swap_model(line: &mut Line, m: &SwapModel, with_name: bool) {
    line.text(&m.model, TEXT)
        .text(" ", MUTED)
        .text(&m.state, swap_color(&m.state));
    if with_name && !m.name.is_empty() {
        line.text(&format!("  {}", m.name), MUTED);
    }
}

/// "vllm :10051" when the URL's host is one of the scraped hosts,
/// otherwise the URL's own `host:port`.
fn endpoint_label(model: &ConsolidatedModel, url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    let host = parsed.host_str().unwrap_or_default();
    let port = parsed
        .port_or_known_default()
        .map(|p| format!(":{p}"))
        .unwrap_or_default();
    let known = model.hosts.iter().find(|h| {
        crate::common::http_hosts::host_identifier(&h.host_id)
            .rsplit_once(':')
            .is_some_and(|(ip, _)| ip == host)
    });
    match known {
        // A host that never answered has no name yet, only its address.
        Some(h) if h.label != h.host_id => format!("{} {port}", h.label),
        _ => format!("{host}{port}"),
    }
}

fn opt(v: Option<u64>) -> String {
    v.map_or_else(|| "?".to_string(), |n| n.to_string())
}
