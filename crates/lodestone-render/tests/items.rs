//! Consolidated test binary for the **items** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "items/banner_pattern_layer_pixels.rs"]
mod banner_pattern_layer_pixels;
#[path = "items/glint_pixels.rs"]
mod glint_pixels;
#[path = "items/held_special_item_placement.rs"]
mod held_special_item_placement;
#[path = "items/item_geometry_gate.rs"]
mod item_geometry_gate;
#[path = "items/item_tint_pixels.rs"]
mod item_tint_pixels;
#[path = "items/item_variant_gate.rs"]
mod item_variant_gate;
#[path = "items/special_item_hand_rig_resolution.rs"]
mod special_item_hand_rig_resolution;
#[path = "items/sprite_drop_pixels.rs"]
mod sprite_drop_pixels;
#[path = "items/thrown_and_held_item_pixels.rs"]
mod thrown_and_held_item_pixels;
