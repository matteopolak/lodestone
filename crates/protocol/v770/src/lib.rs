//! Protocol 770-family (Minecraft 1.21.5 – 26.2) client protocol crate.
//!
//! Phase 1 implements protocol 776 (Minecraft 26.2): the packet definitions for
//! the handshake, login, configuration, and play join flow, plus the
//! [`V770Adapter`] that lifts those packets into the version-free
//! [`lodestone_model`] canonical model, and [`server_protocol::V770ServerProtocol`]
//! that implements the mirror-image [`lodestone_server::ServerProtocol`] seam so
//! `lodestone-server` can drive the same wire format from the other side.
//!
//! This crate depends on the version-free `lodestone-core`, `lodestone-model`,
//! `lodestone-macros`, and `lodestone-world` crates, plus `lodestone-server` (also
//! version-free) for the `V770ServerProtocol` seam — never on another version
//! crate — so the whole version family can be removed by deleting this folder.
//! `cargo xtask check-isolation` enforces this in one direction only: a version
//! crate may depend on version-free crates, but no version-free crate may depend
//! back on this one.

#![forbid(unsafe_code)]

/// Generated authoritative packet id tables for protocol 776.
#[path = "generated/packet_ids.rs"]
pub mod packet_ids;

/// Generated block-state id table (raw rodata statics). Use the [`block_states`]
/// module for the resolution API; this holds only the generated arrays.
#[path = "generated/block_states.rs"]
pub(crate) mod generated_block_states;

/// Generated block collision-shape table (raw rodata statics). Use the
/// [`collision_shapes`] module for the lookup API; this holds only the arrays.
#[path = "generated/collision_shapes.rs"]
pub(crate) mod generated_collision_shapes;

/// Generated node-evaluator path-type table (raw rodata statics). Use the
/// [`path_types`] module for the lookup API; this holds only the arrays.
#[path = "generated/path_types.rs"]
pub(crate) mod generated_path_types;

/// Generated entity-type id→name table (raw rodata statics). Use the
/// [`entity_types`] module for the lookup API; this holds only the array.
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;

#[path = "generated/entity_dimensions.rs"]
pub(crate) mod generated_entity_dimensions;

/// Generated per-entity-type push census (raw rodata statics) — whether a type
/// can shove the local player through `LivingEntity.pushEntities()`. Use the
/// [`entity_census`] module for the lookup API; this holds only the array.
#[path = "generated/entity_census.rs"]
pub(crate) mod generated_entity_census;

/// Generated attribute id→name table (raw rodata statics). Use the
/// [`attribute_types`] module for the lookup API; this holds only the array.
#[path = "generated/attribute_types.rs"]
pub(crate) mod generated_attribute_types;

/// Generated per-block-state hardness/correct-tool table (raw rodata
/// statics). Use the [`hardness`] module for the lookup API; this holds only
/// the arrays.
#[path = "generated/hardness.rs"]
pub(crate) mod generated_hardness;

/// Generated per-block-state `legacySolid`/`blocksMotion` bitsets (raw rodata
/// statics) — the flag `calculateSolid` caches, which is **not** derivable from
/// the collision census because 237 blocks force it on and 8 force it off. Use
/// the [`block_solidity`] module for the lookup API; this holds only the arrays.
#[path = "generated/block_solidity.rs"]
pub(crate) mod generated_block_solidity;

/// Generated `minecraft:block` registry-order tables (raw rodata statics):
/// registry id → name, and block-state id → registry id. Two id spaces the wire
/// uses interchangeably that are *not* the same order — see the module docs.
/// Consumed by [`block_states`] and [`tool`].
#[path = "generated/block_registry.rs"]
pub(crate) mod generated_block_registry;

/// Generated `minecraft:tool` census (raw rodata statics): block tag membership
/// and every item's built-in tool component. Use the [`tool`] module for the
/// evaluation API; this holds only the arrays.
#[path = "generated/tools.rs"]
pub(crate) mod generated_tools;

/// Generated block **outline**/**interaction** shape tables (raw rodata
/// statics) — `BlockStateBase.getShape` and `getInteractionShape`, which are
/// neither the collision shape nor fluid presence. Use the [`outline_shapes`]
/// module for the lookup API; this holds only the arrays.
#[path = "generated/outline_shapes.rs"]
pub(crate) mod generated_outline_shapes;

/// Generated item **prototype** component census (raw rodata statics):
/// per-item `minecraft:max_stack_size`, `minecraft:max_damage` and
/// `minecraft:equippable`, none of which a clientbound stack carries. Use the
/// [`item_prototypes`] module for the lookup API; this holds only the array.
#[path = "generated/item_prototypes.rs"]
pub(crate) mod generated_item_prototypes;

/// Generated sound-event id→(name, fixed range) table (raw rodata statics). Use
/// the [`sound_events`] module for the lookup API; this holds only the array.
#[path = "generated/sound_events.rs"]
pub(crate) mod generated_sound_events;

/// Generated particle-type id→name table (raw rodata statics). Use the
/// [`particle_types`] module for the lookup API; this holds only the array.
#[path = "generated/particle_types.rs"]
pub(crate) mod generated_particle_types;

/// Generated menu id→name table (raw rodata statics). Use the [`menus`] module
/// for the lookup API; this holds only the array.
#[path = "generated/menus.rs"]
pub(crate) mod generated_menus;

#[path = "generated/items.rs"]
pub(crate) mod generated_items;

/// Generated data-component-type id→name table (raw rodata statics). Use the
/// [`data_component_types`] module for the lookup API; this holds only the
/// array. Item stacks reference each patched component by its registry id.
#[path = "generated/data_component_types.rs"]
pub(crate) mod generated_data_component_types;

/// Generated mob-effect id→name table (raw rodata statics). Use the
/// [`mob_effects`] module for the lookup API; this holds only the array. See
/// that file's header for why this table exists outside `xtask gen-registries`.
#[path = "generated/mob_effects.rs"]
pub(crate) mod generated_mob_effects;

pub mod adapter;
pub mod attribute_types;
pub mod block_solidity;
pub mod block_states;
pub mod chunk_batch;
pub mod collision_shapes;
pub mod data_component_types;
pub mod entity_census;
pub mod entity_dimensions;
pub mod entity_types;
pub mod entity_variants;
pub mod hardness;
pub mod item_prototypes;
pub mod items;
pub mod menus;
pub mod mob_effects;
pub mod outline_shapes;
pub mod packets;
pub mod particle_types;
pub mod path_types;
pub mod server_protocol;
pub mod sound_events;
pub mod tool;

pub use adapter::{PROTOCOL, V770Adapter, adapter};
pub use server_protocol::V770ServerProtocol;
