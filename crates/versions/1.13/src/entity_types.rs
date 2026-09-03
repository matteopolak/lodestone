//! Public entity-type id→name resolution for protocol 404.
//!
//! 1.13 unified the entity **registry** — one alphabetical table where 1.12
//! kept a mob table and an object table — but it did **not** unify the two
//! *wire* id spaces, and that is the single most surprising thing about this
//! protocol. Measured against a real 1.13.2 server (see
//! `tests/entity_types.rs` and its committed transcript):
//!
//! * `spawn_entity_living` carries a `varint` type id into the **unified
//!   registry** (95 dense entries, alphabetical).
//! * `spawn_entity` carries a **signed byte** type id into the **pre-1.13
//!   object id space**, unchanged from 1.12. A real server spawns
//!   `armor_stand` there with type id 78, where the unified registry has
//!   `vex`; it spawns `boat` with id 1, where the unified registry has
//!   `armor_stand`. Resolving an object spawn through the unified table names
//!   a real, wrong entity, with nothing red anywhere. 1.14 is where that
//!   field widened to a VarInt and started indexing the unified registry.
//! * `spawn_player` carries no type — it is always [`PLAYER`].
//!
//! So this module holds **two** tables, and [`EntityTypeTable`]'s two
//! accessors are genuinely different lookups rather than aliases.
//!
//! # Provenance, and why it is not a jar report
//!
//! The 1.13.2 jar's own data generator emits block, item and command reports
//! and **no registry dump** — that provider arrived in 1.14 — so unlike the
//! 1.14 era's 498/578 tables the unified registry here starts from
//! `minecraft-data`'s `entities.json`, which is cross-check-grade, not an
//! authority. It is turned into one by the wire: `tests/entity_types.rs`
//! checks every id against the type id a real 1.13.2 server put in a
//! `spawn_entity_living` packet after being asked to summon that entity by
//! name over RCON. The summon *name* comes from vanilla, the id comes from
//! vanilla, and this crate is not on either side of the comparison.
//!
//! The **object** table has no dataset behind it at all: `minecraft-data`'s
//! own object rows carry names that are not identifiers, so it is generated
//! from that wire transcript alone. Its coverage is therefore partial — only
//! entities that can be summoned and that spawn through `spawn_entity` — and
//! an uncovered id resolves to `None`, which the adapter reports rather than
//! guessing at.
//!
//! # Why the tables are per protocol even with one era member
//!
//! The unified registry is dense and alphabetical, so **every insertion
//! renumbers everything after it**; a neighbouring release's table would name
//! a plausible wrong mob for almost every spawn, which is the failure class
//! that produces no error at all. [`table_for`] resolves the negotiated
//! protocol once, at adapter construction, and panics rather than guessing.

use crate::{generated_entity_types, generated_object_types};

/// Canonical identifier for a player entity (`spawn_player`).
pub const PLAYER: &str = "minecraft:player";

/// One protocol's entity-type numbering: the unified registry, plus the
/// separate object id space `spawn_entity` still uses at 404.
#[derive(Debug)]
pub struct EntityTypeTable {
    /// Unified-registry `(type id, canonical identifier)` pairs, sorted by id.
    entries: &'static [(i32, &'static str)],
    /// Object-space `(type id, canonical identifier)` pairs, sorted by id.
    /// Sparse: only the ids the wire oracle covers.
    objects: &'static [(i32, &'static str)],
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

    /// Resolves a `spawn_entity_living` type id to its canonical identifier,
    /// through the unified registry.
    #[must_use]
    pub fn mob_type_name(&self, id: i32) -> Option<&'static str> {
        self.entity_type_name(id)
    }

    /// Resolves a `spawn_entity` (object) type id to its canonical
    /// identifier, through the **object** id space — *not* the unified
    /// registry. See the module docs for the measurement behind that split.
    ///
    /// Returns `None` for an id the wire oracle does not cover, which the
    /// caller reports rather than resolving through the other table.
    #[must_use]
    pub fn object_type_name(&self, id: i32) -> Option<&'static str> {
        self.objects
            .binary_search_by_key(&id, |&(key, _)| key)
            .ok()
            .map(|index| self.objects[index].1)
    }

    /// Number of object-space entries the wire oracle covers.
    #[must_use]
    pub const fn object_len(&self) -> usize {
        self.objects.len()
    }
}

/// Minecraft 1.13.2's registry.
static TABLE_404: EntityTypeTable = EntityTypeTable {
    entries: &generated_entity_types::ENTITY_TYPES,
    objects: &generated_object_types::OBJECT_TYPES,
    declared_len: generated_entity_types::ENTITY_TYPE_COUNT,
};

/// Resolves a negotiated protocol to its entity-type tables.
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
