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

//! Palette for the cluster views (header, All tab, Consolidated tab).
//!
//! Neutral greys carry structure, one muted teal accent marks headings,
//! the selected tab and the active pipeline phase, and green / amber / red
//! appear only where a reading crosses a threshold. Every color the
//! cluster views use comes from here; `NO_COLOR` turns all of them off
//! (see [`color_enabled`]).
//!
//! 256-color indices are used rather than truecolor so the palette looks
//! the same in tmux, over SSH, and in terminals without `COLORTERM`.

use std::sync::OnceLock;

use crossterm::style::Color;

/// Values and names (zinc-200).
pub const TEXT: Color = Color::AnsiValue(254);
/// Secondary values: units, detail rows, sub-lines (zinc-400).
pub const SUBTLE: Color = Color::AnsiValue(248);
/// Labels and column headers (zinc-500).
pub const MUTED: Color = Color::AnsiValue(243);
/// Rules, bar tracks, empty history (zinc-700).
pub const RULE: Color = Color::AnsiValue(238);
/// The one accent: headings, the selected tab, the active pipeline phase.
pub const ACCENT: Color = Color::AnsiValue(73);
/// Reading within its normal range.
pub const OK: Color = Color::AnsiValue(71);
/// Reading approaching its limit.
pub const WARN: Color = Color::AnsiValue(179);
/// Reading at or past its limit, or a host that is down.
pub const CRIT: Color = Color::AnsiValue(167);
/// Text on an accent background (the selected tab).
pub const ON_ACCENT: Color = Color::AnsiValue(233);

/// Utilization thresholds, in percent.
pub const UTIL_WARN: f64 = 70.0;
pub const UTIL_CRIT: f64 = 90.0;
/// Memory thresholds, as a fraction of capacity. Inference servers keep
/// VRAM nearly full by design, so memory turns amber later than compute.
pub const MEM_WARN: f64 = 0.85;
pub const MEM_CRIT: f64 = 0.95;
/// Power thresholds, as a fraction of the board limit.
pub const POWER_WARN: f64 = 0.70;
pub const POWER_CRIT: f64 = 0.90;
/// Temperature margins below the device's slowdown threshold.
pub const TEMP_WARN_MARGIN: u32 = 15;
pub const TEMP_CRIT_MARGIN: u32 = 5;
/// Slowdown threshold assumed when a device reports none (Apple SoCs).
pub const TEMP_DEFAULT_SLOWDOWN: u32 = 100;

/// Three-state reading level; `color()` maps it onto the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    Crit,
}

impl Level {
    pub fn color(self) -> Color {
        match self {
            Self::Ok => OK,
            Self::Warn => WARN,
            Self::Crit => CRIT,
        }
    }

    /// Color for the number itself: plain text while the reading is
    /// normal, so green stays on the bars and the screen stays calm.
    pub fn value_color(self) -> Color {
        match self {
            Self::Ok => TEXT,
            other => other.color(),
        }
    }

    fn from_thresholds(value: f64, warn: f64, crit: f64) -> Self {
        if value >= crit {
            Self::Crit
        } else if value >= warn {
            Self::Warn
        } else {
            Self::Ok
        }
    }
}

pub fn util_level(percent: f64) -> Level {
    Level::from_thresholds(percent, UTIL_WARN, UTIL_CRIT)
}

pub fn mem_level(ratio: f64) -> Level {
    Level::from_thresholds(ratio, MEM_WARN, MEM_CRIT)
}

pub fn power_level(ratio: f64) -> Level {
    Level::from_thresholds(ratio, POWER_WARN, POWER_CRIT)
}

/// Temperature against the device's slowdown threshold (or
/// [`TEMP_DEFAULT_SLOWDOWN`] when it reports none).
pub fn temp_level(celsius: u32, slowdown: Option<u32>) -> Level {
    let limit = slowdown.filter(|s| *s > 0).unwrap_or(TEMP_DEFAULT_SLOWDOWN);
    if celsius + TEMP_CRIT_MARGIN >= limit {
        Level::Crit
    } else if celsius + TEMP_WARN_MARGIN >= limit {
        Level::Warn
    } else {
        Level::Ok
    }
}

/// False when `NO_COLOR` is set to a non-empty value
/// (<https://no-color.org>). Read once per process.
pub fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()))
}

/// Filled and empty glyphs of the one bar style every cluster view uses.
pub const BAR_FILL: char = '━';
pub const BAR_TRACK: char = '─';

/// Split a bar of `width` cells at `ratio` (clamped to `0..=1`). A
/// non-zero reading always shows at least one filled cell.
pub fn bar_cells(width: usize, ratio: f64) -> (usize, usize) {
    let ratio = if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut filled = (ratio * width as f64).round() as usize;
    if ratio > 0.0 && filled == 0 && width > 0 {
        filled = 1;
    }
    let filled = filled.min(width);
    (filled, width - filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds() {
        assert_eq!(util_level(10.0), Level::Ok);
        assert_eq!(util_level(70.0), Level::Warn);
        assert_eq!(util_level(95.0), Level::Crit);
        assert_eq!(mem_level(0.80), Level::Ok);
        assert_eq!(mem_level(0.92), Level::Warn);
        assert_eq!(mem_level(0.96), Level::Crit);
        assert_eq!(power_level(0.5), Level::Ok);
        assert_eq!(power_level(0.95), Level::Crit);
        assert_eq!(temp_level(35, Some(95)), Level::Ok);
        assert_eq!(temp_level(80, Some(95)), Level::Warn);
        assert_eq!(temp_level(91, Some(95)), Level::Crit);
        assert_eq!(temp_level(84, None), Level::Ok);
        assert_eq!(temp_level(85, None), Level::Warn);
    }

    #[test]
    fn value_color_is_plain_when_normal() {
        assert_eq!(Level::Ok.value_color(), TEXT);
        assert_eq!(Level::Crit.value_color(), CRIT);
    }

    #[test]
    fn bar_cells_clamp_and_show_small_readings() {
        assert_eq!(bar_cells(10, 0.0), (0, 10));
        assert_eq!(bar_cells(10, 0.01), (1, 9));
        assert_eq!(bar_cells(10, 0.55), (6, 4));
        assert_eq!(bar_cells(10, 2.0), (10, 0));
        assert_eq!(bar_cells(10, f64::NAN), (0, 10));
        assert_eq!(bar_cells(0, 0.5), (0, 0));
    }
}
