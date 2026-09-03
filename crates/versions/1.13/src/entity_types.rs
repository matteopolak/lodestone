//! Public entity-type id→name resolution for protocol 404.
//!
//! 1.13 flattening **unified** the entity registry, and this is the release
//! that did it. 1.12 kept two overlapping numeric id spaces — a `spawn_mob`
//! mob table and a `spawn_object` object table — where 1.13.2 has
//! `spawn_entity` (objects) and `spawn_entity_living` (mobs) indexing a
//! single registry, so there is one table here rather than two.
//!
//! * `spawn_entity` carries a **signed byte** type id at 404 (it widened to a
//!   VarInt in 1.14), indexing the unified registry.
//! * `spawn_entity_living` carries a `varint` type id from the same registry.
//! * `spawn_player` carries no type — it is always [`PLAYER`].
//!
//! # Provenance, and why it is not a jar report
//!
//! The 1.13.2 jar's own data generator emits block, item and command reports
//! and **no registry dump** — that provider arrived in 1.14 — so unlike the
//! 1.14 era's 498/578 tables this one starts from `minecraft-data`'s
//! `entities.json`, which is cross-check-grade, not an authority. It is
//! turned into one by the wire: `tests/entity_types.rs` checks every id in
//! the committed table against the type id a real 1.13.2 server put in a
//! `spawn_entity_living`/`spawn_entity` packet after being asked to summon
//! that entity by name over RCON. The summon *name* comes from vanilla, the
//! id comes from vanilla, and this crate is not on either side of the
//! comparison.
//!
//! # Why the table is per protocol even with one member
//!
//! The registry is dense and alphabetical, so **every insertion renumbers
//! everything after it**; a neighbouring release's table would name a
//! plausible wrong mob for almost every spawn, which is the failure class
//! that produces no error at all. [`table_for`] resolves the negotiated
//! protocol once, at adapter construction, and panics rather than guessing.

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

/// Minecraft 1.13.2's registry.
static TABLE_404: EntityTypeTable = EntityTypeTable {
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
        crate::adapter::PROTOCOL_1_13_2 => &TABLE_404,
        other => panic!(
            "protocol {other} is outside this family's PROTOCOLS ({:?}); callers must test \
             membership before resolving an entity-type table",
            crate::PROTOCOLS
        ),
    }
}
