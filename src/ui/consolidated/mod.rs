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

//! Consolidated tab (`all-smi view --consolidated`).
//!
//! Shows every accelerator across the scraped hosts as one system: an
//! Apple Silicon GPU (unified memory) next to discrete NVIDIA GPUs (VRAM),
//! with combined memory and power, per-series sparklines, the opt-in host
//! probes (link throughput, lock holders), and an optional pipeline panel
//! for a model served across the machines.
//!
//! Submodules:
//!
//! * [`history`] — the per-series ring buffers behind the sparklines,
//!   recorded by the remote collector each cycle.
//! * [`model`] — a pure view model built from the render snapshot.
//! * [`pipeline`] — the optional pipeline panel's endpoint data
//!   (`--icculus`: engine `/health`, llama-swap `/running`).
//! * [`render`] — writes the model into the frame buffer.

pub mod history;
pub mod model;
pub mod pipeline;
mod pipeline_render;
pub mod render;

pub use history::ConsolidatedState;
pub use render::{ConsolidatedInputs, render_consolidated_tab};
