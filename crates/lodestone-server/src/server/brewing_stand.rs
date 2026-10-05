//! Hand interaction with a brewing stand: which slot an item belongs in and how it is inserted.

use super::*;

/// Which of a brewing stand's five slots a held item routes to — decided by
/// item identity alone: slots 0-2 take potions/bottles, slot 3 takes any
/// registered brewing ingredient, and slot 4 takes the brewing-fuel item tag.
pub(super) enum BrewingSlot {
    /// Blaze powder — the brewing-fuel item tag's sole member (slot 4).
    Fuel,
    /// A bottle this crate can represent — a water bottle, whose potion the
    /// item id fully determines. See [`bottle_from_item`] for why the other
    /// three bottle-shaped items are *not* insertable (slots 0-2).
    Bottle(Bottle),
    /// Any [`is_ingredient`] item (slot 3).
    Ingredient,
}

/// The outcome of one right-click on a brewing stand — this crate's
/// one-item-per-click stand-in for the brewing menu it cannot open (see
/// [`BlockEntity::menu_name`]'s doc comment for why a brewing stand answers
/// `None` there). The outcome distinguishes insertion, a consumed full-slot
/// click, and ordinary placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BrewingInsertOutcome {
    /// An item moved out of the player's hand into the stand; the player's
    /// selected hotbar slot now holds `selected` (`None` when the last of a
    /// stack was consumed).
    Inserted(Option<ItemStack>),
    /// The right-click was consumed by the stand but nothing moved — a valid
    /// brewing item whose matching slot was already full (or held a different
    /// stack). No placement may follow: this stands in for the menu that
    /// would have consumed the click in vanilla. Distinct from `NotBrewing`
    /// because some potion ingredients (`minecraft:stone`, `slime_block`,
    /// `cobweb`) are themselves placeable blocks, and a full brewing stand
    /// must never silently place one.
    Consumed,
    /// The held item belongs to no brewing-stand slot — the caller falls
    /// through to ordinary placement when no brewing slot accepts it.
    NotBrewing,
}

/// The one bottle-shaped item this crate can put in a brewing-stand bottle
/// slot: a water bottle, whose potion is fully determined by its item id.
///
/// A `minecraft:potion`/`splash_potion`/`lingering_potion` stack carries its
/// actual potion in the `minecraft:potion_contents` data component, which
/// `lodestone_model::ItemComponents` does not model (see `brewing.rs`'s
/// module doc: "no potion-contents component anywhere in `ItemComponents`"),
/// so its contents are unknowable here. Inserting one with a guessed potion
/// would let the mix table brew a *wrong* potion from it, so it is rejected
/// rather than guessed — the same declared gap the `Bottle` type itself is.
#[must_use]
pub(super) fn bottle_from_item(item: &str) -> Option<Bottle> {
    match item {
        "minecraft:water_bottle" => Bottle::from_potion_name(BottleKind::Potion, "minecraft:water"),
        _ => None,
    }
}

/// Routes `item` to the brewing-stand slot it belongs in, or `None` if it
/// belongs nowhere — mirroring vanilla's own brewing-stand-block-entity
/// can-place-item check. Blaze powder is checked first even though it is *also* a
/// potion ingredient (strength, `brewing.rs`'s `potion_mix`): the fuel slot
/// wins, matching the slot-4 test vanilla applies.
#[must_use]
pub(super) fn brewing_slot_for(item: &str) -> Option<BrewingSlot> {
    if item == "minecraft:blaze_powder" {
        return Some(BrewingSlot::Fuel);
    }
    if let Some(bottle) = bottle_from_item(item) {
        return Some(BrewingSlot::Bottle(bottle));
    }
    if is_ingredient(item) {
        return Some(BrewingSlot::Ingredient);
    }
    None
}

/// One ingredient/fuel stack's cap before a further right-click is refused —
/// vanilla's default `MAX_STACK_SIZE` (64), the same number `furnace.rs`'s
/// [`MAX_STACK_SIZE`](crate::furnace::MAX_STACK_SIZE) records for output stacks.
pub(super) const BREWING_STACK_CAP: u32 = 64;

/// The window-0 menu slot of the hotbar's first (native) slot — vanilla's
/// `InventoryMenu`: hotbar menu slots `36..=44` address native hotbar `0..=8`
/// (see `crate::inventory::PlayerInventory`'s own doc table). The window-0
/// `container_set_slot` [`apply_use_item_on`] sends after a brewing insert
/// addresses the selected hotbar slot by this menu index, not its native one.
pub(super) const WINDOW_ZERO_HOTBAR_FIRST: i32 = 36;

/// Attempts to insert the player's held item into the brewing stand at `pos`,
/// consuming one from the selected hotbar stack when it lands — the wiring
/// that makes `BrewingStand::set_bottle`/`set_ingredient`/`set_fuel_item` (and
/// therefore the whole brew state machine) reachable from a player at all.
/// See [`BrewingInsertOutcome`] for the three outcomes.
///
/// Merging follows the slot's own shape: fuel and ingredient stacks merge into
/// an existing matching stack up to [`BREWING_STACK_CAP`], while a bottle only
/// ever occupies an empty slot (vanilla's `canPlaceItem` empty-slot test,
/// `:225`, and bottles do not stack).
pub(super) fn insert_into_brewing_stand(
    block_entities: &BlockEntityHandle,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
) -> BrewingInsertOutcome {
    // The registry lookup happens first, so a right-click on any other block
    // is untouched by this branch entirely.
    let is_stand = block_entities.with(|reg| matches!(reg.get(pos), Some(BlockEntity::BrewingStand(_))));
    if !is_stand {
        return BrewingInsertOutcome::NotBrewing;
    }
    let Some(held) = inventory.selected_item().cloned() else {
        return BrewingInsertOutcome::NotBrewing;
    };
    let item = held.item.to_string();
    let Some(slot) = brewing_slot_for(&item) else {
        return BrewingInsertOutcome::NotBrewing;
    };

    // The slot write happens inside the registry lock — nothing else can see
    // a half-inserted item, and the write is validated against the live slot
    // contents in the same critical section.
    let moved = block_entities.with(|reg| {
        let Some(entity) = reg.get_mut(pos) else {
            return false;
        };
        let BlockEntity::BrewingStand(stand) = entity else {
            return false;
        };
        match slot {
            BrewingSlot::Fuel => match stand.fuel_item() {
                Some(("minecraft:blaze_powder", count)) if count < BREWING_STACK_CAP => {
                    stand.set_fuel_item(Some(("minecraft:blaze_powder".into(), count + 1)));
                    true
                }
                None => {
                    stand.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
                    true
                }
                _ => false,
            },
            BrewingSlot::Bottle(bottle) => {
                for raw in 0..lodestone_model::BrewingBottleSlot::COUNT {
                    let index = lodestone_model::BrewingBottleSlot::new(raw)
                        .expect("bounded bottle loop");
                    if stand.bottle_at(index).is_none() {
                        stand.set_bottle_at(index, Some(bottle));
                        return true;
                    }
                }
                false
            }
            BrewingSlot::Ingredient => match stand.ingredient() {
                Some((existing, count)) if existing == item.as_str() && count < BREWING_STACK_CAP => {
                    stand.set_ingredient(Some((item.clone(), count + 1)));
                    true
                }
                None => {
                    stand.set_ingredient(Some((item.clone(), 1)));
                    true
                }
                _ => false,
            },
        }
    });
    if !moved {
        return BrewingInsertOutcome::Consumed;
    }

    // Consume one item from the held stack.
    let native = usize::from(inventory.selected_hotbar_slot());
    let remainder = match inventory.native(native).cloned() {
        Some(mut stack) => {
            stack.count -= 1;
            if stack.count == 0 {
                None
            } else {
                Some(stack)
            }
        }
        None => None,
    };
    inventory.set_native(native, remainder.clone());
    BrewingInsertOutcome::Inserted(remainder)
}

#[cfg(test)]
mod tests;
