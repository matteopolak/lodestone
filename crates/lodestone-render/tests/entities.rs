//! Consolidated test binary for the **entities** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "entities/arrow_pixels.rs"]
mod arrow_pixels;
#[path = "entities/block_entity_rotation_noise_pixels.rs"]
mod block_entity_rotation_noise_pixels;
#[path = "entities/boat_model_resolution.rs"]
mod boat_model_resolution;
#[path = "entities/bow_draw_pose_pixels.rs"]
mod bow_draw_pose_pixels;
#[path = "entities/creeper_swell_pixels.rs"]
mod creeper_swell_pixels;
#[path = "entities/creeper_white_overlay_pixels.rs"]
mod creeper_white_overlay_pixels;
#[path = "entities/elytra_wings.rs"]
mod elytra_wings;
#[path = "entities/entity_anim_pixels.rs"]
mod entity_anim_pixels;
#[path = "entities/entity_depth_coincident_pixels.rs"]
mod entity_depth_coincident_pixels;
#[path = "entities/entity_diffuse_two_lights_pixels.rs"]
mod entity_diffuse_two_lights_pixels;
#[path = "entities/entity_fog_pixels.rs"]
mod entity_fog_pixels;
#[path = "entities/entity_gate.rs"]
mod entity_gate;
#[path = "entities/entity_hurt_overlay_pixels.rs"]
mod entity_hurt_overlay_pixels;
#[path = "entities/entity_light_pixels.rs"]
mod entity_light_pixels;
#[path = "entities/entity_night_pixels.rs"]
mod entity_night_pixels;
#[path = "entities/entity_variant_pixels.rs"]
mod entity_variant_pixels;
#[path = "entities/first_person_arm_swing_pixels.rs"]
mod first_person_arm_swing_pixels;
#[path = "entities/invisible_but_solid_rigs.rs"]
mod invisible_but_solid_rigs;
#[path = "entities/lightning_bolt_walk.rs"]
mod lightning_bolt_walk;
#[path = "entities/non_living_entity_placement.rs"]
mod non_living_entity_placement;
#[path = "entities/sheep_wool_pixels.rs"]
mod sheep_wool_pixels;
#[path = "entities/skull_hat_overlay.rs"]
mod skull_hat_overlay;
