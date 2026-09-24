//! Consolidated test binary for the **pipeline** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "pipeline/camera_pitch_singularity.rs"]
mod camera_pitch_singularity;
#[path = "pipeline/coplanar_overlay_depth_survey.rs"]
mod coplanar_overlay_depth_survey;
#[path = "pipeline/depth_convention_guard.rs"]
mod depth_convention_guard;
#[path = "pipeline/gpu.rs"]
mod gpu;
#[path = "pipeline/live_gate.rs"]
mod live_gate;
#[path = "pipeline/scene_bench.rs"]
mod scene_bench;
#[path = "pipeline/scene_gpu.rs"]
mod scene_gpu;
#[path = "pipeline/screen_effects_pipeline_gpu.rs"]
mod screen_effects_pipeline_gpu;
#[path = "pipeline/tint_gamma_gate.rs"]
mod tint_gamma_gate;
#[path = "pipeline/wgsl_valid.rs"]
mod wgsl_valid;
