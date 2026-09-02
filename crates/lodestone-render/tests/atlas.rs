//! Consolidated test binary for the **atlas** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "atlas/atlas_mip_edge_bleed_gate.rs"]
mod atlas_mip_edge_bleed_gate;
#[path = "atlas/atlas_uploaded_chain_gutter_gate.rs"]
mod atlas_uploaded_chain_gutter_gate;
#[path = "atlas/crack_atlas_gate.rs"]
mod crack_atlas_gate;
#[path = "atlas/gui_atlas_edge_bleed_gate.rs"]
mod gui_atlas_edge_bleed_gate;
#[path = "atlas/gui_atlas_gate.rs"]
mod gui_atlas_gate;
