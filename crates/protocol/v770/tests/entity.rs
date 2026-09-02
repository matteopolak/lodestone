//! Consolidated test binary for the **entity** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "entity/entity_encoders.rs"]
mod entity_encoders;
#[path = "entity/entity_events.rs"]
mod entity_events;
#[path = "entity/entity_facts_seam.rs"]
mod entity_facts_seam;
#[path = "entity/entity_spawn.rs"]
mod entity_spawn;
#[path = "entity/item_components.rs"]
mod item_components;
#[path = "entity/item_entity_metadata.rs"]
mod item_entity_metadata;
#[path = "entity/orb_metadata.rs"]
mod orb_metadata;
#[path = "entity/sheep_wool_default.rs"]
mod sheep_wool_default;
#[path = "entity/set_equipment.rs"]
mod set_equipment;
#[path = "entity/container_encoders.rs"]
mod container_encoders;
#[path = "entity/container_inventory.rs"]
mod container_inventory;
#[path = "entity/crafter_wiring.rs"]
mod crafter_wiring;
#[path = "entity/explode_particle_ids.rs"]
mod explode_particle_ids;
#[path = "entity/move_minecart_along_track.rs"]
mod move_minecart_along_track;
#[path = "entity/movement_selection.rs"]
mod movement_selection;
