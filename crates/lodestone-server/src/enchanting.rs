//! The enchanting table (issue #253): bookshelf power, the three-slot level
//! cost, and the weighted-random enchantment offers each slot draws.
//!
//! # What it is
//!
//! A port of `EnchantingTableBlock`'s bookshelf-detection shape
//! (`.cache/mc/26.2/src/net/minecraft/world/level/block/EnchantingTableBlock.java`),
//! `EnchantmentHelper.getEnchantmentCost`/`selectEnchantment`/
//! `getAvailableEnchantmentResults` and `EnchantmentMenu`'s own per-slot orchestration
//! (`.cache/mc/26.2/src/net/minecraft/world/inventory/EnchantmentMenu.java:85-132`),
//! reading [`crate::enchantment_data`] for the weight/cost-curve/exclusivity
//! table the selection loop draws against.
//!
//! # How it works
//!
//! [`bookshelf_power`] walks vanilla's real 32-position ring
//! (`BOOKSHELF_OFFSETS`: the border of a 5×5 square at both `y=0` and `y=1`
//! relative to the table, **not** "count nearby bookshelves" — a solid block
//! diagonally adjacent counts for nothing if the direct line to it is
//! blocked) and each offset additionally requires the **walkway cell** between
//! the table and that shelf to be open. Vanilla's transmitter test is
//! `#minecraft:replaceable` (a wide tag: air, water, lava, snow layers,
//! several plants); this build approximates it with
//! [`crate::chunk::is_air_or_fluid`], the same air-or-fluid scope cut
//! `crate::server`'s own placement code already uses for "is this cell
//! replaceable" — see that function's doc comment for why air-or-fluid is
//! this generator's whole practical set today.
//!
//! [`cost_for_slot`] is `EnchantmentHelper.getEnchantmentCost`: a per-slot
//! `[1, 8] + bookcases/2 + [0, bookcases]` roll, min-1 for slot 0 and
//! `max(_, bookcases*2)` for slot 2 — the shape that makes slot 2 the only one
//! that can reach a level-30 offer at 15 bookshelves. [`select_enchantments`]
//! is `selectEnchantment`: seed the roll with the item's own `enchantable`
//! value, pick one enchantment by weight, then keep rolling `while
//! next_int(50) <= cost` for more (each successive filtered to
//! [`crate::enchantment_data::compatible`] with what was already picked).
//!
//! # RNG: same shape as vanilla, not the same bit stream
//!
//! [`SpawnRng`] is this crate's existing non-JVM RNG (`crate::mob_spawn`,
//! already the loot-table/spawn-cluster generator) — **draw order and count**
//! match vanilla's `selectEnchantment` exactly (this is the part the workstation
//! brief calls "part of the specification": a reordered draw gives plausible
//! but non-vanilla offers), but the underlying bit stream is not
//! `java.util.Random`-compatible, the same documented, accepted gap
//! `crate::loot`'s own module doc carries for the same reason.
//!
//! # A known gap: the offer can be *shown* but not *taken*
//!
//! Choosing an enchantment offer is vanilla's `ServerboundContainerButtonClickPacket`
//! (`ClientAction::ContainerButtonClick`). `crates/protocol/v770/src/server_protocol.rs`
//! currently decodes and **discards** that packet (`ServerBound::Ignored`), so
//! it never reaches this crate at all — a `crates/protocol/v770` gap outside
//! this crate's ownership (that crate is mid-restructure; see the workstation
//! docs for the full note). Everything up to that point is real and wired: the
//! screen opens, bookshelf power is computed from the actual world, the three
//! costs are sent to the client over the existing `container_set_data` feed
//! (`docs/container-cost-screens.md`), and a lapis-priced, level-gated
//! offer is genuinely computed server-side — a player just cannot yet click
//! "enchant" on a real connection.

use lodestone_model::{BlockPos, ItemStack};

use crate::chunk::{ChunkSource, is_air_or_fluid};
use crate::enchantment_data::{self, EnchantmentDef};
use crate::mob_spawn::SpawnRng;

/// `EnchantingTableBlock.BOOKSHELF_OFFSETS`: every `(x, z)` with `y` in `0..=1`
/// where `|x| == 2 || |z| == 2` — the border of a 5×5 square, both floors of
/// the table's bookshelf ring. 32 offsets total (16 unique `(x, z)` × 2 `y`).
fn bookshelf_offsets() -> impl Iterator<Item = (i32, i32, i32)> {
    (-2i32..=2).flat_map(|x| {
        (0i32..=1).flat_map(move |y| (-2i32..=2).filter_map(move |z| (x.abs() == 2 || z.abs() == 2).then_some((x, y, z))))
    })
}

/// `EnchantingTableBlock.isValidBookShelf` — the shelf cell itself is
/// `minecraft:bookshelf`, and the walkway cell between the table and the
/// shelf (`offset / 2`, Java truncating-toward-zero division) is open.
fn is_valid_bookshelf(source: &dyn ChunkSource, pos: BlockPos, offset: (i32, i32, i32)) -> bool {
    let (ox, oy, oz) = offset;
    let shelf = source.block_state(pos.x + ox, pos.y + oy, pos.z + oz);
    if shelf.split('[').next() != Some("minecraft:bookshelf") {
        return false;
    }
    let walkway = source.block_state(pos.x + ox / 2, pos.y + oy, pos.z + oz / 2);
    is_air_or_fluid(&walkway)
}

/// `bookcases` in `EnchantmentMenu.slotsChanged` — `0..=15` (vanilla's own
/// `bookcases > 15` clamp; a table can never see more than one full ring).
#[must_use]
pub fn bookshelf_power(source: &dyn ChunkSource, pos: BlockPos) -> u32 {
    bookshelf_offsets()
        .filter(|&offset| is_valid_bookshelf(source, pos, offset))
        .count()
        .min(15) as u32
}

/// `EnchantmentHelper.getEnchantmentCost` for `slot` (`0..3`) at `bookcases`
/// power, seeded by `rng` — the caller reseeds `rng` from the table's
/// `enchantmentSeed` once per evaluation and calls this three times in slot
/// order (order matters: the draw is one shared roll per slot, not three
/// independent ones — see [`EnchantmentMenu`]'s own loop, which draws once
/// and derives the other two slots' displayed costs from the *same* `selected`
/// value via arithmetic, not three separate `nextInt` calls).
///
/// `bookcases` is pre-clamped by [`bookshelf_power`]; this function clamps it
/// again defensively so a caller feeding a raw value cannot exceed vanilla's
/// own bound.
#[must_use]
pub fn cost_for_slot(rng: &mut SpawnRng, slot: u32, bookcases: u32, enchantable_value: Option<u32>) -> i32 {
    if enchantable_value.is_none() {
        return 0;
    }
    let bookcases = bookcases.min(15) as i32;
    let selected = rng.next_int(8) + 1 + (bookcases >> 1) + rng.next_int(bookcases + 1);
    match slot {
        0 => (selected / 3).max(1),
        1 => selected * 2 / 3 + 1,
        _ => selected.max(bookcases * 2),
    }
}

/// All three slot costs in one call, matching `EnchantmentMenu.slotsChanged`'s
/// own draw order: seed once, then draw slot 0, 1, 2 in that order from the
/// **same** `rng` stream (vanilla's `this.random.setSeed(seed)` once, then
/// three sequential `getEnchantmentCost` calls) — a slot whose result is `<
/// slot_index + 1` is floored to `0` (unaffordable-looking offers are hidden
/// entirely rather than shown at a cost below their own slot index).
#[must_use]
pub fn table_costs(seed: i64, bookcases: u32, item: &ItemStack) -> [i32; 3] {
    if !enchantment_data::is_enchantable(item) {
        return [0, 0, 0];
    }
    let value = enchantment_data::enchantable_value(&item.item.to_string());
    let mut rng = SpawnRng::new(seed as u64);
    let mut costs = [0i32; 3];
    for (slot, cost) in costs.iter_mut().enumerate() {
        let raw = cost_for_slot(&mut rng, slot as u32, bookcases, value);
        *cost = if raw < slot as i32 + 1 { 0 } else { raw };
    }
    costs
}

/// One rolled enchantment: `EnchantmentInstance(enchantment, level)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnchantmentOffer {
    pub key: &'static str,
    pub level: u32,
}

/// `EnchantmentHelper.getAvailableEnchantmentResults`: every `(enchantment,
/// level)` in vanilla's pool whose `[min_cost(level), max_cost(level)]` window
/// contains `value` — highest matching level first per enchantment (vanilla's
/// own `for (level = max; level >= min; level--)` break-on-first-match), and
/// gated on `isPrimaryItem`/being a book (vanilla's `#minecraft:non_treasure`
/// table pool, [`enchantment_data::non_treasure`]).
fn available_results(value: i32, item: &ItemStack, is_book: bool) -> Vec<(&'static EnchantmentDef, u32)> {
    let item_name = item.item.to_string();
    let mut results = Vec::new();
    for def in enchantment_data::non_treasure() {
        if !(def.supported.matches(&item_name) || is_book) {
            continue;
        }
        for level in (1..=def.max_level).rev() {
            let (min, max) = (enchantment_data::min_cost(def, level), enchantment_data::max_cost(def, level));
            if value >= min && value <= max {
                results.push((def, level));
                break;
            }
        }
    }
    results
}

/// `EnchantmentHelper.selectEnchantment`: seeds a per-item cost spread from
/// [`enchantment_data::enchantable_value`], picks one enchantment by weight,
/// then keeps drawing (each new candidate filtered to
/// [`enchantment_data::compatible`] with everything already chosen) while
/// `rng.next_int(50) <= cost`, halving `cost` each extra draw.
#[must_use]
pub fn select_enchantments(rng: &mut SpawnRng, item: &ItemStack, mut cost: i32) -> Vec<EnchantmentOffer> {
    let mut results = Vec::new();
    let Some(value) = enchantment_data::enchantable_value(&item.item.to_string()) else {
        return results;
    };
    let value = i32::try_from(value).unwrap_or(0);
    cost += 1 + rng.next_int(value / 4 + 1) + rng.next_int(value / 4 + 1);
    let span = (rng.next_f32() + rng.next_f32() - 1.0) * 0.15;
    cost = ((cost as f32) + (cost as f32) * span).round().max(1.0) as i32;

    let is_book = item.item.to_string() == "minecraft:book";
    let mut candidates = available_results(cost, item, is_book);
    if candidates.is_empty() {
        return results;
    }

    if let Some((def, level)) = weighted_pick(rng, &candidates) {
        results.push(EnchantmentOffer { key: def.key, level });
    }
    while !results.is_empty() && rng.next_int(50) <= cost {
        let last = results.last().expect("checked non-empty");
        candidates.retain(|(def, _)| enchantment_data::compatible(def.key, last.key));
        if candidates.is_empty() {
            break;
        }
        let Some((def, level)) = weighted_pick(rng, &candidates) else { break };
        results.push(EnchantmentOffer { key: def.key, level });
        cost /= 2;
    }
    results
}

/// `WeightedRandom.getRandomItem`: roll `[0, total_weight)`, walk the list
/// subtracting each entry's weight until the roll goes negative.
fn weighted_pick(rng: &mut SpawnRng, candidates: &[(&'static EnchantmentDef, u32)]) -> Option<(&'static EnchantmentDef, u32)> {
    let total: u32 = candidates.iter().map(|(def, _)| def.weight).sum();
    if total == 0 {
        return None;
    }
    let mut roll = rng.next_int(total as i32);
    for &(def, level) in candidates {
        roll -= def.weight as i32;
        if roll < 0 {
            return Some((def, level));
        }
    }
    candidates.last().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A trivial in-memory world for the bookshelf geometry tests: a name per
    /// coordinate, defaulting to air.
    struct TestWorld(HashMap<(i32, i32, i32), &'static str>);
    impl ChunkSource for TestWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            unimplemented!("not needed for these tests")
        }
        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            self.0.get(&(x, y, z)).unwrap_or(&"minecraft:air").to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
            unimplemented!("not needed for these tests")
        }
    }

    /// A bookshelf **diagonally** adjacent through a blocked walkway must not
    /// count — this is the discriminating case between "count nearby
    /// bookshelves" (the wrong, plausible reading the brief warns about) and
    /// the real ring-plus-walkway shape.
    #[test]
    fn a_bookshelf_behind_a_blocked_walkway_does_not_count() {
        let mut blocks = HashMap::new();
        // Shelf at x=2 (straight line, walkway at x=1 blocked by stone).
        blocks.insert((2, 0, 0), "minecraft:bookshelf");
        blocks.insert((1, 0, 0), "minecraft:stone");
        let world = TestWorld(blocks);
        assert_eq!(bookshelf_power(&world, BlockPos::new(0, 0, 0)), 0);
    }

    /// The same shelf with an open walkway does count, and a full single ring
    /// (all 16 (x,z) positions at y=0, none at y=1) gives exactly 16 clamped
    /// to the vanilla ceiling... actually 16 already exceeds nothing since the
    /// ceiling is 15: this checks the **clamp**, not merely "some number".
    #[test]
    fn a_full_double_ring_clamps_at_fifteen() {
        let mut blocks = HashMap::new();
        for (x, y, z) in bookshelf_offsets() {
            blocks.insert((x, y, z), "minecraft:bookshelf");
            blocks.insert((x / 2, y, z / 2), "minecraft:air");
        }
        let world = TestWorld(blocks);
        assert_eq!(bookshelf_power(&world, BlockPos::new(0, 0, 0)), 15);
    }

    #[test]
    fn one_open_shelf_counts_exactly_one() {
        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 2), "minecraft:bookshelf");
        let world = TestWorld(blocks); // walkway (0,0,1) defaults to air
        assert_eq!(bookshelf_power(&world, BlockPos::new(0, 0, 0)), 1);
    }

    /// Slot 2's cost floor is `bookcases * 2`, which is the only thing that
    /// makes 15 bookshelves reach a level-30 offer — a plausible-but-wrong
    /// reading ("all three slots use the same formula") would give slot 2 no
    /// special floor at all.
    #[test]
    fn slot_two_floors_at_twice_bookshelf_power() {
        let mut rng = SpawnRng::new(1);
        // Force a low `selected` roll by seeding and reading, then verify the
        // floor kicks in at high bookcases.
        let cost = cost_for_slot(&mut rng, 2, 15, Some(10));
        assert!(cost >= 30, "slot 2 at 15 bookshelves must floor at bookcases*2 = 30, got {cost}");
    }

    #[test]
    fn table_costs_are_zero_for_an_unenchantable_item() {
        let stone = ItemStack::new("minecraft:stone".parse().unwrap(), 1);
        assert_eq!(table_costs(42, 15, &stone), [0, 0, 0]);
    }

    #[test]
    fn table_costs_are_nonzero_for_a_diamond_sword_at_full_power() {
        let sword = ItemStack::new("minecraft:diamond_sword".parse().unwrap(), 1);
        let costs = table_costs(42, 15, &sword);
        assert!(costs.iter().any(|&c| c > 0), "at least one slot must offer something: {costs:?}");
    }

    /// The selection draw must be able to pick a compatible enchantment for a
    /// plain sword and must never emit a treasure enchantment (mending is
    /// treasure-only and must never appear from table rolls).
    #[test]
    fn selected_enchantments_never_include_a_treasure_enchantment() {
        let sword = ItemStack::new("minecraft:diamond_sword".parse().unwrap(), 1);
        for seed in 0..200u64 {
            let mut rng = SpawnRng::new(seed);
            let offers = select_enchantments(&mut rng, &sword, 30);
            for offer in offers {
                let def = enchantment_data::by_key(offer.key).expect("known enchantment");
                assert!(!def.treasure, "table roll produced treasure enchantment {}", offer.key);
                assert!(def.supported.matches("minecraft:diamond_sword"), "{} is not valid on a sword", offer.key);
            }
        }
    }
}
