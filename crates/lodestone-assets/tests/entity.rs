//! Consolidated test binary for the **entity** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "entity/entity.rs"]
mod entity;
#[path = "entity/entity_models.rs"]
mod entity_models;
