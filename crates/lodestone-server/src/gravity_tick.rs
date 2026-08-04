//! Gravity blocks (issue #311): sand/gravel falling when the block below is
//! removed — this landing's whole point is that it is the **first real
//! production caller** of `crate::neighbor_update::NeighborPropagator`
//! (issue #308), which had exactly zero until now (see that module's own
//! doc comment, and `docs/tick-scheduling.md`'s "what this module does not
//! yet have a real producer for").
//!
//! # Cited directly
//!
//! `FallingBlock.onPlace`/`updateShape`/`tick` (`FallingBlock.java:28-54`):
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
//! `getDelayAfterPlace` defaults to `2` (`FallingBlock.java:59-61`). Note
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
//! **No 2-tick scheduled delay.** `ScheduledTickQueue`'s drain dispatch
//! lives in `tick.rs`'s per-tick loop, a file this task's ownership split
//! does not let this landing edit directly (see `docs/tick-scheduling.md`'s
//! own note on brokered files, and this crate's `CLAUDE.md`). Rather than
//! leave gravity blocks as an island until that edit lands, this module's
//! settle step runs **synchronously**, inside the very
//! `NeighborPropagator::propagate` call the triggering mutation already
//! makes (see `crate::random_tick`'s `propagate_and_react` — renamed from
//! `propagate_and_settle_gravity` when issue #314's redstone family became
//! this call site's second reaction) — an immediate settle instead of
//! vanilla's 2-tick delay. `block_ticks` is no longer empty as of #314: it
//! now has real producers (redstone torches/repeaters/comparators/observers
//! — see `crate::redstone_torch`/`crate::redstone_diode`/
//! `crate::redstone_observer`), but gravity's own settle still runs
//! synchronously rather than through that queue; nothing about #314's
//! landing required touching this module.
//!
//! # What actually triggers this today
//!
//! `crate::random_tick::RandomTickScheduler::tick_randomly_ticking_block`
//! calls `NeighborPropagator::propagate` on every position any of its four
//! mutation families (grass↔dirt, crop growth, sapling growth, leaf decay)
//! just changed — mirroring vanilla's `setBlockAndUpdate` always notifying
//! neighbours after a mutation. **This is a real trigger, but a narrow
//! one**: it fires only when one of *those* mutations happens to be
//! adjacent to an unsupported gravity block, not on every block change in
//! the world. The far more common real-world trigger — a player mining the
//! block a sand column rests on — is `server.rs`'s block-break handling,
//! off-limits to this task (owned by a concurrent agent wiring serverbound
//! decode arms) and not yet a `propagate` caller itself. Stated plainly,
//! per this repo's own "nothing is done until something on screen changes"
//! rule: the mechanism is real and reaches a client end to end today, on a
//! genuinely narrower trigger surface than vanilla's.

use crate::chunk::is_air_or_fluid;

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

/// `FallingBlock.isFree` (`FallingBlock.java:63-65`), narrowed to what this
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
