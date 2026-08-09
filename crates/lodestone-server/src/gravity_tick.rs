//! Gravity blocks (issue #311): sand/gravel falling when the block below is
//! removed — this landing's whole point is that it is the **first real
//! production caller** of `crate::neighbor_update::NeighborPropagator`
//! (issue #308), which had exactly zero until now (see that module's own
//! doc comment, and `docs/tick-scheduling.md`'s "what this module does not
//! yet have a real producer for").
//!
//! # Cited directly
//!
//! `FallingBlock.onPlace`/`updateShape`/`tick`:
//!
//! ```text
//! protected void onPlace(BlockState state, Level level, BlockPos pos, BlockState oldState, boolean movedByPiston) {
//!    level.scheduleTick(pos, this, this.getDelayAfterPlace());
//! }
//! protected BlockState updateShape(..., BlockPos pos, ..., BlockPos neighbourPos, BlockState neighbourState, ...) {
//!    ticks.scheduleTick(pos, this, this.getDelayAfterPlace());
//!    return super.updateShape(...);
//! }
//! protected void tick(BlockState state, ServerLevel level, BlockPos pos, RandomSource random) {
//!    if (isFree(level.getBlockState(pos.below())) && pos.getY() >= level.getMinY()) {
//!       FallingBlockEntity entity = FallingBlockEntity.fall(level, pos, state);
//!       this.falling(entity);
//!    }
//! }
//! public static boolean isFree(BlockState state) {
//!    return state.isAir() || state.is(BlockTags.FIRE) || state.liquid() || state.canBeReplaced();
//! }
//! ```
//!
//! `getDelayAfterPlace` defaults to `2` (`FallingBlock.getDelayAfterPlace`). Note
//! `tick()` itself draws **zero** RNG values — the eligibility check
//! (`isFree(below)`) is entirely deterministic, unlike every random-tick
//! family in `crate::random_tick`/`crate::growth_tick`. `sand`/`red_sand`
//! (`SandBlock`) and `gravel` (`GravelBlock`) are plain `FallingBlock`
//! subclasses with no override of any of the above.
//!
//! # Two named deviations from the jar
//!
//! **No `FallingBlockEntity`.** Vanilla's `tick()` spawns a temporary entity
//! that free-falls with real physics (accelerating, sub-block positions)
//! and only becomes a real block again on landing — a smooth multi-tick
//! animation. This crate has no free-entity-simulation seam for a falling
//! block (`crate::mobs`/`crate::block_entities` checked first, per this
//! issue's own comment; neither fits a block-shaped temporary entity), so
//! the block instead moves **directly**: this module computes the exact
//! landing `y` and the caller writes the block there in one step, skipping
//! the entity phase entirely. Visually this is "sand vanishes and
//! reappears lower," not "sand drops smoothly" — a real, named
//! simplification, not a hidden one.
//!
//! **The 2-tick scheduled delay is now real — this deviation is closed.**
//! [`ticks_after_place`] schedules [`TICK_GRAVITY`] at the placed position with
//! [`DELAY_AFTER_PLACE`], which is `FallingBlock.onPlace`'s
//! `level.scheduleTick(pos, this, this.getDelayAfterPlace())` and nothing more.
//! `tick.rs`'s scheduled-tick drain dispatches it to
//! `crate::random_tick::settle_gravity_at` **at the tick's own position**, which
//! is `FallingBlock.tick`'s `isFree(below)` check verbatim.
//!
//! The position matters and is the trap this arm had to avoid: the settle could
//! not simply be routed through `propagate_and_react` like every other reaction,
//! because `NeighborPropagator::propagate(origin)` notifies the origin's **six
//! neighbours and not the origin**. Vanilla's `onPlace` tick fires on the placed
//! block itself, so a propagate-based arm would have settled the sand's
//! neighbours and left the sand hanging — the same symptom, with a scheduled
//! tick that looked like it was working.
//!
//! # What triggers this today
//!
//! Two producers, and the second is the one the owner reported missing.
//!
//! 1. **A neighbour mutation.**
//!    `crate::random_tick::RandomTickScheduler::tick_randomly_ticking_block`
//!    calls `NeighborPropagator::propagate` on every position any of its
//!    mutation families just changed, mirroring vanilla's `setBlockAndUpdate`
//!    always notifying neighbours. Narrow: it fires only when one of *those*
//!    mutations lands next to an unsupported gravity block.
//! 2. **The placement itself**, via [`ticks_after_place`] from
//!    `server.rs`'s `apply_use_item_on`. Until this existed, *"they don't fall
//!    when I place them in the air, they only fall when I place another block
//!    beside them"* was the exact and complete description of what the code did:
//!    only a neighbour update could reach the check, so placing sand in mid-air
//!    left it hanging until something else happened next to it.
//!
//! # What is still missing: the entity, and therefore the animation
//!
//! **No `FallingBlockEntity`.** Vanilla's `tick()` spawns a temporary entity that
//! free-falls with real physics — gravity `0.04`, air drag `0.98`, hitbox
//! `0.98 × 0.98` (`FallingBlockEntity.getDefaultGravity`, `Entity.getAirDrag`,
//! `EntityTypes.FALLING_BLOCK`) — and only becomes a real block again on landing.
//! This crate instead computes the landing `y` and writes the block there in one
//! step ([`find_landing_y`]), so the block *teleports*: "sand vanishes and
//! reappears lower", not "sand drops smoothly". A real, named simplification.
//!
//! Closing it is **not** a change to this module. It needs three things this
//! module cannot reach: a server-side entity that ticks and is broadcast
//! (`ADD_ENTITY`, position updates, `REMOVE_ENTITIES`), a per-tick physics step in
//! the tick loop, and a client-side renderer for a block-shaped entity. The
//! recurrence to port, for whoever does, is `applyGravity` then `move` then a
//! trailing `delta *= 0.98`, i.e. `v_n = 0.98 * v_(n-1) - 0.04` with the
//! displacement taken *before* the drag — which is why the first tick moves
//! `0.04` and not `0.0392`.

use lodestone_model::BlockPos;

use crate::chunk::is_air_or_fluid;
use crate::scheduled_tick::{ScheduledTick, ScheduledTickQueue, TickPriority};

pub const SAND: &str = "minecraft:sand";
pub const RED_SAND: &str = "minecraft:red_sand";
pub const GRAVEL: &str = "minecraft:gravel";

/// `true` for a plain-`FallingBlock` base name this crate models. Vanilla
/// also has `ConcretePowderBlock`/`AnvilBlock`/`PointedDripstoneBlock` as
/// `FallingBlock` subclasses; not covered here (none appear in this crate's
/// worldgen — see `crate::chunk`'s module doc — so extending this table has
/// no way to be exercised end to end yet, the same reasoning
/// `crate::growth_tick` gives for not inventing tree placement).
#[must_use]
pub fn is_gravity_block(base: &str) -> bool {
    matches!(base, SAND | RED_SAND | GRAVEL)
}

/// `FallingBlock.isFree`, narrowed to what this
/// crate can check. `state.isAir() || state.liquid()` maps directly onto
/// `crate::chunk::is_air_or_fluid`. `state.is(BlockTags.FIRE)` and
/// `state.canBeReplaced()` are not modeled: this crate has no fire block
/// yet (issue #312, not landed) and no generic "can be replaced" predicate
/// beyond `is_air_or_fluid` itself (`crate::chunk`'s own doc comment: that
/// function already *is* this crate's "can a placement replace this cell"
/// test) — so the two disjuncts this crate has are the whole set it can
/// evaluate, not an arbitrarily narrowed subset.
#[must_use]
pub fn is_free(state: &str) -> bool {
    is_air_or_fluid(state)
}

/// Scans downward from `start_y` (exclusive) for the first non-free
/// position, using `is_free_below` to test each candidate `y`, and returns
/// the `y` the block should come to rest at — one above the first obstacle,
/// or `min_y` if the whole column below is free. `is_free_below` is a
/// closure rather than a direct `ChunkColumn` reference so this stays a
/// pure function with no world type in scope, exactly like
/// `crate::growth_tick`'s decision functions.
///
/// This is the one-shot replacement for vanilla's multi-tick physics fall —
/// see this module's own doc comment for why a single computed landing
/// position, rather than an animated descent, is what this crate models.
#[must_use]
pub fn find_landing_y(mut is_free_below: impl FnMut(i32) -> bool, start_y: i32, min_y: i32) -> i32 {
    let mut y = start_y;
    while y > min_y && is_free_below(y - 1) {
        y -= 1;
    }
    y
}

/// The scheduled-tick kind a placed gravity block waits on — `FallingBlock`'s own
/// entry in vanilla's `blockTicks` queue, dispatched by `tick.rs`'s drain.
///
/// A distinct kind rather than reusing a redstone or fluid one because
/// `ScheduledTickQueue` deduplicates on `(pos, kind)`: sharing a kind would let a
/// fluid tick at the same cell swallow the gravity check, or the reverse.
pub const TICK_GRAVITY: &str = "gravity";

/// `FallingBlock.getDelayAfterPlace`, which is `2`.
///
/// Not "a couple of ticks": the value is what makes the delay observable as a
/// delay. At 20 ticks per second a placed sand block hangs for 100 ms and then
/// falls, which is the pause vanilla has and an immediate settle does not.
pub const DELAY_AFTER_PLACE: u64 = 2;

/// `FallingBlock.onPlace`: the scheduled tick a gravity block owes itself the
/// moment it is placed.
///
/// Empty for anything [`is_gravity_block`] rejects, so the caller needs no guard —
/// the same shape as `crate::fluid::ticks_after_edit`, and it is requested from the
/// same place for the same reason.
///
/// `trigger_tick` is [`DELAY_AFTER_PLACE`] as a **relative** delay, matching
/// `BlockTickFeed`'s pending lane, which rebases onto the tick loop's counter.
/// Built through a real queue rather than a struct literal because
/// `ScheduledTick::sub_tick_order` is private — again the idiom
/// `crate::fluid::ticks_after_edit` established.
///
/// `TickPriority::Normal` is vanilla's: `scheduleTick(pos, block, delay)` with no
/// priority argument resolves to `TickPriority.NORMAL`.
#[must_use]
pub fn ticks_after_place(pos: BlockPos, state: &str) -> Vec<ScheduledTick<String>> {
    let base = state.split('[').next().unwrap_or(state);
    if !is_gravity_block(base) {
        return Vec::new();
    }
    let mut pending: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    pending.schedule(
        (pos.x, pos.y, pos.z),
        TICK_GRAVITY.to_owned(),
        DELAY_AFTER_PLACE,
        TickPriority::Normal,
    );
    pending.drain_due(u64::MAX, usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sand_red_sand_and_gravel_are_gravity_blocks() {
        assert!(is_gravity_block(SAND));
        assert!(is_gravity_block(RED_SAND));
        assert!(is_gravity_block(GRAVEL));
    }

    #[test]
    fn stone_and_dirt_are_not_gravity_blocks() {
        assert!(!is_gravity_block("minecraft:stone"));
        assert!(!is_gravity_block("minecraft:dirt"));
    }

    #[test]
    fn air_and_water_are_free() {
        assert!(is_free("minecraft:air"));
        assert!(is_free("minecraft:cave_air"));
        assert!(is_free("minecraft:water[level=0]"));
    }

    #[test]
    fn solid_stone_is_not_free() {
        assert!(!is_free("minecraft:stone"));
        assert!(!is_free("minecraft:gravel"));
    }

    /// Predicted value: a column of pure air below `start_y` all the way to
    /// `min_y` lands exactly at `min_y`, not one above it and not below it —
    /// a magnitude check on the boundary, not just "it went down".
    #[test]
    fn falls_all_the_way_to_min_y_when_the_whole_column_is_free() {
        let landing = find_landing_y(|_y| true, 50, -64);
        assert_eq!(landing, -64);
    }

    /// Predicted value: solid support at y=10 (i.e. `is_free_below(10)` is
    /// false) stops the fall at y=11, one above the obstacle.
    #[test]
    fn lands_exactly_one_above_the_first_obstacle() {
        let landing = find_landing_y(|y| y != 10, 50, -64);
        assert_eq!(landing, 11);
    }

    /// Negative control: if the block is already resting on something
    /// (`is_free_below(start_y - 1)` is false from the first call), the
    /// landing position must equal `start_y` itself — no motion at all.
    #[test]
    fn already_supported_does_not_move() {
        let landing = find_landing_y(|_y| false, 50, -64);
        assert_eq!(landing, 50);
    }

    /// Placing a gravity block schedules exactly one tick, at its own position,
    /// on the tick **two** after the placement.
    ///
    /// The tick number is the predicted value and the two candidate readings are
    /// evaluated rather than assumed: `getDelayAfterPlace` is `2`, so a placement
    /// resolved on tick `T` fires on `T + 2` — never `T + 1` (an off-by-one that
    /// still "eventually falls" and so passes any direction-only assertion) and
    /// never `T` (the immediate settle this replaced, which is what made the fall
    /// instantaneous). Asserted on the relative delay because that is what
    /// `BlockTickFeed`'s pending lane carries.
    ///
    /// The **position** assertion is the load-bearing one. Vanilla's `onPlace`
    /// tick fires on the placed block itself, and scheduling it on a neighbour
    /// instead would settle the wrong cell while looking entirely correct in a
    /// queue dump — see this module's doc comment for why the propagate-based
    /// route, which notifies the six neighbours and not the origin, could not be
    /// used here.
    #[test]
    fn placing_a_gravity_block_schedules_one_tick_at_its_own_position_two_ticks_out() {
        let pos = BlockPos::new(12, 70, -5);
        let scheduled = ticks_after_place(pos, SAND);

        assert_eq!(scheduled.len(), 1, "one tick, not one per neighbour");
        let tick = &scheduled[0];
        assert_eq!(
            tick.pos,
            (12, 70, -5),
            "the tick belongs to the placed block, not to a neighbour"
        );
        assert_eq!(tick.kind, TICK_GRAVITY);
        assert_eq!(
            tick.trigger_tick, 2,
            "getDelayAfterPlace is 2: not 1, and not 0 (the immediate settle this replaced)"
        );
        assert_eq!(DELAY_AFTER_PLACE, 2, "the constant this predicts is vanilla's");
    }

    /// A state string with properties still resolves, and a non-gravity placement
    /// schedules nothing at all.
    ///
    /// The property case is the discriminating input: a placement always arrives as
    /// the block's real state, so a predicate matching the whole string against
    /// `"minecraft:sand"` would schedule nothing for a state that carries any
    /// property — and gravel and sand do reach placement with suffixes. `red_sand`
    /// is included because it is the family member most likely to be forgotten.
    #[test]
    fn only_gravity_blocks_schedule_and_a_property_suffix_does_not_defeat_it() {
        for state in [SAND, RED_SAND, GRAVEL, "minecraft:sand[some=prop]"] {
            assert_eq!(
                ticks_after_place(BlockPos::new(0, 64, 0), state).len(),
                1,
                "{state} is a gravity block and must schedule its own onPlace tick"
            );
        }
        for state in [
            "minecraft:stone",
            "minecraft:torch",
            "minecraft:air",
            // A `FallingBlock` subclass this crate deliberately does not model —
            // see `is_gravity_block`. Listed so widening the table is a visible
            // decision rather than an accident.
            "minecraft:anvil",
            "minecraft:white_concrete_powder",
        ] {
            assert!(
                ticks_after_place(BlockPos::new(0, 64, 0), state).is_empty(),
                "{state} must schedule nothing"
            );
        }
    }

    /// Coverage/magnitude control distinguishing this from a function that
    /// always returns `min_y` regardless of input (which would also pass
    /// the "falls all the way" test above) — an obstacle much higher than
    /// `min_y` must be respected, not overridden by the floor.
    #[test]
    fn an_obstacle_far_above_min_y_is_not_overridden_by_the_floor() {
        let landing = find_landing_y(|y| y != 40, 50, -64);
        assert_eq!(landing, 41, "control failed: the function must stop at the real obstacle, not fall through to min_y");
    }
}
