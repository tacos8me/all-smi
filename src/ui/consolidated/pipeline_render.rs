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

//! Engine and llama-swap rows of the pipeline panel. The lock and link
//! rows that follow them are the shared probe rows from [`super::render`].

use std::io::Write;

use crossterm::style::Color;

use crate::ui::braille::sparkline_braille;
use crate::ui::text::display_width;

use super::history::{PIPE_BUSY_KEY, PIPE_QUEUE_KEY, SeriesHistory};
use super::model::ConsolidatedModel;
use super::pipeline::{EngineHealth, PipelineStatus, Probe, SwapModel};
use super::render::{Columns, Line, Writer, fmt_duration};

const LABEL: Color = Color::DarkGrey;
const VALUE: Color = Color::White;
const HEADING: Color = Color::Cyan;
const GIB: f64 = (1u64 << 30) as f64;
use super::render::SUBJECT as ENDPOINT;

pub(crate) fn render_pipeline_heading<W: Write>(w: &mut Writer<'_, W>, p: &PipelineStatus) {
    let title = format!("── {} ", p.config.title);
    let mut line = Line::default();
    line.text(&title, HEADING).text(
        &"─".repeat(w.cols.saturating_sub(display_width(&title))),
        LABEL,
    );
    w.emit(line);
}

pub(crate) fn render_engine<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    p: &PipelineStatus,
    series: &SeriesHistory,
) {
    let mut head = Line::default();
    head.cell("ENGINE", c.host, HEADING).cell(
        &endpoint_label(model, &p.config.health_url),
        ENDPOINT,
        VALUE,
    );
    let health = match &p.engine {
        Probe::Pending => {
            head.text("waiting for the first /health poll", LABEL);
            w.emit(head);
            return;
        }
        Probe::Err(e) => {
            head.text(&format!("unreachable: {e}"), Color::Red);
            w.emit(head);
            return;
        }
        Probe::Ok(h) => h,
    };

    match health.ok {
        Some(true) => head.text("ok", Color::Green),
        Some(false) => head.text("NOT OK", Color::Red),
        None => head.text("status ?", LABEL),
    };
    for (name, state) in [("encoder", &health.encoder), ("step api", &health.step_api)] {
        if let Some(s) = state
            && s != "up"
        {
            head.text(&format!(" · {name} {s}"), Color::Red);
        }
    }
    if let Some(v) = &health.version {
        head.text(" · ", LABEL).text(v, VALUE);
    }
    if let Some(n) = &health.numerics {
        head.text(" · ", LABEL).text(n, VALUE);
    }
    if let Some(up) = health.uptime_s {
        head.text(
            &format!(" · up {}", fmt_duration(up.max(0.0) as u64)),
            LABEL,
        );
    }
    if health.restart_pending == Some(true) {
        head.text(" · restart pending", Color::Yellow);
    }
    w.emit(head);

    w.emit(activity_line(w.cols, c, health, series));
    if let Some(line) = cache_line(c, health) {
        w.emit(line);
    }
}

/// Sessions, connections, the GPU job, the queue, and their sparklines.
fn activity_line(cols: usize, c: &Columns, h: &EngineHealth, series: &SeriesHistory) -> Line {
    let mut line = Line::default();
    line.cell("", c.host, LABEL)
        .cell("", ENDPOINT, LABEL)
        .text("sessions ", LABEL)
        .text(&opt(h.sessions), VALUE)
        .text(" · connections ", LABEL)
        .text(&opt(h.connections), VALUE)
        .text(" · ", LABEL);
    match h.gpu_job_label() {
        Some(job) => {
            line.text("gpu busy: ", Color::Yellow)
                .text(&job, Color::Yellow);
            if let Some(s) = h.gpu_job_s {
                line.text(&format!(" {s:.1}s"), Color::Yellow);
            }
        }
        None => {
            line.text("gpu idle", LABEL);
        }
    }
    let queued = h.queued_jobs.unwrap_or(0);
    line.text(" · queue ", LABEL).text(
        &opt(h.queued_jobs),
        if queued > 0 { Color::Yellow } else { VALUE },
    );

    // "  busy <spark>  queue <spark>" with whatever width is left.
    let spare = cols.saturating_sub(line.width() + 15);
    let spark = (spare / 2).min(30);
    if spark >= 6 {
        let busy = series.values(PIPE_BUSY_KEY);
        let queue = series.values(PIPE_QUEUE_KEY);
        let queue_max = queue.iter().copied().fold(1.0, f64::max);
        line.text("  busy ", LABEL)
            .text(
                &sparkline_braille(&busy, spark, Some((0.0, 1.0))),
                Color::Yellow,
            )
            .text("  queue ", LABEL)
            .text(
                &sparkline_braille(&queue, spark, Some((0.0, queue_max))),
                Color::Magenta,
            );
    }
    line
}

fn cache_line(c: &Columns, h: &EngineHealth) -> Option<Line> {
    let cache = h.cache.as_ref()?;
    let mut line = Line::default();
    line.cell("", c.host, LABEL)
        .cell("", ENDPOINT, LABEL)
        .text("prefix cache ", LABEL);
    match cache.hit_ratio() {
        Some(r) => line.text(
            &format!(
                "{}/{} hits ({:.1}%)",
                opt(cache.hits),
                opt(cache.lookups),
                r * 100.0
            ),
            VALUE,
        ),
        None => line.text("no lookups yet", LABEL),
    };
    if let Some(e) = cache.entries {
        line.text(&format!(" · {e} entries"), LABEL);
    }
    if let (Some(b), Some(budget)) = (cache.bytes, cache.budget) {
        line.text(
            &format!(" · {:.1}/{:.1} GiB", b as f64 / GIB, budget as f64 / GIB),
            LABEL,
        );
    }
    if let Some(t) = cache.resumed_tokens {
        line.text(&format!(" · {} tokens resumed", fmt_count(t)), LABEL);
    }
    Some(line)
}

pub(crate) fn render_swap<W: Write>(
    w: &mut Writer<'_, W>,
    c: &Columns,
    model: &ConsolidatedModel,
    p: &PipelineStatus,
) {
    let mut line = Line::default();
    line.cell("SWAP", c.host, HEADING).cell(
        &endpoint_label(model, &p.config.swap_url),
        ENDPOINT,
        VALUE,
    );
    match &p.swap {
        Probe::Pending => {
            line.text("waiting for the first /running poll", LABEL);
        }
        Probe::Err(e) => {
            line.text(&format!("unreachable: {e}"), Color::Red);
        }
        Probe::Ok(models) if models.is_empty() => {
            line.text("no model loaded", Color::Yellow);
        }
        Probe::Ok(models) => {
            for (i, m) in models.iter().enumerate() {
                if i > 0 {
                    line.text(" · ", LABEL);
                }
                swap_model(&mut line, m, models.len() == 1);
            }
        }
    }
    w.emit(line);
}

fn swap_model(line: &mut Line, m: &SwapModel, with_name: bool) {
    let color = match m.state.as_str() {
        "ready" => Color::Green,
        "starting" | "stopping" => Color::Yellow,
        _ => Color::Red,
    };
    line.text(&m.model, VALUE)
        .text(" ", LABEL)
        .text(&m.state, color);
    if with_name && !m.name.is_empty() {
        line.text(&format!("  {}", m.name), LABEL);
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

fn fmt_count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.2}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_compact() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(5_385_728), "5.39M");
        assert_eq!(fmt_count(12_300), "12.3K");
    }
}
