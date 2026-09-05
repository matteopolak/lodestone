//! Explosion block destruction — the ray-sampled blast, on real blast
//! resistance.
//!
//! # What this is
//!
//! A port of `ServerExplosion::calculateExplodedPositions` plus the one line of
//! `ExplosionDamageCalculator::getBlockExplosionResistance` that gives each
//! traversed cell its cost, read out of the decompiled 26.2 tree as record
//! definitions.
//!
//! This is the *other half* of the explosion work. `lodestone-entity`'s
//! `explosion` module already models entity exposure, damage and knockback, and
//! `MobSim::explode` already fires on the tick a creeper's fuse completes — so a
//! creeper detonating already hurt the player and already reached the client as
//! an `EXPLODE` packet. What it did not do was **remove a single block**: the
//! word `resistance` appeared nowhere in the crate. This module is that missing
//! half, and `tick::run_tick_loop`'s own detonation drain is what calls it.
//!
//! # The algorithm, in vanilla's own order
//!
//! [`exploded_positions`] walks the surface of a 16×16×16 grid — every cell with
//! any coordinate `0` or `15`, which is exactly **1352 rays**
//! (`16³ − 14³ = 4096 − 2744`). For each:
//!
//! 1. the direction is `(2·i/15 − 1)` per axis, normalised;
//! 2. `remainingPower = radius * (0.7 + nextFloat() * 0.6)` — **one RNG draw per
//!    ray**, so a blast's ray pass consumes exactly [`RAY_COUNT`] draws in the
//!    `x → y → z` loop order, and nothing else in the pass is random;
//! 3. then a march in `0.3`-long steps from the centre while power remains:
//!    * read the cell's resistance; if the cell is neither air nor fluid,
//!      subtract `(resistance + 0.3) * 0.3`;
//!    * if power is **still** positive, the cell joins the destroyed set;
//!    * advance `0.3` along the direction and subtract a further `0.22500001`.
//!
//! Both subtractions are `f32`, and the second one is vanilla's own literal
//! `0.22500001F` rather than `0.225` — see [`POWER_DECAY_PER_STEP`].
//!
//! # Where RNG enters a whole explosion, and the one thing that is not portable
//!
//! `ServerExplosion::explode` runs, in order: `calculateExplodedPositions`
//! ([`RAY_COUNT`] `nextFloat` draws), `hurtEntities` (no RNG of its own —
//! `getSeenPercent` is a deterministic grid sample), `interactWithBlocks`, then
//! `createFire` if the blast's `fire` flag is set.
//!
//! **`interactWithBlocks` opens with `Util.shuffle(targetBlocks, level.random)`,
//! and that matters twice.** `Util::shuffle` is Fisher–Yates over the list, so it
//! consumes exactly `n − 1` `nextInt` draws for `n` destroyed blocks — draws that
//! shift every later value in the stream, including `createFire`'s. And the
//! *list it shuffles* is `new ObjectArrayList(toBlowSet)` built from a
//! `HashSet<BlockPos>`, so its input order is **Java hash-iteration order**.
//!
//! The consequence is worth stating plainly rather than discovering later:
//! vanilla's explosion **drop order is not reproducible outside the JVM**, because
//! it is a Fisher–Yates shuffle of a `HashSet` iteration order. Any future drop
//! implementation can match the *multiset* of items and the per-block loot rolls,
//! but not the sequence in which they are emitted, and no amount of care here
//! changes that. It is unobservable today because this module drops nothing (see
//! below), and [`shuffle_draws`] exists so that a caller which *does* model fire
//! consumes the right number of draws in between.
//!
//! # The reach, predicted rather than measured
//!
//! Two costs, and conflating them is the easiest mistake here. A cell that is
//! **truly air with no fluid** yields `Optional.empty()`, so the resistance term
//! is not subtracted at all and the step costs only `0.22500001`. A cell holding a
//! *block* of resistance `r` — including a zero-resistance one like `short_grass` —
//! costs `(r + 0.3) · 0.3 + 0.22500001`, so even resistance `0.0` costs
//! `0.31500003`. [`step_cost`] is the second of those; the first is
//! [`POWER_DECAY_PER_STEP`] alone.
//!
//! **A step is `0.3` blocks, not one block**, so a ray of power `p` travels about
//! `0.3 · p / cost` *blocks*. A creeper's `radius` is `3.0`, so its rays start
//! between `3 · 0.7 = 2.1` and `3 · 1.3 = 3.9` and reach roughly `2.0` to `3.7`
//! blocks through empty air — which is the ~3-block crater radius a creeper
//! actually leaves, and a useful sanity check on the whole port.
//!
//! Through solid stone (`6.0`, so `2.115` per step) the arithmetic is sharper and
//! exact rather than approximate. A ray leaving a one-cell air pocket spends at
//! least two steps (`0.45`) inside it before entering the first stone cell, and
//! that cell costs `1.89` of resistance — so the ray claims it whenever
//! `p > 2.34`, and a **second** stone cell would need `p > 4.455`, which is beyond
//! the `3.9` a creeper can ever produce. So a creeper in solid stone destroys the
//! cells Chebyshev-adjacent to its centre and **provably nothing further**;
//! [`a_creeper_blast_in_stone_destroys_the_predicted_cells`] asserts that bound
//! rather than a fraction.
//!
//! Obsidian at `1200.0` costs `360.315` per step — a creeper's strongest possible
//! ray is `3.9`, so **no creeper ray can ever destroy obsidian**, again an exact
//! arithmetic fact rather than a probability.
//!
//! # What is deliberately not modelled
//!
//! * **Drops.** `BlockBehaviour::onExplosionHit` rolls the block's loot table with
//!   `LootContextParams.EXPLOSION_RADIUS` set for a `DESTROY_WITH_DECAY` blast (a
//!   creeper's). [`crate::loot`] has no `EXPLOSION_RADIUS` parameter at all — its
//!   own module doc lists `survives_explosion` as unconditionally `true` and
//!   `explosion_decay` as a no-op — so rolling here would drop **every** block at
//!   full rate instead of vanilla's `1/radius`. Dropping nothing is the inert
//!   direction; duplicating items into a player's inventory is not. Closing this
//!   needs `EXPLOSION_RADIUS` in `loot.rs`, not a change here.
//! * **`shouldBlockExplode`.** `ExplosionDamageCalculator`'s base implementation
//!   returns `true` unconditionally; the two overrides that matter are
//!   `SimpleExplosionDamageCalculator` (wind charges, which this crate has none
//!   of) and `EntityBasedExplosionDamageCalculator`, which delegates to
//!   `Entity::shouldBlockExplode` — `true` for every entity except the
//!   wither/dragon special cases. A creeper is the only producer, so `true` is
//!   exact here.
//! * **Fire.** `ServerExplosion::createFire` runs only when the blast's own `fire`
//!   flag is set, and a creeper's is `false`. Not modelled: no producer here
//!   ever sets that flag, and an untested, uncalled implementation of it was
//!   removed rather than left as dead weight — reimplement against
//!   `crate::random_tick::is_air_variant` and `block_solidity::legacy_solid`
//!   (one in three, air-over-solid) the day a fire-flagged blast exists.
//! * **Block entities.** A destroyed chest's contents are not spilled.
//! * ~~**`wasExploded` (TNT chain reaction).**~~ Landed, but not in this
//!   module: [`destroy_blocks`] here has no loot/drop knowledge at all (see
//!   the **Drops** bullet above) and is not `tick::run_tick_loop`'s
//!   production path — `crate::block_drops::drop_explosion_loot_in_blast` is,
//!   and that is where a destroyed `minecraft:tnt` block is now chain-primed
//!   instead of looted. A caller using [`destroy_blocks`] directly still gets
//!   no chain reaction; that is this function's existing "no drops at all"
//!   scope, not a new gap.
//!
//! # How to change it
//!
//! Two invariants, both of which exist for a reason paid for elsewhere:
//!
//! * **Every world read goes through [`cell_resistance`]**, which is the single
//!   caching point a future section-level dense cache drops into. See
//!   `docs/explosion-performance.md` for why granularity is the thing that
//!   matters there.
//! * **[`cell_resistance`] bounds-checks before reading.** The march walks outward
//!   from the centre and will happily ask for `min_y - 1` when a blast happens on
//!   the world floor; `ChunkColumn::block_state` indexes unguarded, so an
//!   unchecked read panics the tick thread. Vanilla is safe for the same reason in
//!   reverse: `Level::getBlockState` answers `VOID_AIR` outside build height, and
//!   `calculateExplodedPositions` reads *then* breaks on `isInWorldBounds`, so the
//!   value it read is discarded. Checking first and breaking is exactly
//!   equivalent.
//!
//! The ray count, the step size and the exposure sampling are **physics, not
//! tunables** — do not approximate any of them to make a blast cheaper.

use std::collections::BTreeSet;

use lodestone_data::{block_blast, block_states};
use lodestone_model::{BlockPos, Vec3};

use crate::chunk::ChunkSource;
use crate::mob_spawn::SpawnRng;

/// The grid edge `ServerExplosion` samples (its own `int size = 16`). Only the
/// *surface* of this cube produces a ray.
pub const GRID: i32 = 16;

/// `float stepSize = 0.3F` — both the march step length and, separately, the
/// `+0.3` term added to every resistance.
pub const RAY_STEP: f32 = 0.3;

/// `remainingPower -= 0.22500001F`.
///
/// Written as vanilla's own literal rather than `0.225`: the two differ in the
/// last `f32` mantissa bit, and over the ~12 steps of a creeper ray that is the
/// difference between a cell landing exactly on `0.0` and landing just below it.
pub const POWER_DECAY_PER_STEP: f32 = 0.225_000_01;

/// `0.7F` in `radius * (0.7F + random.nextFloat() * 0.6F)`.
pub const POWER_JITTER_BASE: f32 = 0.7;

/// `0.6F` in the same expression — so a ray's power multiplier is uniform on
/// `[0.7, 1.3)`.
pub const POWER_JITTER_SPAN: f32 = 0.6;

/// The number of rays a blast casts: the surface cells of a 16³ grid,
/// `16³ − 14³`. Also the exact number of RNG draws the ray pass consumes.
pub const RAY_COUNT: usize = 1352;

/// `Level::isInWorldBounds`'s horizontal half — vanilla's ±30,000,000 limit.
pub const HORIZONTAL_LIMIT: i32 = 30_000_000;

/// The dimension's build height, so a blast on the world floor cannot read
/// outside the column.
///
/// Constructed from a real [`crate::chunk::ChunkColumn`]'s own `min_y`/`height`
/// at the call site rather than from 26.2 literals, for the same reason
/// [`crate::fluid::FluidEnv`] is: this crate's overworld shape is the column's,
/// not a constant's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlastEnv {
    /// Lowest addressable `y`.
    pub min_y: i32,
    /// Number of block rows above [`min_y`](Self::min_y).
    pub height: i32,
}

/// Seed for the blast RNG the tick loop owns.
///
/// A distinct stream from every other behaviour generator: one blast draws
/// [`RAY_COUNT`] values, so sharing would make a creeper decide which grass block
/// spreads.
pub const EXPLOSION_BEHAVIOR_SEED: u64 = 0xB1A5_7000_5EED_0001;

impl BlastEnv {
    /// The build height of a real [`crate::chunk::ChunkColumn`] — the constructor
    /// the tick loop uses, for the same reason [`crate::fluid::FluidEnv`] has one:
    /// this crate's overworld shape is the column's, not a constant's.
    #[must_use]
    pub fn in_column(min_y: i32, height: i32) -> BlastEnv {
        BlastEnv { min_y, height }
    }

    /// 26.2's overworld, for tests and for a caller with no column in hand.
    pub const OVERWORLD: BlastEnv = BlastEnv {
        min_y: -64,
        height: 384,
    };

    /// `Level::isInWorldBounds` — inside build height *and* inside the
    /// ±30,000,000 horizontal limit.
    #[must_use]
    pub fn contains(self, pos: BlockPos) -> bool {
        pos.y >= self.min_y
            && pos.y < self.min_y + self.height
            && pos.x >= -HORIZONTAL_LIMIT
            && pos.z >= -HORIZONTAL_LIMIT
            && pos.x < HORIZONTAL_LIMIT
            && pos.z < HORIZONTAL_LIMIT
    }
}

/// What one cell costs a ray, or `None` when the cell is outside the world.
///
/// **The single world-read point of this module**, deliberately: it is where a
/// future section-level dense cache goes, and it is what keeps the march off
/// `ChunkColumn::block_state`'s unguarded index. `Some(None)` is vanilla's
/// `Optional.empty()` — the cell is air and holds no fluid, so no resistance term
/// is subtracted at all.
///
/// The resistance itself comes from
/// [`block_blast::explosion_resistance_for_state_id`], a flat array index with the
/// fluid `max` already folded in, rather than from any string comparison. The one
/// remaining string cost is `ChunkSource::block_state`'s own `String` plus the
/// registry resolution — see `docs/explosion-performance.md`.
fn cell_resistance<S: ChunkSource>(
    world: &S,
    env: BlastEnv,
    pos: BlockPos,
) -> Option<Option<f32>> {
    if !env.contains(pos) {
        return None;
    }
    let state = world.block_state(pos.x, pos.y, pos.z);
    Some(match block_states::StateId::from_state_str(&state) {
        Some(id) => block_blast::explosion_resistance_for_state_id(id),
        None => block_blast::explosion_resistance_for_state(&state),
    })
}

/// The per-step power cost of traversing a cell that holds a **block** of
/// resistance `resistance` — `(resistance + 0.3) * 0.3 + 0.22500001`.
///
/// Note this is **not** the cost of empty air: a cell that is air with no fluid
/// yields `Optional.empty()` and the resistance term is skipped entirely, so it
/// costs [`POWER_DECAY_PER_STEP`] alone. `step_cost(0.0)` is the cost of a
/// zero-resistance *block*, which is a different and larger number.
///
/// Exposed because it is the whole content of this module's arithmetic and the
/// only thing a caller could want to predict: `power / step_cost(r)` is how many
/// *steps* — each `0.3` blocks long — of that material a ray of `power` passes.
#[must_use]
pub fn step_cost(resistance: f32) -> f32 {
    (resistance + RAY_STEP) * RAY_STEP + POWER_DECAY_PER_STEP
}

/// How many RNG draws `Util::shuffle` consumes for a list of `len` entries:
/// `len - 1` (`nextInt(i)` for `i` from `len` down to `2`), and `0` for a list of
/// one or none.
///
/// Not used by [`destroy_blocks`], which does not model drops and therefore does
/// not shuffle. It exists so that a caller wiring `createFire` — the next draw
/// consumer after `interactWithBlocks` in `ServerExplosion::explode` — can keep
/// the stream aligned with vanilla's. See this module's own doc comment for why
/// the shuffle's *order* is not reproducible even though its *count* is.
#[must_use]
pub fn shuffle_draws(len: usize) -> usize {
    len.saturating_sub(1)
}

/// `BlockPos.containing(x, y, z)` — a floor, not a truncation. Getting this wrong
/// mirrors the crater about the origin planes and nowhere else, so it is
/// invisible in a test centred on positive coordinates.
fn containing(x: f64, y: f64, z: f64) -> BlockPos {
    BlockPos::new(
        x.floor() as i32,
        y.floor() as i32,
        z.floor() as i32,
    )
}

/// Every block position a blast of `radius` centred at `centre` destroys.
///
/// A faithful port of `ServerExplosion::calculateExplodedPositions`. Draws
/// exactly [`RAY_COUNT`] values from `rng`, one per ray, in the `x → y → z` order
/// vanilla's triple loop visits — the draw count and order are part of the
/// specification, not an implementation detail, because a blast's crater shape is
/// entirely decided by which ray got which power.
///
/// The returned `Vec` is **sorted** rather than in hash order. Vanilla
/// accumulates into a `HashSet` and then shuffles it; that order is observable
/// only through drop emission, which this module does not model, and it is not
/// reproducible outside the JVM anyway — so a deterministic order is strictly
/// better than an arbitrary one here.
#[must_use]
pub fn exploded_positions<S: ChunkSource>(
    world: &S,
    env: BlastEnv,
    centre: Vec3,
    radius: f32,
    rng: &mut SpawnRng,
) -> Vec<BlockPos> {
    let mut destroyed: BTreeSet<(i32, i32, i32)> = BTreeSet::new();
    for xx in 0..GRID {
        for yy in 0..GRID {
            for zz in 0..GRID {
                let on_surface = xx == 0
                    || xx == GRID - 1
                    || yy == 0
                    || yy == GRID - 1
                    || zz == 0
                    || zz == GRID - 1;
                if !on_surface {
                    continue;
                }
                // Vanilla computes these in `double` from `float` intermediates
                // (`xx / 15.0F * 2.0F - 1.0F`), so the cast order is kept.
                let mut dx = f64::from(xx as f32 / 15.0 * 2.0 - 1.0);
                let mut dy = f64::from(yy as f32 / 15.0 * 2.0 - 1.0);
                let mut dz = f64::from(zz as f32 / 15.0 * 2.0 - 1.0);
                let len = (dx * dx + dy * dy + dz * dz).sqrt();
                dx /= len;
                dy /= len;
                dz /= len;
                // The one and only RNG draw of this ray.
                let mut power = radius * (POWER_JITTER_BASE + rng.next_f32() * POWER_JITTER_SPAN);
                let mut px = centre.x;
                let mut py = centre.y;
                let mut pz = centre.z;
                while power > 0.0 {
                    let pos = containing(px, py, pz);
                    let Some(resistance) = cell_resistance(world, env, pos) else {
                        // Outside the world: vanilla's `isInWorldBounds` break.
                        break;
                    };
                    if let Some(resistance) = resistance {
                        power -= (resistance + RAY_STEP) * RAY_STEP;
                    }
                    if power > 0.0 {
                        destroyed.insert((pos.x, pos.y, pos.z));
                    }
                    px += dx * f64::from(RAY_STEP);
                    py += dy * f64::from(RAY_STEP);
                    pz += dz * f64::from(RAY_STEP);
                    power -= POWER_DECAY_PER_STEP;
                }
            }
        }
    }
    destroyed
        .into_iter()
        .map(|(x, y, z)| BlockPos::new(x, y, z))
        .collect()
}

/// Runs a blast's block half against `world`: computes [`exploded_positions`],
/// writes air into every one of them, and returns the `(position, new state)`
/// pairs a caller must publish to connected clients.
///
/// `ServerExplosion::interactWithBlocks` plus `BlockBehaviour::onExplosionHit`'s
/// own `setBlock(pos, AIR)`, minus the drops — see this module's doc comment for
/// why dropping nothing is the deliberate choice rather than an omission, and for
/// why the shuffle vanilla performs first is skipped.
///
/// Air positions are skipped rather than rewritten, so the published set is the
/// blocks that actually changed. Vanilla's `onExplosionHit` has the same guard.
pub fn destroy_blocks<S: ChunkSource>(
    world: &S,
    env: BlastEnv,
    centre: Vec3,
    radius: f32,
    rng: &mut SpawnRng,
) -> Vec<(BlockPos, String)> {
    let mut changes = Vec::new();
    for pos in exploded_positions(world, env, centre, radius, rng) {
        let state = world.block_state(pos.x, pos.y, pos.z);
        if crate::random_tick::is_air_variant(&state) {
            continue;
        }
        world.set_block(pos.x, pos.y, pos.z, crate::chunk::AIR);
        changes.push((pos, crate::chunk::AIR.to_owned()));
    }
    changes
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::chunk::ChunkColumn;

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;

    /// A `ChunkSource` that retains its edits across as many columns as a test
    /// touches. Retention matters because [`destroy_blocks`] reads the world back
    /// as it writes.
    struct Rig {
        columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
        fill: &'static str,
        /// Everything at or below this `y` is `fill`; above it is air. `None`
        /// fills nothing (a pure-air world).
        floor_y: Option<i32>,
    }

    impl Rig {
        fn new(fill: &'static str, floor_y: Option<i32>) -> Self {
            Self {
                columns: Mutex::new(HashMap::new()),
                fill,
                floor_y,
            }
        }

        fn fresh_column(&self) -> ChunkColumn {
            let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
            if let Some(floor_y) = self.floor_y {
                for y in MIN_Y..=floor_y {
                    for lz in 0..16 {
                        for lx in 0..16 {
                            column.set_block(lx, y, lz, self.fill);
                        }
                    }
                }
            }
            column
        }
    }

    impl ChunkSource for Rig {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut columns = self.columns.lock().expect("rig lock");
            columns
                .entry((cx, cz))
                .or_insert_with(|| self.fresh_column())
                .clone()
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns.entry((cx, cz)).or_insert_with(|| self.fresh_column());
            column.block_state(x - cx * 16, y, z - cz * 16).to_string()
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns.entry((cx, cz)).or_insert_with(|| self.fresh_column());
            column.biome_state_at(x - cx * 16, y, z - cz * 16).to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns.entry((cx, cz)).or_insert_with(|| self.fresh_column());
            column.set_block(x - cx * 16, y, z - cz * 16, name);
        }
    }

    /// One seed, so a test can build two *independent* generators — `SpawnRng` is
    /// not `Copy`, and a determinism or draw-count gate that reused one instance
    /// would be measuring memoisation rather than the count.
    const SEED: u64 = 0x5150_3131_4213_9977;

    fn rng() -> SpawnRng {
        SpawnRng::new(SEED)
    }

    /// Premise check for every gate below: the rig really retains an edit, so a
    /// "nothing was destroyed" result cannot be the rig regenerating terrain.
    #[test]
    fn the_rig_retains_its_own_edits() {
        let rig = Rig::new("minecraft:stone", Some(0));
        assert_eq!(rig.block_state(3, 0, 3), "minecraft:stone");
        rig.set_block(3, 0, 3, crate::chunk::AIR);
        assert_eq!(rig.block_state(3, 0, 3), crate::chunk::AIR);
    }

    /// The ray count is `16³ − 14³`, and it is also the RNG draw count. Counted
    /// against a world where every ray dies on its first step, so the count
    /// cannot be inflated by the march.
    #[test]
    fn a_blast_casts_exactly_1352_rays_and_draws_once_per_ray() {
        // Bedrock everywhere: `step_cost(3_600_000)` is about 1.08e6, so every ray
        // dies inside its first cell and the draw count equals the ray count.
        let rig = Rig::new("minecraft:bedrock", Some(64));
        let mut counted = rng();
        let destroyed = exploded_positions(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(8.5, 8.5, 8.5),
            3.0,
            &mut counted,
        );
        assert!(destroyed.is_empty(), "no ray may survive bedrock");

        // Independently advance a fresh generator RAY_COUNT times and require the
        // two to have landed on the same state — a magnitude check on the draw
        // count, not a sign check.
        let mut reference = rng();
        for _ in 0..RAY_COUNT {
            reference.next_f32();
        }
        assert_eq!(
            reference.next_f32(),
            counted.next_f32(),
            "a blast must consume exactly {RAY_COUNT} draws"
        );
    }

    /// Negative control for the count above: `RAY_COUNT - 1` draws leaves the two
    /// generators out of step, so the equality is measuring the count and not some
    /// property that holds for any advance.
    #[test]
    fn the_draw_count_control_fails_at_one_draw_fewer() {
        let mut a = rng();
        let mut b = rng();
        for _ in 0..RAY_COUNT - 1 {
            a.next_f32();
        }
        for _ in 0..RAY_COUNT {
            b.next_f32();
        }
        assert_ne!(a.next_f32(), b.next_f32());
    }

    /// `Util::shuffle`'s draw count, which a fire-flagged blast has to consume
    /// between the destroyed set and `createFire`.
    #[test]
    fn shuffle_draw_count_is_len_minus_one() {
        assert_eq!(shuffle_draws(0), 0);
        assert_eq!(shuffle_draws(1), 0);
        assert_eq!(shuffle_draws(2), 1);
        assert_eq!(shuffle_draws(87), 86);
    }

    /// The step costs, and the thresholds they imply for a creeper (`radius 3.0`,
    /// so power in `[2.1, 3.9)`). Every expected value comes from the committed JVM
    /// resistance table and vanilla's own arithmetic, not from this module's output.
    #[test]
    fn stone_and_obsidian_thresholds_match_the_arithmetic() {
        // Empty air skips the resistance term entirely.
        assert!(
            (POWER_DECAY_PER_STEP - 0.225).abs() < 1e-7,
            "air costs only the step term"
        );
        // A zero-resistance *block* still pays (0 + 0.3) * 0.3.
        let zero_block = step_cost(0.0);
        assert!(
            (zero_block - 0.315_000_03).abs() < 1e-7,
            "a zero-resistance block step cost was {zero_block}"
        );
        assert!(
            zero_block > POWER_DECAY_PER_STEP,
            "a zero-resistance block must still cost more than empty air"
        );

        let stone = step_cost(6.0);
        assert!((stone - 2.115).abs() < 1e-5, "stone step cost was {stone}");
        // A ray leaving a one-cell air pocket has spent at least two steps of
        // 0.225 before it meets stone.
        let after_pocket = 2.0 * POWER_DECAY_PER_STEP;
        assert!(
            3.9 - after_pocket - stone > 0.0,
            "a strongest creeper ray claims the first stone cell"
        );
        assert!(
            2.1 - after_pocket - stone < 0.0,
            "a weakest creeper ray does not"
        );
        assert!(
            3.9 - after_pocket - 2.0 * stone - POWER_DECAY_PER_STEP < 0.0,
            "and no creeper ray can ever claim a second stone cell"
        );

        let obsidian = step_cost(1200.0);
        assert!((obsidian - 360.315).abs() < 1e-2, "obsidian step cost was {obsidian}");
        assert!(
            3.9 - obsidian < 0.0,
            "no creeper ray can ever pass an obsidian cell"
        );
    }

    /// The visible signature: a creeper-sized blast in solid stone with the centre
    /// in a one-cell air pocket.
    ///
    /// Predicted exactly where the arithmetic allows and bracketed where it does
    /// not, per this repo's own rule for that shape:
    ///
    /// * **exact upper bound** — every destroyed cell is Chebyshev-adjacent to the
    ///   centre. A ray leaving the pocket spends at least `2 x 0.225` inside it,
    ///   the first stone cell costs `1.89`, and a second would need
    ///   `0.45 + 2 x 1.89 + 0.225 = 4.455` power, beyond the `3.9` a creeper can
    ///   ever produce. So 27 cells is a hard ceiling and no seed can exceed it.
    /// * **exact membership** — the centre plus all six *face* neighbours. A face
    ///   neighbour needs only `p > 2.34`, which is 87% of the `[2.1, 3.9)` range,
    ///   and hundreds of rays point at each face.
    /// * **bracketed** — the 12 edge and 8 corner neighbours need a ray to cross
    ///   two or three cell boundaries, so they cost 3 pocket steps rather than 2 and
    ///   are reached by a smaller share of directions. Which of them a given seed
    ///   happens to reach is not predictable from the constants, so the total is
    ///   bracketed rather than pinned.
    ///
    /// A wrong resistance fails the Chebyshev bound; a resistance ignored entirely
    /// fails it by a wide margin.
    #[test]
    fn a_creeper_blast_in_stone_destroys_the_predicted_cells() {
        let rig = Rig::new("minecraft:stone", Some(64));
        rig.set_block(8, 8, 8, crate::chunk::AIR);
        let mut r = rng();
        let destroyed = exploded_positions(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(8.5, 8.5, 8.5),
            3.0,
            &mut r,
        );
        for pos in &destroyed {
            let chebyshev = (pos.x - 8).abs().max((pos.y - 8).abs()).max((pos.z - 8).abs());
            assert!(
                chebyshev <= 1,
                "{pos:?} is Chebyshev {chebyshev} from the centre — no creeper ray can \
                 reach a second stone cell (0.45 + 2 x 2.115 + 0.225 > 3.9)"
            );
        }
        assert!(
            destroyed.contains(&BlockPos::new(8, 8, 8)),
            "the centre air cell must always be claimed"
        );
        for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (0, 0, 1), (0, 0, -1)] {
            let face = BlockPos::new(8 + dx, 8 + dy, 8 + dz);
            assert!(
                destroyed.contains(&face),
                "the face neighbour {face:?} needs only p > 2.34 and must always be destroyed"
            );
        }
        assert!(
            (20..=27).contains(&destroyed.len()),
            "the pocket plus most of its 26 Chebyshev neighbours; 27 is the hard \
             arithmetic ceiling, got {}",
            destroyed.len()
        );
    }

    /// Reach through zero-resistance *blocks*, predicted from `step_cost(0.0)`
    /// rather than measured: the strongest possible ray has power just under `3.9`
    /// and pays `0.31500003` per step, so it takes at most
    /// `(3.9 - 0.09) / 0.315000034 = 12.09` steps, i.e. `12 x 0.3 = 3.6` blocks
    /// from the centre. From a centre at `0.5` that is coordinate `4` and never
    /// `5` — an exact upper bound.
    ///
    /// The lower bound is the other half: coordinate `3` needs only
    /// `9 x 0.3 x ~1 = 2.7` blocks, i.e. `p > 2.925`, which is roughly a third of
    /// the `[2.1, 3.9)` range, so with 1352 rays it is certain.
    #[test]
    fn a_zero_resistance_blast_reaches_the_predicted_distance_and_no_further() {
        let rig = Rig::new("minecraft:short_grass", Some(64));
        let mut r = rng();
        let destroyed = exploded_positions(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(0.5, 32.5, 0.5),
            3.0,
            &mut r,
        );
        let max_axis = destroyed
            .iter()
            .map(|p| p.x.abs().max((p.y - 32).abs()).max(p.z.abs()))
            .max()
            .expect("something was destroyed");
        assert!(
            max_axis <= 4,
            "a radius-3 blast cannot exceed 3.6 blocks through zero-resistance \
             blocks, so coordinate 4 is the hard bound; measured {max_axis}"
        );
        assert!(
            max_axis >= 3,
            "and it must comfortably reach coordinate 3; measured {max_axis}"
        );
    }

    /// Empty air is cheaper per step than a zero-resistance block, so the same
    /// blast in a **pure air** world claims positions further out — and every one of
    /// them is air, which is why `destroy_blocks` reports no change at all.
    ///
    /// The predicted bound: air costs `0.225` per step, so the strongest ray takes
    /// at most `3.9 / 0.225 = 17.3` steps, i.e. `17 x 0.3 = 5.1` blocks, which from
    /// a centre at `0.5` is coordinate `5` and never `6`.
    #[test]
    fn a_blast_in_pure_air_reaches_further_and_destroys_nothing() {
        let rig = Rig::new("minecraft:stone", None);
        let mut r = rng();
        let claimed = exploded_positions(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(0.5, 100.5, 0.5),
            3.0,
            &mut r,
        );
        let max_axis = claimed
            .iter()
            .map(|p| p.x.abs().max((p.y - 100).abs()).max(p.z.abs()))
            .max()
            .expect("air positions are still claimed");
        assert!(
            (5..=5).contains(&max_axis),
            "air costs 0.225 a step, so 17 steps is 5.1 blocks and coordinate 5 is \
             the bound; measured {max_axis}"
        );

        let mut r = rng();
        let changes = destroy_blocks(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(0.5, 100.5, 0.5),
            3.0,
            &mut r,
        );
        assert!(
            changes.is_empty(),
            "air is claimed but never *changed* — vanilla's onExplosionHit skips it"
        );
    }

    /// The floor hazard the fluid port paid for, here as a real gate: a blast
    /// centred on the world floor marches *down* out of build height on its very
    /// first steps. Without [`BlastEnv::contains`] this panics inside
    /// `ChunkColumn::block_state` on the tick thread.
    #[test]
    fn a_blast_on_the_world_floor_does_not_panic() {
        let rig = Rig::new("minecraft:stone", Some(MIN_Y));
        let mut r = rng();
        let destroyed = exploded_positions(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(0.5, f64::from(MIN_Y) + 0.5, 0.5),
            3.0,
            &mut r,
        );
        assert!(
            destroyed.iter().all(|p| p.y >= MIN_Y),
            "nothing below min_y may be destroyed"
        );
        assert!(!destroyed.is_empty(), "the floor cell itself is destroyed");
    }

    /// `destroy_blocks` really writes air through the source, and reports exactly
    /// the cells it changed — the wire this module exists for.
    #[test]
    fn destroy_blocks_writes_air_and_reports_the_changes() {
        let rig = Rig::new("minecraft:stone", Some(64));
        rig.set_block(8, 8, 8, crate::chunk::AIR);
        let mut r = rng();
        let changes = destroy_blocks(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(8.5, 8.5, 8.5),
            3.0,
            &mut r,
        );
        // The centre was already air, so it is in the destroyed *set* but not in
        // the reported changes: exactly one fewer, and every reported cell really is
        // air in the world afterwards. That one-cell difference is the whole content
        // of vanilla's `!state.isAir()` guard.
        let mut probe = rng();
        let rig_probe = Rig::new("minecraft:stone", Some(64));
        rig_probe.set_block(8, 8, 8, crate::chunk::AIR);
        let claimed = exploded_positions(
            &rig_probe,
            BlastEnv::OVERWORLD,
            Vec3::new(8.5, 8.5, 8.5),
            3.0,
            &mut probe,
        );
        assert_eq!(
            changes.len(),
            claimed.len() - 1,
            "exactly the one already-air cell is skipped"
        );
        for (pos, state) in &changes {
            assert_eq!(state, crate::chunk::AIR);
            assert_eq!(
                rig.block_state(pos.x, pos.y, pos.z),
                crate::chunk::AIR,
                "{pos:?} must actually be air in the world now"
            );
        }
        assert!(
            changes.iter().any(|(p, _)| *p == BlockPos::new(8, 9, 8)),
            "the cell above the centre is one of them"
        );
    }

    /// Obsidian is the negative control for the whole module: identical scene,
    /// identical seed, and *nothing* is destroyed — so the stone result above is
    /// measuring resistance rather than merely "a blast happened".
    #[test]
    fn an_obsidian_shell_survives_a_creeper_entirely() {
        let rig = Rig::new("minecraft:obsidian", Some(64));
        rig.set_block(8, 8, 8, crate::chunk::AIR);
        let mut r = rng();
        let changes = destroy_blocks(
            &rig,
            BlastEnv::OVERWORLD,
            Vec3::new(8.5, 8.5, 8.5),
            3.0,
            &mut r,
        );
        assert!(changes.is_empty(), "obsidian must survive, got {changes:?}");
    }

    /// Determinism: two independently built blasts from the same seed agree
    /// exactly. Two fresh generators, not one generator queried twice, per this
    /// repo's own determinism-gate rule.
    #[test]
    fn two_independent_blasts_from_one_seed_agree() {
        let build = || {
            let rig = Rig::new("minecraft:stone", Some(64));
            rig.set_block(8, 8, 8, crate::chunk::AIR);
            let mut r = rng();
            exploded_positions(&rig, BlastEnv::OVERWORLD, Vec3::new(8.5, 8.5, 8.5), 3.0, &mut r)
        };
        assert_eq!(build(), build());
    }
}
