//! Consolidated test binary for the **world** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "world/beacon_beam_pixels.rs"]
mod beacon_beam_pixels;
#[path = "world/biome_tint_live_mesh.rs"]
mod biome_tint_live_mesh;
#[path = "world/canopy_ao.rs"]
mod canopy_ao;
#[path = "world/client_relight_reaches_the_mesher.rs"]
mod client_relight_reaches_the_mesher;
#[path = "world/cutout_minification_flicker_pixels.rs"]
mod cutout_minification_flicker_pixels;
#[path = "world/distant_flat_terrain_holes.rs"]
mod distant_flat_terrain_holes;
#[path = "world/end_gateway_beam_pixels.rs"]
mod end_gateway_beam_pixels;
#[path = "world/end_portal_pixels.rs"]
mod end_portal_pixels;
#[path = "world/far_grazing_ceiling_floor_holes.rs"]
mod far_grazing_ceiling_floor_holes;
#[path = "world/fluid_self_occlusion.rs"]
mod fluid_self_occlusion;
#[path = "world/ground_plate_z_fight_pixels.rs"]
mod ground_plate_z_fight_pixels;
#[path = "world/near_grazing_face_coverage_pixels.rs"]
mod near_grazing_face_coverage_pixels;
#[path = "world/partial_connectivity_hall_holes.rs"]
mod partial_connectivity_hall_holes;
#[path = "world/per_quad_render_layer.rs"]
mod per_quad_render_layer;
#[path = "world/sky_pixels.rs"]
mod sky_pixels;
#[path = "world/translucent_alpha_cutout_pixels.rs"]
mod translucent_alpha_cutout_pixels;
#[path = "world/uneven_terrain_holes.rs"]
mod uneven_terrain_holes;
#[path = "world/water_seam_convergence.rs"]
mod water_seam_convergence;
