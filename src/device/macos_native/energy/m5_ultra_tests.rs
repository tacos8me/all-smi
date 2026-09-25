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

//! Energy Model tests for an Apple M5 Ultra, next to the M1 Ultra ones in
//! `m1_ultra_tests.rs`.
//!
//! The inventory is a verbatim capture from a Mac17,15 on macOS 27.0
//! (26A428). It names its rails the way the M1 Ultra does, so the existing
//! rules already read it. The counter values and span in the cadence test
//! were measured on that machine, headless, whose `AppleT6050PMGR` publishes
//! its mJ channels once every 30 min.

use super::tests::{MS, assert_watts, counts, inventory, ms, obs};
use super::*;

/// `Energy Model` inventory of an Apple M5 Ultra (Mac17,15) on macOS 27.0.
const M5_ULTRA_ENERGY_MODEL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ioreport/m5_ultra_energy_model.tsv"
));

/// Synthetic watts per channel, chosen so that a wrong sum shows up: every
/// channel no rule sums draws 0.5 W, and `GPU0_0` differs from `GPU Energy`.
fn window_watts(name: &str) -> f64 {
    match name {
        "DIE_0_CPU Energy" => 3.1,
        "DIE_1_CPU Energy" => 1.4,
        "GPU Energy" => 0.0215,
        "GPU0_0" => 0.021,
        "ANE0_0" => 0.012,
        "ANE0_1" => 0.004,
        "DRAM0_0" => 1.9,
        "DRAM0_1" => 1.8,
        _ => 0.5,
    }
}

#[test]
fn m5_ultra_inventory_classifies_only_the_rail_channels() {
    let channels = inventory(M5_ULTRA_ENERGY_MODEL);
    assert_eq!(channels.len(), 725);
    for (unit, expected) in [("mJ", 714), ("uJ", 10), ("nJ", 1)] {
        assert_eq!(
            channels.iter().filter(|(_, u)| *u == unit).count(),
            expected,
            "{unit}"
        );
    }

    let classified: Vec<(&str, EnergyRail)> = channels
        .iter()
        .filter_map(|(name, _)| classify_energy_channel(name).map(|rail| (*name, rail)))
        .collect();
    assert_eq!(
        classified,
        vec![
            ("DIE_0_CPU Energy", EnergyRail::Cpu),
            ("DIE_1_CPU Energy", EnergyRail::Cpu),
            ("GPU0_0", EnergyRail::GpuFallback),
            ("ANE0_0", EnergyRail::Ane),
            ("DRAM0_0", EnergyRail::Dram),
            ("ANE0_1", EnergyRail::Ane),
            ("DRAM0_1", EnergyRail::Dram),
            ("GPU Energy", EnergyRail::Gpu),
        ],
        "the other 717 channels must classify to None"
    );
}

/// The M1 Ultra's `DIE_<n>_` layout holds here too: only CPU channels carry
/// the prefix, and no package channel sits beside the per-die ones.
#[test]
fn m5_ultra_die_prefix_is_cpu_only_and_no_package_channel_sits_beside_the_dies() {
    let channels = inventory(M5_ULTRA_ENERGY_MODEL);
    let has = |name: &str| channels.iter().any(|(n, _)| *n == name);

    let prefixed: Vec<&str> = channels
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with("DIE_"))
        .collect();
    assert_eq!(prefixed.len(), 92);
    for name in &prefixed {
        let block = name
            .strip_prefix("DIE_0_")
            .or_else(|| name.strip_prefix("DIE_1_"))
            .expect("an M5 Ultra has dies 0 and 1");
        assert!(
            ["MCPU", "MCPM", "PACC_", "PCPU", "PCPM"]
                .iter()
                .any(|cpu| block.starts_with(cpu))
                || block == "CPU Energy",
            "{name} is not a CPU channel"
        );
    }
    let rails: Vec<&str> = prefixed
        .iter()
        .copied()
        .filter(|name| classify_energy_channel(name).is_some())
        .collect();
    assert_eq!(rails, ["DIE_0_CPU Energy", "DIE_1_CPU Energy"]);

    for block in ["ANE0", "DRAM0", "ISP0", "AVE0", "MSR0", "DCS0", "AMCC0"] {
        assert!(
            has(&format!("{block}_0")) && has(&format!("{block}_1")),
            "{block}"
        );
    }
    for package in ["CPU Energy", "ANE", "ANE0", "DRAM", "DRAM0", "GPU", "GPU0"] {
        assert!(!has(package), "{package} would be counted next to the dies");
    }
}

/// `EnergyTracker` keys channels by name. This inventory lists 300 names
/// twice, all of them unclassified `DTL` telemetry.
#[test]
fn m5_ultra_duplicated_names_are_all_unclassified_telemetry() {
    let channels = inventory(M5_ULTRA_ENERGY_MODEL);
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for (name, _) in &channels {
        *seen.entry(name).or_default() += 1;
    }
    let duplicated: Vec<(&str, usize)> = seen.into_iter().filter(|(_, count)| *count > 1).collect();
    assert_eq!(duplicated.len(), 300);
    for (name, count) in duplicated {
        assert_eq!(count, 2, "{name}");
        assert!(name.contains("DTL"), "{name}");
        assert_eq!(classify_energy_channel(name), None, "{name}");
    }
}

/// The whole inventory through the tracker: CPU is the two
/// `DIE_<n>_CPU Energy` channels, GPU is `GPU Energy` once, and ANE and DRAM
/// are summed per die.
#[test]
fn m5_ultra_window_sums_the_dies_and_counts_the_gpu_once() {
    let channels = inventory(M5_ULTRA_ENERGY_MODEL);
    let t0 = 10_000 * MS;
    let t1 = t0 + 2_000 * MS;
    let mut tracker = EnergyTracker::default();
    tracker.observe_sample(
        t0 + MS,
        channels
            .iter()
            .map(|(name, unit)| obs(name, unit, 0, Some(t0))),
    );
    tracker.observe_sample(
        t1 + MS,
        channels.iter().map(|(name, unit)| {
            let value = counts(window_watts(name), unit, 2.0);
            obs(name, unit, value, Some(t1))
        }),
    );

    let readings = tracker.readings();
    assert_watts(readings.cpu, 3.1 + 1.4);
    assert_watts(readings.gpu, 0.0215);
    assert_watts(readings.ane, 0.012 + 0.004);
    assert_watts(readings.dram, 1.9 + 1.8);
}

/// The measured M5 Ultra cadence. `AppleT6050PMGR` published its mJ
/// channels at 17:35:34.215 and 18:05:34.173 and not in between, a span of
/// 1799.949 s over which `DIE_0_CPU Energy` gained 3 821 928 mJ and
/// `DIE_1_CPU Energy` 2 661 801 mJ, while `GPU Energy` kept publishing on
/// every sample. A tracker first sampling 522.6 s after a publication, as
/// all-smi did, has no CPU reading until the next one, then holds its 3.60 W
/// at the exporter's 3 s poll until two spans have passed without another.
/// Under the fixed 10 s hold it read 0 W after the first 10 s.
#[test]
fn m5_ultra_half_hourly_publication_is_held_until_the_next_one_is_due() {
    const SPAN_MS: f64 = 1_799_949.0;
    const FROZEN: (i64, i64) = (149_863_636, 63_132_475);
    const PUBLISHED: (i64, i64) = (3_821_928, 2_661_801);
    let dies = |values: (i64, i64), ts: u64| {
        [
            obs("DIE_0_CPU Energy", "mJ", values.0, Some(ts)),
            obs("DIE_1_CPU Energy", "mJ", values.1, Some(ts)),
        ]
    };
    let poll = 3_000 * MS;
    let first = 1_000 * MS;
    let second = first + ms(SPAN_MS);
    let mut tracker = EnergyTracker::default();

    let mut now = first + ms(522_600.0);
    while now < second {
        tracker.observe_sample(now, dies(FROZEN, first));
        assert_watts(tracker.readings().cpu, 0.0);
        now += poll;
    }

    let after = (FROZEN.0 + PUBLISHED.0, FROZEN.1 + PUBLISHED.1);
    let watts = (PUBLISHED.0 + PUBLISHED.1) as f64 * 1e-3 / (SPAN_MS / 1e3);
    assert!((watts - 3.602).abs() < 0.001, "{watts}");
    while now <= second + 2 * ms(SPAN_MS) {
        tracker.observe_sample(now, dies(after, second));
        assert_watts(tracker.readings().cpu, watts);
        now += poll;
    }
    tracker.observe_sample(now, dies(after, second));
    assert_watts(tracker.readings().cpu, 0.0);
}
