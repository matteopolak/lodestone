//! Consolidated test binary for the **session** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "session/client_chunk_cycles.rs"]
mod client_chunk_cycles;
#[path = "session/no_production_source_names_testsupport.rs"]
mod no_production_source_names_testsupport;
#[path = "session/no_test_touches_the_real_saves_dir.rs"]
mod no_test_touches_the_real_saves_dir;
#[path = "session/offline_identity_is_stable.rs"]
mod offline_identity_is_stable;
#[path = "session/ownership_gate.rs"]
mod ownership_gate;
#[path = "session/paperdoll_skin_resolver.rs"]
mod paperdoll_skin_resolver;
#[path = "session/resource_pack_stack.rs"]
mod resource_pack_stack;
#[path = "session/singleplayer_joins_as_the_selected_account.rs"]
mod singleplayer_joins_as_the_selected_account;
#[path = "session/singleplayer_persistence.rs"]
mod singleplayer_persistence;
#[path = "session/singleplayer_saved_world_terrain_arrives.rs"]
mod singleplayer_saved_world_terrain_arrives;
#[path = "session/singleplayer_terrain_arrives.rs"]
mod singleplayer_terrain_arrives;
#[path = "session/stranded_entity_producers_wire.rs"]
mod stranded_entity_producers_wire;
