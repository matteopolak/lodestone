//! The random-tick scheduler (issue #307): which block positions get picked
//! for a random tick, how many per section per world tick, and the selection
//! loop every per-block-family handler dispatches from — grass turning to
//! dirt (and back), the block modeled here directly, plus crop growth,
//! sapling growth, and leaf decay (issue #310, `crate::growth_tick`), which
//! [`RandomTickScheduler::tick_chunk`]'s dispatch (see
//! `tick_randomly_ticking_block`) fans out to. Every one of these reaches a
//! real client through the same [`RandomTickEvent`]/`BlockTickFeed` path —
//! see this module's own "what reaches a client" note on
//! [`RandomTickScheduler::tick_chunk`] and `crate::growth_tick`'s module doc
//! for why crop/sapling/leaf blocks specifically have no *natural* producer
//! in this crate's worldgen yet, unlike grass (CLAUDE.md's own "nothing is
//! done until something on screen changes").
//!
//! # Selection, cited directly
//!
//! `ServerChunkCache.java:377,403` (the driver):
//!
//! ```text
//! int tickSpeed = this.level.getGameRules().get(GameRules.RANDOM_TICK_SPEED);
//! ...
//! this.chunkMap.forEachBlockTickingChunk(chunkx -> this.level.tickChunk(chunkx, tickSpeed));
//! ```
//!
//! `RANDOM_TICK_SPEED`'s default is `3`
//! (`GameRules.java:74`,
//! `registerInteger("random_tick_speed", GameRuleCategory.UPDATES, 3, 0)`)
//! — [`DEFAULT_RANDOM_TICK_SPEED`] below.
//!
//! `ServerLevel::tickChunk` (`ServerLevel.java:495-538`) is the per-chunk
//! body:
//!
//! ```text
//! for (int sectionIndex = 0; sectionIndex < sections.length; sectionIndex++) {
//!    LevelChunkSection section = sections[sectionIndex];
//!    if (section.isRandomlyTicking()) {
//!       int minYInSection = SectionPos.sectionToBlockCoord(sectionY);
//!       for (int i = 0; i < tickSpeed; i++) {
//!          BlockPos pos = this.getBlockRandomPos(minX, minYInSection, minZ, 15);
//!          BlockState blockState = section.getBlockState(...);
//!          if (blockState.isRandomlyTicking()) { blockState.randomTick(this, pos, this.random); }
//!          ...
//!       }
//!    }
//! }
//! ```
//!
//! Two things worth being exact about, because "a wrong number of draws
//! desynchronises everything downstream" (this issue's own brief):
//!
//! 1. **The position pick happens exactly `tickSpeed` times per
//!    randomly-ticking section, unconditionally** — whether or not the
//!    picked block turns out to be eligible. A miss still consumes a
//!    position draw; it just does nothing with it.
//! 2. **The position draw and the block's own behaviour draw from two
//!    different generators.** `getBlockRandomPos` (`Level.java:1064-1068`)
//!    advances `this.randValue`, a level-local 32-bit LCG seeded once at
//!    level creation — **not** `this.random` (the `RandomSource` passed into
//!    `blockState.randomTick`). [`next_random_tick_pos`] is the former;
//!    behaviour draws (e.g. grass's spread attempts, below) use a second,
//!    independent generator ([`RandomTickScheduler`]'s own `behavior_rng`).
//!
//! `LevelChunkSection::isRandomlyTicking` (`LevelChunkSection.java:110-118`)
//! is `tickingBlockCount > 0`, an incrementally maintained count vanilla
//! updates on every block change in the section. **This crate now keeps the
//! same counter** — `ChunkColumn::section_ticking`, one `u16` per implicit
//! 16-row window, maintained by `ChunkColumn::set_block` and recomputed once
//! per adopted grid by `recalc_ticking_counts` — so
//! [`RandomTickScheduler::tick_chunk`]'s gate is
//! `ChunkColumn::section_is_randomly_ticking`, an integer compare.
//!
//! It was a scan until `bdf93a28`+1. The history is worth keeping, because the
//! scan is still the *definition*: `is_randomly_ticking` ran on all 4096 blocks
//! of every section, of every column, on every tick, as a **string** predicate
//! — `sample(1)` put **97.6%** of the integrated server's tick thread in it.
//!
//! The budget arithmetic, corrected: this loop iterates `tick_area`, **not** the
//! streamed view. `tick_area` is `mob_area` (`integrated.rs:520`), whose radius
//! is the shell's `view_radius.clamp(1, 3)` (`net.rs:1773`) — a 7×7 square,
//! **49 columns**, as `integrated.rs:538` states independently. At the measured
//! 2.108 ms/column that is **103 ms per 50 ms tick, 2.07× over budget**; the
//! headroom is `50 / 2.108 = 23.7` columns and 49 exceeds it. (Earlier records,
//! including `bdf93a28`'s own commit message, multiplied by the 361-column
//! streamed view instead and reported 761 ms / 15.2×. Those two numbers are
//! wrong and must not be requoted — the conclusion they supported is not.)
//! Chunk delivery starved badly enough that rings 5-8 of the 289-column view
//! never arrived (issue #507, `docs/mesh-fill-rate.md`).
//!
//! The interim fix classified the palette once per tick and scanned palette
//! *indices*, 54× cheaper but still O(blocks) per column per tick. The counter
//! removes the per-tick scan entirely. [`section_has_randomly_ticking_block`]
//! survives as that definition, and the counters are checked against it by a
//! `debug_assert!` inside `tick_chunk` on every debug run.
//!
//! # `getBlockRandomPos`, cited directly
//!
//! `Level.java:1064-1068`:
//!
//! ```text
//! public BlockPos getBlockRandomPos(final int xo, final int yo, final int zo, final int yMask) {
//!    this.randValue = this.randValue * 3 + 1013904223;
//!    int val = this.randValue >> 2;
//!    return new BlockPos(xo + (val & 15), yo + (val >> 16 & yMask), zo + (val >> 8 & 15));
//! }
//! ```
//!
//! [`next_random_tick_pos`] is this, verbatim, using `i32::wrapping_mul`/
//! `wrapping_add` for the deliberate 32-bit overflow the Java `int` LCG
//! relies on.
//!
//! # Grass ↔ dirt, cited directly
//!
//! `SpreadingSnowyBlock.randomTick` (the class `GrassBlock` extends —
//! `SpreadingSnowyBlock.java:44-64`):
//!
//! ```text
//! if (!canStayAlive(state, level, pos)) {
//!    level.setBlockAndUpdate(pos, baseBlock.get().defaultBlockState());  // -> dirt, zero further draws
//! } else if (level.getMaxLocalRawBrightness(pos.above()) >= 9) {
//!    for (int i = 0; i < 4; i++) {
//!       BlockPos testPos = pos.offset(random.nextInt(3) - 1, random.nextInt(5) - 3, random.nextInt(3) - 1);
//!       if (level.getBlockState(testPos).is(baseBlock.get()) && canPropagate(defaultBlockState, level, testPos)) {
//!          level.setBlockAndUpdate(testPos, defaultBlockState.setValue(SNOWY, ...));
//!       }
//!    }
//! }
//! ```
//!
//! `canStayAlive` (`:29-41`) is now modelled for real — see
//! [`grass_can_stay_alive`]. It used to be proxied by "the block directly above
//! is bare air", which killed grass under **any** non-air block including
//! `minecraft:short_grass`, so every patch of grass the generator decorated
//! turned to dirt on its first random tick (issue #544). The proxy existed
//! because there was no per-state light-dampening census; `lodestone_data::light_props`
//! is that census, and the predicate is `dampening(above) < 15` with the
//! snow-layer-1 and full-fluid special cases ahead of it.
//!
//! **One simplification survives, and it is a different one**: the
//! `getMaxLocalRawBrightness(pos.above()) >= 9` gate on the *spread* branch.
//! This driver holds a `ChunkColumn`, not a light map, so the exact brightness
//! is unavailable rather than approximated, and a live grass block always
//! attempts a spread regardless of time of day. It can never make grass *die*
//! wrongly. The **draw pattern** is exact either way: `0` extra draws when
//! `canStayAlive` is false (dies to dirt), exactly `4 * 3 = 12` `next_int` calls
//! otherwise (four attempts, three axis offsets each), matching the jar's own
//! unconditional `for` loop — regardless of how many of the four attempts
//! actually hit a propagatable neighbour.
//!
//! Note this makes the draw count depend on **which** block is above, not merely
//! on whether one is: grass under short grass now consumes 12 draws where it
//! consumed 0. That is vanilla's behaviour for the same above-block, which is the
//! standard here — self-consistency is not.

use crate::gravity_tick;
use crate::growth_tick;
use crate::mob_spawn::SpawnRng;
use crate::neighbor_update::{Direction, NeighborPropagator, Notification, UPDATE_ORDER};
use crate::redstone;
use crate::redstone_diode;
use crate::redstone_dispenser;
use crate::redstone_note_block;
use crate::redstone_observer;
use crate::redstone_openable;
use crate::redstone_rail;
use crate::redstone_torch;
use crate::redstone_tripwire;
use crate::redstone_wire;
use crate::scheduled_tick::{ScheduledTickQueue, TickPriority};
use lodestone_model::BlockPos;

/// Vanilla's own default for the `random_tick_speed` gamerule
/// (`GameRules.java:74`). This crate has no gamerule registry yet (see
/// `crate::server`'s own module doc for why `GameRuleChanged` is currently
/// echoed rather than applied) — every caller of
/// [`RandomTickScheduler::tick_chunk`] passes a `tick_speed` explicitly
/// rather than reading this implicitly, but this is the value production
/// code should pass until a real gamerule store exists.
pub const DEFAULT_RANDOM_TICK_SPEED: u32 = 3;

/// The one block this crate models a real random tick for today. Mirrors
/// `BlockBehaviour.Properties.isRandomlyTicking` being set true only on
/// [`SpreadingSnowyBlock`]'s subclasses (`GrassBlock`, `MyceliumBlock`) —
/// note plain dirt is **not** in this set: dirt does not tick itself, it is
/// only ever a *target* of a neighbouring grass block's own tick.
const GRASS_BLOCK: &str = "minecraft:grass_block";
const DIRT_BLOCK: &str = "minecraft:dirt";
const MYCELIUM_BLOCK: &str = "minecraft:mycelium";
const PODZOL_BLOCK: &str = "minecraft:podzol";

/// `minecraft:lava` — the one **fluid** whose `isRandomlyTicking` is true.
///
/// `LavaFluid` overrides `Fluid::isRandomlyTicking` to return `true`; water never
/// does. Its `randomTick` is what sets fire to flammable blocks near lava, and it
/// is therefore the only thing in a generated world that starts a fire at all —
/// see [`RandomTickScheduler::tick_lava`].
const LAVA_BLOCK: &str = "minecraft:lava";

/// Strips any `[...]` block-state property suffix, mirroring every other
/// canonical-name comparison in this crate (`crate::chunk::is_air_or_fluid`,
/// `crate::chunk::is_water`).
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// `true` for any air variant (`minecraft:air`/`cave_air`/`void_air`) —
/// narrower than [`crate::chunk::is_air_or_fluid`], which also counts
/// fluids. Still this module's light-level proxy for **crops and saplings**
/// (`crate::growth_tick`); grass no longer uses it — see
/// [`grass_can_stay_alive`].
#[must_use]
pub fn is_air_variant(state: &str) -> bool {
    matches!(base_name(state), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
}

/// `true` iff `state`'s fluid state is **full** — vanilla's
/// `BlockState.getFluidState().isFull()`, i.e. `amount == 8`.
///
/// Three cases, and the third is the one a `base_name == "water"` test misses:
///
/// * a source liquid, `minecraft:water[level=0]` / `minecraft:lava[level=0]`:
///   `LiquidBlock.getFluidState` maps `level` to `amount = 8 - level` for
///   `level < 8`, so only `level=0` is full;
/// * a **falling** liquid, `level=8..=15`: those map to `amount = 8` and are
///   full, which is why the check cannot be `level == 0`;
/// * any state carrying `waterlogged=true` — a waterlogged slab, stair or
///   fence has a full water fluid state even though its *block* is not water.
///   Vanilla's `canStayAlive` reads the fluid state, not the block, so
///   waterlogged-anything above grass kills it.
#[must_use]
pub fn has_full_fluid(state: &str) -> bool {
    if property_of(state, "waterlogged") == Some("true") {
        return true;
    }
    if !matches!(base_name(state), "minecraft:water" | "minecraft:lava") {
        return false;
    }
    match property_of(state, "level") {
        // A bare `minecraft:water` with no properties is the default state,
        // `level=0`, so full.
        None => true,
        Some(level) => level
            .parse::<u32>()
            .is_ok_and(|level| level == 0 || level >= 8),
    }
}

/// The value of `state`'s `key=` property, if the state string carries one.
///
/// Deliberately a substring scan rather than a parse: this module already keys
/// everything off the canonical state string [`crate::chunk::ChunkColumn`]
/// stores, and a `key=value` lookup over `a[k=v,k2=v2]` needs no more than
/// that. Matches on the whole key, so `waterlogged` cannot be found inside
/// another property's name or value.
fn property_of<'s>(state: &'s str, key: &str) -> Option<&'s str> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// Vanilla `SpreadingSnowyBlock.canStayAlive`
/// (`.cache/mc/26.2/src/net/minecraft/world/level/block/SpreadingSnowyBlock.java:28-41`),
/// given the canonical state string of the block **directly above** the grass
/// block.
///
/// # The proxy this replaces, and why it was a bug
///
/// Until issue #544 this module used `is_air_variant(above)` for `canStayAlive`,
/// so **any** non-air block above killed the grass. `minecraft:short_grass` is
/// non-air, and vanilla's own vegetation step places short grass on top of grass
/// blocks — so every patch of grass the generator decorated turned to dirt on its
/// first random tick, which is exactly what the owner reported seeing. The
/// generation side was innocent: `feature/top_layer.rs` and
/// `feature/vegetation/` place `grass_block` with `short_grass` above it, as
/// vanilla does.
///
/// The proxy existed because there was no dampening census. There is one now
/// (`lodestone_data::light_props`, landed in `3f26be21`), so this is the real
/// predicate:
///
/// ```text
/// BlockState aboveState = level.getBlockState(pos.above());
/// if (aboveState.is(Blocks.SNOW) && aboveState.getValue(LAYERS) == 1) return true;
/// if (aboveState.getFluidState().isFull()) return false;
/// int d = LightEngine.getLightDampeningInto(state, aboveState, Direction.UP, aboveState.getLightDampening());
/// return d < 15;
/// ```
///
/// Note the order: the snow special case wins over the fluid check, and both win
/// over the dampening comparison. `getLightDampening()` is exactly
/// [`lodestone_data::light_props::dampening`]'s column, and the comparison is
/// `< 15` — strictly, so a full solid (15) kills and everything below it does not.
///
/// # The one branch not modelled
///
/// `getLightDampeningInto` returns a hard `16` — killing the grass — when the two
/// states' occlusion *shapes* merge to a fully-occluding face. That path is only
/// reachable when the block above satisfies `canOcclude() &&
/// useShapeForLightOcclusion()`, i.e. an occluding block that is *not* a full
/// cube (stairs, some slabs). This crate has no occlusion-shape census — only
/// collision shapes, which are a different question (glass has a full collision
/// box and occludes no light) — so those states fall through to their `dampening`
/// column instead. **This can only ever make grass survive where vanilla would
/// kill it**, never the reverse, which is the safe direction and is why it is a
/// documented gap rather than a guess. Adding an occlusion-shape census is the
/// prerequisite for closing it.
///
/// An unresolvable state string is treated as air-like (survives), for the same
/// reason: it cannot destroy a block the player is looking at.
#[must_use]
pub fn grass_can_stay_alive(above_state: &str) -> bool {
    // 1. `aboveState.is(Blocks.SNOW) && LAYERS == 1` — an explicit `true` that
    //    precedes both other checks. A single snow layer is *thin enough to see
    //    through*, so grass under fresh snowfall keeps its `snowy=true` state
    //    instead of dying.
    if base_name(above_state) == "minecraft:snow" && property_of(above_state, "layers") == Some("1")
    {
        return true;
    }
    // 2. `aboveState.getFluidState().isFull()` — drowned grass dies. Checked
    //    before dampening because water's own dampening is 1, which would
    //    otherwise pass.
    if has_full_fluid(above_state) {
        return false;
    }
    // 3. `getLightDampeningInto(...) < 15`.
    match crate::mobs::block_state_id(above_state) {
        Some(id) => lodestone_data::light_props::dampening(id) < 15,
        None => true,
    }
}

/// Vanilla `SnowyBlock.isSnowySetting` (`SnowyBlock.java:53-55`) —
/// `state.is(BlockTags.SNOW)`, which is **three** blocks in 26.2
/// (`data/minecraft/tags/block/snow.json`), given the state directly above.
///
/// Deliberately *not* shared with [`grass_can_stay_alive`]'s own snow branch,
/// which is the narrower `is(Blocks.SNOW) && LAYERS == 1`: the two predicates
/// live in the same vanilla class and are different on purpose.
#[must_use]
pub fn is_snowy_setting(above_state: &str) -> bool {
    matches!(
        base_name(above_state),
        "minecraft:snow" | "minecraft:snow_block" | "minecraft:powder_snow"
    )
}

/// `defaultBlockState().setValue(SNOWY, isSnowySetting(above))` for a
/// `SpreadingSnowyBlock` — the write vanilla performs both when grass spreads
/// (`SpreadingSnowyBlock.java:63`) and when the block above one changes
/// (`SnowyBlock.updateShape`, `SnowyBlock.java:41-45`).
///
/// The property is not optional. `v770`'s `resolve_state_id` resolves a bare
/// name to the block's default state, so a bare `minecraft:grass_block` is
/// *now* correct on the wire — but it is still the wrong value half the time,
/// and the server's own state string is what everything downstream reads.
#[must_use]
fn spreading_snowy_state(block: &str, above_state: &str) -> &'static str {
    match (block, is_snowy_setting(above_state)) {
        (GRASS_BLOCK, true) => "minecraft:grass_block[snowy=true]",
        (GRASS_BLOCK, false) => "minecraft:grass_block[snowy=false]",
        (MYCELIUM_BLOCK, true) => "minecraft:mycelium[snowy=true]",
        (MYCELIUM_BLOCK, false) => "minecraft:mycelium[snowy=false]",
        (PODZOL_BLOCK, true) => "minecraft:podzol[snowy=true]",
        _ => "minecraft:podzol[snowy=false]",
    }
}

/// The three blocks carrying `BlockStateProperties.SNOWY` in 26.2 — exactly the
/// six states `lodestone_data::snow_support::has_snowy_property` marks. Only
/// `grass_block` is spread-ticked by this crate today (see [`GRASS_BLOCK`]);
/// the other two still need their `snowy` kept current when snow lands on or
/// leaves them.
const SNOWY_FAMILY: [&str; 3] = [GRASS_BLOCK, MYCELIUM_BLOCK, PODZOL_BLOCK];

/// `true` iff `block_state` is one this crate models a random tick for.
/// Mirrors `BlockState.isRandomlyTicking()`
/// (`BlockBehaviour.java:401-402`) — grass/mycelium-family spreading (see
/// [`GRASS_BLOCK`]'s doc comment for why dirt is deliberately excluded), plus
/// the three families issue #310 added: crop growth, sapling growth, and
/// leaf decay, all cited in `crate::growth_tick`'s own module doc comment.
#[must_use]
pub fn is_randomly_ticking(block_state: &str) -> bool {
    #[cfg(test)]
    predicate_calls::bump();
    base_name(block_state) == GRASS_BLOCK
        || base_name(block_state) == LAVA_BLOCK
        || growth_tick::is_growable_crop(block_state)
        || growth_tick::is_sapling(block_state)
        || growth_tick::leaves_should_decay(block_state)
}

/// An instrument, not a mechanism: how many times [`is_randomly_ticking`] has
/// been evaluated on **this thread**.
///
/// Issue #507's fix is a claim about an operation *count*, and this repo's
/// evidence rule says to measure a count rather than a duration (this machine's
/// wall clock reproduces to 10.8% at best, and one stage swung 22% across three
/// runs of an identical binary). The two competing hypotheses are computable
/// exactly: with the counters, `tick_chunk` on an already-built column performs
/// **0** evaluations; without them it performs at least `palette.len()` per
/// tick (the interim mask) or 4096 per section (the original scan). A gate that
/// lands on 0 therefore distinguishes them with no tolerance at all.
///
/// Thread-local rather than a global `AtomicU64` on purpose: the lib test binary
/// runs unit tests concurrently, and a shared global would make every count a
/// race. `cfg(test)`-only, so the instrument cannot exist in a build anything
/// ships.
#[cfg(test)]
mod predicate_calls {
    use std::cell::Cell;

    thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }

    pub(super) fn bump() {
        CALLS.with(|c| c.set(c.get() + 1));
    }

    /// Reads the current count for this thread.
    pub(super) fn get() -> u64 {
        CALLS.with(Cell::get)
    }
}

/// The position-pick LCG, verbatim from `Level.getBlockRandomPos`
/// (`Level.java:1064-1068`) — see this module's doc comment for the exact
/// citation. `state` is vanilla's `randValue` field: a 32-bit value that
/// persists across every call for the lifetime of the level (seeded once,
/// arbitrarily, at level creation in vanilla; callers here choose their own
/// seed via [`RandomTickScheduler::new`]).
///
/// Returns the picked `(x, y, z)` in world coordinates and advances `state`
/// in place.
#[must_use]
pub fn next_random_tick_pos(
    state: &mut i32,
    xo: i32,
    yo: i32,
    zo: i32,
    y_mask: i32,
) -> (i32, i32, i32) {
    *state = state.wrapping_mul(3).wrapping_add(1013904223);
    let val = *state >> 2;
    (xo + (val & 15), yo + ((val >> 16) & y_mask), zo + ((val >> 8) & 15))
}

/// One random tick that actually changed a block, as returned by
/// [`RandomTickScheduler::tick_chunk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomTickEvent {
    pub pos: (i32, i32, i32),
    pub from: String,
    pub to: String,
}

/// The outcome of one [`grass_random_tick`] call, before any world mutation
/// is applied — kept separate from [`RandomTickEvent`] so the pure decision
/// (this function) stays testable with no `ChunkColumn`/`ChunkSource` in
/// scope at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrassOutcome {
    /// `canStayAlive` was false: convert to dirt. Zero further RNG draws.
    DiesToDirt,
    /// Air-exposed; all four spread attempts (12 draws total) were issued,
    /// but `try_propagate` accepted none of them.
    NoPropagationTargetAccepted,
    /// Air-exposed; the listed relative offsets (a subset of the four
    /// attempted, in attempt order) were accepted by `try_propagate` and
    /// should become grass.
    Spreads(Vec<(i32, i32, i32)>),
}

/// The pure grass ↔ dirt decision — see this module's doc comment for the
/// jar citation and the one check this crate still cannot make.
///
/// `can_stay_alive` is vanilla's `canStayAlive` verdict, which the driver
/// computes with [`grass_can_stay_alive`]. It used to be the parameter
/// `above_is_air`, a proxy that killed grass under *any* non-air block —
/// including `minecraft:short_grass`, which vanilla's own vegetation step
/// places on top of grass blocks. That is issue #544.
///
/// **`can_stay_alive` still doubles as the `getMaxLocalRawBrightness(pos.above())
/// >= 9` gate**, which is a *different* simplification from the one #544 removed
/// and remains: this crate's random-tick driver holds a `ChunkColumn`, not a light
/// map, so the exact brightness is unavailable rather than approximated. The
/// consequence is that a live grass block always attempts a spread regardless of
/// time of day, never that it dies wrongly. The **draw pattern** is exact either
/// way: `0` extra draws when `canStayAlive` is false, exactly `4 * 3 = 12`
/// `next_int` calls otherwise, matching the jar's unconditional `for` loop.
///
/// `try_propagate` is called for each of the four attempts' relative
/// `(dx, dy, dz)` offset (already drawn from `rng` before the call, exactly like
/// vanilla's `pos.offset(random.nextInt(3) - 1, ...)`) and must itself decide
/// whether the target position is a valid spread destination — a `ChunkColumn`
/// lookup this pure function does not perform, so it stays testable with a fake
/// world.
pub fn grass_random_tick(
    can_stay_alive: bool,
    rng: &mut SpawnRng,
    mut try_propagate: impl FnMut(i32, i32, i32) -> bool,
) -> GrassOutcome {
    if !can_stay_alive {
        return GrassOutcome::DiesToDirt;
    }
    let mut spreads = Vec::new();
    for _ in 0..4 {
        let dx = rng.next_int(3) - 1;
        let dy = rng.next_int(5) - 3;
        let dz = rng.next_int(3) - 1;
        if try_propagate(dx, dy, dz) {
            spreads.push((dx, dy, dz));
        }
    }
    if spreads.is_empty() {
        GrassOutcome::NoPropagationTargetAccepted
    } else {
        GrassOutcome::Spreads(spreads)
    }
}

/// `true` iff a dirt block at the target offset can become grass — vanilla's
/// `canPropagate` (`SpreadingSnowyBlock.java:43-46`):
///
/// ```text
/// return canStayAlive(state, level, pos) && !level.getFluidState(pos.above()).is(FluidTags.WATER);
/// ```
///
/// So two conditions on the block above the *target*, not one, and the second is
/// not implied by the first: `canStayAlive` rejects a **full** fluid, while
/// `canPropagate` additionally rejects any water fluid at all — flowing water
/// included. Grass therefore does not spread into a shallow stream it *could*
/// survive under. Plus this crate's own precondition that the target is dirt,
/// which is vanilla's `level.getBlockState(testPos).is(baseBlock)` at the call
/// site.
///
/// Before issue #544 this was `is_air_variant(above_target_state)`, which
/// collapsed both conditions into the same proxy [`grass_can_stay_alive`]
/// documents.
#[must_use]
pub fn can_propagate_onto(target_state: &str, above_target_state: &str) -> bool {
    base_name(target_state) == DIRT_BLOCK
        && grass_can_stay_alive(above_target_state)
        && base_name(above_target_state) != "minecraft:water"
        && property_of(above_target_state, "waterlogged") != Some("true")
}

/// The random-tick driver: owns the two independent generators
/// `ServerLevel` keeps (the position LCG and the behaviour RNG — see this
/// module's doc comment for why they must stay separate), and drives
/// [`grass_random_tick`] against a real [`crate::chunk::ChunkColumn`].
#[derive(Debug, Clone)]
pub struct RandomTickScheduler {
    /// Vanilla's `Level.randValue` — see [`next_random_tick_pos`].
    position_state: i32,
    /// A generator independent of `position_state`, standing in for
    /// vanilla's `ServerLevel.random` (the `RandomSource` passed into every
    /// `BlockState.randomTick`). Not vanilla's actual PRNG algorithm — see
    /// this module's doc comment: only the **draw pattern** (how many calls,
    /// in what order) is asserted anywhere in this crate, never the literal
    /// values, so a different (but still deterministic) generator is a
    /// faithful stand-in.
    behavior_rng: SpawnRng,
}

impl RandomTickScheduler {
    /// Seeds both generators. `position_seed` feeds `position_state`
    /// directly (vanilla seeds `randValue` from an arbitrary thread-local
    /// draw at level creation — this crate takes the seed explicitly so
    /// tests and the tick loop can be deterministic); `behavior_seed` seeds
    /// [`SpawnRng`].
    #[must_use]
    pub fn new(position_seed: i32, behavior_seed: u64) -> Self {
        Self { position_state: position_seed, behavior_rng: SpawnRng::new(behavior_seed) }
    }

    /// Vanilla's `Level.randValue` as it stands now — the position LCG's whole
    /// state, and therefore the exact number of position draws that have
    /// happened since [`new`](Self::new).
    ///
    /// Read-only, and it exists for one observation a gate cannot make any other
    /// way: `tests/random_tick_section_counters.rs` replays the expected draw
    /// sequence from `next_random_tick_pos` alone (never consulting the section
    /// counters) and compares the two states exactly. Since the per-(column,
    /// section, tick) boolean is the *only* input that decides whether draws
    /// happen, an equal final state is proof that the O(1) counter decision put
    /// the LCG on the same sequence the definitional scan would have — which is
    /// the real compatibility requirement of issue #507's fix, not merely that
    /// the same blocks changed.
    #[must_use]
    pub fn position_state(&self) -> i32 {
        self.position_state
    }

    /// One chunk's worth of random ticks at `tick_speed` picks per
    /// randomly-ticking 16-block section — mirrors `ServerLevel::tickChunk`'s
    /// block-ticking loop (`ServerLevel.java:508-535`; this crate does not
    /// model the `iceandsnow`/`tickPrecipitation` loop above it, which is
    /// weather, out of scope for #307/#308).
    ///
    /// `column` is read fresh from `source.column(cx, cz)` by the caller and
    /// passed in as `&mut` so within-call mutations (a grass block spreading
    /// onto a dirt block earlier in the same call) are visible to later
    /// picks in the same call — matching vanilla's `section.getBlockState`
    /// reading the live, already-mutated section array mid-`tickChunk`.
    /// Every mutation this function makes to `column` is also returned as a
    /// [`RandomTickEvent`] so the caller can persist it through
    /// [`crate::chunk::ChunkSource::set_block`] and notify a connected
    /// client — `column` alone is not persisted by this function.
    ///
    /// The per-position dispatch (`tick_randomly_ticking_block`) fans out to
    /// grass (this module) or crop/sapling/leaves (`crate::growth_tick`,
    /// issue #310) — every family returns through this same `Vec`, so this
    /// function's caller (`tick::run_tick_loop`) needed zero changes to gain
    /// the new families: it already forwards whatever `tick_chunk` hands
    /// back, generically, one block-state string at a time.
    ///
    /// `block_ticks`/`current_tick` (issue #314's own extension of this call
    /// site) are threaded through to [`propagate_and_react`] so a mutation
    /// adjacent to a redstone torch/repeater/comparator/observer can
    /// schedule a delayed recheck — see that function's own doc comment.
    /// `tick::run_tick_loop` (the real caller) passes its own persistent
    /// `block_ticks` queue and `game_tick` counter; nothing here owns either.
    pub fn tick_chunk(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        cx: i32,
        cz: i32,
        tick_speed: u32,
        block_ticks: &mut ScheduledTickQueue<String>,
        current_tick: u64,
    ) -> Vec<RandomTickEvent> {
        let mut events = Vec::new();
        if tick_speed == 0 {
            return events;
        }
        let min_x = cx * 16;
        let min_z = cz * 16;
        // Vanilla's `tickingBlockCount > 0`, now an O(1) integer compare
        // against the counter `ChunkColumn` maintains on every mutation
        // (issue #507's real fix). Nothing here reads the index grid at all:
        // the whole-column early exit below is at most 24 compares, and the
        // per-section decision is one.
        //
        // **Fluids are deliberately out of scope, and this is the boundary.**
        // Vanilla's gate is `isRandomlyTickingBlocks() || isRandomlyTickingFluids()`
        // (`LevelChunkSection.java:110-112`), and lava is the one fluid whose
        // `isRandomlyTicking()` is true (`LavaFluid.java:221` overrides
        // `Fluid.java:79`'s `false`; water never overrides). This crate models
        // no fluid random ticks — `is_randomly_ticking` names no fluid — so a
        // `tickingFluidCount` today would have zero producers and zero
        // consumers: an island by construction. The disclosed consequence is
        // that our LCG position stream is not vanilla-comparable for a section
        // whose only ticking content is lava, unchanged by the counters. When a
        // lava `randomTick` handler first lands, the same change must (1) add
        // the fluid counter maintained at `ChunkColumn`'s same three sites and
        // (2) widen *this* condition to the OR. See
        // `docs/plans/random-tick-counter.md` §"Fluids".
        if !column.has_randomly_ticking_block() {
            return events;
        }
        // The definitional scan, kept as the tripwire's reference arm below.
        // Debug builds only, so every `cargo test` run in this repo pays for it
        // and no release build does.
        #[cfg(debug_assertions)]
        let definitional_mask = randomly_ticking_palette_mask(column);
        let mut section_min_y = column.min_y;
        while section_min_y < column.min_y + column.height {
            let section_ticks = column.section_is_randomly_ticking(section_min_y);
            // Permanent debug tripwire. The counters are maintained by
            // `ChunkColumn::set_block`/`recalc_ticking_counts`; any future
            // mutation path inside `chunk.rs` that reaches `blocks` without
            // updating them desyncs silently in release, and this fails the
            // nearest debug run at the point of *consumption*, naming the
            // section. The reference is the same index scan that shipped as the
            // interim fix, so this is the one comparison that keeps the O(1)
            // decision bit-for-bit identical to the definition — and therefore
            // keeps the `tick_speed` position draws on the same LCG sequence.
            #[cfg(debug_assertions)]
            debug_assert_eq!(
                section_ticks,
                section_has_randomly_ticking_block(column, section_min_y, &definitional_mask),
                "random-tick counter desync at chunk ({cx}, {cz}) section_min_y {section_min_y}: \
                 counters say {section_ticks}, the definitional index scan disagrees — some \
                 mutation path bypassed `ChunkColumn`'s counter maintenance"
            );
            if section_ticks {
                for _ in 0..tick_speed {
                    let (x, y, z) =
                        next_random_tick_pos(&mut self.position_state, min_x, section_min_y, min_z, 15);
                    let lx = x - min_x;
                    let lz = z - min_z;
                    let state = column.block_state(lx, y, lz).to_string();
                    if !is_randomly_ticking(&state) {
                        continue;
                    }
                    events.extend(self.tick_randomly_ticking_block(
                        column,
                        min_x,
                        min_z,
                        x,
                        y,
                        z,
                        &state,
                        block_ticks,
                        current_tick,
                    ));
                }
            }
            section_min_y += 16;
        }
        events
    }

    /// Dispatches a position already confirmed eligible by
    /// [`is_randomly_ticking`] to the right per-block-family handler — grass
    /// (this module) or crop/sapling/leaves (`crate::growth_tick`, issue
    /// #310). One dispatch point keeps `tick_chunk`'s own selection loop
    /// ignorant of which families exist, exactly like vanilla's single
    /// `blockState.randomTick(...)` virtual call fanning out to whichever
    /// `Block` subclass is actually at that position.
    #[allow(clippy::too_many_arguments)]
    fn tick_randomly_ticking_block(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
        block_ticks: &mut ScheduledTickQueue<String>,
        current_tick: u64,
    ) -> Vec<RandomTickEvent> {
        let base = base_name(state);
        let mut events = if base == GRASS_BLOCK {
            self.tick_grass_block(column, min_x, min_z, x, y, z, state)
        } else if base == LAVA_BLOCK {
            self.tick_lava(column, min_x, min_z, x, y, z, block_ticks, current_tick)
        } else if growth_tick::crop_max_age(base).is_some() {
            self.tick_crop_block(column, min_x, min_z, x, y, z, state, base)
        } else if growth_tick::is_sapling(state) {
            self.tick_sapling_block(column, min_x, min_z, x, y, z, state, base)
        } else if growth_tick::is_leaves(state) {
            self.tick_leaves_block(column, min_x, min_z, x, y, z, state)
        } else {
            Vec::new()
        };

        // Issue #311/#314: every mutation above notifies its six neighbours,
        // mirroring vanilla's own `setBlockAndUpdate` (this is
        // `NeighborPropagator`'s first real production call — see
        // `crate::gravity_tick`'s module doc). Two reactions are modeled
        // today: a gravity block settling once its support disappears, and
        // the redstone family (#314/#315/#317) recomputing dust power or
        // scheduling a torch/diode/observer recheck.
        let mutated: Vec<(i32, i32, i32)> = events.iter().map(|e| e.pos).collect();
        for (ex, ey, ez) in mutated {
            events.extend(propagate_and_react(column, min_x, min_z, ex, ey, ez, block_ticks, current_tick));
        }
        events
    }

    /// `LavaFluid::randomTick` — lava setting fire to what is near it, and the
    /// only thing in a generated world that starts a fire at all.
    ///
    /// Every fire in this crate traces back to here: `crate::fire` owns the
    /// spread and the burnout, but a fire block has to exist first, and nothing
    /// else creates one (fire has no item, so a player cannot place it, and a
    /// creeper's blast carries no fire flag). So this arm is what makes the whole
    /// fire family reachable rather than an island.
    ///
    /// # The two branches and their draws
    ///
    /// One `nextInt(3)` decides which branch runs, and the draw counts differ:
    ///
    /// * `passes > 0` (2 of 3 outcomes): walk up to `passes` cells, each step
    ///   `(nextInt(3) - 1, +1, nextInt(3) - 1)` — **two draws per step** — and stop
    ///   at the first air cell with a flammable neighbour, lighting it. A cell that
    ///   blocks motion ends the walk.
    /// * `passes == 0`: three independent probes at the lava's own level,
    ///   `(nextInt(3) - 1, 0, nextInt(3) - 1)` — again two draws each, **six
    ///   total** — and each probe with air above it and a lava-ignitable block at it
    ///   lights the cell above. This branch does **not** stop after a success.
    ///
    /// `ignitedByLava` is the flammability test here, and it is a *different set*
    /// from fire's own ignite odds — every bed is lava-ignitable with no ignite
    /// odds, every small flower the reverse. See `lodestone_data::block_blast`.
    ///
    /// # The one reduction
    ///
    /// Vanilla's `!level.isLoaded(testPos)` returns from the whole method. This
    /// runs inside `tick_chunk`, which holds exactly one column, so a probe that
    /// lands outside it is treated as unloaded and returns — which is the same
    /// branch vanilla takes at a real chunk border, just reached more often. The
    /// RNG draws for that probe have already happened either way, so the stream
    /// stays aligned.
    #[allow(clippy::too_many_arguments)]
    fn tick_lava(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        block_ticks: &mut ScheduledTickQueue<String>,
        current_tick: u64,
    ) -> Vec<RandomTickEvent> {
        let mut events = Vec::new();
        // `ServerLevel::canSpreadFireAround` is a player-proximity test this
        // function has no players for; the tick loop gates the whole random-tick
        // pass on the tick area already being player-centred, so treating it as
        // true here matches the default 128-block radius.
        let min_y = column.min_y;
        let max_y = column.min_y + column.height;
        // Reads a cell through local coordinates, answering air for anything
        // outside this column or outside build height — the same single-accessor
        // invariant `crate::fire` and `crate::fluid` document.
        let read = |column: &crate::chunk::ChunkColumn, at: BlockPos| -> Option<String> {
            let lx = at.x - min_x;
            let lz = at.z - min_z;
            if !(0..16).contains(&lx) || !(0..16).contains(&lz) || at.y < min_y || at.y >= max_y {
                return None;
            }
            Some(column.block_state(lx, at.y, lz).to_string())
        };
        let flammable = |column: &crate::chunk::ChunkColumn, at: BlockPos| -> bool {
            read(column, at).is_some_and(|state| {
                lodestone_data::block_blast::blast_or_inert(&state).ignited_by_lava
            })
        };
        let mut light = |column: &mut crate::chunk::ChunkColumn,
                         at: BlockPos,
                         support: BlockPos,
                         events: &mut Vec<RandomTickEvent>| {
            let Some(from) = read(column, at) else { return };
            // `BaseFireBlock::getState(level, support)` — soul fire over a soul
            // base, otherwise ordinary fire with its connected faces derived from
            // `support`'s neighbourhood.
            let new_state = fire_state_in_column(column, min_x, min_z, min_y, max_y, support);
            column.set_block(at.x - min_x, at.y, at.z - min_z, &new_state);
            events.push(RandomTickEvent {
                pos: (at.x, at.y, at.z),
                from,
                to: new_state,
            });
            // Without this the fire is inert forever — see `crate::fire`.
            if !block_ticks.has_scheduled((at.x, at.y, at.z), &crate::fire::TICK_FIRE.to_string()) {
                block_ticks.schedule(
                    (at.x, at.y, at.z),
                    crate::fire::TICK_FIRE.to_string(),
                    current_tick + crate::fire::TICK_DELAY_BASE,
                    TickPriority::Normal,
                );
            }
        };

        let passes = self.behavior_rng.next_int(3);
        if passes > 0 {
            let mut test = BlockPos::new(x, y, z);
            for _ in 0..passes {
                let dx = self.behavior_rng.next_int(3) - 1;
                let dz = self.behavior_rng.next_int(3) - 1;
                test = BlockPos::new(test.x + dx, test.y + 1, test.z + dz);
                let Some(state) = read(column, test) else {
                    return events;
                };
                if is_air_variant(&state) {
                    let has_flammable_neighbour = [
                        (0, -1, 0),
                        (0, 1, 0),
                        (0, 0, -1),
                        (0, 0, 1),
                        (-1, 0, 0),
                        (1, 0, 0),
                    ]
                    .iter()
                    .any(|&(nx, ny, nz)| {
                        flammable(column, BlockPos::new(test.x + nx, test.y + ny, test.z + nz))
                    });
                    if has_flammable_neighbour {
                        light(column, test, test, &mut events);
                        return events;
                    }
                } else if lodestone_data::block_states::state_id(&state)
                    .and_then(lodestone_data::block_solidity::blocks_motion)
                    .unwrap_or(false)
                {
                    return events;
                }
            }
        } else {
            for _ in 0..3 {
                let dx = self.behavior_rng.next_int(3) - 1;
                let dz = self.behavior_rng.next_int(3) - 1;
                let test = BlockPos::new(x + dx, y, z + dz);
                let above = BlockPos::new(test.x, test.y + 1, test.z);
                if read(column, test).is_none() {
                    return events;
                }
                let above_is_air = read(column, above).is_some_and(|s| is_air_variant(&s));
                if above_is_air && flammable(column, test) {
                    // Vanilla passes `testPos`, not `testPos.above()`, to
                    // `BaseFireBlock::getState` here while writing to
                    // `testPos.above()`. Transcribed as written: the support cell
                    // the state is derived from really is one lower than the cell
                    // being lit.
                    light(column, above, test, &mut events);
                }
            }
        }
        events
    }

    /// Crop growth (issue #310) — see `crate::growth_tick`'s module doc for
    /// the jar citation. Reads the block directly above as the light-check
    /// proxy (same convention grass uses), draws through the shared
    /// `behavior_rng`, and on a hit persists the new age into `column`.
    fn tick_crop_block(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
        base: &str,
    ) -> Vec<RandomTickEvent> {
        let lx = x - min_x;
        let lz = z - min_z;
        let above = column.block_state(lx, y + 1, lz).to_string();
        let above_is_air = is_air_variant(&above);
        let age = growth_tick::get_age(state);
        match growth_tick::crop_random_tick(base, age, above_is_air, &mut self.behavior_rng) {
            growth_tick::CropOutcome::Grew(new_age) => {
                let new_state = growth_tick::set_age(base, new_age);
                column.set_block(lx, y, lz, &new_state);
                vec![RandomTickEvent {
                    pos: (x, y, z),
                    from: state.to_string(),
                    to: new_state,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Sapling growth (issue #310) — see `crate::growth_tick`'s module doc
    /// for the jar citation, including why an already-stage-1 hit is a
    /// named no-op (no tree feature exists in this crate to grow into).
    fn tick_sapling_block(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
        base: &str,
    ) -> Vec<RandomTickEvent> {
        let lx = x - min_x;
        let lz = z - min_z;
        let above = column.block_state(lx, y + 1, lz).to_string();
        let above_is_air = is_air_variant(&above);
        let stage = growth_tick::get_stage(state);
        match growth_tick::sapling_random_tick(above_is_air, stage, &mut self.behavior_rng) {
            growth_tick::SaplingOutcome::AdvancedToStage1 => {
                let new_state = growth_tick::set_stage(base, 1);
                column.set_block(lx, y, lz, &new_state);
                vec![RandomTickEvent {
                    pos: (x, y, z),
                    from: state.to_string(),
                    to: new_state,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Leaf decay (issue #310) — see `crate::growth_tick`'s module doc for
    /// why this draws **zero** RNG values: `is_randomly_ticking` already
    /// proved `leaves_should_decay`, and vanilla's own `randomTick` for
    /// `LeavesBlock` has no `random.nextInt` call at all, only the
    /// deterministic `decaying(state)` check. Removes the block (sets it to
    /// air); item-drop spawning (`dropResources`) is out of scope — see the
    /// module doc's own note.
    fn tick_leaves_block(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
    ) -> Vec<RandomTickEvent> {
        let lx = x - min_x;
        let lz = z - min_z;
        column.set_block(lx, y, lz, crate::chunk::AIR);
        vec![RandomTickEvent {
            pos: (x, y, z),
            from: state.to_string(),
            to: crate::chunk::AIR.to_string(),
        }]
    }

    /// Applies [`grass_random_tick`] at world position `(x, y, z)` against
    /// `column`, mutating it and returning every resulting event: at most
    /// one self-conversion (dies-to-dirt) OR up to four spread events (one
    /// per accepted propagation target).
    fn tick_grass_block(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        x: i32,
        y: i32,
        z: i32,
        current_state: &str,
    ) -> Vec<RandomTickEvent> {
        let lx = x - min_x;
        let lz = z - min_z;
        let above = column.block_state(lx, y + 1, lz).to_string();
        // Issue #544: vanilla's real `canStayAlive`, not the old
        // `is_air_variant` proxy. The proxy killed grass under *any* non-air
        // block, and vanilla's own vegetation step puts `minecraft:short_grass`
        // on top of grass blocks — so every decorated patch turned to dirt on
        // its first random tick.
        let can_stay_alive = grass_can_stay_alive(&above);

        // `try_propagate` only reads `column` (via the immutable reborrow
        // below) — no mutation happens until after `grass_random_tick`
        // returns and this borrow ends, so the two phases (decide, then
        // apply) never overlap.
        let outcome = {
            let column_ref: &crate::chunk::ChunkColumn = column;
            grass_random_tick(can_stay_alive, &mut self.behavior_rng, |dx, dy, dz| {
                let tx = x + dx;
                let tz = z + dz;
                let tlx = tx - min_x;
                let tlz = tz - min_z;
                if !(0..16).contains(&tlx) || !(0..16).contains(&tlz) {
                    // Cross-chunk propagation target: this crate has no
                    // neighbour-column access from inside `tick_chunk` (only
                    // the one column being ticked is in scope) — treated as
                    // "not a valid target," matching vanilla's own
                    // `canPropagate` returning false for anything that fails
                    // its checks. The RNG draw for this attempt still
                    // happened (see `grass_random_tick`), only the mutation
                    // is skipped.
                    return false;
                }
                let ty = y + dy;
                // `block_state` takes LOCAL x/z (issue #472): passing the
                // absolute `tz` here tripped `ChunkColumn::index`'s
                // `debug_assert` on every singleplayer session, and in
                // release silently aliased onto a different cell — the
                // index is `((y_local * 16 + z) * 16 + x)`, so an absolute
                // `z` of `min_z + tlz` reads local z `tlz` at a y-level
                // `min_z / 16` sections higher. Invisible at chunk (0, 0),
                // where the two coordinates coincide.
                let target_state = column_ref.block_state(tlx, ty, tlz);
                let above_target = column_ref.block_state(tlx, ty + 1, tlz);
                can_propagate_onto(target_state, above_target)
            })
        };

        let mut events = Vec::new();
        match outcome {
            GrassOutcome::DiesToDirt => {
                column.set_block(lx, y, lz, DIRT_BLOCK);
                events.push(RandomTickEvent {
                    pos: (x, y, z),
                    from: current_state.to_string(),
                    to: DIRT_BLOCK.to_string(),
                });
            }
            GrassOutcome::NoPropagationTargetAccepted => {}
            GrassOutcome::Spreads(offsets) => {
                for (dx, dy, dz) in offsets {
                    let tx = x + dx;
                    let ty = y + dy;
                    let tz = z + dz;
                    let tlx = tx - min_x;
                    let tlz = tz - min_z;
                    let above_target = column.block_state(tlx, ty + 1, tlz).to_string();
                    let spread_state = spreading_snowy_state(GRASS_BLOCK, &above_target);
                    column.set_block(tlx, ty, tlz, spread_state);
                    events.push(RandomTickEvent {
                        pos: (tx, ty, tz),
                        from: DIRT_BLOCK.to_string(),
                        to: spread_state.to_string(),
                    });
                }
            }
        }
        events
    }
}

/// `BaseFireBlock::getState`, evaluated against a single [`crate::chunk::ChunkColumn`].
///
/// `crate::fire::state_at` is the [`crate::chunk::ChunkSource`] form and is what
/// the tick loop's fire drain uses; this is the same rule for the one caller that
/// holds a column instead — [`RandomTickScheduler::tick_lava`]. A cell outside the
/// column reads as air, so a fire lit at a chunk border simply has fewer connected
/// faces than vanilla would give it, which is cosmetic.
fn fire_state_in_column(
    column: &crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    min_y: i32,
    max_y: i32,
    pos: BlockPos,
) -> String {
    let read = |at: BlockPos| -> String {
        let lx = at.x - min_x;
        let lz = at.z - min_z;
        if !(0..16).contains(&lx) || !(0..16).contains(&lz) || at.y < min_y || at.y >= max_y {
            return crate::chunk::AIR.to_owned();
        }
        column.block_state(lx, at.y, lz).to_string()
    };
    let below = read(BlockPos::new(pos.x, pos.y - 1, pos.z));
    if crate::fire::SOUL_FIRE_BASE.contains(&base_name(&below)) {
        return crate::fire::SOUL_FIRE.to_owned();
    }
    if !crate::fire::can_burn(&below) && !crate::fire::face_sturdy_up(&below) {
        let up = crate::fire::can_burn(&read(BlockPos::new(pos.x, pos.y + 1, pos.z)));
        let north = crate::fire::can_burn(&read(BlockPos::new(pos.x, pos.y, pos.z - 1)));
        let south = crate::fire::can_burn(&read(BlockPos::new(pos.x, pos.y, pos.z + 1)));
        let west = crate::fire::can_burn(&read(BlockPos::new(pos.x - 1, pos.y, pos.z)));
        let east = crate::fire::can_burn(&read(BlockPos::new(pos.x + 1, pos.y, pos.z)));
        return format!(
            "{}[age=0,east={east},north={north},south={south},up={up},west={west}]",
            crate::fire::FIRE
        );
    }
    format!(
        "{}[age=0,east=false,north=false,south=false,up=false,west=false]",
        crate::fire::FIRE
    )
}

/// What `FallingBlock.tick` decided at one position: the block is unsupported and
/// is about to become a `FallingBlockEntity`.
///
/// Returned rather than applied because the two halves live in different places —
/// the world mutation is the caller's (`crate::tick`'s drain owns the column and
/// the outbound feed) and the entity is `crate::mobs::MobSim`'s. Keeping the
/// decision pure is also what stops this function reintroducing the teleport it
/// used to be: it can no longer write a block anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GravitySettle {
    /// The state that leaves the world and rides the entity.
    pub state: String,
    /// Where the entity will come to rest —
    /// [`gravity_tick::find_landing_y`]'s answer against the world as it is now.
    pub landing_y: i32,
}

/// `FallingBlock.tick`: whether the gravity block at world `(x, y, z)` is
/// unsupported and should become a falling entity, and where it will land.
///
/// `None` for anything that is not a gravity block, for one that is still
/// supported, and for the (unreachable) case where the landing scan does not
/// move. Draws no RNG — `FallingBlock.tick` itself draws none — so this needs no
/// `&mut self`, and it no longer needs `&mut column` either: **it mutates
/// nothing.**
///
/// # What changed, and why it is not a regression
///
/// This used to move the block: `set_block(y, AIR)` plus
/// `set_block(landing_y, state)` in one step, returning both as
/// [`RandomTickEvent`]s. That was the whole of the reported *"it just teleports to
/// its final place at the bottom instead of falling down and landing"* — a real,
/// documented simplification standing in for the `FallingBlockEntity` that did not
/// exist. It does now, so the teleport is gone and this function answers the
/// question rather than acting on it.
///
/// `pub(crate)` for `crate::tick`'s scheduled-tick drain, which dispatches
/// `gravity_tick::TICK_GRAVITY` **straight here** rather than through
/// [`propagate_and_react`]. That is not a shortcut: `propagate` notifies an
/// origin's six neighbours and not the origin, while vanilla's `onPlace` tick
/// fires on the placed block itself, so the propagate route would settle the
/// wrong cells. See `crate::gravity_tick`'s module doc.
pub(crate) fn settle_gravity_at(
    column: &crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
) -> Option<GravitySettle> {
    let lx = x - min_x;
    let lz = z - min_z;
    let state = column.block_state(lx, y, lz).to_string();
    if !gravity_tick::is_gravity_block(base_name(&state)) {
        return None;
    }
    let below = column.block_state(lx, y - 1, lz).to_string();
    if !gravity_tick::is_free(&below) {
        return None;
    }
    let landing_y = gravity_tick::find_landing_y(
        |probe_y| gravity_tick::is_free(column.block_state(lx, probe_y, lz)),
        y,
        column.min_y,
    );
    if landing_y == y {
        // Shouldn't happen (we already confirmed `below` is free, so the
        // scan must move at least one step) — defensive, not reachable.
        return None;
    }
    Some(GravitySettle { state, landing_y })
}

/// `DefaultRedstoneWireEvaluator.updatePowerStrength`'s update set, in full
/// (`DefaultRedstoneWireEvaluator.java:27-37`):
///
/// ```text
/// Set<BlockPos> toUpdate = Sets.newHashSet();
/// toUpdate.add(pos);
/// for (Direction direction : Direction.values()) { toUpdate.add(pos.relative(direction)); }
/// for (BlockPos blockPos : toUpdate) { level.updateNeighborsAt(blockPos, this.wireBlock); }
/// ```
///
/// Seven *centres* — the wire's own position and each of its six neighbours —
/// each of which gets a full six-direction `updateNeighborsAt` fan-out, so 42
/// notifications with duplicates among them. Vanilla really does issue the
/// duplicates; the `HashSet` dedupes the centres, not the notifications.
///
/// # Why the second layer is not a corner case
///
/// An earlier landing implemented centre 0 only and described the omission as
/// "a diagonal-over-conductor corner update". It is not: the geometry the
/// first layer alone cannot reach is the **standard torch-inverter** — dust
/// sitting on top of a block with a torch on that block's side. The torch is
/// diagonal to the dust, so it is a neighbour of a *neighbour* and only ever
/// appears in the second layer. Measured on live vanilla 26.2, that torch
/// inverts reliably; with the first layer alone we never notified it and it
/// stayed lit forever.
///
/// # Ordering
///
/// Vanilla iterates a `HashSet`, so its order is unspecified and cannot be
/// copied. This picks the one deterministic order available: centres in
/// `[pos] ++ UPDATE_ORDER`, and within each centre the six directions in
/// [`UPDATE_ORDER`]. Determinism is what this crate needs from it; no vanilla
/// behaviour can depend on an order vanilla itself does not guarantee.
fn wire_update_centres(pos: BlockPos) -> Vec<BlockPos> {
    std::iter::once(pos).chain(UPDATE_ORDER.iter().map(|d| d.relative(pos))).collect()
}

/// [`wire_update_centres`] flattened into the notifications those seven
/// `updateNeighborsAt` calls issue, for use as a cascade return value. The two
/// are the same thing: the propagator resolves a returned notification and its
/// own cascade fully before moving to the next, which is exactly what
/// `updateNeighborsAt` does per centre.
fn wire_update_fan_out(pos: BlockPos) -> Vec<Notification> {
    let mut out = Vec::with_capacity(UPDATE_ORDER.len() * (UPDATE_ORDER.len() + 1));
    for centre in wire_update_centres(pos) {
        for d in UPDATE_ORDER {
            out.push(Notification { pos: d.relative(centre), from: d });
        }
    }
    out
}

/// Notifies the six neighbours of a just-mutated position `(x, y, z)` via
/// `NeighborPropagator` (issue #308's own primitive) and dispatches every
/// reaction this crate models to a neighbour notification:
///
/// 1. **Gravity (#311)** — settles any neighbour that is an unsupported
///    gravity block; a settled block's *old* position is re-notified from
///    directly above so a stacked column collapses one at a time,
///    depth-first. Unchanged from the #311 landing.
/// 2. **Redstone dust (#314)** — recomputes the neighbour's target power
///    strength (`crate::redstone_wire::calculate_target_strength`); if it
///    changed, writes the new power and re-fans-out through
///    [`wire_update_fan_out`], which is
///    `DefaultRedstoneWireEvaluator.updatePowerStrength`'s **complete**
///    update set (`DefaultRedstoneWireEvaluator.java:27-37`), both layers.
/// 3. **Redstone torches/repeaters/comparators/observers (#314/#315/#317)**
///    — schedule a delayed recheck into `block_ticks` when the neighbour's
///    steady-state condition disagrees with its current state (torch:
///    `LIT == hasSignal`; diode: `POWERED != shouldTurnOn`; observer: the
///    notification travelled from its watched face and it isn't already
///    outputting) — see each family's own module for the exact per-block
///    citation. No immediate mutation happens here: the flip itself runs
///    when `block_ticks` drains, in `tick::run_tick_loop`.
///
/// Neighbours outside this column's 16×16 footprint are skipped — the same
/// cross-chunk limitation `tick_grass_block`'s own spread already accepts.
/// [`propagate_and_react`], preceded by the reaction the **placed block itself**
/// owes (issue #465).
///
/// # Why this is a separate entry point and not a flag on the one above
///
/// `NeighborPropagator::propagate` issues notifications to the origin's six
/// *neighbours* and never to the origin — faithfully, because it models
/// `Level.updateNeighborsAt`, which does exactly that. Every existing caller of
/// [`propagate_and_react`] is a *change* whose origin has already had its say
/// (a drained scheduled tick has just run the block's own callback; a random
/// tick has just mutated it), so the omission is correct there.
///
/// A **placement** is the one case where it is not. Vanilla splits the two
/// halves across different callbacks, and the placed block's own half lives in
/// `BlockBehaviour.setPlacedBy`, called from `BlockItem.place` — nowhere near
/// the neighbour pass. Without it, placing a repeater into an already-powered
/// line does nothing at all: the fan-out notifies the dust either side, neither
/// dust changes power, no cascade reaches the repeater, and the repeater is
/// never asked whether it should turn on.
///
/// # What the jar says, per family
///
/// | family | `setPlacedBy` | modelled here |
/// |---|---|---|
/// | repeater, comparator (`DiodeBlock:160-165`) | `if (shouldTurnOn) scheduleTick(pos, this, 1)` | yes |
/// | redstone torch (`RedstoneTorchBlock`) | none — only `onPlace`'s neighbour notify | nothing to do |
/// | observer (`ObserverBlock`) | none; its `onPlace:115-123` only *cancels* a stale pulse on a block it replaced, which cannot apply to a placement into air | nothing to do |
///
/// **The delay is 1, not `getDelay(state)`, and that is not a slip.** A
/// repeater dropped into a live line lights one game tick later at *every* one
/// of its four delay settings; the `2d` delay governs signal *changes* reaching
/// an already-placed repeater, through `checkTickOnNeighbor`
/// (`DiodeBlock:88-104`), which is a different callback with a different delay.
/// `redstone_placement_gate` measures both and separates them, because reading
/// `2d` here is the single most plausible wrong model of this function.
pub(crate) fn react_at_placement(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
) -> Vec<RandomTickEvent> {
    let tlx = x - min_x;
    let tlz = z - min_z;
    let in_column = (0..16).contains(&tlx)
        && (0..16).contains(&tlz)
        && y >= column.min_y
        && y < column.min_y + column.height;
    let mut own = Vec::new();
    if in_column {
        let state = column.block_state(tlx, y, tlz).to_string();
        let pos = BlockPos::new(x, y, z);
        // `HopperBlock.onPlace` (`HopperBlock.java:100-104`) calls the same
        // `checkPoweredState` its `neighborChanged` does, so a hopper placed
        // into an already-powered cell must come up locked (issue #321). The
        // neighbour pass cannot do this: it never notifies the origin.
        if redstone::is_hopper(&state) {
            let should_be_on =
                redstone::best_neighbor_signal(&redstone::make_lookup(column, min_x, min_z), pos, false) == 0;
            if should_be_on != redstone::hopper_enabled(&state) {
                let new_state = redstone::with_property(&state, "enabled", if should_be_on { "true" } else { "false" });
                column.set_block(tlx, y, tlz, &new_state);
                own.push(RandomTickEvent { pos: (x, y, z), from: state.clone(), to: new_state });
            }
        }
        let placed_kind = if redstone::is_repeater(&state) {
            let facing = redstone::diode_facing(&state);
            redstone_diode::repeater_should_turn_on(&redstone::make_lookup(column, min_x, min_z), pos, facing)
                .then_some(redstone::TICK_REPEATER)
        } else if redstone::is_comparator(&state) {
            let facing = redstone::diode_facing(&state);
            let input = redstone::input_signal(&redstone::make_lookup(column, min_x, min_z), pos, facing);
            let side = redstone::alternate_signal(&redstone::make_lookup(column, min_x, min_z), pos, facing, false);
            let subtract = redstone::comparator_mode_subtract(&state);
            redstone_diode::comparator_should_turn_on(input, side, subtract).then_some(redstone::TICK_COMPARATOR)
        } else {
            None
        };
        if let Some(kind) = placed_kind {
            if !block_ticks.has_scheduled((x, y, z), &kind.to_string()) {
                // `level.scheduleTick(pos, this, 1)` — the three-argument
                // overload, so `TickPriority.NORMAL`.
                block_ticks.schedule((x, y, z), kind.to_string(), current_tick + 1, TickPriority::Normal);
            }
        }
        // `FireBlock::onPlace` schedules the fire's own first tick, and without
        // it a fire block is inert forever — it neither spreads nor goes out,
        // because every later tick comes from the previous one's reschedule.
        // This is the same "the placed block owes itself a reaction the
        // neighbour pass cannot deliver" case as the hopper above.
        if crate::fire::is_ordinary_fire(&state) {
            for pending in crate::fire::ticks_after_edit(pos) {
                if !block_ticks.has_scheduled(pending.pos, &pending.kind) {
                    block_ticks.schedule(
                        pending.pos,
                        pending.kind,
                        current_tick + pending.trigger_tick,
                        pending.priority,
                    );
                }
            }
        }
        // `BaseRailBlock.onPlace` -> `updateState` -> `level.neighborChanged(state,
        // pos, this, ...)` (`BaseRailBlock.java:64-77`): a freshly placed
        // powered/activator rail notifies **itself**, the same "placed block
        // owes itself a reaction the neighbour pass cannot deliver" shape as the
        // hopper arm above. `crate::redstone_rail`'s own module doc names why
        // only `POWERED` (not `SHAPE`/connectivity) is modelled.
        if redstone_rail::is_powered_rail_family(&state) {
            let new_state = {
                let lookup = redstone::make_lookup(column, min_x, min_z);
                let has_signal = |p: BlockPos| redstone::best_neighbor_signal(&lookup, p, false) > 0;
                redstone_rail::update_state(&lookup, &has_signal, pos, &state)
            };
            if let Some(new_state) = new_state {
                column.set_block(tlx, y, tlz, &new_state);
                own.push(RandomTickEvent { pos: (x, y, z), from: state.clone(), to: new_state });
            }
        }
        // `TripWireHookBlock.setPlacedBy` (`:104-106`) calls `calculateState`
        // directly on the just-placed hook, with no neighbour notification at
        // all — see `crate::redstone_tripwire`'s own module doc for why this
        // family lives in `react_at_placement` rather than
        // `react_to_notification`.
        if redstone::is_tripwire_hook(&state) {
            let result = {
                let lookup = redstone::make_lookup(column, min_x, min_z);
                redstone_tripwire::calculate_state(&lookup, pos, &state, false, None)
            };
            apply_tripwire_result(column, min_x, min_z, &result, &mut own);
        }
        // `TripWireBlock.onPlace` (`:101-105`) calls `updateSource`, which
        // scans south/west for a controlling hook and recalculates *that*
        // hook's state with this wire cell as its `wireSource`.
        if base_name(&state) == redstone_tripwire::TRIPWIRE {
            let found = {
                let lookup = redstone::make_lookup(column, min_x, min_z);
                redstone_tripwire::find_controlling_hooks(&lookup, pos, &state)
            };
            for (hook_pos, source) in found {
                let hook_state = redstone::make_lookup(column, min_x, min_z)(hook_pos);
                if base_name(&hook_state) != redstone_tripwire::TRIPWIRE_HOOK {
                    continue;
                }
                let result = {
                    let lookup = redstone::make_lookup(column, min_x, min_z);
                    redstone_tripwire::calculate_state(&lookup, hook_pos, &hook_state, false, Some(&source))
                };
                apply_tripwire_result(column, min_x, min_z, &result, &mut own);
                if result.reschedule_recheck
                    && !block_ticks.has_scheduled((hook_pos.x, hook_pos.y, hook_pos.z), &redstone_tripwire::TICK_TRIPWIRE_RECHECK.to_string())
                {
                    block_ticks.schedule(
                        (hook_pos.x, hook_pos.y, hook_pos.z),
                        redstone_tripwire::TICK_TRIPWIRE_RECHECK.to_string(),
                        current_tick + u64::from(redstone_tripwire::RECHECK_DELAY),
                        TickPriority::Normal,
                    );
                }
            }
        }
    }
    own.extend(propagate_and_react(column, min_x, min_z, x, y, z, block_ticks, current_tick));
    own
}

/// Applies a [`redstone_tripwire::CalculatedState`]'s write plan to `column`,
/// skipping any position outside it — the same "out of this column, so the
/// write cannot happen" limit `react_to_notification`'s piston arm already
/// accepts, since a tripwire run can span far more than one 16×16 column.
fn apply_tripwire_result(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    result: &redstone_tripwire::CalculatedState,
    own: &mut Vec<RandomTickEvent>,
) {
    let mut writes: Vec<(BlockPos, String)> = Vec::new();
    if let Some(w) = &result.hook_write {
        writes.push(w.clone());
    }
    if let Some(w) = &result.receiver_write {
        writes.push(w.clone());
    }
    writes.extend(result.wire_writes.iter().cloned());

    for (pos, new_state) in writes {
        let lx = pos.x - min_x;
        let lz = pos.z - min_z;
        if !(0..16).contains(&lx) || !(0..16).contains(&lz) || pos.y < column.min_y || pos.y >= column.min_y + column.height {
            continue;
        }
        let from = column.block_state(lx, pos.y, lz).to_string();
        if from == new_state {
            continue;
        }
        column.set_block(lx, pos.y, lz, &new_state);
        own.push(RandomTickEvent {
            pos: (pos.x, pos.y, pos.z),
            from,
            to: new_state,
        });
    }
}

/// The `redstone_tripwire::TICK_TRIPWIRE_RECHECK` scheduled-tick body —
/// `TripWireHookBlock.tick` (`:196-199`), which re-runs `calculate_state` with
/// no `wire_source`. `pub(crate)` for `crate::tick`'s scheduled-tick drain,
/// the same shape [`settle_gravity_at`] already has for gravity's own
/// specially-handled arm (a multi-position write plan, not a single
/// replacement state, so it cannot go through the ordinary `Option<String>`
/// dispatch chain every diode/torch/observer arm uses).
pub(crate) fn run_tripwire_recheck(column: &mut crate::chunk::ChunkColumn, min_x: i32, min_z: i32, pos: BlockPos) -> Vec<RandomTickEvent> {
    let tlx = pos.x - min_x;
    let tlz = pos.z - min_z;
    if !(0..16).contains(&tlx) || !(0..16).contains(&tlz) || pos.y < column.min_y || pos.y >= column.min_y + column.height {
        return Vec::new();
    }
    let state = column.block_state(tlx, pos.y, tlz).to_string();
    if base_name(&state) != redstone_tripwire::TRIPWIRE_HOOK {
        return Vec::new();
    }
    let result = {
        let lookup = redstone::make_lookup(column, min_x, min_z);
        redstone_tripwire::calculate_state(&lookup, pos, &state, false, None)
    };
    let mut own = Vec::new();
    apply_tripwire_result(column, min_x, min_z, &result, &mut own);
    own
}

pub(crate) fn propagate_and_react(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
) -> Vec<RandomTickEvent> {
    crate::redstone_counters::begin_drain();
    let mut events = Vec::new();
    let propagator = NeighborPropagator::default();
    let origin = BlockPos::new(x, y, z);

    // The mutated block itself decides how wide the *outermost* fan-out is.
    // Every mutation family except dust mirrors `setBlockAndUpdate`, which is
    // a single `updateNeighborsAt(pos)`; a dust power change instead runs
    // `DefaultRedstoneWireEvaluator.updatePowerStrength`'s seven-centre set —
    // and that applies to the origin exactly as it applies to a wire reached
    // mid-cascade, which is the half an earlier landing missed.
    let origin_is_wire = {
        let tlx = x - min_x;
        let tlz = z - min_z;
        (0..16).contains(&tlx)
            && (0..16).contains(&tlz)
            && y >= column.min_y
            && y < column.min_y + column.height
            && redstone::is_wire(column.block_state(tlx, y, tlz))
    };
    let centres = if origin_is_wire { wire_update_centres(origin) } else { vec![origin] };

    for centre in centres {
        propagator.propagate(centre, None, |n: Notification| -> Vec<Notification> {
            react_to_notification(column, min_x, min_z, n, block_ticks, current_tick, &mut events)
        });
    }
    crate::redstone_counters::end_drain();
    events
}

/// One neighbour notification's worth of reaction dispatch — the body of
/// [`propagate_and_react`]'s `notify` closure, named so the seven centres a
/// dust change fans out from can share it.
fn react_to_notification(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    n: Notification,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
    events: &mut Vec<RandomTickEvent>,
) -> Vec<Notification> {
    {
        let tlx = n.pos.x - min_x;
        let tlz = n.pos.z - min_z;
        if !(0..16).contains(&tlx) || !(0..16).contains(&tlz) {
            return Vec::new();
        }
        if n.pos.y < column.min_y || n.pos.y >= column.min_y + column.height {
            return Vec::new();
        }

        crate::redstone_counters::bump_notification();
        let state = column.block_state(tlx, n.pos.y, tlz).to_string();

        // 1. Gravity — first, matching the existing precedent.
        //
        // **`FallingBlock.updateShape`, which is `scheduleTick(pos, this,
        // getDelayAfterPlace())` and nothing else.** No `isFree(below)` test here
        // and no fall: the eligibility check belongs to `FallingBlock.tick`, which
        // the scheduled tick dispatches to (`crate::tick`'s drain →
        // `settle_gravity_at`).
        //
        // This arm used to *settle inline*, which was a second fall path that
        // skipped both the 2-tick delay and — once the entity existed — the entity
        // itself. Sand whose support was removed by a neighbour mutation
        // teleported while sand placed in mid-air fell properly, from the same
        // module, for no reason a reader could see. There is now exactly one place
        // a block ever leaves the world for a fall.
        //
        // No further fan-out (empty return): `updateShape` returns
        // `super.updateShape(...)` unchanged, so nothing about the world moved and
        // there is nothing to notify.
        if gravity_tick::is_gravity_block(base_name(&state)) {
            block_ticks.schedule(
                (n.pos.x, n.pos.y, n.pos.z),
                gravity_tick::TICK_GRAVITY.to_string(),
                current_tick + gravity_tick::DELAY_AFTER_PLACE,
                TickPriority::Normal,
            );
            return Vec::new();
        }

        // 1b. `snowy` upkeep (#546). `SnowyBlock.updateShape`
        // (`SnowyBlock.java:41-45`) recomputes `snowy` from the block above
        // whenever that neighbour changes, so placing or breaking snow on grass
        // flips it in both directions. Nothing did this before, so `snowy` was
        // whatever it was written as and never moved.
        //
        // Recomputed unconditionally rather than only for a from-above
        // notification: the value depends solely on the block above, so the two
        // agree, and this needs no assumption about `Notification::from`'s
        // orientation. Flag 2 in vanilla's own `setBlock` — clients told, no
        // further fan-out, hence the empty return.
        if SNOWY_FAMILY.contains(&base_name(&state)) {
            let above = column.block_state(tlx, n.pos.y + 1, tlz).to_string();
            let want_snowy = is_snowy_setting(&above);
            // Compared on the *value*, not on the whole string: a property-less
            // `minecraft:grass_block` already means the default, `snowy=false`,
            // so this rewrites only when the value really has to flip.
            if (property_of(&state, "snowy") == Some("true")) != want_snowy {
                let want = spreading_snowy_state(base_name(&state), &above);
                column.set_block(tlx, n.pos.y, tlz, want);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state,
                    to: want.to_string(),
                });
            }
            return Vec::new();
        }

        // 2. Redstone dust (#314).
        if redstone::is_wire(&state) {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Dust);
            crate::redstone_counters::bump_wire_recompute();
            let new_power = redstone_wire::calculate_target_strength(&redstone::make_lookup(column, min_x, min_z), n.pos);
            let old_power = redstone::wire_power(&state);
            if new_power != old_power {
                let new_state = redstone_wire::set_power(new_power);
                column.set_block(tlx, n.pos.y, tlz, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state, to: new_state });
                return wire_update_fan_out(n.pos);
            }
            return Vec::new();
        }

        // 3a. Redstone torches (#314).
        if redstone::is_torch(&state) {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Torch);
            let has_signal = redstone_torch::has_neighbor_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, &state);
            if redstone_torch::should_schedule_check(&state, has_signal) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_TORCH.to_string()) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        redstone::TICK_TORCH.to_string(),
                        current_tick + 2,
                        TickPriority::Normal,
                    );
                }
            }
            return Vec::new();
        }

        // 3b. Repeaters (#315).
        if redstone::is_repeater(&state) {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Repeater);
            let facing = redstone::diode_facing(&state);
            let recomputed_lock = redstone_diode::recompute_locked(&redstone::make_lookup(column, min_x, min_z), n.pos, &state);
            if let Some(new_state) = recomputed_lock {
                column.set_block(tlx, n.pos.y, tlz, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state, to: new_state });
            }
            let state_now = column.block_state(tlx, n.pos.y, tlz).to_string();
            let should_on = redstone_diode::repeater_should_turn_on(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
            if redstone_diode::should_schedule_repeater_check(&state_now, should_on) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_REPEATER.to_string()) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    let priority = redstone_diode::repeater_schedule_priority(
                        &redstone::make_lookup(column, min_x, min_z),
                        n.pos,
                        facing,
                        redstone::diode_powered(&state_now),
                    );
                    let delay = redstone_diode::repeater_delay(&state_now);
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        redstone::TICK_REPEATER.to_string(),
                        current_tick + u64::from(delay),
                        priority,
                    );
                }
            }
            return Vec::new();
        }

        // 3b-bis. Pistons (#316). `PistonBaseBlock.neighborChanged` calls
        // `checkIfExtend`, which is **immediate** — it fires a block event rather
        // than scheduling a tick, so the move happens in the same neighbour pass
        // that noticed the signal. That is why this arm mutates here and returns a
        // fan-out rather than scheduling.
        //
        // The signal test is `piston::has_extend_signal`, which includes
        // **quasi-connectivity** — see its own doc comment for why that is not a
        // bug to be fixed.
        //
        // **The move is two-phase.** `crate::piston::begin_move` splits
        // `apply_move`'s one-step writes into the cells that empty now and the cells
        // that hold a `moving_piston` for `PISTON_MOVE_DELAY` ticks, and each of the
        // latter schedules its own commit carrying the state it will write
        // (`piston::finish_kind`). `crate::tick`'s scheduled-tick drain runs that
        // commit, so the world two ticks from now is what the one-step path used to
        // produce immediately — and in between, a client has a `moving_piston` cell
        // and a block entity to animate.
        //
        // What is still missing is interruption (a pending commit runs to
        // completion, so no 0-tick pulse) and entity shoving. Both are named in
        // `crate::piston`'s module doc; #316 stays open on them.
        if crate::piston::is_piston(&state) {
            let facing = crate::piston::piston_facing(&state);
            let extended = crate::piston::piston_extended(&state);
            let want_extended =
                crate::piston::has_extend_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
            if want_extended != extended {
                let sticky = crate::piston::is_sticky_piston(&state);
                let resolution = crate::piston::resolve(
                    &redstone::make_lookup(column, min_x, min_z),
                    n.pos,
                    facing,
                    want_extended,
                );
                // A retraction always happens (the head comes back even with
                // nothing to pull); an extension only happens if the run resolves.
                // That asymmetry is vanilla's: `checkIfExtend` gates the *extend*
                // event on `resolve()` and the contract event on nothing.
                let resolution = match (want_extended, resolution) {
                    (true, None) => return Vec::new(),
                    (_, Some(resolution)) => resolution,
                    (false, None) => crate::piston::Resolution {
                        to_push: Vec::new(),
                        to_destroy: Vec::new(),
                        push_direction: facing.opposite(),
                    },
                };
                // A sticky piston pulls; a normal one only drops its head.
                let resolution = if want_extended || sticky {
                    resolution
                } else {
                    crate::piston::Resolution { to_push: Vec::new(), ..resolution }
                };
                let writes = crate::piston::apply_move(
                    &redstone::make_lookup(column, min_x, min_z),
                    &resolution,
                    n.pos,
                    facing,
                    want_extended,
                    sticky,
                );
                let start = crate::piston::begin_move(&writes, &state, n.pos, facing, want_extended);

                // Destinations first, then the cells the run vacated, then the
                // base's own immediate write — `apply_move`'s own order, kept
                // because it is the order that never overwrites a block still
                // waiting to move. Each entry carries the block entity to schedule,
                // or `None` for a plain write.
                let mut plan: Vec<(BlockPos, String, Option<crate::piston::MovingBlockEntity>)> =
                    Vec::with_capacity(start.moving.len() + start.cleared.len() + 1);
                for (pos, moving_state, entity) in &start.moving {
                    plan.push((*pos, moving_state.clone(), Some(entity.clone())));
                }
                for pos in &start.cleared {
                    plan.push((*pos, "minecraft:air".to_string(), None));
                }
                if let Some(base_now) = &start.base_now {
                    plan.push((n.pos, base_now.clone(), None));
                }

                let mut fan_out = Vec::new();
                for (pos, to, entity) in plan {
                    let wlx = pos.x - min_x;
                    let wlz = pos.z - min_z;
                    if !(0..16).contains(&wlx)
                        || !(0..16).contains(&wlz)
                        || pos.y < column.min_y
                        || pos.y >= column.min_y + column.height
                    {
                        // Out of this column, so the `moving_piston` write cannot
                        // happen — and the commit must not be scheduled either, or a
                        // cell that never animated would still be rewritten two ticks
                        // late. Same border limit the module doc already records for
                        // the whole redstone family.
                        continue;
                    }
                    // A pending commit is scheduled even when the state write is a
                    // no-op: the write is idempotent (a cell already holding this
                    // exact `moving_piston` state) but the commit is what actually
                    // moves the block, so skipping it would strand the cell.
                    if let Some(entity) = &entity {
                        block_ticks.schedule(
                            (pos.x, pos.y, pos.z),
                            crate::piston::finish_kind(entity),
                            current_tick + crate::piston::PISTON_MOVE_DELAY,
                            TickPriority::Normal,
                        );
                    }
                    let from = column.block_state(wlx, pos.y, wlz).to_string();
                    if from == to {
                        continue;
                    }
                    column.set_block(wlx, pos.y, wlz, &to);
                    events.push(RandomTickEvent {
                        pos: (pos.x, pos.y, pos.z),
                        from,
                        to,
                    });
                    fan_out.push(Notification { pos, from: Direction::Down });
                }
                return fan_out;
            }
            return Vec::new();
        }

        // 3c. Comparators (#315).
        if redstone::is_comparator(&state) {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Comparator);
            let facing = redstone::diode_facing(&state);
            let input = redstone::input_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
            let side = redstone::alternate_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, facing, false);
            if redstone_diode::should_schedule_comparator_check(&state, input, side) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_COMPARATOR.to_string()) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    let priority = redstone_diode::comparator_schedule_priority(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        redstone::TICK_COMPARATOR.to_string(),
                        current_tick + 2,
                        priority,
                    );
                }
            }
            return Vec::new();
        }

        // 3c-bis. Hoppers (#321). `HopperBlock.checkPoweredState`
        // (`HopperBlock.java:125-130`), reached from `neighborChanged` (`:119-123`)
        // and `onPlace` (`:100-104`):
        //
        //     boolean shouldBeOn = !level.hasNeighborSignal(pos);
        //     if (shouldBeOn != state.getValue(ENABLED)) {
        //        level.setBlock(pos, state.setValue(ENABLED, shouldBeOn), 2);
        //     }
        //
        // Unlike every other family in this function, a hopper's reaction is
        // **immediate, not scheduled** — vanilla writes the new state right here
        // and there is no `scheduleTick` in that method. Flag 2 is
        // `UPDATE_CLIENTS` without `UPDATE_NEIGHBORS`, so the write does not
        // fan out further; returning an empty notification list is that.
        //
        // The `enabled` property is what `BlockEntityRegistry::tick_all` reads to
        // decide whether the hopper transfers, so this is the whole lock: the
        // block state is the single source of truth, exactly as in vanilla, and
        // it is a real property of `minecraft:hopper` so the client is told
        // precisely (see `redstone::with_property`).
        if redstone::is_hopper(&state) {
            let should_be_on =
                redstone::best_neighbor_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, false) == 0;
            if should_be_on != redstone::hopper_enabled(&state) {
                let new_state = redstone::with_property(&state, "enabled", if should_be_on { "true" } else { "false" });
                column.set_block(tlx, n.pos.y, tlz, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state, to: new_state });
            }
            return Vec::new();
        }

        // 3d. Observers (#317).
        if redstone::is_observer(&state) {
            crate::redstone_counters::bump_reaction(crate::redstone_counters::ReactionKind::Observer);
            let watch = redstone_observer::watch_direction(&state);
            if n.from == watch && redstone_observer::should_start_signal(&state) {
                if block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_OBSERVER.to_string()) {
                    crate::redstone_counters::bump_schedule_deduped();
                } else {
                    crate::redstone_counters::bump_schedule_requested();
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        redstone::TICK_OBSERVER.to_string(),
                        current_tick + 2,
                        TickPriority::Normal,
                    );
                }
            }
            return Vec::new();
        }

        // 3e. Redstone-openable blocks (#319): doors, trapdoors and fence
        // gates. `DoorBlock.neighborChanged` / `TrapDoorBlock.neighborChanged` /
        // `FenceGateBlock.neighborChanged` read whether the block is
        // redstone-powered and, when that differs from the stored `powered`,
        // write both `open` and `powered` to the new value — **immediately**,
        // with a flag-2 `setBlock` (no `scheduleTick`, no neighbour fan-out),
        // exactly like the hopper arm above rather than the delayed torch/
        // diode/observer families. See `crate::redstone_openable`'s module doc
        // for the full citation and for why the door's two-high half is synced
        // here (this crate has no `updateShape` pass for vanilla's to live in).
        if redstone_openable::is_openable(&state) {
            let has_signal = redstone_openable::has_neighbor_signal(
                &redstone::make_lookup(column, min_x, min_z),
                n.pos,
                &state,
            );
            if let Some(new_state) = redstone_openable::react(&state, has_signal) {
                // Resolve the other door half before `state` is moved into
                // the event below (this function has no `updateShape`, so the
                // half-sync vanilla performs there is done right here).
                let other_half = redstone_openable::other_door_half_pos(n.pos, &state);
                column.set_block(tlx, n.pos.y, tlz, &new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state,
                    to: new_state,
                });
                // A door occupies two cells; both halves must flip together.
                // Vanilla keeps them in sync through `DoorBlock.updateShape`;
                // this crate has no such pass, so the same `signal` is applied
                // to the other half here. The other half is not re-notified
                // (empty cascade below), matching flag 2's no-fan-out.
                if let Some(other) = other_half {
                    let other_lx = other.x - min_x;
                    let other_lz = other.z - min_z;
                    if (0..16).contains(&other_lx)
                        && (0..16).contains(&other_lz)
                        && other.y >= column.min_y
                        && other.y < column.min_y + column.height
                    {
                        let other_state = column.block_state(other_lx, other.y, other_lz).to_string();
                        if redstone_openable::is_door(&other_state) {
                            if let Some(other_new) = redstone_openable::react(&other_state, has_signal) {
                                column.set_block(other_lx, other.y, other_lz, &other_new);
                                events.push(RandomTickEvent {
                                    pos: (other.x, other.y, other.z),
                                    from: other_state,
                                    to: other_new,
                                });
                            }
                        }
                    }
                }
            }
            return Vec::new();
        }

        // 3f. Note blocks (issue #322's first fixture). `NoteBlock.neighborChanged`
        // (`NoteBlock.java:87-99`) — immediate, like the hopper/openable arms
        // above, not scheduled. See `crate::redstone_note_block`'s own module
        // doc for the client-visible "pulse" half this crate cannot transport
        // yet (`reaction.play_pulse` is computed correctly but not consumed
        // here — there is nowhere in this event type to put it).
        if base_name(&state) == redstone_note_block::NOTE_BLOCK {
            let (has_signal, above_is_air) = {
                let lookup = redstone::make_lookup(column, min_x, min_z);
                let has_signal = redstone::best_neighbor_signal(&lookup, n.pos, false) > 0;
                let above_state = lookup(Direction::Up.relative(n.pos));
                (has_signal, is_air_variant(&above_state))
            };
            if let Some(reaction) = redstone_note_block::on_neighbor_changed(&state, has_signal, above_is_air) {
                column.set_block(tlx, n.pos.y, tlz, &reaction.new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state,
                    to: reaction.new_state,
                });
            }
            return Vec::new();
        }

        // 3g. Powered/activator rails (issue #318's rail half — detector
        // rail's own producer is still unbuilt, see `crate::redstone_rail`'s
        // module doc). `PoweredRailBlock.updateState`, reached through
        // `BaseRailBlock.neighborChanged` (`:80-92`) since neither block
        // overrides `neighborChanged` itself.
        if redstone_rail::is_powered_rail_family(&state) {
            let new_state = {
                let lookup = redstone::make_lookup(column, min_x, min_z);
                let has_signal = |p: BlockPos| redstone::best_neighbor_signal(&lookup, p, false) > 0;
                redstone_rail::update_state(&lookup, &has_signal, n.pos, &state)
            };
            if let Some(new_state) = new_state {
                column.set_block(tlx, n.pos.y, tlz, &new_state);
                let shape = redstone_rail::shape_of(&new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state,
                    to: new_state,
                });
                if let Some(shape) = shape {
                    return redstone_rail::extra_notifications(n.pos, shape);
                }
            }
            return Vec::new();
        }

        // 3h. Dispensers/droppers (issue #320) — the `TRIGGERED` state
        // machine only. `DispenserBlock.neighborChanged`
        // (`DispenserBlock.java:127-139`); see `crate::redstone_dispenser`'s
        // own module doc for exactly why the actual fire (the scheduled tick
        // this arm schedules) has nothing to consume yet.
        if redstone_dispenser::is_dispenser_family(&state) {
            let should_trigger = {
                let lookup = redstone::make_lookup(column, min_x, min_z);
                redstone::best_neighbor_signal(&lookup, n.pos, false) > 0
                    || redstone::best_neighbor_signal(&lookup, Direction::Up.relative(n.pos), false) > 0
            };
            if let Some(reaction) = redstone_dispenser::on_neighbor_changed(&state, should_trigger) {
                column.set_block(tlx, n.pos.y, tlz, &reaction.new_state);
                events.push(RandomTickEvent {
                    pos: (n.pos.x, n.pos.y, n.pos.z),
                    from: state,
                    to: reaction.new_state,
                });
                if reaction.schedule_fire
                    && !block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone_dispenser::TICK_DISPENSER_FIRE.to_string())
                {
                    block_ticks.schedule(
                        (n.pos.x, n.pos.y, n.pos.z),
                        redstone_dispenser::TICK_DISPENSER_FIRE.to_string(),
                        current_tick + u64::from(redstone_dispenser::TRIGGER_DURATION),
                        TickPriority::Normal,
                    );
                }
            }
            return Vec::new();
        }

        Vec::new()
    }
}

/// Which of `column`'s palette entries are randomly ticking, indexed by
/// palette id.
///
/// **No longer the production path.** `ChunkColumn` now keeps this
/// classification permanently (one entry pushed per palette append) and a
/// per-section count derived from it, so [`RandomTickScheduler::tick_chunk`]
/// reaches the decision with an integer compare. This function and
/// [`section_has_randomly_ticking_block`] below are kept because they are the
/// **validated definition** of that decision: they are the tripwire's reference
/// arm in debug builds and the reference the unit test below compares against.
/// Deleting them would throw away the spec; leaving them in release builds
/// would be dead production code, so they are `cfg`-gated to exactly the
/// configurations that use them.
///
/// The prefilter that makes [`section_has_randomly_ticking_block`] affordable.
/// [`is_randomly_ticking`] is a **string** predicate (four `base_name` splits
/// in the worst case), and the scan below used to run it on all 4096 blocks of
/// every section, of every column, on every tick. A column's palette is tens
/// of entries, so classifying the palette once per column and then comparing
/// integers reaches the *identical* decision for a small constant instead of a
/// per-block one — the same argument
/// [`ChunkColumn::raw_palette`](crate::chunk::ChunkColumn::raw_palette)
/// already makes for the save path.
#[cfg(any(test, debug_assertions))]
fn randomly_ticking_palette_mask(column: &crate::chunk::ChunkColumn) -> Vec<bool> {
    column
        .raw_palette()
        .iter()
        .map(|state| is_randomly_ticking(state))
        .collect()
}

/// `LevelChunkSection::isRandomlyTicking`'s boolean, computed by scanning the
/// section's palette **indices** against `mask` — see this module's doc comment
/// for why a scan is the faithful reduction for a chunk representation with no
/// incremental per-section counter, and
/// [`randomly_ticking_palette_mask`] for why the scan tests integers.
///
/// The decision is bit-for-bit the one the string scan reached, so the
/// `tick_speed` position draws that follow it stay on the same LCG sequence.
///
/// See [`randomly_ticking_palette_mask`] for why this is no longer the
/// production path and why it is nonetheless kept.
#[cfg(any(test, debug_assertions))]
fn section_has_randomly_ticking_block(
    column: &crate::chunk::ChunkColumn,
    section_min_y: i32,
    mask: &[bool],
) -> bool {
    // Section-indexed rather than y-row-indexed since issue #551 packed the grid
    // per section (`crate::chunk_blocks`): `section_min_y` is a section boundary by
    // construction at every call site, so this is the same 4,096 cells the y-row
    // walk covered, reached through the accessor that now exists.
    let y_local = section_min_y - column.min_y;
    if y_local < 0 || y_local >= column.height {
        return false;
    }
    let mut cells = Vec::with_capacity(4096);
    column.append_section_cells(y_local as usize / 16, &mut cells);
    cells.iter().any(|&id| mask[id as usize])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkColumn;

    // # `next_random_tick_pos`: predicted values computed independently
    //
    // Computed via a standalone Python script (32-bit wrapping arithmetic,
    // arithmetic right shift), NOT by calling this Rust function — an
    // external re-derivation of the same jar formula, so this is a real
    // check against the spec rather than the function checking itself.
    // `next_random_tick_pos(state=12345, xo=0, yo=64, zo=0, y_mask=15)`,
    // five calls in sequence:
    //   (1013941258, x=2,  y=75, z=1)
    //   (-239239299, x=15, y=79, z=15)
    //   (296186326,  x=5,  y=73, z=12)
    //   (1902463201, x=8,  y=73, z=2)
    //   (-1868640766, x=0, y=71, z=3)
    #[test]
    fn position_pick_matches_independently_computed_lcg_sequence() {
        let mut state = 12345i32;
        let expected = [
            (1_013_941_258i32, 2, 75, 1),
            (-239_239_299, 15, 79, 15),
            (296_186_326, 5, 73, 12),
            (1_902_463_201, 8, 73, 2),
            (-1_868_640_766, 0, 71, 3),
        ];
        for (expected_state, ex, ey, ez) in expected {
            let (x, y, z) = next_random_tick_pos(&mut state, 0, 64, 0, 15);
            assert_eq!(state, expected_state, "LCG state diverged from the independently computed sequence");
            assert_eq!((x, y, z), (ex, ey, ez));
        }
    }

    /// Negative control: a different seed must diverge immediately — proves
    /// the test above is not vacuously true for any seed (e.g. a bugged
    /// function that ignores `state` entirely).
    #[test]
    fn a_different_seed_does_not_reproduce_the_same_first_position() {
        let mut state = 999i32;
        let (x, y, z) = next_random_tick_pos(&mut state, 0, 64, 0, 15);
        assert_ne!((x, y, z), (2, 75, 1), "control failed: different seeds must diverge");
    }

    /// Every position pick advances `position_state` exactly once, whether
    /// or not the picked block turns out eligible — mirrors
    /// `ServerLevel::tickChunk`'s unconditional `for (i = 0; i < tickSpeed; i++)`
    /// draw. Ticking one section at `tick_speed = 5` with **no** eligible
    /// block anywhere in it must still advance the position LCG exactly 5
    /// times — proven indirectly here by checking the *next* pick after a
    /// tick_chunk call with zero eligible blocks lands exactly where 5 raw
    /// `next_random_tick_pos` calls (computed independently) would put it.
    #[test]
    fn position_draws_happen_even_when_no_block_is_eligible() {
        let mut column = ChunkColumn::new(0, 16);
        // Fill the one section with stone: zero grass blocks anywhere, so
        // `section_has_randomly_ticking_block` is false — this must SKIP the
        // whole section (zero draws), which is the real prediction for this
        // setup. See the companion test below for the "eligible section,
        // zero hits" case, which is where the "still draws" claim actually
        // bites.
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        let mut scheduler = RandomTickScheduler::new(12345, 0);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = scheduler.tick_chunk(&mut column, 0, 0, 5, &mut block_ticks, 0);
        assert!(events.is_empty());

        // An ineligible SECTION draws zero — confirmed by the position LCG
        // not having moved at all from its seed.
        assert_eq!(scheduler_position_state(&scheduler), 12345);

        // Negative control, proving the assertion above is not vacuous:
        // `next_random_tick_pos` really does mutate its state in general
        // (i.e. "zero draws happened" is a real, distinguishable outcome,
        // not just what this function always does regardless of input).
        let mut control_state = 12345i32;
        let _ = next_random_tick_pos(&mut control_state, 0, 0, 0, 15);
        assert_ne!(control_state, 12345, "control failed: the LCG must actually advance when called");
    }

    fn scheduler_position_state(s: &RandomTickScheduler) -> i32 {
        s.position_state
    }

    /// The real "still draws on a miss" case: a section WITH one eligible
    /// grass block, ticked at `tick_speed = 5`. Vanilla draws exactly 5
    /// positions regardless of how many of those 5 draws actually land on
    /// the grass block — predicted here as "the position LCG advances
    /// exactly 5 times," independent of hits.
    #[test]
    fn position_draws_happen_exactly_tick_speed_times_per_eligible_section_regardless_of_hits() {
        let mut column = ChunkColumn::new(0, 16);
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        // One grass block, buried under stone above (so it always dies —
        // zero behaviour draws — keeping this test purely about the
        // POSITION draw count, not grass's own behaviour draws).
        column.set_block(0, 0, 0, GRASS_BLOCK);

        let mut scheduler = RandomTickScheduler::new(12345, 0);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        scheduler.tick_chunk(&mut column, 0, 0, 5, &mut block_ticks, 0);

        let mut expected_state = 12345i32;
        for _ in 0..5 {
            let _ = next_random_tick_pos(&mut expected_state, 0, 0, 0, 15);
        }
        assert_eq!(
            scheduler_position_state(&scheduler),
            expected_state,
            "expected exactly 5 position draws for tick_speed=5 on one eligible section"
        );
    }

    /// `grass_random_tick`'s die branch: exactly zero `next_int` draws.
    /// Proven by comparing the RNG's state against an untouched clone.
    #[test]
    fn dying_to_dirt_consumes_zero_behavior_draws() {
        let mut rng = SpawnRng::new(7);
        let before = format!("{rng:?}");
        let outcome = grass_random_tick(false, &mut rng, |_, _, _| true);
        assert_eq!(outcome, GrassOutcome::DiesToDirt);
        assert_eq!(format!("{rng:?}"), before, "the die branch must not draw from the behaviour RNG at all");
    }

    /// `grass_random_tick`'s spread branch: exactly 12 draws (4 attempts * 3
    /// axes), proven by replaying 12 raw `next_int` calls against an
    /// independently seeded clone and asserting the resulting states match —
    /// not merely a count, the actual draw *pattern*.
    #[test]
    fn spreading_consumes_exactly_twelve_behavior_draws_regardless_of_hits() {
        let mut rng_a = SpawnRng::new(7);
        let _ = grass_random_tick(true, &mut rng_a, |_, _, _| false); // every attempt rejected
        let after_a = format!("{rng_a:?}");

        let mut rng_b = SpawnRng::new(7);
        for i in 0..12 {
            let bound = if i % 3 == 1 { 5 } else { 3 };
            let _ = rng_b.next_int(bound);
        }
        let after_b = format!("{rng_b:?}");

        assert_eq!(after_a, after_b, "expected exactly 12 draws (bounds 3,5,3 repeated 4x) regardless of hits");
    }

    /// Negative control: proves the equality check above can actually fail
    /// — an 11-draw replay must NOT match.
    #[test]
    fn eleven_draws_do_not_match_the_real_twelve_draw_pattern() {
        let mut rng_a = SpawnRng::new(7);
        let _ = grass_random_tick(true, &mut rng_a, |_, _, _| false);
        let after_a = format!("{rng_a:?}");

        let mut rng_b = SpawnRng::new(7);
        for i in 0..11 {
            let bound = if i % 3 == 1 { 5 } else { 3 };
            let _ = rng_b.next_int(bound);
        }
        let after_b = format!("{rng_b:?}");
        assert_ne!(after_a, after_b, "control failed: 11 draws must not equal 12");
    }

    /// End-to-end: a grass block covered by stone dies to dirt in one tick,
    /// via `tick_chunk` against a real `ChunkColumn` — the "at least one
    /// real ticking block" proof at the column level (the client-visible
    /// proof lives in `tick.rs`'s own wiring).
    #[test]
    fn a_covered_grass_block_becomes_dirt_after_one_tick_chunk_call() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, GRASS_BLOCK);
        column.set_block(3, 6, 3, "minecraft:stone"); // covers it: not air-exposed
        assert_eq!(column.block_state(3, 5, 3), GRASS_BLOCK);

        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // `tick_speed = 200` and 3000 calls: the position pick lands on
        // (3, 5, 3) with probability 200/4096 per call, so the expected hit
        // count here is ~146 — comfortably certain (P(zero hits) ~ e^-146)
        // without asserting anything about *which specific* draw hits, only
        // that "eventually" is a real, bounded claim rather than a fluke of
        // the first LCG output. Loop rather than assume the very first call
        // hits it.
        let mut converted = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == (3, 5, 3) && e.to == DIRT_BLOCK) {
                converted = true;
                break;
            }
        }
        assert!(converted, "a covered grass block must eventually die to dirt");
        assert_eq!(column.block_state(3, 5, 3), DIRT_BLOCK);
    }

    /// Negative control for the end-to-end test: an UNCOVERED grass block
    /// (air above) must NOT die to dirt, however many ticks run — proving
    /// the die branch's gate actually discriminates on `above_is_air`
    /// rather than firing unconditionally.
    #[test]
    fn an_uncovered_grass_block_never_dies_to_dirt() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, GRASS_BLOCK);
        // Above is air by construction (ChunkColumn::new is all-air).
        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..500 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0);
            assert!(
                !events.iter().any(|e| e.to == DIRT_BLOCK),
                "an air-exposed grass block must never die to dirt"
            );
        }
        assert_eq!(column.block_state(3, 5, 3), GRASS_BLOCK);
    }

    /// **Issue #544: which above-block kills grass, predicted from vanilla's
    /// record and the dampening census rather than from this crate's answer.**
    ///
    /// `SpreadingSnowyBlock.canStayAlive` is, in order: snow with `LAYERS == 1`
    /// is `true`; a **full** fluid state is `false`; otherwise
    /// `getLightDampeningInto(...) < 15`, which for two full-cube states is the
    /// above block's own `getLightDampening()` — exactly
    /// `lodestone_data::light_props::dampening`'s column.
    ///
    /// # The fixture, stated because this is badly exposed to the *world* species
    ///
    /// A fixture of only air (survives) and only stone (dies) **cannot see this
    /// bug at all** — both proxy and real predicate agree on those two — and that
    /// is precisely the fixture shape the old `is_air_variant` proxy shipped
    /// under. So the rows below are chosen to *disagree* under the two
    /// hypotheses, and each row's `dampening` value is asserted as a hard
    /// precondition so the prediction's basis is visible rather than implied:
    ///
    /// | above | dampening | vanilla | the old air proxy |
    /// |---|---|---|---|
    /// | `air` | 0 | survives | survives (agrees) |
    /// | `short_grass` | 0 | **survives** | **dies** ← the reported bug |
    /// | `oak_leaves` | 1 | survives | dies |
    /// | `torch` | 0 | survives | dies |
    /// | `stone` | 15 | dies | dies (agrees) |
    /// | `snow[layers=1]` | — | **survives** (explicit special case) | dies |
    /// | `water[level=0]` | 1 | **dies** (full fluid, checked *before* dampening) | dies |
    /// | `water[level=3]` | 1 | survives (flowing is not full) | dies |
    /// | waterlogged slab | — | **dies** (its *fluid state* is full) | dies |
    ///
    /// The `water[level=0]` row is the one that makes the ordering load-bearing:
    /// water's dampening is `1`, so a predicate that only compared dampening
    /// would let grass live under an ocean.
    #[test]
    fn grass_survives_exactly_the_above_blocks_vanillas_can_stay_alive_allows() {
        // Preconditions on the fixture itself. A name this version's census does
        // not carry would otherwise fall through `grass_can_stay_alive`'s
        // unknown-state arm and read as "survives" for the wrong reason.
        for state in [
            "minecraft:air",
            "minecraft:short_grass",
            "minecraft:stone",
            "minecraft:snow[layers=1]",
            "minecraft:water[level=0]",
            "minecraft:water[level=3]",
            "minecraft:torch",
        ] {
            assert!(
                crate::mobs::block_state_id(state).is_some(),
                "fixture precondition: {state} must be a real 26.2 block state, or \
                 this test measures the unknown-state fallback instead"
            );
        }
        let dampening = |state: &str| {
            let id = crate::mobs::block_state_id(state)
                .unwrap_or_else(|| panic!("{state} is not a known block state"));
            lodestone_data::light_props::dampening(id)
        };
        // The values the predictions rest on, asserted from the census.
        assert_eq!(dampening("minecraft:air"), 0);
        assert_eq!(
            dampening("minecraft:short_grass"),
            0,
            "short grass dampens no light, which is why vanilla's grass survives \
             under it — the whole of issue #544"
        );
        assert_eq!(dampening("minecraft:stone"), 15, "a full solid is the kill case");
        assert!(
            dampening("minecraft:water[level=0]") < 15,
            "water dampens only a little, so the FULL-FLUID check must run before \
             the dampening comparison or grass survives underwater"
        );

        for (above, expected) in [
            ("minecraft:air", true),
            ("minecraft:short_grass", true),
            ("minecraft:oak_leaves[distance=7,persistent=false,waterlogged=false]", true),
            ("minecraft:torch", true),
            ("minecraft:stone", false),
            ("minecraft:snow[layers=1]", true),
            ("minecraft:water[level=0]", false),
            ("minecraft:water[level=3]", true),
            ("minecraft:oak_slab[type=bottom,waterlogged=true]", false),
        ] {
            assert_eq!(
                grass_can_stay_alive(above),
                expected,
                "canStayAlive under {above}: vanilla says {expected}"
            );
        }
    }

    /// The three `isFull()` cases, since `has_full_fluid` is what stops grass
    /// living under an ocean and a `level == 0` test misses two of them.
    ///
    /// `LiquidBlock.getFluidState` maps `level` to `amount = 8 - level` when
    /// `level < 8` and to `8` (falling) otherwise, and `isFull()` is
    /// `amount == 8`.
    #[test]
    fn a_full_fluid_state_is_source_falling_or_waterlogged() {
        assert!(has_full_fluid("minecraft:water[level=0]"), "a source block");
        assert!(has_full_fluid("minecraft:water"), "no properties is the default, level=0");
        assert!(has_full_fluid("minecraft:lava[level=0]"));
        assert!(
            has_full_fluid("minecraft:water[level=8]"),
            "level 8..=15 is FALLING water, whose amount is 8 — a `level == 0` \
             test reads this as not full"
        );
        assert!(has_full_fluid("minecraft:water[level=15]"));
        assert!(
            has_full_fluid("minecraft:oak_slab[type=bottom,waterlogged=true]"),
            "a waterlogged block's *fluid state* is full even though its block is \
             not water — canStayAlive reads the fluid state"
        );
        assert!(!has_full_fluid("minecraft:water[level=1]"), "flowing, amount 7");
        assert!(!has_full_fluid("minecraft:water[level=7]"), "flowing, amount 1");
        assert!(!has_full_fluid("minecraft:air"));
        assert!(!has_full_fluid("minecraft:oak_slab[type=bottom,waterlogged=false]"));
        // The whole-key match: a property whose *value* contains the key name
        // must not be mistaken for it.
        assert!(!has_full_fluid("minecraft:stone[shape=waterlogged=true]"));
    }

    /// `canPropagate` is **`canStayAlive` AND not any water fluid**, so grass
    /// does not spread into a shallow stream it could survive under. The
    /// `water[level=3]` row is the only one where the two conditions disagree,
    /// and it is the reason `can_propagate_onto` cannot simply call
    /// `grass_can_stay_alive`.
    #[test]
    fn can_propagate_rejects_flowing_water_that_can_stay_alive_accepts() {
        let flowing = "minecraft:water[level=3]";
        assert!(
            grass_can_stay_alive(flowing),
            "precondition: flowing water is not a full fluid, so canStayAlive accepts it"
        );
        assert!(
            !can_propagate_onto(DIRT_BLOCK, flowing),
            "canPropagate additionally rejects any WATER fluid, flowing included"
        );
        assert!(can_propagate_onto(DIRT_BLOCK, "minecraft:air"));
        assert!(
            can_propagate_onto(DIRT_BLOCK, "minecraft:short_grass"),
            "issue #544's other half: grass spreads under short grass too"
        );
        assert!(!can_propagate_onto(DIRT_BLOCK, "minecraft:stone"));
        assert!(
            !can_propagate_onto("minecraft:stone", "minecraft:air"),
            "the target must be dirt (vanilla's `is(baseBlock)` at the call site)"
        );
    }

    /// **The end-to-end half of #544, through the real `tick_chunk` driver:**
    /// grass under short grass must survive, and the draw count must be the
    /// *live* one (12 behaviour draws), not the die branch's zero.
    ///
    /// This is a paired assertion on purpose. "It did not die" alone is also
    /// satisfied by the position pick never landing on the block, so the second
    /// half — that the behaviour RNG advanced — is what proves the tick actually
    /// ran and took the live branch. The companion
    /// `a_covered_grass_block_becomes_dirt_after_one_tick_chunk_call` (stone
    /// above) is the control that the die branch still fires.
    #[test]
    fn grass_under_short_grass_survives_the_real_tick_driver_and_takes_the_live_branch() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, GRASS_BLOCK);
        column.set_block(3, 6, 3, "minecraft:short_grass");
        assert_eq!(
            column.block_state(3, 6, 3),
            "minecraft:short_grass",
            "fixture precondition: the cover is short grass, not air and not stone \
             — a fixture of either cannot see this bug"
        );

        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            assert!(
                !events.iter().any(|e| e.to == DIRT_BLOCK),
                "grass under short grass must never die to dirt"
            );
        }
        assert_eq!(column.block_state(3, 5, 3), GRASS_BLOCK);
        // The tick really ran: with `tick_speed = 200` over 3,000 calls the
        // position pick lands on this cell ~146 times, and each landing costs
        // 12 behaviour draws on the live branch and 0 on the die branch. So a
        // behaviour RNG that never moved would mean either "never picked"
        // (P ~ e^-146) or "took the die branch".
        assert_ne!(
            scheduler.behavior_rng.next_int(1 << 30),
            RandomTickScheduler::new(1, 1).behavior_rng.next_int(1 << 30),
            "the behaviour RNG must have advanced — otherwise this test proves \
             only that the block was never ticked"
        );
    }

    /// End-to-end spread: a dirt block adjacent to an air-exposed grass
    /// block, itself also air-exposed, must eventually turn to grass.
    #[test]
    fn an_eligible_neighboring_dirt_block_eventually_becomes_grass() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK);
        column.set_block(6, 5, 5, DIRT_BLOCK); // one step east, also air-exposed above
        let mut scheduler = RandomTickScheduler::new(2, 2);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // Two independent random events must both happen: the position pick
        // must land on the grass block (`tick_speed = 200` / 4096 chance per
        // call), AND one of its 4 spread attempts must draw the exact
        // (+1, 0, 0) offset (chance ~0.0866 per pick — see
        // `spreading_consumes_exactly_twelve_behavior_draws_regardless_of_hits`
        // for where the 3/5/3 bounds come from). Combined per-call
        // probability ~0.0042; 3000 calls gives an expected ~12.7 hits
        // (P(zero) ~ 3e-6).
        let mut spread = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == (6, 5, 5) && base_name(&e.to) == GRASS_BLOCK) {
                spread = true;
                break;
            }
        }
        assert!(spread, "an eligible adjacent dirt block must eventually turn to grass");
    }

    /// #546, the two halves of `snowy`. The tag is `#minecraft:snow` — three
    /// blocks, so `snow_block` counts and this is not a `minecraft:snow` check
    /// — and `SnowyBlock.updateShape` moves the property in both directions
    /// when the block above changes.
    #[test]
    fn snowy_tracks_the_block_above_in_both_directions() {
        assert!(is_snowy_setting("minecraft:snow_block"));
        assert!(is_snowy_setting("minecraft:powder_snow"));
        assert!(is_snowy_setting("minecraft:snow[layers=1]"));
        assert!(!is_snowy_setting("minecraft:short_grass"));

        let mut column = ChunkColumn::new(0, 16);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        column.set_block(5, 5, 5, "minecraft:grass_block[snowy=false]");

        column.set_block(5, 6, 5, "minecraft:snow_block");
        propagate_and_react(&mut column, 0, 0, 5, 6, 5, &mut block_ticks, 0);
        assert_eq!(column.block_state(5, 5, 5), "minecraft:grass_block[snowy=true]");

        column.set_block(5, 6, 5, crate::chunk::AIR);
        propagate_and_react(&mut column, 0, 0, 5, 6, 5, &mut block_ticks, 0);
        assert_eq!(column.block_state(5, 5, 5), "minecraft:grass_block[snowy=false]");
    }

    /// Determinism control: two independently constructed schedulers, same
    /// seeds, same script, must produce byte-identical event sequences —
    /// two separate `RandomTickScheduler::new` calls, not one instance
    /// ticked twice (CLAUDE.md's own warning about pointer-identity gates).
    #[test]
    fn two_independently_built_schedulers_produce_identical_events() {
        fn run() -> Vec<RandomTickEvent> {
            let mut column = ChunkColumn::new(0, 16);
            column.set_block(1, 1, 1, GRASS_BLOCK);
            column.set_block(2, 1, 1, "minecraft:stone");
            let mut scheduler = RandomTickScheduler::new(555, 555);
            let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let mut all = Vec::new();
            for _ in 0..50 {
                all.extend(scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0));
            }
            all
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn default_random_tick_speed_is_three() {
        assert_eq!(DEFAULT_RANDOM_TICK_SPEED, 3);
    }

    // # Issue #310 end-to-end: crop growth, sapling growth, leaf decay
    // through `tick_chunk` against a real `ChunkColumn` — the same level of
    // proof `a_covered_grass_block_becomes_dirt_after_one_tick_chunk_call`
    // gives grass, above. The pure per-branch draw-pattern proofs live in
    // `crate::growth_tick`'s own test module; these tests are about the
    // DISPATCH (`is_randomly_ticking` selecting the position, then routing
    // to the right handler) actually wiring into `tick_chunk`.

    /// An air-exposed, sub-max-age wheat crop eventually grows by exactly
    /// one age step — proven the same probabilistic-but-bounded way the
    /// existing grass tests are (loop until observed, with an astronomically
    /// small false-negative probability), not a single lucky seed.
    #[test]
    fn an_air_exposed_wheat_crop_eventually_grows_one_age() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(4, 5, 4, "minecraft:wheat[age=0]");
        // Above is air by construction (ChunkColumn::new is all-air).
        let mut scheduler = RandomTickScheduler::new(21, 21);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut grew = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == (4, 5, 4) && e.to == "minecraft:wheat[age=1]") {
                grew = true;
                break;
            }
        }
        assert!(grew, "an air-exposed sub-max-age wheat crop must eventually grow");
        assert_eq!(column.block_state(4, 5, 4), "minecraft:wheat[age=1]");
    }

    /// Negative control for the assertion above: a crop already at max age
    /// must NEVER grow (or even get selected — `is_randomly_ticking` gates
    /// it out entirely), however many ticks run.
    #[test]
    fn a_max_age_wheat_crop_never_grows_further() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(4, 5, 4, "minecraft:wheat[age=7]");
        let mut scheduler = RandomTickScheduler::new(21, 21);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..500 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0);
            assert!(events.is_empty(), "a max-age crop must never be selected for a random tick at all");
        }
        assert_eq!(column.block_state(4, 5, 4), "minecraft:wheat[age=7]");
    }

    /// Negative control, the light-gated half: a wheat crop covered by stone
    /// (not air-exposed) must never grow, however many ticks run — proving
    /// the light proxy actually gates growth rather than being decorative.
    #[test]
    fn a_covered_wheat_crop_never_grows() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(4, 5, 4, "minecraft:wheat[age=0]");
        column.set_block(4, 6, 4, "minecraft:stone"); // covers it
        let mut scheduler = RandomTickScheduler::new(21, 21);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..500 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0);
            assert!(events.is_empty(), "a covered wheat crop must never grow (or draw at all)");
        }
        assert_eq!(column.block_state(4, 5, 4), "minecraft:wheat[age=0]");
    }

    /// An air-exposed oak sapling at stage 0 eventually advances to stage 1.
    #[test]
    fn an_air_exposed_sapling_eventually_advances_to_stage_one() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(2, 5, 2, "minecraft:oak_sapling[stage=0]");
        let mut scheduler = RandomTickScheduler::new(9, 9);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut advanced = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == (2, 5, 2) && e.to == "minecraft:oak_sapling[stage=1]") {
                advanced = true;
                break;
            }
        }
        assert!(advanced, "an air-exposed sapling must eventually advance to stage 1");
        assert_eq!(column.block_state(2, 5, 2), "minecraft:oak_sapling[stage=1]");
    }

    /// A stage-1 sapling never produces an event at all: the "grow a tree"
    /// branch is a named no-op (`growth_tick::SaplingOutcome::TreeGrowthNotModeled`),
    /// not a silent mutation — pinned here at the `tick_chunk` level so a
    /// future tree feature landing changes this test, loudly, rather than
    /// this crate quietly starting to fabricate trees unnoticed.
    #[test]
    fn a_stage_one_sapling_never_produces_an_event() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(2, 5, 2, "minecraft:oak_sapling[stage=1]");
        let mut scheduler = RandomTickScheduler::new(9, 9);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            assert!(events.is_empty(), "a stage-1 sapling must never mutate — no tree feature exists");
        }
        assert_eq!(column.block_state(2, 5, 2), "minecraft:oak_sapling[stage=1]");
    }

    /// A distance-7, non-persistent leaf decays to air on the very first
    /// tick it is selected for — zero draws means zero probabilistic delay,
    /// so (unlike grass/crops) this needs no retry loop, only enough ticks
    /// to guarantee the position LCG lands on it at least once.
    #[test]
    fn a_decaying_leaf_becomes_air() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(6, 5, 6, "minecraft:oak_leaves[distance=7,persistent=false]");
        let mut scheduler = RandomTickScheduler::new(4, 4);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut decayed = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == (6, 5, 6) && e.to == "minecraft:air") {
                decayed = true;
                break;
            }
        }
        assert!(decayed, "a distance-7 non-persistent leaf must eventually decay");
        assert_eq!(column.block_state(6, 5, 6), "minecraft:air");
    }

    /// Negative control: a persistent leaf at the same distance never
    /// decays, however many ticks run — proving `persistent` actually gates
    /// selection (via `is_randomly_ticking`), not just the action.
    #[test]
    fn a_persistent_leaf_never_decays() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(6, 5, 6, "minecraft:oak_leaves[distance=7,persistent=true]");
        let mut scheduler = RandomTickScheduler::new(4, 4);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..500 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0);
            assert!(events.is_empty(), "a persistent leaf must never be selected for a random tick");
        }
        assert_eq!(column.block_state(6, 5, 6), "minecraft:oak_leaves[distance=7,persistent=true]");
    }

    /// Negative control: a leaf within range of a log (`distance < 7`) never
    /// decays, however many ticks run.
    #[test]
    fn a_leaf_within_range_of_a_log_never_decays() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(6, 5, 6, "minecraft:oak_leaves[distance=3,persistent=false]");
        let mut scheduler = RandomTickScheduler::new(4, 4);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..500 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0);
            assert!(events.is_empty(), "a leaf within range of a log must never be selected for a random tick");
        }
        assert_eq!(column.block_state(6, 5, 6), "minecraft:oak_leaves[distance=3,persistent=false]");
    }

    // # Gravity blocks reached through `NeighborPropagator`'s first real
    // production call. Every test below triggers the reaction via an ADJACENT
    // random-tick mutation (grass dying to dirt) — this crate's only producer
    // reachable from *this* module, since block-place/break lives in `server.rs`.
    //
    // ## These assertions were inverted, deliberately, and reading them cannot
    // ## tell you that — so it is written down here
    //
    // Two of the three gates below used to assert the **teleport**: that an
    // adjacent mutation moved the sand from `y = 5` to `y = 0` inside
    // `tick_chunk`, in one step, with no entity. That was correct and evidenced
    // when written — the `FallingBlockEntity` did not exist and
    // `settle_gravity_at` really did write the block at its landing position. It
    // is now the *bug the owner reported* ("it just teleports to its final place
    // at the bottom instead of falling down and landing"), so the gates assert
    // the opposite: the sand does not move here at all, and what the notification
    // produces is a scheduled `TICK_GRAVITY` — `FallingBlock.updateShape`'s
    // `scheduleTick(pos, this, getDelayAfterPlace())` and nothing else.
    //
    // The fall itself is `crate::tick`'s drain plus `crate::mobs`, one layer up,
    // and `crate::gravity_tick`'s own tests own the motion. Nothing in *this*
    // module moves a gravity block any more, which is the point.

    /// A sand block adjacent to a grass-dies-to-dirt conversion, with nothing
    /// solid beneath it, **schedules its own gravity tick and does not move**.
    ///
    /// Both halves are load-bearing and the second is the inverted one. The
    /// position assertion is what separates a correct schedule from
    /// `propagate`'s natural mistake: it notifies an origin's six neighbours and
    /// not the origin, so a tick scheduled at the grass block's cell instead
    /// would look entirely right in a queue dump and settle nothing.
    ///
    /// The delay is the predicted value and the candidate readings are evaluated
    /// rather than assumed: `getDelayAfterPlace` is `2`, so a notification
    /// resolved on tick `T` fires at `T + 2` — never `T` (which is the immediate
    /// settle this replaced) and never `T + 1`.
    #[test]
    fn an_unsupported_gravity_block_schedules_a_gravity_tick_and_does_not_move() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK);
        column.set_block(5, 6, 5, "minecraft:stone"); // covers the grass: dies to dirt
        column.set_block(6, 5, 5, "minecraft:sand"); // east neighbour, unsupported (air below by default)
        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut scheduled = false;
        for _ in 0..3000 {
            scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if block_ticks.has_scheduled((6, 5, 5), &gravity_tick::TICK_GRAVITY.to_string()) {
                scheduled = true;
                break;
            }
        }
        assert!(
            scheduled,
            "an unsupported sand block adjacent to a grass conversion must schedule \
             its own `FallingBlock` tick"
        );
        assert!(
            !block_ticks.has_scheduled((5, 5, 5), &gravity_tick::TICK_GRAVITY.to_string()),
            "the tick belongs to the sand, not to the cell that was mutated"
        );
        // The inverted half: no teleport.
        assert_eq!(
            column.block_state(6, 5, 5),
            "minecraft:sand",
            "the sand must still be where it was — the fall is the drain's job now, \
             and moving it here is the teleport this landing removed"
        );
        assert_eq!(
            column.block_state(6, 0, 5),
            "minecraft:air",
            "nothing may appear at the landing position from this layer"
        );

        let due = block_ticks.drain_due(u64::MAX, usize::MAX);
        let gravity: Vec<_> = due
            .iter()
            .filter(|t| t.kind == gravity_tick::TICK_GRAVITY)
            .collect();
        assert_eq!(gravity.len(), 1, "one tick, not one per notification");
        assert_eq!(
            gravity[0].trigger_tick,
            gravity_tick::DELAY_AFTER_PLACE,
            "getDelayAfterPlace is 2: not 0 (the immediate settle) and not 1"
        );
    }

    /// Negative control, **repointed**: the support test now belongs to
    /// `settle_gravity_at` rather than to the notification.
    ///
    /// `FallingBlock.updateShape` schedules unconditionally for *any*
    /// `FallingBlock` — there is no `isFree(below)` test in it — so a supported
    /// sand block does get a scheduled tick, and the discrimination happens in
    /// `FallingBlock.tick`. Asserting "no tick was scheduled" here would
    /// therefore be asserting a bug. This gate instead requires
    /// `settle_gravity_at` to answer `None` for the supported block and `Some`
    /// for the unsupported one, in the same column, so the detector is proven to
    /// discriminate rather than to always refuse.
    #[test]
    fn support_is_what_settle_gravity_at_discriminates_on_not_the_notification() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(6, 5, 5, "minecraft:sand");
        column.set_block(6, 4, 5, "minecraft:stone"); // real support
        column.set_block(9, 5, 5, "minecraft:sand"); // unsupported: air below
        assert_eq!(
            settle_gravity_at(&column, 0, 0, 6, 5, 5),
            None,
            "a supported sand block must never become a falling entity"
        );
        assert_eq!(
            settle_gravity_at(&column, 0, 0, 9, 5, 5),
            Some(GravitySettle {
                state: "minecraft:sand".to_string(),
                landing_y: 0,
            }),
            "control failed: the unsupported arm must be `Some`, or the `None` above \
             is measuring nothing"
        );
        assert_eq!(
            column.block_state(9, 5, 5),
            "minecraft:sand",
            "`settle_gravity_at` must not move anything — it answers, it does not act"
        );
    }

    /// A stacked column: only the block the propagation actually **reaches**
    /// schedules a tick.
    ///
    /// This gate used to assert that both blocks teleported in one `tick_chunk`
    /// call, cascading through a `Direction::Down` re-notification from the
    /// vacated cell. That cascade is now one layer up and one tick later: the
    /// bottom sand becomes an entity in `crate::tick`'s drain, whose
    /// `propagate_and_react` on the vacated cell is what notifies the gravel — so
    /// the pile still collapses layer by layer, with vanilla's delay per layer
    /// instead of resolving the whole column inside one tick.
    ///
    /// What is assertable *here* is the boundary: the gravel is a neighbour of a
    /// neighbour, so it must **not** be scheduled by this pass. That is the
    /// discriminating claim — a propagation that fanned out one layer too far
    /// would schedule it, and the old teleporting version effectively did.
    #[test]
    fn only_the_notified_block_of_a_stack_schedules_and_neither_moves() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK);
        column.set_block(5, 6, 5, "minecraft:stone");
        column.set_block(6, 5, 5, "minecraft:sand"); // bottom of the stack, unsupported
        column.set_block(6, 6, 5, "minecraft:gravel"); // resting on top of the sand
        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let kind = gravity_tick::TICK_GRAVITY.to_string();
        let mut scheduled = false;
        for _ in 0..3000 {
            scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if block_ticks.has_scheduled((6, 5, 5), &kind) {
                scheduled = true;
                break;
            }
        }
        assert!(scheduled, "the sand the propagation reaches must schedule its own tick");
        assert!(
            !block_ticks.has_scheduled((6, 6, 5), &kind),
            "the gravel is a neighbour of a neighbour: this pass must not reach it. \
             The cascade is the scheduled-tick drain's, one tick later."
        );
        assert_eq!(column.block_state(6, 5, 5), "minecraft:sand", "no teleport");
        assert_eq!(column.block_state(6, 6, 5), "minecraft:gravel", "no teleport");
    }

    // # Issue #472: local vs absolute `z` in grass propagation
    //
    // Every other test in this module ticks chunk `(0, 0)`, where `min_z` is
    // 0 and so local `z` == absolute `z`. That makes the two coordinates
    // indistinguishable and the bug structurally invisible — the *world*
    // species of vacuous test, where the flaw is in the fixture rather than
    // in anything readable in the test body. The two gates below tick chunk
    // `(2, 3)` instead, so `min_z = 48` and local 5 is absolute 53.
    //
    // ## Where the wrong read lands
    //
    // `ChunkColumn::index` is `((y_local * 16 + z) * 16 + x)` with a
    // `debug_assert!((0..16).contains(&z))`. Passing an absolute
    // `tz = cz * 16 + tlz` therefore panics in a debug build, and in a
    // release build (where the assert compiles out) silently aliases onto
    // local `(tlx, ty + cz, tlz)` — the same column, `cz` y-levels too high.
    // For chunk `(2, 3)` and a target at local `(6, 5, 5)`:
    //
    //   index(6, y_local=5, z=53) = (5 * 16 + 53) * 16 + 6 = 2134
    //   index(6, y_local=8, z= 5) = (8 * 16 +  5) * 16 + 6 = 2134
    //
    // so the misread lands on local `(6, 8, 5)`, and its `ty + 1` companion
    // on local `(6, 9, 5)`. Both are inside the 4096-cell backing store, so
    // release genuinely misreads rather than panicking on a slice bound.
    // The two cells are stocked deliberately in each gate below.

    /// #472, forward direction: an eligible dirt block must still be found
    /// when the chunk's `min_z` is non-zero. The two cells the absolute-`z`
    /// misread aliases onto are stocked with stone, so under the bug
    /// `can_propagate_onto("minecraft:stone", ..)` is false and the spread
    /// can never happen — a release build fails on the loop exhausting, a
    /// debug build fails on `ChunkColumn::index`'s `debug_assert`.
    #[test]
    fn grass_spreads_at_a_chunk_whose_local_and_absolute_z_differ() {
        const CX: i32 = 2;
        const CZ: i32 = 3;
        let (min_x, min_z) = (CX * 16, CZ * 16);

        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK); // source, air above
        column.set_block(6, 5, 5, DIRT_BLOCK); // one step east, air above
        // The cells an absolute-`z` read would alias onto (see the block
        // comment above): stone rejects `can_propagate_onto`, so the buggy
        // read cannot accidentally agree with the correct one.
        column.set_block(6, 8, 5, "minecraft:stone");
        column.set_block(6, 9, 5, "minecraft:stone");

        let target_abs = (min_x + 6, 5, min_z + 5); // (38, 5, 53)
        assert_ne!(target_abs.2, 5, "fixture must have absolute z != local z, or it cannot see #472");

        let mut scheduler = RandomTickScheduler::new(2, 2);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // Same two-independent-events reasoning as
        // `an_eligible_neighboring_dirt_block_eventually_becomes_grass`:
        // ~0.0042 per call, so 3000 calls gives ~12.7 expected hits.
        let mut spread = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, CX, CZ, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == target_abs && base_name(&e.to) == GRASS_BLOCK) {
                spread = true;
                break;
            }
        }
        assert!(
            spread,
            "dirt at local (6, 5, 5) = absolute {target_abs:?} in chunk ({CX}, {CZ}) must become grass; \
             it did not, so the propagation probe read the wrong cell (#472). \
             local (6,5,5) is {:?}, the absolute-z alias local (6,8,5) is {:?}",
            column.block_state(6, 5, 5),
            column.block_state(6, 8, 5),
        );
        // `snowy=false` explicitly (#546): the spread write sets the property
        // vanilla's `SpreadingSnowyBlock.randomTick` sets, air being above.
        assert_eq!(column.block_state(6, 5, 5), "minecraft:grass_block[snowy=false]", "at local (6, 5, 5)");
        // Nothing may have been written at the alias cells.
        assert_eq!(column.block_state(6, 8, 5), "minecraft:stone", "at alias cell local (6, 8, 5)");
        assert_eq!(column.block_state(6, 9, 5), "minecraft:stone", "at alias cell local (6, 9, 5)");
    }

    /// #472, misread direction: the *write* at the end of `tick_grass_block`
    /// always used the local `tlz` — only the probe read was wrong — so the
    /// bug converts a block that is not dirt, at the correct coordinate,
    /// having consulted a cell three y-levels up. Here local `(6, 5, 5)` is
    /// stone (a correct probe rejects it) while the alias cells hold
    /// dirt-under-air (a buggy probe accepts). Under the bug a release build
    /// finds grass at a coordinate that was stone; a debug build panics.
    ///
    /// The detector for this absence assertion is
    /// `grass_spreads_at_a_chunk_whose_local_and_absolute_z_differ`: same
    /// chunk, same scheduler seeds, same tick budget, and it does observe a
    /// spread — so "no spread here" is a discrimination, not a dead loop.
    /// The offset needed to reach local `(6, 8, 5)` legitimately is
    /// `dy = +3`, outside `grass_random_tick`'s `next_int(5) - 3` range of
    /// `-3..=+1`, so a correct probe can never reach those cells at all.
    #[test]
    fn an_absolute_z_misread_would_convert_a_non_dirt_block_at_the_correct_coordinate() {
        const CX: i32 = 2;
        const CZ: i32 = 3;

        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK); // source, air above
        column.set_block(6, 5, 5, "minecraft:stone"); // NOT a legal target
        column.set_block(6, 8, 5, DIRT_BLOCK); // alias cell: dirt, air above

        let mut scheduler = RandomTickScheduler::new(2, 2);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, CX, CZ, 200, &mut block_ticks, 0);
            assert!(
                !events.iter().any(|e| base_name(&e.to) == GRASS_BLOCK),
                "no grass conversion is legal here, but one landed at {:?} (#472: the probe read \
                 the absolute-z alias local (6, 8, 5) and the write used the correct local (6, 5, 5))",
                events.iter().find(|e| base_name(&e.to) == GRASS_BLOCK).map(|e| e.pos),
            );
        }
        assert_eq!(column.block_state(6, 5, 5), "minecraft:stone", "at local (6, 5, 5) in chunk (2, 3)");
        assert_eq!(column.block_state(6, 8, 5), DIRT_BLOCK, "at alias cell local (6, 8, 5)");
    }

    // # Issue #319 end-to-end: redstone-openable blocks through
    // `propagate_and_react` — the production reaction dispatch (the same call
    // site `tick::run_tick_loop` uses after a scheduled redstone flip or a
    // random-tick mutation). The pure per-family decisions live in
    // `crate::redstone_openable`'s own test module; these tests are about the
    // WIRING — that a neighbour notification reaches an adjacent door/trapdoor
    // and that a door's two halves flip together.

    /// The trigger shape used throughout: a lit redstone torch adjacent to the
    /// openable block, "flipped" in place (as `tick.rs` writes a torch's new
    /// state before re-propagating), then `propagate_and_react` fanned out
    /// from the torch's own position — the exact entry a torch's scheduled-tick
    /// flip uses.
    fn flip_torch_and_propagate(
        column: &mut ChunkColumn,
        torch_pos: (i32, i32, i32),
        lit: bool,
    ) -> Vec<RandomTickEvent> {
        let (tx, ty, tz) = torch_pos;
        column.set_block(tx - 0, ty, tz, &format!("minecraft:redstone_torch[lit={lit}]"));
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        propagate_and_react(column, 0, 0, tx, ty, tz, &mut block_ticks, 0)
    }

    /// A two-high door, one half adjacent to a lit torch: both halves must
    /// open when the torch is lit, and both must close when it goes out —
    /// through the real `propagate_and_react` dispatch, not the pure
    /// functions.
    #[test]
    fn a_powered_door_opens_and_closes_both_halves_through_the_reaction_dispatch() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, "minecraft:oak_door[half=lower,open=false,powered=false]");
        column.set_block(3, 6, 3, "minecraft:oak_door[half=upper,open=false,powered=false]");
        let torch = (2, 5, 3); // west of the bottom half

        // Power on: the torch fan-out notifies the door; both halves flip.
        let events = flip_torch_and_propagate(&mut column, torch, true);
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_door[half=lower,open=true,powered=true]",
            "the bottom half must open when powered"
        );
        assert_eq!(
            column.block_state(3, 6, 3),
            "minecraft:oak_door[half=upper,open=true,powered=true]",
            "the top half must open together with the bottom half"
        );
        let flipped: Vec<(i32, i32, i32)> = events.iter().map(|e| e.pos).collect();
        assert!(
            flipped.contains(&(3, 5, 3)) && flipped.contains(&(3, 6, 3)),
            "both half flips must be reported for the client: {flipped:?}"
        );

        // Power off: the same fan-out closes both halves.
        let events = flip_torch_and_propagate(&mut column, torch, false);
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_door[half=lower,open=false,powered=false]"
        );
        assert_eq!(
            column.block_state(3, 6, 3),
            "minecraft:oak_door[half=upper,open=false,powered=false]"
        );
        let flipped: Vec<(i32, i32, i32)> = events.iter().map(|e| e.pos).collect();
        assert!(
            flipped.contains(&(3, 5, 3)) && flipped.contains(&(3, 6, 3)),
            "both half closures must be reported for the client: {flipped:?}"
        );
    }

    /// The two-high power check, end to end in the other direction: a source
    /// adjacent to the TOP half must open the door — and the BOTTOM half must
    /// follow.
    #[test]
    fn a_door_opens_from_power_at_the_top_half_too() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, "minecraft:oak_door[half=lower,open=false,powered=false]");
        column.set_block(3, 6, 3, "minecraft:oak_door[half=upper,open=false,powered=false]");
        let torch = (2, 6, 3); // west of the TOP half

        flip_torch_and_propagate(&mut column, torch, true);
        assert_eq!(
            column.block_state(3, 6, 3),
            "minecraft:oak_door[half=upper,open=true,powered=true]",
            "the notified top half must open"
        );
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_door[half=lower,open=true,powered=true]",
            "the bottom half must follow a signal at the top half"
        );
    }

    /// A single-block family: a trapdoor opens when its adjacent torch lights
    /// and closes when it goes out, and the door half-sync does not fire (the
    /// event list is exactly the trapdoor's own flip, no spurious second
    /// event).
    #[test]
    fn a_powered_trapdoor_opens_and_closes_with_exactly_one_event() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, "minecraft:oak_trapdoor[half=bottom,open=false,powered=false]");
        let torch = (2, 5, 3);

        let events = flip_torch_and_propagate(&mut column, torch, true);
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_trapdoor[half=bottom,open=true,powered=true]"
        );
        assert_eq!(events.len(), 1, "a trapdoor is one block — exactly one flip event");
        assert_eq!(events[0].pos, (3, 5, 3));

        let events = flip_torch_and_propagate(&mut column, torch, false);
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_trapdoor[half=bottom,open=false,powered=false]"
        );
        assert_eq!(events.len(), 1);
    }

    /// Negative control: an UNLIT torch adjacent to the trapdoor is not a
    /// signal, so the trapdoor stays closed even though it IS notified —
    /// proving the `signal != powered` gate discriminates in the wiring, not
    /// just in the pure decision.
    #[test]
    fn an_unpowered_trapdoor_does_not_flip_even_when_notified() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, "minecraft:oak_trapdoor[half=bottom,open=false,powered=false]");
        let torch = (2, 5, 3);

        let events = flip_torch_and_propagate(&mut column, torch, false);
        assert!(events.is_empty(), "an unlit torch must produce no reaction events");
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_trapdoor[half=bottom,open=false,powered=false]",
            "the trapdoor must stay closed"
        );
    }

    /// A fence gate follows the same shape as the trapdoor — opens when
    /// powered, closes when not, one event.
    #[test]
    fn a_powered_fence_gate_opens_and_closes() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(3, 5, 3, "minecraft:oak_fence_gate[open=false,powered=false]");
        let torch = (2, 5, 3);

        flip_torch_and_propagate(&mut column, torch, true);
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_fence_gate[open=true,powered=true]"
        );
        flip_torch_and_propagate(&mut column, torch, false);
        assert_eq!(
            column.block_state(3, 5, 3),
            "minecraft:oak_fence_gate[open=false,powered=false]"
        );
    }

    // ---- Issue #507: the counter is O(1), proven as a count ---------------

    /// **U3-b, the O(1) claim as a count.** The section *decision* evaluates
    /// [`is_randomly_ticking`] zero times, so `tick_chunk`'s per-tick predicate
    /// count depends on `tick_speed` and on how many sections tick — never on
    /// the column's height or block count.
    ///
    /// Both hypotheses are computed from outside the code under test, so this is
    /// a prediction rather than a sign check (`DESIGN.md` §12.43's *magnitude*
    /// species). Per `tick_chunk` call, a correct counter implementation
    /// evaluates the predicate exactly:
    ///
    /// * `tick_speed` times — vanilla's own per-picked-position
    ///   `blockState.isRandomlyTicking()` (`ServerLevel.java:513`), one per
    ///   position draw in the one randomly-ticking section. This term is
    ///   *supposed* to be there; it is bounded by `tick_speed`, not by cells.
    /// * plus `palette.len()` **in debug builds only**, for the definitional
    ///   scan the permanent tripwire runs as its reference arm.
    ///
    /// The competing hypothesis — the pre-`bdf93a28` per-block string scan —
    /// evaluates the predicate up to 4096 times per non-ticking section per
    /// tick, so it predicts ~200k for the short column below and ~2.4M for the
    /// tall one. The **12× column-height ratio is the discriminator**: the two
    /// arms carry byte-identical content in one section and differ only in how
    /// many empty sections sit above it, so any implementation whose decision
    /// touches cells must report different counts for them.
    ///
    /// **What this gate deliberately cannot separate:** the interim palette mask
    /// (`bdf93a28`) also evaluated the predicate `palette.len()` times per tick
    /// and no more, so a predicate count cannot tell it from counters-plus-debug-
    /// tripwire. What separated them was *index-grid reads*, and that proof is
    /// structural rather than measured:
    /// [`section_has_randomly_ticking_block`] and
    /// [`randomly_ticking_palette_mask`] are `#[cfg(any(test, debug_assertions))]`,
    /// so they **do not exist** in a release build and the shipped decision
    /// provably reads no cell of the index grid.
    #[test]
    fn per_tick_predicate_count_is_independent_of_column_height() {
        /// A stage-1 sapling: randomly ticking (`is_sapling`), and its handler
        /// is a named no-op at stage 1 (`SaplingOutcome::TreeGrowthNotModeled`),
        /// so nothing this gate ticks ever mutates a block. That keeps the
        /// palette at a fixed 2 entries and makes the per-tick count exact
        /// rather than "exact plus however many states the run happened to
        /// intern".
        const INERT_TICKING_STATE: &str = "minecraft:oak_sapling[stage=1]";
        const TICKS: u64 = 25;
        const TICK_SPEED: u32 = 7;

        fn measure(height: i32) -> (u64, usize, usize) {
            let mut column = ChunkColumn::new(0, height);
            column.set_block(3, 5, 3, INERT_TICKING_STATE);
            // World-species preconditions, failing rather than skipping. Without
            // a ticking section the whole-column early exit fires and this gate
            // measures nothing; without a non-ticking section the height arm
            // below has no empty sections to be independent of.
            assert!(
                column.has_randomly_ticking_block(),
                "fixture at height {height} holds no randomly-ticking block"
            );
            let sections = column.section_ticking_counts().len();
            let ticking = column
                .section_ticking_counts()
                .iter()
                .filter(|&&c| c > 0)
                .count();
            assert_eq!(ticking, 1, "fixture at height {height} must tick exactly one section");
            assert!(
                sections > ticking,
                "fixture at height {height} has no non-ticking section, so a scan-based \
                 implementation would cost the same as a counter-based one here"
            );

            let mut scheduler = RandomTickScheduler::new(31, 31);
            let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let before = predicate_calls::get();
            for _ in 0..TICKS {
                scheduler.tick_chunk(&mut column, 0, 0, TICK_SPEED, &mut block_ticks, 0);
            }
            let during = predicate_calls::get() - before;
            // Nothing mutated, so the palette never grew: the count is exact.
            assert_eq!(column.raw_palette().len(), 2);
            (during, sections, ticking)
        }

        // Two sections vs twenty-four, same content in section 0.
        let (short_count, short_sections, _) = measure(32);
        let (tall_count, tall_sections, _) = measure(384);
        assert_eq!((short_sections, tall_sections), (2, 24));

        // The debug tripwire's reference scan classifies the palette once per
        // `tick_chunk` call. Derived from the same `cfg` the tripwire itself
        // uses, not restated as a literal.
        let tripwire_per_tick = if cfg!(debug_assertions) { 2u64 } else { 0 };
        let expected = TICKS * (u64::from(TICK_SPEED) + tripwire_per_tick);

        assert_eq!(
            short_count, expected,
            "2-section column: expected exactly {expected} predicate evaluations over {TICKS} \
             ticks at tick_speed {TICK_SPEED} ({TICK_SPEED} position checks + \
             {tripwire_per_tick} tripwire per tick)"
        );
        assert_eq!(
            tall_count, expected,
            "24-section column: expected the SAME {expected} evaluations as the 2-section \
             column — the decision must not touch cells. A per-block scan predicts ~12x more \
             here than there"
        );
    }

    /// **U3-b's other half, and U3-c's instrument control.** Building a column
    /// evaluates the predicate exactly `palette.len()` times — one per palette
    /// entry, once, ever.
    ///
    /// This is also what proves the instrument in the gate above is not simply
    /// broken: a counter that never increments would report two vacuous zeros.
    /// Here it must report a specific non-zero number, predicted from the
    /// palette the constructor adopts.
    #[test]
    fn constructing_a_column_evaluates_the_predicate_once_per_palette_entry() {
        // Control first: the instrument really does count a bare call.
        let before_bare = predicate_calls::get();
        let _ = is_randomly_ticking(GRASS_BLOCK);
        assert_eq!(
            predicate_calls::get() - before_bare,
            1,
            "instrument control failed: a single `is_randomly_ticking` call must register as 1, \
             otherwise the zero this gate's sibling reports means nothing"
        );

        // `ChunkColumn::new` is the all-air constructor: palette of exactly 1.
        let before_new = predicate_calls::get();
        let column = ChunkColumn::new(0, 32);
        assert_eq!(
            predicate_calls::get() - before_new,
            column.raw_palette().len() as u64,
            "the all-air constructor must classify exactly its one palette entry"
        );

        // The real generator column: `from_generated` + `recalc_ticking_counts`,
        // the production transport (`OverworldChunkSource::column`), not a
        // hand-rolled source.
        let source = crate::overworld_chunk_source(2026);
        let before_gen = predicate_calls::get();
        let generated = crate::chunk::ChunkSource::column(&source, 0, 0);
        let generated_calls = predicate_calls::get() - before_gen;
        assert_eq!(
            generated_calls,
            generated.raw_palette().len() as u64,
            "a generated column must classify each of its {} palette entries exactly once — \
             any multiple of that means the classification is being redone",
            generated.raw_palette().len()
        );
        assert!(
            generated_calls > 1,
            "a real generator column with a single-entry palette cannot exercise this gate \
             (got {generated_calls} evaluations)"
        );
    }

    /// The counters' decision must equal the definitional index scan
    /// ([`section_has_randomly_ticking_block`]) for every section, at every step
    /// of a mutation sequence — the same invariant `tick_chunk`'s debug tripwire
    /// asserts, pinned here as a test so it is visible in the crate's own suite
    /// and so the definition stays live in `--release` test builds too.
    ///
    /// The broad parity gate, over real generator columns and an NBT round trip,
    /// is `tests/random_tick_section_counters.rs`; this is the in-module version
    /// that keeps the reference scan honest.
    #[test]
    fn counter_decision_equals_the_definitional_scan_through_a_mutation_sequence() {
        let mut column = ChunkColumn::new(-16, 48);
        let script: [(i32, i32, i32, &str); 8] = [
            (0, -16, 0, GRASS_BLOCK),            // bottom section, 0 -> 1
            (1, -16, 1, GRASS_BLOCK),            // same section, 1 -> 2
            (0, -16, 0, DIRT_BLOCK),             // 2 -> 1
            (1, -16, 1, DIRT_BLOCK),             // 1 -> 0
            (2, 20, 2, "minecraft:wheat[age=3]"), // middle section, 0 -> 1
            (2, 20, 2, "minecraft:wheat[age=4]"), // ticking -> ticking, unchanged
            (3, 31, 3, GRASS_BLOCK),             // top section, 0 -> 1
            (3, 31, 3, "minecraft:stone"),       // 1 -> 0
        ];
        let mut saw_ticking = false;
        for (i, (x, y, z, state)) in script.iter().enumerate() {
            column.set_block(*x, *y, *z, state);
            let mask = randomly_ticking_palette_mask(&column);
            let mut section_min_y = column.min_y;
            while section_min_y < column.min_y + column.height {
                let expected = section_has_randomly_ticking_block(&column, section_min_y, &mask);
                saw_ticking |= expected;
                assert_eq!(
                    column.section_is_randomly_ticking(section_min_y),
                    expected,
                    "step {i} ({state} at ({x}, {y}, {z})): counter and definitional scan \
                     disagree for section_min_y {section_min_y}"
                );
                section_min_y += 16;
            }
        }
        assert!(
            saw_ticking,
            "no step of this script ever produced a ticking section — the comparison above \
             was `false == false` throughout and proved nothing"
        );
    }
}
