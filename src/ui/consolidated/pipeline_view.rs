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

//! What each half of a split pipeline is doing right now.
//!
//! The front host (the engine) runs prefill and the early layers of every
//! decode step; the back host finishes each step (the late layers, the
//! head, the drafter). The phases come from what is measurable:
//!
//! * front: the engine's `/health` GPU job (a prefill chunk, a restore, a
//!   step) and how long the poller has seen a prefill running;
//! * back: the JSON probes its exporter publishes (`api --json-probe`):
//!   the split worker's step counter rate, the supervisor's in-flight
//!   count, and the server's token counters;
//! * link: the interface rates both exporters publish, per direction.
//!
//! Decode tokens per second are estimated from the step rate and the
//! worker's lifetime tokens per step (completion tokens over steps), and
//! are shown with `≈`.

use crate::probes::HostProbes;

use super::model::ConsolidatedModel;
use super::pipeline::{PipelineStatus, Probe};

/// Step rate (per second) below which the back host counts as idle.
const DECODE_MIN_STEPS: f64 = 0.2;
/// GPU utilization (%) above which a back host that is not serving is
/// shown as busy with other work.
const BUSY_GPU_UTIL: f64 = 20.0;
/// Link traffic (bytes/s) that, with a session open, means steps are
/// flowing between polls.
const LINK_BUSY: f64 = 1e6;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Endpoint or exporter unreachable.
    Down(String),
    /// No reading yet, or no probe configured.
    Unknown(String),
    Idle(String),
    /// Request in flight but this half has nothing to compute yet.
    Waiting(String),
    Prefill {
        elapsed_s: Option<f64>,
        job: String,
        job_s: Option<f64>,
    },
    Decode(String),
    /// Some other GPU job (a close, a barrier).
    Busy(String),
}

impl Phase {
    /// The phase is doing work: drawn in the accent color.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Prefill { .. } | Self::Decode(_) | Self::Busy(_))
    }

    /// Upper-case phase word and detail text.
    pub fn words(&self) -> (&'static str, String) {
        match self {
            Self::Down(e) => ("DOWN", e.clone()),
            Self::Unknown(e) => ("", e.clone()),
            Self::Idle(d) => ("idle", d.clone()),
            Self::Waiting(d) => ("waiting", d.clone()),
            Self::Prefill {
                elapsed_s,
                job,
                job_s,
            } => {
                let mut detail = elapsed_s.map_or(String::new(), |s| format!("{s:.0}s"));
                let chunk = match job_s {
                    Some(s) => format!("{job} {s:.1}s"),
                    None => job.clone(),
                };
                if detail.is_empty() {
                    detail = chunk;
                } else {
                    detail = format!("{detail} · {chunk}");
                }
                ("PREFILL", detail)
            }
            Self::Decode(d) => ("DECODE", d.clone()),
            Self::Busy(job) => ("BUSY", job.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinkUse {
    Idle,
    /// Front → back: the prefill state streaming over.
    PrefillState,
    /// Per-step activations both ways.
    Steps,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PipelineView {
    pub front_host: String,
    pub back_host: String,
    pub front: Phase,
    pub back: Phase,
    /// Bytes per second front → back and back → front.
    pub to_back: Option<f64>,
    pub to_front: Option<f64>,
    pub link: LinkUse,
}

impl PipelineView {
    pub fn build(model: &ConsolidatedModel, p: &PipelineStatus, probes: &[HostProbes]) -> Self {
        let front_id = host_for_url(model, &p.config.health_url);
        let back_id = host_for_url(model, &p.config.swap_url);
        let label = |id: &Option<String>| {
            id.as_deref()
                .map_or_else(|| "?".to_string(), |h| model.host_label(h))
        };
        let probe_of = |id: &Option<String>| {
            id.as_deref()
                .and_then(|h| probes.iter().find(|p| p.host_id == h))
        };
        let front_probes = probe_of(&front_id);
        let back_probes = probe_of(&back_id);
        let back_up = back_id
            .as_deref()
            .and_then(|h| model.hosts.iter().find(|s| s.host_id == h))
            .is_none_or(|s| s.connected);

        let split = back_probes.and_then(|b| b.json_probe(&p.config.probe_split));
        let sup = back_probes.and_then(|b| b.json_probe(&p.config.probe_supervisor));
        let server = back_probes.and_then(|b| b.json_probe(&p.config.probe_server));
        let steps = split
            .filter(|s| s.up)
            .and_then(|s| s.rate("steps"))
            .unwrap_or(0.0);
        let rows = split.and_then(|s| s.rate("rows")).unwrap_or(0.0);
        let decoding = steps >= DECODE_MIN_STEPS;
        let in_flight = sup.and_then(|s| s.value("inflight")).unwrap_or(0.0) > 0.0
            || server
                .and_then(|s| s.value("active_requests"))
                .unwrap_or(0.0)
                > 0.0;

        let to_back = link_rate(front_probes, true).or_else(|| link_rate(back_probes, false));
        let to_front = link_rate(back_probes, true).or_else(|| link_rate(front_probes, false));
        let link_busy = [to_back, to_front]
            .into_iter()
            .flatten()
            .any(|r| r >= LINK_BUSY);

        let front = match &p.engine {
            Probe::Pending => Phase::Unknown("waiting for /health".to_string()),
            Probe::Err(e) => Phase::Down(e.clone()),
            Probe::Ok(h) if h.is_prefill() => Phase::Prefill {
                elapsed_s: p.prefill_elapsed.map(|d| d.as_secs_f64()),
                job: h.gpu_job_label().unwrap_or_default(),
                job_s: h.gpu_job_s,
            },
            Probe::Ok(_) if decoding => Phase::Decode(format!("verify {rows:.0} rows/s")),
            Probe::Ok(h) => match h.gpu_job_label() {
                Some(job) if job == "step" => Phase::Decode("verify step".to_string()),
                Some(job) => Phase::Busy(job),
                // Steps last milliseconds and fall between polls; an open
                // session with traffic on the link is decoding.
                None if h.sessions.unwrap_or(0) > 0 && link_busy => {
                    Phase::Decode("steps on the link".to_string())
                }
                None => {
                    let sessions = h.sessions.unwrap_or(0);
                    let detail = if sessions > 0 {
                        format!(
                            "{sessions} session{} open",
                            if sessions == 1 { "" } else { "s" }
                        )
                    } else if let Some(d) = p.last_prefill {
                        format!("last prefill {:.1}s", d.as_secs_f64())
                    } else {
                        String::new()
                    };
                    Phase::Idle(detail)
                }
            },
        };

        let back = if !back_up {
            Phase::Down("exporter unreachable".to_string())
        } else if split.is_none() && sup.is_none() {
            Phase::Unknown("no phase probe (api --json-probe)".to_string())
        } else if !split.is_some_and(|s| s.up) && !sup.is_some_and(|s| s.up) {
            // The exporter answers but the serving process does not. The
            // GPU may still be busy with work outside the serving stack
            // (a benchmark holding the lock): say so rather than "idle".
            let gpu = back_id
                .as_deref()
                .and_then(|h| model.hosts.iter().find(|s| s.host_id == h))
                .and_then(|s| {
                    s.devices
                        .iter()
                        .filter_map(|d| d.utilization)
                        .reduce(f64::max)
                });
            let holder = back_probes
                .and_then(|b| b.locks.iter().find_map(|l| l.holders.first()))
                .map(|h| format!(" · lock: {} (pid {})", h.command, h.pid))
                .unwrap_or_default();
            match gpu {
                Some(u) if u >= BUSY_GPU_UTIL => {
                    Phase::Busy(format!("not serving · GPU {u:.0}%{holder}"))
                }
                _ => Phase::Idle(format!("not serving{holder}")),
            }
        } else if decoding {
            let per_step = match (
                server.and_then(|s| s.value("total_completion_tokens")),
                split.and_then(|s| s.value("steps")),
            ) {
                (Some(tokens), Some(total)) if total > 0.0 && tokens > 0.0 => Some(tokens / total),
                _ => None,
            };
            Phase::Decode(match per_step {
                Some(k) => format!("≈{:.0} tok/s · {steps:.0} steps/s", steps * k),
                None => format!("{steps:.0} steps/s"),
            })
        } else if in_flight {
            if matches!(front, Phase::Prefill { .. }) {
                Phase::Waiting("for the prefill state".to_string())
            } else {
                Phase::Busy("request in flight".to_string())
            }
        } else {
            let avg = server
                .and_then(|s| s.value("avg_generation_tps"))
                .filter(|v| *v > 0.0);
            Phase::Idle(avg.map_or(String::new(), |v| format!("avg {v:.0} tok/s")))
        };

        let link = match (&front, &back) {
            (Phase::Prefill { .. }, _) => LinkUse::PrefillState,
            (_, Phase::Decode(_)) | (Phase::Decode(_), _) => LinkUse::Steps,
            _ => LinkUse::Idle,
        };

        Self {
            front_host: label(&front_id),
            back_host: label(&back_id),
            front,
            back,
            to_back,
            to_front,
            link,
        }
    }
}

/// Transmit (or receive) rate of a host's first probed interface.
fn link_rate(probes: Option<&HostProbes>, tx: bool) -> Option<f64> {
    let iface = probes?.interfaces.first()?;
    if tx {
        iface.tx_bytes_per_sec
    } else {
        iface.rx_bytes_per_sec
    }
}

/// The scraped host whose address matches the URL's host, if any.
fn host_for_url(model: &ConsolidatedModel, url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    model
        .hosts
        .iter()
        .find(|h| {
            crate::common::http_hosts::host_identifier(&h.host_id)
                .rsplit_once(':')
                .is_some_and(|(ip, _)| ip == host)
        })
        .map(|h| h.host_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probes::JsonProbeSample;
    use crate::probes::net::NetInterfaceSample;
    use crate::ui::consolidated::model::tests::{mac_and_box, model_of};
    use crate::ui::consolidated::pipeline::{EngineHealth, PipelineConfig};
    use std::time::Duration;

    fn json(name: &str, values: &[(&str, f64)], rates: &[(&str, f64)]) -> JsonProbeSample {
        JsonProbeSample {
            name: name.to_string(),
            up: true,
            values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            rates: rates.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    fn probes(steps_rate: f64, inflight: f64, box_tx: f64) -> Vec<HostProbes> {
        vec![
            HostProbes {
                host_id: "10.10.10.1:9090".to_string(),
                interfaces: vec![NetInterfaceSample {
                    interface: "enp".to_string(),
                    tx_bytes_per_sec: Some(box_tx),
                    rx_bytes_per_sec: Some(2_000.0),
                    ..Default::default()
                }],
                ..Default::default()
            },
            HostProbes {
                host_id: "10.10.10.2:9090".to_string(),
                json: vec![
                    json("sup", &[("inflight", inflight)], &[]),
                    json(
                        "og",
                        &[("steps", 100.0)],
                        &[("steps", steps_rate), ("rows", 90.0)],
                    ),
                    json(
                        "omlx",
                        &[
                            ("total_completion_tokens", 200.0),
                            ("avg_generation_tps", 71.4),
                        ],
                        &[],
                    ),
                ],
                ..Default::default()
            },
        ]
    }

    fn status(job: Option<&str>) -> PipelineStatus {
        let mut p = PipelineStatus::new(PipelineConfig::icculis(None, None));
        let body = match job {
            Some(j) => format!(r#"{{"gpu_job": "{j}", "gpu_job_s": 0.5, "sessions": 1}}"#),
            None => r#"{"gpu_job": null, "sessions": 0}"#.to_string(),
        };
        p.engine = Probe::Ok(EngineHealth::parse(body.as_bytes()).unwrap());
        p
    }

    #[test]
    fn prefill_on_the_box_while_the_mac_waits() {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let mut p = status(Some("prefill_chunk"));
        p.prefill_elapsed = Some(Duration::from_secs(8));
        let v = PipelineView::build(&model, &p, &probes(0.0, 1.0, 1.1e9));
        assert_eq!(v.front_host, "vllm");
        assert_eq!(v.back_host, "ians-Mac-Studio");
        assert_eq!(
            v.front.words(),
            ("PREFILL", "8s · prefill_chunk 0.5s".to_string())
        );
        assert_eq!(v.back, Phase::Waiting("for the prefill state".to_string()));
        assert_eq!(v.link, LinkUse::PrefillState);
        assert_eq!(v.to_back, Some(1.1e9));
        assert_eq!(v.to_front, Some(2_000.0));
    }

    #[test]
    fn decode_on_both_with_estimated_tokens_per_second() {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let v = PipelineView::build(&model, &status(None), &probes(30.0, 1.0, 5e6));
        assert_eq!(v.back, Phase::Decode("≈60 tok/s · 30 steps/s".to_string()));
        assert_eq!(v.front, Phase::Decode("verify 90 rows/s".to_string()));
        assert_eq!(v.link, LinkUse::Steps);
        assert!(v.front.is_active() && v.back.is_active());

        // No step caught by the poll, but a session with a busy link.
        let mut p = PipelineStatus::new(PipelineConfig::icculis(None, None));
        p.engine = Probe::Ok(EngineHealth::parse(br#"{"gpu_job": null, "sessions": 1}"#).unwrap());
        let v = PipelineView::build(&model, &p, &probes(0.0, 0.0, 5e6));
        assert_eq!(v.front, Phase::Decode("steps on the link".to_string()));
    }

    #[test]
    fn idle_and_unprobed() {
        let (gpus, tabs, statuses) = mac_and_box();
        let model = model_of(&gpus, &[], &tabs, &statuses);
        let v = PipelineView::build(&model, &status(None), &probes(0.0, 0.0, 300.0));
        assert_eq!(v.front, Phase::Idle(String::new()));
        assert_eq!(v.back, Phase::Idle("avg 71 tok/s".to_string()));
        assert_eq!(v.link, LinkUse::Idle);
        let bare = PipelineView::build(&model, &status(None), &[]);
        assert!(matches!(bare.back, Phase::Unknown(_)));
        assert!(!bare.back.is_active());

        // Probes configured, serving process gone.
        let mut stopped = probes(0.0, 0.0, 300.0);
        for p in &mut stopped[1].json {
            p.up = false;
            p.values.clear();
        }
        let v = PipelineView::build(&model, &status(None), &stopped);
        assert_eq!(v.back, Phase::Idle("not serving".to_string()));

        // Not serving, but the Mac GPU is busy under someone else's lock.
        let (mut gpus, tabs, statuses) = mac_and_box();
        for g in &mut gpus {
            g.utilization = 85.0;
        }
        let model = model_of(&gpus, &[], &tabs, &statuses);
        stopped[1].locks = vec![crate::probes::lock::LockSample {
            path: "/l".to_string(),
            holders: vec![crate::probes::lock::LockHolder {
                pid: 7,
                command: "python".to_string(),
            }],
            since_unix: None,
        }];
        let v = PipelineView::build(&model, &status(Some("step")), &stopped);
        assert_eq!(
            v.back,
            Phase::Busy("not serving · GPU 85% · lock: python (pid 7)".to_string())
        );
        assert_eq!(v.front, Phase::Decode("verify step".to_string()));
    }
}
