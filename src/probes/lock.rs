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

//! Lock-file holder probe.
//!
//! A process that serializes GPU work through an advisory lock keeps the
//! lock file open for as long as it holds the lock. `lsof` lists those
//! processes without opening the file, so the probe can never take,
//! block, or perturb the lock the way a `flock(LOCK_NB)` test would.
//! Holder names come from `ps -o comm=`, which reflects a `setproctitle`
//! rename (e.g. a Python server that calls itself `omlx-server`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::utils::command_timeout::run_command_with_timeout;

/// Upper bound on one `lsof` or `ps` call. Both normally finish in about
/// 10 ms; the bound keeps a wedged filesystem from stalling the loop.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// One process holding the lock file open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolder {
    pub pid: u32,
    pub command: String,
}

/// The state of one watched lock file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockSample {
    pub path: String,
    /// Processes with the file open, ordered by pid. Empty means free.
    pub holders: Vec<LockHolder>,
    /// Unix time at which the current holder set was first observed;
    /// `None` while the lock is free.
    pub since_unix: Option<u64>,
}

impl LockSample {
    pub fn is_held(&self) -> bool {
        !self.holders.is_empty()
    }
}

pub struct LockWatcher {
    paths: Vec<PathBuf>,
    /// Per path: the holder pids last seen and when that set first appeared.
    first_seen: HashMap<String, (Vec<u32>, u64)>,
}

impl LockWatcher {
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            first_seen: HashMap::new(),
        }
    }

    /// Probe every watched path. A path whose probe fails (no `lsof`,
    /// timeout) is left out so the exporter omits it instead of claiming
    /// the lock is free.
    pub fn sample(&mut self) -> Vec<LockSample> {
        let now = unix_now();
        let mut out = Vec::with_capacity(self.paths.len());
        for path in &self.paths {
            let key = path.to_string_lossy().into_owned();
            let Some(mut holders) = lsof_holders(&key) else {
                continue;
            };
            resolve_display_names(&mut holders);
            let pids: Vec<u32> = holders.iter().map(|h| h.pid).collect();
            let since_unix = if pids.is_empty() {
                self.first_seen.remove(&key);
                None
            } else {
                let entry = self
                    .first_seen
                    .entry(key.clone())
                    .or_insert_with(|| (pids.clone(), now));
                if entry.0 != pids {
                    *entry = (pids, now);
                }
                Some(entry.1)
            };
            out.push(LockSample {
                path: key,
                holders,
                since_unix,
            });
        }
        out
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run `lsof` for one path. `None` when lsof could not run; an empty list
/// when nothing has the file open (lsof exits 1 with no output then).
fn lsof_holders(path: &str) -> Option<Vec<LockHolder>> {
    let args = ["-n", "-P", "-w", "-Fpc", "--", path];
    let output = ["lsof", "/usr/sbin/lsof", "/usr/bin/lsof"]
        .iter()
        .find_map(|bin| run_command_with_timeout(bin, &args, PROBE_TIMEOUT).ok())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() && !stdout.trim().is_empty() {
        return None;
    }
    Some(parse_lsof_fields(&stdout))
}

/// Parse `lsof -F pc` output: a `p<pid>` line opens each process set and
/// a `c<command>` line names it. File-set lines (`f`, `n`) are ignored.
pub(crate) fn parse_lsof_fields(text: &str) -> Vec<LockHolder> {
    let mut holders: Vec<LockHolder> = Vec::new();
    for line in text.lines() {
        if let Some(pid) = line.strip_prefix('p') {
            if let Ok(pid) = pid.trim().parse::<u32>()
                && !holders.iter().any(|h| h.pid == pid)
            {
                holders.push(LockHolder {
                    pid,
                    command: String::new(),
                });
            }
        } else if let Some(cmd) = line.strip_prefix('c')
            && let Some(last) = holders.last_mut()
            && last.command.is_empty()
        {
            last.command = sanitize(cmd);
        }
    }
    holders.sort_by_key(|h| h.pid);
    holders
}

/// Replace lsof's truncated command names with `ps -o comm=`, which shows
/// a process's `setproctitle` name. Keeps the lsof name when ps fails.
fn resolve_display_names(holders: &mut [LockHolder]) {
    if holders.is_empty() {
        return;
    }
    let pid_list = holders
        .iter()
        .map(|h| h.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let Ok(output) =
        run_command_with_timeout("ps", &["-o", "pid=,comm=", "-p", &pid_list], PROBE_TIMEOUT)
    else {
        return;
    };
    let names = parse_ps_comm(&String::from_utf8_lossy(&output.stdout));
    for holder in holders.iter_mut() {
        if let Some(name) = names.get(&holder.pid) {
            holder.command = name.clone();
        }
    }
}

/// Parse `ps -o pid=,comm=` lines into pid → executable basename.
pub(crate) fn parse_ps_comm(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, comm) = line.split_once(char::is_whitespace)?;
            let pid = pid.parse::<u32>().ok()?;
            let comm = comm.trim();
            let base = comm.rsplit('/').next().unwrap_or(comm);
            (!base.is_empty()).then(|| (pid, sanitize(base)))
        })
        .collect()
}

/// Drop control characters so a hostile process name cannot inject
/// terminal escapes into the viewer, and cap the length.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsof_field_output() {
        let out = "p35491\ncpython3.12\nf3\nn/Users/ian/llm/locks/gpu.lock\np12\ncother\nf4\n";
        let holders = parse_lsof_fields(out);
        assert_eq!(
            holders,
            vec![
                LockHolder {
                    pid: 12,
                    command: "other".to_string()
                },
                LockHolder {
                    pid: 35491,
                    command: "python3.12".to_string()
                },
            ]
        );
    }

    #[test]
    fn empty_lsof_output_means_free() {
        assert!(parse_lsof_fields("").is_empty());
    }

    #[test]
    fn parses_ps_comm_basename_and_rename() {
        let names = parse_ps_comm(
            "35489 /Users/ian/llm/.venv/bin/python\n35491 omlx-server      \n  7 /Apps/My App/x\n",
        );
        assert_eq!(names.get(&35489).map(String::as_str), Some("python"));
        assert_eq!(names.get(&35491).map(String::as_str), Some("omlx-server"));
        assert_eq!(names.get(&7).map(String::as_str), Some("x"));
    }

    #[test]
    fn sanitize_strips_escape_sequences() {
        assert_eq!(sanitize("evil\x1b[2Jname"), "evil[2Jname");
    }

    #[test]
    fn unwatched_free_file_reports_no_holders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("free.lock");
        std::fs::write(&path, b"").unwrap();
        let mut watcher = LockWatcher::new(vec![path.clone()]);
        let samples = watcher.sample();
        // Environments without lsof skip the path entirely.
        if let Some(sample) = samples.first() {
            assert_eq!(sample.path, path.to_string_lossy());
            assert!(!sample.is_held());
            assert_eq!(sample.since_unix, None);
        }
    }

    #[test]
    fn open_file_is_reported_with_this_process_as_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.lock");
        let _file = std::fs::File::create(&path).unwrap();
        let mut watcher = LockWatcher::new(vec![path]);
        let Some(sample) = watcher.sample().into_iter().next() else {
            return; // no lsof available
        };
        let me = std::process::id();
        assert!(sample.holders.iter().any(|h| h.pid == me), "{sample:?}");
        let first_since = sample.since_unix.expect("held lock has a since time");
        let again = watcher.sample().into_iter().next().unwrap();
        assert_eq!(again.since_unix, Some(first_since));
    }
}
