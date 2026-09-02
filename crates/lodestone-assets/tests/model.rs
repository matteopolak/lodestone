//! Consolidated test binary for the **model** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "model/bake.rs"]
mod bake;
#[path = "model/blockstate.rs"]
mod blockstate;
#[path = "model/fluid.rs"]
mod fluid;
#[path = "model/item.rs"]
mod item;
#[path = "model/item_model.rs"]
mod item_model;
#[path = "model/model.rs"]
mod model;
