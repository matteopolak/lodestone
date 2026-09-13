//! The random-tick scheduler: which block positions get picked
//! for a random tick, how many per section per world tick, and the selection
//! loop every per-block-family handler dispatches from — grass turning to
//! dirt (and back), the block modeled here directly, plus crop growth,
//! sapling growth, and leaf decay (`crate::growth_tick`), which
//! [`RandomTickScheduler::tick_chunk`]'s dispatch (see
//! `tick_randomly_ticking_block`) fans out to. Every one of these reaches a
//! real client through the same [`RandomTickEvent`]/`BlockTickFeed` path —
//! see this module's own "what reaches a client" note on
//! [`RandomTickScheduler::tick_chunk`] and `crate::growth_tick`'s module doc
//! for why crop/sapling/leaf blocks specifically have no *natural* producer
//! in this crate's worldgen yet, unlike grass (CLAUDE.md's own "nothing is
//! done until something on screen changes").
//!
//! # Selection, transcribed from the real driver
//!
//! The real per-world chunk driver, transcribed as the rule it implements:
//! read the `random_tick_speed` gamerule once, then for every block-ticking
//! chunk, tick it with that speed.
//!
//! `RANDOM_TICK_SPEED`'s default is `3`
//! — [`DEFAULT_RANDOM_TICK_SPEED`] below.
//!
//! The real per-chunk tick body, transcribed as the rule it implements: for
//! every section, if it is randomly ticking, then `tickSpeed` times, pick a
//! random position inside that section and — if the block state actually
//! picked there is itself randomly-ticking — run its random tick.
//!
//! Two things worth being exact about, because "a wrong number of draws
//! desynchronises everything downstream" (the scheduler's central invariant):
//!
//! 1. **The position pick happens exactly `tickSpeed` times per
//!    randomly-ticking section, unconditionally** — whether or not the
//!    picked block turns out to be eligible. A miss still consumes a
//!    position draw; it just does nothing with it.
//! 2. **The position draw and the block's own behaviour draw from two
//!    different generators.** The real position-pick advances a
//!    level-local 32-bit LCG seeded once at
//!    level creation — **not** the general-purpose random source passed into
//!    the block's own random-tick handler. [`next_random_tick_pos`] is the former;
//!    behaviour draws (e.g. grass's spread attempts, below) use a second,
//!    independent generator ([`RandomTickScheduler`]'s own `behavior_rng`).
//!
//! The real per-section "is randomly ticking" query
//! is `tickingBlockCount > 0`, an incrementally maintained count the real
//! engine
//! updates on every block change in the section. **This crate now keeps the
//! same counter** — `ChunkColumn::section_ticking`, one `u16` per implicit
//! 16-row window, maintained by `ChunkColumn::set_block` and recomputed once
//! per adopted grid by `recalc_ticking_counts` — so
//! [`RandomTickScheduler::tick_chunk`]'s gate is
//! `ChunkColumn::section_is_randomly_ticking`, an integer compare.
//!
//! The counter definition is the correctness reference; the measurement is useful because the
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
//! never arrived (`docs/mesh-fill-rate.md`).
//!
//! The interim fix classified the palette once per tick and scanned palette
//! *indices*, 54× cheaper but still O(blocks) per column per tick. The counter
//! removes the per-tick scan entirely. [`section_has_randomly_ticking_block`]
//! survives as that definition, and the counters are checked against it by a
//! `debug_assert!` inside `tick_chunk` on every debug run.
//!
//! # The block-random-position derivation, transcribed from the real driver
//!
//! The real position pick, transcribed as the rule it implements: advance a
//! level-local 32-bit LCG state by `state = state * 3 + 1013904223`, take the
//! result right-shifted by 2 as `val`, and combine `val`'s low 4 bits, its
//! bits 16 through 16+yMask's width, and its bits 8 through 11 into the
//! x/y/z offsets from the section's origin.
//!
//! [`next_random_tick_pos`] is this, verbatim, using `i32::wrapping_mul`/
//! `wrapping_add` for the deliberate 32-bit overflow the Java `int` LCG
//! relies on.
//!
//! # Grass ↔ dirt, transcribed from the real random-tick handler
//!
//! The real spreading-snowy-block random tick (the class the grass block
//! extends), transcribed as the rule it implements:
//!
//! 1. If the block can no longer stay alive, replace it with its base block
//!    (dirt) and stop — no further draws this tick.
//! 2. Otherwise, if the block above is bright enough (raw light level at
//!    least 9), attempt **four** spreads: each attempt offsets from the
//!    current position by a random `-1..=1` on x, `-3..=1` on y, and
//!    `-1..=1` on z (three draws per attempt, four attempts, twelve draws
//!    total regardless of outcome), and if the block at that offset is the
//!    base block and can be propagated onto, replace it with this block's
//!    own default state carrying the matching snowy value.
//!
//! The real can-stay-alive check is now modelled for real — see
//! [`grass_can_stay_alive`]. It used to be proxied by "the block directly above
//! is bare air", which killed grass under **any** non-air block including
//! `minecraft:short_grass`, so every patch of grass the generator decorated
//! turned to dirt on its first random tick. The proxy existed
//! because there was no per-state light-dampening census; `lodestone_data::light_props`
//! is that census, and the predicate is `dampening(above) < 15` with the
//! snow-layer-1 and full-fluid special cases ahead of it.
//!
//! **One simplification survives, and it is a different one**: the real raw
//! max-local-brightness-at-least-9 gate on the *spread* branch.
//! This driver holds a `ChunkColumn`, not a light map, so the exact brightness
//! is unavailable rather than approximated, and a live grass block always
//! attempts a spread regardless of time of day. It can never make grass *die*
//! wrongly. The **draw pattern** is exact either way: `0` extra draws when
//! the can-stay-alive check is false (dies to dirt), exactly `4 * 3 = 12` `next_int` calls
//! otherwise (four attempts, three axis offsets each), matching the real
//! unconditional `for` loop — regardless of how many of the four attempts
//! actually hit a propagatable neighbour.
//!
//! Note this makes the draw count depend on **which** block is above, not merely
//! on whether one is: grass under short grass now consumes 12 draws where it
//! consumed 0. That is the real behaviour for the same above-block, which is the
//! standard here — self-consistency is not.

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::gravity_tick;
use crate::growth_tick;
use crate::mob_spawn::SpawnRng;
use crate::neighbor_update::{Direction, NeighborPropagator, Notification};
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
use crate::scheduled_tick::{ScheduledTickKind, ScheduledTickQueueAccess, TickPriority};
#[cfg(test)]
use crate::scheduled_tick::ScheduledTickQueue;
use lodestone_model::BlockPos;
use std::cell::RefCell;
use std::collections::HashMap;

mod grass;
mod lava;
mod gravity;
#[path = "random_tick/redstone.rs"]
mod redstone_family;

pub use grass::{can_propagate_onto, grass_random_tick, GrassOutcome};
pub(crate) use gravity::settle_gravity_at;
#[cfg(test)]
pub(crate) use gravity::GravitySettle;
pub use redstone_family::react_at_placement_with_entities;
pub(crate) use redstone_family::{
    propagate_and_react_with_entities_across_chunks,
    react_at_removal, run_tripwire_recheck, RedstoneColumns,
};
#[cfg(test)]
pub(crate) use redstone_family::{propagate_and_react, propagate_and_react_with_entities, react_at_placement, NoNeighbors};

/// The real default for the `random_tick_speed` gamerule. This crate has no gamerule registry yet (see
/// `crate::server`'s own module doc for why `GameRuleChanged` is currently
/// echoed rather than applied) — every caller of
/// [`RandomTickScheduler::tick_chunk`] passes a `tick_speed` explicitly
/// rather than reading this implicitly, but this is the value production
/// code should pass until a real gamerule store exists.
pub const DEFAULT_RANDOM_TICK_SPEED: u32 = 3;

/// The one block this crate models a real random tick for today. Mirrors
/// the real is-randomly-ticking property being set true only on
/// the grass and mycelium spreading-snowy-block subclasses —
/// note plain dirt is **not** in this set: dirt does not tick itself, it is
/// only ever a *target* of a neighbouring grass block's own tick.
pub(super) const GRASS_BLOCK: &str = "minecraft:grass_block";
pub(super) const DIRT_BLOCK: &str = "minecraft:dirt";
pub(super) const MYCELIUM_BLOCK: &str = "minecraft:mycelium";
pub(super) const PODZOL_BLOCK: &str = "minecraft:podzol";

/// `minecraft:lava` — the one **fluid** whose real is-randomly-ticking flag is true.
///
/// The real lava fluid overrides that check to return `true`; water never
/// does. Its own random tick is what sets fire to flammable blocks near lava, and it
/// is therefore the only thing in a generated world that starts a fire at all —
/// see [`RandomTickScheduler::tick_lava`].
pub(super) const LAVA_BLOCK: &str = "minecraft:lava";

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

/// `true` iff `state`'s fluid state is **full** — the real "is full" fluid
/// check, i.e. `amount == 8`.
///
/// Three cases, and the third is the one a `base_name == "water"` test misses:
///
/// * a source liquid, `minecraft:water[level=0]` / `minecraft:lava[level=0]`:
///   the real fluid-state query maps `level` to `amount = 8 - level` for
///   `level < 8`, so only `level=0` is full;
/// * a **falling** liquid, `level=8..=15`: those map to `amount = 8` and are
///   full, which is why the check cannot be `level == 0`;
/// * any state carrying `waterlogged=true` — a waterlogged slab, stair or
///   fence has a full water fluid state even though its *block* is not water.
///   The real can-stay-alive check reads the fluid state, not the block, so
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

/// Vanilla's own grass/mycelium can-stay-alive check,
/// given the canonical state string of the block **directly above** the grass
/// block.
///
/// # The proxy this replaces, and why it was a bug
///
/// The earlier `is_air_variant(above)` proxy for `canStayAlive` meant
/// **any** non-air block above killed the grass. `minecraft:short_grass` is
/// non-air, and vanilla's own vegetation step places short grass on top of grass
/// blocks — so every patch of grass the generator decorated turned to dirt on its
/// first random tick, which is exactly what the owner reported seeing. The
/// generation side was innocent: `feature/top_layer.rs` and
/// `feature/vegetation/` place `grass_block` with `short_grass` above it, as
/// vanilla does.
///
/// The proxy existed because there was no dampening census. There is one now
/// (`lodestone_data::light_props`), so this is the intended
/// predicate:
///
/// 1. Read the block state directly above.
/// 2. If it is snow with exactly one layer, the grass survives unconditionally.
/// 3. Otherwise, if its fluid state is full, the grass dies.
/// 4. Otherwise, compute the light dampening the block above casts down onto
///    this one, and the grass survives iff that dampening is strictly less
///    than 15.
///
/// Note the order: the snow special case wins over the fluid check, and both win
/// over the dampening comparison. The real light-dampening-into query is exactly
/// [`lodestone_data::light_props::dampening`]'s column, and the comparison is
/// `< 15` — strictly, so a full solid (15) kills and everything below it does not.
///
/// # The one branch not modelled
///
/// The real light-dampening-into query returns a hard `16` — killing the grass — when the two
/// states' occlusion *shapes* merge to a fully-occluding face. That path is only
/// reachable when the block above is an occluding block that uses its shape for
/// light occlusion, i.e. an occluding block that is *not* a full
/// cube (stairs, some slabs). This crate has no occlusion-shape census — only
/// collision shapes, which are a different question (glass has a full collision
/// box and occludes no light) — so those states fall through to their `dampening`
/// column instead. **This can only ever make grass survive where the real
/// engine would
/// kill it**, never the reverse, which is the safe direction and is why it is a
/// documented gap rather than a guess. Adding an occlusion-shape census is the
/// prerequisite for closing it.
///
/// An unresolvable state string is treated as air-like (survives), for the same
/// reason: it cannot destroy a block the player is looking at.
#[must_use]
pub fn grass_can_stay_alive(above_state: &str) -> bool {
    // 1. Snow above with exactly one layer — an explicit `true` that
    //    precedes both other checks. A single snow layer is *thin enough to see
    //    through*, so grass under fresh snowfall keeps its `snowy=true` state
    //    instead of dying.
    if base_name(above_state) == "minecraft:snow" && property_of(above_state, "layers") == Some("1")
    {
        return true;
    }
    // 2. A full fluid state above — drowned grass dies. Checked
    //    before dampening because water's own dampening is 1, which would
    //    otherwise pass.
    if has_full_fluid(above_state) {
        return false;
    }
    // 3. The real light-dampening-into query is strictly less than 15.
    match crate::mobs::block_state_id(above_state) {
        Some(id) => lodestone_data::light_props::dampening(id) < 15,
        None => true,
    }
}

/// The real "is snowy setting" check — the block above is tagged as snow,
/// which is **three** blocks in 26.2, given the state directly above.
///
/// Deliberately *not* shared with [`grass_can_stay_alive`]'s own snow branch,
/// which is the narrower "snow with exactly one layer": the two predicates
/// live in the same real class and are different on purpose.
#[must_use]
pub fn is_snowy_setting(above_state: &str) -> bool {
    matches!(
        base_name(above_state),
        "minecraft:snow" | "minecraft:snow_block" | "minecraft:powder_snow"
    )
}

/// `defaultBlockState().setValue(SNOWY, isSnowySetting(above))` for a
/// `SpreadingSnowyBlock` — the write vanilla performs both when grass spreads
/// and when the block above one changes
/// (its own update-shape hook).
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

/// [`SNOWY_FAMILY`] membership as a named predicate, so
/// [`crate::redstone_graph::classify`] can mirror this dispatcher's own
/// `snowy` arm without the family list leaving this module. Takes a **base
/// name**, matching the `SNOWY_FAMILY.contains(&base_name(&state))` guard it
/// reproduces.
#[must_use]
pub(crate) fn is_snowy_family(base: &str) -> bool {
    SNOWY_FAMILY.contains(&base)
}

/// `true` iff `block_state` is one this crate models a random tick for.
/// Mirrors `BlockState.isRandomlyTicking()`
/// (vanilla's own default implementation) — grass/mycelium-family spreading (see
/// [`GRASS_BLOCK`]'s doc comment for why dirt is deliberately excluded), plus
/// the three families added: crop growth, sapling growth, and
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
/// This gate is a claim about an operation *count*, and this repo's
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

/// The position-pick LCG, verbatim from vanilla's own block-random-pos
/// getter — see this module's doc comment for the exact
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
    /// the real compatibility requirement, not merely that
    /// the same blocks changed.
    #[must_use]
    pub fn position_state(&self) -> i32 {
        self.position_state
    }

    /// One chunk's worth of random ticks at `tick_speed` picks per
    /// randomly-ticking 16-block section — mirrors vanilla's own per-chunk tick's
    /// block-ticking loop (this crate does not
    /// model the `iceandsnow`/`tickPrecipitation` loop above it, which is
    /// weather, out of scope here).
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
    /// — every family returns through this same `Vec`, so this
    /// function's caller (`tick::run_tick_loop`) needed zero changes to gain
    /// the new families: it already forwards whatever `tick_chunk` hands
    /// back, generically, one block-state string at a time.
    ///
    /// `block_ticks`/`current_tick` are threaded through to
    /// [`propagate_and_react`] so a mutation
    /// adjacent to a redstone torch/repeater/comparator/observer can
    /// schedule a delayed recheck — see that function's own doc comment.
    /// `tick::run_tick_loop` (the real caller) passes its own persistent
    /// `block_ticks` queue and `game_tick` counter; nothing here owns either.
    pub fn tick_chunk<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        cx: i32,
        cz: i32,
        tick_speed: u32,
        block_ticks: &mut Q,
        current_tick: u64,
        world: &dyn ChunkSource,
    ) -> Vec<RandomTickEvent> {
        let mut events = Vec::new();
        if tick_speed == 0 {
            return events;
        }
        let min_x = cx * 16;
        let min_z = cz * 16;
        // Vanilla's `tickingBlockCount > 0`, now an O(1) integer compare
        // against the counter `ChunkColumn` maintains on every mutation
        // (the counter path). Nothing here reads the index grid at all:
        // the whole-column early exit below is at most 24 compares, and the
        // per-section decision is one.
        //
        // **Fluids are deliberately out of scope, and this is the boundary.**
        // Vanilla's gate is `isRandomlyTickingBlocks() || isRandomlyTickingFluids()`
        // (its own per-section ticking counters), and lava is the one fluid whose
        // `isRandomlyTicking()` is true (its own override
        // of the base fluid's `false`; water never overrides). This crate models
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
                        world,
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
    /// (this module) or crop/sapling/leaves (`crate::growth_tick`). One
    /// dispatch point keeps `tick_chunk`'s own selection loop
    /// ignorant of which families exist, exactly like vanilla's single
    /// `blockState.randomTick(...)` virtual call fanning out to whichever
    /// `Block` subclass is actually at that position.
    #[allow(clippy::too_many_arguments)]
    fn tick_randomly_ticking_block<Q: ScheduledTickQueueAccess<ScheduledTickKind> + ?Sized>(
        &mut self,
        column: &mut crate::chunk::ChunkColumn,
        min_x: i32,
        min_z: i32,
        world: &dyn ChunkSource,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
        block_ticks: &mut Q,
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

        // every mutation above notifies its six neighbours,
        // mirroring vanilla's own `setBlockAndUpdate` (this is
        // `NeighborPropagator`'s first real production call — see
        // `crate::gravity_tick`'s module doc). Two reactions are modeled
        // today: a gravity block settling once its support disappears, and
        // the redstone family recomputing dust power or
        // scheduling a torch/diode/observer recheck.
        let mutated: Vec<(i32, i32, i32)> = events.iter().map(|e| e.pos).collect();
        for (ex, ey, ez) in mutated {
            // A grass, crop, sapling or leaf cell that mutates on the last
            // column of its chunk owes the notification to its east or south
            // neighbour just as much as to the five inside its own footprint
            // — an observer one cell over the seam watches that mutation and
            // must pulse for it. `world` is what makes those two cells
            // reachable; see [`RedstoneColumns`] for the residency boundary
            // that still stops the cascade at the edge of loaded simulation.
            events.extend(propagate_and_react_with_entities_across_chunks(
                column, min_x, min_z, world, ex, ey, ez, block_ticks, current_tick, None,
            ));
        }
        events
    }


    /// Crop growth — see `crate::growth_tick`'s module doc for
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

    /// Sapling growth — see `crate::growth_tick`'s module doc
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

    /// Leaf decay — see `crate::growth_tick`'s module doc for
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
    // Section-indexed rather than y-row-indexed: the grid is packed per
    // section (`crate::chunk_blocks`), and `section_min_y` is a section boundary by
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
    use std::collections::HashSet;
    use std::sync::Mutex;

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
        let events = scheduler.tick_chunk(&mut column, 0, 0, 5, &mut block_ticks, 0, &NoNeighbors);
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
        scheduler.tick_chunk(&mut column, 0, 0, 5, &mut block_ticks, 0, &NoNeighbors);

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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0, &NoNeighbors);
            assert!(
                !events.iter().any(|e| e.to == DIRT_BLOCK),
                "an air-exposed grass block must never die to dirt"
            );
        }
        assert_eq!(column.block_state(3, 5, 3), GRASS_BLOCK);
    }

    /// **Which above-block kills grass, predicted from the documented rules:**
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

    /// **End to end through the real `tick_chunk` driver:**
    /// grass under short grass must survive, and the draw count must be the
    /// *live* one (12 behaviour draws), not the die branch's zero.
    ///
    /// This is a paired assertion on purpose. "It did not die" alone is also
    /// satisfied by the position pick never reaching the block, so the second
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
            assert!(
                !events.iter().any(|e| e.to == DIRT_BLOCK),
                "grass under short grass must never die to dirt"
            );
        }
        assert_eq!(column.block_state(3, 5, 3), GRASS_BLOCK);
        // The tick really ran: with `tick_speed = 200` over 3,000 calls the
        // position pick lands on this cell ~146 times, and each visit costs
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
            if events.iter().any(|e| e.pos == (6, 5, 5) && base_name(&e.to) == GRASS_BLOCK) {
                spread = true;
                break;
            }
        }
        assert!(spread, "an eligible adjacent dirt block must eventually turn to grass");
    }

    /// the two halves of `snowy`. The tag is `#minecraft:snow` — three
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
                all.extend(scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0, &NoNeighbors));
            }
            all
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn default_random_tick_speed_is_three() {
        assert_eq!(DEFAULT_RANDOM_TICK_SPEED, 3);
    }

    // End-to-end: crop growth, sapling growth, leaf decay
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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
    /// future tree-generation support changes this test loudly, rather than
    /// this crate quietly starting to fabricate trees unnoticed.
    #[test]
    fn a_stage_one_sapling_never_produces_an_event() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(2, 5, 2, "minecraft:oak_sapling[stage=1]");
        let mut scheduler = RandomTickScheduler::new(9, 9);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for _ in 0..3000 {
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0, &NoNeighbors);
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
            let events = scheduler.tick_chunk(&mut column, 0, 0, 8, &mut block_ticks, 0, &NoNeighbors);
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
            scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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
                state: lodestone_data::block_states::BlockStateValue::parse("minecraft:sand"),
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
            scheduler.tick_chunk(&mut column, 0, 0, 200, &mut block_ticks, 0, &NoNeighbors);
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

    // Local versus absolute `z` in grass propagation
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

    /// forward direction: an eligible dirt block must still be found
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
            let events = scheduler.tick_chunk(&mut column, CX, CZ, 200, &mut block_ticks, 0, &NoNeighbors);
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
        // `snowy=false` explicitly: the spread write sets the property
        // vanilla's `SpreadingSnowyBlock.randomTick` sets, air being above.
        assert_eq!(column.block_state(6, 5, 5), "minecraft:grass_block[snowy=false]", "at local (6, 5, 5)");
        // Nothing may have been written at the alias cells.
        assert_eq!(column.block_state(6, 8, 5), "minecraft:stone", "at alias cell local (6, 8, 5)");
        assert_eq!(column.block_state(6, 9, 5), "minecraft:stone", "at alias cell local (6, 9, 5)");
    }

    /// misread direction: the *write* at the end of `tick_grass_block`
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
            let events = scheduler.tick_chunk(&mut column, CX, CZ, 200, &mut block_ticks, 0, &NoNeighbors);
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

    // End-to-end: redstone-openable blocks through
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

    // ---- the counter is O(1), proven as a count ---------------

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
    ///   `blockState.isRandomlyTicking()` check, one per
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
                scheduler.tick_chunk(&mut column, 0, 0, TICK_SPEED, &mut block_ticks, 0, &NoNeighbors);
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

    /// The second interrupt (sticky retraction only)
    /// "update-order quirk"): `piston::relative_n(pos, facing, 2)` — a
    /// *different* cell from the arm the first interrupt already covers — is
    /// checked too, and a still-**extending** `moving_piston` entity found
    /// there is finalTicked exactly as the arm's own pending commit is, per
    /// `PistonBaseBlock.triggerEvent`'s `isSticky` branch.
    ///
    /// **The discriminating pair.** Scenario A leaves a plain pushable block at
    /// the two-cell-out position with no pending commit — an ordinary sticky
    /// pull, which must schedule a fresh `moving_piston` commit to grab it.
    /// Scenario B reaches the *identical final cell content* (the interrupt's
    /// own write lands on the same block scenario A started from) but through
    /// a pending commit instead, and must schedule **no** pull at all — per
    /// vanilla's `if (!pistonPiece)` guard, which skips the whole
    /// moveBlocks-or-removeBlock decision once the interrupt fires. If the
    /// interrupt only mutated the cell without suppressing the pull decision,
    /// scenario B would schedule a pull exactly like scenario A and this test
    /// could not tell the two apart.
    #[test]
    fn sticky_retract_interrupts_a_second_pistons_extension_two_cells_out_and_skips_the_pull() {
        let piston_pos = BlockPos::new(5, 5, 5);
        // West of the piston, i.e. *not* the push direction (East) — a valid
        // direct neighbour signal source, mirroring
        // `redstone_piston_order_oracle_gate.rs`'s own `piston_rig()`. The
        // piston never reacts to a notification on its **own**
        // position (`NeighborPropagator::propagate` notifies a centre's
        // neighbours, never the centre itself) — it must be notified via one
        // of its own neighbours, exactly as production always reaches it.
        let torch_pos = BlockPos::new(4, 5, 5);
        let two_pos = crate::piston::relative_n(piston_pos, Direction::East, 2);

        // Scenario A (control): an ordinary sticky pull, nothing pending at
        // `two_pos` — proves the pull mechanism itself fires absent interference.
        {
            let mut column = ChunkColumn::new(0, 16);
            column.set_block(5, 5, 5, "minecraft:sticky_piston[extended=true,facing=east]");
            column.set_block(4, 5, 5, &redstone_torch::set_standing_lit(true));
            column.set_block(7, 5, 5, "minecraft:dirt");
            let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            // Un-light the torch and notify from *it* — the piston loses its
            // extend signal and retracts.
            column.set_block(4, 5, 5, &redstone_torch::set_standing_lit(false));
            let _events = propagate_and_react(
                &mut column, 0, 0, torch_pos.x, torch_pos.y, torch_pos.z, &mut block_ticks, 0,
            );
            let arm_pos = crate::piston::relative_n(piston_pos, Direction::East, 1);
            let pull_at_arm = block_ticks.iter().any(|t| {
                t.pos == (arm_pos.x, arm_pos.y, arm_pos.z)
                    && crate::piston::parse_finish_kind(&t.kind).is_some_and(|e| !e.source)
            });
            assert!(
                pull_at_arm,
                "control failed: an ordinary sticky pull with nothing intervening must \
                 schedule a carried-block (non-source) commit at the arm"
            );
        }

        // Scenario B: the same cell reached through a pending commit instead.
        {
            let mut column = ChunkColumn::new(0, 16);
            column.set_block(5, 5, 5, "minecraft:sticky_piston[extended=true,facing=east]");
            column.set_block(4, 5, 5, &redstone_torch::set_standing_lit(true));
            column.set_block(7, 5, 5, "minecraft:moving_piston[facing=east,type=normal]");
            let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let pending_entity = crate::piston::MovingBlockEntity::new(
                "minecraft:dirt".to_string(),
                Direction::East,
                true,
                false,
            );
            block_ticks.schedule(
                (two_pos.x, two_pos.y, two_pos.z),
                crate::piston::finish_kind(&pending_entity),
                100,
                TickPriority::Normal,
            );

            column.set_block(4, 5, 5, &redstone_torch::set_standing_lit(false));
            let events = propagate_and_react(
                &mut column, 0, 0, torch_pos.x, torch_pos.y, torch_pos.z, &mut block_ticks, 0,
            );

            // The pending commit is gone — interrupted, not left to also fire
            // later against a cell the interrupt already rewrote.
            assert!(
                block_ticks.iter().all(|t| t.pos != (two_pos.x, two_pos.y, two_pos.z)),
                "the interrupted commit must be removed, not merely superseded"
            );
            // The interrupt's write is observable: a non-source entity writes its
            // `moved_state` — exactly the block scenario A started from.
            assert_eq!(
                column.block_state(two_pos.x, two_pos.y, two_pos.z),
                "minecraft:dirt",
                "a non-source interrupted entity must write its moved_state"
            );
            assert!(
                events
                    .iter()
                    .any(|e| e.pos == (two_pos.x, two_pos.y, two_pos.z) && e.to == "minecraft:dirt"),
                "the interrupt's write must be reported as an event"
            );

            // The discriminator: no *carried-block* commit was scheduled at
            // the arm — where `apply_move` lands a successful pull — because
            // vanilla's `!pistonPiece` guard skips the pull decision entirely
            // once the interrupt fired. (The base's own retraction commit at
            // `piston_pos`, `source: true`, is unconditional and expected
            // regardless — see `begin_move`'s own retract arm — so this checks
            // the arm specifically, not "no commit anywhere".)
            let arm_pos = crate::piston::relative_n(piston_pos, Direction::East, 1);
            let pull_at_arm = block_ticks.iter().any(|t| {
                t.pos == (arm_pos.x, arm_pos.y, arm_pos.z)
                    && crate::piston::parse_finish_kind(&t.kind).is_some_and(|e| !e.source)
            });
            assert!(
                !pull_at_arm,
                "an intercepted sticky retraction must not also schedule a pull at the arm \
                 (found one) — scenario A's control above proves this would otherwise happen"
            );
        }
    }

    /// **The tripwire block-removal hook** (`TripWireBlock.affectNeighborsAfterRemoval`),
    /// wired for the first time through [`react_at_removal`].
    ///
    /// Layout: hook@(0,5,0) facing east, real wire cells at x=1 and x=3 (neither
    /// powered), the wire at x=2 already broken to air, and a receiver hook@(4,5,0)
    /// facing west. Every real wire cell is `powered=false` — the discriminating
    /// choice: if the removal hook's own `powered=true` override on the broken
    /// cell had no effect, the controlling hook could only ever read `powered:
    /// false` here, because nothing else in this rig is powered at all.
    #[test]
    fn breaking_a_tripwire_wire_cell_pulses_its_controlling_hook_powered_true() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(0, 5, 0, "minecraft:tripwire_hook[facing=east,attached=true,powered=false]");
        column.set_block(1, 5, 0, "minecraft:tripwire[attached=true,powered=false,disarmed=false]");
        // x=2 is already air by the time the reaction runs — `destroy_block`
        // overwrites the cell before calling `propagate_removal_with_entities`.
        let broken = "minecraft:tripwire[attached=true,powered=false,disarmed=false]".to_string();
        column.set_block(3, 5, 0, "minecraft:tripwire[attached=true,powered=false,disarmed=false]");
        column.set_block(4, 5, 0, "minecraft:tripwire_hook[facing=west,attached=true,powered=false]");

        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = react_at_removal(&mut column, 0, 0, &NoNeighbors, 2, 5, 0, &broken, &mut block_ticks, 0);

        let hook_now = column.block_state(0, 5, 0).to_string();
        assert_eq!(
            hook_now, "minecraft:tripwire_hook[facing=east,attached=true,powered=true]",
            "even though neither surviving wire cell is itself powered, breaking the \
             middle one must pulse the controlling hook powered=true for one instant"
        );
        assert!(
            events.iter().any(|e| e.pos == (0, 5, 0) && e.to.contains("powered=true")),
            "the hook rewrite must be reported as an event, not just written silently \
             into the column: {events:?}"
        );

        // One scan, two endpoints: the receiver hook is rewritten too.
        let receiver_now = column.block_state(4, 5, 0).to_string();
        assert_eq!(
            receiver_now, "minecraft:tripwire_hook[facing=west,attached=true,powered=true]"
        );

        // The pulse is transient: a recheck is scheduled so the hook settles
        // back down once the real (now genuinely gapped) world is re-read.
        assert!(
            block_ticks.has_scheduled(
                (0, 5, 0),
                &redstone_tripwire::TICK_TRIPWIRE_RECHECK.to_string()
            ),
            "the pulse must schedule the periodic recheck that settles it back down"
        );

        // The control: recomputing from the real (post-removal) world with no
        // synthetic override — what a plain rescan would see — finds the x=2
        // gap for real and must NOT report the pulse. This is what proves the
        // `on_wire_removed` override, not something else, produced the result
        // above.
        let lookup = redstone::make_lookup(&column, 0, 0);
        let naive = redstone_tripwire::calculate_state(
            &lookup,
            BlockPos::new(0, 5, 0),
            "minecraft:tripwire_hook[facing=east,attached=true,powered=false]",
            false,
            None,
        );
        assert!(
            !naive.powered && !naive.attached,
            "a rescan with no removal override must see the real gap at x=2 and settle \
             attached=false, powered=false — {naive:?}"
        );
    }

    /// A no-op control: breaking a block that is not a tripwire must produce
    /// no events and schedule nothing, so [`react_at_removal`]'s guard is
    /// proven rather than merely assumed.
    #[test]
    fn breaking_a_non_tripwire_block_is_a_no_op_for_the_removal_hook() {
        let mut column = ChunkColumn::new(0, 16);
        column.set_block(0, 5, 0, "minecraft:tripwire_hook[facing=east,attached=true,powered=false]");
        column.set_block(1, 5, 0, "minecraft:tripwire[attached=true,powered=false,disarmed=false]");
        let broken = "minecraft:stone".to_string();
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = react_at_removal(&mut column, 0, 0, &NoNeighbors, 2, 5, 0, &broken, &mut block_ticks, 0);
        assert!(events.is_empty(), "breaking stone must not touch any tripwire hook: {events:?}");
        assert!(block_ticks.drain_due(u64::MAX, usize::MAX).is_empty());
    }

    // -----------------------------------------------------------------
    // cross-chunk propagation.
    // -----------------------------------------------------------------

    /// A minimal multi-column [`ChunkSource`] for the tests below: an
    /// explicit map of resident columns keyed by chunk coordinate, with
    /// residency exactly what the map contains — no implicit
    /// "assume resident" default (unlike [`ChunkSource`]'s own default),
    /// so a test can insert exactly the neighbours it wants reachable and
    /// leave every other chunk genuinely unloaded.
    struct TestWorld {
        columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
        /// Chunks whose data this world holds but which it reports **not**
        /// resident.
        ///
        /// This is what lets a control keep a fixture's far half seeded —
        /// so an assertion can read it back and say "this cell was not
        /// rewritten" — while the cascade under test is genuinely unable to
        /// reach it. Without the split, "unloaded" and "unseeded" are the
        /// same state and a control cannot tell a truncated cascade from an
        /// empty fixture.
        unloaded: Mutex<HashSet<(i32, i32)>>,
    }

    impl TestWorld {
        fn new() -> Self {
            Self { columns: Mutex::new(HashMap::new()), unloaded: Mutex::new(HashSet::new()) }
        }

        fn insert(&self, cx: i32, cz: i32, column: ChunkColumn) {
            self.columns.lock().expect("test world poisoned").insert((cx, cz), column);
        }

        /// Declares `(cx, cz)` not resident while keeping whatever column
        /// was inserted for it readable through [`Self::block_state`].
        fn unload_but_keep(&self, cx: i32, cz: i32) {
            self.unloaded.lock().expect("test world poisoned").insert((cx, cz));
        }
    }

    impl ChunkSource for TestWorld {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            self.columns
                .lock()
                .expect("test world poisoned")
                .get(&(cx, cz))
                .cloned()
                .unwrap_or_else(|| ChunkColumn::new(0, 16))
        }
        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.columns
                .lock()
                .expect("test world poisoned")
                .get(&(cx, cz))
                .map(|c| c.block_state(x.rem_euclid(16), y, z.rem_euclid(16)).to_string())
                .unwrap_or_else(|| "minecraft:air".to_string())
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:plains".to_string()
        }
        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
            self.columns
                .lock()
                .expect("test world poisoned")
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(0, 16))
                .set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
        }
        // The one override that matters here: residency is exactly "this
        // test inserted a column for that chunk", never the trait's own
        // "assume resident" default — see this type's own doc comment.
        fn is_column_resident(&self, cx: i32, cz: i32) -> bool {
            if self.unloaded.lock().expect("test world poisoned").contains(&(cx, cz)) {
                return false;
            }
            self.columns.lock().expect("test world poisoned").contains_key(&(cx, cz))
        }
    }

    /// **The main proof, with an independent reference.** A
    /// `minecraft:redstone_block` (a constant, always-15 power source no
    /// arm in [`react_to_notification`] ever reclassifies or rewrites) sits
    /// at world x=15 in the home column (chunk (0,0)). A dust cell sits
    /// right across the chunk seam at world x=16, in an already-loaded
    /// neighbour column (chunk (1,0)), holding a stale `power=0`. Notifying
    /// the redstone block's own neighbours must recompute the dust cell's
    /// power from a source it can only see by reaching across the chunk
    /// boundary.
    ///
    /// The predicted value, 15, is **not** derived from this fix's own
    /// code: it is `redstone_wire::calculate_target_strength` — an
    /// already-oracle-tested function the cross-chunk path never touches — run over
    /// the *same two cells placed inside one single column*
    /// (`single_column_reference`, below), the pre-existing,
    /// independently-validated code path this crate has used for wire
    /// power for this subsystem. The only variable between that reference and the
    /// cross-chunk case is whether an arbitrary administrative chunk
    /// boundary happens to fall between the two cells — real Minecraft
    /// redstone has no such concept, so the two numbers must agree if this
    /// fix is correct.
    #[test]
    fn wire_power_reaches_across_a_loaded_chunk_boundary_and_matches_the_single_column_reference() {
        // The independent reference: both cells inside ONE column, at
        // local x=14 (source) and x=15 (dust) — the same relative
        // geometry the cross-chunk case below uses, computed through the
        // ordinary single-column lookup this crate has always used.
        let mut single_column_reference = ChunkColumn::new(0, 16);
        single_column_reference.set_block(14, 5, 8, redstone::REDSTONE_BLOCK);
        single_column_reference.set_block(15, 5, 8, &redstone_wire::set_power(0));
        let expected = redstone_wire::calculate_target_strength(
            &redstone::make_lookup(&single_column_reference, 0, 0),
            BlockPos::new(15, 5, 8),
        );
        assert_eq!(
            expected, 15,
            "sanity: the pre-existing single-column wire evaluator must read a redstone block as strong power 15"
        );

        // The cross-chunk case: the same two relative cells, now split
        // across world x=15 (home, chunk (0,0)) / x=16 (neighbour, chunk
        // (1,0)).
        let mut home = ChunkColumn::new(0, 16);
        home.set_block(15, 5, 8, redstone::REDSTONE_BLOCK);
        let mut neighbor = ChunkColumn::new(0, 16);
        neighbor.set_block(0, 5, 8, &redstone_wire::set_power(0));

        let world = TestWorld::new();
        world.insert(1, 0, neighbor);

        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = propagate_and_react_with_entities_across_chunks(
            &mut home, 0, 0, &world, 15, 5, 8, &mut block_ticks, 0, None,
        );
        let neighbor_event = events
            .iter()
            .find(|e| e.pos == (16, 5, 8))
            .unwrap_or_else(|| panic!("no event at the neighbour cell (16, 5, 8) — cross-chunk reach did not fire: {events:?}"));
        assert_eq!(
            neighbor_event.to,
            redstone_wire::set_power(expected),
            "the neighbour's recomputed power must match the single-column reference exactly"
        );

        // Control, proving this differential can actually fail rather than
        // being trivially satisfied: run the identical scenario through
        // the pre-cross-chunk entry point
        // (`propagate_and_react_with_entities`, home column only, no
        // `ChunkSource`). Home's own fan-out never reaches world x=16 at
        // all — it is outside home's own 16-wide footprint — so the
        // neighbour cell must be neither notified nor rewritten.
        let mut home_again = ChunkColumn::new(0, 16);
        home_again.set_block(15, 5, 8, redstone::REDSTONE_BLOCK);
        let mut block_ticks_control: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let control_events = propagate_and_react_with_entities(
            &mut home_again, 0, 0, 15, 5, 8, &mut block_ticks_control, 0, None,
        );
        assert!(
            !control_events.iter().any(|e| e.pos == (16, 5, 8)),
            "control failed: the single-column entry point must NOT reach the neighbour cell — got {control_events:?}"
        );
    }

    /// The residency boundary, proven directly: the identical setup to the
    /// test above, except the neighbour chunk (1, 0) is never inserted
    /// into [`TestWorld`] — genuinely unloaded, not merely "not the home
    /// column". `propagate_and_react_with_entities_across_chunks` must
    /// truncate exactly as the pre-cross-chunk single-column path always
    /// did: no event past the home column's own edge, and — checked
    /// directly, not merely inferred from the absence of an event —
    /// `world.column`/`world.set_block` must never be called for the
    /// unloaded chunk, so no chunk is ever generated purely because a
    /// cascade grazed its border.
    #[test]
    fn an_unloaded_neighbour_still_truncates_the_cascade() {
        let mut home = ChunkColumn::new(0, 16);
        home.set_block(15, 5, 8, redstone::REDSTONE_BLOCK);

        // Deliberately empty: chunk (1, 0) is never inserted, so
        // `TestWorld::is_column_resident(1, 0)` answers `false` and
        // `TestWorld::column`/`set_block` — which would panic-worthy
        // fabricate an unrelated all-air column if ever reached for a
        // chunk this test did not seed — must never be called for it.
        let world = TestWorld::new();

        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events = propagate_and_react_with_entities_across_chunks(
            &mut home, 0, 0, &world, 15, 5, 8, &mut block_ticks, 0, None,
        );
        assert!(
            !events.iter().any(|e| e.pos == (16, 5, 8)),
            "an unloaded neighbour must not be reachable at all: {events:?}"
        );
        assert!(
            !world.is_column_resident(1, 0),
            "control: chunk (1, 0) must still read as not resident — this is what the cascade is gated on"
        );
    }

    // -----------------------------------------------------------------
    // The three entry points a player action or the world tick reaches
    // redstone through, each driven across a chunk seam.
    //
    // # Where the expected values come from
    //
    // Two outside sources, neither of them this crate:
    //
    // * [`crate::redstone_oracle_gate::ORACLE_DUST_ATTENUATION`] — dust
    //   power by distance from its source, probed cell-by-cell on a **live
    //   26.2 server** with `execute if block <pos>
    //   minecraft:redstone_wire[power=N]`, three independent readings that
    //   agreed exactly. That module carries the full provenance.
    // * **Spatial-boundary invariance.** A 16x16 chunk column is an
    //   administrative unit of storage; redstone has no player-visible
    //   concept of one. So the same relative geometry must produce the same
    //   result whether or not a seam happens to fall inside it, and the
    //   reference reading is the same fixture laid out inside one column.
    //
    // # Why the coordinates are what they are
    //
    // Every fixture here is placed so the three candidate models disagree at
    // the *first* cell past the seam, rather than only in aggregate:
    //
    // | model | dust power at world x=16, 3 cells from the source |
    // |---|---|
    // | seam-invariant (the oracle) | 13 |
    // | cascade truncates at the column edge | 0 |
    // | cascade crosses but restarts at full strength | 15 |
    //
    // A run whose far end sat at power 0 or 15 under the correct model would
    // make two of those three coincide, so the runs below are sized to end
    // on neither.

    const SEAM_FLOOR_Y: i32 = 0;
    const SEAM_ROW_Y: i32 = 1;
    const SEAM_ROW_Z: i32 = 8;

    /// A column with a stone floor across its whole footprint, so no rig
    /// below reads air where a real world would have ground.
    fn seam_column_with_floor() -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block(x, SEAM_FLOOR_Y, z, "minecraft:stone");
            }
        }
        column
    }

    /// The canonical tripwire-hook state string, in
    /// `redstone_tripwire`'s own property order so a seeded hook is
    /// comparable to a computed one byte for byte.
    fn seam_hook(facing: &str, attached: bool, powered: bool) -> String {
        format!("minecraft:tripwire_hook[facing={facing},attached={attached},powered={powered}]")
    }

    const SEAM_TRIPWIRE: &str = "minecraft:tripwire[attached=true,powered=false,disarmed=false]";

    /// **Placement.** `crate::server::propagate_placement_with_entities` is
    /// the fan-out every block a player places or breaks owes its
    /// neighbours — `apply_use_item_on`'s placement arm, its hand-use arm
    /// (a lever or button click) and `destroy_block`'s post-break cascade
    /// all reach it. A redstone block dropped at world x=13 must drive a
    /// 12-long dust run to the live server's attenuation profile even
    /// though the run leaves chunk (0, 0) after two cells.
    ///
    /// The predicted profile is `ORACLE_DUST_ATTENUATION`'s first twelve
    /// entries — `15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4` — asserted per
    /// coordinate rather than as an aggregate, so a failure names the cell.
    /// Ten of those twelve cells lie in the neighbouring column, and the
    /// far end reads 4: not 0, so a truncating model is distinguishable,
    /// and not 15, so a model that re-seeds full strength at the seam is
    /// distinguishable too.
    #[test]
    fn a_placed_source_drives_its_dust_run_across_a_chunk_seam_to_the_live_server_profile() {
        const SOURCE_X: i32 = 13;
        const RUN: i32 = 12;

        let world = TestWorld::new();
        world.insert(0, 0, seam_column_with_floor());
        world.insert(1, 0, seam_column_with_floor());
        for step in 1..=RUN {
            world.set_block(SOURCE_X + step, SEAM_ROW_Y, SEAM_ROW_Z, &redstone_wire::set_power(0));
        }
        // The seam really is inside the run, not beyond it — a fixture that
        // drifted entirely into one column would pass while proving nothing.
        assert_eq!((SOURCE_X + 1).div_euclid(16), 0, "the run must start in chunk (0, 0)");
        assert_eq!((SOURCE_X + RUN).div_euclid(16), 1, "the run must end in chunk (1, 0)");

        // The block is written first and the fan-out follows, which is the
        // order `apply_use_item_on` performs a placement in.
        world.set_block(SOURCE_X, SEAM_ROW_Y, SEAM_ROW_Z, redstone::REDSTONE_BLOCK);
        let (changed, _scheduled) = crate::server::propagate_placement_with_entities(
            &world,
            BlockPos::new(SOURCE_X, SEAM_ROW_Y, SEAM_ROW_Z),
            None,
        );

        let expected: Vec<(i32, u8)> = crate::redstone_oracle_gate::ORACLE_DUST_ATTENUATION
            .iter()
            .copied()
            .take(RUN as usize)
            .collect();
        let measured: Vec<(i32, u8)> = (1..=RUN)
            .map(|step| {
                (
                    step,
                    redstone::wire_power(&world.block_state(SOURCE_X + step, SEAM_ROW_Y, SEAM_ROW_Z)),
                )
            })
            .collect();

        for (&(distance, oracle_power), &(step, measured_power)) in expected.iter().zip(measured.iter()) {
            assert_eq!(distance, step, "rig misalignment: oracle distance {distance} vs run step {step}");
            let x = SOURCE_X + step;
            assert_eq!(
                measured_power, oracle_power,
                "dust at world (x={x}, y={SEAM_ROW_Y}, z={SEAM_ROW_Z}) in chunk ({}, 0), \
                 {distance} cell(s) from the placed source: our model says power={measured_power}, \
                 the live 26.2 server measured power={oracle_power}. full profile: {measured:?}",
                x.div_euclid(16)
            );
        }
        assert_eq!(
            changed.len(),
            RUN as usize,
            "every dust cell in the run must be reported as changed, and nothing else: {changed:?}"
        );

        // The control. The identical fixture, with the neighbouring chunk's
        // data still seeded and readable but declared not resident: the
        // cascade must stop at the home column's own edge, leaving every
        // cell from the seam onward at the `power=0` it was laid with. This
        // is the reading the same assertions produce when the cross-seam
        // reach is absent, so "the profile matched" above is a
        // discrimination rather than something the rig produces regardless.
        let control = TestWorld::new();
        control.insert(0, 0, seam_column_with_floor());
        control.insert(1, 0, seam_column_with_floor());
        for step in 1..=RUN {
            control.set_block(SOURCE_X + step, SEAM_ROW_Y, SEAM_ROW_Z, &redstone_wire::set_power(0));
        }
        control.unload_but_keep(1, 0);
        control.set_block(SOURCE_X, SEAM_ROW_Y, SEAM_ROW_Z, redstone::REDSTONE_BLOCK);
        let _ = crate::server::propagate_placement_with_entities(
            &control,
            BlockPos::new(SOURCE_X, SEAM_ROW_Y, SEAM_ROW_Z),
            None,
        );
        assert!(
            !control.is_column_resident(1, 0),
            "control premise: chunk (1, 0) must read as not resident"
        );
        for step in 1..=RUN {
            let x = SOURCE_X + step;
            let power = redstone::wire_power(&control.block_state(x, SEAM_ROW_Y, SEAM_ROW_Z));
            if x.div_euclid(16) == 0 {
                assert_eq!(
                    power,
                    (16 - step) as u8,
                    "control: dust still inside the home column must attenuate normally, at x={x}"
                );
            } else {
                assert_eq!(
                    power, 0,
                    "control failed: dust at x={x} is past an unloaded seam and must be untouched, \
                     so the assertions above can distinguish a truncated cascade from a reaching one"
                );
            }
        }
    }

    /// **Removal.** `crate::server::propagate_removal_with_entities` is the
    /// hook `destroy_block` calls with the state it just overwrote, and the
    /// only family that reacts to it scans up to
    /// `redstone_tripwire::WIRE_DIST_MAX - 1` = 41 cells for the hook that
    /// controls the broken string. Forty-one cells is two and a half chunk
    /// columns, so a scan bounded to one column cannot find the controlling
    /// hook of most legal runs at all.
    ///
    /// The predicted values are the two hook state strings and the recheck
    /// schedule, and the reference for them is the same fixture short
    /// enough to fit inside one column: breaking a cell of a taut, armed
    /// run must leave **both** endpoints
    /// `attached=true, powered=true` — the instantaneous pulse a snapped
    /// string fires — and schedule one recheck at the scanning hook,
    /// `redstone_tripwire::RECHECK_DELAY` = 10 ticks out. Run length is not
    /// part of that answer for any length in the legal range, so the long
    /// run that crosses a seam must produce byte-identical hook states.
    #[test]
    fn breaking_a_tripwire_reaches_the_hook_in_the_next_chunk_and_matches_the_single_column_run() {
        // The reference: hooks at x=2 and x=7, four wire cells between
        // them, the middle one broken. Entirely inside chunk (0, 0).
        let reference = TestWorld::new();
        reference.insert(0, 0, seam_column_with_floor());
        reference.set_block(2, SEAM_ROW_Y, SEAM_ROW_Z, &seam_hook("east", true, false));
        for x in 3..=6 {
            reference.set_block(x, SEAM_ROW_Y, SEAM_ROW_Z, SEAM_TRIPWIRE);
        }
        reference.set_block(7, SEAM_ROW_Y, SEAM_ROW_Z, &seam_hook("west", true, false));
        reference.set_block(5, SEAM_ROW_Y, SEAM_ROW_Z, "minecraft:air");
        let (reference_changed, reference_scheduled) = crate::server::propagate_removal_with_entities(
            &reference,
            BlockPos::new(5, SEAM_ROW_Y, SEAM_ROW_Z),
            SEAM_TRIPWIRE,
        );
        let reference_scanning = reference.block_state(2, SEAM_ROW_Y, SEAM_ROW_Z);
        let reference_receiving = reference.block_state(7, SEAM_ROW_Y, SEAM_ROW_Z);
        assert_eq!(
            reference_scanning,
            seam_hook("east", true, true),
            "reference premise: a snapped armed string must pulse the scanning hook"
        );
        assert_eq!(
            reference_receiving,
            seam_hook("west", true, true),
            "reference premise: one scan rewrites both endpoints"
        );
        assert_eq!(reference_changed.len(), 2, "reference premise: exactly the two hooks: {reference_changed:?}");
        assert_eq!(reference_scheduled.len(), 1, "reference premise: one recheck: {reference_scheduled:?}");
        assert_eq!(
            (
                reference_scheduled[0].pos,
                reference_scheduled[0].kind.as_ref(),
                reference_scheduled[0].trigger_tick,
            ),
            (
                (2, SEAM_ROW_Y, SEAM_ROW_Z),
                redstone_tripwire::TICK_TRIPWIRE_RECHECK,
                u64::from(redstone_tripwire::RECHECK_DELAY),
            ),
            "reference premise: the recheck belongs to the scanning hook, {} ticks out",
            redstone_tripwire::RECHECK_DELAY
        );

        // The cross-seam case: the same shape at a legal longer length —
        // hooks at x=2 (chunk 0) and x=21 (chunk 1), the broken cell at
        // x=18 (chunk 1). The scan runs from a chunk-1 cell 16 cells west
        // into chunk 0 to find its hook, then back east past the seam to
        // find the receiver, and writes to both chunks.
        const BREAK_X: i32 = 18;
        const SCANNING_X: i32 = 2;
        const RECEIVING_X: i32 = 21;
        assert_eq!(BREAK_X.div_euclid(16), 1, "the broken cell must sit in chunk (1, 0)");
        assert_eq!(SCANNING_X.div_euclid(16), 0, "the controlling hook must sit in chunk (0, 0)");
        assert_eq!(RECEIVING_X.div_euclid(16), 1, "the receiving hook must sit in chunk (1, 0)");
        assert!(
            RECEIVING_X - SCANNING_X < redstone_tripwire::WIRE_DIST_MAX,
            "the run must be a length a real hook scan would still reach"
        );

        let world = TestWorld::new();
        world.insert(0, 0, seam_column_with_floor());
        world.insert(1, 0, seam_column_with_floor());
        world.set_block(SCANNING_X, SEAM_ROW_Y, SEAM_ROW_Z, &seam_hook("east", true, false));
        for x in SCANNING_X + 1..RECEIVING_X {
            world.set_block(x, SEAM_ROW_Y, SEAM_ROW_Z, SEAM_TRIPWIRE);
        }
        world.set_block(RECEIVING_X, SEAM_ROW_Y, SEAM_ROW_Z, &seam_hook("west", true, false));
        world.set_block(BREAK_X, SEAM_ROW_Y, SEAM_ROW_Z, "minecraft:air");

        let (changed, scheduled) = crate::server::propagate_removal_with_entities(
            &world,
            BlockPos::new(BREAK_X, SEAM_ROW_Y, SEAM_ROW_Z),
            SEAM_TRIPWIRE,
        );

        assert_eq!(
            world.block_state(SCANNING_X, SEAM_ROW_Y, SEAM_ROW_Z),
            reference_scanning,
            "the controlling hook at world x={SCANNING_X} is in chunk (0, 0) while the broken cell is in \
             chunk (1, 0); its state must not depend on where the seam fell"
        );
        assert_eq!(
            world.block_state(RECEIVING_X, SEAM_ROW_Y, SEAM_ROW_Z),
            reference_receiving,
            "the receiving hook at world x={RECEIVING_X}"
        );
        assert_eq!(changed.len(), 2, "exactly the two hooks may be rewritten: {changed:?}");
        assert_eq!(scheduled.len(), 1, "exactly one recheck: {scheduled:?}");
        assert_eq!(
            (scheduled[0].pos, scheduled[0].kind.as_ref(), scheduled[0].trigger_tick),
            (
                (SCANNING_X, SEAM_ROW_Y, SEAM_ROW_Z),
                redstone_tripwire::TICK_TRIPWIRE_RECHECK,
                u64::from(redstone_tripwire::RECHECK_DELAY),
            ),
            "the recheck must land on the scanning hook across the seam, at the same delay"
        );
        // No wire cell may be rewritten: the run's `attached` did not flip,
        // so a model that rewrote every scanned segment would be caught
        // here rather than passing on the hook states alone.
        for x in [SCANNING_X + 1, 15, 16, RECEIVING_X - 1] {
            assert_eq!(
                world.block_state(x, SEAM_ROW_Y, SEAM_ROW_Z),
                SEAM_TRIPWIRE,
                "wire cell at world x={x} must be untouched"
            );
        }

        // The control: the same cross-seam fixture with chunk (0, 0)'s data
        // seeded and readable but declared not resident. The westward scan
        // must run out at the home column's edge and find no hook, so
        // nothing is rewritten and nothing is scheduled.
        let control = TestWorld::new();
        control.insert(0, 0, seam_column_with_floor());
        control.insert(1, 0, seam_column_with_floor());
        control.set_block(SCANNING_X, SEAM_ROW_Y, SEAM_ROW_Z, &seam_hook("east", true, false));
        for x in SCANNING_X + 1..RECEIVING_X {
            control.set_block(x, SEAM_ROW_Y, SEAM_ROW_Z, SEAM_TRIPWIRE);
        }
        control.set_block(RECEIVING_X, SEAM_ROW_Y, SEAM_ROW_Z, &seam_hook("west", true, false));
        control.set_block(BREAK_X, SEAM_ROW_Y, SEAM_ROW_Z, "minecraft:air");
        control.unload_but_keep(0, 0);
        let (control_changed, control_scheduled) = crate::server::propagate_removal_with_entities(
            &control,
            BlockPos::new(BREAK_X, SEAM_ROW_Y, SEAM_ROW_Z),
            SEAM_TRIPWIRE,
        );
        assert!(!control.is_column_resident(0, 0), "control premise: chunk (0, 0) must read as not resident");
        assert!(
            control_changed.is_empty() && control_scheduled.is_empty(),
            "control failed: with the controlling hook's chunk unloaded the scan must find nothing — \
             got {control_changed:?} / {control_scheduled:?}"
        );
        assert_eq!(
            control.block_state(SCANNING_X, SEAM_ROW_Y, SEAM_ROW_Z),
            seam_hook("east", true, false),
            "control failed: the unreachable hook must keep the state it was seeded with"
        );
    }

    /// **The random-tick fan-out.** `RandomTickScheduler::tick_chunk` is
    /// what `crate::tick::run_tick_loop`'s random-tick pass calls once per
    /// chunk in the follow area, and every mutation it makes owes its six
    /// neighbours a notification. An observer watches whatever cell sits in
    /// its facing direction and pulses when that cell's state changes, so
    /// an observer one cell over a chunk seam from a spreading grass block
    /// is the discriminating case: its pulse depends on a notification
    /// leaving the mutated cell's own column.
    ///
    /// The predicted value is the whole scheduled tick, not its existence:
    /// position `(16, 1, 8)`, kind `redstone:observer`, and trigger tick
    /// `4175` — the observer's own two-tick delay added to a deliberately
    /// unround `current_tick` of `4173`, so a model that scheduled at the
    /// current tick, at a repeater's `2 * delay`, or under another kind
    /// lands somewhere else. The reference for it is the same three-cell
    /// arrangement laid out inside one column.
    #[test]
    fn a_random_tick_mutation_notifies_the_observer_across_a_chunk_seam() {
        const CURRENT_TICK: u64 = 4173;
        const OBSERVER_DELAY: u64 = 2;

        /// Ticks a rig until the dirt cell at `target` becomes grass,
        /// returning the scheduled ticks that accumulated. Grass spread is
        /// a per-call probability, so this is a bounded wait rather than a
        /// single call — the same shape
        /// `grass_spreads_at_a_chunk_whose_local_and_absolute_z_differ`
        /// already uses.
        fn spread_then_collect(
            column: &mut ChunkColumn,
            world: &dyn ChunkSource,
            target: (i32, i32, i32),
        ) -> Vec<crate::scheduled_tick::ScheduledTick<String>> {
            let mut scheduler = RandomTickScheduler::new(7, 7);
            let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            for _ in 0..3000 {
                let events = scheduler.tick_chunk(column, 0, 0, 200, &mut block_ticks, CURRENT_TICK, world);
                if events.iter().any(|e| e.pos == target && base_name(&e.to) == GRASS_BLOCK) {
                    return block_ticks.drain_due(u64::MAX, usize::MAX);
                }
            }
            panic!("the dirt cell at {target:?} never became grass, so nothing was ever notified");
        }

        // The reference: grass source, the dirt it spreads onto, and the
        // observer watching that dirt, all inside chunk (0, 0).
        let mut reference_column = seam_column_with_floor();
        reference_column.set_block(11, SEAM_ROW_Y, SEAM_ROW_Z, GRASS_BLOCK);
        reference_column.set_block(12, SEAM_ROW_Y, SEAM_ROW_Z, DIRT_BLOCK);
        reference_column.set_block(
            13,
            SEAM_ROW_Y,
            SEAM_ROW_Z,
            &redstone_observer::set_observer(Direction::West, false),
        );
        let reference_scheduled = spread_then_collect(
            &mut reference_column,
            &NoNeighbors,
            (12, SEAM_ROW_Y, SEAM_ROW_Z),
        );
        assert_eq!(
            reference_scheduled
                .iter()
                .map(|t| (t.pos, t.kind.as_ref(), t.trigger_tick))
                .collect::<Vec<_>>(),
            vec![(
                (13, SEAM_ROW_Y, SEAM_ROW_Z),
                redstone::TICK_OBSERVER,
                CURRENT_TICK + OBSERVER_DELAY
            )],
            "reference premise: an observer beside a spreading grass block schedules exactly one pulse"
        );

        // The cross-seam case: the same three cells shifted so the observer
        // lands in chunk (1, 0) while the mutation stays in chunk (0, 0).
        let mut column = seam_column_with_floor();
        column.set_block(14, SEAM_ROW_Y, SEAM_ROW_Z, GRASS_BLOCK);
        column.set_block(15, SEAM_ROW_Y, SEAM_ROW_Z, DIRT_BLOCK);
        let mut neighbor = seam_column_with_floor();
        neighbor.set_block(
            0,
            SEAM_ROW_Y,
            SEAM_ROW_Z,
            &redstone_observer::set_observer(Direction::West, false),
        );
        let world = TestWorld::new();
        world.insert(1, 0, neighbor);

        let scheduled = spread_then_collect(&mut column, &world, (15, SEAM_ROW_Y, SEAM_ROW_Z));
        assert_eq!(
            scheduled
                .iter()
                .map(|t| (t.pos, t.kind.as_ref(), t.trigger_tick))
                .collect::<Vec<_>>(),
            vec![(
                (16, SEAM_ROW_Y, SEAM_ROW_Z),
                redstone::TICK_OBSERVER,
                CURRENT_TICK + OBSERVER_DELAY
            )],
            "the observer across the seam must be scheduled exactly as the single-column reference was, \
             at the same delay and under the same kind"
        );

        // The control: the same fixture with the observer's chunk seeded
        // and readable but declared not resident. Nothing may be
        // scheduled, which is the reading the assertion above produces when
        // the notification cannot leave its column.
        let mut control_column = seam_column_with_floor();
        control_column.set_block(14, SEAM_ROW_Y, SEAM_ROW_Z, GRASS_BLOCK);
        control_column.set_block(15, SEAM_ROW_Y, SEAM_ROW_Z, DIRT_BLOCK);
        let mut control_neighbor = seam_column_with_floor();
        control_neighbor.set_block(
            0,
            SEAM_ROW_Y,
            SEAM_ROW_Z,
            &redstone_observer::set_observer(Direction::West, false),
        );
        let control = TestWorld::new();
        control.insert(1, 0, control_neighbor);
        control.unload_but_keep(1, 0);
        let control_scheduled =
            spread_then_collect(&mut control_column, &control, (15, SEAM_ROW_Y, SEAM_ROW_Z));
        assert!(!control.is_column_resident(1, 0), "control premise: chunk (1, 0) must read as not resident");
        assert!(
            control_scheduled.is_empty(),
            "control failed: with the observer's chunk unloaded nothing may be scheduled — got {control_scheduled:?}"
        );
        assert_eq!(
            control.block_state(16, SEAM_ROW_Y, SEAM_ROW_Z),
            redstone_observer::set_observer(Direction::West, false),
            "control failed: the unreachable observer must keep the state it was seeded with"
        );
    }
}
