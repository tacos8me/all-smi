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

//! History panel at the bottom of the All tab: one column of charts per
//! accelerator (utilization, memory in use, power), sized to the rows left
//! under the device table. With more devices than fit side by side the
//! columns wrap; when even that does not fit, the panel falls back to one
//! cluster-wide column (average utilization, total power).

use std::io::Write;

use crossterm::style::Color;

use crate::ui::braille::sparkline_braille_rows;
use crate::ui::consolidated::history::{self, SeriesHistory};
use crate::ui::consolidated::model::{ConsolidatedModel, DeviceRow};
use crate::ui::text::display_width;
use crate::ui::theme::{ACCENT, MUTED, RULE, SUBTLE, TEXT};

use super::line::{Line, Writer, fit_keeping_index, fmt_duration, fmt_watts};

/// Narrowest chart column.
const MIN_CELL: usize = 36;
/// Gap between columns.
const GAP: usize = 3;
const MARGIN: usize = 1;
/// Tallest chart; taller panels leave the extra rows blank.
const MAX_CHART_ROWS: usize = 10;

/// One chart: a title, a series and its fixed range.
struct Chart {
    label: &'static str,
    values: Vec<f64>,
    range: (f64, f64),
    format: fn(f64) -> String,
    color: Color,
}

/// One column: a device (or the cluster) and its charts, top to bottom.
struct Column {
    title: String,
    charts: Vec<Chart>,
}

/// Rows the panel needs to be worth drawing: heading, a title line, and
/// two charts of two rows each with their labels.
pub const MIN_ROWS: usize = 1 + 1 + 2 * 3;

pub fn render_history<W: Write>(
    w: &mut Writer<'_, W>,
    model: &ConsolidatedModel,
    series: &SeriesHistory,
    interval_secs: u64,
) {
    let rows = w.rows_left;
    if rows < MIN_ROWS {
        return;
    }
    let devices: Vec<&DeviceRow> = model.hosts.iter().flat_map(|h| &h.devices).collect();
    let width = w.cols.saturating_sub(MARGIN);
    let fit = ((width + GAP) / (MIN_CELL + GAP)).max(1);

    // Per device when every column row still gets two-row charts;
    // otherwise one cluster-wide column.
    let per_row = fit.min(devices.len().max(1));
    let column_rows = devices.len().div_ceil(per_row).max(1);
    let per_device_rows = (rows - 1) / column_rows;
    // A title line, then three charts of at least two rows with a label.
    let columns: Vec<Column> = if !devices.is_empty() && per_device_rows > 3 * 3 {
        devices.iter().map(|d| device_column(d, series)).collect()
    } else {
        vec![cluster_column(model, series)]
    };
    let per_row = fit.min(columns.len());
    let column_rows = columns.len().div_ceil(per_row);
    let charts = columns.iter().map(|c| c.charts.len()).max().unwrap_or(1);
    // Rows per column row: the title, then a label line per chart.
    let body = (rows - 1) / column_rows;
    let chart_h = (body.saturating_sub(1 + charts) / charts).clamp(1, MAX_CHART_ROWS);
    let cell_w = (width + GAP) / per_row - GAP;

    let span = (cell_w * 2) as u64 * interval_secs.max(1);
    let mut title = Line::default();
    title.text("History", ACCENT);
    let mut note = Line::default();
    note.text(&format!("last {}", fmt_duration(span)), MUTED);
    w.heading(&title, Some(&note));

    for row in columns.chunks(per_row) {
        let mut line = Line::default();
        for (i, col) in row.iter().enumerate() {
            line.pad_to(MARGIN + i * (cell_w + GAP))
                .text(&fit_keeping_index(&col.title, cell_w), TEXT);
        }
        w.emit(line);
        for k in 0..charts {
            emit_label(w, row, k, cell_w);
            emit_chart(w, row, k, cell_w, chart_h);
        }
    }
}

fn percent(v: f64) -> String {
    format!("{v:.0}%")
}

fn device_column(d: &DeviceRow, series: &SeriesHistory) -> Column {
    let power = series.values(&history::power_key(&d.uuid));
    Column {
        title: d.label(),
        charts: vec![
            Chart {
                label: "util",
                values: series.values(&history::util_key(&d.uuid)),
                range: (0.0, 100.0),
                format: percent,
                color: ACCENT,
            },
            Chart {
                label: match d.memory_kind {
                    crate::ui::consolidated::model::MemoryKind::Unified => "unified mem",
                    crate::ui::consolidated::model::MemoryKind::Dedicated => "VRAM",
                },
                values: series.values(&history::mem_key(&d.uuid)),
                range: (0.0, 100.0),
                format: percent,
                color: SUBTLE,
            },
            Chart {
                label: "power",
                range: (0.0, power_ceiling(d.power_limit_watts, &power)),
                values: power,
                format: fmt_watts,
                color: SUBTLE,
            },
        ],
    }
}

fn cluster_column(model: &ConsolidatedModel, series: &SeriesHistory) -> Column {
    let power = series.values(history::TOTAL_POWER_KEY);
    let limit = (model.totals.power_limit_watts > 0.0).then_some(model.totals.power_limit_watts);
    Column {
        title: format!("all {} accelerators", model.totals.devices),
        charts: vec![
            Chart {
                label: "avg util",
                values: series.values(history::TOTAL_UTIL_KEY),
                range: (0.0, 100.0),
                format: percent,
                color: ACCENT,
            },
            Chart {
                label: "total power",
                range: (0.0, power_ceiling(limit, &power)),
                values: power,
                format: fmt_watts,
                color: SUBTLE,
            },
        ],
    }
}

/// The board limit when there is one; otherwise headroom over the peak
/// so a steady draw does not fill the chart.
fn power_ceiling(limit: Option<f64>, values: &[f64]) -> f64 {
    let peak = values.iter().copied().fold(0.0, f64::max);
    match limit {
        Some(l) if l > 0.0 => l.max(peak),
        _ => (peak * 1.5).max(1.0),
    }
}

/// `util                 30% · peak 55%` above chart `k` of each column.
fn emit_label<W: Write>(w: &mut Writer<'_, W>, row: &[Column], k: usize, cell_w: usize) {
    let mut line = Line::default();
    for (i, col) in row.iter().enumerate() {
        let Some(chart) = col.charts.get(k) else {
            continue;
        };
        let start = MARGIN + i * (cell_w + GAP);
        let now = chart
            .values
            .last()
            .map_or("n/a".to_string(), |v| (chart.format)(*v));
        let peak = chart.values.iter().copied().fold(f64::NAN, f64::max);
        let stat = if peak.is_finite() {
            format!("{now} · peak {}", (chart.format)(peak))
        } else {
            now
        };
        line.pad_to(start)
            .text(chart.label, MUTED)
            .pad_to(start + cell_w.saturating_sub(display_width(&stat)))
            .text(&stat, SUBTLE);
    }
    w.emit(line);
}

fn emit_chart<W: Write>(
    w: &mut Writer<'_, W>,
    row: &[Column],
    k: usize,
    cell_w: usize,
    height: usize,
) {
    let rendered: Vec<Option<(Vec<String>, Color)>> = row
        .iter()
        .map(|col| {
            col.charts.get(k).map(|c| {
                (
                    sparkline_braille_rows(&c.values, cell_w, height, Some(c.range)),
                    c.color,
                )
            })
        })
        .collect();
    for r in 0..height {
        let mut line = Line::default();
        for (i, chart) in rendered.iter().enumerate() {
            let Some((rows, color)) = chart else {
                continue;
            };
            line.pad_to(MARGIN + i * (cell_w + GAP));
            let text = &rows[r];
            if r + 1 == height {
                // The bottom row doubles as the axis: history not yet
                // recorded shows as a dim baseline, not as empty space.
                let blank = text.chars().take_while(|c| *c == ' ').count();
                line.text(&"⣀".repeat(blank), RULE)
                    .text(&text.chars().skip(blank).collect::<String>(), *color);
            } else {
                line.text(text, *color);
            }
        }
        w.emit(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consolidated::model::tests::{mac_and_box, model_of};

    fn render(cols: usize, rows: usize, samples: usize) -> String {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let mut series = SeriesHistory::default();
        for _ in 0..samples {
            series.record_collection(&gpus, &[]);
        }
        let mut buf = Vec::new();
        let mut w = Writer {
            out: &mut buf,
            cols,
            rows_left: rows,
        };
        render_history(&mut w, &model, &series, 3);
        let out = String::from_utf8(buf).unwrap();
        let mut plain = String::new();
        let mut chars = out.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                plain.push(c);
            }
        }
        plain
    }

    #[test]
    fn one_column_per_device_when_they_fit() {
        let out = render(160, 26, 10);
        assert!(out.contains("History"), "{out}");
        assert!(out.contains("Apple M5 Ultra · 80-core GPU"), "{out}");
        assert!(out.contains("NVIDIA RTX PRO 6000 Blackwell #1"), "{out}");
        assert!(out.contains("30% · peak 30%"), "{out}");
        assert!(out.contains("unified mem"), "{out}");
        assert!(out.contains("VRAM"), "{out}");
        assert!(out.contains("100 W · peak 100 W"), "{out}");
        assert_eq!(out.matches("\r\n").count(), 26, "{out}");
        for line in out.split("\r\n") {
            assert!(display_width(line) <= 160, "{line:?}");
        }
    }

    #[test]
    fn falls_back_to_cluster_charts_when_short() {
        let out = render(60, 10, 10);
        assert!(out.contains("all 3 accelerators"), "{out}");
        assert!(out.contains("total power"), "{out}");
        assert!(render(160, MIN_ROWS - 1, 10).is_empty());
    }

    #[test]
    fn power_ceiling_uses_the_limit_or_headroom() {
        assert_eq!(power_ceiling(Some(600.0), &[100.0]), 600.0);
        assert_eq!(power_ceiling(None, &[10.0, 12.0]), 18.0);
        assert_eq!(power_ceiling(None, &[]), 1.0);
    }
}
