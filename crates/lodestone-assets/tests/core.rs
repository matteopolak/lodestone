//! Consolidated test binary for the **core** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "core/location.rs"]
mod location;
#[path = "core/manager.rs"]
mod manager;
#[path = "core/meta.rs"]
mod meta;
#[path = "core/profile.rs"]
mod profile;
#[path = "core/source.rs"]
mod source;
#[path = "core/texture.rs"]
mod texture;
