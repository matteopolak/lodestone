//! Consolidated test binary for the **terrain** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "terrain/animated_block_pixels.rs"]
mod animated_block_pixels;
#[path = "terrain/biome_tint_gate.rs"]
mod biome_tint_gate;
#[path = "terrain/biome_tint_row_identity_gate.rs"]
mod biome_tint_row_identity_gate;
#[path = "terrain/block_models_gate.rs"]
mod block_models_gate;
#[path = "terrain/block_texture_gate.rs"]
mod block_texture_gate;
#[path = "terrain/crack_gate.rs"]
mod crack_gate;
#[path = "terrain/cross_plant_light_position_gate.rs"]
mod cross_plant_light_position_gate;
#[path = "terrain/fluid_coplanar_depth_gate.rs"]
mod fluid_coplanar_depth_gate;
#[path = "terrain/fluid_falling_column_gate.rs"]
mod fluid_falling_column_gate;
#[path = "terrain/fluid_gate.rs"]
mod fluid_gate;
#[path = "terrain/fluid_lava_backface_gate.rs"]
mod fluid_lava_backface_gate;
#[path = "terrain/fluid_mesh_identity_gate.rs"]
mod fluid_mesh_identity_gate;
#[path = "terrain/fluid_shoreline_gate.rs"]
mod fluid_shoreline_gate;
#[path = "terrain/fog_gate.rs"]
mod fog_gate;
#[path = "terrain/grass_light_response_gate.rs"]
mod grass_light_response_gate;
#[path = "terrain/ground_plane_coplanarity_census.rs"]
mod ground_plane_coplanarity_census;
#[path = "terrain/half_transparent_interior_cull_gate.rs"]
mod half_transparent_interior_cull_gate;
#[path = "terrain/model_ao_corner_gate.rs"]
mod model_ao_corner_gate;
#[path = "terrain/model_census.rs"]
mod model_census;
#[path = "terrain/model_shade_gamma_gate.rs"]
mod model_shade_gamma_gate;
#[path = "terrain/occlusion_angle_sweep.rs"]
mod occlusion_angle_sweep;
#[path = "terrain/packed_cube_layer_gate.rs"]
mod packed_cube_layer_gate;
#[path = "terrain/packed_night_fog_pixels.rs"]
mod packed_night_fog_pixels;
#[path = "terrain/section_fade_pixels.rs"]
mod section_fade_pixels;
#[path = "terrain/translucent_model_backface_cull_gate.rs"]
mod translucent_model_backface_cull_gate;
#[path = "terrain/water_translucency_gate.rs"]
mod water_translucency_gate;
#[path = "terrain/world_mesher_bench.rs"]
mod world_mesher_bench;
