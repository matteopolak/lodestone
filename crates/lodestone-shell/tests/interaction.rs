//! Consolidated test binary for the **interaction** group. Each module below was
//! previously its own top-level test binary under `tests/`; merging them
//! here cuts the per-binary link cost without moving or renaming a single
//! test. See each module for what it guards.

#[path = "interaction/break_intent.rs"]
mod break_intent;
#[path = "interaction/break_particle_tint.rs"]
mod break_particle_tint;
#[path = "interaction/break_particles_pixels.rs"]
mod break_particles_pixels;
#[path = "interaction/client_app_installs_command_registry.rs"]
mod client_app_installs_command_registry;
#[path = "interaction/command_tree_completion.rs"]
mod command_tree_completion;
#[path = "interaction/crack_live_gather_pixels.rs"]
mod crack_live_gather_pixels;
#[path = "interaction/crack_multi_target_pixels.rs"]
mod crack_multi_target_pixels;
#[path = "interaction/mining_deadlock.rs"]
mod mining_deadlock;
#[path = "interaction/mining_destroy_burst.rs"]
mod mining_destroy_burst;
#[path = "interaction/place_intent.rs"]
mod place_intent;
#[path = "interaction/plugin_registers_a_recipe.rs"]
mod plugin_registers_a_recipe;
#[path = "interaction/rendered_client_takes_a_plugin.rs"]
mod rendered_client_takes_a_plugin;
#[path = "interaction/select_slot_intent.rs"]
mod select_slot_intent;
