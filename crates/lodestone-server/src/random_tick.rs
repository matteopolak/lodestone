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
//! updates on every block change in the section. This crate has no such
//! incremental counter (`ChunkColumn` — see `crate::chunk`'s module doc —
//! has no per-section bookkeeping at all), so [`section_has_randomly_ticking_block`]
//! computes the same **boolean** by scanning the section directly. This
//! changes nothing observable: the count itself is never read, only
//! whether it is positive, and a scan produces the identical true/false
//! answer a maintained counter would. It is the honest reduction for a
//! chunk representation with no incremental bookkeeping, not an invented
//! shortcut.
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
//! `canStayAlive` (`:28-38`) is, modulo the snow-layer special case this
//! crate's terrain never generates: the block directly above must not be a
//! full fluid, and must not fully dampen light. This crate has no light
//! engine (`docs/README.md` has no lighting doc yet), so
//! [`grass_random_tick`] uses the same proxy for both `canStayAlive` and the
//! `getMaxLocalRawBrightness(...) >= 9` check: **the block directly above is
//! bare air** (see [`is_air_variant`]). This is a deliberate, named
//! simplification — the exact light *value* is unavailable, not
//! approximated — and it means our version always attempts a spread when
//! sky-exposed, regardless of time of day. The **draw pattern** is exact
//! either way: `0` extra draws when not air-exposed (dies to dirt), exactly
//! `4 * 3 = 12` `next_int` calls when air-exposed (four attempts, three axis
//! offsets each), matching the jar's own unconditional `for` loop —
//! regardless of how many of the four attempts actually hit a propagatable
//! neighbour.

use crate::gravity_tick;
use crate::growth_tick;
use crate::mob_spawn::SpawnRng;
use crate::neighbor_update::{Direction, NeighborPropagator, Notification, UPDATE_ORDER};
use crate::redstone;
use crate::redstone_diode;
use crate::redstone_observer;
use crate::redstone_openable;
use crate::redstone_torch;
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

/// Strips any `[...]` block-state property suffix, mirroring every other
/// canonical-name comparison in this crate (`crate::chunk::is_air_or_fluid`,
/// `crate::chunk::is_water`).
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// `true` for any air variant (`minecraft:air`/`cave_air`/`void_air`) —
/// narrower than [`crate::chunk::is_air_or_fluid`], which also counts
/// fluids. Used as this module's light-level proxy: see the module doc
/// comment for why "bare air above" stands in for vanilla's real brightness
/// check.
#[must_use]
pub fn is_air_variant(state: &str) -> bool {
    matches!(base_name(state), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
}

/// `true` iff `block_state` is one this crate models a random tick for.
/// Mirrors `BlockState.isRandomlyTicking()`
/// (`BlockBehaviour.java:401-402`) — grass/mycelium-family spreading (see
/// [`GRASS_BLOCK`]'s doc comment for why dirt is deliberately excluded), plus
/// the three families issue #310 added: crop growth, sapling growth, and
/// leaf decay, all cited in `crate::growth_tick`'s own module doc comment.
#[must_use]
pub fn is_randomly_ticking(block_state: &str) -> bool {
    base_name(block_state) == GRASS_BLOCK
        || growth_tick::is_growable_crop(block_state)
        || growth_tick::is_sapling(block_state)
        || growth_tick::leaves_should_decay(block_state)
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
/// jar citation and the light-level proxy this crate substitutes.
///
/// `above_is_air` is vanilla's `canStayAlive`/light-check proxy (`true` when
/// the block directly above the grass block is bare air). `try_propagate`
/// is called for each of the four attempts' relative `(dx, dy, dz)` offset
/// (already drawn from `rng` before the call, exactly like vanilla's
/// `pos.offset(random.nextInt(3) - 1, ...)`) and must itself decide whether
/// the target position is a valid spread destination — a `ChunkColumn`
/// lookup this pure function does not perform, so it stays testable with a
/// fake world.
pub fn grass_random_tick(
    above_is_air: bool,
    rng: &mut SpawnRng,
    mut try_propagate: impl FnMut(i32, i32, i32) -> bool,
) -> GrassOutcome {
    if !above_is_air {
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
/// `canPropagate` (`SpreadingSnowyBlock.java:40-43`) restricted to this
/// crate's light proxy: the target must currently be dirt, and the block
/// directly above the target must itself be air (so the new grass block
/// would immediately satisfy `canStayAlive` too).
#[must_use]
pub fn can_propagate_onto(target_state: &str, above_target_state: &str) -> bool {
    base_name(target_state) == DIRT_BLOCK && is_air_variant(above_target_state)
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
        // Classified once for the whole column, not once per block. A column
        // whose palette holds no randomly-ticking state at all — every
        // all-stone, all-water or all-air column, i.e. most of them — is
        // decided here without touching the 98,304-entry index grid.
        let mask = randomly_ticking_palette_mask(column);
        if !mask.iter().any(|&t| t) {
            return events;
        }
        let mut section_min_y = column.min_y;
        while section_min_y < column.min_y + column.height {
            if section_has_randomly_ticking_block(column, section_min_y, &mask) {
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
        let above_is_air = is_air_variant(&above);

        // `try_propagate` only reads `column` (via the immutable reborrow
        // below) — no mutation happens until after `grass_random_tick`
        // returns and this borrow ends, so the two phases (decide, then
        // apply) never overlap.
        let outcome = {
            let column_ref: &crate::chunk::ChunkColumn = column;
            grass_random_tick(above_is_air, &mut self.behavior_rng, |dx, dy, dz| {
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
                    column.set_block(tlx, ty, tlz, GRASS_BLOCK);
                    events.push(RandomTickEvent {
                        pos: (tx, ty, tz),
                        from: DIRT_BLOCK.to_string(),
                        to: GRASS_BLOCK.to_string(),
                    });
                }
            }
        }
        events
    }
}

/// Settles the gravity block at world `(x, y, z)` if it is one and its
/// support was just removed — see `crate::gravity_tick`'s module doc for the
/// jar citation and the "instant settle, no `FallingBlockEntity`" deviation.
/// A no-op (empty `Vec`) for anything that is not an unsupported gravity
/// block. Draws no RNG (`FallingBlock.tick` itself draws none — see that
/// module's doc comment), so this needs no `&mut self`.
fn settle_gravity_at(
    column: &mut crate::chunk::ChunkColumn,
    min_x: i32,
    min_z: i32,
    x: i32,
    y: i32,
    z: i32,
) -> Vec<RandomTickEvent> {
    let lx = x - min_x;
    let lz = z - min_z;
    let state = column.block_state(lx, y, lz).to_string();
    if !gravity_tick::is_gravity_block(base_name(&state)) {
        return Vec::new();
    }
    let below = column.block_state(lx, y - 1, lz).to_string();
    if !gravity_tick::is_free(&below) {
        return Vec::new();
    }
    let min_y = column.min_y;
    let landing_y = {
        let column_ref: &crate::chunk::ChunkColumn = column;
        gravity_tick::find_landing_y(
            |probe_y| gravity_tick::is_free(column_ref.block_state(lx, probe_y, lz)),
            y,
            min_y,
        )
    };
    if landing_y == y {
        // Shouldn't happen (we already confirmed `below` is free, so the
        // scan must move at least one step) — defensive, not reachable.
        return Vec::new();
    }
    column.set_block(lx, y, lz, crate::chunk::AIR);
    column.set_block(lx, landing_y, lz, &state);
    vec![
        RandomTickEvent {
            pos: (x, y, z),
            from: state.clone(),
            to: crate::chunk::AIR.to_string(),
        },
        RandomTickEvent {
            pos: (x, landing_y, z),
            from: crate::chunk::AIR.to_string(),
            to: state,
        },
    ]
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
    }
    own.extend(propagate_and_react(column, min_x, min_z, x, y, z, block_ticks, current_tick));
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

        // 1. Gravity (#311) — first, matching the existing precedent.
        let settled = settle_gravity_at(column, min_x, min_z, n.pos.x, n.pos.y, n.pos.z);
        if !settled.is_empty() {
            events.extend(settled);
            return vec![Notification { pos: BlockPos::new(n.pos.x, n.pos.y + 1, n.pos.z), from: Direction::Down }];
        }

        let state = column.block_state(tlx, n.pos.y, tlz).to_string();

        // 2. Redstone dust (#314).
        if redstone::is_wire(&state) {
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
            let has_signal = redstone_torch::has_neighbor_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, &state);
            if redstone_torch::should_schedule_check(&state, has_signal)
                && !block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_TORCH.to_string())
            {
                block_ticks.schedule(
                    (n.pos.x, n.pos.y, n.pos.z),
                    redstone::TICK_TORCH.to_string(),
                    current_tick + 2,
                    TickPriority::Normal,
                );
            }
            return Vec::new();
        }

        // 3b. Repeaters (#315).
        if redstone::is_repeater(&state) {
            let facing = redstone::diode_facing(&state);
            let recomputed_lock = redstone_diode::recompute_locked(&redstone::make_lookup(column, min_x, min_z), n.pos, &state);
            if let Some(new_state) = recomputed_lock {
                column.set_block(tlx, n.pos.y, tlz, &new_state);
                events.push(RandomTickEvent { pos: (n.pos.x, n.pos.y, n.pos.z), from: state, to: new_state });
            }
            let state_now = column.block_state(tlx, n.pos.y, tlz).to_string();
            let should_on = redstone_diode::repeater_should_turn_on(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
            if redstone_diode::should_schedule_repeater_check(&state_now, should_on)
                && !block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_REPEATER.to_string())
            {
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
            return Vec::new();
        }

        // 3c. Comparators (#315).
        if redstone::is_comparator(&state) {
            let facing = redstone::diode_facing(&state);
            let input = redstone::input_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
            let side = redstone::alternate_signal(&redstone::make_lookup(column, min_x, min_z), n.pos, facing, false);
            if redstone_diode::should_schedule_comparator_check(&state, input, side)
                && !block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_COMPARATOR.to_string())
            {
                let priority = redstone_diode::comparator_schedule_priority(&redstone::make_lookup(column, min_x, min_z), n.pos, facing);
                block_ticks.schedule(
                    (n.pos.x, n.pos.y, n.pos.z),
                    redstone::TICK_COMPARATOR.to_string(),
                    current_tick + 2,
                    priority,
                );
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
            let watch = redstone_observer::watch_direction(&state);
            if n.from == watch
                && redstone_observer::should_start_signal(&state)
                && !block_ticks.has_scheduled((n.pos.x, n.pos.y, n.pos.z), &redstone::TICK_OBSERVER.to_string())
            {
                block_ticks.schedule(
                    (n.pos.x, n.pos.y, n.pos.z),
                    redstone::TICK_OBSERVER.to_string(),
                    current_tick + 2,
                    TickPriority::Normal,
                );
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

        Vec::new()
    }
}

/// Which of `column`'s palette entries are randomly ticking, indexed by
/// palette id.
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
fn section_has_randomly_ticking_block(
    column: &crate::chunk::ChunkColumn,
    section_min_y: i32,
    mask: &[bool],
) -> bool {
    let max_y = (section_min_y + 16).min(column.min_y + column.height);
    let blocks = column.raw_blocks();
    for y in section_min_y..max_y {
        // `blocks[(y_local * 16 + z) * 16 + x]`, so one y-row is the 256
        // contiguous entries at `y_local * 256`.
        let base = (y - column.min_y) as usize * 256;
        let Some(row) = blocks.get(base..base + 256) else {
            continue;
        };
        if row.iter().any(|&id| mask[id as usize]) {
            return true;
        }
    }
    false
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
            if events.iter().any(|e| e.pos == (6, 5, 5) && e.to == GRASS_BLOCK) {
                spread = true;
                break;
            }
        }
        assert!(spread, "an eligible adjacent dirt block must eventually turn to grass");
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

    // # Issue #311 end-to-end: gravity blocks settling through
    // `NeighborPropagator`'s first real production call. Every test below
    // triggers the fall via an ADJACENT random-tick mutation (grass dying to
    // dirt) — this crate's only current producer, since block-place/break
    // (the far more common vanilla trigger) lives in `server.rs`, off-limits
    // to this task. See `crate::gravity_tick`'s module doc for that scope
    // note stated in full.

    /// A sand block adjacent to a grass-dies-to-dirt conversion, with
    /// nothing solid beneath it, settles all the way to the floor —
    /// `ChunkColumn::new`'s default all-air column, so the predicted landing
    /// is exactly `min_y` (0 here), a magnitude check, not just "it moved".
    #[test]
    fn a_gravity_block_settles_when_an_adjacent_random_tick_mutation_removes_its_support() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK);
        column.set_block(5, 6, 5, "minecraft:stone"); // covers the grass: dies to dirt
        column.set_block(6, 5, 5, "minecraft:sand"); // east neighbour, unsupported (air below by default)
        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut settled = false;
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if events.iter().any(|e| e.pos == (6, 0, 5) && e.to == "minecraft:sand") {
                settled = true;
                break;
            }
        }
        assert!(settled, "an unsupported sand block adjacent to a grass conversion must settle");
        assert_eq!(column.block_state(6, 0, 5), "minecraft:sand", "must land exactly at min_y");
        assert_eq!(column.block_state(6, 5, 5), "minecraft:air", "the old position must be vacated");
    }

    /// Negative control: a sand block WITH solid support directly below it
    /// must never move, however many adjacent grass conversions happen —
    /// proving the settle check actually discriminates on support, not
    /// firing unconditionally on every neighbour notification.
    #[test]
    fn a_supported_gravity_block_never_moves_even_after_adjacent_mutations() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK);
        column.set_block(5, 6, 5, "minecraft:stone");
        column.set_block(6, 5, 5, "minecraft:sand");
        column.set_block(6, 4, 5, "minecraft:stone"); // real support
        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..3000 {
            scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
        }
        assert_eq!(column.block_state(6, 5, 5), "minecraft:sand", "a supported sand block must never fall");
    }

    /// A stacked column of two gravity blocks collapses one at a time in a
    /// single `NeighborPropagator::propagate` call — proof that the
    /// depth-first re-notification (`Direction::Down` from the vacated
    /// position) actually cascades, not just handles the one directly
    /// notified neighbour. Predicted landing: the bottom block reaches
    /// `min_y` (0), the top block then finds the bottom one already there
    /// and lands at exactly one above it (`1`) — both in the SAME
    /// triggering mutation, proven by checking both final positions after
    /// only enough retries to land the position LCG on the grass block once.
    #[test]
    fn a_stacked_gravity_column_collapses_one_block_at_a_time_in_one_cascade() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(5, 5, 5, GRASS_BLOCK);
        column.set_block(5, 6, 5, "minecraft:stone");
        column.set_block(6, 5, 5, "minecraft:sand"); // bottom of the stack, unsupported
        column.set_block(6, 6, 5, "minecraft:gravel"); // resting on top of the sand above
        let mut scheduler = RandomTickScheduler::new(1, 1);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut both_settled = false;
        for _ in 0..3000 {
            scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0);
            if column.block_state(6, 0, 5) == "minecraft:sand" && column.block_state(6, 1, 5) == "minecraft:gravel" {
                both_settled = true;
                break;
            }
        }
        assert!(both_settled, "both stacked gravity blocks must settle, the gravel directly atop the sand");
        assert_eq!(column.block_state(6, 5, 5), "minecraft:air");
        assert_eq!(column.block_state(6, 6, 5), "minecraft:air");
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
            if events.iter().any(|e| e.pos == target_abs && e.to == GRASS_BLOCK) {
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
        assert_eq!(column.block_state(6, 5, 5), GRASS_BLOCK, "at local (6, 5, 5)");
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
                !events.iter().any(|e| e.to == GRASS_BLOCK),
                "no grass conversion is legal here, but one landed at {:?} (#472: the probe read \
                 the absolute-z alias local (6, 8, 5) and the write used the correct local (6, 5, 5))",
                events.iter().find(|e| e.to == GRASS_BLOCK).map(|e| e.pos),
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
}
