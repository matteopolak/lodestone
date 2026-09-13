//! Public entity-type id→name resolution for this era's three protocols.
//!
//! 1.13 flattening **unified** the entity registry. 1.12 kept two overlapping
//! numeric id spaces — a `spawn_mob` mob table and a `spawn_object` object
//! table — but every protocol here has `spawn_entity` (objects) and
//! `spawn_entity_living` (mobs) indexing a single registry, so there is one
//! table per protocol rather than two per protocol.
//!
//! * `spawn_entity` carries a `varint` type id (the unified entity id).
//! * `spawn_entity_living` carries a `varint` type id (the same registry).
//! * `spawn_player` carries no type — it is always [`PLAYER`].
//!
//! # Why three tables
//!
//! The registry is dense and alphabetical, so **every insertion renumbers
//! everything after it**: 1.15 inserted the bee at id 4 and 1.16 inserted the
//! hoglin, piglin, strider and zoglin, leaving 102 / 103 / 108 entries with
//! only the first four ids agreeing. Measured against the three registries
//! below, 102 of 108 ids name a different entity at 498 than at 754. A shared
//! table would name a plausible wrong mob for almost every spawn, which is
//! the failure class that produces no error at all, so [`table_for`] resolves
//! the negotiated protocol once, at adapter construction.
//!
//! This version-specific data never lives in a shared crate. The 498 and 578
//! tables come from each jar's own `--reports` registry dump; the 754 table
//! predates those dumps here and still comes from the community
//! `minecraft-data` project, whose 1.16 `name` fields are already lowercase
//! snake_case identifiers.

use crate::{generated_entity_types, generated_entity_types_498, generated_entity_types_578};

/// Canonical identifier for a player entity (`spawn_player`).
pub const PLAYER: &str = "minecraft:player";

/// One protocol's unified entity-type registry.
#[derive(Debug)]
pub struct EntityTypeTable {
    /// `(type id, canonical identifier)` pairs, sorted by id.
    entries: &'static [(i32, &'static str)],
    /// The count the generator rendered alongside `entries`, kept as its own
    /// field so the two can disagree and be caught: a hand edit to one half
    /// of a generated file is exactly the drift the generate-or-assert gate
    /// exists for, and `tests/entity_types.rs` checks this pair on every run
    /// without needing the source data.
    declared_len: usize,
}

impl EntityTypeTable {
    /// Number of entries in this protocol's registry.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entry count the generator declared, for the consistency check in
    /// `tests/entity_types.rs`. Always equal to [`Self::len`].
    #[must_use]
    pub const fn declared_len(&self) -> usize {
        self.declared_len
    }

    /// Whether this protocol's registry is empty (never true for a generated
    /// table; present so `len` does not stand alone).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Resolves a unified entity type id to its canonical identifier.
    ///
    /// Returns `None` for ids absent from this protocol's entity table.
    #[must_use]
    pub fn entity_type_name(&self, id: i32) -> Option<&'static str> {
        self.entries
            .binary_search_by_key(&id, |&(key, _)| key)
            .ok()
            .map(|index| self.entries[index].1)
    }

    /// Resolves a `spawn_entity_living` type id to its canonical identifier.
    ///
    /// Mobs and objects share the unified registry from 1.13 on, so this is
    /// an alias for [`Self::entity_type_name`].
    #[must_use]
    pub fn mob_type_name(&self, id: i32) -> Option<&'static str> {
        self.entity_type_name(id)
    }

    /// Resolves a `spawn_entity` (object) type id to its canonical
    /// identifier. Also an alias for [`Self::entity_type_name`].
    #[must_use]
    pub fn object_type_name(&self, id: i32) -> Option<&'static str> {
        self.entity_type_name(id)
    }
}

/// Minecraft 1.14.4's registry.
static TABLE_498: EntityTypeTable = EntityTypeTable {
    entries: &generated_entity_types_498::ENTITY_TYPES,
    declared_len: generated_entity_types_498::ENTITY_TYPE_COUNT,
};
/// Minecraft 1.15.2's registry.
static TABLE_578: EntityTypeTable = EntityTypeTable {
    entries: &generated_entity_types_578::ENTITY_TYPES,
    declared_len: generated_entity_types_578::ENTITY_TYPE_COUNT,
};
/// Minecraft 1.16.5's registry.
static TABLE_754: EntityTypeTable = EntityTypeTable {
    entries: &generated_entity_types::ENTITY_TYPES,
    declared_len: generated_entity_types::ENTITY_TYPE_COUNT,
};

/// Resolves a negotiated protocol to its entity-type registry.
///
/// # Panics
///
/// Panics for a protocol outside [`crate::PROTOCOLS`], for the same reason
/// [`crate::canonical::table_for`] does: answering with a neighbouring
/// protocol's registry names a real but wrong mob, with nothing red anywhere.
#[must_use]
pub fn table_for(protocol: i32) -> &'static EntityTypeTable {
    match protocol {
        crate::adapter::PROTOCOL_1_14_4 => &TABLE_498,
        crate::adapter::PROTOCOL_1_15_2 => &TABLE_578,
        crate::adapter::PROTOCOL_1_16_5 => &TABLE_754,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({:?}); callers must test \
             membership before resolving an entity-type table",
            crate::PROTOCOLS
        ),
    }
}
