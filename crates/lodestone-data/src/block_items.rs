//! Item → placed-block census for protocol 776 (Minecraft 26.2): which block,
//! if any, a given item places when a player right-clicks with it.
//!
//! # Why this is a census and not a name match
//!
//! Vanilla's `BlockItem` holds an explicit `Block` reference
//! (`BlockItem.getBlock()`, `BlockItem.java:185`) and is registered against an
//! item id that need not match it. "The item places the block of the same
//! name" is therefore a heuristic, and it is measurably wrong: against the
//! committed dump it disagrees on **16 of the 1,537 items**, in both
//! directions.
//!
//! * **14 false negatives** — a real placeable item it declines outright,
//!   because the block has a different name. `minecraft:redstone` places
//!   `minecraft:redstone_wire` (`Items.java:753-755`); `minecraft:string`
//!   places `minecraft:tripwire`; `wheat_seeds`→`wheat`,
//!   `cocoa_beans`→`cocoa`, `carrot`→`carrots`, `potato`→`potatoes`,
//!   `pumpkin_seeds`→`pumpkin_stem`, `melon_seeds`→`melon_stem`.
//! * **2 false positives** — a block of that name exists but the item is not a
//!   `BlockItem` at all, so a name match would place a block for an item that
//!   places nothing. `minecraft:wheat` is the crop's *drop*
//!   (`Items.java:1048`, a plain `registerItem`) while `minecraft:wheat` the
//!   block is the crop; and `minecraft:air`.
//!
//! A false positive is the worse failure, because it places a block the player
//! never asked for — the same class of defect as writing `minecraft:stone` for
//! everything (#466), just rarer.
//!
//! # Scope: `BlockItem`, and the block, not its state
//!
//! Two deliberate cuts:
//!
//! * **Only `BlockItem` counts as placing a block.** Items that put something
//!   into the world by another mechanism — `BucketItem` placing a fluid, spawn
//!   eggs and minecarts spawning entities, `flint_and_steel` lighting a fire —
//!   report `None` here. Each needs its own mechanism, and folding them in
//!   would be a hand-written guess wearing generated clothes.
//! * **This answers the *block*, never the *state*.** Stairs, slabs, logs,
//!   doors, and redstone dust all place with orientation or connection state
//!   derived from the click face, cursor position and neighbours. Consumers
//!   that need a state id must resolve one themselves (the shell's
//!   `sim::placement` census does); this table hands back the block's
//!   canonical name only.
//!
//! # Dependencies
//!
//! Generated from `tests/support/block_items_jvm.txt`, an authoritative dump
//! from the real 26.2 server (`oracle-java/BlockItemOracle.java`). See
//! `tests/block_items.rs` for the drift guard and the `LODESTONE_REGEN=1`
//! refresh command.

use crate::block::Block;
use crate::generated_block_items as generated;
use crate::item::Item;

pub use generated::ITEM_COUNT;

/// The block that the item with **network registry id** `id` places, or `None`
/// when that item places no block — either because it is not a `BlockItem`
/// (a sword, a bucket, a spawn egg) or because `id` is outside
/// `0..`[`ITEM_COUNT`].
///
/// O(1). This is the hot path: a decoded item stack already holds the registry
/// id.
///
/// The two `None` reasons are deliberately not distinguished: every caller
/// this exists for (placement) does the same thing with both, and an unknown
/// id is exactly as unplaceable as a sword.
#[must_use]
pub fn block_for_item_id(id: i32) -> Option<Block> {
    usize::try_from(id)
        .ok()
        .and_then(|index| generated::BLOCK_FOR_ITEM.get(index))
        .copied()
        .flatten()
}

/// The block that `item` (for example `"minecraft:dirt"` or
/// `"minecraft:redstone"`) places, or `None` for an item that places no block
/// and for an item this version does not know.
///
/// Resolves the name through [`crate::items::item_id`], so it costs one linear
/// scan of the item-name table — the same trade
/// [`crate::item_prototypes::prototype`] makes, and for the same reason:
/// placement is a per-right-click query, not a per-tick one, and minting a
/// second name index here could drift from the first.
///
/// # Prefer the typed forms
///
/// Both halves of this signature are the un-migrated string spelling, kept
/// because it is what the placement path in `lodestone-server` still calls.
/// New code should take the item as a typed id and use [`block_for_item_id`],
/// which hands back a [`Block`]; the string here is produced by
/// [`Block::name`], so the two can never disagree.
#[must_use]
pub fn block_for_item(item: &str) -> Option<&'static str> {
    block_for_item_id(crate::items::item_id(item)?).map(Block::name)
}

/// The block that `item` places, as a [`Block`] — the typed sibling of
/// [`block_for_item`].
#[must_use]
pub fn block_placed_by(item: &str) -> Option<Block> {
    block_for_item_id(crate::items::item_id(item)?)
}

/// Whether `item` places a block at all — [`block_for_item`] reduced to a
/// predicate, for callers that only need the yes/no.
#[must_use]
pub fn is_block_item(item: &str) -> bool {
    block_for_item(item).is_some()
}

/// The **inverse** of [`block_for_item_id`]: the item that picking `block`
/// yields — vanilla's `Block.asItem()` (`Item.byBlock(this)`), which is what
/// `BlockBehaviour.getCloneItemStack`'s default `new ItemStack(this.asItem())`
/// bottoms out in, and therefore what pick-block (middle-click) resolves for
/// every block with no `getCloneItemStack` override.
///
/// `None` for a block with no registered `BlockItem` at all — air, fire,
/// fluids, redstone wire, portal blocks, and every other block that is placed
/// by some mechanism other than right-clicking a `BlockItem`. That is exactly
/// vanilla's own answer: `Item.byBlock` for such a block returns the sentinel
/// `Items.AIR`, and `getCloneItemStack`'s caller discards an empty stack — so
/// reporting `None` here rather than a fake `minecraft:air` item spares every
/// caller from re-deriving "empty" from a specific item value.
///
/// # Not a second table
///
/// This is **not** a second hand-maintained census: `BLOCK_FOR_ITEM` already
/// carries the whole item→block relation, and vanilla registers each block
/// through at most one `BlockItem`, so the reverse mapping is a pure function
/// of the forward one. Computed once behind a [`std::sync::OnceLock`] (1,196
/// entries, one linear pass over `BLOCK_FOR_ITEM`) rather than duplicated as a
/// second generated file, so the two directions cannot independently drift —
/// see `crate::block_states::block_state_index`'s identical
/// compute-once-from-the-generated-table shape.
///
/// # Scope cut: the block, never a `getCloneItemStack` override
///
/// Like [`block_for_item`]'s own "block, never state" cut, this answers only
/// the *default* clone-item-stack. Real vanilla overrides it per block for
/// crops (a wheat *block* clones to `wheat_seeds`, not itself), flower pots
/// (clones the potted plant), banners (clones the banner entity's pattern
/// data), beehives, candle cakes, and a dozen more — each one a distinct
/// `BlockBehaviour.getCloneItemStack` override this crate has no per-block
/// model for. Callers that need one of those need their own lookup; this
/// function is deliberately the base case only, the same "generator-derived
/// default, not the full override set" cut this module already makes for
/// placement.
#[must_use]
pub fn item_for_block(block: Block) -> Option<Item> {
    static INDEX: std::sync::OnceLock<Vec<Option<Item>>> = std::sync::OnceLock::new();
    let inverse = INDEX.get_or_init(|| {
        let mut table = vec![None; Block::COUNT as usize];
        for id in 0..generated::ITEM_COUNT {
            let Some(placed) = generated::BLOCK_FOR_ITEM[id as usize] else {
                continue;
            };
            // Vanilla registers each block through at most one `BlockItem`
            // (`Items.registerBlock` is called once per block), so the first
            // writer to a slot is the only writer in practice; keeping it
            // rather than overwriting is a defensive tie-break, not a
            // modelled choice.
            let slot = &mut table[placed as usize];
            if slot.is_none() {
                *slot = Item::from_registry_id(id as u16);
            }
        }
        table
    });
    inverse.get(block as usize).copied().flatten()
}
