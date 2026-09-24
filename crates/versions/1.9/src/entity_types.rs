//! Public entity-type id→name resolution for protocol 340 (Minecraft 1.12.2).
//!
//! 1.12 still splits entity spawning across three packets and keeps **two**
//! numeric id spaces, though its `spawn_mob` id space is the unified numeric
//! entity registry rather than 1.8's separate mob table:
//!
//! * `spawn_mob` carries a `varint` type id (the unified entity id).
//! * `spawn_object` carries an `i8` **object** type id (the classic object
//!   ids, unchanged from 1.8: boat `1`, arrow `60`, …).
//! * `spawn_player` carries no type — it is always [`PLAYER`].
//!
//! The two id spaces overlap, so they resolve through independent tables. This
//! version-specific data is generated from the community `minecraft-data`
//! project and never lives in a shared crate.
//!
//! Unlike 1.8, `minecraft-data`'s 1.12 `name` fields are already lowercase
//! snake_case identifiers, so the mapping is a faithful id→key table with no
//! casing judgement. (They remain pre-flattening names — e.g.
//! `minecraft:zombie_pigman` exists but block-carrying entities do not — which
//! is correct for this protocol family and must not be unified with modern.)

pub use crate::generated_entity_types::{MOB_TYPE_COUNT, OBJECT_TYPE_COUNT};
use crate::generated_entity_types::{MOB_TYPES, OBJECT_TYPES};

/// Canonical identifier for a player entity (`spawn_player`).
pub const PLAYER: &str = "minecraft:player";

/// Resolves a `spawn_mob` type id to its canonical identifier.
///
/// Returns `None` for ids absent from the 1.12 entity table.
#[must_use]
pub fn mob_type_name(id: i32) -> Option<&'static str> {
    lookup(&MOB_TYPES, id)
}

/// Resolves a `spawn_object` type id to its canonical identifier.
///
/// Returns `None` for ids absent from the 1.12 object table.
#[must_use]
pub fn object_type_name(id: i32) -> Option<&'static str> {
    lookup(&OBJECT_TYPES, id)
}

fn lookup(table: &[(i32, &'static str)], id: i32) -> Option<&'static str> {
    table
        .binary_search_by_key(&id, |&(key, _)| key)
        .ok()
        .map(|index| table[index].1)
}
