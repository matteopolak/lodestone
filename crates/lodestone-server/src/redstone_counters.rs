//! Structural counters through the redstone notification/reaction/scheduling
//! path — `docs/plans/redstone-execution-model.md`'s unit **U1**, the first
//! deliverable everything conditional in that plan keys off (§6, §9): "the
//! actual cost split (parse vs. read vs. enumerate vs. queue)".
//!
//! # What it is
//!
//! A flat set of process-global relaxed atomics, incremented from named hook
//! points in [`crate::random_tick::propagate_and_react`] and
//! [`crate::redstone`], **compiled out entirely unless the
//! `redstone-counters` cargo feature is on** — the same shape
//! `lodestone-worldgen-core::counters` already established for the worldgen
//! rewrite, copied deliberately rather than reinvented. Every hook is
//! `#[inline(always)]` with an empty body in the default build, so a release
//! build without the feature contains no counter code and no `#[cfg]`
//! clutter at any call site.
//!
//! # Why counters, not a timing
//!
//! Measured on this machine: wall clock reproduces at ~10.8%, instruction
//! counts at 0.16–0.21%, while the quantities this plan's execution-model
//! rework tries to reduce (notifications, reads, schedules) are *counts* by
//! nature and machine-independent. §6 of the plan names the rule this module
//! exists to satisfy: **a counter that cannot predict a hand-derived value
//! cannot gate** anything — see this module's own
//! `wire_recomputes_matches_the_hand_derived_count_for_a_single_settling_cell`
//! test for the one calibrated fixture, and the null-contraption control
//! below for the "an idle circuit costs zero" tripwire §6 also names.
//!
//! # The counters
//!
//! | counter | bumped where | what it answers |
//! |---|---|---|
//! | [`Snapshot::notifications_issued`] | once per [`Notification`](crate::neighbor_update::Notification) `react_to_notification` receives | how much work one mutation's cascade fans out to |
//! | [`Snapshot::reactions_dispatched`] | once per family arm that actually fires, by [`ReactionKind`] | which families the cascade actually touches, not merely visits |
//! | [`Snapshot::cell_reads`] | inside [`crate::redstone::make_lookup`]'s closure | raw `ChunkColumn::block_state` reads during signal computation — Layer A's own target |
//! | [`Snapshot::state_parses`] | inside [`crate::redstone::own_signal`] | how often a state string is interpreted into a signal value, the representative "parse" point every family's own-signal read funnels through |
//! | [`Snapshot::signal_queries`] | inside [`crate::redstone::best_neighbor_signal`] | higher-level neighbour-signal queries, each of which issues several `cell_reads` |
//! | [`Snapshot::wire_recomputes`] | once per dust re-evaluation (`redstone_wire::calculate_target_strength`) | the dust-specific share of the cascade, since §8 names wire-run batching as the likely next step if this dominates |
//! | [`Snapshot::schedules_requested`] / [`Snapshot::schedules_deduped`] | at each dedup-guarded `block_ticks.schedule(..)` site (torch/repeater/comparator/observer) | how often a scheduling decision was made versus skipped as already pending |
//! | [`Snapshot::max_notifications_per_drain`] | end of each [`crate::random_tick::propagate_and_react`] call | the latency counter: peak notifications processed inside one *unserviced window* — one call is the closest boundary this crate can name without also touching `tick.rs` (a choke-point file this unit does not open); a real worst-case cascade (a piston-clock loop) would show up here first |
//!
//! `reactions_dispatched` is not exhaustive over every family
//! `react_to_notification` matches (gravity, snowy upkeep, piston, hopper,
//! note block, rail and dispenser all fold into
//! [`ReactionKind::Other`]) — the plan's own scope for U1 is "the actual cost
//! split", answerable from the five families that also have live-oracle
//! coverage (dust, torch, repeater, comparator, observer); widening the
//! per-kind breakdown is a follow-up with no architectural blocker.
//!
//! # How to change it
//!
//! Adding a counter is the same three edits `lodestone-worldgen-core`'s
//! module doc describes: a field on [`Snapshot`], a `bump_*` hook in the
//! `redstone-counters` `imp` module below (plus its empty twin in the
//! `not(feature)` module), and the one call site. **Call the hook
//! unconditionally at the call site** — a `#[cfg(feature = ...)]` there is
//! how a hook silently stops being called, the same gotcha
//! `lodestone-worldgen-core::counters` warns about.
//!
//! # Configuration
//!
//! One cargo feature, `redstone-counters`, default **off**, defined in this
//! crate's own `Cargo.toml`.
//!
//! # Dependencies
//!
//! None beyond `core`/`std`.

/// Which family a dispatched reaction belongs to, for
/// [`Snapshot::reactions_dispatched`]. See this module's own doc table for
/// which families are folded into [`ReactionKind::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ReactionKind {
    Dust = 0,
    Torch = 1,
    Repeater = 2,
    Comparator = 3,
    Observer = 4,
    /// Every other `react_to_notification` family — gravity, snowy upkeep,
    /// pistons, hoppers, redstone-openable blocks, note blocks, rails and
    /// dispensers. See this module's own doc for why these are not split
    /// further yet.
    Other = 5,
}

/// Number of [`ReactionKind`] variants.
pub const REACTION_KIND_COUNT: usize = 6;

/// [`ReactionKind`] names in discriminant order.
pub const REACTION_KIND_NAMES: [&str; REACTION_KIND_COUNT] =
    ["dust", "torch", "repeater", "comparator", "observer", "other"];

/// A plain-`u64` reading, independent of whether the `redstone-counters`
/// feature is on — `snapshot()` returns all zeros without it, which is
/// indistinguishable from "measured and found nothing" by design: a caller
/// that forgets to enable the feature gets a silently vacuous measurement
/// rather than a compile error, so **always check the feature is on before
/// trusting a non-zero-expected reading**, the same "a guard that measured
/// nothing must not share a value with a pass" rule `CLAUDE.md` names for
/// `wasm-check.sh`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub notifications_issued: u64,
    pub reactions_dispatched: [u64; REACTION_KIND_COUNT],
    pub cell_reads: u64,
    pub state_parses: u64,
    pub signal_queries: u64,
    pub wire_recomputes: u64,
    pub schedules_requested: u64,
    pub schedules_deduped: u64,
    pub max_notifications_per_drain: u64,
}

impl Snapshot {
    /// Total reactions dispatched across every [`ReactionKind`], including
    /// [`ReactionKind::Other`].
    #[must_use]
    pub fn reactions_total(&self) -> u64 {
        self.reactions_dispatched.iter().sum()
    }
}

/// Zeroes every counter. A measurement is `reset(); work(); snapshot()`, the
/// same pattern `lodestone-worldgen-core::counters` documents and for the
/// same reason: these are process globals that outlive any one measurement,
/// so an absolute reading without a reset mostly reports whatever earlier
/// work happened to run first.
///
/// **This module's own `TEST_LOCK` only serialises tests *within this
/// module* against each other — it cannot, being module-private, protect
/// against a concurrently-running test in a *different* module that also
/// drives [`crate::random_tick::propagate_and_react`] while the
/// `redstone-counters` feature is on.** Measured: running a counters
/// measurement under `--test-threads=2` alongside an unrelated piston
/// fixture in `random_tick::tests` moved `notifications_issued`/`cell_reads`
/// run to run (659/667/674 across three runs) purely from the other test's
/// own notifications landing on the same static atomics; the identical
/// fixture run alone reproduces `659` exactly every time. **Any reading from
/// this module should be taken with `--test-threads=1` or a test filter
/// narrow enough to exclude every other `propagate_and_react` caller** —
/// see `docs/plans/redstone-execution-model.md` §9 for the full account.
#[inline]
pub fn reset() {
    imp::reset();
}

/// Reads every counter without resetting them.
#[inline]
#[must_use]
pub fn snapshot() -> Snapshot {
    imp::snapshot()
}

#[inline]
pub(crate) fn bump_notification() {
    imp::bump_notification();
}

#[inline]
pub(crate) fn bump_reaction(kind: ReactionKind) {
    imp::bump_reaction(kind);
}

#[inline]
pub(crate) fn bump_cell_read() {
    imp::bump_cell_read();
}

#[inline]
pub(crate) fn bump_state_parse() {
    imp::bump_state_parse();
}

#[inline]
pub(crate) fn bump_signal_query() {
    imp::bump_signal_query();
}

#[inline]
pub(crate) fn bump_wire_recompute() {
    imp::bump_wire_recompute();
}

#[inline]
pub(crate) fn bump_schedule_requested() {
    imp::bump_schedule_requested();
}

#[inline]
pub(crate) fn bump_schedule_deduped() {
    imp::bump_schedule_deduped();
}

/// Marks the start of one `propagate_and_react` call — the "drain" this
/// module's [`Snapshot::max_notifications_per_drain`] measures the peak
/// notification count within. Pairs with [`end_drain`].
#[inline]
pub(crate) fn begin_drain() {
    imp::begin_drain();
}

/// Marks the end of one `propagate_and_react` call, folding this drain's own
/// notification count into the running max.
#[inline]
pub(crate) fn end_drain() {
    imp::end_drain();
}

#[cfg(feature = "redstone-counters")]
mod imp {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use super::{REACTION_KIND_COUNT, ReactionKind, Snapshot};

    struct Counters {
        notifications_issued: AtomicU64,
        reactions_dispatched: [AtomicU64; REACTION_KIND_COUNT],
        cell_reads: AtomicU64,
        state_parses: AtomicU64,
        signal_queries: AtomicU64,
        wire_recomputes: AtomicU64,
        schedules_requested: AtomicU64,
        schedules_deduped: AtomicU64,
        max_notifications_per_drain: AtomicU64,
    }

    static C: Counters = Counters {
        notifications_issued: AtomicU64::new(0),
        reactions_dispatched: [const { AtomicU64::new(0) }; REACTION_KIND_COUNT],
        cell_reads: AtomicU64::new(0),
        state_parses: AtomicU64::new(0),
        signal_queries: AtomicU64::new(0),
        wire_recomputes: AtomicU64::new(0),
        schedules_requested: AtomicU64::new(0),
        schedules_deduped: AtomicU64::new(0),
        max_notifications_per_drain: AtomicU64::new(0),
    };

    thread_local! {
        /// This thread's notification count for the `propagate_and_react`
        /// call currently in flight. `propagate_and_react` never recurses
        /// into itself (a dust re-notification goes back through the
        /// `NeighborPropagator`'s own queue, not a nested call), so a single
        /// cell — not a stack — is enough.
        static DRAIN_COUNT: Cell<u64> = const { Cell::new(0) };
    }

    #[inline]
    fn bump(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }

    #[inline]
    pub fn bump_notification() {
        bump(&C.notifications_issued);
        DRAIN_COUNT.with(|c| c.set(c.get() + 1));
    }

    #[inline]
    pub fn bump_reaction(kind: ReactionKind) {
        bump(&C.reactions_dispatched[kind as usize]);
    }

    #[inline]
    pub fn bump_cell_read() {
        bump(&C.cell_reads);
    }

    #[inline]
    pub fn bump_state_parse() {
        bump(&C.state_parses);
    }

    #[inline]
    pub fn bump_signal_query() {
        bump(&C.signal_queries);
    }

    #[inline]
    pub fn bump_wire_recompute() {
        bump(&C.wire_recomputes);
    }

    #[inline]
    pub fn bump_schedule_requested() {
        bump(&C.schedules_requested);
    }

    #[inline]
    pub fn bump_schedule_deduped() {
        bump(&C.schedules_deduped);
    }

    #[inline]
    pub fn begin_drain() {
        DRAIN_COUNT.with(|c| c.set(0));
    }

    #[inline]
    pub fn end_drain() {
        let this_drain = DRAIN_COUNT.with(Cell::get);
        // `fetch_max` rather than a read-compare-write: this module's own
        // counters are process-global, so two threads (this crate has no
        // parallel redstone dispatch today, but the primitive should not
        // assume that) ending a drain at the same time must not lose the
        // larger of the two to a race.
        C.max_notifications_per_drain.fetch_max(this_drain, Relaxed);
    }

    pub fn reset() {
        C.notifications_issued.store(0, Relaxed);
        for slot in &C.reactions_dispatched {
            slot.store(0, Relaxed);
        }
        C.cell_reads.store(0, Relaxed);
        C.state_parses.store(0, Relaxed);
        C.signal_queries.store(0, Relaxed);
        C.wire_recomputes.store(0, Relaxed);
        C.schedules_requested.store(0, Relaxed);
        C.schedules_deduped.store(0, Relaxed);
        C.max_notifications_per_drain.store(0, Relaxed);
        DRAIN_COUNT.with(|c| c.set(0));
    }

    pub fn snapshot() -> Snapshot {
        Snapshot {
            notifications_issued: C.notifications_issued.load(Relaxed),
            reactions_dispatched: std::array::from_fn(|i| C.reactions_dispatched[i].load(Relaxed)),
            cell_reads: C.cell_reads.load(Relaxed),
            state_parses: C.state_parses.load(Relaxed),
            signal_queries: C.signal_queries.load(Relaxed),
            wire_recomputes: C.wire_recomputes.load(Relaxed),
            schedules_requested: C.schedules_requested.load(Relaxed),
            schedules_deduped: C.schedules_deduped.load(Relaxed),
            max_notifications_per_drain: C.max_notifications_per_drain.load(Relaxed),
        }
    }
}

#[cfg(not(feature = "redstone-counters"))]
mod imp {
    use super::{ReactionKind, Snapshot};

    #[inline(always)]
    pub fn bump_notification() {}
    #[inline(always)]
    pub fn bump_reaction(_kind: ReactionKind) {}
    #[inline(always)]
    pub fn bump_cell_read() {}
    #[inline(always)]
    pub fn bump_state_parse() {}
    #[inline(always)]
    pub fn bump_signal_query() {}
    #[inline(always)]
    pub fn bump_wire_recompute() {}
    #[inline(always)]
    pub fn bump_schedule_requested() {}
    #[inline(always)]
    pub fn bump_schedule_deduped() {}
    #[inline(always)]
    pub fn begin_drain() {}
    #[inline(always)]
    pub fn end_drain() {}
    #[inline(always)]
    pub fn reset() {}
    #[inline(always)]
    pub fn snapshot() -> Snapshot {
        Snapshot::default()
    }
}

#[cfg(all(test, feature = "redstone-counters"))]
mod tests {
    use super::*;
    use crate::chunk::ChunkColumn;
    use crate::random_tick::propagate_and_react;
    use crate::scheduled_tick::ScheduledTickQueue;
    use crate::{redstone, redstone_torch, redstone_wire};

    /// The counters this module measures are **process-global**, by design
    /// (see the module doc) — so `cargo test`'s default parallel test
    /// threads racing `reset()`/`propagate_and_react()`/`snapshot()` across
    /// different `#[test]` functions in this module would attribute one
    /// test's activity to another's reading. Every test here holds this for
    /// its entire `reset(); work(); snapshot()` measurement, serialising them
    /// against each other without needing `--test-threads=1` for the whole
    /// binary.
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;
    const FLOOR_Y: i32 = 0;
    const Y: i32 = 1;
    const ROW_Z: i32 = 8;
    const NOW: u64 = 1_000;

    fn at(column: &ChunkColumn, x: i32, y: i32, z: i32) -> String {
        column.block_state(x, y, z).to_string()
    }

    fn column_with_floor() -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block(x, FLOOR_Y, z, "minecraft:stone");
            }
        }
        column
    }

    /// **The null-contraption control.** `docs/plans/redstone-execution-model.md`
    /// §6 predicts this must already hold, by construction, with no rework: a
    /// lit torch feeding already-settled dust, ticked with a notification at
    /// a position **nothing reads from**, costs zero on every counter. This is
    /// the tripwire for accidental de-incrementalisation the plan names,
    /// proven rather than assumed — a control that is merely described is not
    /// a control.
    #[test]
    fn a_settled_circuit_notified_at_an_unrelated_cell_costs_every_counter_zero() {
        let mut column = column_with_floor();
        column.set_block(1, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
        // Already at the settled power a lit torch at x=1 drives: real
        // steady state, not merely "some value" — a wrong settled value would
        // make this control pass by accident (every cell would still change
        // once, the same defect a stale expectation would produce).
        column.set_block(2, Y, ROW_Z, &redstone_wire::set_power(15));

        let _guard = TEST_LOCK.lock().unwrap();
        reset();
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // Notified at a position with nothing on it and nothing adjacent to
        // it — `(10, Y, ROW_Z)` is six cells from the dust run above and nine
        // from the torch, so its own one-layer fan-out cannot reach either.
        let events = propagate_and_react(&mut column, 0, 0, 10, Y, ROW_Z, &mut block_ticks, NOW);
        let snap = snapshot();

        assert!(events.is_empty(), "an air cell must produce no events, got {events:?}");
        assert_eq!(snap.reactions_total(), 0, "no reaction may fire for air: {snap:?}");
        assert_eq!(snap.cell_reads, 0, "an untouched region must read no cells: {snap:?}");
        assert_eq!(snap.state_parses, 0, "{snap:?}");
        assert_eq!(snap.signal_queries, 0, "{snap:?}");
        assert_eq!(snap.wire_recomputes, 0, "{snap:?}");
        assert_eq!(snap.schedules_requested, 0, "{snap:?}");
        // `notifications_issued` is the one counter genuinely allowed to be
        // non-zero here: `propagate_and_react` still visits the origin's own
        // one-layer fan-out (six neighbours) even though air answers every
        // one of them with "not a redstone family" and returns immediately.
        // That is the premise check below, not a defect in this control.
        assert!(
            snap.notifications_issued > 0,
            "PREMISE: the propagator must still have visited the origin's neighbours, or this \
             control proves nothing about idle *reactions* specifically"
        );
    }

    /// **A control on the control above.** The identical rig, notified at the
    /// torch's own attachment position instead of an unrelated cell, must
    /// move every counter the previous test required to be zero — proving
    /// the previous test's zeros were "nothing to do" and not "the counters
    /// do not work".
    #[test]
    fn the_same_circuit_notified_at_the_dust_moves_every_counter_the_control_required_zero() {
        let mut column = column_with_floor();
        column.set_block(1, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
        // Deliberately UNSETTLED here (power 0), so the notification at the
        // dust has real work to do — re-deriving 15 from the adjacent lit
        // torch and re-fanning-out, matching `redstone_oracle_gate.rs`'s own
        // rig shape for the same reason its own doc gives.
        column.set_block(2, Y, ROW_Z, &redstone_wire::set_power(0));

        let _guard = TEST_LOCK.lock().unwrap();
        reset();
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = propagate_and_react(&mut column, 0, 0, 2, Y, ROW_Z, &mut block_ticks, NOW);
        let snap = snapshot();

        assert!(!events.is_empty(), "CONTROL FAILED: the dust must actually re-power");
        assert!(snap.reactions_total() > 0, "CONTROL FAILED: {snap:?}");
        assert!(snap.cell_reads > 0, "CONTROL FAILED: {snap:?}");
        assert!(snap.wire_recomputes > 0, "CONTROL FAILED: {snap:?}");
    }

    /// **Reading 3's hand-derived prediction**, one dust cell rather than a
    /// long run.
    ///
    /// # Why one cell, not fifteen
    ///
    /// The first version of this test predicted `wire_recomputes == 15` for a
    /// 15-long run — "one settle per cell, no repeat visits" — and measured
    /// 145. That was exactly `CLAUDE.md`'s "do not predict the plausible
    /// round number" trap: `wire_update_fan_out`'s own seven-centre,
    /// six-direction shape (`docs/redstone.md`'s "Neighbour-update cascade"
    /// section) means a single dust power change re-notifies its own
    /// position from **six separate directions** (the six satellite centres
    /// each aim one of their own six neighbours back at the origin), so one
    /// settle costs 1 initial recompute **plus 6 revisits that recompute
    /// again and find no further change** — 7, not 1 — before the geometry
    /// even reaches a second cell. A 15-long run compounds this at every
    /// step the wavefront advances, which is exactly why 145 (not a multiple
    /// of 15) was the honest number rather than a bug: the counter caught the
    /// prediction being wrong, which is what it is for.
    ///
    /// # The one-cell derivation
    ///
    /// Rig: a lit torch at `x=0`, one unpowered dust cell at `x=1`, nothing
    /// else reactive within the fan-out's reach (`x=2` stays air). Traced by
    /// hand against [`crate::neighbor_update::NeighborPropagator::propagate`]'s
    /// documented depth-first, uncapped, no-dedup semantics:
    ///
    /// 1. `propagate_and_react`'s own origin fan-out (the torch's six
    ///    neighbours) reaches the dust once, at `East`. `is_wire` is true:
    ///    **recompute #1**. Power goes `0 -> 15`, which *changes*, so this
    ///    returns `wire_update_fan_out((1, Y, ROW_Z))` — the 7-centre,
    ///    6-direction, 42-notification cascade — as this notification's own
    ///    resolved-before-continuing cascade.
    /// 2. Of those 42, exactly seven land on a reactive block: the centre at
    ///    the dust's own position aims `West` at the torch (a torch hit, not
    ///    a recompute), and each of the six *satellite* centres
    ///    (`West`/`East`/`Down`/`Up`/`North`/`South` of the dust) has exactly
    ///    one of its own six directions aimed back at the dust's position —
    ///    **six revisits, six more recomputes** (#2–#7). Power is already 15
    ///    on each, so `calculate_target_strength` returns 15 again — no
    ///    change, so none of these six re-triggers a further cascade.
    ///
    /// Total: `1 + 6 = 7`.
    #[test]
    fn wire_recomputes_matches_the_hand_derived_count_for_a_single_settling_cell() {
        let mut column = column_with_floor();
        column.set_block(0, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
        column.set_block(1, Y, ROW_Z, &redstone_wire::set_power(0));
        // Deliberately left air: a second dust cell here would let the
        // cascade reach a cell this derivation does not account for.

        let _guard = TEST_LOCK.lock().unwrap();
        reset();
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = propagate_and_react(&mut column, 0, 0, 0, Y, ROW_Z, &mut block_ticks, NOW);
        let snap = snapshot();

        assert_eq!(
            redstone::wire_power(&at(&column, 1, Y, ROW_Z)),
            15,
            "PREMISE: the single dust cell must actually settle to 15, or the derivation above \
             (which assumes exactly one power change) does not apply"
        );
        assert_eq!(
            events.iter().filter(|e| e.pos == (1, Y, ROW_Z)).count(),
            1,
            "PREMISE: the dust cell's power must change exactly once (0 -> 15), not settle in \
             multiple published steps, or the derivation's 'no further cascade' step is wrong"
        );

        let predicted = 7u64;
        assert_eq!(
            snap.wire_recomputes, predicted,
            "PREDICTED {predicted} wire_recomputes by hand (1 initial settle + 6 revisits from \
             wire_update_fan_out's seven-centre cascade, each finding no further change), \
             MEASURED {} — see this test's own doc comment for the derivation a mismatch means \
             is wrong",
            snap.wire_recomputes
        );
    }

    /// **A "large" contraption in this file's own vocabulary**: a 15-long
    /// dust run lit from one end — `MAX_PUSH_DEPTH`-scale, the largest single
    /// number named anywhere in this crate's redstone family, used here as
    /// the "large" yardstick until a real community-contraption corpus
    /// exists (`docs/plans/redstone-execution-model.md`'s U6/U0). This
    /// answers §9's still-open question ("the actual cost split") with real
    /// counters rather than leaving it unmeasured.
    ///
    /// **Deliberately not a gate.** The single-cell test above already shows
    /// that a naive prediction at this scale is exactly the trap
    /// `CLAUDE.md` warns about — the first version of *that* test predicted
    /// `wire_recomputes == 15` and measured 145, because one settle's own
    /// 7-centre fan-out compounds at every step a longer run advances.
    /// Hand-deriving the exact 15-cell number would mean re-deriving that
    /// compounding by hand fifteen times over, which is worth doing only if
    /// a future change needs this exact fixture to gate on. Until then this
    /// records real measurements and asserts only invariants that hold
    /// regardless of the exact compounding.
    #[test]
    fn measured_cost_split_for_a_fifteen_cell_dust_run() {
        const RUN_LEN: i32 = 15;
        let mut column = column_with_floor();
        column.set_block(0, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
        for x in 1..=RUN_LEN {
            column.set_block(x, Y, ROW_Z, &redstone_wire::set_power(0));
        }

        let _guard = TEST_LOCK.lock().unwrap();
        reset();
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = propagate_and_react(&mut column, 0, 0, 1, Y, ROW_Z, &mut block_ticks, NOW);
        let snap = snapshot();

        // PREMISE: the whole run must actually settle end to end, or this
        // measures an aborted cascade rather than a real 15-cell contraption.
        let far_power = redstone::wire_power(&at(&column, RUN_LEN, Y, ROW_Z));
        assert!(
            far_power > 0,
            "PREMISE FAILED: the far end of a 15-cell run from a lit torch must carry non-zero \
             power, or this is not measuring a settled 15-cell cascade. Got {far_power}"
        );
        assert!(!events.is_empty(), "PREMISE FAILED: the run must actually change something");

        // Structural invariants that hold regardless of the exact compounding
        // — see this test's own doc comment for why an exact count is not
        // asserted at this length.
        assert!(snap.notifications_issued > 0, "{snap:?}");
        assert!(snap.wire_recomputes > 0, "{snap:?}");
        // Every notification reads at least its own cell before dispatching
        // — Layer A's whole target (§1.2) is shrinking this cost, not
        // eliminating the read.
        assert!(
            snap.cell_reads >= snap.notifications_issued,
            "cell_reads ({}) must be at least notifications_issued ({}) — every notification \
             reads its own cell before dispatching",
            snap.cell_reads,
            snap.notifications_issued
        );

        // Not asserted, only recorded: the actual cost split. Run with
        // `--features redstone-counters -- --nocapture` to read it.
        eprintln!(
            "measured 15-cell dust run: notifications_issued={} reactions_total={} \
             cell_reads={} state_parses={} signal_queries={} wire_recomputes={} \
             schedules_requested={} schedules_deduped={} max_notifications_per_drain={} \
             -- cell_reads/notification={:.2} state_parses/notification={:.2} \
             signal_queries/notification={:.2}",
            snap.notifications_issued,
            snap.reactions_total(),
            snap.cell_reads,
            snap.state_parses,
            snap.signal_queries,
            snap.wire_recomputes,
            snap.schedules_requested,
            snap.schedules_deduped,
            snap.max_notifications_per_drain,
            snap.cell_reads as f64 / snap.notifications_issued as f64,
            snap.state_parses as f64 / snap.notifications_issued as f64,
            snap.signal_queries as f64 / snap.notifications_issued as f64,
        );
    }
}
