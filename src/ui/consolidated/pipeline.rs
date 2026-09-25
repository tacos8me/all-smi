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

//! Pipeline panel data (`view --icculus`): a model served across the
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

use serde::Deserialize;
use serde_json::Value;

/// Default endpoints for the Icculus deployment: the box engine on the
/// direct link and the Mac's llama-swap.
pub const DEFAULT_HEALTH_URL: &str = "http://10.10.10.1:10051/health";
pub const DEFAULT_SWAP_URL: &str = "http://10.10.10.2:8080";

/// Where the pipeline panel reads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineConfig {
    pub title: String,
    pub health_url: String,
    /// llama-swap base URL; `/running` is appended.
    pub swap_url: String,
}

impl PipelineConfig {
    pub fn icculus(health_url: Option<String>, swap_url: Option<String>) -> Self {
        Self {
            title: "Icculus pipeline".to_string(),
            health_url: health_url.unwrap_or_else(|| DEFAULT_HEALTH_URL.to_string()),
            swap_url: swap_url.unwrap_or_else(|| DEFAULT_SWAP_URL.to_string()),
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
}

impl PipelineStatus {
    pub fn new(config: PipelineConfig) -> Self {
        Self {
            config,
            engine: Probe::Pending,
            swap: Probe::Pending,
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
            "name":"Icculus","description":"d"}]}"#;
        let models = parse_swap_running(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "ds41");
        assert_eq!(models[0].state, "ready");
        assert_eq!(models[0].name, "Icculus");
        assert!(parse_swap_running(br#"{"running":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn swap_url_gets_running_path() {
        let c = PipelineConfig::icculus(None, Some("http://mac:8080/".to_string()));
        assert_eq!(c.swap_running_url(), "http://mac:8080/running");
        assert_eq!(c.health_url, DEFAULT_HEALTH_URL);
    }
}
