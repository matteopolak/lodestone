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
//! * **Tool-sensitive drops** are two separate mechanisms and conflating them is
//!   the trap (issue #539). `Silk Touch`/`Fortune`/`match_tool` are *loot*
//!   features, evaluated inside [`crate::loot`] against
//!   [`crate::LootContext::tool`], which [`drop_block_loot`] fills from its
//!   `held` argument. The **correct-tool** requirement is *not* a loot condition
//!   at all: it is [`drops_are_allowed`], vanilla's
//!   `Player.hasCorrectToolForDrops`, which the caller must consult *before*
//!   rolling — see that function's doc for why folding it into the roll is wrong
//!   twice.
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

use crate::loot::{LootBlockState, LootContext, LootTableSet, LootTool};
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

/// The loot context's `LootContextParams.BLOCK_STATE` for a block-state string —
/// the block's identity plus its **fully resolved** property set.
///
/// # Why this resolves through the census instead of reading the brackets
///
/// Vanilla's `StatePropertiesPredicate.PropertyMatcher.match` asks the block's
/// `StateDefinition` for the property and then reads the *state's* value, so
/// every property the block has contributes a value whether or not the caller
/// spelled it. Splitting `"minecraft:wheat[age=3]"` on commas would agree by luck
/// for anything `crate::growth_tick` wrote and disagree for a bare
/// `"minecraft:wheat"`, which is the same state as `age=0` and must fail an
/// `age=7` matcher *because zero is not seven* rather than because the string
/// happened to omit the property.
///
/// `lodestone_data::block_states::state_id` is the resolution — Mojang's own
/// 32,366-state table, with its default-plus-overrides fallback — and
/// `properties` reads the canonical `(name, value)` list straight back out.
///
/// Falls back to the properties written in the string for a state the census
/// cannot resolve at all. That path is the *conservative* direction rather than a
/// silent guess: a synthetic state this server invents (`minecraft:comparator`'s
/// `output=N`) keeps whatever it spelled, and a matcher over a property the census
/// would have supplied simply fails, dropping less rather than more.
#[must_use]
pub fn loot_block_state(block_state: &str) -> Option<LootBlockState> {
    let name = block_state
        .split_once('[')
        .map_or(block_state, |(name, _)| name)
        .trim();
    if name.is_empty() {
        return None;
    }
    let block: ResourceKey = if name.contains(':') {
        name.parse().ok()?
    } else {
        format!("minecraft:{name}").parse().ok()?
    };
    // The census is keyed by fully-qualified name, so a namespace-less input has
    // to be re-qualified before the lookup — and the bracket part carried across
    // verbatim, since `state_id`'s tiers are what turn a partial property list
    // into a real state.
    let query = match block_state.split_once('[') {
        Some((_, rest)) => format!("{block}[{rest}"),
        None => block.to_string(),
    };
    if let Some(properties) = lodestone_data::block_states::state_id(&query)
        .and_then(lodestone_data::block_states::properties)
    {
        return Some(LootBlockState::with_properties(block, properties.iter().copied()));
    }
    Some(LootBlockState::with_properties(block, parsed_properties(block_state)))
}

/// The `(name, value)` pairs literally written between the brackets of a
/// block-state string. Only the fallback path of [`loot_block_state`] uses this;
/// prefer the census, which supplies the properties a string omitted.
fn parsed_properties(block_state: &str) -> Vec<(&str, &str)> {
    let Some((_, raw)) = block_state.split_once('[') else {
        return Vec::new();
    };
    raw.strip_suffix(']')
        .unwrap_or(raw)
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect()
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
    (position, dropped_item_velocity(rng))
}

/// The `ItemEntity` constructor's own initial velocity — two horizontal draws
/// and a constant `0.2` upward (`ItemEntity.java`'s
/// `setDeltaMovement(random.nextDouble() * 0.2 - 0.1, 0.2, random.nextDouble() * 0.2 - 0.1)`).
///
/// Shared by [`pop_resource_placement`] (a block's drop) and mob death loot
/// (`Entity.spawnAtLocation`), which differ only in the *position*: a block's is
/// jittered inside its cell, a mob's is the mob's own position.
#[must_use]
pub fn dropped_item_velocity(rng: &mut SpawnRng) -> Vec3 {
    let velocity_x = rng.next_f64() * (POP_VELOCITY_SPREAD * 2.0) - POP_VELOCITY_SPREAD;
    let velocity_z = rng.next_f64() * (POP_VELOCITY_SPREAD * 2.0) - POP_VELOCITY_SPREAD;
    Vec3::new(velocity_x, POP_VELOCITY_Y, velocity_z)
}

/// Ticks a **player-thrown** stack cannot be picked back up for —
/// `LivingEntity.createItemStackToDrop`'s `entity.setPickUpDelay(40)`, and
/// **four times** the 10 a block pop uses (`setDefaultPickUpDelay`).
///
/// That difference is the whole point of the constant: at 10 ticks a player who
/// throws an item while walking forwards immediately walks into it and picks it
/// straight back up, which reads as "throwing does not work" even though the
/// entity really did spawn.
pub const THROWN_PICKUP_DELAY_TICKS: i16 = 40;

/// How far below eye level a thrown stack leaves the player's hand —
/// `createItemStackToDrop`'s `this.getEyeY() - 0.3F`.
pub const THROW_HAND_DROP: f64 = 0.3;

/// The forward impulse of a player throw, before the random spread —
/// `createItemStackToDrop`'s `0.3F`, applied to the look vector.
const THROW_POWER: f64 = 0.3;

/// Peak of the random horizontal spread cone, `0.02F * random.nextFloat()`.
const THROW_SPREAD: f64 = 0.02;

/// Constant lift added to a throw on top of the look vector's vertical
/// component — `+ 0.1F` in `createItemStackToDrop`.
const THROW_LIFT: f64 = 0.1;

/// Amplitude of the throw's vertical jitter,
/// `(random.nextFloat() - random.nextFloat()) * 0.1F`. Note this is a
/// **difference of two draws**, so it is triangular about zero and consumes two
/// numbers, not one.
const THROW_VERTICAL_JITTER: f64 = 0.1;

/// The velocity vanilla gives a stack a player throws out of their hand — `Q` /
/// `Ctrl+Q`, i.e. `LivingEntity.createItemStackToDrop`'s `randomly == false`
/// branch (26.2 `LivingEntity.java:3455-3465`):
///
/// ```text
/// vx = -sin(yaw)·cos(pitch)·0.3 + cos(dir)·spread
/// vy = -sin(pitch)·0.3 + 0.1 + (r₁ − r₂)·0.1
/// vz =  cos(yaw)·cos(pitch)·0.3 + sin(dir)·spread
/// ```
///
/// with `dir = r₃·2π` and `spread = 0.02·r₄`. Angles are degrees on the wire and
/// radians here, and the leading minus on `x` is Minecraft's yaw convention
/// (`yaw = 0` looks towards `+Z`, `90` towards `−X`) — dropping it throws items
/// behind the player's left shoulder, which still *looks* like a throw.
///
/// This is **not** [`dropped_item_velocity`]: a block pop is a near-vertical hop
/// with a tiny random horizontal offset and no notion of facing at all. Reusing it
/// for a throw drops the item at the player's feet.
///
/// # Draw order is load-bearing
///
/// Four draws, in this order: the two vertical-jitter floats, then the direction
/// angle, then the spread magnitude — matching the order the decompiled source
/// evaluates them in. A `SpawnRng` is a shared stream, so a reordering here
/// changes every later consumer's numbers as well as this one's.
#[must_use]
pub fn thrown_item_velocity(yaw_degrees: f32, pitch_degrees: f32, rng: &mut SpawnRng) -> Vec3 {
    let yaw = f64::from(yaw_degrees).to_radians();
    let pitch = f64::from(pitch_degrees).to_radians();
    // Vanilla evaluates `nextFloat() - nextFloat()` for the vertical jitter
    // *inside* the `setDeltaMovement` argument list, i.e. after `dir` and
    // `pow2` are bound. Java evaluates arguments left to right, so `y`'s two
    // draws come after those two — hence this order.
    let dir = rng.next_f64() * std::f64::consts::TAU;
    let spread = THROW_SPREAD * rng.next_f64();
    let jitter = (rng.next_f64() - rng.next_f64()) * THROW_VERTICAL_JITTER;
    Vec3::new(
        -yaw.sin() * pitch.cos() * THROW_POWER + dir.cos() * spread,
        -pitch.sin() * THROW_POWER + THROW_LIFT + jitter,
        yaw.cos() * pitch.cos() * THROW_POWER + dir.sin() * spread,
    )
}

/// The loot-table key for a mob's death drop — `LivingEntity.getLootTable`, whose
/// default is `EntityType`'s built-in `entities/<path>`
/// (`EntityType.Builder`'s `lootTable` supplier).
///
/// Same shape as [`block_loot_table_id`] one directory over, and the same
/// tolerance: a name that parses but has no bundled table misses in
/// [`LootTableSet::get`], which is where a missing table belongs.
#[must_use]
pub fn mob_loot_table_id(entity_type: &ResourceKey) -> Option<ResourceKey> {
    let path = entity_type.path();
    if path.is_empty() {
        return None;
    }
    format!("minecraft:entities/{path}").parse().ok()
}

/// `Mth.nextDouble(random, min, max)` (`util/Mth.java:154-156`):
/// `random.nextDouble() * (max - min) + min`.
fn next_in_range(rng: &mut SpawnRng, min: f64, max: f64) -> f64 {
    if min >= max {
        return min;
    }
    rng.next_f64() * (max - min) + min
}

/// `Player.hasCorrectToolForDrops` (`world/entity/player/Player.java:617-619`):
/// `!state.requiresCorrectToolForDrops() || selectedItem.isCorrectToolForDrops(state)`.
///
/// **This is not a loot condition** and deliberately does not live inside
/// [`drop_block_loot`]. Vanilla consults it in `ServerPlayerGameMode.destroyBlock`
/// (`:295`) and, when it is false, simply never calls `playerDestroy` →
/// `dropResources` at all — the block still breaks, and nothing drops. Folding it
/// into the table roll would look equivalent and be wrong twice: the roll's RNG
/// draws would still happen (shifting the stream for the next break), and a table
/// with no `match_tool` branch would still be consulted.
///
/// The whole computation already existed in [`lodestone_data::tool::mining`],
/// whose `correct_tool` field is *this* flag rather than the block's own
/// `requiresCorrectToolForDrops` — the two are routinely confused, and
/// `lodestone-shell`'s `sim.rs` carries the same warning for the mining-speed
/// divider. `held` is the main-hand stack; `None` is a bare hand.
///
/// Returns `true` for a block state this version's census does not know, which is
/// the same direction the pre-#539 behaviour took (everything dropped) and keeps
/// an unknown state from silently swallowing its drops.
#[must_use]
pub fn drops_are_allowed(block_state: &str, held: Option<&ItemStack>) -> bool {
    let Some(state_id) = crate::mobs::block_state_id(block_state) else {
        return true;
    };
    lodestone_data::tool::mining(held, state_id).is_none_or(|mining| mining.correct_tool)
}

/// Rolls `block_state`'s loot table and returns one [`PoppedItem`] per
/// resulting stack — vanilla's `Block.dropResources` → `getDrops` →
/// `popResource` chain.
///
/// `held` is the breaking player's main-hand stack, becoming the loot context's
/// `LootContextParams.TOOL`; `None` is a bare hand and reproduces the empty
/// context exactly. **Callers must gate on [`drops_are_allowed`] first** — this
/// function models `getDrops`, not `destroyBlock`, so it will happily roll a
/// stone table for a bare hand.
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
    held: Option<&ItemStack>,
    rng: &mut SpawnRng,
) -> Vec<PoppedItem> {
    drop_block_loot_in(tables, block_state, pos, held, None, rng)
}

/// Rolls `block_state`'s loot table for a block destroyed by an **explosion** of
/// `radius` — vanilla's `BlockBehaviour.onExplosionHit` for a
/// `DESTROY_WITH_DECAY` blast, which is what a creeper's is.
///
/// The only difference from [`drop_block_loot`] is
/// [`LootContext::explosion_radius`], and it is the difference between a faithful
/// blast and a duplication glitch: with the parameter absent every table's
/// `survives_explosion` condition passes unconditionally, so a creeper would drop
/// **every** block in its crater. With it set the condition keeps `1/radius` of
/// them and `explosion_decay` thins multi-item stacks item by item. `crate::loot`'s
/// own doc has the two transcribed record definitions.
///
/// Vanilla's `DESTROY` variant (a blast with `yield = 1.0`) exists too and passes
/// **no** radius; nothing in this crate produces one, so it is
/// [`drop_block_loot`] and not a third entry point.
#[must_use]
pub fn drop_explosion_loot(
    tables: &LootTableSet,
    block_state: &str,
    pos: BlockPos,
    radius: f32,
    rng: &mut SpawnRng,
) -> Vec<PoppedItem> {
    // No tool: a blast breaks the block, not a player, so `LootContextParams.TOOL`
    // is absent exactly as it is for a bare hand.
    drop_block_loot_in(tables, block_state, pos, None, Some(radius), rng)
}

/// Runs a blast's block half **with drops**: computes the exploded set, rolls
/// each destroyed block's table with the radius in the loot context, writes air,
/// and returns the block changes alongside the items to spawn.
///
/// # Chain reaction
///
/// A destroyed `minecraft:tnt` block is chain-primed rather than looted —
/// `TntBlock::wasExploded` — because `TntBlock.dropFromExplosion` is `false`:
/// vanilla replaces the loot roll for that cell with a fresh `PrimedTnt`
/// entirely, it does not also drop a TNT item. Its positions are the third
/// return value; the caller (`tick::run_tick_loop`) is what owns a
/// [`crate::mobs::MobSim`] to spawn into, so this function only reports where,
/// not the entity itself. Each one gets `PrimedTnt.getRandomShortFuse`'s
/// shortened fuse, not [`crate::mobs::tnt::DEFAULT_FUSE_TIME`] — see
/// [`crate::mobs::MobSim::spawn_tnt_short_fuse`].
///
/// # Why this lives here and not in `crate::explosion_blocks`
///
/// [`crate::explosion_blocks::destroy_blocks`] writes air *before* it returns, so
/// its caller can no longer see what was there — and the loot roll needs the old
/// state. Rather than change that function's contract, this walks
/// [`crate::explosion_blocks::exploded_positions`] itself and repeats its
/// three-line air-skip guard. The blast physics stay in one place; the loot
/// knowledge stays in this module, next to the table set it needs.
///
/// # Two RNG streams, on purpose
///
/// `blast_rng` feeds the ray march — one draw per ray, 1,352 of them — and its
/// draw count is the specification `explosion_blocks` gates against. `drops_rng`
/// feeds the loot tables and the `popResource` placement jitter. Sharing one
/// stream would make the crater shape depend on how many items happened to drop,
/// which is both wrong and untestable.
///
/// # The parity ceiling, stated rather than discovered later
///
/// Vanilla opens `interactWithBlocks` with `Util.shuffle(targetBlocks, random)` —
/// Fisher–Yates over a list built from a `HashSet<BlockPos>`, so its input order
/// is JVM hash-iteration order. That order is **not reproducible outside the
/// JVM** at any level of care. `exploded_positions` returns a sorted set instead,
/// so the *sequence* of drops here will not match a real server's. The **count**
/// and the **multiset** do, and those are what a gate should assert.
pub fn drop_explosion_loot_in_blast<S: crate::chunk::ChunkSource>(
    world: &S,
    env: crate::explosion_blocks::BlastEnv,
    centre: Vec3,
    radius: f32,
    tables: &LootTableSet,
    blast_rng: &mut SpawnRng,
    drops_rng: &mut SpawnRng,
) -> (Vec<(BlockPos, String)>, Vec<PoppedItem>, Vec<BlockPos>) {
    let mut changes = Vec::new();
    let mut popped = Vec::new();
    let mut primed_tnt = Vec::new();
    for pos in crate::explosion_blocks::exploded_positions(world, env, centre, radius, blast_rng) {
        let state = world.block_state(pos.x, pos.y, pos.z);
        if crate::random_tick::is_air_variant(&state) {
            continue;
        }
        if crate::mobs::tnt::is_tnt_block(&state) {
            primed_tnt.push(pos);
            world.set_block(pos.x, pos.y, pos.z, crate::chunk::AIR);
            changes.push((pos, crate::chunk::AIR.to_owned()));
            continue;
        }
        // Rolled **before** the write, because the table is keyed off the state
        // that is about to be destroyed.
        popped.extend(drop_explosion_loot(tables, &state, pos, radius, drops_rng));
        world.set_block(pos.x, pos.y, pos.z, crate::chunk::AIR);
        changes.push((pos, crate::chunk::AIR.to_owned()));
    }
    (changes, popped, primed_tnt)
}

/// The shared body of [`drop_block_loot`] and [`drop_explosion_loot`] — one
/// implementation so the two cannot drift in their placement draws or their
/// zero-count filtering.
fn drop_block_loot_in(
    tables: &LootTableSet,
    block_state: &str,
    pos: BlockPos,
    held: Option<&ItemStack>,
    explosion_radius: Option<f32>,
    rng: &mut SpawnRng,
) -> Vec<PoppedItem> {
    let Some(table_id) = block_loot_table_id(block_state) else {
        return Vec::new();
    };
    let Some(table) = tables.get(&table_id) else {
        return Vec::new();
    };
    let context = LootContext {
        luck: 0.0,
        tool: held.map(LootTool::from_held_item),
        explosion_radius,
        // Free: the state being broken is already this function's argument. It was
        // simply thrown away here, which is the whole of why every
        // `block_state_property` condition took the wrong branch.
        block_state: loot_block_state(block_state),
    };
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
                let drops = drop_block_loot(&tables, block, pos, None, &mut rng);
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
            let drops = drop_block_loot(&tables, "minecraft:gravel", pos, None, &mut rng);
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

    /// A block with no table at all drops nothing, and does not panic.
    ///
    /// Since #538 the bundle is the whole clean vanilla corpus, so this path is
    /// no longer "almost every block" — it is the blocks vanilla itself gives no
    /// loot table: `bedrock`, `barrier`, `air`/`cave_air`, the fluids,
    /// `end_portal`. Each row below is a real 26.2 block for which
    /// `.cache/mc/26.2/client-src/data/minecraft/loot_table/blocks/` has **no
    /// file**, checked rather than assumed, plus one unparseable name.
    ///
    /// This is also the *world*-species guard for the "no table" branch: a
    /// fixture naming a block that merely happens not to be bundled would stop
    /// exercising it the moment the bundle grew, which is exactly what happened
    /// to this test's previous subject (`deepslate_emerald_ore`, now bundled).
    #[test]
    fn a_block_with_no_bundled_table_drops_nothing() {
        let tables = LootTableSet::load_bundled();
        for block in [
            "minecraft:bedrock",
            "minecraft:barrier",
            "minecraft:water",
            "minecraft:lava",
            "minecraft:cave_air",
            "minecraft:end_portal",
        ] {
            assert!(
                tables
                    .get(&block_loot_table_id(block).expect("parses"))
                    .is_none(),
                "precondition: vanilla ships no loot table for {block}, so it must \
                 not be in the bundle either"
            );
            let drops = drop_block_loot(
                &tables,
                block,
                BlockPos::new(0, 0, 0),
                None,
                &mut SpawnRng::new(1),
            );
            assert!(drops.is_empty(), "{block} must drop nothing, got {drops:?}");
        }
        // And a name that is not a resource key at all.
        assert!(
            drop_block_loot(&tables, "minecraft:", BlockPos::new(0, 0, 0), None, &mut SpawnRng::new(1))
                .is_empty()
        );
    }

    /// A tool for these tests. Enchantments are attached by **key**, which is
    /// what [`LootTool`] carries — see its doc comment for why an id would not
    /// work here.
    fn tool(item: &str) -> ItemStack {
        ItemStack::new(item.parse().expect("valid item key"), 1)
    }

    /// `drop_block_loot` with an explicit [`LootTool`] rather than a bare
    /// `ItemStack`, so a test can name an enchantment level. Mirrors the
    /// production call exactly apart from the context construction.
    ///
    /// **Every field must keep mirroring [`drop_block_loot_in`].** This helper
    /// filled `block_state` with `None` for as long as the production path did,
    /// which was harmless only because everything it is pointed at (`stone`,
    /// `gravel`, the ores) has no `block_state_property` in its table — a test
    /// double complete enough to pass. Pointing it at a crop with the field still
    /// `None` would have reproduced the bug inside the gate.
    fn drop_with_tool(
        tables: &LootTableSet,
        block: &str,
        pos: BlockPos,
        tool: LootTool,
        rng: &mut SpawnRng,
    ) -> Vec<PoppedItem> {
        let table_id = block_loot_table_id(block).expect("test block name parses");
        let table = tables.get(&table_id).expect("test block has a bundled table");
        let context = LootContext {
            luck: 0.0,
            tool: Some(tool),
            explosion_radius: None,
            block_state: loot_block_state(block),
        };
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

    fn silk_touch() -> LootTool {
        LootTool::new("minecraft:diamond_pickaxe".parse().unwrap())
            .with_enchantment("minecraft:silk_touch".parse().unwrap(), 1)
    }

    fn fortune(level: u32) -> LootTool {
        LootTool::new("minecraft:diamond_pickaxe".parse().unwrap())
            .with_enchantment("minecraft:fortune".parse().unwrap(), level)
    }

    /// **Silk Touch flips every silk-gated table to the block itself, exactly,
    /// on every seed.**
    ///
    /// Each bundled block table except `dirt` is an `alternatives` whose *first*
    /// child is gated on a `match_tool` predicate requiring `minecraft:silk_touch`
    /// at `levels: {min: 1}`. With that tool the first child expands, so
    /// `alternatives` short-circuits and the second child is never reached — which
    /// makes the prediction degenerate and therefore exact: the item is the block,
    /// count 1, and `cobblestone`/`flint`/`coal`/`raw_iron` must **never** appear.
    ///
    /// The "never" half is the assertion that a plausible-but-wrong `match_tool`
    /// fails: a predicate that only checked "is anything in hand" would pass here
    /// too, so the *negative* row below (a plain pickaxe with no enchantment) is
    /// what separates them.
    #[test]
    fn silk_touch_drops_the_block_itself_and_never_the_processed_item() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(-8, 40, 12);
        for (block, silked, unsilked) in [
            ("minecraft:stone", "minecraft:stone", "minecraft:cobblestone"),
            ("minecraft:gravel", "minecraft:gravel", "minecraft:flint"),
            ("minecraft:coal_ore", "minecraft:coal_ore", "minecraft:coal"),
            ("minecraft:iron_ore", "minecraft:iron_ore", "minecraft:raw_iron"),
        ] {
            for seed in 0..256u64 {
                let mut rng = SpawnRng::new(seed);
                let drops = drop_with_tool(&tables, block, pos, silk_touch(), &mut rng);
                assert_eq!(drops.len(), 1, "{block} seed {seed}: {drops:?}");
                assert_eq!(
                    drops[0].stack.item.to_string(),
                    silked,
                    "{block} with silk touch at seed {seed} must never be {unsilked}"
                );
                assert_eq!(drops[0].stack.count, 1, "{block} seed {seed}");
            }
        }
    }

    /// **The negative row for the test above**, and the one that distinguishes a
    /// real `ItemPredicate` from "is anything in hand".
    ///
    /// A plain, unenchanted diamond pickaxe is a *present* tool, so
    /// `MatchTool.test`'s first clause (`tool != null`) passes — and the
    /// enchantment predicate must then fail, dropping the roll through to the
    /// second alternative. If `match_tool` were modelled as tool-presence alone,
    /// every row here would produce the block itself and the previous test would
    /// still pass.
    #[test]
    fn an_unenchanted_tool_is_present_but_does_not_satisfy_the_silk_touch_predicate() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(1, 2, 3);
        let pick = tool("minecraft:diamond_pickaxe");
        for (block, expected) in [
            ("minecraft:stone", "minecraft:cobblestone"),
            ("minecraft:coal_ore", "minecraft:coal"),
            ("minecraft:iron_ore", "minecraft:raw_iron"),
        ] {
            for seed in 0..64u64 {
                let mut rng = SpawnRng::new(seed);
                let drops = drop_block_loot(&tables, block, pos, Some(&pick), &mut rng);
                assert_eq!(drops.len(), 1);
                assert_eq!(
                    drops[0].stack.item.to_string(),
                    expected,
                    "{block} with a plain pickaxe at seed {seed}: an unenchanted \
                     tool must not satisfy a silk-touch enchantment predicate"
                );
            }
        }
    }

    /// **Fortune 3 on gravel drops flint on every single seed.**
    ///
    /// `gravel.json`'s `table_bonus` carries `chances: [0.1, 0.14285715, 0.25,
    /// 1.0]` on `minecraft:fortune`, and `BonusLevelTableCondition.test` reads
    /// `values[min(level, len - 1)]` — so at level 3 the chance is exactly `1.0`
    /// and `nextFloat() < 1.0` is true for every draw `nextFloat` can produce
    /// (its range is `[0, 1)`). That makes this the strongest single assertion
    /// available on this table: a degenerate but *exact* prediction, over every
    /// seed, with no bracket.
    ///
    /// The three lower levels are asserted as shares, because they are genuinely
    /// random — and each share is predicted from the table's own number, not from
    /// this crate's behaviour. Fortune 4 (above the list's length) must clamp to
    /// `values[3]` and so also be certain: that row is what a `values[level]`
    /// transliteration panics on.
    #[test]
    fn fortunes_table_bonus_reads_the_predicted_chance_for_its_level() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(0, 70, 0);
        // Levels 3 and 4 are certain, so assert them per-seed with no tolerance.
        for level in [3u32, 4] {
            for seed in 0..512u64 {
                let mut rng = SpawnRng::new(seed);
                let drops = drop_with_tool(&tables, "minecraft:gravel", pos, fortune(level), &mut rng);
                assert_eq!(drops.len(), 1);
                assert_eq!(
                    drops[0].stack.item.to_string(),
                    "minecraft:flint",
                    "fortune {level} makes gravel's chances[min(level, 3)] = 1.0, \
                     so flint is certain (seed {seed})"
                );
            }
        }
        // Levels 0-2 are chances[0..3] = 0.1 / 0.14285715 / 0.25. σ for the
        // widest of these over 8,192 samples is under 0.005, so ±0.03 is six σ.
        const SAMPLES: u64 = 8192;
        for (level, expected) in [(0u32, 0.1f64), (1, 0.142_857_15), (2, 0.25)] {
            let mut flint = 0usize;
            for seed in 0..SAMPLES {
                let mut rng = SpawnRng::new(seed);
                let drops = drop_with_tool(&tables, "minecraft:gravel", pos, fortune(level), &mut rng);
                assert_eq!(drops.len(), 1);
                match drops[0].stack.item.to_string().as_str() {
                    "minecraft:flint" => flint += 1,
                    "minecraft:gravel" => {}
                    other => panic!("gravel dropped {other} at fortune {level}"),
                }
            }
            let share = flint as f64 / SAMPLES as f64;
            assert!(
                (expected - 0.03..expected + 0.03).contains(&share),
                "fortune {level} flint share {share} is not near the table's own \
                 chances[{level}] = {expected} ({flint} of {SAMPLES})"
            );
        }
    }

    /// **`ore_drops`, predicted from the record body rather than a restatement of
    /// it.**
    ///
    /// `ApplyBonusCount.OreDrops.calculateNewCount`
    /// (`…/functions/ApplyBonusCount.java`) is:
    ///
    /// ```text
    /// if (level > 0) {
    ///    int bonus = random.nextInt(level + 2) - 1;
    ///    if (bonus < 0) bonus = 0;
    ///    return count * (bonus + 1);
    /// } else {
    ///    return count;
    /// }
    /// ```
    ///
    /// so with `count = 1` the multiplier is `max(nextInt(level + 2), 1)` and the
    /// **support** is exactly `1..=level+1` with `P(1) = 2/(level+2)` and
    /// `P(k) = 1/(level+2)` for every other `k`. Both halves are asserted:
    ///
    /// * a count above `level + 1` is impossible — this is the assertion a
    ///   "Fortune N means N+1 drops" implementation fails immediately;
    /// * every value in `1..=level+1` must actually occur, which is what a
    ///   `max(1, …)` applied to the wrong variable fails;
    /// * `P(1)` is **twice** every other outcome, which is the `- 1` then
    ///   `max(0)` clamp and nothing else. A formula without the clamp would make
    ///   all `level + 2` outcomes equally likely, so this ratio is the only thing
    ///   that separates the two — and both hypotheses are computed below rather
    ///   than one being asserted.
    #[test]
    fn ore_drops_produces_the_records_exact_support_and_doubled_first_outcome() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(3, 12, -40);
        const SAMPLES: u64 = 16_384;
        for level in 1u32..=3 {
            let max_count = level + 1;
            let mut histogram = vec![0usize; (max_count + 2) as usize];
            for seed in 0..SAMPLES {
                let mut rng = SpawnRng::new(seed);
                let drops = drop_with_tool(&tables, "minecraft:coal_ore", pos, fortune(level), &mut rng);
                assert_eq!(drops.len(), 1, "one pool, one roll");
                assert_eq!(drops[0].stack.item.to_string(), "minecraft:coal");
                let count = drops[0].stack.count;
                assert!(
                    (1..=max_count).contains(&count),
                    "fortune {level} produced coal x{count}; `count * (max(nextInt({}), 1))` \
                     cannot exceed {max_count}",
                    level + 2
                );
                histogram[count as usize] += 1;
            }
            for k in 1..=max_count {
                assert!(
                    histogram[k as usize] > 0,
                    "fortune {level}: count {k} is in the record's support and never occurred"
                );
            }
            // The two competing hypotheses, both computed from outside constants:
            //   clamped (correct): P(1) = 2/(level+2)
            //   unclamped (wrong): P(1) = 1/(level+2)
            let clamped = 2.0 / f64::from(level + 2);
            let unclamped = 1.0 / f64::from(level + 2);
            let observed = histogram[1] as f64 / SAMPLES as f64;
            assert!(
                (observed - clamped).abs() < (observed - unclamped).abs(),
                "fortune {level}: P(count == 1) measured {observed:.4}; the clamped \
                 record predicts {clamped:.4} and an unclamped nextInt would predict \
                 {unclamped:.4}, and the measurement must land on the former"
            );
            assert!(
                (clamped - 0.03..clamped + 0.03).contains(&observed),
                "fortune {level}: P(count == 1) measured {observed:.4}, predicted {clamped:.4}"
            );
        }
    }

    /// **`ore_drops` draws nothing at level 0, and that is a draw-*count* claim a
    /// distribution test structurally cannot make.**
    ///
    /// The record's guard is `if (level > 0)`, not `if (count > 1)`, so an
    /// unenchanted tool skips the formula entirely. The commonly-quoted
    /// restatement `count * max(1, nextInt(fortune + 2))` is arithmetically
    /// identical at level 0 (`max(1, nextInt(2))` is 1 or 1) and **draws once**
    /// — producing the same coal x1 and a different RNG stream for every later
    /// draw in the roll.
    ///
    /// The observable is `popResource`'s placement, which happens *after* the
    /// roll from the same stream: if `apply_bonus` consumed a draw, the drop
    /// would land somewhere else. So a bare hand and an unenchanted tool must
    /// produce **byte-identical positions**, and a fortune-1 tool (which does
    /// draw) must not.
    #[test]
    fn an_unenchanted_tool_costs_ore_drops_no_rng_draw_but_fortune_does() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(0, 64, 0);
        let plain = tool("minecraft:diamond_pickaxe");

        let mut a = SpawnRng::new(0x0DE_D00D);
        let bare = drop_block_loot(&tables, "minecraft:coal_ore", pos, None, &mut a);
        let mut b = SpawnRng::new(0x0DE_D00D);
        let unenchanted = drop_block_loot(&tables, "minecraft:coal_ore", pos, Some(&plain), &mut b);
        assert_eq!(
            bare[0].position, unenchanted[0].position,
            "`ore_drops` guards on `level > 0`, so an unenchanted tool must leave \
             the stream exactly where a bare hand does — a `max(1, nextInt(2))` \
             port would shift the placement draws by one"
        );
        assert_eq!(bare[0].velocity, unenchanted[0].velocity);

        let mut c = SpawnRng::new(0x0DE_D00D);
        let enchanted = drop_with_tool(&tables, "minecraft:coal_ore", pos, fortune(1), &mut c);
        assert_ne!(
            bare[0].position, enchanted[0].position,
            "fortune 1 *does* draw, so the placement must move — if it did not, \
             `apply_bonus` never ran"
        );
    }

    /// **`Player.hasCorrectToolForDrops`: a bare hand on stone drops nothing.**
    ///
    /// This is an *absence*, so the control is the row that must fail with the
    /// gate removed — see this test's own `wooden_pickaxe` row and
    /// `docs/block-drops.md`'s control table. Every expectation comes from
    /// `lodestone_data::tool`'s generated census (itself dumped from the real
    /// jar), not from this module.
    ///
    /// The two directions both matter and are asymmetric:
    ///
    /// * `minecraft:stone` **requires** a correct tool, so a bare hand and a
    ///   shovel are both refused while any pickaxe is allowed;
    /// * `minecraft:dirt` requires none, so a bare hand is allowed — and a gate
    ///   written as "you need a tool" rather than
    ///   "`!requires || tool_is_correct`" refuses it. That row is the one that
    ///   makes this more than a restatement of `requires_correct_tool`.
    #[test]
    fn the_correct_tool_gate_is_vanillas_and_not_a_requires_correct_tool_restatement() {
        let wooden = tool("minecraft:wooden_pickaxe");
        let shovel = tool("minecraft:diamond_shovel");

        assert!(
            !drops_are_allowed("minecraft:stone", None),
            "stone requires a correct tool, so a bare hand drops nothing — \
             vanilla `destroyBlock` never calls `dropResources`"
        );
        assert!(
            !drops_are_allowed("minecraft:stone", Some(&shovel)),
            "a shovel is not `mineable/pickaxe`, so it is the wrong tool for stone"
        );
        assert!(
            drops_are_allowed("minecraft:stone", Some(&wooden)),
            "the weakest pickaxe is still `correct_for_drops` on stone"
        );
        assert!(
            drops_are_allowed("minecraft:dirt", None),
            "dirt does not require a correct tool, so a bare hand drops it — \
             a gate reading `tool.is_some()` refuses this"
        );
        assert!(drops_are_allowed("minecraft:dirt", Some(&shovel)));
        // An unknown state must not silently swallow its drops.
        assert!(drops_are_allowed("minecraft:not_a_real_block", None));
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

    /// **The player-throw velocity, checked against the look vector's geometry
    /// rather than against a re-typed copy of the formula.**
    ///
    /// Re-deriving `-sin(yaw)·cos(pitch)·0.3` inside the assertion would be a
    /// closed loop: the same transcription error appears on both sides and the
    /// test agrees with the bug. So the expectation here comes from what the
    /// numbers *mean* — a `0.3`-long impulse along the direction the player is
    /// looking, in Minecraft's yaw convention (`0` looks towards `+Z`, `90`
    /// towards `−X`) — which is a property of the geometry and holds for any
    /// correct implementation.
    ///
    /// The four errors this actually catches, all of which produce a plausible
    /// non-zero throw:
    ///
    /// | mistake | what this sees |
    /// |---|---|
    /// | missing leading `−` on `x` | west becomes east |
    /// | `sin`/`cos` swapped on yaw | south becomes west |
    /// | degrees passed as radians | every component near the `+Z` axis |
    /// | `dropped_item_velocity` reused | ~0 horizontal, `+0.2` vertical |
    ///
    /// Tolerance is `THROW_SPREAD` (0.02) plus the vertical jitter (0.1), which
    /// are the only random terms — so it is the exact bound the formula permits,
    /// not a slack figure.
    #[test]
    fn a_thrown_stack_flies_the_way_the_player_is_looking() {
        let mut rng = SpawnRng::new(0x5EED);
        // Yaw 0, pitch 0: due +Z, level.
        let south = thrown_item_velocity(0.0, 0.0, &mut rng);
        assert!(
            (south.z - 0.3).abs() <= 0.02,
            "looking towards +Z must throw at +0.3 on z, got {south:?}"
        );
        assert!(
            south.x.abs() <= 0.02,
            "…and nothing sideways beyond the 0.02 spread, got {south:?}"
        );

        // Yaw 90: due −X. This is the assertion that fails if the leading minus
        // on `x` is dropped, and the one that fails if sin/cos are swapped.
        let west = thrown_item_velocity(90.0, 0.0, &mut rng);
        assert!(
            (west.x + 0.3).abs() <= 0.02,
            "yaw 90 looks towards −X, so x must be −0.3, got {west:?} — a positive x here is \
             the dropped sign, and Minecraft's yaw convention is the whole reason for it"
        );
        assert!(
            west.z.abs() <= 0.02,
            "…and nothing on z, got {west:?}"
        );

        // Pitch 90 is straight down: the impulse leaves the horizontal plane
        // entirely and the constant +0.1 lift is all that offsets it.
        let down = thrown_item_velocity(0.0, 90.0, &mut rng);
        assert!(
            (down.y - (-0.3 + 0.1)).abs() <= 0.1,
            "looking straight down must throw downwards at −0.3 + 0.1 lift, got {down:?}"
        );
        assert!(
            down.x.abs() <= 0.02 && down.z.abs() <= 0.02,
            "…with no horizontal component left, got {down:?}"
        );

        // And it is emphatically not the block-pop velocity, which has no notion
        // of facing at all. Predicting *both* hypotheses rather than asserting a
        // sign: the pop is +0.2 up with |horizontal| <= 0.1, the throw at level
        // pitch is +0.1 up with |z| ~ 0.3.
        let mut pop_rng = SpawnRng::new(0x5EED);
        let pop = dropped_item_velocity(&mut pop_rng);
        assert!(
            pop.z.abs() <= POP_VELOCITY_SPREAD && south.z.abs() > 0.25,
            "a block pop must stay within {POP_VELOCITY_SPREAD} horizontally while a throw \
             carries ~0.3; got pop {pop:?} and throw {south:?}. If these are close, the \
             throw is using the pop formula"
        );
    }

    /// [`loot_block_state`] hands the loot context the **whole** property set,
    /// filling in what the state string left out — the property that makes a
    /// `block_state_property` matcher behave like vanilla's, where
    /// `StateDefinition.getProperty` finds every property the block has whether or
    /// not the caller named it.
    ///
    /// The discriminating input is a *bare* name. A comma-splitting implementation
    /// answers "no properties" for `"minecraft:wheat"`, so an `age=7` matcher would
    /// fail for the right reason by accident and an
    /// `inverted { age=7 }` matcher would pass for the wrong one. Both readings
    /// agree on `"minecraft:wheat[age=7]"`, which is why that alone is not a test.
    #[test]
    fn loot_block_state_fills_in_the_properties_the_string_left_out() {
        let of = |state: &str| {
            let resolved = loot_block_state(state).expect("state names a block");
            let mut pairs: Vec<String> = resolved
                .properties
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            pairs.sort();
            (resolved.block.to_string(), pairs.join(","))
        };

        // A bare name is the block's default state, and wheat's default `age` is 0
        // — a value, not an absence.
        assert_eq!(of("minecraft:wheat"), ("minecraft:wheat".to_string(), "age=0".to_string()));
        assert_eq!(of("minecraft:wheat[age=7]"), ("minecraft:wheat".to_string(), "age=7".to_string()));
        // Namespace-less input, which the rest of this crate accepts.
        assert_eq!(of("wheat[age=5]"), ("minecraft:wheat".to_string(), "age=5".to_string()));
        // A partially-specified multi-property state keeps its named value and
        // takes the vanilla default for the rest — `state_id`'s tier 2.
        let (block, properties) = of("minecraft:oak_door[half=upper]");
        assert_eq!(block, "minecraft:oak_door");
        assert!(
            properties.contains("half=upper") && properties.contains("hinge="),
            "the unnamed properties must be present with their defaults, got {properties}"
        );
        // A block the census does not know keeps whatever the string said, rather
        // than losing the properties entirely.
        assert_eq!(
            of("modded:widget[colour=red]"),
            ("modded:widget".to_string(), "colour=red".to_string())
        );
        assert_eq!(loot_block_state(""), None);
    }

    /// **The reported bug, end to end through the production entry point.**
    /// Breaking fully-grown wheat with a bare hand pops one wheat and one seed;
    /// breaking it before it ripens pops one seed and nothing else.
    ///
    /// `drop_block_loot` is the function `server.rs`'s `StopDestroy` arm calls, so
    /// this is the whole path rather than the roller in isolation — the same
    /// `block_state` argument that picks the loot table now also fills
    /// `LootContextParams.BLOCK_STATE`, which is why the fix needed no new argument
    /// at any call site.
    #[test]
    fn breaking_ripe_wheat_pops_wheat_and_a_seed_and_unripe_wheat_pops_one_seed() {
        let tables = LootTableSet::load_bundled();
        let pos = BlockPos::new(4, 65, -7);
        let popped = |state: &str| {
            let mut rng = SpawnRng::new(BLOCK_DROPS_BEHAVIOR_SEED);
            drop_block_loot(&tables, state, pos, None, &mut rng)
                .into_iter()
                .map(|item| format!("{}x{}", item.stack.item, item.stack.count))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            popped("minecraft:wheat[age=7]"),
            vec![
                "minecraft:wheatx1".to_string(),
                "minecraft:wheat_seedsx1".to_string(),
            ],
        );
        // Every earlier age, collected rather than asserted in the loop so a
        // regression shows all seven rather than only `age=0`.
        let mut wrong = Vec::new();
        for age in 0..7 {
            let state = format!("minecraft:wheat[age={age}]");
            let got = popped(&state);
            if got != vec!["minecraft:wheat_seedsx1".to_string()] {
                wrong.push(format!("{state}: {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "unripe wheat must pop exactly one seed: {wrong:?}");

        // Wheat requires no tool, so `drops_are_allowed` is not what is being
        // measured here — but assert it, because a `false` would make the whole
        // thing moot at the call site rather than in this function.
        assert!(drops_are_allowed("minecraft:wheat[age=7]", None));
    }
}
