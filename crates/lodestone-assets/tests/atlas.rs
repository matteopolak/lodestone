//! Consolidated test binary for the **atlas** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "atlas/adversarial_atlas.rs"]
mod adversarial_atlas;
#[path = "atlas/anim_table.rs"]
mod anim_table;
#[path = "atlas/animation_seam.rs"]
mod animation_seam;
#[path = "atlas/atlas.rs"]
mod atlas;
#[path = "atlas/atlas_mips.rs"]
mod atlas_mips;
#[path = "atlas/atlas_source.rs"]
mod atlas_source;
#[path = "atlas/gui.rs"]
mod gui;
#[path = "atlas/icon.rs"]
mod icon;
#[path = "atlas/item_atlas.rs"]
mod item_atlas;
#[path = "atlas/mipmap.rs"]
mod mipmap;
#[path = "atlas/trim_atlas_gate.rs"]
mod trim_atlas_gate;
