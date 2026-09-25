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

//! Pipeline panel data (`view --icculis`): a model served across the
//! machines, seen through two read-only HTTP endpoints.
//!
//! * the box engine's `/health` JSON (sessions, connections, the GPU job
//!   in flight, queue depth, prefix-cache counters, build identity);
//! * llama-swap's `/running` on the Mac (which model is loaded, and its
//!   state).
//!
//! The lock holder and link throughput come from the exporters' host
//! probes, not from here. Every field is optional so an endpoint that
//! grows or drops a key degrades to a blank instead of a parse failure.

use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;

/// Default endpoints for the Icculis deployment: the box engine on the
/// direct link and the Mac's llama-swap.
pub const DEFAULT_HEALTH_URL: &str = "http://10.10.10.1:10051/health";
pub const DEFAULT_SWAP_URL: &str = "http://10.10.10.2:8080";

/// Where the pipeline panel reads from, and how its two halves are named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineConfig {
    pub title: String,
    /// Model served, shown after the title.
    pub subtitle: String,
    pub health_url: String,
    /// llama-swap base URL; `/running` is appended.
    pub swap_url: String,
    /// The engine host (first half: prefill and the early layers of
    /// every decode step) and the host that finishes each step.
    pub front: Stage,
    pub back: Stage,
    /// Name of the link between them.
    pub link: String,
    /// JSON probe names the back host's exporter publishes
    /// (`api --json-probe`): the serving supervisor's `/health`, the
    /// worker's split-pipeline counters, and its server status.
    pub probe_supervisor: String,
    pub probe_split: String,
    pub probe_server: String,
}

/// One half of the split.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    /// Short machine name ("RTX box").
    pub name: String,
    /// What it runs ("layers 0-19").
    pub role: String,
    /// The role in a few characters, for the one-line strip ("L0-19").
    pub short: String,
}

impl PipelineConfig {
    pub fn icculis(health_url: Option<String>, swap_url: Option<String>) -> Self {
        Self {
            title: "Icculis".to_string(),
            subtitle: "DeepSeek-V4.1-Flash · original weights".to_string(),
            health_url: health_url.unwrap_or_else(|| DEFAULT_HEALTH_URL.to_string()),
            swap_url: swap_url.unwrap_or_else(|| DEFAULT_SWAP_URL.to_string()),
            front: Stage {
                name: "RTX box".to_string(),
                role: "layers 0-19 · prefill + verify".to_string(),
                short: "L0-19".to_string(),
            },
            back: Stage {
                name: "M5 Ultra".to_string(),
                role: "layers 20-39 · head · DSpark draft".to_string(),
                short: "L20-39 + head + draft".to_string(),
            },
            link: "10GbE".to_string(),
            probe_supervisor: "sup".to_string(),
            probe_split: "og".to_string(),
            probe_server: "omlx".to_string(),
        }
    }

    pub fn swap_running_url(&self) -> String {
        format!("{}/running", self.swap_url.trim_end_matches('/'))
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct EngineHealth {
    pub ok: Option<bool>,
    pub encoder: Option<String>,
    pub step_api: Option<String>,
    pub sessions: Option<u64>,
    pub connections: Option<u64>,
    /// `null` when idle; otherwise whatever the engine reports for the job
    /// holding the GPU.
    pub gpu_job: Option<Value>,
    pub gpu_job_s: Option<f64>,
    pub queued_jobs: Option<u64>,
    pub cache: Option<EngineCache>,
    pub uptime_s: Option<f64>,
    pub version: Option<String>,
    pub numerics: Option<String>,
    pub restart_pending: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct EngineCache {
    pub lookups: Option<u64>,
    pub hits: Option<u64>,
    pub resumed_tokens: Option<u64>,
    pub entries: Option<u64>,
    pub bytes: Option<u64>,
    pub budget: Option<u64>,
}

impl EngineCache {
    /// Hit ratio in `[0, 1]`, `None` before the first lookup.
    pub fn hit_ratio(&self) -> Option<f64> {
        match (self.hits, self.lookups) {
            (Some(h), Some(l)) if l > 0 => Some(h as f64 / l as f64),
            _ => None,
        }
    }
}

impl EngineHealth {
    pub fn parse(body: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(body).map_err(|e| format!("bad /health JSON: {e}"))
    }

    /// Short description of the GPU job, `None` when idle.
    pub fn gpu_job_label(&self) -> Option<String> {
        match self.gpu_job.as_ref()? {
            Value::Null => None,
            Value::String(s) => Some(sanitize(s)),
            Value::Object(map) => {
                // Prefer a human field when the engine reports a structure.
                ["name", "kind", "type", "job", "session"]
                    .iter()
                    .find_map(|k| map.get(*k))
                    .map(|v| match v {
                        Value::String(s) => sanitize(s),
                        other => sanitize(&other.to_string()),
                    })
                    .or_else(|| Some(sanitize(&Value::Object(map.clone()).to_string())))
            }
            other => Some(sanitize(&other.to_string())),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.gpu_job_label().is_some()
    }

    /// The GPU job is part of a prefill: chunks, the vision encoder, a
    /// prefix-cache restore or the snapshot that saves one.
    pub fn is_prefill(&self) -> bool {
        self.gpu_job_label().is_some_and(|job| {
            ["prefill", "vision", "restore", "snapshot"]
                .iter()
                .any(|k| job.starts_with(k))
        })
    }
}

/// One llama-swap `/running` entry.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SwapModel {
    pub model: String,
    pub state: String,
    pub name: String,
}

#[derive(Deserialize)]
struct SwapRunning {
    #[serde(default)]
    running: Vec<SwapModel>,
}

pub fn parse_swap_running(body: &[u8]) -> Result<Vec<SwapModel>, String> {
    let parsed: SwapRunning =
        serde_json::from_slice(body).map_err(|e| format!("bad /running JSON: {e}"))?;
    Ok(parsed
        .running
        .into_iter()
        .take(16)
        .map(|m| SwapModel {
            model: sanitize(&m.model),
            state: sanitize(&m.state),
            name: sanitize(&m.name),
        })
        .collect())
}

/// Result of the latest poll of one endpoint.
#[derive(Clone, Debug, PartialEq)]
pub enum Probe<T> {
    Pending,
    Ok(T),
    Err(String),
}

/// What the pipeline panel shows. Lives in `ConsolidatedState`.
#[derive(Clone, Debug, PartialEq)]
pub struct PipelineStatus {
    pub config: PipelineConfig,
    pub engine: Probe<EngineHealth>,
    pub swap: Probe<Vec<SwapModel>>,
    /// First poll of the prefill in progress, if one is.
    pub prefill_since: Option<Instant>,
    /// How long the prefill in progress has run, as of the last poll.
    pub prefill_elapsed: Option<Duration>,
    /// Duration of the last finished prefill (as seen by the poller).
    pub last_prefill: Option<Duration>,
}

impl PipelineStatus {
    pub fn new(config: PipelineConfig) -> Self {
        Self {
            config,
            engine: Probe::Pending,
            swap: Probe::Pending,
            prefill_since: None,
            prefill_elapsed: None,
            last_prefill: None,
        }
    }

    /// Track the prefill phase across polls at time `now`.
    pub fn observe_phase(&mut self, now: Instant) {
        let prefill = matches!(&self.engine, Probe::Ok(h) if h.is_prefill());
        if prefill {
            let since = *self.prefill_since.get_or_insert(now);
            self.prefill_elapsed = Some(now.saturating_duration_since(since));
        } else if self.prefill_since.take().is_some() {
            self.last_prefill = self.prefill_elapsed.take();
        }
    }
}

/// Drop control characters (terminal escapes) and cap the length of any
/// string an endpoint hands us before it reaches the screen.
fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(160).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEALTH: &str = r#"{"ok": true, "encoder": "up", "step_api": "up", "token_map": true,
        "sessions": 0, "connections": 1, "gpu_job": null, "gpu_job_s": 0.0, "queued_jobs": 0,
        "cache": {"lookups": 406, "hits": 301, "resumed_tokens": 5385728, "saved": 420,
        "evicted": 0, "loaded": 357, "dropped_at_load": 0, "entries": 777, "blocks": 582,
        "bytes": 29383016290, "budget": 68719476736, "root": "/dev/shm/x"},
        "uptime_s": 4227, "version": "d11dccf", "sglang": "a618602", "numerics": "og-s4.3",
        "tree_head": "d11dccf", "restart_pending": false, "dev_hook": false}"#;

    #[test]
    fn parses_engine_health() {
        let h = EngineHealth::parse(HEALTH.as_bytes()).unwrap();
        assert_eq!(h.ok, Some(true));
        assert_eq!(h.connections, Some(1));
        assert_eq!(h.queued_jobs, Some(0));
        assert!(!h.is_busy());
        assert_eq!(h.version.as_deref(), Some("d11dccf"));
        assert_eq!(h.numerics.as_deref(), Some("og-s4.3"));
        let cache = h.cache.unwrap();
        assert!((cache.hit_ratio().unwrap() - 301.0 / 406.0).abs() < 1e-12);
        assert_eq!(cache.budget, Some(68_719_476_736));
    }

    #[test]
    fn gpu_job_labels() {
        let busy = EngineHealth::parse(br#"{"gpu_job": "prefill s12", "gpu_job_s": 1.5}"#).unwrap();
        assert_eq!(busy.gpu_job_label().as_deref(), Some("prefill s12"));
        let obj = EngineHealth::parse(br#"{"gpu_job": {"kind": "step", "n": 3}}"#).unwrap();
        assert_eq!(obj.gpu_job_label().as_deref(), Some("step"));
        let evil = EngineHealth::parse(b"{\"gpu_job\": \"a\\u001b[2Jb\"}").unwrap();
        assert_eq!(evil.gpu_job_label().as_deref(), Some("a[2Jb"));
    }

    #[test]
    fn missing_and_unknown_fields_are_tolerated() {
        let h = EngineHealth::parse(br#"{"new_field": [1, 2], "sessions": 3}"#).unwrap();
        assert_eq!(h.sessions, Some(3));
        assert_eq!(h.cache, None);
        assert!(EngineHealth::parse(b"not json").is_err());
    }

    #[test]
    fn parses_llama_swap_running() {
        let body = br#"{"running":[{"model":"ds41","state":"ready","cmd":"x","proxy":"y","ttl":0,
            "name":"Icculis","description":"d"}]}"#;
        let models = parse_swap_running(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "ds41");
        assert_eq!(models[0].state, "ready");
        assert_eq!(models[0].name, "Icculis");
        assert!(parse_swap_running(br#"{"running":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn prefill_phase_is_timed_across_polls() {
        let mut p = PipelineStatus::new(PipelineConfig::icculis(None, None));
        let t0 = Instant::now();
        let prefill =
            EngineHealth::parse(br#"{"gpu_job": "prefill_chunk", "gpu_job_s": 0.4}"#).unwrap();
        assert!(prefill.is_prefill());
        p.engine = Probe::Ok(prefill.clone());
        p.observe_phase(t0);
        p.observe_phase(t0 + Duration::from_secs(3));
        assert_eq!(p.prefill_elapsed, Some(Duration::from_secs(3)));
        let step = EngineHealth::parse(br#"{"gpu_job": "step"}"#).unwrap();
        assert!(!step.is_prefill());
        p.engine = Probe::Ok(step);
        p.observe_phase(t0 + Duration::from_secs(4));
        assert_eq!(p.prefill_since, None);
        assert_eq!(p.prefill_elapsed, None);
        assert_eq!(p.last_prefill, Some(Duration::from_secs(3)));
    }

    #[test]
    fn swap_url_gets_running_path() {
        let c = PipelineConfig::icculis(None, Some("http://mac:8080/".to_string()));
        assert_eq!(c.swap_running_url(), "http://mac:8080/running");
        assert_eq!(c.health_url, DEFAULT_HEALTH_URL);
    }
}
