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

//! Line assembly and number formatting shared by the cluster views.
//!
//! A [`Line`] is a list of colored segments with cell helpers for aligned
//! columns; a [`Writer`] emits lines clipped to the terminal width and stops
//! at a row budget, so a view can never push the footer off screen.

use std::io::Write;

use crossterm::{
    queue,
    style::{Color, Print},
};

use crate::ui::text::{display_width, print_colored_text, truncate_to_width};
use crate::ui::theme::{self, BAR_FILL, BAR_TRACK, RULE};

pub const GIB: f64 = (1u64 << 30) as f64;

#[derive(Default, Clone)]
pub struct Line {
    segs: Vec<(String, Color)>,
}

impl Line {
    /// Display width of everything added so far.
    pub fn width(&self) -> usize {
        self.segs.iter().map(|(t, _)| display_width(t)).sum()
    }

    pub fn text(&mut self, s: &str, color: Color) -> &mut Self {
        if !s.is_empty() {
            self.segs.push((s.to_string(), color));
        }
        self
    }

    /// Left-aligned cell of exactly `width` columns (one trailing space is
    /// reserved as the column gap).
    pub fn cell(&mut self, s: &str, width: usize, color: Color) -> &mut Self {
        let body = width.saturating_sub(1);
        let t = truncate_to_width(s, body);
        let pad = width.saturating_sub(display_width(&t));
        self.segs.push((format!("{t}{}", " ".repeat(pad)), color));
        self
    }

    /// Right-aligned cell of exactly `width` columns, gap on the right.
    pub fn rcell(&mut self, s: &str, width: usize, color: Color) -> &mut Self {
        let body = width.saturating_sub(1);
        let t = truncate_to_width(s, body);
        let pad = body.saturating_sub(display_width(&t));
        self.segs.push((format!("{}{t} ", " ".repeat(pad)), color));
        self
    }

    /// Spaces up to column `col` (no-op when already past it).
    pub fn pad_to(&mut self, col: usize) -> &mut Self {
        let w = self.width();
        if col > w {
            self.segs.push((" ".repeat(col - w), RULE));
        }
        self
    }

    /// A `width`-cell bar filled to `ratio` in `fill`, on a dim track.
    pub fn bar(&mut self, width: usize, ratio: f64, fill: Color) -> &mut Self {
        let (filled, empty) = theme::bar_cells(width, ratio);
        if filled > 0 {
            self.segs.push((BAR_FILL.to_string().repeat(filled), fill));
        }
        if empty > 0 {
            self.segs.push((BAR_TRACK.to_string().repeat(empty), RULE));
        }
        self
    }

    /// Append another line's segments.
    pub fn extend(&mut self, other: Line) -> &mut Self {
        self.segs.extend(other.segs);
        self
    }

    /// Plain text of the line, without colors.
    #[cfg(test)]
    pub fn plain(&self) -> String {
        self.segs.iter().map(|(t, _)| t.as_str()).collect()
    }
}

pub struct Writer<'w, W: Write> {
    pub out: &'w mut W,
    pub cols: usize,
    pub rows_left: usize,
}

impl<W: Write> Writer<'_, W> {
    /// Write one line clipped to the terminal width. Lines past the row
    /// budget are dropped.
    pub fn emit(&mut self, line: Line) {
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

    pub fn blank(&mut self) {
        self.emit(Line::default());
    }

    pub fn rule(&mut self) {
        let mut line = Line::default();
        line.text(&"─".repeat(self.cols), RULE);
        self.emit(line);
    }

    /// `title ────…── right`: a section heading in `title_color` with an
    /// optional right-aligned note.
    pub fn heading(&mut self, title: &Line, right: Option<&Line>) {
        let mut line = Line::default();
        line.text(" ", RULE).extend(title.clone()).text(" ", RULE);
        let right_w = right.map_or(0, |r| r.width() + 2);
        let fill = self.cols.saturating_sub(line.width() + right_w);
        line.text(&"─".repeat(fill), RULE);
        if let Some(r) = right {
            line.text(" ", RULE).extend(r.clone()).text(" ", RULE);
        }
        self.emit(line);
    }
}

pub fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

pub fn fmt_watts(w: f64) -> String {
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

/// A GiB figure with precision that suits its size: `88.1`, `154`.
pub fn fmt_gib_value(bytes: u64) -> String {
    let g = bytes as f64 / GIB;
    if g >= 100.0 {
        format!("{g:.0}")
    } else {
        format!("{g:.1}")
    }
}

/// `used/total GiB`, compact.
pub fn fmt_gib_pair(used: u64, total: u64) -> String {
    format!("{}/{} GiB", fmt_gib_value(used), fmt_gib_value(total))
}

/// Decimal network units, as link speeds are quoted.
pub fn fmt_rate(bytes_per_sec: Option<f64>) -> String {
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

pub fn fmt_duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..86400 => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {:02}h", secs / 86400, (secs % 86400) / 3600),
    }
}

/// Truncate a device name to `width`, keeping a trailing ` #<index>` so
/// identical boards stay distinguishable on narrow terminals.
pub fn fit_keeping_index(name: &str, width: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_watts(0.02), "0.02 W");
        assert_eq!(fmt_watts(92.4), "92.4 W");
        assert_eq!(fmt_watts(188.6), "189 W");
        assert_eq!(fmt_watts(1234.0), "1.23 kW");
        assert_eq!(fmt_rate(None), "n/a");
        assert_eq!(fmt_rate(Some(999.0)), "999 B/s");
        assert_eq!(fmt_rate(Some(1.25e9)), "1.25 GB/s");
        assert_eq!(fmt_duration(59), "59s");
        assert_eq!(fmt_duration(3599), "59m 59s");
        assert_eq!(fmt_duration(3 * 3600 + 5 * 60), "3h 05m");
        assert_eq!(fmt_duration(2 * 86400 + 4 * 3600), "2d 04h");
        assert_eq!(fmt_gib_pair(154 << 30, 256 << 30), "154/256 GiB");
        assert_eq!(
            fmt_gib_pair((88.1 * GIB) as u64, (95.6 * GIB) as u64),
            "88.1/95.6 GiB"
        );
        assert_eq!(
            fit_keeping_index("NVIDIA RTX PRO 6000 Blackwell #1", 19),
            "NVIDIA RTX PRO 6 #1"
        );
        assert_eq!(fit_keeping_index("Apple M5 Ultra", 19), "Apple M5 Ultra");
    }

    #[test]
    fn cells_and_bars_have_exact_widths() {
        let mut line = Line::default();
        line.cell("abc", 6, RULE)
            .rcell("12", 5, RULE)
            .bar(8, 0.5, RULE)
            .pad_to(25);
        assert_eq!(line.plain(), "abc     12 ━━━━────      ");
        assert_eq!(line.width(), 25);
    }
}
