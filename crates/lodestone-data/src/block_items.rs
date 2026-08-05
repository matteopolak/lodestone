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

use crate::generated_block_items as generated;

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
pub fn block_for_item_id(id: i32) -> Option<&'static str> {
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
#[must_use]
pub fn block_for_item(item: &str) -> Option<&'static str> {
    block_for_item_id(crate::items::item_id(item)?)
}

/// Whether `item` places a block at all — [`block_for_item`] reduced to a
/// predicate, for callers that only need the yes/no.
#[must_use]
pub fn is_block_item(item: &str) -> bool {
    block_for_item(item).is_some()
}
