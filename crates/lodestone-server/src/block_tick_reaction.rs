//! One due entry from the scheduled block-tick queue, resolved: the state the
//! block becomes, plus the cascade that state change drives.
//!
//! ## What it is
//!
//! The world tick loop drains [`crate::ScheduledTickQueue`] once per game tick
//! and, for each due entry, has to answer two questions: what does *this*
//! block do now, and what does the rest of the circuit do about it. This
//! module is that answer. [`run_due_block_tick`] takes a due entry's kind,
//! position and current state and returns a [`BlockTickReaction`] — the new
//! state if the block changed, and every further world edit the change
//! cascaded into.
//!
//! ## Why it is a function and not loop body
//!
//! It used to be neither: it was ~80 lines inline in the tick loop's drain,
//! reachable only by standing up the whole loop — a live `MobSim`, a block
//! entity registry, a weather handle, a world-state handle, a command tree.
//! So the delayed redstone families (torch, repeater, comparator, observer)
//! had no test that ran a *chain* of them across several ticks, which is the
//! only way their delays and their ordering are observable at all. Extracting
//! it makes the drain callable from a differential oracle that has none of
//! those handles — `crates/lodestone-fuzz`'s redstone side is the consumer —
//! and it is what lets an exact-tick, multi-repeater contraption be asserted
//! against a number measured on a real server.
//!
//! The wire half deliberately stays in the caller: which sounds to publish,
//! which entities a moving piston shoves, what a note block vibrates. Those
//! need feeds and a mob simulation, and none of them changes a block state.
//!
//! ## How to change it
//!
//! Adding a delayed block family means one more arm in
//! [`run_due_block_tick`]'s decision, keyed on the tick kind, returning
//! `Some(new_state)` when the block changes and `None` when it does not. A
//! family whose reaction rewrites *more than its own cell* does not belong
//! here — fire spread, gravity settling, a tripwire recheck and a dispenser
//! firing are all handled by their own arms in the caller's drain, before this
//! is reached, precisely because a single `Option<String>` cannot express
//! them.
//!
//! Gotcha: the cascade is run whenever the decision is `Some`, **including**
//! when the new state equals the old one. That is not redundant. A repeater
//! re-affirming `powered=true` still has to notify its neighbours, because
//! the notification is what re-arms the next component down the line; gating
//! the cascade on an actual state change stalls a chain at its first
//! steady-state link.
//!
//! ## Dependencies
//!
//! The per-family decisions live with their families (`redstone_torch`,
//! `redstone_diode`, `redstone_observer`, `redstone_target`, `hand_use`,
//! `piston`); the cascade is `random_tick`'s cross-chunk fan-out. Reads go
//! through `random_tick::RedstoneColumns`, so a repeater whose input or side
//! is across a chunk seam sees what the resident neighbour actually holds.

use lodestone_model::BlockPos;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::{ChunkColumn, ChunkSource};
use crate::random_tick::RandomTickEvent;
use crate::scheduled_tick::{ScheduledTickQueue, TickPriority};

/// What one due block tick did.
#[derive(Debug, Default)]
pub struct BlockTickReaction {
    /// The state the tick's own position now holds, if the block decided to
    /// change at all. `Some` with a value equal to the old state is a real
    /// outcome, not a no-op — see the module doc.
    pub new_state: Option<String>,
    /// Every *other* cell the change rewrote, in the order the cascade
    /// produced them. Written into the home column already, but **not**
    /// through `world`: the cascade's view of a neighbouring column is a
    /// copy, so a caller has to write each event back through its own
    /// [`ChunkSource`] as well as publishing it.
    pub events: Vec<RandomTickEvent>,
}

/// Resolves one due entry from the block-tick queue.
///
/// `state` is the state currently at `pos`, read by the caller (which already
/// holds the column for its own bounds check). `min_x`/`min_z` are `column`'s
/// own origin in world coordinates. Reschedules — a repeater re-arming after
/// its delay, an observer's second pulse phase — land in `block_ticks`
/// relative to `current_tick`.
///
/// The write of `new_state` happens here, through both `column` and `world`,
/// so the cascade below it reads the post-change world exactly as the drain
/// used to.
#[allow(clippy::too_many_arguments)]
pub fn run_due_block_tick(
    column: &mut ChunkColumn,
    min_x: i32,
    min_z: i32,
    world: &dyn ChunkSource,
    kind: &str,
    pos: BlockPos,
    state: &str,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
    block_entities: Option<&BlockEntityHandle>,
) -> BlockTickReaction {
    let (x, y, z) = (pos.x, pos.y, pos.z);
    // Scoped so the borrow this view holds on `column` ends before the direct
    // `column.set_block` below and before the cascade builds its own view.
    let new_state = {
        let columns = crate::random_tick::RedstoneColumns::new(column, min_x, min_z, world);
        if kind == crate::redstone::TICK_TORCH {
            let has_signal = crate::redstone_torch::has_neighbor_signal(
                &crate::redstone::make_columns_lookup(&columns),
                pos,
                state,
            );
            crate::redstone_torch::run_scheduled_tick(state, has_signal)
        } else if kind == crate::redstone::TICK_REPEATER {
            let facing = crate::redstone::diode_facing(state);
            let should_on = crate::redstone_diode::repeater_should_turn_on(
                &crate::redstone::make_columns_lookup(&columns),
                pos,
                facing,
            );
            match crate::redstone_diode::run_scheduled_tick(state, should_on) {
                crate::redstone_diode::RepeaterTickOutcome::TurnedOff(s) => Some(s),
                crate::redstone_diode::RepeaterTickOutcome::TurnedOn { new_state, reschedule } => {
                    if reschedule {
                        let delay = crate::redstone_diode::repeater_delay(&new_state);
                        block_ticks.schedule(
                            (x, y, z),
                            crate::redstone::TICK_REPEATER.to_string(),
                            current_tick + u64::from(delay),
                            TickPriority::VeryHigh,
                        );
                    }
                    Some(new_state)
                }
                crate::redstone_diode::RepeaterTickOutcome::Locked
                | crate::redstone_diode::RepeaterTickOutcome::NoChange => None,
            }
        } else if kind == crate::redstone::TICK_COMPARATOR {
            let facing = crate::redstone::diode_facing(state);
            let input = crate::redstone::input_signal(
                &crate::redstone::make_columns_lookup(&columns),
                pos,
                facing,
            );
            let side = crate::redstone::alternate_signal(
                &crate::redstone::make_columns_lookup(&columns),
                pos,
                facing,
                false,
            );
            crate::redstone_diode::run_scheduled_comparator_tick(state, input, side)
        } else if kind == crate::redstone::TICK_OBSERVER {
            let (new_state, reschedule) = crate::redstone_observer::run_scheduled_tick(state);
            if reschedule {
                block_ticks.schedule(
                    (x, y, z),
                    crate::redstone::TICK_OBSERVER.to_string(),
                    current_tick + 2,
                    TickPriority::Normal,
                );
            }
            Some(new_state)
        } else if kind == crate::redstone_target::TICK_TARGET_DECAY {
            // A target block's analog `power` decaying back to 0 after a
            // projectile hit set it. Scheduled by the projectile-block-hit
            // resolution earlier in the same drain region.
            crate::redstone_target::run_scheduled_tick(state)
        } else if kind == crate::hand_use::TICK_BUTTON {
            // A pressed button releasing itself once its hold time is up, so
            // a button feeding a door closes it again when the button pops.
            crate::hand_use::release_button(state)
        } else if crate::piston::is_finish_kind(kind) {
            // The commit phase of a piston move. The state to write travels
            // in the tick's own kind, because the pending tick *is* this
            // crate's moving block entity (see `piston::finish_kind`).
            //
            // Guarded on the cell still holding a moving-piston state:
            // anything else already rewrote it (a player broke it, a second
            // move claimed it), and committing over that would resurrect a
            // block without a matching pending move.
            if crate::piston::is_moving_piston(state) {
                crate::piston::parse_finish_kind(kind).map(|entity| entity.moved_state)
            } else {
                None
            }
        } else {
            // No other block-tick behaviour is modelled — see this module's
            // own doc comment for which families are handled elsewhere and
            // why they cannot be handled here.
            None
        }
    };

    let Some(new_state) = new_state else {
        return BlockTickReaction::default();
    };
    let changed = new_state != state;
    if changed {
        column.set_block(x - min_x, y, z - min_z, &new_state);
        world.set_block(x, y, z, &new_state);
    }
    let events = crate::random_tick::propagate_and_react_with_entities_across_chunks(
        column,
        min_x,
        min_z,
        world,
        x,
        y,
        z,
        block_ticks,
        current_tick,
        block_entities,
    );
    BlockTickReaction {
        new_state: Some(new_state),
        events,
    }
}
