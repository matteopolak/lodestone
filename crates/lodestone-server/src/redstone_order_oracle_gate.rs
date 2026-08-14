//! `docs/plans/redstone-execution-model.md`'s unit **U0**: an order-sensitive
//! oracle corpus, captured against a live vanilla 26.2 server, that exists
//! **before** any execution-model rework — the plan's own safety net, not
//! part of the rework itself.
//!
//! # Why this file is separate from `redstone_oracle_gate.rs` and
//! `redstone_diode_oracle_gate.rs`
//!
//! Both of those measure **steady state**: given a settled input, what value
//! does the circuit reach. Neither exercises a scenario where the *order* two
//! events are processed in changes the outcome — `docs/redstone.md` names this
//! gap explicitly: "Repeaters, comparators and observers are still unverified
//! against a live oracle... An order-sensitive circuit (a T-junction, a
//! repeater-locked latch) is the strongest remaining step."
//!
//! This file covers the **repeater-locked latch** half of that gap: a
//! repeater that already has a scheduled flip pending, raced against a side
//! lock arriving before that scheduled tick fires. `redstone_diode_oracle_gate.rs`'s
//! own locking test (`a_locked_repeater_schedules_nothing_while_the_same_rig_unlocked_schedules_at_two_ticks`)
//! only ever drives a repeater that is locked *from the start* — a steady-state
//! fact, not a race. This corpus is deliberately smaller than the plan's full
//! U0 wishlist (T-junction fan-out order, an observer chain, a BUD rig): each
//! of those needs its own live-oracle session and its own rig design, and a
//! shallow, unverified version of all four is worse than a solid version of
//! one — see this file's own "What is not covered" section at the bottom.
//!
//! # The oracle
//!
//! Every number below was measured on the live creative oracle
//! (`scripts/live-oracles/creative.sh`, game `:25570`, RCON `:25571`), under
//! `/tick freeze` and `/tick step 1`, reading `powered`/`locked` with
//! `execute if block <pos> <block>[<property>=<value>]` — the same technique
//! `redstone_diode_oracle_gate.rs`'s own doc comment describes, including its
//! two traps (`pause-when-empty-seconds` and the since-corrected `/tick step`
//! note). Rig, on one row at `y=1, z=98`: a standing torch at `x=91`, dust at
//! `x=92..=93`, a `repeater[facing=west,delay=1]` at `x=94`, output dust at
//! `x=95`, and a side-lock `repeater[facing=south,delay=1]` at `(94, 1, 99)`
//! (north of the main repeater's row, i.e. `z=99`, feeding it from the side
//! `DiodeBlock.getSignal` reads).
//!
//! ## What was measured
//!
//! 1. **The race, arrival-before-fire.** With the torch off and the rig
//!    settled (repeater unpowered, unlocked), the torch was lit — which
//!    schedules the repeater's flip for `gametime + 2` (the standard
//!    `delay=1` timing `redstone_diode_oracle_gate.rs`'s own table already
//!    established) — and, **in the same frozen instant**, the side lock was
//!    placed powered. The main repeater read `locked=true` immediately (no
//!    tick needed), and stayed `powered=false` **and** `locked=true` for
//!    every one of the next six stepped ticks — past the tick the flip was
//!    scheduled for.
//! 2. **The race, one tick of slack.** Repeated with one tick of real
//!    stepping between "torch lit" (`T+0`, schedules the flip for `T+2`) and
//!    "lock placed" (`T+1`, one tick before the scheduled fire): at `T+2`,
//!    the tick the flip was due, the repeater was still `powered=false` and
//!    `locked=true`. The lock arriving one tick after scheduling, but still
//!    before the scheduled tick itself fires, is enough to suppress it.
//! 3. **The reciprocal: unlocking with the front input already high.**
//!    Starting from the locked-and-unpowered state above (front input still
//!    high), the side lock was removed. The repeater read `locked=false`
//!    immediately, and powered on exactly **2 ticks** later — the same
//!    `delay=1 → 2 game ticks` figure as an ordinary flip, not a different
//!    constant for "unlocking" as a trigger.
//!
//! None of this was derived from this crate's own model: `decode(encode(x))
//! == x` is satisfied by two symmetric misunderstandings, and a
//! self-authored expectation for exactly the case this crate might have
//! gotten backwards is worth nothing.
//!
//! # What is gated here versus what is a re-implementation
//!
//! Steps 1 and 2 above call [`crate::random_tick::propagate_and_react`] —
//! the production entry point `crate::tick::run_tick_loop` itself calls —
//! for *every* mutation, including the side lock's own placement, so the
//! immediate `locked` recompute and the scheduling decision are both
//! production code. The scheduled tick's own *firing* is driven through
//! [`crate::redstone_diode::run_scheduled_tick`] directly, the same "drain
//! mirror" `redstone_diode_oracle_gate.rs`'s own module doc discloses and
//! names as a re-implementation rather than production coverage.
//!
//! # What is not covered (follow-up, per the plan's own U0 entry)
//!
//! - **T-junction fan-out order**: two dust paths of different length
//!   reconverging, to check whether `wire_update_fan_out`'s own
//!   `[pos] ++ UPDATE_ORDER` centre order (itself a documented *choice* —
//!   "vanilla's iteration order... is unspecified and cannot be copied") ever
//!   produces an observably different *event sequence* than vanilla's, not
//!   merely the same final state.
//! - **Observer chain**: a run of observers each watching the next, to pin
//!   the pulse-propagation cadence `redstone_diode_oracle_gate.rs`'s
//!   single-observer pulse-width measurement cannot see.
//! - **A BUD (block-update detector) rig**: a comparator or observer sensing
//!   a block-state change with no power change at all, which is the
//!   scenario `docs/plans/redstone-execution-model.md` §9 flags as needing a
//!   read of `CollectingNeighborUpdater`/chunk-ticket sources before it can
//!   even be designed correctly, not just measured.
//!
//! Each needs its own rig and its own live-oracle session; folding an
//! under-verified version of any of them into this landing would be exactly
//! the "described control, not a run one" trap `CLAUDE.md`'s evidence
//! standard warns about.

use crate::chunk::ChunkColumn;
use crate::neighbor_update::Direction;
use crate::random_tick::propagate_and_react;
use crate::scheduled_tick::ScheduledTickQueue;
use crate::{redstone, redstone_diode, redstone_torch, redstone_wire};
use lodestone_model::BlockPos;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR_Y: i32 = 0;
const Y: i32 = 1;
const ROW_Z: i32 = 8;
const NOW: u64 = 1_000;

// `ChunkColumn` coordinates are chunk-local (`0..16`); the live oracle rig
// this file's readings were measured against used absolute world coordinates
// `x=91..=95, z=98..=99` (an area clear of prior oracle debris), which this
// file's own `ChunkColumn` rig cannot address directly — only the relative
// layout (torch, two dust, repeater, output dust, one row apart) is what
// carries over, matching `redstone_diode_oracle_gate.rs`'s own rig shape.
const TORCH_X: i32 = 1;
const REPEATER_X: i32 = 4;
const OUT_X: i32 = 5;
/// The side-lock repeater's row — `ROW_Z + 1`, matching
/// `redstone_diode_oracle_gate.rs`'s own locking rig, whose own doc comment
/// explains why: the main repeater faces west, so its side positions are
/// north/south, and `DiodeBlock.getSignal` only answers for its own `FACING`.
const LOCK_Z: i32 = ROW_Z + 1;

/// `redstone_diode_oracle_gate.rs`'s live-measured `delay=1` timing: the
/// standard flip lands 2 game ticks after the notification that scheduled it,
/// on both edges. Restated here (not imported — that constant is private to
/// its own file) because reading 3 above depends on unlocking producing this
/// *same* figure rather than a different one, and a shared constant could not
/// tell the two apart if the delay function were wrong in a way that affected
/// both call sites identically.
const ORACLE_REPEATER_DELAY_1: u64 = 2;

fn column_with_floor() -> ChunkColumn {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            column.set_block(x, FLOOR_Y, z, "minecraft:stone");
        }
    }
    column
}

fn at(column: &ChunkColumn, x: i32, y: i32, z: i32) -> String {
    column.block_state(x, y, z).to_string()
}

/// The same rig `redstone_diode_oracle_gate.rs`'s `repeater_rig` builds
/// (unpowered repeater, torch off, dust at its pre-flip zero so the first
/// `propagate_and_react` from the torch is what derives it), moved to this
/// file's own row so a coordinate typo cannot silently alias the two files'
/// rigs.
fn locked_latch_rig() -> ChunkColumn {
    let mut column = column_with_floor();
    column.set_block(TORCH_X, Y, ROW_Z, &redstone_torch::set_standing_lit(false));
    column.set_block(TORCH_X + 1, Y, ROW_Z, &redstone_wire::set_power(0));
    column.set_block(TORCH_X + 2, Y, ROW_Z, &redstone_wire::set_power(0));
    column.set_block(
        REPEATER_X,
        Y,
        ROW_Z,
        &redstone_diode::set_repeater(Direction::West, 1, false, false),
    );
    column.set_block(OUT_X, Y, ROW_Z, &redstone_wire::set_power(0));
    column
}

/// Every entry `propagate_and_react` scheduled, as `(pos, kind, trigger_tick)`.
fn scheduled(block_ticks: &mut ScheduledTickQueue<String>) -> Vec<((i32, i32, i32), String, u64)> {
    block_ticks
        .drain_due(NOW + 4_096, 4_096)
        .into_iter()
        .map(|t| (t.pos, t.kind, t.trigger_tick))
        .collect()
}

fn find(entries: &[((i32, i32, i32), String, u64)], pos: (i32, i32, i32), kind: &str) -> Option<u64> {
    entries.iter().find(|(p, k, _)| *p == pos && k == kind).map(|(_, _, t)| *t)
}

/// **Reading 1 and 2's gate.** A repeater whose flip was already scheduled
/// must be suppressed if a side lock arrives before that scheduled tick
/// fires — proven directly (calling
/// [`redstone_diode::run_scheduled_tick`] at the state the notification
/// pipeline actually produced), not by reasoning about what "should" happen.
///
/// # The premise, and the negative control
///
/// Without a `should_schedule` premise check, "nothing happened" is
/// ambiguous between "correctly suppressed" and "never reached the repeater
/// at all" — the exact trap `redstone_diode_oracle_gate.rs`'s own doc
/// comment names as having cost real time on this rig once already. So the
/// control arm runs the identical sequence with **no** side lock and
/// requires it to schedule normally, at the oracle's own `delay=1 → 2 ticks`
/// figure.
#[test]
fn a_repeater_arrival_locked_before_its_scheduled_flip_fires_is_suppressed() {
    // -- Locked arm: lock arrives in the same instant the source turns on. --
    let mut column = locked_latch_rig();
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();

    // T+0: the source lights, scheduling the repeater's flip.
    column.set_block(TORCH_X, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    let _ = propagate_and_react(&mut column, 0, 0, TORCH_X, Y, ROW_Z, &mut block_ticks, NOW);
    let pre_lock_entries = scheduled(&mut block_ticks);
    let fire_tick = find(&pre_lock_entries, (REPEATER_X, Y, ROW_Z), redstone::TICK_REPEATER)
        .unwrap_or_else(|| {
            panic!(
                "PREMISE FAILED: lighting the source scheduled nothing for the repeater at \
                 (x={REPEATER_X}, y={Y}, z={ROW_Z}) -- the race below would be vacuous. \
                 entries: {pre_lock_entries:?}"
            )
        });
    assert_eq!(
        fire_tick - NOW,
        ORACLE_REPEATER_DELAY_1,
        "PREMISE: the unraced schedule must land on the oracle's own delay=1 figure"
    );

    // The lock arrives before `fire_tick` — the race. `propagate_and_react`
    // is called from the side repeater's own position, exactly as
    // `react_to_notification`'s arm 3b (repeaters) is reached in production
    // when a neighbouring diode's placement notifies it.
    column.set_block(
        REPEATER_X,
        Y,
        LOCK_Z,
        &redstone_diode::set_repeater(Direction::South, 1, false, true),
    );
    let mut lock_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, REPEATER_X, Y, LOCK_Z, &mut lock_ticks, NOW + 1);
    let locked_state = at(&column, REPEATER_X, Y, ROW_Z);
    assert!(
        redstone_diode::is_locked(
            &redstone::make_lookup(&column, 0, 0),
            BlockPos::new(REPEATER_X, Y, ROW_Z),
            Direction::West
        ),
        "PREMISE FAILED: the side lock did not take -- state is {locked_state:?}"
    );

    // Fire the *original* scheduled entry now, against the post-lock state --
    // exactly what `run_tick_loop`'s drain does: read the column at fire
    // time, not at scheduling time.
    match redstone_diode::run_scheduled_tick(&locked_state, true) {
        redstone_diode::RepeaterTickOutcome::Locked => {}
        other => panic!(
            "a repeater locked before its scheduled flip fired must report Locked at fire time, \
             got {other:?} -- the live server measured `powered=false, locked=true` for at least \
             6 ticks past the scheduled fire tick under this exact race"
        ),
    }

    // -- Control arm: identical sequence, no side lock. --
    let mut control_column = locked_latch_rig();
    let mut control_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    control_column.set_block(TORCH_X, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    let _ =
        propagate_and_react(&mut control_column, 0, 0, TORCH_X, Y, ROW_Z, &mut control_ticks, NOW);
    let control_entries = scheduled(&mut control_ticks);
    let control_fire = find(&control_entries, (REPEATER_X, Y, ROW_Z), redstone::TICK_REPEATER)
        .unwrap_or_else(|| {
            panic!(
                "CONTROL FAILED: the unlocked control scheduled nothing either, so the locked \
                 arm's suppression proves nothing about locking specifically. entries: \
                 {control_entries:?}"
            )
        });
    let control_state = at(&control_column, REPEATER_X, Y, ROW_Z);
    match redstone_diode::run_scheduled_tick(&control_state, true) {
        redstone_diode::RepeaterTickOutcome::TurnedOn { new_state, .. } => {
            assert!(
                redstone::diode_powered(&new_state),
                "CONTROL FAILED: the unlocked control's own flip did not turn the repeater on"
            );
        }
        other => panic!("CONTROL FAILED: expected the unlocked control to turn on, got {other:?}"),
    }
    assert_eq!(
        control_fire - NOW,
        ORACLE_REPEATER_DELAY_1,
        "CONTROL: the unlocked control must schedule at the oracle's own delay=1 figure, or it is \
         not a fair comparison against the locked arm"
    );
}

/// **Reading 3's gate, the reciprocal direction.** Unlocking a repeater whose
/// front input is already high must schedule a fresh flip — at the *same*
/// `delay=1 → 2 game ticks` figure an ordinary edge uses, not a different
/// constant for "unlocked" as the trigger. Live-measured: removing the lock
/// at `gametime T` powered the repeater at exactly `T + 2`.
#[test]
fn unlocking_with_the_front_input_already_high_schedules_a_fresh_flip_at_the_ordinary_delay() {
    let mut column = locked_latch_rig();
    let mut settle: ScheduledTickQueue<String> = ScheduledTickQueue::new();

    // Reach the same locked-and-pending state the race gate above starts
    // from: light the source, then lock before the flip fires.
    column.set_block(TORCH_X, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    let _ = propagate_and_react(&mut column, 0, 0, TORCH_X, Y, ROW_Z, &mut settle, NOW);
    column.set_block(
        REPEATER_X,
        Y,
        LOCK_Z,
        &redstone_diode::set_repeater(Direction::South, 1, false, true),
    );
    let mut lock_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, REPEATER_X, Y, LOCK_Z, &mut lock_ticks, NOW + 1);
    assert!(
        redstone_diode::is_locked(
            &redstone::make_lookup(&column, 0, 0),
            BlockPos::new(REPEATER_X, Y, ROW_Z),
            Direction::West
        ),
        "PREMISE FAILED: the repeater must be locked before this gate removes the lock"
    );
    // Drop the pending schedule the setup above created — this gate is about
    // what *removing the lock* schedules on its own, not a leftover entry
    // from setup, matching this file's own "arrival-before-fire" gate's
    // premise check but discarded here rather than asserted twice.
    let _ = scheduled(&mut lock_ticks);

    // Remove the lock at `NOW + 2` (an arbitrary tick past setup — the
    // oracle measured the *interval*, not an absolute tick).
    const UNLOCK_TICK: u64 = NOW + 2;
    column.set_block(REPEATER_X, Y, LOCK_Z, "minecraft:air");
    let mut unlock_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ =
        propagate_and_react(&mut column, 0, 0, REPEATER_X, Y, LOCK_Z, &mut unlock_ticks, UNLOCK_TICK);

    assert!(
        !redstone_diode::is_locked(
            &redstone::make_lookup(&column, 0, 0),
            BlockPos::new(REPEATER_X, Y, ROW_Z),
            Direction::West
        ),
        "the repeater must read unlocked immediately after the side block is removed, with no \
         tick required -- `recompute_locked` is documented as an immediate write"
    );

    let entries = unlock_ticks
        .drain_due(UNLOCK_TICK + 4_096, 4_096)
        .into_iter()
        .map(|t| (t.pos, t.kind, t.trigger_tick))
        .collect::<Vec<_>>();
    let fire_tick = find(&entries, (REPEATER_X, Y, ROW_Z), redstone::TICK_REPEATER).unwrap_or_else(
        || {
            panic!(
                "unlocking a repeater whose front input is already high scheduled nothing -- the \
                 live server powered it exactly 2 ticks after the lock was removed. entries: \
                 {entries:?}"
            )
        },
    );
    assert_eq!(
        fire_tick - UNLOCK_TICK,
        ORACLE_REPEATER_DELAY_1,
        "unlocking scheduled a flip {} tick(s) after removal; the live server measured exactly {} \
         (the ordinary delay=1 figure, not a distinct constant for unlocking)",
        fire_tick - UNLOCK_TICK,
        ORACLE_REPEATER_DELAY_1
    );
}
