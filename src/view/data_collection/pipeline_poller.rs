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

//! Background poller for the Consolidated tab's pipeline panel
//! (`view --icculis`).
//!
//! Issues plain read-only `GET`s: the engine's `/health` every cycle and
//! llama-swap's `/running` at most every [`SWAP_POLL_EVERY`], because
//! llama-swap logs every request and a model's load state changes rarely.
//! Results land in `AppState::consolidated`; a failed poll is shown as
//! unreachable rather than hiding the last good reading.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};

use crate::app_state::AppState;
use crate::ui::consolidated::pipeline::{EngineHealth, PipelineConfig, parse_swap_running};

/// Per-request timeout; both endpoints answer in well under 100 ms.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest body accepted from either endpoint.
const MAX_BODY_BYTES: usize = 1 << 20;
/// Minimum spacing between llama-swap `/running` polls.
pub const SWAP_POLL_EVERY: Duration = Duration::from_secs(10);

pub async fn run_pipeline_poller(
    app_state: Arc<Mutex<AppState>>,
    notify: Arc<Notify>,
    config: PipelineConfig,
    interval: Duration,
) {
    let client = match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(1)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let mut state = app_state.lock().await;
            if let Some(c) = state.consolidated.as_mut() {
                c.record_engine(Err(format!("HTTP client: {e}")));
            }
            return;
        }
    };
    let swap_url = config.swap_running_url();
    let mut last_swap_poll: Option<Instant> = None;

    loop {
        let engine = fetch(&client, &config.health_url)
            .await
            .and_then(|body| EngineHealth::parse(&body));
        let swap = if last_swap_poll.is_none_or(|t| t.elapsed() >= SWAP_POLL_EVERY) {
            last_swap_poll = Some(Instant::now());
            Some(
                fetch(&client, &swap_url)
                    .await
                    .and_then(|body| parse_swap_running(&body)),
            )
        } else {
            None
        };

        {
            let mut state = app_state.lock().await;
            if let Some(consolidated) = state.consolidated.as_mut() {
                consolidated.record_engine(engine);
                if let Some(swap) = swap {
                    consolidated.record_swap(swap);
                }
            }
            state.mark_data_changed();
        }
        notify.notify_one();
        tokio::time::sleep(interval).await;
    }
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client.get(url).send().await.map_err(|e| describe(&e))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body = response.bytes().await.map_err(|e| describe(&e))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(format!("response too large ({} bytes)", body.len()));
    }
    Ok(body.to_vec())
}

/// reqwest's own messages repeat the URL; the panel already shows it.
fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "connection failed".to_string()
    } else {
        e.to_string().chars().take(80).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consolidated::ConsolidatedState;
    use crate::ui::consolidated::pipeline::{PipelineStatus, Probe};

    #[tokio::test]
    async fn unreachable_endpoints_are_reported_not_hidden() {
        // Port 9 (discard) on loopback is closed in any sane test sandbox.
        let config = PipelineConfig::icculis(
            Some("http://127.0.0.1:9/health".to_string()),
            Some("http://127.0.0.1:9".to_string()),
        );
        let mut state = AppState::new();
        state.consolidated = Some(ConsolidatedState {
            pipeline: Some(PipelineStatus::new(config.clone())),
            ..Default::default()
        });
        let state = Arc::new(Mutex::new(state));
        let notify = Arc::new(Notify::new());
        let handle = tokio::spawn(run_pipeline_poller(
            Arc::clone(&state),
            Arc::clone(&notify),
            config,
            Duration::from_secs(60),
        ));
        tokio::time::timeout(Duration::from_secs(5), notify.notified())
            .await
            .expect("poller reports within the request timeout");
        handle.abort();
        let guard = state.lock().await;
        let pipeline = guard
            .consolidated
            .as_ref()
            .unwrap()
            .pipeline
            .as_ref()
            .unwrap();
        assert!(matches!(pipeline.engine, Probe::Err(_)), "{pipeline:?}");
        assert!(matches!(pipeline.swap, Probe::Err(_)), "{pipeline:?}");
    }
}
