//! The `minecraft:block` registry as a type: [`Block`], plus [`BlockRef`] for
//! the plugin case.
//!
//! # What it is
//!
//! [`Block`] is a generated 1,196-variant enum whose discriminant *is* the
//! `minecraft:block` registry id. It replaces the `&'static str` /
//! `"minecraft:stone"` pair that block identity used to travel as, so that a
//! block is a value the compiler checks rather than a string every consumer
//! re-parses and can silently misspell.
//!
//! # The representation, and what it deliberately is not
//!
//! Three properties, in the order they mattered:
//!
//! 1. **The built-in path is a bare discriminant.** `block as u16` is the wire's
//!    registry id with no lookup and no branch, so every per-block census in
//!    this crate stays a plain array indexed by it. Nothing about supporting
//!    non-vanilla blocks is allowed to cost the vanilla path an instruction.
//! 2. **`Block` has no `Custom` variant and is not `#[non_exhaustive]`.** A
//!    match over it is exhaustive. This is the property that a `Custom(..)` arm
//!    would destroy: every match would need a wildcard, and a wildcard arm in
//!    this codebase is an island factory — a version bump that adds a block
//!    would fall into it silently instead of failing the build.
//! 3. **The plugin case is one level out, in [`BlockRef`].** A `BlockRef` is a
//!    single `u32`: values below [`Block::COUNT`] are a built-in registry id and
//!    anything at or above it is a custom index. Code that can encounter a
//!    plugin block pays exactly one comparison, once, at the point it converts;
//!    code that cannot never names the type. And the custom side carries **no
//!    storage in this crate at all** — [`CustomBlockId`] is an opaque handle
//!    into a registry the host owns, so an application with no plugins links
//!    zero bytes of interner.
//!
//! # Block versus block state
//!
//! These are two id spaces and conflating them is the mistake that surfaces
//! late. [`Block`] is one of 1,196 block *types*, in registration order.
//! [`crate::block_states::StateId`] is one of 32,366 block *states* — a
//! validated newtype index, not an enum, because 32,366 variants is past the
//! point where an enum buys anything and because states are a cross product of
//! property domains rather than a hand-named set. The two orders are unrelated:
//! `minecraft:air` is registry id 0 but the states table is name-sorted. Go
//! between them with [`StateId::block`](crate::block_states::StateId::block) and
//! [`Block::default_state`].
//!
//! # How to change it
//!
//! [`Block`]'s variants and the three index tables are generated — see
//! `src/generated/block_enum.rs` and the `LODESTONE_REGEN=1` command in
//! `tests/tools.rs`. Do not hand-edit either. Everything in *this* file is
//! hand-written accessor code over those tables and is free to change.

use crate::block_states::StateId;
use crate::generated_block_enum as table;
use crate::generated_block_registry::BLOCK_REGISTRY_NAMES;

pub use table::Block;

/// The namespace every built-in block lives in. Asserted by the generator, not
/// assumed: a registry entry outside it would fail generation loudly.
const BUILTIN_NAMESPACE: &str = "minecraft";

impl Block {
    /// The number of built-in blocks — registry ids are `0..COUNT`.
    pub const COUNT: u16 = table::BLOCKS_BY_REGISTRY_ID.len() as u16;

    /// This block's `minecraft:block` registry id, as a `Holder<Block>` carries
    /// it on the wire.
    ///
    /// Free: the discriminant is the id.
    #[must_use]
    pub const fn registry_id(self) -> u16 {
        self as u16
    }

    /// The block with registry id `id`, or `None` if `id` is not in
    /// `0..`[`Block::COUNT`].
    ///
    /// This is the only fallible step on the decode path. Once you hold a
    /// `Block`, every accessor below is total — which is the point of the type:
    /// the `Option` lives at one construction site instead of at every call
    /// site.
    #[must_use]
    pub fn from_registry_id(id: u16) -> Option<Self> {
        table::BLOCKS_BY_REGISTRY_ID.get(id as usize).copied()
    }

    /// The canonical namespaced name, for example `"minecraft:oak_stairs"`.
    ///
    /// Zero-heap: a `&'static str` straight out of rodata, from the *same*
    /// table `block_type_name` reads. There is exactly one copy of each block
    /// name in the binary.
    #[must_use]
    pub fn name(self) -> &'static str {
        BLOCK_REGISTRY_NAMES[self as usize]
    }

    /// The name without its namespace, for example `"oak_stairs"`.
    #[must_use]
    pub fn path(self) -> &'static str {
        // Every built-in name is `minecraft:<path>`, asserted at generation.
        &self.name()[BUILTIN_NAMESPACE.len() + 1..]
    }

    /// The block named `name`, or `None` if this version has no such block.
    ///
    /// Accepts both the namespaced form (`"minecraft:stone"`) and the bare path
    /// (`"stone"`), matching vanilla's own identifier parsing, and allocates for
    /// neither. `O(log 1196)`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        // `REGISTRY_IDS_BY_NAME` is sorted by full canonical name, and every
        // canonical name shares the one `minecraft:` prefix — so that order is
        // also path order, and a bare path can binary-search the same
        // permutation without being rewritten into a namespaced `String`.
        // `name_order_is_also_path_order` in `tests/block_enum.rs` is what keeps
        // that true rather than merely presently correct.
        let (key, project): (&str, fn(Block) -> &'static str) = match name.split_once(':') {
            Some((BUILTIN_NAMESPACE, path)) => (path, Block::path),
            // A namespace other than `minecraft:` is never a built-in block.
            Some(_) => return None,
            None => (name, Block::path),
        };
        let ids = &table::REGISTRY_IDS_BY_NAME;
        let slot = ids
            .binary_search_by(|&id| {
                project(table::BLOCKS_BY_REGISTRY_ID[id as usize]).cmp(key)
            })
            .ok()?;
        Some(table::BLOCKS_BY_REGISTRY_ID[ids[slot] as usize])
    }

    /// This block's own default-block-state, as a validated [`StateId`].
    ///
    /// Total, and O(1). The default is *not* the block's lowest state id — it
    /// differs for 661 of the 797 multi-state blocks — so this reads the
    /// server's own default mark out of a generated column rather than
    /// inferring one.
    #[must_use]
    pub fn default_state(self) -> StateId {
        StateId::new(table::DEFAULT_STATE[self as usize])
            .expect("generated default-state column holds a valid state id")
    }

    /// Every block, in registration order.
    pub fn all() -> impl ExactSizeIterator<Item = Self> + Clone {
        table::BLOCKS_BY_REGISTRY_ID.iter().copied()
    }
}

/// A handle to a block this build's registry does not contain — one a plugin or
/// a data pack added.
///
/// Deliberately opaque and deliberately storage-free: the mapping from a
/// `CustomBlockId` back to a name belongs to whichever component owns the
/// plugin registry, so this crate ships no interner and an application with no
/// plugins pays nothing for the existence of this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomBlockId(u32);

impl CustomBlockId {
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

/// Either a built-in [`Block`] or a plugin-supplied one, in a single `u32`.
///
/// # Why this is a separate type rather than a `Block::Custom` variant
///
/// Folding the custom case into `Block` would tax the 99% case three ways: every
/// match over a block would need a wildcard or a `Custom` arm (and a wildcard is
/// how a subsystem silently stops handling a newly added block); `Block` would
/// stop being a bare `u16` discriminant, so per-block censuses could no longer
/// be plain arrays indexed by it; and `block as u16 == registry id` — the
/// property the whole representation rests on — would no longer hold.
///
/// Keeping it one level out means the branch exists exactly where a plugin block
/// can actually appear, and nowhere else.
///
/// # Encoding
///
/// `0..Block::COUNT` is a built-in registry id; `Block::COUNT..` is
/// `Block::COUNT + custom index`. So [`BlockRef::builtin`] is a widening cast
/// with no arithmetic, and [`BlockRef::kind`] is one comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockRef(u32);

/// The resolved form of a [`BlockRef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// A block in this version's built-in registry.
    Builtin(Block),
    /// A block only the host's plugin registry knows about.
    Custom(CustomBlockId),
}

impl BlockRef {
    /// A reference to a built-in block. No arithmetic, no allocation.
    #[must_use]
    pub const fn builtin(block: Block) -> Self {
        Self(block as u16 as u32)
    }

    /// A reference to a plugin-supplied block.
    ///
    /// # Panics
    ///
    /// Panics if the index is large enough to overflow the encoding — a host
    /// registry with more than `u32::MAX - 1196` entries, which is a
    /// programming error rather than a runtime condition.
    #[must_use]
    pub const fn custom(id: CustomBlockId) -> Self {
        match id.0.checked_add(Block::COUNT as u32) {
            Some(raw) => Self(raw),
            None => panic!("custom block index overflows the BlockRef encoding"),
        }
    }

    /// Splits into the built-in or custom case. One comparison.
    #[must_use]
    pub const fn kind(self) -> BlockKind {
        match Block::from_registry_id_const(self.0) {
            Some(block) => BlockKind::Builtin(block),
            None => BlockKind::Custom(CustomBlockId(self.0 - Block::COUNT as u32)),
        }
    }

    /// The built-in block, or `None` for a plugin block.
    ///
    /// The shape most consumers want: a subsystem that has no meaning for a
    /// non-vanilla block says so once, here, instead of threading a second case
    /// through its whole match.
    #[must_use]
    pub const fn builtin_or_none(self) -> Option<Block> {
        match self.kind() {
            BlockKind::Builtin(block) => Some(block),
            BlockKind::Custom(_) => None,
        }
    }
}

impl From<Block> for BlockRef {
    fn from(block: Block) -> Self {
        Self::builtin(block)
    }
}

impl Block {
    /// `const` sibling of [`Block::from_registry_id`] taking a `u32`, for
    /// [`BlockRef::kind`]. Indexing a static in a `const fn` is allowed, so this
    /// is the same table lookup rather than a second source of truth.
    const fn from_registry_id_const(id: u32) -> Option<Self> {
        if id < Self::COUNT as u32 {
            Some(table::BLOCKS_BY_REGISTRY_ID[id as usize])
        } else {
            None
        }
    }
}
