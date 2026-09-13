//! `docs/plans/redstone-execution-model.md`'s **U0** corpus, piston half:
//! interrupting a piston's own pending commit — the specific "update-order
//! quirk" that names this class of bug, and the mechanism a 0-tick pulse
//! generator depends on.
//!
//! # Why this file is separate from `redstone_order_oracle_gate.rs`
//!
//! That file's own doc comment names its scope as the **repeater-locked
//! latch** half of U0 and lists "a BUD rig" among what it does not cover.
//! This file covers a different order-sensitive race, specific to pistons:
//! what happens when a piston is asked to retract while its own *extend* is
//! still mid-animation, before the two-tick commit
//! (`crate::piston::PISTON_MOVE_DELAY`) has landed.
//!
//! # The oracle
//!
//! Measured on the live creative oracle (`scripts/live-oracles/creative.sh`,
//! game `:25570`, RCON `:25571`), under `/tick freeze` and `/tick step 1`,
//! reading block-entity NBT with `data get block <pos>` (which vanilla only
//! answers for a block *with* a block entity — a `moving_piston` cell always
//! has one, a plain `piston`/`air`/`dirt` cell never does, so the presence or
//! absence of `data get`'s "not a block entity" response is itself a signal)
//! and plain block identity with `execute if block <pos> <id>`.
//!
//! Rig: a `piston[facing=south,extended=false]` with a `minecraft:dirt` block
//! one cell ahead (the push direction) and a `minecraft:redstone_block`
//! beside it (any face but the push direction — quasi-connectivity is not in
//! play here, this is a direct, non-QC signal). Sequence:
//!
//! 1. Place the `redstone_block`, then `tick step 1`: the piston extends.
//!    Reading the piston's own **arm** cell (`pos.relative(facing)`,
//!    one step ahead) as a block entity returned
//!    `{extending: 1b, source: 1b, progress: 0.0f, blockState: {Name:
//!    "minecraft:piston_head", ...}}` — a fresh `source` entity, exactly
//!    [`crate::piston::begin_move`]'s own shape for an extension's arm cell.
//! 2. Remove the `redstone_block` (no further ticks) — this is the retract
//!    signal arriving *before* the extend's own commit, exactly the
//!    situation [`crate::piston::interrupt`] exists for. Then `tick step 1`:
//!    * the **arm** cell now reads as plain `minecraft:air` via `execute if
//!      block ... minecraft:air` — the head that was mid-materialisation
//!      never appears, matching [`crate::piston::interrupt`]'s `source` arm
//!      writing air rather than the moved state;
//!    * the piston's own **base** cell now reads as a block entity with
//!      `extending: 0b, source: 1b` — the retraction's own new moving entity,
//!      created in the same tick the interrupt ran, exactly
//!      [`crate::piston::begin_move`]'s retraction branch;
//!    * the cell the dirt was pushed *into* (two cells from the base, one
//!      past the arm) **still reads as a block entity**, `source: 0b,
//!      extending: 1b, progress: 0.5f` — continuing its own, independent
//!      countdown, unaffected by the arm's interruption one cell closer to
//!      the piston.
//! 3. Further `tick step 1`s: the pushed dirt's own entity reaches its commit
//!    and the cell becomes `minecraft:dirt` (no longer a block entity) while
//!    the retraction's own base entity is still animating; one tick later
//!    the base commits back to plain `minecraft:piston[extended=false]`.
//!
//! # What this proves, and what it does not
//!
//! This confirms the *shape* of the interrupt (arm evaporates to air, the
//! pushed block's own entity is untouched, a fresh retraction entity appears
//! at the base) against a real server. It does not capture the **0-tick
//! pulse** contraption itself (a piston re-extending inside the same window)
//! — that needs its own rig and is still open, same as
//! `redstone_order_oracle_gate.rs`'s own "what is not covered" list names for
//! its remaining U0 items.
//!
//! # What is gated here versus what is a re-implementation
//!
//! [`crate::random_tick::propagate_and_react`] is the production entry point
//! `crate::tick::run_tick_loop` itself calls, driven twice below (once for
//! the extend, once for the retract) exactly as two separate neighbour
//! notifications reach it in production — nothing here re-implements the
//! reaction dispatch.

use crate::chunk::ChunkColumn;
use crate::neighbor_update::Direction;
use crate::piston;
use crate::random_tick::propagate_and_react;
use crate::redstone_torch;
use crate::scheduled_tick::ScheduledTickQueue;

const FLOOR_Y: i32 = 0;
const Y: i32 = 1;
const ROW_Z: i32 = 8;
const PISTON_X: i32 = 4;
const NOW: u64 = 40;

fn column_with_floor() -> ChunkColumn {
    let mut column = ChunkColumn::new(FLOOR_Y - 1, 16);
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

/// Every pending piston commit, as `(pos, entity)` — a thin wrapper over
/// [`ScheduledTickQueue::iter`] (non-destructive, unlike `drain_due`, which
/// this file must not use mid-sequence or it would desync the queue from
/// what the next `propagate_and_react` call needs to see).
fn pending_piston_commits(
    block_ticks: &ScheduledTickQueue<String>,
) -> Vec<((i32, i32, i32), piston::MovingBlockEntity)> {
    block_ticks
        .iter()
        .filter(|t| piston::is_finish_kind(&t.kind))
        .filter_map(|t| piston::parse_finish_kind(&t.kind).map(|e| (t.pos, e)))
        .collect()
}

/// The rig: `piston[facing=south,extended=false]` at `(PISTON_X, Y, ROW_Z)`,
/// `minecraft:dirt` one cell south (the push direction, `PISTON_X, Y, ROW_Z+1`
/// -- south increases `z`, matching every other file in this family's own
/// `Direction::South` convention), and an *unlit* trigger the caller lights.
fn piston_rig() -> ChunkColumn {
    let mut column = column_with_floor();
    column.set_block(PISTON_X, Y, ROW_Z, "minecraft:piston[facing=south,extended=false]");
    column.set_block(PISTON_X, Y, ROW_Z + 1, "minecraft:dirt");
    column.set_block(PISTON_X - 1, Y, ROW_Z, &redstone_torch::set_standing_lit(false));
    column
}

/// **The gate.** Extending, then retracting before the commit lands, must:
/// leave the arm cell reading air (never `piston_head`); cancel exactly the
/// arm's own pending commit, not the pushed block's; and schedule a fresh
/// retraction commit at the base. Every claim is checked against the
/// collection of pending commits at each stage, not a single `assert!` per
/// stage, so a version that cancels the *wrong* entry (or none) is caught by
/// what is left as much as by what is gone.
#[test]
fn retracting_mid_extend_interrupts_only_the_arm_and_leaves_the_pushed_blocks_own_commit_alone() {
    let mut column = piston_rig();
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();

    // -- Extend: light the trigger. --
    column.set_block(PISTON_X - 1, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        PISTON_X - 1,
        Y,
        ROW_Z,
        &mut block_ticks,
        NOW,
    );

    let arm = (PISTON_X, Y, ROW_Z + 1);
    let pushed = (PISTON_X, Y, ROW_Z + 2);
    let base = (PISTON_X, Y, ROW_Z);

    assert!(
        piston::is_moving_piston(&at(&column, arm.0, arm.1, arm.2)),
        "PREMISE FAILED: the arm cell must hold a moving_piston right after extending, got {:?}",
        at(&column, arm.0, arm.1, arm.2)
    );
    let before = pending_piston_commits(&block_ticks);
    let arm_entity_before = before
        .iter()
        .find(|(pos, _)| *pos == arm)
        .map(|(_, e)| e.clone())
        .unwrap_or_else(|| {
            panic!("PREMISE FAILED: no pending commit at the arm cell -- the race below is vacuous. pending: {before:?}")
        });
    assert!(arm_entity_before.source, "PREMISE: the arm's own entity must be a source cell");
    assert!(
        before.iter().any(|(pos, _)| *pos == pushed),
        "PREMISE FAILED: the pushed dirt must have its own pending commit two cells from the \
         base -- pending: {before:?}"
    );

    // -- Retract, one tick later, before PISTON_MOVE_DELAY (2) has elapsed. --
    column.set_block(PISTON_X - 1, Y, ROW_Z, &redstone_torch::set_standing_lit(false));
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        PISTON_X - 1,
        Y,
        ROW_Z,
        &mut block_ticks,
        NOW + 1,
    );

    // The arm must read plain air -- never `piston_head`, and not still
    // `moving_piston` either (that would mean the interrupt never landed).
    let arm_state = at(&column, arm.0, arm.1, arm.2);
    assert_eq!(
        arm_state, "minecraft:air",
        "the interrupted arm cell must evaporate to air, not materialise a head or stay \
         mid-animation; got {arm_state:?}"
    );

    let after = pending_piston_commits(&block_ticks);
    let arm_pending: Vec<_> = after.iter().filter(|(pos, _)| *pos == arm).collect();
    assert!(
        arm_pending.is_empty(),
        "the arm's original commit must be cancelled by the interrupt, not merely superseded; \
         still pending: {arm_pending:?}"
    );

    let pushed_entity_after = after
        .iter()
        .find(|(pos, _)| *pos == pushed)
        .map(|(_, e)| e.clone())
        .unwrap_or_else(|| {
            panic!(
                "the pushed dirt's own commit must survive the arm's interrupt untouched -- \
                 pending: {after:?}"
            )
        });
    // Untouched means byte-identical to what extending scheduled, not just
    // "still present" -- a version that re-derives it fresh could coincide.
    let pushed_entity_before =
        before.iter().find(|(pos, _)| *pos == pushed).map(|(_, e)| e.clone()).unwrap();
    assert_eq!(
        pushed_entity_after, pushed_entity_before,
        "the pushed block's pending commit must be byte-identical before and after the arm's \
         interrupt -- the interrupt must touch only the arm cell"
    );

    let base_pending: Vec<_> = after.iter().filter(|(pos, _)| *pos == base).collect();
    assert_eq!(
        base_pending.len(),
        1,
        "retracting must schedule exactly one fresh commit at the piston's own base cell; got \
         {base_pending:?}"
    );
    assert!(!base_pending[0].1.extending, "the base's own entity must be a retraction (extending=false)");
    assert!(base_pending[0].1.source, "the base's own entity must be a source cell");
    assert_eq!(
        base_pending[0].1.direction,
        Direction::South,
        "the base's own entity must carry the piston's facing"
    );
}

/// Negative control for the gate above: retracting **after** the extend's
/// commit has already landed (no pending entity left at the arm at all) must
/// not find anything to interrupt, must not panic, and must not accidentally
/// cancel the retraction's own freshly scheduled commit. Without this, the
/// gate above cannot distinguish "correctly found and cancelled one entry"
/// from "the cancellation code path never runs at all".
#[test]
fn retracting_after_the_extend_already_committed_finds_nothing_to_interrupt() {
    let mut column = piston_rig();
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();

    column.set_block(PISTON_X - 1, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    let _ = propagate_and_react(&mut column, 0, 0, PISTON_X - 1, Y, ROW_Z, &mut block_ticks, NOW);

    let arm = (PISTON_X, Y, ROW_Z + 1);
    assert!(
        pending_piston_commits(&block_ticks).iter().any(|(pos, _)| *pos == arm),
        "PREMISE FAILED: extending must schedule a commit at the arm"
    );

    // Run the commit to completion by hand -- the same replay every other
    // two-phase test in `piston.rs` uses, not a re-derivation: drain exactly
    // the due entries and apply their own `moved_state`.
    let due = block_ticks.drain_due(NOW + piston::PISTON_MOVE_DELAY, 100);
    for tick in &due {
        if let Some(entity) = piston::parse_finish_kind(&tick.kind) {
            let (x, y, z) = tick.pos;
            column.set_block(x, y, z, entity.committed_state());
        }
    }
    assert!(
        !piston::is_moving_piston(&at(&column, arm.0, arm.1, arm.2)),
        "PREMISE FAILED: the arm must have committed away from moving_piston by now, got {:?}",
        at(&column, arm.0, arm.1, arm.2)
    );

    column.set_block(PISTON_X - 1, Y, ROW_Z, &redstone_torch::set_standing_lit(false));
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        PISTON_X - 1,
        Y,
        ROW_Z,
        &mut block_ticks,
        NOW + piston::PISTON_MOVE_DELAY + 1,
    );

    let base = (PISTON_X, Y, ROW_Z);
    let after = pending_piston_commits(&block_ticks);
    let base_pending: Vec<_> = after.iter().filter(|(pos, _)| *pos == base).collect();
    assert_eq!(
        base_pending.len(),
        1,
        "CONTROL FAILED: retracting after the extend already committed must still schedule its \
         own base commit normally; got {base_pending:?}"
    );
}
