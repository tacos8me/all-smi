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

//! All tab device table: one group per host (a host line with OS, CPU and
//! RAM), one aligned row per accelerator under it, and an optional details
//! line per device (`x`).
//!
//! Columns give way in a fixed order as the terminal narrows: clock, the
//! memory-kind label, bar width down to a minimum, the name down to a
//! minimum, then the memory bar and finally the utilization bar.

use crossterm::style::Color;

use crate::ui::consolidated::model::{DeviceRow, HostSection, MemoryKind};
use crate::ui::theme::{self, CRIT, MUTED, OK, SUBTLE, TEXT};

use super::line::{Line, fit_keeping_index, fmt_gib_pair, fmt_watts};

const INDENT: usize = 3;
const MIN_NAME: usize = 20;
const MAX_NAME: usize = 36;
const MIN_BAR: usize = 8;
const MAX_BAR: usize = 32;
const UTIL_VALUE: usize = 6;
const MEM_TEXT: usize = 16;
const KIND: usize = 9;
const TEMP: usize = 7;
const POWER: usize = 12;
const CLOCK: usize = 11;

/// Column widths for one terminal width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Columns {
    pub name: usize,
    pub util_bar: usize,
    pub mem_bar: usize,
    pub kind: bool,
    pub clock: bool,
    pub power: bool,
    pub temp: bool,
}

impl Columns {
    /// `longest_name` is the widest device label on screen, so the name
    /// column is never wider than it needs to be.
    pub fn for_width(cols: usize, longest_name: usize) -> Self {
        let name = longest_name.clamp(MIN_NAME, MAX_NAME);
        let mut c = Self {
            name,
            util_bar: 0,
            mem_bar: 0,
            kind: true,
            clock: true,
            power: true,
            temp: true,
        };
        // Give way in priority order until two minimum bars fit; past the
        // name, only until one does.
        let mut steps = 0;
        loop {
            let want = if steps < 3 { 2 * MIN_BAR } else { MIN_BAR };
            if c.spare(cols) >= want as isize || steps == 5 {
                break;
            }
            match steps {
                0 => c.clock = false,
                1 => c.kind = false,
                2 => c.name = MIN_NAME.min(name),
                3 => c.power = false,
                _ => c.temp = false,
            }
            steps += 1;
        }
        let spare = c.spare(cols);
        if spare >= 2 * MIN_BAR as isize {
            let each = (spare as usize / 2).min(MAX_BAR);
            c.util_bar = each;
            c.mem_bar = each;
        } else if spare >= MIN_BAR as isize {
            c.util_bar = (spare as usize).min(MAX_BAR);
        }
        c
    }

    fn fixed(&self) -> usize {
        INDENT
            + self.name
            + 2
            + UTIL_VALUE
            + 1
            + MEM_TEXT
            + if self.kind { KIND } else { 0 }
            + if self.temp { TEMP } else { 0 }
            + if self.power { POWER } else { 0 }
            + if self.clock { CLOCK } else { 0 }
    }

    fn spare(&self, cols: usize) -> isize {
        cols as isize - self.fixed() as isize - 1
    }

    /// Display width of a full device row.
    #[cfg(test)]
    pub fn width(&self) -> usize {
        self.fixed() + self.util_bar + self.mem_bar
    }
}

pub fn column_header(c: &Columns) -> Line {
    let mut line = Line::default();
    line.text(&" ".repeat(INDENT), MUTED)
        .cell("DEVICE", c.name + 2, MUTED)
        .cell("UTIL", c.util_bar + UTIL_VALUE, MUTED)
        .text(" ", MUTED)
        .cell("MEMORY", c.mem_bar + MEM_TEXT, MUTED);
    if c.kind {
        line.cell("", KIND, MUTED);
    }
    if c.temp {
        line.rcell("TEMP", TEMP, MUTED);
    }
    if c.power {
        line.rcell("POWER", POWER, MUTED);
    }
    if c.clock {
        line.rcell("CLOCK", CLOCK, MUTED);
    }
    line
}

/// The host line: status dot, name, then OS / CPU / RAM facts.
pub fn host_line(host: &HostSection, cols: usize) -> Line {
    let mut line = Line::default();
    line.text(" ", MUTED)
        .text("● ", if host.connected { OK } else { CRIT })
        .text(&host.label, TEXT);
    if !host.connected {
        line.text("  unreachable", CRIT);
        if let Some(e) = &host.last_error {
            line.text(&format!(": {e}"), MUTED);
        }
        return line;
    }
    let mut facts: Vec<(String, Color)> = Vec::new();
    let mut platform = Vec::new();
    if let Some(os) = &host.os {
        platform.push(os.clone());
    }
    if let Some(cpu) = &host.cpu {
        platform.push(cpu.model.clone());
        if cpu.cores > 0 {
            platform.push(format!("{}-core CPU", cpu.cores));
        }
    }
    if !platform.is_empty() {
        facts.push((platform.join(" · "), MUTED));
    }
    if let Some(cpu) = &host.cpu {
        facts.push((format!("CPU {:.0}%", cpu.utilization), SUBTLE));
    }
    if host.ram_total > 0 {
        let pool = if host.has_unified_memory() {
            " unified"
        } else {
            ""
        };
        facts.push((
            format!("RAM {}{pool}", fmt_gib_pair(host.ram_used, host.ram_total)),
            SUBTLE,
        ));
    }
    if host.devices.is_empty() {
        facts.push(("no accelerators reported".to_string(), MUTED));
    }
    line.text("   ", MUTED);
    for (i, (text, color)) in facts.into_iter().enumerate() {
        if i > 0 {
            line.text("  ·  ", MUTED);
        }
        if line.width() + text.chars().count() > cols {
            break;
        }
        line.text(&text, color);
    }
    line
}

/// One device row. `dim` renders every value muted (a stale host, or a
/// device outside the active filter).
pub fn device_line(d: &DeviceRow, c: &Columns, dim: bool) -> Line {
    let tone = |color: Color| if dim { MUTED } else { color };
    let mut line = Line::default();
    line.text(&" ".repeat(INDENT), MUTED).cell(
        &fit_keeping_index(&d.label(), c.name),
        c.name + 2,
        tone(TEXT),
    );

    match d.utilization {
        Some(u) => {
            let level = theme::util_level(u);
            if c.util_bar > 0 {
                line.bar(c.util_bar, u / 100.0, tone(level.color()));
            }
            line.rcell(&format!("{u:.0}%"), UTIL_VALUE, tone(level.value_color()));
        }
        None => {
            if c.util_bar > 0 {
                line.bar(c.util_bar, 0.0, MUTED);
            }
            line.rcell("n/a", UTIL_VALUE, MUTED);
        }
    }

    line.text(" ", MUTED);
    let ratio = d.memory_ratio();
    let mem_level = theme::mem_level(ratio);
    if c.mem_bar > 0 {
        line.bar(c.mem_bar, ratio, tone(mem_level.color()));
        line.text(" ", MUTED);
        line.cell(
            &fmt_gib_pair(d.used_memory, d.total_memory),
            MEM_TEXT - 1,
            tone(mem_level.value_color()),
        );
    } else {
        line.cell(
            &fmt_gib_pair(d.used_memory, d.total_memory),
            MEM_TEXT,
            tone(mem_level.value_color()),
        );
    }
    if c.kind {
        let kind = match d.memory_kind {
            MemoryKind::Unified => "unified",
            MemoryKind::Dedicated => "VRAM",
        };
        line.cell(kind, KIND, MUTED);
    }

    if c.temp {
        match d.temperature_c {
            Some(t) => {
                let level = theme::temp_level(t, d.slowdown_c);
                line.rcell(&format!("{t}°C"), TEMP, tone(level.value_color()))
            }
            None => line.rcell("n/a", TEMP, MUTED),
        };
    }

    if c.power {
        let power = match (d.power_watts, d.power_limit_watts) {
            (Some(p), Some(limit)) => format!("{p:.0}/{limit:.0} W"),
            (Some(p), None) => fmt_watts(p),
            (None, _) => "n/a".to_string(),
        };
        let power_color = d
            .power_ratio()
            .map_or(TEXT, |r| theme::power_level(r).value_color());
        line.rcell(&power, POWER, tone(power_color));
    }

    if c.clock {
        let clock = d
            .frequency_mhz
            .map_or_else(|| "n/a".to_string(), |f| format!("{f} MHz"));
        line.rcell(&clock, CLOCK, tone(SUBTLE));
    }
    line
}

/// Details line (`x`): thresholds, P-state, driver, firmware, link; SoC
/// rails for Apple GPUs.
pub fn details_line(d: &DeviceRow, host: &HostSection) -> Option<Line> {
    let mut parts = Vec::new();
    if let Some(ane) = d.ane_watts {
        parts.push(format!("ANE {}", fmt_watts(ane)));
        if let Some(cpu) = host.cpu_power_watts {
            parts.push(format!("CPU {}", fmt_watts(cpu)));
        }
    }
    parts.extend(d.details.iter().cloned());
    if parts.is_empty() {
        return None;
    }
    let mut line = Line::default();
    line.text(&" ".repeat(INDENT + 2), MUTED)
        .text(&parts.join("  ·  "), MUTED);
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consolidated::model::tests::{mac_and_box, model_of};
    use crate::ui::text::display_width;

    #[test]
    fn columns_give_way_in_order() {
        let wide = Columns::for_width(200, 32);
        assert!(wide.clock && wide.kind);
        assert_eq!(wide.util_bar, MAX_BAR);
        let mid = Columns::for_width(120, 32);
        assert!(mid.clock && mid.kind, "{mid:?}");
        assert!(mid.util_bar >= MIN_BAR);
        let narrow = Columns::for_width(100, 32);
        assert!(!narrow.clock, "{narrow:?}");
        let tiny = Columns::for_width(80, 32);
        assert!(!tiny.clock && !tiny.kind);
        for cols in [60usize, 80, 100, 120, 160, 200] {
            let c = Columns::for_width(cols, 32);
            assert!(c.width() < cols, "{cols}: {c:?} is {}", c.width());
        }
    }

    #[test]
    fn rows_align_with_the_header() {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let c = Columns::for_width(160, 32);
        let header = column_header(&c).plain();
        let row = device_line(&model.hosts[1].devices[0], &c, false).plain();
        assert_eq!(display_width(&header), c.width());
        assert_eq!(display_width(&row), c.width());
        assert!(row.contains("NVIDIA RTX PRO 6000 Blackwell #0"), "{row}");
        assert!(row.contains("50%"), "{row}");
        assert!(row.contains("87.0/96.0 GiB"), "{row}");
        assert!(row.contains("VRAM"), "{row}");
        assert!(row.contains("100/600 W"), "{row}");
        let mac = device_line(&model.hosts[0].devices[0], &c, false).plain();
        assert!(mac.contains("Apple M5 Ultra · 80-core GPU"), "{mac}");
        assert!(mac.contains("154/256 GiB"), "{mac}");
        assert!(mac.contains("unified"), "{mac}");
    }

    #[test]
    fn down_host_line_names_the_error() {
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
        let line = host_line(&model.hosts[0], 160).plain();
        assert!(
            line.contains("ians-Mac-Studio  unreachable: Connection refused"),
            "{line}"
        );
    }
}
