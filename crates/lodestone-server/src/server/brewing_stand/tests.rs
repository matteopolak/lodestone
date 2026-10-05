//! Tests for brewing-stand insertion.

use super::*;
use crate::brewing::BrewingStand;
use crate::server::tests::stack;


// -- brewing stand interaction  --

/// A brewing stand registered at `pos`, and a player inventory whose
/// selected hotbar slot (0) holds `held`.
fn brew_scene(held: Option<ItemStack>) -> (BlockEntityHandle, PlayerInventory, BlockPos) {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(4, 64, 4);
    block_entities.with(|reg| reg.insert(pos, BlockEntity::BrewingStand(BrewingStand::new())));
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, held);
    (block_entities, inventory, pos)
}

/// The stand's ingredient slot as owned `(item, count)`, read back through
/// the registry.
fn ingredient_of(block_entities: &BlockEntityHandle, pos: BlockPos) -> Option<(String, u32)> {
    block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::BrewingStand(stand)) => {
            stand.ingredient().map(|(item, count)| (item.to_string(), count))
        }
        _ => None,
    })
}

/// A water bottle lands in bottle slot 0 and the single held item is fully
/// consumed — the basic insert that makes `set_bottle` reachable at all.
#[test]
fn right_click_puts_a_water_bottle_in_the_first_empty_bottle_slot_and_consumes_it() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:water_bottle", 1)));

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(
        outcome,
        BrewingInsertOutcome::Inserted(None),
        "a single bottle is fully consumed"
    );
    assert_eq!(inventory.native(0), None, "the selected slot is empty after the click");
    let bottle = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::BrewingStand(stand)) => stand.bottle(0).cloned(),
        _ => None,
    });
    assert_eq!(
        bottle,
        Bottle::from_potion_name(BottleKind::Potion, "minecraft:water"),
        "the water bottle must land in bottle slot 0"
    );
}

/// Blaze powder lands in the fuel slot and one of a multi-count stack is
/// consumed, leaving the remainder in hand.
#[test]
fn right_click_puts_blaze_powder_in_the_fuel_slot_and_consumes_one() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:blaze_powder", 3)));

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::Inserted(Some(stack("minecraft:blaze_powder", 2))));
    let fuel = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::BrewingStand(stand)) => {
            stand.fuel_item().map(|(item, count)| (item.to_string(), count))
        }
        _ => None,
    });
    assert_eq!(fuel, Some(("minecraft:blaze_powder".to_string(), 1)));
}

/// **Control**: blaze powder is *also* a potion ingredient (strength,
/// `brewing.rs`'s `potion_mix`), so this proves the fuel routing wins over
/// the ingredient routing — the item lands only in the fuel slot.
#[test]
fn blaze_powder_routes_to_fuel_not_ingredient_even_though_it_is_both() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:blaze_powder", 1)));

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::Inserted(None));
    let (fuel, ingredient) = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::BrewingStand(stand)) => {
            (stand.fuel_item().is_some(), stand.ingredient().is_some())
        }
        _ => (false, false),
    });
    assert!(fuel, "blaze powder must land in the fuel slot");
    assert!(!ingredient, "it must not also land in the ingredient slot");
}

/// An ingredient lands in the ingredient slot, and a second click with the
/// same item **merges** into the existing stack rather than starting a new
/// one — one consumed from the hand each time.
#[test]
fn right_click_puts_an_ingredient_in_the_ingredient_slot_and_a_second_click_merges() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:nether_wart", 2)));

    let first = insert_into_brewing_stand(&block_entities, &mut inventory, pos);
    assert_eq!(first, BrewingInsertOutcome::Inserted(Some(stack("minecraft:nether_wart", 1))));

    let second = insert_into_brewing_stand(&block_entities, &mut inventory, pos);
    assert_eq!(second, BrewingInsertOutcome::Inserted(None), "the second of two is fully consumed");

    assert_eq!(
        ingredient_of(&block_entities, pos),
        Some(("minecraft:nether_wart".to_string(), 2)),
        "both clicks must merge into one stack of two"
    );
}

/// A held item that belongs to no brewing-stand slot falls through without
/// consuming anything and without touching the stand — the caller's cue to
/// try ordinary placement.
#[test]
fn a_non_brewing_item_falls_through_without_touching_anything() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:diamond", 1)));

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::NotBrewing);
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:diamond", 1)),
        "the item must stay in hand"
    );
    let empty_stand = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::BrewingStand(stand)) => {
            stand.bottle(0).is_none() && stand.ingredient().is_none() && stand.fuel_item().is_none()
        }
        _ => false,
    });
    assert!(empty_stand, "nothing may land in the stand");
}

/// **Control**: a valid bottle with all three bottle slots full is consumed
/// without placing anything — the `Consumed` distinction exists because
/// some ingredients (`minecraft:stone`, `slime_block`, `cobweb`) are
/// themselves placeable blocks, and a full stand must never fall through to
/// placement and place one.
#[test]
fn a_valid_bottle_with_all_three_slots_full_is_consumed_without_placing() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:water_bottle", 1)));
    block_entities.with(|reg| {
        if let Some(BlockEntity::BrewingStand(stand)) = reg.get_mut(pos) {
            for slot in 0..3 {
                stand.set_bottle(
                    slot,
                    Bottle::from_potion_name(BottleKind::Potion, "minecraft:awkward"),
                );
            }
        }
    });

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::Consumed);
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:water_bottle", 1)),
        "nothing may be consumed from a full stand"
    );
}

/// A different ingredient already in the ingredient slot is not silently
/// overwritten — the click is consumed and the original stack survives.
#[test]
fn a_different_ingredient_does_not_overwrite_the_one_already_in_the_slot() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:redstone", 1)));
    block_entities.with(|reg| {
        if let Some(BlockEntity::BrewingStand(stand)) = reg.get_mut(pos) {
            stand.set_ingredient(Some(("minecraft:nether_wart".into(), 1)));
        }
    });

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::Consumed);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:redstone", 1)), "nothing may be consumed");
    assert_eq!(
        ingredient_of(&block_entities, pos),
        Some(("minecraft:nether_wart".to_string(), 1)),
        "the original ingredient is untouched"
    );
}

/// **Control**: a `minecraft:potion`/`splash_potion`/`lingering_potion`
/// stack's actual potion lives in an unmodeled `potion_contents` component
/// (see [`bottle_from_item`]'s doc comment), so it is rejected rather than
/// guessed — inserting it with a wrong potion would let the mix table brew
/// the wrong thing from it.
#[test]
fn an_unmodelable_potion_item_is_rejected_not_guessed() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:potion", 1)));

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::NotBrewing);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:potion", 1)), "the potion must stay in hand");
    let no_bottle = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::BrewingStand(stand)) => stand.bottle(0).is_none(),
        _ => false,
    });
    assert!(no_bottle, "no bottle may be fabricated from an unmodelable potion");
}

/// **Control**: an ingredient stack already at the 64 cap is not grown past
/// it — the click is consumed without moving anything.
#[test]
fn an_ingredient_stack_at_the_cap_is_not_grown_further() {
    let (block_entities, mut inventory, pos) = brew_scene(Some(stack("minecraft:nether_wart", 1)));
    block_entities.with(|reg| {
        if let Some(BlockEntity::BrewingStand(stand)) = reg.get_mut(pos) {
            stand.set_ingredient(Some(("minecraft:nether_wart".into(), BREWING_STACK_CAP)));
        }
    });

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, pos);

    assert_eq!(outcome, BrewingInsertOutcome::Consumed);
    assert_eq!(
        inventory.native(0),
        Some(&stack("minecraft:nether_wart", 1)),
        "nothing may be consumed when the slot is full"
    );
    assert_eq!(
        ingredient_of(&block_entities, pos),
        Some(("minecraft:nether_wart".to_string(), BREWING_STACK_CAP)),
        "the full stack is untouched"
    );
}

/// A position holding no brewing stand is not a brewing interaction at all,
/// regardless of the held item.
#[test]
fn a_position_without_a_brewing_stand_is_not_a_brewing_interaction() {
    let block_entities = BlockEntityHandle::new();
    let mut inventory = PlayerInventory::new();
    inventory.set_native(0, Some(stack("minecraft:nether_wart", 1)));

    let outcome = insert_into_brewing_stand(&block_entities, &mut inventory, BlockPos::new(9, 9, 9));

    assert_eq!(outcome, BrewingInsertOutcome::NotBrewing);
    assert_eq!(inventory.native(0), Some(&stack("minecraft:nether_wart", 1)));
}
