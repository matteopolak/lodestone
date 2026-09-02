//! Consolidated test binary for the **entities** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "entities/aggressive_bow_pose_pixels.rs"]
mod aggressive_bow_pose_pixels;
#[path = "entities/armor_stand_hologram_pixels.rs"]
mod armor_stand_hologram_pixels;
#[path = "entities/armor_stand_pose_wire.rs"]
mod armor_stand_pose_wire;
#[path = "entities/armour_pixels.rs"]
mod armour_pixels;
#[path = "entities/boat_water_mask_pixels.rs"]
mod boat_water_mask_pixels;
#[path = "entities/copper_golem_statue_pixels.rs"]
mod copper_golem_statue_pixels;
#[path = "entities/display_entity_pixels.rs"]
mod display_entity_pixels;
#[path = "entities/elytra_wings_pixels.rs"]
mod elytra_wings_pixels;
#[path = "entities/entity_shadow_pixels.rs"]
mod entity_shadow_pixels;
#[path = "entities/entity_shadow_z_fight_pixels.rs"]
mod entity_shadow_z_fight_pixels;
#[path = "entities/entity_sprite_pixels.rs"]
mod entity_sprite_pixels;
#[path = "entities/first_person_banner_hand_pixels.rs"]
mod first_person_banner_hand_pixels;
#[path = "entities/first_person_hand_light_pixels.rs"]
mod first_person_hand_light_pixels;
#[path = "entities/first_person_head_hand_pixels.rs"]
mod first_person_head_hand_pixels;
#[path = "entities/first_person_shield_hand_pixels.rs"]
mod first_person_shield_hand_pixels;
#[path = "entities/left_handed_bow_pose_pixels.rs"]
mod left_handed_bow_pose_pixels;
#[path = "entities/lightning_bolt_pixels.rs"]
mod lightning_bolt_pixels;
#[path = "entities/mob_fire_pixels.rs"]
mod mob_fire_pixels;
#[path = "entities/nametag_pixels.rs"]
mod nametag_pixels;
#[path = "entities/painting_pixels.rs"]
mod painting_pixels;
#[path = "entities/remote_entity_swing_pixels.rs"]
mod remote_entity_swing_pixels;
#[path = "entities/sheep_wool_pixels.rs"]
mod sheep_wool_pixels;
#[path = "entities/spawner_mob_pixels.rs"]
mod spawner_mob_pixels;
#[path = "entities/trial_spawner_mob_pixels.rs"]
mod trial_spawner_mob_pixels;
