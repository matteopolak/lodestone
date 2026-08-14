//! The `minecraft:item` registry as a type: [`Item`], plus [`ItemRef`] for the
//! plugin case.
//!
//! # What it is
//!
//! [`Item`] is a generated 1,537-variant enum whose discriminant *is* the
//! `minecraft:item` registry id — the same representation [`crate::block`]
//! established for [`crate::block::Block`], applied to the second of the two
//! large registries named in `docs/registry-types.md`'s Stage 1. It replaces
//! the `&'static str` / `"minecraft:diamond_pickaxe"` pair an item stack's
//! identity used to travel as.
//!
//! # The representation, and what it deliberately is not
//!
//! Identical to [`crate::block::Block`]'s three properties, restated for this
//! registry:
//!
//! 1. **The built-in path is a bare discriminant.** `item as u16` is the
//!    wire's registry id with no lookup and no branch, so every per-item
//!    census in this crate stays a plain array indexed by it.
//! 2. **`Item` has no `Custom` variant and is not `#[non_exhaustive]`.** A
//!    match over it is exhaustive, so a version bump that adds an item fails
//!    the compile of every incomplete match instead of falling into a
//!    wildcard.
//! 3. **The plugin case is one level out, in [`ItemRef`].** A `ItemRef` is a
//!    single `u32`: values below [`Item::COUNT`] are a built-in registry id
//!    and anything at or above it is a custom index. [`CustomItemId`] carries
//!    no storage in this crate — an application with no plugins links zero
//!    bytes of interner.
//!
//! # How to change it
//!
//! [`Item`] and its two index tables are generated — see
//! `src/generated/item_enum.rs` and the `LODESTONE_REGEN=1` command in
//! `tests/item_enum.rs`. Do not hand-edit either. Everything in *this* file is
//! hand-written accessor code over those tables and is free to change.
//!
//! Unlike `Block`, there is no default-state table here: an item has no
//! analogue of a block state, so the generated file carries only the enum
//! and the two id/name index tables, and `item.rs` needs no `default_state`
//! accessor.

use crate::generated_item_enum as table;
use crate::generated_items::ITEM_NAMES;

pub use table::Item;

/// The namespace every built-in item lives in. Asserted by the generator, not
/// assumed: a registry entry outside it would fail generation loudly.
const BUILTIN_NAMESPACE: &str = "minecraft";

impl Item {
    /// The number of built-in items — registry ids are `0..COUNT`.
    pub const COUNT: u16 = table::ITEMS_BY_REGISTRY_ID.len() as u16;

    /// This item's `minecraft:item` registry id, as a `Holder<Item>` carries
    /// it on the wire.
    ///
    /// Free: the discriminant is the id.
    #[must_use]
    pub const fn registry_id(self) -> u16 {
        self as u16
    }

    /// The item with registry id `id`, or `None` if `id` is not in
    /// `0..`[`Item::COUNT`].
    ///
    /// This is the only fallible step on the decode path. Once you hold an
    /// `Item`, every accessor below is total.
    #[must_use]
    pub fn from_registry_id(id: u16) -> Option<Self> {
        table::ITEMS_BY_REGISTRY_ID.get(id as usize).copied()
    }

    /// The canonical namespaced name, for example `"minecraft:diamond_hoe"`.
    ///
    /// Zero-heap: a `&'static str` straight out of rodata, the same table
    /// [`crate::items::item_name`] reads. There is exactly one copy of each
    /// item name in the binary.
    #[must_use]
    pub fn name(self) -> &'static str {
        ITEM_NAMES[self as usize]
    }

    /// The name without its namespace, for example `"diamond_hoe"`.
    #[must_use]
    pub fn path(self) -> &'static str {
        // Every built-in name is `minecraft:<path>`, asserted at generation.
        &self.name()[BUILTIN_NAMESPACE.len() + 1..]
    }

    /// The item named `name`, or `None` if this version has no such item.
    ///
    /// Accepts both the namespaced form (`"minecraft:stone"`) and the bare
    /// path (`"stone"`), matching vanilla's own identifier parsing, and
    /// allocates for neither. `O(log 1537)`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        // `REGISTRY_IDS_BY_NAME` is sorted by full canonical name, and every
        // canonical name shares the one `minecraft:` prefix — so that order
        // is also path order, and a bare path can binary-search the same
        // permutation without being rewritten into a namespaced `String`.
        // `name_order_is_also_path_order` in `tests/item_enum.rs` is what
        // keeps that true rather than merely presently correct.
        let (key, project): (&str, fn(Item) -> &'static str) = match name.split_once(':') {
            Some((BUILTIN_NAMESPACE, path)) => (path, Item::path),
            Some(_) => return None,
            None => (name, Item::path),
        };
        let ids = &table::REGISTRY_IDS_BY_NAME;
        let slot = ids
            .binary_search_by(|&id| project(table::ITEMS_BY_REGISTRY_ID[id as usize]).cmp(key))
            .ok()?;
        Some(table::ITEMS_BY_REGISTRY_ID[ids[slot] as usize])
    }

    /// Every item, in registration order.
    pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {
        table::ITEMS_BY_REGISTRY_ID.iter().copied()
    }
}

/// A handle to an item this build's registry does not contain — one a plugin
/// or a data pack added.
///
/// Deliberately opaque and deliberately storage-free — see
/// [`crate::block::CustomBlockId`], whose reasoning applies unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomItemId(u32);

impl CustomItemId {
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

/// Either a built-in [`Item`] or a plugin-supplied one, in a single `u32`.
///
/// See [`crate::block::BlockRef`] for why this is a separate type rather than
/// an `Item::Custom` variant — the reasoning is identical, substituting
/// "item" for "block" throughout.
///
/// # Encoding
///
/// `0..Item::COUNT` is a built-in registry id; `Item::COUNT..` is
/// `Item::COUNT + custom index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemRef(u32);

/// The resolved form of an [`ItemRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// An item in this version's built-in registry.
    Builtin(Item),
    /// An item only the host's plugin registry knows about.
    Custom(CustomItemId),
}

impl ItemRef {
    /// A reference to a built-in item. No arithmetic, no allocation.
    #[must_use]
    pub const fn builtin(item: Item) -> Self {
        Self(item as u16 as u32)
    }

    /// A reference to a plugin-supplied item.
    ///
    /// # Panics
    ///
    /// Panics if the index is large enough to overflow the encoding — a host
    /// registry with more than `u32::MAX - 1537` entries, which is a
    /// programming error rather than a runtime condition.
    #[must_use]
    pub const fn custom(id: CustomItemId) -> Self {
        match id.0.checked_add(Item::COUNT as u32) {
            Some(raw) => Self(raw),
            None => panic!("custom item index overflows the ItemRef encoding"),
        }
    }

    /// Splits into the built-in or custom case. One comparison.
    #[must_use]
    pub const fn kind(self) -> ItemKind {
        match Item::from_registry_id_const(self.0) {
            Some(item) => ItemKind::Builtin(item),
            None => ItemKind::Custom(CustomItemId(self.0 - Item::COUNT as u32)),
        }
    }

    /// The built-in item, or `None` for a plugin item.
    #[must_use]
    pub const fn builtin_or_none(self) -> Option<Item> {
        match self.kind() {
            ItemKind::Builtin(item) => Some(item),
            ItemKind::Custom(_) => None,
        }
    }
}

impl From<Item> for ItemRef {
    fn from(item: Item) -> Self {
        Self::builtin(item)
    }
}

impl Item {
    /// `const` sibling of [`Item::from_registry_id`] taking a `u32`, for
    /// [`ItemRef::kind`]. Indexing a static in a `const fn` is allowed, so
    /// this is the same table lookup rather than a second source of truth.
    const fn from_registry_id_const(id: u32) -> Option<Self> {
        if id < Self::COUNT as u32 {
            Some(table::ITEMS_BY_REGISTRY_ID[id as usize])
        } else {
            None
        }
    }
}
