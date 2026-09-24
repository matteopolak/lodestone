//! Public entity-type id->name resolution for this era's two protocols.
//!
//! 1.13 flattening **unified** the entity registry, so both protocols here
//! have `spawn_entity` (objects) and `spawn_entity_living` (mobs) indexing a
//! single table rather than the two overlapping id spaces the pre-1.13 eras
//! carry.
//!
//! * `spawn_entity` carries a `varint` type id (the unified entity id).
//! * `spawn_entity_living` carries a `varint` type id (the same registry).
//! * `spawn_player` carries no type — it is always [`PLAYER`].
//!
//! # Why *one* table here, when the 1.14 era needs three
//!
//! The registry is dense and alphabetical, so any insertion renumbers
//! everything after it — that is what forces one table per protocol in the
//! era below, where 102 of 108 ids name a different entity at 498 than at
//! 754. Measured here, 1.18 inserted nothing: both jars' `--reports` registry
//! dumps carry the same 113 entity types with the same ids, checked entry by
//! entry in `tests/entity_types.rs` against **both** committed dumps rather
//! than asserted here. [`table_for`] still routes through the negotiated
//! protocol, so a third member that did renumber could not silently inherit
//! this table.
//!
//! This version-specific data never lives in a shared crate; it comes from
//! the jars' own `--reports` registry dumps.

use crate::generated_entity_types;

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

/// The era's single unified entity registry, shared by 756 and 758 because
/// the two jars' own dumps agree id for id — see the module docs.
static TABLE: EntityTypeTable = EntityTypeTable {
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
        crate::adapter::PROTOCOL_1_17_1 | crate::adapter::PROTOCOL_1_18_2 => &TABLE,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({:?}); callers must test \
             membership before resolving an entity-type table",
            crate::PROTOCOLS
        ),
    }
}
