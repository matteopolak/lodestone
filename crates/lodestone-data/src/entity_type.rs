//! The `minecraft:entity_type` registry as a type: [`EntityType`], plus
//! [`EntityTypeRef`] for the plugin case.
//!
//! # What it is
//!
//! [`EntityType`] is a generated 158-variant enum whose discriminant *is* the
//! `minecraft:entity_type` registry id — the same representation
//! [`crate::block`] established for [`crate::block::Block`], applied to the
//! smaller of the two Stage 1 registries in `docs/registry-types.md`. 158
//! entries fit in a `u8`, unlike `Block` (1,196) or `Item` (1,537), so this is
//! the one Stage 1 registry that uses `#[repr(u8)]`.
//!
//! # The representation, and what it deliberately is not
//!
//! Identical to [`crate::block::Block`]'s three properties, restated for this
//! registry:
//!
//! 1. **The built-in path is a bare discriminant.** `entity_type as u8` is
//!    the wire's registry id with no lookup and no branch, so every
//!    per-entity-type census in this crate stays a plain array indexed by
//!    it.
//! 2. **`EntityType` has no `Custom` variant and is not `#[non_exhaustive]`.**
//!    A match over it is exhaustive, so a version bump that adds an entity
//!    type fails the compile of every incomplete match instead of falling
//!    into a wildcard.
//! 3. **The plugin case is one level out, in [`EntityTypeRef`].** An
//!    `EntityTypeRef` is a single `u32`: values below [`EntityType::COUNT`]
//!    are a built-in registry id and anything at or above it is a custom
//!    index. [`CustomEntityTypeId`] carries no storage in this crate.
//!
//! # How to change it
//!
//! [`EntityType`] and its two index tables are generated — see
//! `src/generated/entity_type_enum.rs` and the `LODESTONE_REGEN=1` command in
//! `tests/entity_type_enum.rs`. Do not hand-edit either. Everything in *this*
//! file is hand-written accessor code over those tables and is free to
//! change.

use crate::generated_entity_type_enum as table;
use crate::generated_entity_types::ENTITY_TYPE_NAMES;

pub use table::EntityType;

/// The namespace every built-in entity type lives in. Asserted by the
/// generator, not assumed: a registry entry outside it would fail generation
/// loudly.
const BUILTIN_NAMESPACE: &str = "minecraft";

impl EntityType {
    /// The number of built-in entity types — registry ids are `0..COUNT`.
    pub const COUNT: u8 = table::TYPES_BY_REGISTRY_ID.len() as u8;

    /// This entity type's `minecraft:entity_type` registry id, as `add_entity`
    /// carries it on the wire.
    ///
    /// Free: the discriminant is the id.
    #[must_use]
    pub const fn registry_id(self) -> u8 {
        self as u8
    }

    /// The entity type with registry id `id`, or `None` if `id` is not in
    /// `0..`[`EntityType::COUNT`].
    ///
    /// This is the only fallible step on the decode path. Once you hold an
    /// `EntityType`, every accessor below is total.
    #[must_use]
    pub fn from_registry_id(id: u8) -> Option<Self> {
        table::TYPES_BY_REGISTRY_ID.get(id as usize).copied()
    }

    /// The canonical namespaced name, for example `"minecraft:zombie"`.
    ///
    /// Zero-heap: a `&'static str` straight out of rodata, the same table
    /// [`crate::entity_types::entity_type_name`] reads. There is exactly one
    /// copy of each entity-type name in the binary.
    #[must_use]
    pub fn name(self) -> &'static str {
        ENTITY_TYPE_NAMES[self as usize]
    }

    /// The name without its namespace, for example `"zombie"`.
    #[must_use]
    pub fn path(self) -> &'static str {
        // Every built-in name is `minecraft:<path>`, asserted at generation.
        &self.name()[BUILTIN_NAMESPACE.len() + 1..]
    }

    /// The entity type named `name`, or `None` if this version has no such
    /// type.
    ///
    /// Accepts both the namespaced form (`"minecraft:pig"`) and the bare path
    /// (`"pig"`), matching vanilla's own identifier parsing, and allocates
    /// for neither. `O(log 158)`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        // `REGISTRY_IDS_BY_NAME` is sorted by full canonical name, and every
        // canonical name shares the one `minecraft:` prefix — so that order
        // is also path order. `name_order_is_also_path_order` in
        // `tests/entity_type_enum.rs` is what keeps that true rather than
        // merely presently correct.
        let (key, project): (&str, fn(EntityType) -> &'static str) = match name.split_once(':') {
            Some((BUILTIN_NAMESPACE, path)) => (path, EntityType::path),
            Some(_) => return None,
            None => (name, EntityType::path),
        };
        let ids = &table::REGISTRY_IDS_BY_NAME;
        let slot = ids
            .binary_search_by(|&id| {
                project(table::TYPES_BY_REGISTRY_ID[id as usize]).cmp(key)
            })
            .ok()?;
        Some(table::TYPES_BY_REGISTRY_ID[ids[slot] as usize])
    }

    /// Every entity type, in registration order.
    pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {
        table::TYPES_BY_REGISTRY_ID.iter().copied()
    }
}

/// A handle to an entity type this build's registry does not contain — one a
/// plugin or a data pack added.
///
/// Deliberately opaque and deliberately storage-free — see
/// [`crate::block::CustomBlockId`], whose reasoning applies unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomEntityTypeId(u32);

impl CustomEntityTypeId {
    /// Wraps a host-assigned index.
    #[must_use]
    pub const fn from_index(index: u32) -> Self {
        Self(index)
    }

    /// The host-assigned index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Either a built-in [`EntityType`] or a plugin-supplied one, in a single
/// `u32`.
///
/// See [`crate::block::BlockRef`] for why this is a separate type rather than
/// an `EntityType::Custom` variant — the reasoning is identical, substituting
/// "entity type" for "block" throughout.
///
/// # Encoding
///
/// `0..EntityType::COUNT` is a built-in registry id;
/// `EntityType::COUNT..` is `EntityType::COUNT + custom index`. Widened to
/// `u32` (rather than `u16`, which would suffice for `EntityType` alone) so
/// the encoding has the same headroom as [`crate::block::BlockRef`] and
/// [`crate::item::ItemRef`] for a plugin registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityTypeRef(u32);

/// The resolved form of an [`EntityTypeRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityTypeKind {
    /// An entity type in this version's built-in registry.
    Builtin(EntityType),
    /// An entity type only the host's plugin registry knows about.
    Custom(CustomEntityTypeId),
}

impl EntityTypeRef {
    /// A reference to a built-in entity type. No arithmetic, no allocation.
    #[must_use]
    pub const fn builtin(entity_type: EntityType) -> Self {
        Self(entity_type as u8 as u32)
    }

    /// A reference to a plugin-supplied entity type.
    ///
    /// # Panics
    ///
    /// Panics if the index is large enough to overflow the encoding — a host
    /// registry with more than `u32::MAX - 158` entries, which is a
    /// programming error rather than a runtime condition.
    #[must_use]
    pub const fn custom(id: CustomEntityTypeId) -> Self {
        match id.0.checked_add(EntityType::COUNT as u32) {
            Some(raw) => Self(raw),
            None => panic!("custom entity type index overflows the EntityTypeRef encoding"),
        }
    }

    /// Splits into the built-in or custom case. One comparison.
    #[must_use]
    pub const fn kind(self) -> EntityTypeKind {
        match EntityType::from_registry_id_const(self.0) {
            Some(entity_type) => EntityTypeKind::Builtin(entity_type),
            None => EntityTypeKind::Custom(CustomEntityTypeId(self.0 - EntityType::COUNT as u32)),
        }
    }

    /// The built-in entity type, or `None` for a plugin entity type.
    #[must_use]
    pub const fn builtin_or_none(self) -> Option<EntityType> {
        match self.kind() {
            EntityTypeKind::Builtin(entity_type) => Some(entity_type),
            EntityTypeKind::Custom(_) => None,
        }
    }
}

impl From<EntityType> for EntityTypeRef {
    fn from(entity_type: EntityType) -> Self {
        Self::builtin(entity_type)
    }
}

impl EntityType {
    /// `const` sibling of [`EntityType::from_registry_id`] taking a `u32`,
    /// for [`EntityTypeRef::kind`]. Indexing a static in a `const fn` is
    /// allowed, so this is the same table lookup rather than a second source
    /// of truth.
    const fn from_registry_id_const(id: u32) -> Option<Self> {
        if id < Self::COUNT as u32 {
            Some(table::TYPES_BY_REGISTRY_ID[id as usize])
        } else {
            None
        }
    }
}
