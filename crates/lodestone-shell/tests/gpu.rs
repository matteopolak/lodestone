//! Consolidated test binary for the **gpu** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "gpu/capture_screenshots.rs"]
mod capture_screenshots;
#[path = "gpu/frame_benchmark_showcase_fixture.rs"]
mod frame_benchmark_showcase_fixture;
#[path = "gpu/hud_scene_fixture.rs"]
mod hud_scene_fixture;
#[path = "gpu/mesh_fill_rate.rs"]
mod mesh_fill_rate;
#[path = "gpu/wgsl_valid.rs"]
mod wgsl_valid;
