//! Block drops: rolling a broken block's loot table and popping the result as
//! item entities (issue #337's missing consumer).
//!
//! # What it is
//!
//! The join between three things that already existed separately and never met:
//! [`crate::loot`] (a 1,551-line loot-table parser and roller, reached only by
//! its own unit tests), [`crate::MobSim::spawn_item`] (a server-side item
//! entity with real fall dynamics, reached by exactly one production caller —
//! the composter's bone-meal extraction), and `apply_block_action`'s
//! `StopDestroy` arm, which set the block to air and dropped **nothing**.
//!
//! This module is the glue plus the two pieces of vanilla behaviour neither
//! side had: which loot table a block state resolves to, and where/how fast the
//! resulting item entity appears.
//!
//! # How it works
//!
//! 1. [`block_loot_table_id`] turns a block state (`"minecraft:stone"`, or
//!    `"minecraft:oak_log[axis=y]"`) into its loot-table key
//!    (`minecraft:blocks/stone`).
//! 2. [`drop_block_loot`] looks that table up in the [`crate::LootTableSet`],
//!    rolls it, and turns each resulting [`ItemStack`] into a [`PoppedItem`]
//!    carrying the position and velocity vanilla's `Block.popResource` would
//!    have given it.
//! 3. The caller (`server.rs`'s `StopDestroy`) hands each [`PoppedItem`] to
//!    [`crate::MobSim::spawn_item`], which is already ticked every server tick
//!    by [`crate::tick::run_tick_loop`] and already streamed to every
//!    connection by [`crate::MobSim::snapshots`].
//! 4. [`is_within_pickup_range`] is the other end: the geometry that decides
//!    whether a player standing here collects an item entity there.
//!
//! # `popResource`'s draw order *is* the specification
//!
//! From `Block.popResource` (`.cache/mc/26.2/src/net/minecraft/world/level/
//! block/Block.java:412-419`) and the `ItemEntity` constructor it calls
//! (`world/entity/item/ItemEntity.java:61-66`), in order:
//!
//! ```text
//! double halfHeight = EntityTypes.ITEM.getHeight() / 2.0;       // 0.25 / 2
//! double x = pos.getX() + 0.5 + Mth.nextDouble(random, -0.25, 0.25);
//! double y = pos.getY() + 0.5 + Mth.nextDouble(random, -0.25, 0.25) - halfHeight;
//! double z = pos.getZ() + 0.5 + Mth.nextDouble(random, -0.25, 0.25);
//! …
//! this.setDeltaMovement(this.random.nextDouble() * 0.2 - 0.1, 0.2, this.random.nextDouble() * 0.2 - 0.1);
//! ```
//!
//! **Five draws, in the order x, y, z, vx, vz** — `vy` is the constant `0.2`
//! and consumes nothing. A port that draws `vy` too, or that draws the velocity
//! before the position, produces a statistically identical cloud of items and
//! desyncs from vanilla for any given seed. That is why
//! [`pop_resource_placement`] takes the RNG and makes the five draws itself
//! rather than accepting an offset from a caller.
//!
//! Note `EntityTypes.ITEM` is `.sized(0.25F, 0.25F)`
//! (`world/entity/EntityTypes.java:558-566`), so `halfHeight` is `0.125` — the
//! item entity's *feet* sit an eighth of a block below the block centre, which
//! is what centres its 0.25-tall box on the centre. This is a real position, not
//! a rounding detail: get the sign wrong and every drop spawns *above* centre.
//!
//! ## The RNG divergence that is deliberate, and visible
//!
//! Vanilla makes the three position draws from the **level's** `RandomSource`
//! and the two velocity draws from the **entity's own** freshly-seeded one —
//! two independent streams. This crate has one [`SpawnRng`] per call site, so
//! all five come from that single stream. The draw *count* and *order* are
//! vanilla's; the stream is not, and [`crate::loot`]'s own module doc records
//! the same divergence for the roll itself (`SpawnRng` is SplitMix64, vanilla's
//! is Xoroshiro). Byte-exact stream parity with a JVM roll is a separate,
//! larger piece of work — see that doc.
//!
//! # How to change it
//!
//! * **Bundling another block's table** is a JSON file under
//!   `assets/loot_table/blocks/`, nothing here. [`block_loot_table_id`] is
//!   purely mechanical (`minecraft:blocks/` + the block path) so a new table is
//!   found without a code change — and a block with **no** bundled table drops
//!   nothing, which is the honest behaviour rather than a guessed default.
//!   [`drop_block_loot`] returns an empty `Vec` for both "no such table" and "a
//!   table that rolled nothing", because vanilla does not distinguish them at
//!   this seam either.
//! * **Tool-sensitive drops** (silk touch, fortune, correct-tool) need
//!   [`crate::LootContext`] to carry the tool, not a change here. Under the
//!   empty context every bundled table's `match_tool` branch fails and the
//!   `alternatives` fall through to the un-enchanted branch — which is *exactly*
//!   right for a bare hand, and exactly wrong for a silk-touch pickaxe. See
//!   this module's tests for the predicted-value table.
//! * **The pickup volume** is vanilla's player AABB inflated by `(1.0, 0.5,
//!   1.0)` intersected against the item's own AABB, not a radius. Both boxes
//!   matter: see [`is_within_pickup_range`].
//!
//! # Dependencies
//!
//! [`crate::loot`] for the tables, [`crate::mob_spawn::SpawnRng`] for the draws,
//! `lodestone_model` for the vocabulary. Names no packet and no protocol
//! version, like the rest of this crate.

use lodestone_model::{BlockPos, ItemStack, ResourceKey, Vec3};

use crate::loot::{LootContext, LootTableSet};
use crate::mob_spawn::SpawnRng;

/// Half the height of `EntityTypes.ITEM`, which is `.sized(0.25F, 0.25F)`
/// (`world/entity/EntityTypes.java:558-566`). `Block.popResource` subtracts
/// this from the y it computes so the item's 0.25-tall box is *centred* on the
/// block centre rather than sitting on it.
const ITEM_HALF_HEIGHT: f64 = 0.25 / 2.0;

/// The `±0.25` spread `Block.popResource` applies to each of the three axes
/// (`Mth.nextDouble(random, -0.25, 0.25)`).
const POP_SPREAD: f64 = 0.25;

/// Vanilla's `ItemEntity` constructor sets `deltaMovement.y` to this constant —
/// it consumes **no** RNG draw, unlike x and z. See the module doc comment.
const POP_VELOCITY_Y: f64 = 0.2;

/// Horizontal velocity spread: `random.nextDouble() * 0.2 - 0.1`, i.e. `±0.1`.
const POP_VELOCITY_SPREAD: f64 = 0.1;

/// `ItemEntity.setDefaultPickUpDelay()` (`ItemEntity.java:400-402`) — ten ticks
/// before a freshly popped drop can be collected, which is what stops the
/// player who broke the block from re-absorbing it instantly and is why a
/// pickup gate must advance the tick clock before asserting.
pub const DEFAULT_PICKUP_DELAY: i16 = 10;

/// Vanilla player bounding-box width (`EntityTypes.PLAYER` is
/// `.sized(0.6F, 1.8F)`), halved — the box is centred on the feet position in
/// x/z.
const PLAYER_HALF_WIDTH: f64 = 0.6 / 2.0;

/// Vanilla player bounding-box height.
const PLAYER_HEIGHT: f64 = 1.8;

/// Half-extents of the item entity's own box (`sized(0.25F, 0.25F)`) in x/z.
const ITEM_HALF_WIDTH: f64 = 0.25 / 2.0;

/// Full height of the item entity's box.
const ITEM_HEIGHT: f64 = 0.25;

/// `Player.aiStep`'s pickup inflation (`world/entity/player/Player.java:462`:
/// `this.getBoundingBox().inflate(1.0, 0.5, 1.0)`), as `(horizontal, vertical)`.
const PICKUP_INFLATE_XZ: f64 = 1.0;
const PICKUP_INFLATE_Y: f64 = 0.5;

/// The seed for the per-connection [`SpawnRng`] that draws a block break's loot
/// roll and its `popResource` placement.
///
/// Explicit rather than drawn, matching [`crate::tick`]'s
/// `RANDOM_TICK_BEHAVIOR_SEED` and `server`'s `COMPOSTER_BEHAVIOR_SEED` — this
/// crate takes seeds so a test can replay an exact outcome. Per-*connection*
/// like the composter's, which means two players mining simultaneously draw from
/// different streams; that changes which roll a given break sees and nothing
/// about the world state, since the drops themselves land in the shared
/// `MobSim`.
pub const BLOCK_DROPS_BEHAVIOR_SEED: u64 = 0xD_1207_5EED;

/// The bundled loot-table corpus, parsed once per process.
///
/// [`LootTableSet::load_bundled`] parses the JSON embedded by `build.rs`, so it
/// is neither free nor expensive. A `OnceLock` rather than a per-connection copy
/// because the tables are immutable and shared by every connection — and rather
/// than a parameter threaded from `serve_play`, because `handle_play_packet`
/// already takes twenty-five arguments and this one has exactly one possible
/// value.
///
/// Note the debug assertion inside `load_bundled`: every bundled table must have
/// **zero** unsupported features, so a newly-dropped-in table that uses a
/// condition [`crate::loot`] does not model fails loudly in a debug build rather
/// than silently rolling nothing.
#[must_use]
pub fn bundled_tables() -> &'static LootTableSet {
    static TABLES: std::sync::OnceLock<LootTableSet> = std::sync::OnceLock::new();
    TABLES.get_or_init(LootTableSet::load_bundled)
}

/// One item entity a block break wants spawned: what to spawn, where, and how
/// fast.
///
/// Deliberately *not* spawned by this module. `MobSim` lives behind a
/// `MobHandle` mutex the caller already holds for other reasons, and returning
/// a plain value keeps [`drop_block_loot`] a pure function of
/// `(block state, position, rng)` — which is what lets its tests predict an
/// exact position and count from vanilla constants rather than observing
/// whatever the simulation happened to do.
#[derive(Debug, Clone, PartialEq)]
pub struct PoppedItem {
    /// The rolled stack. `count` can legitimately be `0` — see
    /// [`crate::loot`]'s note on `set_count`; [`drop_block_loot`] filters those
    /// out, because vanilla's `popResource` skips an empty stack
    /// (`!itemStack.isEmpty()` in the private `popResource` overload).
    pub stack: ItemStack,
    /// World-space feet position, already carrying `popResource`'s jitter and
    /// its `- halfHeight` centring.
    pub position: Vec3,
    /// Velocity in blocks/tick, as the `ItemEntity` constructor sets it.
    pub velocity: Vec3,
}

/// The loot-table key for a block state — vanilla's `Block.getLootTable`, whose
/// default is `minecraft:blocks/` + the block's registry path
/// (`Block.java`'s `lootTable` supplier, built from the block id).
///
/// Accepts a state string with or without properties: `"minecraft:oak_log
/// [axis=y]"` and `"minecraft:oak_log"` resolve alike, because a loot table is
/// keyed by *block*, not block state. A bare path with no namespace is treated
/// as `minecraft:`, matching how the rest of this crate reads block names.
///
/// Returns `None` only for a name this crate cannot parse as a resource key at
/// all; a syntactically fine name for a block with no bundled table resolves
/// happily here and then misses in [`LootTableSet::get`], which is the right
/// place for that to be noticed.
#[must_use]
pub fn block_loot_table_id(block_state: &str) -> Option<ResourceKey> {
    let name = block_state
        .split_once('[')
        .map_or(block_state, |(name, _)| name)
        .trim();
    let path = name.split_once(':').map_or(name, |(_, path)| path);
    if path.is_empty() {
        return None;
    }
    format!("minecraft:blocks/{path}").parse().ok()
}

/// `Block.popResource`'s position and the `ItemEntity` constructor's velocity,
/// in vanilla's exact five-draw order.
///
/// Separated from [`drop_block_loot`] so a test can pin the draw *sequence*
/// against a known RNG state — the property that a "spawn it near the block
/// with a bit of upward toss" reimplementation satisfies statistically and
/// violates per-seed. See the module doc comment.
#[must_use]
pub fn pop_resource_placement(pos: BlockPos, rng: &mut SpawnRng) -> (Vec3, Vec3) {
    // Draws 1-3: position jitter, x then y then z.
    let jitter_x = next_in_range(rng, -POP_SPREAD, POP_SPREAD);
    let jitter_y = next_in_range(rng, -POP_SPREAD, POP_SPREAD);
    let jitter_z = next_in_range(rng, -POP_SPREAD, POP_SPREAD);
    let position = Vec3::new(
        f64::from(pos.x) + 0.5 + jitter_x,
        f64::from(pos.y) + 0.5 + jitter_y - ITEM_HALF_HEIGHT,
        f64::from(pos.z) + 0.5 + jitter_z,
    );
    // Draws 4-5: horizontal velocity. `y` is the constant `0.2` and draws
    // nothing — the single easiest thing to get wrong here.
    let velocity_x = rng.next_f64() * (POP_VELOCITY_SPREAD * 2.0) - POP_VELOCITY_SPREAD;
    let velocity_z = rng.next_f64() * (POP_VELOCITY_SPREAD * 2.0) - POP_VELOCITY_SPREAD;
    let velocity = Vec3::new(velocity_x, POP_VELOCITY_Y, velocity_z);
    (position, velocity)
}

/// `Mth.nextDouble(random, min, max)` (`util/Mth.java:154-156`):
/// `random.nextDouble() * (max - min) + min`.
fn next_in_range(rng: &mut SpawnRng, min: f64, max: f64) -> f64 {
    if min >= max {
        return min;
    }
    rng.next_f64() * (max - min) + min
}

/// Rolls `block_state`'s loot table and returns one [`PoppedItem`] per
/// resulting stack — vanilla's `Block.dropResources` → `getDrops` →
/// `popResource` chain, for the empty loot context.
///
/// Empty for a block with no bundled table and for a table that rolled nothing.
/// Zero-count stacks are dropped, matching the `!itemStack.isEmpty()` guard in
/// vanilla's private `popResource` overload.
///
/// **The RNG is threaded, not re-seeded per stack.** A table that rolls three
/// stacks makes its own draws first and then `3 × 5` placement draws, in stack
/// order. That ordering is part of the spec for the same reason the five draws
/// inside one placement are.
#[must_use]
pub fn drop_block_loot(
    tables: &LootTableSet,
    block_state: &str,
    pos: BlockPos,
    rng: &mut SpawnRng,
) -> Vec<PoppedItem> {
    let Some(table_id) = block_loot_table_id(block_state) else {
        return Vec::new();
    };
    let Some(table) = tables.get(&table_id) else {
        return Vec::new();
    };
    let context = LootContext::default();
    table
        .roll(&context, rng)
        .into_iter()
        .filter(|stack| stack.count > 0)
        .map(|stack| {
            let (position, velocity) = pop_resource_placement(pos, rng);
            PoppedItem {
                stack,
                position,
                velocity,
            }
        })
        .collect()
}

/// Whether a player whose **feet** are at `player_feet` collects an item entity
/// whose feet are at `item_position`.
///
/// This is `Player.aiStep`'s test, not a radius:
/// `this.getBoundingBox().inflate(1.0, 0.5, 1.0)` intersected against the other
/// entity's box (`Player.java:457-474`, via `level().getEntities(this, area)`).
/// Two boxes, so **both** sets of half-extents contribute:
///
/// | axis | reach from the player's feet |
/// |---|---|
/// | x/z | `0.3` (player half-width) `+ 1.0` (inflate) `+ 0.125` (item half-width) = `1.425` |
/// | y, below | `0.5` (inflate) `+ 0.25` (item height) |
/// | y, above | `1.8` (player height) `+ 0.5` (inflate) |
///
/// Modelling it as a sphere of radius 1.0 around the feet — the obvious
/// simplification — is wrong in three separate ways: too short horizontally, far
/// too short upward, and it makes the volume isotropic when vanilla's is not.
/// A drop that has just come to rest sits at roughly `y + 0.125` relative to the
/// block top, well inside the vertical band, so the horizontal reach is what a
/// walking player actually notices.
#[must_use]
pub fn is_within_pickup_range(player_feet: Vec3, item_position: Vec3) -> bool {
    let reach_xz = PLAYER_HALF_WIDTH + PICKUP_INFLATE_XZ + ITEM_HALF_WIDTH;
    if (item_position.x - player_feet.x).abs() >= reach_xz {
        return false;
    }
    if (item_position.z - player_feet.z).abs() >= reach_xz {
        return false;
    }
    // The two y intervals must overlap: the item's box spans
    // `[y, y + ITEM_HEIGHT]`, the inflated player's `[feet - 0.5, feet + 1.8 + 0.5]`.
    let player_min_y = player_feet.y - PICKUP_INFLATE_Y;
    let player_max_y = player_feet.y + PLAYER_HEIGHT + PICKUP_INFLATE_Y;
    item_position.y + ITEM_HEIGHT > player_min_y && item_position.y < player_max_y
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mechanical half: a block state, with or without properties, resolves
    /// to the `blocks/`-prefixed key the bundled corpus is keyed by. The
    /// expected values come from the bundled JSON's own `random_sequence`
    /// fields (`"minecraft:blocks/stone"` etc.), which is Mojang's data naming
    /// the id rather than this crate restating a convention.
    #[test]
    fn a_block_state_resolves_to_its_vanilla_loot_table_id() {
        let id = |s: &str| block_loot_table_id(s).map(|key| key.to_string());
        assert_eq!(id("minecraft:stone").as_deref(), Some("minecraft:blocks/stone"));
        assert_eq!(id("stone").as_deref(), Some("minecraft:blocks/stone"));
        assert_eq!(
            id("minecraft:oak_log[axis=y]").as_deref(),
            Some("minecraft:blocks/oak_log"),
            "a loot table is keyed by block, not block state"
        );
        assert_eq!(
            id("minecraft:coal_ore").as_deref(),
            Some("minecraft:blocks/coal_ore")
        );
        assert_eq!(id(""), None);
        assert_eq!(id("minecraft:"), None);
    }

    /// **The exact predicted drop for every bundled block table**, under the
    /// empty loot context, with the reasoning for each written out.
    ///
    /// This is the *world*-species guard: the fixture is not "a block", it is
    /// the five bundled tables, and between them they exercise
    /// `minecraft:alternatives` (all but dirt), `match_tool` with a silk-touch
    /// enchantment predicate (all but dirt), `survives_explosion` (all),
    /// `table_bonus` on fortune (gravel), `apply_bonus`/`ore_drops` and
    /// `explosion_decay` (both ores). A fixture of stone alone would exercise
    /// alternatives and match_tool and *nothing else*, and would say nothing
    /// about whether an ore's bonus functions no-op correctly.
    ///
    /// Every expectation below is a **value**, not a sign or a non-emptiness:
    ///
    /// | block | drop | why, under the empty context |
    /// |---|---|---|
    /// | `stone` | `cobblestone` × 1 | silk-touch `match_tool` fails with no tool, so `alternatives` falls to the second child |
    /// | `dirt` | `dirt` × 1 | one unconditional pool, `survives_explosion` passes with no explosion |
    /// | `coal_ore` | `coal` × 1 | same fall-through; `apply_bonus`/`ore_drops` is a no-op at fortune 0, `explosion_decay` a no-op with no radius |
    /// | `iron_ore` | `raw_iron` × 1 | as `coal_ore` |
    ///
    /// Gravel is deliberately excluded here and gets its own test: it is the
    /// one bundled table with a genuinely random outcome.
    #[test]
    fn every_deterministic_bundled_block_drops_its_predicted_stack() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(10, 64, -3);
        // Each case is checked across many seeds, because "deterministic" is
        // the claim being tested — a single seed cannot distinguish a fixed
        // outcome from a lucky one.
        for (block, expected_item) in [
            ("minecraft:stone", "minecraft:cobblestone"),
            ("minecraft:dirt", "minecraft:dirt"),
            ("minecraft:coal_ore", "minecraft:coal"),
            ("minecraft:iron_ore", "minecraft:raw_iron"),
        ] {
            for seed in 0..64u64 {
                let mut rng = SpawnRng::new(seed);
                let drops = drop_block_loot(&tables, block, pos, &mut rng);
                assert_eq!(
                    drops.len(),
                    1,
                    "{block} must drop exactly one stack (seed {seed}), got {drops:?}"
                );
                assert_eq!(
                    drops[0].stack.item.to_string(),
                    expected_item,
                    "{block} at seed {seed}"
                );
                assert_eq!(
                    drops[0].stack.count, 1,
                    "{block} at seed {seed}: count is predicted, not merely non-zero"
                );
            }
        }
    }

    /// Gravel's `table_bonus` gives flint a `chances[0] = 0.1` probability at
    /// fortune 0, else gravel. The prediction here is the **pair of possible
    /// values and the rough split**, since the outcome is genuinely random:
    /// exactly one of two items, never anything else, never zero drops, and the
    /// flint share sits near a tenth rather than at either degenerate extreme.
    ///
    /// The bracket is what makes this more than a shape check: the two failure
    /// modes worth catching are "`table_bonus` always fails" (0% flint) and
    /// "`table_bonus` always passes" (100% flint), and both are excluded. The
    /// bounds are wide because 4,096 samples of a p=0.1 Bernoulli has a real
    /// spread (σ ≈ 0.0047, so ±0.03 is over six σ) — a tight bound here would
    /// be a flaky test, not a stronger one.
    #[test]
    fn gravel_drops_flint_about_a_tenth_of_the_time_and_gravel_otherwise() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(0, 70, 0);
        let samples = 4096;
        let mut flint = 0usize;
        let mut gravel = 0usize;
        for seed in 0..samples {
            let mut rng = SpawnRng::new(seed);
            let drops = drop_block_loot(&tables, "minecraft:gravel", pos, &mut rng);
            assert_eq!(drops.len(), 1, "gravel always drops exactly one stack");
            match drops[0].stack.item.to_string().as_str() {
                "minecraft:flint" => flint += 1,
                "minecraft:gravel" => gravel += 1,
                other => panic!("gravel dropped {other}, which is in neither branch of its table"),
            }
        }
        assert_eq!(flint + gravel, usize::try_from(samples).unwrap());
        let share = flint as f64 / samples as f64;
        assert!(
            (0.07..0.13).contains(&share),
            "flint share {share} is outside a tenth ± 0.03; \
             0.0 would mean table_bonus never passes and 1.0 that it always does \
             ({flint} flint, {gravel} gravel of {samples})"
        );
    }

    /// A block with no bundled table drops nothing, and does not panic. The
    /// honest behaviour: this crate bundles five block tables, so almost every
    /// block in the game takes this path today.
    #[test]
    fn a_block_with_no_bundled_table_drops_nothing() {
        let tables = LootTableSet::load_bundled();
        let drops = drop_block_loot(
            &tables,
            "minecraft:deepslate_emerald_ore",
            BlockPos::new(0, 0, 0),
            &mut SpawnRng::new(1),
        );
        assert!(drops.is_empty());
    }

    /// `popResource`'s geometry, predicted from the vanilla constants rather
    /// than observed: the jitter is `±0.25` on each axis about the block
    /// centre, y additionally carries `- 0.125`, and the velocity is `±0.1`
    /// horizontally with a **constant** `0.2` vertically.
    ///
    /// The `vy` assertion is exact and has no tolerance, which is the point:
    /// it is the one component that consumes no RNG draw, so a port that draws
    /// for it would produce a `vy` in `[0.1, 0.3)` here and fail — while still
    /// producing perfectly plausible-looking arcs on screen.
    #[test]
    fn a_popped_item_lands_in_vanillas_predicted_envelope() {
        let pos = BlockPos::new(4, 65, -7);
        for seed in 0..512u64 {
            let mut rng = SpawnRng::new(seed);
            let (position, velocity) = pop_resource_placement(pos, &mut rng);
            let centre_x = f64::from(pos.x) + 0.5;
            let centre_y = f64::from(pos.y) + 0.5 - ITEM_HALF_HEIGHT;
            let centre_z = f64::from(pos.z) + 0.5;
            assert!(
                (position.x - centre_x).abs() < POP_SPREAD,
                "seed {seed}: x {} outside centre {centre_x} ± {POP_SPREAD}",
                position.x
            );
            assert!(
                (position.y - centre_y).abs() < POP_SPREAD,
                "seed {seed}: y {} outside centre {centre_y} ± {POP_SPREAD} \
                 (centre already carries the -{ITEM_HALF_HEIGHT} half-height)",
                position.y
            );
            assert!(
                (position.z - centre_z).abs() < POP_SPREAD,
                "seed {seed}: z {} outside centre {centre_z} ± {POP_SPREAD}",
                position.z
            );
            assert!(velocity.x.abs() < POP_VELOCITY_SPREAD, "seed {seed}");
            assert!(velocity.z.abs() < POP_VELOCITY_SPREAD, "seed {seed}");
            assert_eq!(
                velocity.y, POP_VELOCITY_Y,
                "seed {seed}: vy is the constant 0.2 and consumes no draw; \
                 a port that draws for it lands in [0.1, 0.3) instead"
            );
        }
    }

    /// The draw *order* is pinned by a distinguishing property rather than by
    /// restating the numbers: with one shared stream, the sequence
    /// `(x, y, z, vx, vz)` means the five values are the first five
    /// `next_f64()`s of that stream, in that order. Recomputing them by hand
    /// from a fresh RNG and comparing is what a reordered or extra-draw port
    /// fails.
    ///
    /// This is the assertion that a statistical envelope check cannot make —
    /// see the module doc on why an extra `vy` draw is invisible to the test
    /// above.
    #[test]
    fn the_five_draws_happen_in_vanillas_order_from_one_stream() {
        let pos = BlockPos::new(0, 0, 0);
        let mut actual_rng = SpawnRng::new(0xD1CE);
        let (position, velocity) = pop_resource_placement(pos, &mut actual_rng);

        let mut expect_rng = SpawnRng::new(0xD1CE);
        let d1 = expect_rng.next_f64();
        let d2 = expect_rng.next_f64();
        let d3 = expect_rng.next_f64();
        let d4 = expect_rng.next_f64();
        let d5 = expect_rng.next_f64();

        assert_eq!(position.x, 0.5 + (d1 * 0.5 - 0.25), "draw 1 is the x jitter");
        assert_eq!(
            position.y,
            0.5 + (d2 * 0.5 - 0.25) - ITEM_HALF_HEIGHT,
            "draw 2 is the y jitter"
        );
        assert_eq!(position.z, 0.5 + (d3 * 0.5 - 0.25), "draw 3 is the z jitter");
        assert_eq!(velocity.x, d4 * 0.2 - 0.1, "draw 4 is vx");
        assert_eq!(velocity.z, d5 * 0.2 - 0.1, "draw 5 is vz");
    }

    /// The pickup volume's boundaries, each predicted from the vanilla AABBs
    /// rather than from a radius.
    ///
    /// The `1.3` row is the load-bearing one: it is inside vanilla's reach
    /// (`1.425`) and outside a naive `inflate`-only reach of `1.3` that forgets
    /// the *item's* own half-width — so a port that intersects the inflated
    /// player box against a **point** fails exactly here and nowhere else.
    #[test]
    fn the_pickup_volume_matches_vanillas_inflated_boxes() {
        let feet = Vec3::new(0.0, 64.0, 0.0);
        // An item resting on the floor the player stands on.
        let resting = |dx: f64, dz: f64| Vec3::new(dx, 64.0, dz);

        assert!(is_within_pickup_range(feet, resting(0.0, 0.0)));
        assert!(
            is_within_pickup_range(feet, resting(1.3, 0.0)),
            "1.3 is inside vanilla's 0.3 + 1.0 + 0.125 = 1.425 reach; \
             a point-vs-inflated-box test stops at 1.3 and fails here"
        );
        assert!(is_within_pickup_range(feet, resting(0.0, -1.4)));
        assert!(
            !is_within_pickup_range(feet, resting(1.43, 0.0)),
            "past the 1.425 reach"
        );
        assert!(!is_within_pickup_range(feet, resting(0.0, 1.5)));

        // Vertical: the band is asymmetric — 0.5 + item height below the feet,
        // 1.8 + 0.5 above them. A symmetric test passes on both halves of a
        // wrong implementation.
        assert!(is_within_pickup_range(feet, Vec3::new(0.0, 64.0 + 2.29, 0.0)));
        assert!(!is_within_pickup_range(feet, Vec3::new(0.0, 64.0 + 2.31, 0.0)));
        assert!(is_within_pickup_range(feet, Vec3::new(0.0, 64.0 - 0.7, 0.0)));
        assert!(
            !is_within_pickup_range(feet, Vec3::new(0.0, 64.0 - 0.8, 0.0)),
            "0.8 below puts the item's whole 0.25-tall box under the -0.5 floor"
        );
    }
}
