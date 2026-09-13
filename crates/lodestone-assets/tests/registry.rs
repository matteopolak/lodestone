//! Consolidated test binary for the **registry** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "registry/particle.rs"]
mod particle;
#[path = "registry/sound.rs"]
mod sound;
#[path = "registry/tint.rs"]
mod tint;
