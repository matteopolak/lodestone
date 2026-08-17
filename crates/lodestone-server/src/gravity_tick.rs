//! Gravity blocks: sand/gravel falling when the block below is
//! removed — this landing's whole point is that it is the **first real
//! production caller** of `crate::neighbor_update::NeighborPropagator`,
//! which had exactly zero until now (see that module's own
//! doc comment, and `docs/tick-scheduling.md`'s "what this module does not
//! yet have a real producer for").
//!
//! # Cited directly
//!
//! `FallingBlock.onPlace`/`updateShape`/`tick`:
//!
//! (Line numbers deliberately dropped from these citations — a class-and-method
//! name is just as findable in `.cache/mc/26.2/src` and does not rot when the
//! cache is re-extracted.)
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
//! # Deviations from the jar, and the two that are now closed
//!
//! **The `FallingBlockEntity` is now real — this deviation is closed.**
//! [`FallingBlockMotion`] is the port of `FallingBlockEntity.tick`'s motion half,
//! `crate::mobs::MobSim`'s falling-block registry tracks and broadcasts one, and
//! `tick.rs`'s scheduled-tick drain is the single place one is created. The
//! one-shot [`find_landing_y`] is still here and still used, but only to resolve
//! *where* the entity will come to rest, not to teleport the block there.
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
//!    mutations lands next to an unsupported gravity block. This route now
//!    **schedules** [`TICK_GRAVITY`] rather than settling inline, which is what
//!    `FallingBlock.updateShape` does — it is `scheduleTick(pos, this,
//!    getDelayAfterPlace())` and nothing else. Settling inline was a second,
//!    undocumented fall path that skipped the delay *and* the entity; there is
//!    now exactly one place a block ever leaves the world for a fall, and it is
//!    the drain below.
//! 2. **The placement itself**, via [`ticks_after_place`] from
//!    `server.rs`'s `apply_use_item_on`. Until this existed, *"they don't fall
//!    when I place them in the air, they only fall when I place another block
//!    beside them"* was the exact and complete description of what the code did:
//!    only a neighbour update could reach the check, so placing sand in mid-air
//!    left it hanging until something else happened next to it.
//!
//! # The fall itself: `FallingBlockEntity.tick`'s motion half
//!
//! `FallingBlockEntity.tick` runs, in this order: `time++`, `applyGravity()`,
//! `move(SELF, getDeltaMovement())`, the landing decision, and — as the method's
//! **last** statement, after everything above —
//! `setDeltaMovement(getDeltaMovement().scale(getAirDrag()))`.
//!
//! `Entity.applyGravity` is `setDeltaMovement(getDeltaMovement().add(0, -gravity,
//! 0))` with `FallingBlockEntity.getDefaultGravity` = `0.04`, and
//! `Entity.getAirDrag` is `0.98F`. So the displacement taken in tick *n* is
//!
//! ```text
//! v_n = 0.98 * v_(n-1) - 0.04,   v_0 = 0     (setDeltaMovement(Vec3.ZERO))
//! ```
//!
//! **with the displacement applied before the drag.** That ordering is the whole
//! of [`fall_step`] and it is the part that is easy to get backwards: tick one
//! moves exactly `0.04`, *not* `0.0392`. `0.0392` is what a drag-first reading
//! (`v_n = 0.98 * (v_(n-1) - 0.04)`) produces, and the two differ by under 2%, so
//! no approximate assertion can separate them — which is why
//! [`fall_step`]'s own test predicts both and requires the wrong one to fail.
//!
//! One named numeric deviation: `getAirDrag` returns a **`float`** and
//! `Vec3.scale` takes a `double`, so the JVM widens `0.98F` to
//! `0.980000019073486…`. [`FALLING_BLOCK_AIR_DRAG`] is the exact decimal `0.98`,
//! matching `lodestone_entity::item_entity::ITEM_AIR_DRAG`, which is the same
//! choice already made for dropped items. The divergence is ~2e-9 blocks per
//! tick — five orders of magnitude below the `f64` position the wire carries and
//! well under a texel at any draw distance — and keeping one convention in the
//! tree is worth more than tracking it.
//!
//! # The three orderings that are load-bearing and invisible
//!
//! Everything about a falling block that a player can actually see is an
//! *ordering* fact, and each one fails in a direction that looks like something
//! else. [`FallingBlockEffect`] exists so all three are properties of a value
//! this module returns rather than of the order a caller happens to write two
//! statements in.
//!
//! 1. **Displacement before drag** — above.
//! 2. **Clear the origin cell before the entity is broadcast.**
//!    `FallingBlockEntity.fall` is `new FallingBlockEntity(...)`,
//!    `level.setBlock(pos, air, 3)`, *then* `level.addFreshEntity(entity)`. A
//!    client that sees the `ADD_ENTITY` first shows the block **and** the entity
//!    in the same cell for as long as the two packets are apart.
//! 3. **Place the landed block before discarding the entity.** The landing branch
//!    is `level.setBlock(pos, blockState, 3)`, then
//!    `sendToTrackingPlayers(ClientboundBlockUpdatePacket)`, then `discard()`. The
//!    reverse leaves the client with *neither* a block nor an entity — the shape
//!    that made the item-pickup animation invisible, where `take` had to precede
//!    `discard` for the same reason.
//!
//! Note this crate's transport does **not** guarantee (2) and (3) reach the
//! client in the order they are produced: `server.rs`'s connection loop drains
//! the block feed on its `container_sync_tick` arm and runs the entity streaming
//! pass on its `read_packet` arm, two different `select!` arms at ~50 ms each. So
//! the *server-side* order is exact and asserted here; the wire order is
//! within-one-tick and unspecified. Fixing that means giving the connection loop
//! one ordered outbound queue, which is a change to a far more contended file
//! than this one.

use lodestone_model::{BlockPos, Vec3};

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
/// `state.canBeReplaced()` are not modeled here: `crate::fire::is_fire` could
/// answer the first now that this crate has a real fire block, but nothing
/// wires it into this predicate yet, and there is still no generic "can be
/// replaced" predicate beyond `is_air_or_fluid` itself (`crate::chunk`'s own
/// doc comment: that function already *is* this crate's "can a placement
/// replace this cell" test) — so the two disjuncts this crate has are the
/// whole set it can evaluate, not an arbitrarily narrowed subset.
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

/// `FallingBlockEntity.getDefaultGravity` — `0.04` blocks per tick, per tick.
///
/// Note this is the *entity's* override, not `Entity.getDefaultGravity`, which is
/// `0.0`: an entity with no override does not fall at all. It happens to equal
/// `lodestone_entity::item_entity::ITEM_GRAVITY`, which is a coincidence of two
/// separate overrides and not a shared constant.
pub const FALLING_BLOCK_GRAVITY: f64 = 0.04;

/// `Entity.getAirDrag` — `0.98F`, applied to the whole delta as the **last**
/// statement of `FallingBlockEntity.tick`.
///
/// See this module's doc for why this is the exact decimal `0.98` rather than the
/// `float`-widened `0.980000019073486…` the JVM actually multiplies by, and what
/// that costs.
pub const FALLING_BLOCK_AIR_DRAG: f64 = 0.98;

/// `EntityTypes.FALLING_BLOCK`'s registry key — the `entity_type` a tracked
/// falling block streams under.
pub const FALLING_BLOCK_ENTITY_TYPE: &str = "minecraft:falling_block";

/// `FallingBlockEntity.tick`'s hard cap: `this.time > 600` discards the entity
/// wherever it is.
///
/// Unreachable in this crate for a fall that starts inside the world —
/// [`find_landing_y`] bottoms out at the column floor, so the longest possible
/// descent is the world height and takes far fewer than 600 ticks — but kept as a
/// real bound rather than an `unreachable!()`, because the alternative failure
/// mode is an entity that streams forever.
pub const MAX_FALL_TICKS: u32 = 600;

/// One tick of `FallingBlockEntity`'s vertical motion, in vanilla's own order:
/// `applyGravity` (which *adds* `-0.04` to the delta), then `move` with that
/// delta, then — after the landing decision — the trailing
/// `scale(getAirDrag())`.
///
/// Returns `(dy, velocity_after)`: the displacement this tick takes and the
/// velocity the *next* tick starts from. Both are negative for a falling block.
///
/// The split return is what makes the ordering assertable. Folding the drag into
/// the returned displacement — the natural-looking `(v - g) * drag` one-liner —
/// is exactly the drag-first hypothesis this module's doc rejects, and it moves
/// `0.0392` on the first tick instead of `0.04`.
#[must_use]
pub fn fall_step(velocity_y: f64) -> (f64, f64) {
    // `Entity.applyGravity`: `setDeltaMovement(getDeltaMovement().add(0, -g, 0))`.
    let after_gravity = velocity_y - FALLING_BLOCK_GRAVITY;
    // `move(MoverType.SELF, getDeltaMovement())` — the displacement is the delta
    // as it stands *now*, before the method's trailing drag.
    let dy = after_gravity;
    // The last statement of `tick()`.
    (dy, after_gravity * FALLING_BLOCK_AIR_DRAG)
}

/// A live `FallingBlockEntity`'s own motion state: everything
/// `FallingBlockEntity.tick` reads and writes that is not the world.
///
/// `x`/`z` never change. `FallingBlockEntity`'s constructor calls
/// `setDeltaMovement(Vec3.ZERO)` and `move(SELF, delta)` with a purely vertical
/// delta adds no horizontal component, so a falling block descends in a straight
/// line — which is also why [`velocity_y`](Self::velocity_y) is a scalar rather
/// than a [`Vec3`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FallingBlockMotion {
    /// Entity position, in vanilla's own entity-space: the **centre** of the
    /// block's footprint horizontally, its bottom face vertically.
    pub position: Vec3,
    /// Vertical velocity in blocks per tick, negative while falling. Starts at
    /// exactly `0.0` (`setDeltaMovement(Vec3.ZERO)`), *not* at `-0.04`.
    pub velocity_y: f64,
    /// `FallingBlockEntity.time`, incremented once per tick before the motion.
    pub time: u32,
}

impl FallingBlockMotion {
    /// `FallingBlockEntity.fall`'s spawn position: `pos.getX() + 0.5`,
    /// `pos.getY()`, `pos.getZ() + 0.5`.
    ///
    /// The `+ 0.5` on `x`/`z` and **not** on `y` is not a rounding choice: an
    /// entity's position is its feet, so a falling block whose `y` is the block's
    /// own `y` occupies exactly the cell it left. Adding `0.5` to `y` too would
    /// float every falling block half a block above where it came from, which
    /// looks like a plausible centring bug rather than an obvious one.
    #[must_use]
    pub fn fall_from(pos: BlockPos) -> Self {
        Self {
            position: Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y), f64::from(pos.z) + 0.5),
            velocity_y: 0.0,
            time: 0,
        }
    }

    /// One `FallingBlockEntity.tick`, against a pre-resolved `landing_y`.
    ///
    /// `landing_y` is [`find_landing_y`]'s answer, captured when the entity was
    /// created. Vanilla instead asks `onGround()` every tick, which re-reads the
    /// world; `crate::mobs::MobSim` holds its world immutably and cannot see an
    /// edit made after it was built (see `crate::tick`'s own note on that), so
    /// re-reading here would answer from a stale snapshot anyway. The visible
    /// consequence is bounded and named: a block that appears *underneath* a
    /// falling block mid-flight is fallen through rather than landed on.
    pub fn step(&mut self, landing_y: i32) -> FallingBlockStep {
        self.time += 1;
        let (dy, next_velocity) = fall_step(self.velocity_y);
        self.velocity_y = next_velocity;
        self.position.y += dy;
        let floor = f64::from(landing_y);
        if self.position.y <= floor {
            // Snap, so the placed block and the last broadcast position agree.
            // Vanilla's `move` clips the step against the collision box for the
            // same reason.
            self.position.y = floor;
            return FallingBlockStep::Landed { y: landing_y };
        }
        if self.time > MAX_FALL_TICKS {
            return FallingBlockStep::Expired;
        }
        FallingBlockStep::Falling
    }
}

/// What one [`FallingBlockMotion::step`] resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallingBlockStep {
    /// Still airborne; the entity's new position is the thing to broadcast.
    Falling,
    /// Came to rest at `y` — the block goes back into the world here.
    Landed {
        /// The `y` the block is written at.
        y: i32,
    },
    /// `FallingBlockEntity.tick`'s `time > 600` cap. Discard with no placement.
    Expired,
}

/// One world-visible effect of the falling-block lifecycle, **in the order
/// vanilla produces it**.
///
/// This enum exists for one reason: the two orderings a player can see are
/// otherwise properties of the order a caller wrote two statements in, which no
/// test can observe. As a returned sequence they are assertable. See this
/// module's doc for the two jar citations and for what each reversal looks like
/// on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallingBlockEffect {
    /// `FallingBlockEntity.fall`'s `level.setBlock(pos, air, 3)`. Comes
    /// **before** [`Spawned`](Self::Spawned).
    ClearedOrigin {
        /// The cell the block left.
        pos: BlockPos,
        /// The entity that is about to appear there.
        entity_id: i32,
    },
    /// `level.addFreshEntity(entity)` — the entity is now tracked and will be
    /// picked up by the next streaming pass.
    Spawned {
        /// The new entity's id.
        entity_id: i32,
    },
    /// The landing branch's `level.setBlock(pos, blockState, 3)`. Comes
    /// **before** [`Discarded`](Self::Discarded).
    Placed {
        /// Where the block came to rest.
        pos: BlockPos,
        /// The state written there — the state the entity was imitating.
        state: String,
        /// The entity that is about to be discarded.
        entity_id: i32,
    },
    /// `discard()`. The entity leaves the snapshot set, so the next streaming
    /// pass emits its `REMOVE_ENTITIES`.
    Discarded {
        /// The entity that stopped existing.
        entity_id: i32,
    },
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

    // -----------------------------------------------------------------------
    // `FallingBlockEntity.tick`'s motion, against a hand-solved closed form
    // -----------------------------------------------------------------------

    /// The **correct** hypothesis' displacement on tick `n`, solved by hand from
    /// `a_n = 0.98·a_(n-1) − 0.04`, `a_0 = 0`.
    ///
    /// The recurrence is linear with fixed point `a* = −0.04 / (1 − 0.98) = −2`,
    /// so `a_n − a* = 0.98^n · (a_0 − a*)` and `a_n = 2·(0.98^n − 1)`.
    ///
    /// This is an outside expectation and not `decode(encode(x))`: it is an
    /// algebraic solution of the recurrence the *jar* states, evaluated with
    /// `powi`, and it shares no code path with [`fall_step`]'s iteration. A
    /// transcription error in `fall_step` cannot produce a matching error here.
    fn displacement_at_tick(n: u32) -> f64 {
        2.0 * (0.98_f64.powi(n as i32) - 1.0)
    }

    /// The **wrong** (drag-first) hypothesis' displacement on tick `n`:
    /// `b_n = 0.98·(b_(n-1) − 0.04)`, i.e. `b_n = 1.96·(0.98^n − 1)`. Fixed point
    /// `−0.0392 / 0.02 = −1.96`, same solution shape.
    ///
    /// Kept as a named function rather than a magic number because it is the
    /// hypothesis every assertion below has to *exclude*, and CLAUDE.md's rule is
    /// to compute both readings from outside constants rather than assert the
    /// sign of the difference.
    fn drag_first_displacement_at_tick(n: u32) -> f64 {
        1.96 * (0.98_f64.powi(n as i32) - 1.0)
    }

    /// Cumulative fall after `n` ticks under the correct hypothesis:
    /// `Σ 2(0.98^k − 1) = 98·(1 − 0.98^n) − 2n`, using
    /// `Σ_{k=1..n} 0.98^k = 0.98(1 − 0.98^n)/0.02 = 49(1 − 0.98^n)`.
    fn total_fall_after(n: u32) -> f64 {
        98.0 * (1.0 - 0.98_f64.powi(n as i32)) - 2.0 * f64::from(n)
    }

    /// Cumulative fall under the drag-first hypothesis:
    /// `96.04·(1 − 0.98^n) − 1.96n`.
    fn drag_first_total_fall_after(n: u32) -> f64 {
        96.04 * (1.0 - 0.98_f64.powi(n as i32)) - 1.96 * f64::from(n)
    }

    /// **The discriminating assertion.** Tick one displaces exactly `0.04`, and
    /// the drag-first reading's `0.0392` must fail.
    ///
    /// Exact equality, not a tolerance, and that is affordable here rather than
    /// lucky: `0.0 − 0.04` is the negation of a literal, so the correct answer is
    /// representable to the bit. The two hypotheses differ by under 2%, so this is
    /// precisely the place where an approximate assertion would measure nothing.
    #[test]
    fn tick_one_displaces_exactly_the_bare_gravity_and_not_the_dragged_value() {
        let (dy, velocity_after) = fall_step(0.0);
        assert_eq!(dy, -0.04, "tick one takes the delta as `applyGravity` left it");

        // The wrong hypothesis, evaluated rather than described.
        let drag_first = -0.98 * 0.04;
        assert!(
            (dy - drag_first).abs() > 1e-6,
            "displacement {dy} is the drag-first value {drag_first}: the trailing \
             `scale(getAirDrag())` has been folded into `move`"
        );

        // The drag *is* applied — to what the next tick starts from, not to this
        // tick's displacement. Without this the test above is also satisfied by
        // dropping the drag entirely.
        assert!(
            (velocity_after - drag_first).abs() < 1e-12,
            "the carried velocity must be the dragged one, got {velocity_after}"
        );
    }

    /// Per-tick displacement matches the hand-solved closed form for the first 80
    /// ticks, and diverges from the drag-first solution at every single one of
    /// them.
    ///
    /// Both arms collect into a `Vec` and assert on the collection: an `assert!`
    /// inside the loop aborts at the first mismatch, so a deliberate neuter would
    /// demonstrate one tick and leave the other 79 as arguments rather than
    /// observations.
    #[test]
    fn per_tick_displacement_matches_the_closed_form_and_never_the_drag_first_one() {
        let mut velocity = 0.0;
        let mut wrong_form_matches: Vec<(u32, f64, f64)> = Vec::new();
        let mut closed_form_mismatches: Vec<(u32, f64, f64)> = Vec::new();
        for n in 1..=80 {
            let (dy, next) = fall_step(velocity);
            velocity = next;
            let expected = displacement_at_tick(n);
            if (dy - expected).abs() > 1e-12 {
                closed_form_mismatches.push((n, dy, expected));
            }
            let wrong = drag_first_displacement_at_tick(n);
            if (dy - wrong).abs() <= 1e-12 {
                wrong_form_matches.push((n, dy, wrong));
            }
        }
        assert!(
            closed_form_mismatches.is_empty(),
            "displacement diverged from the hand-solved recurrence at \
             (tick, got, expected): {closed_form_mismatches:?}"
        );
        assert!(
            wrong_form_matches.is_empty(),
            "displacement coincided with the drag-first solution at \
             (tick, got, wrong): {wrong_form_matches:?}"
        );
    }

    /// **The fall height is chosen so the two hypotheses cannot coincide.** A
    /// one-block drop resolves in two ticks under either reading and separates
    /// nothing; over 60 blocks the drag term compounds until the two land more
    /// than a whole block apart.
    ///
    /// The predicted numbers are re-derived, not guessed: `total_fall_after(66)` is
    /// ~59.84 blocks and the drag-first solution's is ~58.64, a gap of ~1.2 blocks.
    /// Neither is the round number "60 at tick 60" that a plausible guess would
    /// reach for.
    #[test]
    fn a_sixty_block_drop_separates_the_two_hypotheses_by_over_a_block() {
        let ticks = 66;
        let correct = total_fall_after(ticks);
        let wrong = drag_first_total_fall_after(ticks);
        assert!(
            correct < -59.0 && correct > -60.0,
            "the chosen tick count must land inside a 60-block drop, got {correct}"
        );
        assert!(
            (correct - wrong).abs() > 1.0,
            "this input does not discriminate: correct {correct} vs drag-first \
             {wrong} differ by only {}",
            (correct - wrong).abs()
        );

        // And the simulation reaches the correct one.
        let mut motion = FallingBlockMotion::fall_from(BlockPos::new(0, 0, 0));
        // A floor far below anything 66 ticks can reach, so nothing lands early.
        for _ in 0..ticks {
            assert_eq!(motion.step(-1000), FallingBlockStep::Falling);
        }
        assert!(
            (motion.position.y - correct).abs() < 1e-9,
            "after {ticks} ticks the entity is at {} but the closed form says {correct}",
            motion.position.y
        );
    }

    /// The spawn position is `FallingBlockEntity.fall`'s: block centre in `x`/`z`,
    /// the block's own `y` **unshifted**, and zero velocity.
    ///
    /// The `y` is the discriminating field: `+ 0.5` there too is the plausible
    /// symmetric mistake, and it floats every falling block half a block high.
    #[test]
    fn the_spawn_position_is_the_block_centre_horizontally_and_its_own_y() {
        let motion = FallingBlockMotion::fall_from(BlockPos::new(-3, 71, 12));
        assert_eq!(motion.position.x, -2.5);
        assert_eq!(motion.position.y, 71.0, "y is the block's own, not y + 0.5");
        assert_eq!(motion.position.z, 12.5);
        assert_eq!(
            motion.velocity_y, 0.0,
            "`setDeltaMovement(Vec3.ZERO)`: the first tick's gravity is what starts it"
        );
        assert_eq!(motion.time, 0);
    }

    /// A fall lands on the resolved floor and snaps exactly onto it, so the last
    /// broadcast position and the placed block agree.
    ///
    /// The tick count is the predicted value: from `y = 70` to `y = 64` is a
    /// 6-block drop, and `total_fall_after(n)` first passes `−6.0` at `n = 18`
    /// (`−5.51` at 17, `−6.12` at 18) — one tick later than the drag-free
    /// `sqrt(2·6/0.04) ≈ 17.3` a no-drag guess would round to, which is what makes
    /// 6 blocks a discriminating height rather than an arbitrary one.
    ///
    /// The bracket below is not decoration: the first version of this test
    /// asserted 19, from arithmetic done by hand, and failed on its first run.
    /// The assertion is the re-derivation, so the prediction cannot silently drift
    /// from the closed form again.
    #[test]
    fn a_six_block_fall_lands_on_the_floor_at_the_predicted_tick() {
        assert!(
            total_fall_after(17) > -6.0 && total_fall_after(18) < -6.0,
            "re-derive the tick count: 17 gives {} and 18 gives {}",
            total_fall_after(17),
            total_fall_after(18)
        );
        let mut motion = FallingBlockMotion::fall_from(BlockPos::new(0, 70, 0));
        let mut landed_at = None;
        for tick in 1..=40u32 {
            if let FallingBlockStep::Landed { y } = motion.step(64) {
                landed_at = Some((tick, y));
                break;
            }
        }
        assert_eq!(landed_at, Some((18, 64)));
        assert_eq!(
            motion.position.y, 64.0,
            "the landing must snap onto the floor, not overshoot below it"
        );
    }

    /// Negative control for the snap: without it the entity's final `y` would be
    /// *below* the floor by the overshoot, and the last position broadcast would
    /// disagree with the placed block.
    ///
    /// Establishes the overshoot is real and non-trivial, so the snap above is
    /// measuring something. `total_fall_after(18) ≈ −6.123` from `y = 70` puts the
    /// unsnapped position at ~`63.877`, i.e. ~0.123 blocks past the floor — enough
    /// to be a visible sink into the ground, and enough to make the last broadcast
    /// position disagree with the placed block.
    #[test]
    fn the_unsnapped_overshoot_the_landing_snap_absorbs_is_real() {
        let raw = 70.0 + total_fall_after(18);
        assert!(
            raw < 64.0 && (64.0 - raw) > 0.1,
            "control failed: an unsnapped 19-tick fall from 70 must overshoot \
             y=64 by a visible margin, got {raw}"
        );
    }

    /// A block already resting on the floor lands on its very first step rather
    /// than falling through — the `landing_y == start_y` case
    /// `crate::random_tick`'s settle rejects before an entity is ever created,
    /// asserted here so the motion is safe if it ever is reached.
    #[test]
    fn a_motion_whose_floor_is_its_own_y_lands_immediately() {
        let mut motion = FallingBlockMotion::fall_from(BlockPos::new(0, 64, 0));
        assert_eq!(motion.step(64), FallingBlockStep::Landed { y: 64 });
        assert_eq!(motion.position.y, 64.0);
    }

    /// The `time > 600` cap fires for a fall with no floor at all, and does so
    /// after 601 steps rather than 600 — `FallingBlockEntity.tick` increments
    /// `time` *before* the comparison, so the strict `>` is reached on the tick
    /// after the count equals the bound.
    ///
    /// Both candidate readings are evaluated: at 600 steps the entity must still
    /// be falling. A gate that only checked "expires eventually" passes under the
    /// off-by-one.
    #[test]
    fn the_six_hundred_tick_cap_fires_one_tick_after_the_bound() {
        let mut motion = FallingBlockMotion::fall_from(BlockPos::new(0, 0, 0));
        for tick in 1..=MAX_FALL_TICKS {
            assert_eq!(
                motion.step(i32::MIN / 2),
                FallingBlockStep::Falling,
                "tick {tick} must still be falling: `time > 600` is strict"
            );
        }
        assert_eq!(motion.step(i32::MIN / 2), FallingBlockStep::Expired);
    }
}
