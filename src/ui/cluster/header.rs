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

//! Remote-mode header: a title line and the cluster overview, a row of
//! stat blocks (label, value, and a bar or a note under it).
//!
//! Memory is split by where it lives: an Apple Silicon host's RAM is its
//! GPU's unified pool and is shown once, as unified memory; discrete GPUs
//! show their VRAM; the RAM of the other hosts is shown as host RAM.

use std::io::Write;

use crossterm::style::Color;

use crate::ui::consolidated::model::{ConsolidatedModel, Totals};
use crate::ui::text::display_width;
use crate::ui::theme::{self, ACCENT, CRIT, MUTED, SUBTLE, TEXT};

use super::line::{Line, Writer, fmt_gib_value, fmt_watts, plural};

/// Narrowest stat block, gap included.
const MIN_BLOCK: usize = 17;
/// Widest stat block; wider terminals leave the rest of the row empty.
const MAX_BLOCK: usize = 30;
/// Left margin of the overview.
const MARGIN: usize = 1;

/// Title-line facts that do not come from the model.
pub struct TitleInfo<'a> {
    pub time: &'a str,
    pub version: &'a str,
    /// Container / VM runtime the viewer runs in, if any.
    pub runtime: Option<&'a str>,
}

enum Under {
    Bar {
        ratio: f64,
        fill: Color,
        note: String,
    },
    Note(String, Color),
}

struct Block {
    label: &'static str,
    value: String,
    value_color: Color,
    under: Under,
}

/// Rows the header occupies at this width: title, blank, three per row of
/// stat blocks, blank.
pub fn header_rows(model: &ConsolidatedModel, cols: u16) -> u16 {
    let n = blocks(&model.totals, model).len();
    (3 + 3 * block_rows(n, cols as usize)) as u16
}

fn block_rows(n: usize, cols: usize) -> usize {
    let per_row = per_row(n, cols);
    n.div_ceil(per_row).max(1)
}

fn per_row(n: usize, cols: usize) -> usize {
    (cols.saturating_sub(MARGIN) / MIN_BLOCK).clamp(1, n.max(1))
}

pub fn render_header<W: Write>(
    out: &mut W,
    model: &ConsolidatedModel,
    title: &TitleInfo<'_>,
    cols: u16,
) {
    let cols = cols as usize;
    let mut w = Writer {
        out,
        cols,
        rows_left: usize::MAX,
    };
    render_title(&mut w, model, title);
    w.blank();
    render_blocks(&mut w, &blocks(&model.totals, model));
    w.blank();
}

fn render_title<W: Write>(w: &mut Writer<'_, W>, model: &ConsolidatedModel, info: &TitleInfo<'_>) {
    let t = &model.totals;
    let mut left = Line::default();
    left.text(" all-smi", ACCENT).text("  cluster", SUBTLE);
    if let Some(rt) = info.runtime {
        left.text(&format!("  [{rt}]"), MUTED);
    }
    let mut right = Line::default();
    right
        .text(
            &format!("{} of {} hosts up", t.hosts_up, t.hosts_total),
            if t.hosts_up < t.hosts_total {
                CRIT
            } else {
                MUTED
            },
        )
        .text("  ·  ", MUTED)
        .text(info.time, SUBTLE)
        .text("  ·  ", MUTED)
        .text(&format!("v{} ", info.version), MUTED);
    let gap = w.cols.saturating_sub(left.width() + right.width());
    if gap >= 2 {
        left.text(&" ".repeat(gap), MUTED).extend(right);
    } else {
        // Narrow: drop the version, keep the clock.
        left.pad_to(w.cols.saturating_sub(display_width(info.time) + 1))
            .text(info.time, SUBTLE);
    }
    w.emit(left);
}

fn blocks(t: &Totals, model: &ConsolidatedModel) -> Vec<Block> {
    let mut out = Vec::new();

    let down: Vec<&str> = model
        .hosts
        .iter()
        .filter(|h| !h.connected)
        .map(|h| h.label.as_str())
        .collect();
    // The title counts the hosts; a block appears only to name the ones
    // that are down.
    if !down.is_empty() {
        out.push(Block {
            label: "HOSTS",
            value: format!("{} / {} up", t.hosts_up, t.hosts_total),
            value_color: CRIT,
            under: Under::Note(format!("down: {}", down.join(", ")), CRIT),
        });
    }

    let discrete = t.devices - t.unified_devices;
    let mix = match (t.unified_devices, discrete) {
        (0, d) => plural(d, "discrete GPU"),
        (u, 0) => plural(u, "SoC GPU"),
        (u, d) => format!("{u} SoC · {d} dGPU"),
    };
    out.push(Block {
        label: "ACCELERATORS",
        value: t.devices.to_string(),
        value_color: TEXT,
        under: Under::Note(mix, MUTED),
    });

    match t.avg_utilization {
        Some(u) => {
            let level = theme::util_level(u);
            out.push(Block {
                label: "GPU UTIL",
                value: format!("{u:.0}% avg"),
                value_color: level.value_color(),
                under: Under::Bar {
                    ratio: u / 100.0,
                    fill: level.color(),
                    note: String::new(),
                },
            });
        }
        None => out.push(Block {
            label: "GPU UTIL",
            value: "n/a".to_string(),
            value_color: MUTED,
            under: Under::Note("no readings".to_string(), MUTED),
        }),
    }

    let pools = [
        ("UNIFIED MEMORY", t.unified_used, t.unified_total),
        ("VRAM", t.dedicated_used, t.dedicated_total),
        ("HOST RAM", t.host_ram_used, t.host_ram_total),
    ];
    for (label, used, total) in pools {
        if total == 0 {
            continue;
        }
        let ratio = used as f64 / total as f64;
        let level = theme::mem_level(ratio);
        out.push(Block {
            label,
            value: format!("{} / {} GiB", fmt_gib_value(used), fmt_gib_value(total)),
            value_color: TEXT,
            under: Under::Bar {
                ratio,
                fill: level.color(),
                note: format!("{:.0}%", ratio * 100.0),
            },
        });
    }

    let power = t.power_watts();
    let (value_color, note) = if t.power_limit_watts > 0.0 {
        let level = theme::power_level(t.dedicated_power_watts / t.power_limit_watts);
        (
            level.value_color(),
            format!("limit {}", fmt_watts(t.power_limit_watts)),
        )
    } else {
        (TEXT, "accelerators".to_string())
    };
    out.push(Block {
        label: "POWER",
        value: fmt_watts(power),
        value_color,
        under: Under::Note(note, MUTED),
    });

    if let Some((temp, slowdown)) = t.max_temperature {
        let level = theme::temp_level(temp, slowdown);
        let note = match slowdown {
            Some(s) => format!("slowdown {s}°C"),
            None => "hottest device".to_string(),
        };
        out.push(Block {
            label: "TEMP MAX",
            value: format!("{temp}°C"),
            value_color: level.value_color(),
            under: Under::Note(note, MUTED),
        });
    }
    out
}

fn render_blocks<W: Write>(w: &mut Writer<'_, W>, blocks: &[Block]) {
    let per_row = per_row(blocks.len(), w.cols);
    let width = (w.cols.saturating_sub(MARGIN) / per_row).clamp(1, MAX_BLOCK);
    for row in blocks.chunks(per_row) {
        let mut labels = Line::default();
        let mut values = Line::default();
        let mut unders = Line::default();
        for (i, b) in row.iter().enumerate() {
            let col = MARGIN + i * width;
            let body = width.saturating_sub(3);
            labels.pad_to(col).cell(b.label, width, MUTED);
            values.pad_to(col).cell(&b.value, width, b.value_color);
            unders.pad_to(col);
            match &b.under {
                Under::Note(text, color) => {
                    unders.cell(text, width, *color);
                }
                Under::Bar { ratio, fill, note } => {
                    let note_w = if note.is_empty() {
                        0
                    } else {
                        display_width(note) + 1
                    };
                    unders
                        .bar(body.saturating_sub(note_w), *ratio, *fill)
                        .text(if note.is_empty() { "" } else { " " }, MUTED)
                        .text(note, SUBTLE);
                }
            }
        }
        w.emit(labels);
        w.emit(values);
        w.emit(unders);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consolidated::model::tests::{mac_and_box, model_of};

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

    fn render(cols: u16) -> String {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let mut buf = Vec::new();
        let title = TitleInfo {
            time: "2026-09-25 22:43:07",
            version: "0.26.3",
            runtime: None,
        };
        render_header(&mut buf, &model, &title, cols);
        assert_eq!(
            strip(&String::from_utf8(buf.clone()).unwrap())
                .matches("\r\n")
                .count(),
            header_rows(&model, cols) as usize
        );
        strip(&String::from_utf8(buf).unwrap())
    }

    #[test]
    fn overview_separates_unified_memory_from_vram() {
        let out = render(160);
        assert!(out.contains("2 of 2 hosts up"), "{out}");
        assert!(!out.contains("HOSTS"), "{out}");
        assert!(out.contains("UNIFIED MEMORY"), "{out}");
        assert!(out.contains("154 / 256 GiB"), "{out}");
        assert!(out.contains("VRAM"), "{out}");
        assert!(out.contains("174 / 192 GiB"), "{out}");
        assert!(out.contains("1 SoC · 2 dGPU"), "{out}");
        assert!(out.contains("28% avg"), "{out}");
        assert!(out.contains("212 W"), "{out}");
        assert!(out.contains("limit 1.20 kW"), "{out}");
        // No host RAM block without memory series for non-unified hosts.
        assert!(!out.contains("HOST RAM"), "{out}");
        assert!(!out.contains("GPU Cores"), "{out}");
    }

    #[test]
    fn a_down_host_gets_a_block() {
        let (gpus, tabs, mut statuses) = mac_and_box();
        statuses
            .get_mut("10.10.10.2:9090")
            .unwrap()
            .mark_failure("Connection refused".to_string());
        let gpus: Vec<_> = gpus
            .into_iter()
            .filter(|g| g.host_id != "10.10.10.2:9090")
            .collect();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let mut buf = Vec::new();
        let title = TitleInfo {
            time: "t",
            version: "v",
            runtime: None,
        };
        render_header(&mut buf, &model, &title, 160);
        let out = strip(&String::from_utf8(buf).unwrap());
        assert!(out.contains("1 of 2 hosts up"), "{out}");
        assert!(out.contains("1 / 2 up"), "{out}");
        assert!(out.contains("down: ians-Mac-Studio"), "{out}");
        assert!(!out.contains("UNIFIED MEMORY"), "{out}");
    }

    #[test]
    fn narrow_terminals_wrap_blocks_and_stay_in_bounds() {
        for cols in [60u16, 80, 100, 120, 160, 200] {
            let out = render(cols);
            for line in out.split("\r\n") {
                assert!(display_width(line) <= cols as usize, "{cols}: {line:?}");
            }
        }
        let wide = render(200);
        let narrow = render(80);
        assert!(narrow.matches("\r\n").count() > wide.matches("\r\n").count());
    }
}
