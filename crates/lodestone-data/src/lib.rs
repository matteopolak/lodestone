//! Canonical 26.2 game-data censuses.
//!
//! Extracted from `crates/protocol/v770`: of the ~20 tables that
//! used to live under that crate's `generated/`, exactly one —
//! `packet_ids.rs` — is wire format and stayed behind. Every table here
//! answers a **game** question (block collision, entity hitboxes, item
//! prototypes, mining speed, ...), not a **protocol** question, and is
//! reachable without depending on any wire-format implementation.
//!
//! Each table is dumped from a real, headless 26.2 server — either by walking
//! a registry/report (`registries.json`, `blocks.json`) or by booting the jar
//! and asking it directly (`oracle-java/`, see each module's doc header for
//! its specific oracle and the `LODESTONE_REGEN=1` regeneration command). See
//! `docs/lodestone-data-crate.md`.
//!
//! # Why this crate depends on nothing but `lodestone-model`
//!
//! Every lookup here resolves to a version-free type
//! (`lodestone_model::BlockAabb`, `PathType`, `ItemPrototype`,
//! `EntityBaseDimensions`, ...), so a consumer — including
//! `lodestone-server`, which has zero protocol dependency by design — can
//! read game facts without naming a wire format at all.
//!
//! # This crate is not itself version-generic
//!
//! 26.2 is the one canonical internal version; these tables are
//! this version's canonical data, not a version-parameterised abstraction.
//! Older protocol crates (`v47`, `v340`, `v735`) keep their own
//! version-specific translation tables (e.g. `v340`'s pre-Flattening
//! `id:meta` table) because that data is genuinely about translating an old
//! wire format into this canonical space — it is not a second copy of the
//! canonical census, and does not belong here. See
//! `docs/protocol-340-flattening-table.md`.

#![forbid(unsafe_code)]

#[path = "generated/attribute_types.rs"]
pub(crate) mod generated_attribute_types;
#[path = "generated/block_blast.rs"]
pub(crate) mod generated_block_blast;
#[path = "generated/block_entity_types.rs"]
pub(crate) mod generated_block_entity_types;
#[path = "generated/block_enum.rs"]
pub(crate) mod generated_block_enum;
#[path = "generated/block_items.rs"]
pub(crate) mod generated_block_items;
#[path = "generated/block_registry.rs"]
pub(crate) mod generated_block_registry;
#[path = "generated/block_solidity.rs"]
pub(crate) mod generated_block_solidity;
#[path = "generated/block_states.rs"]
pub(crate) mod generated_block_states;
#[path = "generated/collision_shapes.rs"]
pub(crate) mod generated_collision_shapes;
#[path = "generated/damage_types.rs"]
pub(crate) mod generated_damage_types;
#[path = "generated/data_component_types.rs"]
pub(crate) mod generated_data_component_types;
#[path = "generated/enchantments.rs"]
pub(crate) mod generated_enchantments;
#[path = "generated/entity_census.rs"]
pub(crate) mod generated_entity_census;
#[path = "generated/entity_dimensions.rs"]
pub(crate) mod generated_entity_dimensions;
#[path = "generated/entity_type_enum.rs"]
pub(crate) mod generated_entity_type_enum;
#[path = "generated/entity_types.rs"]
pub(crate) mod generated_entity_types;
#[path = "generated/hardness.rs"]
pub(crate) mod generated_hardness;
#[path = "generated/item_enum.rs"]
pub(crate) mod generated_item_enum;
#[path = "generated/item_prototypes.rs"]
pub(crate) mod generated_item_prototypes;
#[path = "generated/items.rs"]
pub(crate) mod generated_items;
#[path = "generated/light_props.rs"]
pub(crate) mod generated_light_props;
#[path = "generated/menus.rs"]
pub(crate) mod generated_menus;
#[path = "generated/mob_effect_colors.rs"]
pub(crate) mod generated_mob_effect_colors;
#[path = "generated/mob_effects.rs"]
pub(crate) mod generated_mob_effects;
#[path = "generated/outline_shapes.rs"]
pub(crate) mod generated_outline_shapes;
#[path = "generated/particle_types.rs"]
pub(crate) mod generated_particle_types;
#[path = "generated/path_types.rs"]
pub(crate) mod generated_path_types;
#[path = "generated/potion_effect_keys.rs"]
pub(crate) mod generated_potion_effect_keys;
#[path = "generated/potion_effects.rs"]
pub(crate) mod generated_potion_effects;
#[path = "generated/potions.rs"]
pub(crate) mod generated_potions;
#[path = "generated/shade_brightness.rs"]
pub(crate) mod generated_shade_brightness;
#[path = "generated/snow_support.rs"]
pub(crate) mod generated_snow_support;
#[path = "generated/sound_events.rs"]
pub(crate) mod generated_sound_events;
#[path = "generated/sound_types.rs"]
pub(crate) mod generated_sound_types;
#[path = "generated/tools.rs"]
pub(crate) mod generated_tools;

pub mod attribute_types;
pub mod biomes;
pub mod block;
pub mod block_blast;
pub mod block_entity_types;
pub mod block_items;
pub mod block_solidity;
pub mod block_states;
pub mod collision_shapes;
pub mod damage_types;
pub mod data_component_types;
pub mod enchantment;
pub mod entity_census;
pub mod entity_disguise;
pub mod entity_dimensions;
pub mod entity_type;
pub mod entity_types;
pub mod hardness;
pub mod item;
pub mod item_prototypes;
pub mod items;
pub mod light_props;
pub mod menus;
pub mod mob_effects;
pub mod outline_shapes;
pub mod particle_types;
pub mod path_types;
pub mod potion;
pub mod shade_brightness;
pub mod snow_support;
pub mod sound_events;
pub mod sound_types;
pub mod tool;
pub mod villager_trades;
