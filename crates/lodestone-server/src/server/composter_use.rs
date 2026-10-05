//! Hand interaction with a composter: filling, harvesting and the behaviour seeds shared with bone meal and burning.

use super::*;

/// The seed for the per-connection [`SpawnRng`] that draws a composter's
/// insert roll — the same explicit-seed shape `tick::RANDOM_TICK_BEHAVIOR_SEED`
/// uses for crop growth, and for the same reason: this crate takes seeds
/// explicitly rather than drawing them, so a test can replay an exact
/// outcome. Per-*connection*, not per-*level*: two players feeding the same
/// composter draw from different streams, which only changes which roll a
/// given insert sees — the shared fill state lives in the registry regardless.
pub(super) const COMPOSTER_BEHAVIOR_SEED: u64 = 0x5EED_C011;

/// The seed for the per-connection [`SpawnRng`] that draws bone meal's crop-age
/// and sapling-success values — its own stream rather than the composter's, for
/// the reason that constant's own comment gives about coupling two features
/// through one RNG. `crate::bone_meal`'s draw-count gates hold only against a
/// stream nothing else advances.
pub(super) const BONE_MEAL_BEHAVIOR_SEED: u64 = 0x5EED_B04E;

/// The seed for the per-connection [`SpawnRng`] that draws
/// the base fire-block's `1..=3` player ramp. Its own stream serves the same
/// isolation purpose as the two constants above.
pub(super) const BURN_BEHAVIOR_SEED: u64 = 0x5EED_F14E;

/// What a right-click on a composter did, so [`apply_use_item_on`] can decide
/// whether the ordinary placement logic may still run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposterUseOutcome {
    /// `pos` held no composter block entity, or the click left both the
    /// composter and the player's hand untouched. This covers a
    /// non-compostable held item and an empty hand on a composter below the
    /// ready level. The block's item-use result is `PASS`, so the placement
    /// logic below must run.
    NotComposter,
    /// The composter consumed the click but nothing moved — level `7`,
    /// waiting on its scheduled tick; the hand is untouched. No
    /// placement may follow.
    Noop,
    /// One item was consumed from the player's hand. The updated hand contents are sent through
    /// the caller to push as a window-0 slot update; `block_state` is the new
    /// block state to write — `Some` when the fill level advanced, `None` on a
    /// failed roll (the item is still consumed; only the state is unchanged).
    Consumed {
        remainder: Option<ItemStack>,
        block_state: Option<StateId>,
    },
    /// Bone meal was extracted (level `8` -> `0`, vanilla's own `extractProduce`)
    /// — the caller spawns the item entity and
    /// writes `block_state`.
    Extracted { block_state: StateId },
}

/// The decision `apply_composter_use`'s registry-locked step makes; the caller
/// then applies the world side effects (inventory shrink, bone-meal spawn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ComposterStep {
    /// Not a composter, or the click leaves everything untouched — the
    /// placement logic below may run.
    FallThrough,
    /// Click consumed, nothing changed (level 7 waiting).
    Noop,
    /// One item consumed; `block_state` is `Some` when the fill level advanced.
    Consumed { block_state: Option<StateId> },
    /// Level 8 reached — extract bone meal, reset to 0.
    Extract,
}

/// The block-state string for a composter at fill `level` — vanilla's
/// `minecraft:composter[level=0..8]`, the string the client's
/// `resolve_state_id` maps to the composter's per-level ids (block 229 in
/// `crates/lodestone-data/src/generated/block_states.rs`).
pub(super) fn composter_state(level: u8) -> StateId {
    let value = match level {
        0 => BuiltinPropertyValue::Value0,
        1 => BuiltinPropertyValue::Value1,
        2 => BuiltinPropertyValue::Value2,
        3 => BuiltinPropertyValue::Value3,
        4 => BuiltinPropertyValue::Value4,
        5 => BuiltinPropertyValue::Value5,
        6 => BuiltinPropertyValue::Value6,
        7 => BuiltinPropertyValue::Value7,
        8 => BuiltinPropertyValue::Value8,
        _ => unreachable!("composter level is bounded to 0..=8"),
    };
    let properties = Properties::try_from_pairs(&[(
        PropertyKey::Level,
        PropertyValue::builtin(value),
    )])
    .expect("generated composter properties");
    Properties::state_for_block(Block::Composter, &properties).expect("generated composter state")
}

/// Applies one right-click on the composter at `pos` — the wiring that makes
/// `Composter::insert`/`extract`, keeping the seven-tier fill state machine
/// reachable from a player.
///
/// Mirrors the composter interaction order: the held item (if any) is
/// offered to the fill machine first, and an unconsumed click enters the
/// hand-use branch, which extracts at level `8` and otherwise returns `PASS`.
/// Concretely:
///
/// * an empty hand on a ready (level `8`) composter extracts the bone meal;
/// * a compostable item is rolled against its chance, consuming one from the
///   hand either way (a failed roll consumes the item);
/// * a compostable item at level `7` (waiting on its scheduled tick) is
///   consumed as a click but changes nothing;
/// * a *non*-compostable item never reaches `insert`'s level gate, because at
///   level `7` the compostable-item check fails before the fill-level add, so
///   the click falls through to placement while
///   the same item at level `8` extracts. Checking the chance table up front
///   reproduces that ordering (`insert` alone would answer `NotAccepting` for
///   both a compostable and a non-compostable item at level 7, and nothing
///   could tell them apart).
///
/// `roll` is an injected `[0.0, 1.0)` sample the caller draws once per
/// interaction — the "caller supplies the randomness" shape
/// [`Composter::insert`] documents, so a test can pin an exact outcome.
///
/// The inventory shrink and the bone-meal spawn are **this** function's job,
/// not the block-state writer's: item consumption lives with the caller (the
/// composter never holds the inserted item — see `composter.rs`'s module
/// doc), and `spawn_item` gives the extraction its world-facing item entity
/// (which `MobSim::snapshots` streams to clients). The caller writes the
/// returned `block_state` (if any) and pushes the window-0 slot update.
pub(super) fn apply_composter_use(
    block_entities: &BlockEntityHandle,
    inventory: &mut PlayerInventory,
    mobs: &MobHandle,
    pos: BlockPos,
    roll: f64,
) -> ComposterUseOutcome {
    // The registry lookup happens first, so a right-click on any other block
    // is untouched by this branch entirely.
    let held = inventory.selected_item().cloned();
    let step = block_entities.with(|reg| {
        let Some(BlockEntity::Composter(composter)) = reg.get_mut(pos) else {
            return ComposterStep::FallThrough;
        };
        let Some(held) = held else {
            // Empty hand: run the composter's hand-use branch.
            if composter.extract() {
                return ComposterStep::Extract;
            }
            return ComposterStep::FallThrough;
        };
        let item = held.item.to_string();
        // Non-compostable items enter the hand-use branch (see the doc comment
        // above for why this must be checked before `insert`, not by it).
        if compostable_chance(&item).is_none() {
            if composter.extract() {
                return ComposterStep::Extract;
            }
            return ComposterStep::FallThrough;
        }
        match composter.insert(&item, roll) {
            InsertOutcome::Consumed { level_increased } => {
                ComposterStep::Consumed {
                    block_state: level_increased.then(|| composter_state(composter.level())),
                }
            }
            InsertOutcome::NotAccepting => {
                // Level 7 (waiting, compostable): the interaction returns
                // SUCCESS with the hand untouched. Level 8 (ready): the item
                // offer failed below level 8, so the hand-use half extracts
                // instead.
                if composter.extract() {
                    ComposterStep::Extract
                } else {
                    ComposterStep::Noop
                }
            }
            InsertOutcome::NotCompostable => unreachable!(
                "compostable_chance() is the same table insert() consults; \
                 the up-front guard above rules this out"
            ),
        }
    });
    match step {
        ComposterStep::FallThrough => ComposterUseOutcome::NotComposter,
        ComposterStep::Noop => ComposterUseOutcome::Noop,
        ComposterStep::Consumed { block_state } => {
            // Consume one item from the selected hotbar stack, the same shrink
            // `insert_into_brewing_stand` performs for its own consumed insert.
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
            ComposterUseOutcome::Consumed {
                remainder,
                block_state,
            }
        }
        ComposterStep::Extract => {
            // Extract exactly one bone meal at the block's top, with the hand
            // untouched. The standard horizontal jitter is skipped because
            // this crate has no gaussian f64 source; a gentle upward toss is
            // enough to leave the block.
            mobs.with(|sim| {
                sim.spawn_item(
                    "minecraft:bone_meal".parse::<lodestone_model::ResourceKey>().expect("bone_meal is a valid item id"),
                    Vec3::new(
                        pos.x as f64 + 0.5,
                        pos.y as f64 + 1.01,
                        pos.z as f64 + 0.5,
                    ),
                    Vec3::new(0.0, 0.2, 0.0),
                    ItemLifecycle::newly_dropped(1, 64),
                );
            });
            ComposterUseOutcome::Extracted {
                block_state: composter_state(0),
            }
        }
    }
}

#[cfg(test)]
mod tests;
