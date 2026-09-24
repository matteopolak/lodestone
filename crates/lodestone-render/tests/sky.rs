//! Consolidated test binary for the **sky** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "sky/cloud_face_cache_counts.rs"]
mod cloud_face_cache_counts;
#[path = "sky/sky_gradient_pixels.rs"]
mod sky_gradient_pixels;
#[path = "sky/sky_light_factor_timeline.rs"]
mod sky_light_factor_timeline;
#[path = "sky/sky_pipeline_gpu.rs"]
mod sky_pipeline_gpu;
#[path = "sky/sky_star_field_build_counts.rs"]
mod sky_star_field_build_counts;
#[path = "sky/sunrise_sunset_timeline.rs"]
mod sunrise_sunset_timeline;
#[path = "sky/weather_probe_query_counts.rs"]
mod weather_probe_query_counts;
