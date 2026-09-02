//! Public entity-type id→name resolution for protocol 754 (Minecraft 1.16.5).
//!
//! 1.13 flattening **unified** the entity registry. 1.12 kept two overlapping
//! numeric id spaces — a `spawn_mob` mob table and a `spawn_object` object
//! table — but 1.16's `spawn_entity` (objects) and `spawn_entity_living`
//! (mobs) both index a single registry, so there is one table here.
//!
//! * `spawn_entity` carries a `varint` type id (the unified entity id).
//! * `spawn_entity_living` carries a `varint` type id (the same registry).
//! * `spawn_player` carries no type — it is always [`PLAYER`].
//!
//! This version-specific data is generated from the community `minecraft-data`
//! project and never lives in a shared crate. `minecraft-data`'s 1.16 `name`
//! fields are already lowercase snake_case identifiers, so the mapping is a
//! faithful id→key table with no casing judgement.

pub use crate::generated_entity_types::ENTITY_TYPE_COUNT;
use crate::generated_entity_types::ENTITY_TYPES;

/// Canonical identifier for a player entity (`spawn_player`).
pub const PLAYER: &str = "minecraft:player";

/// Resolves a unified entity type id to its canonical identifier.
///
/// Returns `None` for ids absent from the 1.16 entity table.
#[must_use]
pub fn entity_type_name(id: i32) -> Option<&'static str> {
    lookup(&ENTITY_TYPES, id)
}

/// Resolves a `spawn_entity_living` type id to its canonical identifier.
///
/// In 1.16 mobs and objects share the unified registry, so this is an alias
/// for [`entity_type_name`]. Returns `None` for ids absent from the table.
#[must_use]
pub fn mob_type_name(id: i32) -> Option<&'static str> {
    entity_type_name(id)
}

/// Resolves a `spawn_entity` (object) type id to its canonical identifier.
///
/// In 1.16 mobs and objects share the unified registry, so this is an alias
/// for [`entity_type_name`]. Returns `None` for ids absent from the table.
#[must_use]
pub fn object_type_name(id: i32) -> Option<&'static str> {
    entity_type_name(id)
}

fn lookup(table: &[(i32, &'static str)], id: i32) -> Option<&'static str> {
    table
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| table[index].1)
}
