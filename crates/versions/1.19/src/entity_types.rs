//! Public entity-type id->name resolution for this era's one protocol.
//!
//! 1.13 flattening **unified** the entity registry, and 1.19.4 goes one step
//! further: the separate mob-spawn packet the eras below carry is gone, so
//! every non-player entity — object and mob alike — arrives through a single
//! spawn packet carrying a `varint` type id into this one table.
//!
//! * the generic spawn packet carries a `varint` type id (the unified id).
//! * the player-spawn packet carries no type — it is always [`PLAYER`].
//!
//! # Why this table is per era and cannot be borrowed
//!
//! The registry is dense and alphabetical, so any insertion renumbers
//! everything after it. 1.19 added `allay`, which sorts **first**, so id 0 is
//! `minecraft:allay` here and `minecraft:area_effect_cloud` in every era
//! below — every subsequent id has moved too. Reading a 1.19.4 spawn through
//! a neighbouring era's table would therefore name a real, wrong entity for
//! essentially every id, with no error anywhere.
//! `tests/entity_types.rs` asserts that shift from the committed dump rather
//! than trusting this paragraph.
//!
//! This version-specific data never lives in a shared crate; it comes from
//! the jar's own `--reports` registry dump.

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

}

/// The era's unified entity registry, rendered from the jar's own
/// `--reports` dump — see the module docs for why it is per era.
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
        crate::adapter::PROTOCOL_1_19_4 => &TABLE,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({:?}); callers must test \
             membership before resolving an entity-type table",
            crate::PROTOCOLS
        ),
    }
}
