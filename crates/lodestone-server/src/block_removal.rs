//! What happens to the stuff in a cell when its block goes.
//!
//! # What it is
//!
//! The one destroy path every block removal shares: the cell's loot, the
//! contents of whatever it stored, the block entity itself and its side
//! effects (a jukebox's song stopping, a hive's bees leaving when the cause
//! releases them). A player's dig,
//! an explosion, a piston, a fluid, a lost support and `/setblock destroy` all
//! end here, so a chest blown up by TNT spills as surely as one mined.
//!
//! # How it works
//!
//! [`destroy`] (or [`settle`], for a caller that has already rolled the loot)
//! takes the entity out of the registry through
//! [`BlockEntityRegistry::destroy_at`](crate::block_entities::BlockEntityRegistry::destroy_at),
//! the only function that can, so a removal that skips this module cannot
//! leave a stale entity behind either. What drops depends on the [`Cause`]:
//!
//! * the loot table rolls with the cause's context: the mining tool for
//!   [`Cause::Mined`], the blast radius for [`Cause::Blast`] (which is what
//!   makes `survives_explosion` thin the crater), nothing for the rest;
//! * contents spill whatever the cause, and whatever the drop rule says;
//! * the item a block carries its contents on (a shulker box, a decorated pot)
//!   follows the drop rule like any other loot;
//! * [`Cause::Cleared`] (a command replacing a block) drops nothing at all.
//!
//! # How to change it
//!
//! A new removal site calls [`destroy`] with its cause. A new kind of
//! container teaches `BlockEntity::spilled_contents` or
//! `stored_items::broken_drops` its slots. The test module scans the sources
//! for a second caller of the registry hook.
//!
//! # Dependencies
//!
//! [`crate::block_entities`] (the registry), [`crate::block_drops`] (loot
//! tables and the pop placement), [`crate::stored_items`], and the mob sim to
//! spawn the items.

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack};
use uuid::Uuid;

use crate::block_drops::PoppedItem;
use crate::block_entities::{BlockEntity, BlockEntityHandle, Removal};
use crate::commands::BlockEditMode;
use crate::mob_spawn::SpawnRng;
use crate::mobs::MobHandle;
use crate::tick::BlockTickFeed;

/// Why a block is going, which decides what its loot table is rolled with.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Cause<'a> {
    /// `breaker` dug it with `held`. `drops` is false in creative or with the
    /// block-drop rule off; `creative` is whether `breaker` was in creative.
    Mined { held: Option<&'a ItemStack>, drops: bool, creative: bool, breaker: Uuid },
    /// An explosion of this radius destroyed it. `empties_hives` is whether
    /// its source (primed TNT, a creeper, a TNT minecart) lets a destroyed
    /// hive's bees out.
    Blast { radius: f32, drops: bool, empties_hives: bool },
    /// A piston, a fluid, a lost support or `/setblock destroy`.
    Natural { drops: bool },
    /// A command replaced it: the entity goes and nothing drops.
    Cleared,
}

/// The shared handles a removal needs.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    pub(crate) block_entities: &'a BlockEntityHandle,
    pub(crate) mobs: &'a MobHandle,
    pub(crate) feed: &'a BlockTickFeed,
    /// The entity the world's terrain holds at a cell. Generated containers
    /// live in the column until something opens them, so a block that was never
    /// touched has no registry entry yet still spills.
    pub(crate) stored: &'a (dyn Fn(BlockPos) -> Option<BlockEntity> + Sync),
}

/// Rolls the cell's loot for `cause`, takes its entity out and drops
/// everything. The caller has already written the cell.
pub(crate) fn destroy(env: Env<'_>, pos: BlockPos, was: StateId, cause: Cause<'_>, rng: &mut SpawnRng) {
    let loot = loot(cause, was, pos, rng);
    settle(env, pos, was, cause, loot, rng);
}

/// Takes the entity out and drops `loot` plus the cell's contents. For a
/// caller that rolled the loot itself.
pub(crate) fn settle(env: Env<'_>, pos: BlockPos, was: StateId, cause: Cause<'_>, loot: Vec<PoppedItem>, rng: &mut SpawnRng) {
    let removal = env.block_entities.with(|registry| registry.destroy_at(pos, was));
    dispose(env, removal, cause, loot, rng);
}

/// Drops everything an already-taken removal holds, plus `loot`.
fn dispose(env: Env<'_>, removal: Removal, cause: Cause<'_>, loot: Vec<PoppedItem>, rng: &mut SpawnRng) {
    if matches!(cause, Cause::Cleared) {
        return;
    }
    let Removal { pos, was, entity } = removal;
    let entity = entity.or_else(|| {
        lodestone_data::block_entity_types::block_entity_type(was).and_then(|_| (env.stored)(pos))
    });
    let item_drops = match cause {
        Cause::Mined { drops, .. } | Cause::Blast { drops, .. } | Cause::Natural { drops } => drops,
        Cause::Cleared => false,
    };
    if was.block() == Block::Jukebox
        && let Some(BlockEntity::Opaque { nbt, .. }) = &entity
        && crate::stored_items::jukebox_disc(nbt).is_some()
    {
        env.feed.publish_effect(crate::effects::WorldEffect::LevelEvent {
            event: crate::stored_items::JUKEBOX_STOP_EVENT,
            pos,
            data: 0,
            global: false,
        });
    }
    let held = crate::stored_items::broken_drops(was, entity.as_ref(), item_drops);
    let popped: Vec<PoppedItem> = loot
        .into_iter()
        .chain(held.into_iter().map(|stack| {
            let (position, velocity) = crate::block_drops::pop_resource_placement(pos, rng);
            PoppedItem { stack, position, velocity }
        }))
        .collect();
    spawn(env.mobs, popped);
    // A destroyed hive lets its bees out at its own cell and turns the bees
    // about it on the players there. A player's dig does it unless the tool
    // keeps bees in (silk touch) or the player is in creative; the blasts of
    // primed TNT, a creeper and a TNT minecart do it; every other cause just
    // loses the bees. Any blast angers the bees nearby.
    if matches!(was.block(), Block::Beehive | Block::BeeNest) {
        let (release, breaker) = match cause {
            Cause::Mined { held, creative, breaker, .. } => (!creative && !crate::stored_items::has_silk_touch(held), Some(breaker)),
            Cause::Blast { empties_hives, .. } => (empties_hives, None),
            Cause::Natural { .. } | Cause::Cleared => (false, None),
        };
        let anger = (release && breaker.is_some()) || matches!(cause, Cause::Blast { .. });
        let mut released = Vec::new();
        if release && let Some(BlockEntity::Beehive(mut hive)) = entity {
            let flower = hive.flower();
            let facing = crate::mobs::bees::hive_facing(was);
            let bees = hive.release_all();
            released = bees.iter().filter_map(|bee| crate::entity_record::SavedEntity::from_nbt(&bee.occupant.entity_data)).map(|saved| saved.uuid).collect();
            env.mobs.with(|sim| {
                for bee in &bees {
                    sim.release_bee(pos, facing, true, bee, flower, false);
                }
            });
        }
        if anger {
            env.mobs.with(|sim| sim.anger_bees_after_hive_break(pos, &released, breaker, rng));
        }
    }
}

/// Whether `/setblock` or `/fill` in `mode` writes over `old`: `keep` writes
/// only into air.
#[must_use]
pub(crate) fn edit_writes(old: StateId, mode: BlockEditMode) -> bool {
    mode != BlockEditMode::Keep || crate::random_tick::is_air_variant_id(old)
}

/// What a command's write over `old` does to the block it replaced, after the
/// caller has written `new`. `destroy` breaks it like any other destruction;
/// `replace` only clears the entity, and only when the block itself changed.
pub(crate) fn finish_edit(
    env: Env<'_>,
    pos: BlockPos,
    old: StateId,
    new: StateId,
    mode: BlockEditMode,
    drops: bool,
    rng: &mut SpawnRng,
) {
    if crate::chunk::is_air_or_fluid_id(old) {
        return;
    }
    match mode {
        BlockEditMode::Destroy => destroy(env, pos, old, Cause::Natural { drops }, rng),
        BlockEditMode::Replace | BlockEditMode::Keep if old.block() != new.block() => {
            destroy(env, pos, old, Cause::Cleared, rng);
        }
        BlockEditMode::Replace | BlockEditMode::Keep => {}
    }
}

/// One `/setblock` or `/fill` cell: writes `new` over whatever is there per
/// `mode` and runs the removal of what it replaced. Returns whether the cell
/// was written, for the caller to publish.
pub(crate) fn apply_edit<S: crate::chunk::ChunkSource + ?Sized>(
    env: Env<'_>,
    source: &S,
    pos: BlockPos,
    new: StateId,
    mode: BlockEditMode,
    drops: bool,
    rng: &mut SpawnRng,
) -> bool {
    let old = source.block_state_id(pos.x, pos.y, pos.z);
    if !edit_writes(old, mode) {
        return false;
    }
    source.set_block(pos.x, pos.y, pos.z, new);
    finish_edit(env, pos, old, new, mode, drops, rng);
    true
}

/// Drops what the removals deferred by the reaction code left behind, each as
/// a [`Cause::Natural`] removal. `drops` is the world's block-drop rule.
pub(crate) fn drain_deferred(env: Env<'_>, drops: bool, rng: &mut SpawnRng) {
    let deferred = env.block_entities.with(|registry| registry.take_deferred());
    for removal in deferred {
        let cause = Cause::Natural { drops };
        let loot = loot(cause, removal.was, removal.pos, rng);
        dispose(env, removal, cause, loot, rng);
    }
}

/// The loot table's drops for `cause`.
#[must_use]
pub(crate) fn loot(cause: Cause<'_>, was: StateId, pos: BlockPos, rng: &mut SpawnRng) -> Vec<PoppedItem> {
    let tables = crate::block_drops::bundled_tables();
    match cause {
        Cause::Mined { held, drops: true, .. } if crate::block_drops::drops_are_allowed(was, held) => {
            crate::block_drops::drop_block_loot(tables, was, pos, held, rng)
        }
        Cause::Blast { radius, drops: true, .. } => crate::block_drops::drop_explosion_loot(tables, was, pos, radius, rng),
        Cause::Natural { drops: true } => crate::block_drops::drop_block_loot(tables, was, pos, None, rng),
        _ => Vec::new(),
    }
}

/// Spawns each popped stack as an item entity.
pub(crate) fn spawn(mobs: &MobHandle, popped: Vec<PoppedItem>) {
    if popped.is_empty() {
        return;
    }
    mobs.with(|sim| {
        for drop in popped {
            // `newly_dropped` sets the pickup delay, so the breaker cannot
            // re-absorb the drop on the spawning tick.
            let count = u8::try_from(drop.stack.count).unwrap_or(u8::MAX);
            sim.spawn_item(
                &drop.stack,
                drop.position,
                drop.velocity,
                lodestone_entity::ItemLifecycle::newly_dropped(count, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
            );
        }
    });
}

#[cfg(test)]
mod tests;
