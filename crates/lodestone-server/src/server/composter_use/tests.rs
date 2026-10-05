//! Tests for composter interaction.

use super::*;
use crate::composter::{Composter, MAX_FILL_LEVEL, READY_DELAY_TICKS};
use lodestone_model::Vec3;
use crate::server::tests::{fixture_state, stack};

// -- the composter interaction  --

/// A composter at `pos`, and a player inventory whose selected hotbar slot
/// (0) holds `held`. `MobHandle::default()` is an empty sim, so the first
/// `spawn_item` in a test is entity id 1 (its `next_id` starts at 1 — see
/// `MobSim::new`).
fn composter_scene(
    composter: Composter,
    held: Option<ItemStack>,
) -> (BlockEntityHandle, PlayerInventory, BlockPos, MobHandle) {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(4, 64, 4);
    block_entities.with(|reg| reg.insert(pos, BlockEntity::Composter(composter)));
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, held);
    (block_entities, inventory, pos, MobHandle::default())
}

/// The composter's fill level, read back through the registry.
fn composter_level(block_entities: &BlockEntityHandle, pos: BlockPos) -> u8 {
    block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Composter(composter)) => composter.level(),
        _ => u8::MAX,
    })
}

/// A right-click with a compostable item consumes one from the hand and
/// raises the fill level — the wiring that makes `Composter::insert`
/// reachable at all.
#[test]
fn right_click_consumes_one_compostable_and_raises_the_level() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::new(), Some(stack("minecraft:oak_leaves", 3)));

    // oak_leaves chance is 0.3; roll 0.0 always beats it (and level 0
    // always advances regardless of roll — the documented special case).
    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Consumed {
            remainder: Some(stack("minecraft:oak_leaves", 2)),
            block_state: Some(fixture_state("minecraft:composter[level=1]")),
        }
    );
    assert_eq!(composter_level(&block_entities, pos), 1);
}

/// A single compostable item in hand is fully consumed, emptying the slot.
#[test]
fn right_click_fully_consumes_a_single_item() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::new(), Some(stack("minecraft:wheat", 1)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Consumed {
            remainder: None,
            block_state: Some(fixture_state("minecraft:composter[level=1]")),
        }
    );
    assert_eq!(inventory.native(0), None, "the selected slot is empty after the click");
}

/// **Control**: a failed roll still consumes the item (vanilla consumes on
/// every accepted insert, per its own composter fill routine) but leaves the level —
/// and therefore the block state — unchanged.
#[test]
fn a_failed_roll_still_consumes_the_item_but_keeps_the_state() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::restore(1, None), Some(stack("minecraft:oak_leaves", 2)));

    // oak_leaves chance is 0.3; a roll of 0.9 fails away from level 0.
    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.9);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Consumed {
            remainder: Some(stack("minecraft:oak_leaves", 1)),
            block_state: None,
        }
    );
    assert_eq!(composter_level(&block_entities, pos), 1);
}

/// A non-compostable held item falls through without consuming anything or
/// touching the composter — the caller's cue to try ordinary placement
/// so the ordinary placement path can handle it.
#[test]
fn a_non_compostable_item_falls_through_without_touching_anything() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::new(), Some(stack("minecraft:diamond", 1)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(outcome, ComposterUseOutcome::NotComposter);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:diamond", 1)));
    assert_eq!(composter_level(&block_entities, pos), 0);
}

/// An empty hand on a composter below level 8 returns `PASS`, so the
/// placement logic may place a block on top of the partially filled
/// composter.
#[test]
fn an_empty_hand_on_a_not_ready_composter_falls_through() {
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(Composter::restore(3, None), None);

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(outcome, ComposterUseOutcome::NotComposter);
    assert_eq!(composter_level(&block_entities, pos), 3);
}

/// A full (level 7, waiting on its scheduled tick) composter consumes the
/// click without touching the hand at `fillLevel == 7` with nothing to add.
#[test]
fn level_seven_consumes_the_click_without_touching_the_hand() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        assert!(matches!(
            composter.insert("minecraft:cake", 0.0),
            InsertOutcome::Consumed {
                level_increased: true
            }
        ));
    }
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(composter, Some(stack("minecraft:cake", 2)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(outcome, ComposterUseOutcome::Noop);
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:cake", 2)),
        "the hand must be untouched"
    );
    assert_eq!(composter_level(&block_entities, pos), MAX_FILL_LEVEL);
}

/// A ready composter (level 8) with an empty hand yields one bone-meal item
/// entity just above the block and resets to level 0 — the extraction half
/// of the interaction (`extractProduce`).
#[test]
fn extracting_a_ready_composter_spawns_bone_meal_and_resets() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        composter.insert("minecraft:cake", 0.0);
    }
    for _ in 0..READY_DELAY_TICKS {
        composter.tick();
    }
    assert!(composter.is_ready());
    let (block_entities, mut inventory, pos, mobs) = composter_scene(composter, None);
    let bone_meal_id = mobs.with(|sim| sim.next_id());

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Extracted {
            block_state: fixture_state("minecraft:composter[level=0]"),
        }
    );
    assert_eq!(composter_level(&block_entities, pos), 0);
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "exactly one bone-meal item entity must spawn"
    );
    // The spawn takes the sim's next id, and it must land at the block's
    // centre with the measured `1.01`-block vertical offset.
    assert_eq!(
        mobs.with(|sim| sim.item_position(bone_meal_id)),
        Some(Vec3::new(4.5, 65.01, 4.5)),
        "the bone meal must spawn just above the composter"
    );
}

/// **Control**: extraction reaches the player even with a compostable item
/// in hand — the item offer fails below level 8 (returns `NotAccepting`)
/// and the hand-use half extracts without consuming the hand.
#[test]
fn extracting_a_ready_composter_works_even_with_an_item_in_hand() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        composter.insert("minecraft:cake", 0.0);
    }
    for _ in 0..READY_DELAY_TICKS {
        composter.tick();
    }
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(composter, Some(stack("minecraft:cake", 2)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Extracted {
            block_state: fixture_state("minecraft:composter[level=0]"),
        }
    );
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:cake", 2)),
        "extraction must not consume the hand"
    );
    assert_eq!(mobs.with(|sim| sim.item_count()), 1);
}

/// **Control**: a non-compostable item on a *ready* composter also extracts
/// — the item offer fails the compostability check and the hand-use half
/// runs without consuming the hand.
#[test]
fn extracting_a_ready_composter_works_for_a_non_compostable_item_too() {
    let mut composter = Composter::new();
    for _ in 0..MAX_FILL_LEVEL {
        composter.insert("minecraft:cake", 0.0);
    }
    for _ in 0..READY_DELAY_TICKS {
        composter.tick();
    }
    let (block_entities, mut inventory, pos, mobs) =
        composter_scene(composter, Some(stack("minecraft:diamond", 1)));

    let outcome = apply_composter_use(&block_entities, &mut inventory, &mobs, pos, 0.0);

    assert_eq!(
        outcome,
        ComposterUseOutcome::Extracted {
            block_state: fixture_state("minecraft:composter[level=0]"),
        }
    );
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:diamond", 1)),
        "the non-compostable item must stay in hand"
    );
    assert_eq!(mobs.with(|sim| sim.item_count()), 1);
}

/// A position holding no composter is not a composter interaction at all,
/// regardless of the held item.
#[test]
fn a_position_without_a_composter_is_not_a_composter_interaction() {
    let block_entities = BlockEntityHandle::new();
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, Some(stack("minecraft:oak_leaves", 1)));

    let outcome = apply_composter_use(
        &block_entities,
        &mut inventory,
        &MobHandle::default(),
        BlockPos::new(9, 9, 9),
        0.0,
    );

    assert_eq!(outcome, ComposterUseOutcome::NotComposter);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:oak_leaves", 1)));
}
