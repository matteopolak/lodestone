//! Fire spread and burnout — `FireBlock`'s scheduled tick, on the block-tick
//! queue.
//!
//! # What this is
//!
//! A port of `FireBlock` (its `tick`, `checkBurnOut`, `getIgniteOdds`,
//! `isValidFireLocation`, `canSurvive`, `getStateForPlacement` and
//! `getFireTickDelay`) plus the two `BaseFireBlock` statics it leans on
//! (`getState`, and `SoulFireBlock::canSurviveOnBlock`), read out of the
//! decompiled 26.2 tree as record definitions.
//!
//! Nothing about fire existed in this crate before: a `minecraft:fire` block sat
//! inert forever, spread to nothing and never went out. This module is the
//! behaviour, [`crate::random_tick`]'s lava arm is the producer that starts one,
//! and `tick::run_tick_loop`'s block-tick drain is what runs it.
//!
//! # It rides the scheduled-tick queue, not the random tick
//!
//! `Blocks.FIRE` is registered **without** `randomTicks()`, so fire never
//! random-ticks. It schedules itself: `FireBlock::onPlace` schedules one tick,
//! and the first statement of `FireBlock::tick` is *always* another
//! `scheduleTick(pos, this, getFireTickDelay(random))`. That is why
//! [`run_scheduled_tick`] reschedules unconditionally before it does anything
//! else, and why a fire block that somehow loses its pending tick is inert
//! forever — see [`ticks_after_edit`], the seeding hook that makes a fire written
//! by any path start ticking.
//!
//! # The RNG draw sequence, which *is* the specification
//!
//! A reordered or extra draw produces a plausible fire that is not vanilla's, so
//! the order below is transcribed rather than reasoned about. One tick of one fire
//! block draws, in order:
//!
//! | # | draw | when |
//! |---|---|---|
//! | 1 | `nextInt(10)` — the reschedule delay | **always**, before the spread gate |
//! | 2 | `nextFloat()` — the rain-out roll | only if not infiniburn **and** raining **and** near rain |
//! | 3 | `nextInt(3)` — the age advance, `age + n/2` | whenever draw 2 did not extinguish |
//! | 4 | `nextInt(4)` — the age-15 self-extinguish | only if `age == 15` and not infiniburn |
//! | 5–10 | `nextInt(300 or 250)` — one per neighbour burn-out check | **always**, six of them, east, west, down, up, north, south |
//! | (5–10, on a hit) | `nextInt(age + 10)`, then `nextInt(5)` | per neighbour that the check consumed |
//! | 11… | `nextInt(rate)` — one per spread candidate whose odds are positive | over the 26-cell neighbourhood, in `x → z → y` order |
//! | (11…, on a hit) | `nextInt(5)` — the spread age | per cell actually set alight |
//!
//! Two of those are easy to get wrong and both are pinned by a test.
//! **`checkBurnOut` draws its `nextInt(chance)` even when the neighbour's burn
//! odds are `0`** — the comparison is `nextInt(chance) < odds`, evaluated after
//! the draw — so a fire in the middle of stone still consumes exactly six draws
//! there. And **the neighbourhood loop is `x`, then `z`, then `y`**, not `x, y,
//! z`, with `y` running `-1..=4`: fire reaches four cells *up* and one down.
//!
//! # The odds arithmetic, and what it predicts
//!
//! A candidate cell's chance is integer arithmetic:
//! `odds = (igniteOdds + 40 + difficulty * 7) / (age + 30)`, halved if the
//! dimension has increased burnout, and the cell catches when
//! `nextInt(rate) <= odds`, where `rate` is `100` at or below one cell up and
//! `100 * y` above that. Both divisions truncate, and that truncation is the
//! whole shape of fire's behaviour:
//!
//! * fresh oak planks (`igniteOdds` 5) on normal difficulty (2) beside a
//!   brand-new fire (`age` 0) give `(5 + 40 + 14) / 30 = 1`, so a 2-in-100 chance
//!   per tick (`nextInt(100) <= 1` is two of a hundred values);
//! * the same planks beside a fully aged fire (`age` 15) give `59 / 45 = 1` —
//!   *unchanged*, because truncation swallows the difference;
//! * short grass (`igniteOdds` 60) at `age` 0 gives `114 / 30 = 3`, four times as
//!   likely as planks — which is why a grass fire runs and a plank fire creeps;
//! * anything two or more cells above the fire has `rate` 200 or more, so its
//!   chance halves per level.
//!
//! [`spread_odds`] is that expression alone, and
//! [`spread_odds_match_the_predicted_integer_arithmetic`] asserts the four
//! numbers above rather than a direction.
//!
//! # What is deliberately not modelled
//!
//! * **`isRainingAt`'s heightmap term.** Vanilla is
//!   `isRaining() && canSeeSky(pos) && heightmap(MOTION_BLOCKING) <= y && biome
//!   precipitation == RAIN`. [`FireEnv::raining`] carries the world flag and
//!   [`sky_exposed`] stands in for the `canSeeSky` + heightmap pair by scanning
//!   upward for a motion-blocking block; the biome term is absent, so fire is
//!   rained out in a desert where vanilla would not rain at all. Everything here
//!   is gated behind `raining`, so a dry world pays for none of it.
//! * **`SoulFireBlock`'s own tick.** [`state_at`] will place `minecraft:soul_fire`
//!   over soul sand or soul soil, matching `BaseFireBlock::getState`, but soul
//!   fire does not spread or burn out in vanilla either (it has no `tick`), so
//!   nothing further is needed.
//! * **Neighbour notification.** A block consumed by fire does not notify its own
//!   neighbours here, so a torch losing its support to a fire stays floating.
//! * **`TntBlock::prime`.** `checkBurnOut` primes TNT it consumes; this crate has
//!   no primed-TNT entity, so the TNT is simply burnt away.
//! * **Fire damage to entities.** `BaseFireBlock::entityInside` is the entity
//!   domain's.
//!
//! # How to change it
//!
//! **Every world read goes through [`block_at`]**, which answers air outside build
//! height. That is a hard invariant, not a style preference, and it is the same
//! one the fluid port documents: this module reads the cell *below* whatever it
//! inspects, so a fire on the world floor asks for `min_y - 1`, and
//! `ChunkColumn::block_state` indexes unguarded — an unchecked read panics the
//! world tick thread. `Level::getBlockState`'s own first line is the same guard.

use lodestone_data::block_blast;
use lodestone_model::BlockPos;

use crate::chunk::ChunkSource;
use crate::mob_spawn::SpawnRng;
use crate::scheduled_tick::{ScheduledTick, ScheduledTickQueue, TickPriority};

/// The scheduled-tick `kind` every fire tick carries, in the same `String`-keyed
/// space `crate::redstone`'s `TICK_TORCH` and `crate::fluid`'s `TICK_FLUID`
/// already use. `tick::run_tick_loop`'s block-tick drain dispatches on it.
pub const TICK_FIRE: &str = "lodestone:fire";

/// `FireBlock.MAX_AGE`.
pub const MAX_AGE: u32 = 15;

/// `getFireTickDelay`'s base — `30 + random.nextInt(10)`.
pub const TICK_DELAY_BASE: u64 = 30;

/// `getFireTickDelay`'s jitter bound.
pub const TICK_DELAY_JITTER: i32 = 10;

/// The `+40` in `(igniteOdds + 40 + difficulty * 7) / (age + 30)`.
pub const ODDS_BASE: i32 = 40;

/// The `* 7` difficulty weight in the same expression.
pub const ODDS_PER_DIFFICULTY: i32 = 7;

/// The `+30` denominator offset in the same expression.
pub const ODDS_AGE_OFFSET: i32 = 30;

/// `minecraft:fire`.
pub const FIRE: &str = "minecraft:fire";

/// `minecraft:soul_fire`.
pub const SOUL_FIRE: &str = "minecraft:soul_fire";

/// `#minecraft:infiniburn_overworld`, read straight out of the server jar's own
/// tag data — **`netherrack` and `magma_block`, and not `bedrock`**.
///
/// It is a block *tag*, not a name list, which is exactly why it is worth pinning
/// here: guessing it as "bedrock" (the intuitive answer, and the nether's tag is a
/// different set again) would make an eternal fire over netherrack burn out and a
/// fire on bedrock eternal — both backwards.
pub const INFINIBURN_OVERWORLD: [&str; 2] = ["minecraft:netherrack", "minecraft:magma_block"];

/// The blocks `SoulFireBlock::canSurviveOnBlock` accepts (`#minecraft:soul_fire_base_blocks`).
pub const SOUL_FIRE_BASE: [&str; 2] = ["minecraft:soul_sand", "minecraft:soul_soil"];

/// Everything about the world a fire tick needs that is not a block state.
///
/// A plain value rather than a handle, so this module needs no shared state and
/// the tick loop stays the only thing that reads `world_state`/`weather`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FireEnv {
    /// Lowest addressable `y` — see [`block_at`].
    pub min_y: i32,
    /// Number of block rows above [`min_y`](Self::min_y).
    pub height: i32,
    /// `Difficulty::getId`: peaceful 0, easy 1, normal 2, hard 3. Feeds
    /// [`spread_odds`]'s `difficulty * 7` term, so a hard-difficulty world really
    /// does spread fire faster.
    pub difficulty_id: i32,
    /// `Level::isRaining` — the world flag, not the per-position test. When
    /// `false`, nothing here performs a sky scan at all.
    pub raining: bool,
    /// `ServerLevel::canSpreadFireAround` reduced to its answer: in 26.2 that is
    /// `fire_spread_radius_around_player == -1 || a player is within it`, so the
    /// caller (which knows where the players are) resolves it and passes the
    /// boolean. `false` freezes fire completely, exactly as the old `doFireTick`
    /// gamerule did.
    pub spread_allowed: bool,
    /// `EnvironmentAttributes.INCREASED_FIRE_BURNOUT` — a dimension attribute.
    /// `false` in the overworld; kept so the arithmetic is written down once
    /// rather than rediscovered when a second dimension lands.
    pub increased_burnout: bool,
}

impl FireEnv {
    /// 26.2's overworld at normal difficulty, dry, with fire spreading — the
    /// shape every test here uses and a sensible default for a caller with no
    /// column in hand.
    pub const OVERWORLD: FireEnv = FireEnv {
        min_y: -64,
        height: 384,
        difficulty_id: 2,
        raining: false,
        spread_allowed: true,
        increased_burnout: false,
    };

    /// `LevelHeightAccessor::isInsideBuildHeight`.
    #[must_use]
    pub fn contains_y(self, y: i32) -> bool {
        y >= self.min_y && y < self.min_y + self.height
    }
}

/// The block state at `pos`, or air when `pos` is outside the dimension's build
/// height — `Level::getBlockState`, whose own first line is the same guard.
///
/// **Every world read in this module goes through here.** See the module doc for
/// why that is load-bearing rather than tidy.
#[must_use]
pub fn block_at<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> String {
    if env.contains_y(pos.y) {
        world.block_state(pos.x, pos.y, pos.z)
    } else {
        crate::chunk::AIR.to_owned()
    }
}

/// Strips a `[...]` property suffix.
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// The value of `state`'s `key=` property. A whole-key match.
fn property_of<'s>(state: &'s str, key: &str) -> Option<&'s str> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// `true` for `minecraft:fire` or `minecraft:soul_fire`.
#[must_use]
pub fn is_fire(state: &str) -> bool {
    matches!(base_name(state), FIRE | SOUL_FIRE)
}

/// `true` for `minecraft:fire` alone — soul fire has no `age` and no tick.
#[must_use]
pub fn is_ordinary_fire(state: &str) -> bool {
    base_name(state) == FIRE
}

/// `FireBlock.AGE` for a fire state, defaulting to `0` (the default state's
/// value) when the property is absent.
#[must_use]
pub fn age_of(state: &str) -> u32 {
    property_of(state, "age")
        .and_then(|value| value.parse::<u32>().ok())
        .map_or(0, |age| age.min(MAX_AGE))
}

/// `FireBlock::canBurn` — `getIgniteOdds(state) > 0`, with the
/// `waterlogged=true` override already applied.
#[must_use]
pub fn can_burn(state: &str) -> bool {
    block_blast::ignite_odds_for_state(state) > 0
}

/// `BlockStateBase::isFaceSturdy(level, pos, UP)`, from the committed
/// `face_full_up` census — the same fact vanilla's `isFaceSturdy(…, UP,
/// SupportType.FULL)` reads off the collision shape.
#[must_use]
pub fn face_sturdy_up(state: &str) -> bool {
    lodestone_data::block_states::state_id(state)
        .and_then(lodestone_data::snow_support::face_full_up)
        .unwrap_or(false)
}

/// `BlockStateBase::blocksMotion`, for the sky scan.
fn blocks_motion(state: &str) -> bool {
    lodestone_data::block_states::state_id(state)
        .and_then(lodestone_data::block_solidity::blocks_motion)
        .unwrap_or(false)
}

/// The six face offsets in `Direction.values()` order — **down, up, north,
/// south, west, east**.
///
/// Order matters for `isValidFireLocation`/`getIgniteOdds` only in that both are
/// an OR and a max respectively, so a wrong order is invisible; it is written down
/// for exactly that reason. It does **not** decide the `checkBurnOut` order, which
/// is a separate hand-written sequence — see [`BURN_OUT_ORDER`].
const FACE_OFFSETS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

/// `FireBlock::tick`'s six `checkBurnOut` calls, in the order it makes them,
/// with each one's `chance` denominator: **east, west, below, above, north,
/// south**, at `300, 300, 250, 250, 300, 300`.
///
/// This order and these numbers are part of the RNG specification: six draws
/// happen every tick regardless of what is there, so swapping two entries shifts
/// every later draw in the tick. The vertical pair being `250` rather than `300`
/// is why fire eats the block above and below it faster than the ones beside it.
pub const BURN_OUT_ORDER: [((i32, i32, i32), i32); 6] = [
    ((1, 0, 0), 300),
    ((-1, 0, 0), 300),
    ((0, -1, 0), 250),
    ((0, 1, 0), 250),
    ((0, 0, -1), 300),
    ((0, 0, 1), 300),
];

/// `FireBlock::isValidFireLocation` — any of the six face neighbours can burn.
#[must_use]
pub fn is_valid_fire_location<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> bool {
    FACE_OFFSETS.iter().any(|&(dx, dy, dz)| {
        can_burn(&block_at(
            world,
            env,
            BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz),
        ))
    })
}

/// `FireBlock::canSurvive` — the block below has a sturdy up face, or some
/// neighbour can burn.
#[must_use]
pub fn can_survive<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> bool {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    face_sturdy_up(&block_at(world, env, below)) || is_valid_fire_location(world, env, pos)
}

/// `FireBlock::getIgniteOdds(LevelReader, BlockPos)` — `0` unless the cell itself
/// is empty, otherwise the **maximum** ignite odds over its six face neighbours.
#[must_use]
pub fn ignite_odds_at<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> u8 {
    if !crate::random_tick::is_air_variant(&block_at(world, env, pos)) {
        return 0;
    }
    FACE_OFFSETS
        .iter()
        .map(|&(dx, dy, dz)| {
            block_blast::ignite_odds_for_state(&block_at(
                world,
                env,
                BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz),
            ))
        })
        .max()
        .unwrap_or(0)
}

/// `(igniteOdds + 40 + difficulty * 7) / (age + 30)`, halved for increased
/// burnout — both divisions truncating, as Java's integer division does.
///
/// Pure arithmetic with no world and no RNG, so it can be predicted rather than
/// observed. See this module's doc comment for the four numbers it implies.
#[must_use]
pub fn spread_odds(ignite_odds: u8, age: u32, difficulty_id: i32, increased_burnout: bool) -> i32 {
    let numerator =
        i32::from(ignite_odds) + ODDS_BASE + difficulty_id * ODDS_PER_DIFFICULTY;
    let mut odds = numerator / (age as i32 + ODDS_AGE_OFFSET);
    if increased_burnout {
        odds /= 2;
    }
    odds
}

/// The `rate` a spread candidate at vertical offset `dy` rolls against: `100`,
/// plus `100` per cell above the first one up.
#[must_use]
pub fn spread_rate(dy: i32) -> i32 {
    let mut rate = 100;
    if dy > 1 {
        rate += (dy - 1) * 100;
    }
    rate
}

/// `getFireTickDelay` — `30 + random.nextInt(10)`. One draw, always the first of
/// a tick.
#[must_use]
pub fn fire_tick_delay(rng: &mut SpawnRng) -> u64 {
    TICK_DELAY_BASE + rng.next_int(TICK_DELAY_JITTER) as u64
}

/// `Level::isRainingAt` reduced to what this crate can answer: the world is
/// raining and nothing motion-blocking stands above `pos`.
///
/// Stands in for `canSeeSky` **and** the `MOTION_BLOCKING` heightmap term at
/// once; the biome-precipitation term is absent. Scans upward to build height, so
/// it is only ever called behind [`FireEnv::raining`].
#[must_use]
pub fn is_raining_at<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> bool {
    env.raining && sky_exposed(world, env, pos)
}

/// `Level::canSeeSky` plus the heightmap term — nothing motion-blocking between
/// `pos` and build height.
#[must_use]
pub fn sky_exposed<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> bool {
    let top = env.min_y + env.height;
    let mut y = pos.y + 1;
    while y < top {
        if blocks_motion(&block_at(world, env, BlockPos::new(pos.x, y, pos.z))) {
            return false;
        }
        y += 1;
    }
    true
}

/// `FireBlock::isNearRain` — raining at this cell or any of its four horizontal
/// neighbours.
#[must_use]
pub fn is_near_rain<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> bool {
    if !env.raining {
        return false;
    }
    is_raining_at(world, env, pos)
        || is_raining_at(world, env, BlockPos::new(pos.x - 1, pos.y, pos.z))
        || is_raining_at(world, env, BlockPos::new(pos.x + 1, pos.y, pos.z))
        || is_raining_at(world, env, BlockPos::new(pos.x, pos.y, pos.z - 1))
        || is_raining_at(world, env, BlockPos::new(pos.x, pos.y, pos.z + 1))
}

/// `FireBlock::getStateForPlacement` — the connected-face form when the cell has
/// no sturdy or burnable support below it, otherwise the plain default state.
///
/// The five booleans are the client's rendering input: a fire with no floor draws
/// itself against whichever walls can burn.
#[must_use]
pub fn state_for_placement<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> String {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let below_state = block_at(world, env, below);
    if !can_burn(&below_state) && !face_sturdy_up(&below_state) {
        let up = can_burn(&block_at(world, env, BlockPos::new(pos.x, pos.y + 1, pos.z)));
        let north = can_burn(&block_at(world, env, BlockPos::new(pos.x, pos.y, pos.z - 1)));
        let south = can_burn(&block_at(world, env, BlockPos::new(pos.x, pos.y, pos.z + 1)));
        let west = can_burn(&block_at(world, env, BlockPos::new(pos.x - 1, pos.y, pos.z)));
        let east = can_burn(&block_at(world, env, BlockPos::new(pos.x + 1, pos.y, pos.z)));
        // Properties in the canonical alphabetical order this crate's state
        // strings always use.
        format!(
            "{FIRE}[age=0,east={east},north={north},south={south},up={up},west={west}]"
        )
    } else {
        format!("{FIRE}[age=0,east=false,north=false,south=false,up=false,west=false]")
    }
}

/// `BaseFireBlock::getState` — soul fire over a soul-fire base block, otherwise
/// `FireBlock::getStateForPlacement`.
#[must_use]
pub fn state_at<S: ChunkSource>(world: &S, env: FireEnv, pos: BlockPos) -> String {
    let below = block_at(world, env, BlockPos::new(pos.x, pos.y - 1, pos.z));
    if SOUL_FIRE_BASE.contains(&base_name(&below)) {
        return SOUL_FIRE.to_owned();
    }
    state_for_placement(world, env, pos)
}

/// `FireBlock::getStateWithAge` — [`state_at`] with `age` written over it, but
/// only when the answer really is ordinary fire (soul fire has no `age`).
#[must_use]
pub fn state_with_age<S: ChunkSource>(
    world: &S,
    env: FireEnv,
    pos: BlockPos,
    age: u32,
) -> String {
    let state = state_at(world, env, pos);
    if !is_ordinary_fire(&state) {
        return state;
    }
    with_age(&state, age)
}

/// Rewrites a fire state's `age=` property.
fn with_age(state: &str, age: u32) -> String {
    match state.split_once('[') {
        None => format!("{state}[age={age}]"),
        Some((name, rest)) => {
            let props = rest.strip_suffix(']').unwrap_or(rest);
            let rewritten: Vec<String> = props
                .split(',')
                .map(|pair| match pair.split_once('=') {
                    Some((key, _)) if key.trim() == "age" => format!("age={age}"),
                    _ => pair.to_owned(),
                })
                .collect();
            format!("{name}[{}]", rewritten.join(","))
        }
    }
}

/// The fire ticks one block edit owes — this cell alone, at a **relative** delay
/// the tick loop rebases onto its own counter.
///
/// The seeding hook, standing in for `FireBlock::onPlace`. Unlike the fluid
/// equivalent it does **not** cover the six neighbours: fire is not woken by a
/// neighbour changing (its `updateShape` only re-derives its own connected faces),
/// so only the edited cell can owe a tick.
///
/// The delay is the fixed [`TICK_DELAY_BASE`] rather than
/// `30 + nextInt(10)`, because this runs on a connection thread with no RNG in
/// scope. The consequence is that the *first* tick of a hand-placed fire is
/// exactly 30 ticks away instead of 30–39; every subsequent one draws properly in
/// [`run_scheduled_tick`]. That is a timing jitter, not a decision — the same
/// class of documented reduction as the lava spread delay in the fluid port.
#[must_use]
pub fn ticks_after_edit(pos: BlockPos) -> Vec<ScheduledTick<String>> {
    // Built through a real queue rather than struct literals because
    // `ScheduledTick::sub_tick_order` is private.
    let mut pending: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    pending.schedule(
        (pos.x, pos.y, pos.z),
        TICK_FIRE.to_owned(),
        TICK_DELAY_BASE,
        TickPriority::Normal,
    );
    pending.drain_due(u64::MAX, usize::MAX)
}

/// One fire block's scheduled tick — `FireBlock::tick`, transcribed.
///
/// Writes every change straight through `world` (as vanilla's immediate
/// `setBlock` does, and because the spread loop reads cells it has already
/// written) and appends each one to `changes` for the caller to publish.
/// Reschedules itself into `block_ticks` unconditionally as its first act.
///
/// `pos` need not still hold fire: a tick whose cell has been replaced since it
/// was scheduled returns after the reschedule is skipped, so a stale entry costs
/// one read.
pub fn run_scheduled_tick<S: ChunkSource>(
    world: &S,
    env: FireEnv,
    pos: BlockPos,
    block_ticks: &mut ScheduledTickQueue<String>,
    current_tick: u64,
    rng: &mut SpawnRng,
    changes: &mut Vec<(BlockPos, String)>,
) {
    let state = block_at(world, env, pos);
    if !is_ordinary_fire(&state) {
        // Not our block any more (burnt out, replaced, or soul fire, which does
        // not tick). No reschedule and no draws — vanilla would not have
        // dispatched `FireBlock::tick` here at all.
        return;
    }

    // Draw 1, and vanilla's own first statement.
    let delay = fire_tick_delay(rng);
    block_ticks.schedule(
        (pos.x, pos.y, pos.z),
        TICK_FIRE.to_owned(),
        current_tick + delay,
        TickPriority::Normal,
    );

    if !env.spread_allowed {
        return;
    }

    let set = |world: &S, at: BlockPos, new_state: String, changes: &mut Vec<(BlockPos, String)>| {
        if !env.contains_y(at.y) {
            return;
        }
        world.set_block(at.x, at.y, at.z, &new_state);
        changes.push((at, new_state));
    };

    // `if (!state.canSurvive(level, pos)) level.removeBlock(pos, false);` — and
    // vanilla does **not** return here, so the rest of the tick still runs
    // against the `state` local and can write fire back.
    if !can_survive(world, env, pos) {
        set(world, pos, crate::chunk::AIR.to_owned(), changes);
    }

    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let below_state = block_at(world, env, below);
    let infini_burn = INFINIBURN_OVERWORLD.contains(&base_name(&below_state));
    let age = age_of(&state);

    // Draw 2, behind three short-circuiting conditions.
    if !infini_burn && env.raining && is_near_rain(world, env, pos) {
        if rng.next_f32() < 0.2 + age as f32 * 0.03 {
            set(world, pos, crate::chunk::AIR.to_owned(), changes);
            return;
        }
    }

    // Draw 3.
    let new_age = MAX_AGE.min(age + (rng.next_int(3) / 2) as u32);
    if age != new_age {
        set(world, pos, with_age(&state, new_age), changes);
    }

    if !infini_burn {
        if !is_valid_fire_location(world, env, pos) {
            if !face_sturdy_up(&below_state) || age > 3 {
                set(world, pos, crate::chunk::AIR.to_owned(), changes);
            }
            return;
        }
        // Draw 4.
        if age == MAX_AGE && rng.next_int(4) == 0 && !can_burn(&below_state) {
            set(world, pos, crate::chunk::AIR.to_owned(), changes);
            return;
        }
    }

    let extra = if env.increased_burnout { -50 } else { 0 };
    // Draws 5-10, plus their conditional follow-ups.
    for &((dx, dy, dz), chance) in &BURN_OUT_ORDER {
        check_burn_out(
            world,
            env,
            BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz),
            chance + extra,
            rng,
            age,
            changes,
        );
    }

    // The spread neighbourhood: x, then z, then y, with y from -1 to 4.
    for dx in -1..=1 {
        for dz in -1..=1 {
            for dy in -1..=4 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                let rate = spread_rate(dy);
                let test = BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz);
                let ignite_odds = ignite_odds_at(world, env, test);
                if ignite_odds == 0 {
                    continue;
                }
                let odds = spread_odds(ignite_odds, age, env.difficulty_id, env.increased_burnout);
                if odds <= 0 {
                    continue;
                }
                // One draw per positive-odds candidate.
                if rng.next_int(rate) <= odds
                    && (!env.raining || !is_near_rain(world, env, test))
                {
                    let spread_age = MAX_AGE.min(age + (rng.next_int(5) / 4) as u32);
                    let new_state = state_with_age(world, env, test, spread_age);
                    set(world, test, new_state, changes);
                    // The new fire owes itself a tick, or it is inert forever.
                    block_ticks.schedule(
                        (test.x, test.y, test.z),
                        TICK_FIRE.to_owned(),
                        current_tick + TICK_DELAY_BASE,
                        TickPriority::Normal,
                    );
                }
            }
        }
    }
}

/// `FireBlock::checkBurnOut` — the one neighbour-consuming half.
///
/// **Draws `nextInt(chance)` unconditionally**, before comparing against the
/// neighbour's burn odds, so a check against stone still costs one draw. That is
/// the single easiest thing here to "optimise" into a divergent RNG stream.
#[allow(clippy::too_many_arguments)]
fn check_burn_out<S: ChunkSource>(
    world: &S,
    env: FireEnv,
    pos: BlockPos,
    chance: i32,
    rng: &mut SpawnRng,
    age: u32,
    changes: &mut Vec<(BlockPos, String)>,
) {
    let state = block_at(world, env, pos);
    let odds = i32::from(block_blast::burn_odds_for_state(&state));
    if rng.next_int(chance.max(1)) >= odds {
        return;
    }
    if !env.contains_y(pos.y) {
        return;
    }
    // `random.nextInt(age + 10) < 5 && !level.isRainingAt(pos)` — the draw
    // happens first, the rain test only if it hits.
    if rng.next_int(age as i32 + 10) < 5 && !is_raining_at(world, env, pos) {
        let new_age = MAX_AGE.min(age + (rng.next_int(5) / 4) as u32);
        let new_state = state_with_age(world, env, pos, new_age);
        world.set_block(pos.x, pos.y, pos.z, &new_state);
        changes.push((pos, new_state));
    } else {
        world.set_block(pos.x, pos.y, pos.z, crate::chunk::AIR);
        changes.push((pos, crate::chunk::AIR.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::chunk::ChunkColumn;

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;

    /// A `ChunkSource` that retains its edits, as `run_scheduled_tick` requires:
    /// the spread loop reads cells it has already written.
    struct Rig {
        columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
    }

    impl Rig {
        fn new() -> Self {
            Self {
                columns: Mutex::new(HashMap::new()),
            }
        }

        /// A solid `fill` floor at `y <= floor_y`, air above.
        fn with_floor(fill: &str, floor_y: i32) -> Self {
            let rig = Self::new();
            for z in -20..20 {
                for x in -20..20 {
                    for y in MIN_Y..=floor_y {
                        rig.set_block(x, y, z, fill);
                    }
                }
            }
            rig
        }
    }

    impl ChunkSource for Rig {
        fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
            let mut columns = self.columns.lock().expect("rig lock");
            columns
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
                .clone()
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.block_state(x - cx * 16, y, z - cz * 16).to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns
                .entry((cx, cz))
                .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.set_block(x - cx * 16, y, z - cz * 16, name);
        }
    }

    /// One seed, so a test can build two *independent* generators — `SpawnRng` is
    /// not `Copy`, and a determinism or draw-count gate that reused one instance
    /// would be measuring memoisation rather than the count.
    const SEED: u64 = 0x0F1_1EE0_D00D_5EED;

    fn rng() -> SpawnRng {
        SpawnRng::new(SEED)
    }

    fn fire_at(rig: &Rig, pos: BlockPos, age: u32) {
        rig.set_block(
            pos.x,
            pos.y,
            pos.z,
            &format!("{FIRE}[age={age},east=false,north=false,south=false,up=false,west=false]"),
        );
    }

    /// Premise check: the rig retains edits, so "the fire went out" cannot be the
    /// rig regenerating terrain under the test.
    #[test]
    fn the_rig_retains_its_own_edits() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        assert_eq!(rig.block_state(0, 0, 0), "minecraft:stone");
        rig.set_block(0, 1, 0, "minecraft:oak_planks");
        assert_eq!(rig.block_state(0, 1, 0), "minecraft:oak_planks");
    }

    /// The four odds values this module's doc comment predicts, computed from
    /// vanilla's constants and the committed ignite-odds table — not from any
    /// output of this module.
    #[test]
    fn spread_odds_match_the_predicted_integer_arithmetic() {
        let planks = block_blast::blast("minecraft:oak_planks").unwrap().ignite_odds;
        assert_eq!(planks, 5);
        let grass = block_blast::blast("minecraft:short_grass").unwrap().ignite_odds;
        assert_eq!(grass, 60);

        // (5 + 40 + 2*7) / (0 + 30) = 59 / 30 = 1
        assert_eq!(spread_odds(planks, 0, 2, false), 1);
        // (5 + 40 + 14) / (15 + 30) = 59 / 45 = 1 — truncation swallows the age.
        assert_eq!(spread_odds(planks, 15, 2, false), 1);
        // (60 + 40 + 14) / 30 = 114 / 30 = 3
        assert_eq!(spread_odds(grass, 0, 2, false), 3);
        // Hard difficulty adds 7: (60 + 40 + 21) / 30 = 121 / 30 = 4
        assert_eq!(spread_odds(grass, 0, 3, false), 4);
        // Increased burnout halves it after the truncation: 3 / 2 = 1.
        assert_eq!(spread_odds(grass, 0, 2, true), 1);
        // Peaceful still spreads: (60 + 40) / 30 = 3.
        assert_eq!(spread_odds(grass, 0, 0, false), 3);
    }

    /// `rate` per vertical offset — `100` at and below one up, then `+100` per
    /// level, which is what makes fire climb slower the higher it reaches.
    #[test]
    fn spread_rate_matches_the_vertical_table() {
        assert_eq!(spread_rate(-1), 100);
        assert_eq!(spread_rate(0), 100);
        assert_eq!(spread_rate(1), 100);
        assert_eq!(spread_rate(2), 200);
        assert_eq!(spread_rate(3), 300);
        assert_eq!(spread_rate(4), 400);
    }

    /// The infiniburn tag is netherrack and magma block, **not** bedrock. Read out
    /// of the jar's own tag JSON; asserted here so a "surely it is bedrock" edit
    /// fails immediately.
    #[test]
    fn the_infiniburn_tag_is_netherrack_and_magma_not_bedrock() {
        assert!(INFINIBURN_OVERWORLD.contains(&"minecraft:netherrack"));
        assert!(INFINIBURN_OVERWORLD.contains(&"minecraft:magma_block"));
        assert!(!INFINIBURN_OVERWORLD.contains(&"minecraft:bedrock"));
        assert_eq!(INFINIBURN_OVERWORLD.len(), 2);
    }

    /// The exact draw count of one tick of a fire over **netherrack** with nothing
    /// burnable anywhere: **1 delay + 1 age + 6 burn-out checks = 8**, and not one
    /// more.
    ///
    /// Netherrack rather than stone, and the difference is the point: over stone
    /// the tick returns early at `isValidFireLocation` (no neighbour can burn) and
    /// draws only 2, while infiniburn skips that return and reaches the burn-out
    /// checks. So this is the tick's full RNG budget in its most reducible case,
    /// and it catches the two easy mistakes: skipping the `nextInt(chance)` when a
    /// neighbour's burn odds are `0` would give 2, and drawing per spread candidate
    /// regardless of its odds would give 34.
    #[test]
    fn a_fire_over_netherrack_draws_exactly_eight_values() {
        let rig = Rig::with_floor("minecraft:netherrack", 0);
        let pos = BlockPos::new(2, 1, 2);
        fire_at(&rig, pos, 0);
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut r = rng();
        let mut changes = Vec::new();
        run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, 100, &mut r, &mut changes);

        let mut reference = rng();
        // 1: nextInt(10); 2: nextInt(3); 3-8: six nextInt(300/250).
        reference.next_int(10);
        reference.next_int(3);
        for &(_, chance) in &BURN_OUT_ORDER {
            reference.next_int(chance);
        }
        assert_eq!(
            reference.next_int(1_000_000),
            r.next_int(1_000_000),
            "a fire over netherrack with nothing burnable must draw exactly 8 values"
        );
    }

    /// The other side of the same fork: over **stone** the tick returns at
    /// `isValidFireLocation` having drawn only the delay and the age advance.
    /// Together with the test above this pins the early return itself, which no
    /// outcome assertion would notice.
    #[test]
    fn a_fire_over_stone_returns_early_after_two_draws() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let pos = BlockPos::new(2, 1, 2);
        fire_at(&rig, pos, 0);
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut r = rng();
        let mut changes = Vec::new();
        run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, 100, &mut r, &mut changes);

        let mut reference = rng();
        reference.next_int(10);
        reference.next_int(3);
        assert_eq!(
            reference.next_int(1_000_000),
            r.next_int(1_000_000),
            "a fire over bare stone must draw exactly 2 values"
        );
    }

    /// Negative control for the count above: seven draws leaves the generators out
    /// of step, so the equality is measuring the count.
    #[test]
    fn the_eight_draw_control_fails_at_seven() {
        let mut a = rng();
        let mut b = rng();
        for _ in 0..7 {
            a.next_int(300);
        }
        for _ in 0..8 {
            b.next_int(300);
        }
        assert_ne!(a.next_int(1_000_000), b.next_int(1_000_000));
    }

    /// Fire reschedules itself unconditionally, at `30 + nextInt(10)`, so the
    /// pending tick is always in `[current + 30, current + 39]`.
    #[test]
    fn every_tick_reschedules_itself_within_thirty_to_thirty_nine() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let pos = BlockPos::new(2, 1, 2);
        let mut r = rng();
        for round in 0..40u64 {
            fire_at(&rig, pos, 0);
            let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let mut changes = Vec::new();
            run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, round, &mut r, &mut changes);
            let pending: Vec<_> = queue.drain_due(u64::MAX, usize::MAX);
            let own = pending
                .iter()
                .find(|t| t.pos == (pos.x, pos.y, pos.z))
                .expect("fire always reschedules itself");
            assert!(
                own.trigger_tick >= round + 30 && own.trigger_tick <= round + 39,
                "delay out of range: {} at round {round}",
                own.trigger_tick - round
            );
        }
    }

    /// A fire with no support and no burnable neighbour goes out — `canSurvive`
    /// failing, then the `isValidFireLocation` early return.
    #[test]
    fn fire_in_mid_air_goes_out() {
        let rig = Rig::new();
        let pos = BlockPos::new(2, 100, 2);
        fire_at(&rig, pos, 0);
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut r = rng();
        let mut changes = Vec::new();
        run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, 0, &mut r, &mut changes);
        assert_eq!(
            rig.block_state(pos.x, pos.y, pos.z),
            crate::chunk::AIR,
            "unsupported fire must go out"
        );
    }

    /// A fire on **netherrack** never goes out however long it burns, because
    /// infiniburn skips both the `isValidFireLocation` removal and the age-15
    /// self-extinguish. 200 consecutive ticks, and the cell still holds fire.
    ///
    /// The negative control is the same 200 ticks over stone, which *does* go out —
    /// so this measures the tag and not the loop.
    #[test]
    fn fire_over_netherrack_is_eternal_and_over_stone_is_not() {
        for (fill, eternal) in [("minecraft:netherrack", true), ("minecraft:stone", false)] {
            let rig = Rig::with_floor(fill, 0);
            let pos = BlockPos::new(2, 1, 2);
            fire_at(&rig, pos, 0);
            let mut r = rng();
            let mut survived = 0;
            for tick in 0..200u64 {
                let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
                let mut changes = Vec::new();
                run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, tick, &mut r, &mut changes);
                if is_fire(&rig.block_state(pos.x, pos.y, pos.z)) {
                    survived += 1;
                } else {
                    break;
                }
            }
            if eternal {
                assert_eq!(survived, 200, "{fill}: fire must never go out");
            } else {
                assert!(survived < 200, "{fill}: fire must eventually go out");
            }
        }
    }

    /// The burn-out rate at the cell **below** a fresh fire, predicted from
    /// vanilla's constants and the committed burn-odds table rather than observed:
    /// `checkBurnOut(below, 250)` hits when `nextInt(250) < burnOdds`, and oak
    /// planks' burn odds are `20`, so `20/250 = 0.08`. On a hit the follow-up
    /// `nextInt(age + 10) < 5` is `nextInt(10) < 5 = 0.5` at age 0, and it decides
    /// *fire* versus *air* — so the plank becomes air `0.04` of the time and fire
    /// the other `0.04`.
    ///
    /// Both halves are asserted, which is what makes this a magnitude gate: a port
    /// that removed the block on every hit would land on 0.08/0.00 and pass any
    /// "the plank burns" assertion.
    #[test]
    fn the_burn_out_rate_below_a_fire_matches_the_predicted_odds() {
        const TRIALS: usize = 20_000;
        assert_eq!(
            block_blast::blast("minecraft:oak_planks").unwrap().burn_odds,
            20,
            "the prediction below is derived from this"
        );
        let mut r = rng();
        let mut to_air = 0usize;
        let mut to_fire = 0usize;
        for _ in 0..TRIALS {
            // A fresh two-cell scene per trial, so each trial is an independent
            // sample of one tick's behaviour rather than a walk down one history.
            let rig = Rig::new();
            let pos = BlockPos::new(0, 100, 0);
            rig.set_block(0, 99, 0, "minecraft:oak_planks");
            fire_at(&rig, pos, 0);
            let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            let mut changes = Vec::new();
            run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, 0, &mut r, &mut changes);
            match rig.block_state(0, 99, 0).as_str() {
                crate::chunk::AIR => to_air += 1,
                s if is_fire(s) => to_fire += 1,
                _ => {}
            }
        }
        let air_rate = to_air as f64 / TRIALS as f64;
        let fire_rate = to_fire as f64 / TRIALS as f64;
        assert!(
            (air_rate - 0.04).abs() < 0.008,
            "predicted 0.04 of planks below a fire burn to air, measured {air_rate}"
        );
        assert!(
            (fire_rate - 0.04).abs() < 0.008,
            "predicted 0.04 become fire instead, measured {fire_rate}"
        );
    }

    /// The spread rate onto one named horizontal candidate, predicted the same
    /// way: the candidate is air with oak planks below it, so
    /// `ignite_odds_at == 5`, `spread_odds(5, 0, 2, false) == 1` and `rate == 100`,
    /// giving `nextInt(100) <= 1` — **exactly 2 in 100** per tick.
    ///
    /// Short grass is the second arm: `ignite_odds == 60` gives
    /// `spread_odds == 3`, so `4 in 100`, twice the planks' rate. Predicting both
    /// and requiring the measurement to separate them is what rules out a port
    /// that ignored `igniteOdds` entirely.
    #[test]
    fn the_spread_rate_onto_a_named_candidate_matches_the_predicted_odds() {
        const TRIALS: usize = 20_000;
        let mut r = rng();
        let mut measured = Vec::new();
        for (fill, predicted) in [("minecraft:oak_planks", 0.02), ("minecraft:short_grass", 0.04)] {
            let mut caught = 0usize;
            for _ in 0..TRIALS {
                let rig = Rig::new();
                let pos = BlockPos::new(0, 100, 0);
                // The fire's own support, and the candidate's, are the material
                // under test; nothing else is flammable, so exactly one candidate
                // has positive odds.
                rig.set_block(0, 99, 0, fill);
                rig.set_block(1, 99, 0, fill);
                fire_at(&rig, pos, 0);
                let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
                let mut changes = Vec::new();
                run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, 0, &mut r, &mut changes);
                if is_fire(&rig.block_state(1, 100, 0)) {
                    caught += 1;
                }
            }
            let rate = caught as f64 / TRIALS as f64;
            assert!(
                (rate - predicted).abs() < 0.006,
                "{fill}: predicted {predicted} spread rate, measured {rate}"
            );
            measured.push(rate);
        }
        assert!(
            measured[1] > measured[0] * 1.5,
            "short grass must spread markedly faster than planks: {measured:?}"
        );
    }

    /// The loop really closes: a fire on a plank floor left to run reaches cells it
    /// did not start on, and consumes planks. A lower bound rather than a rate —
    /// the two gates above own the rates; this one owns "the scheduled tick, the
    /// reschedule and the spread are actually wired to each other".
    ///
    /// The bound is still derived: about `8000 / 35` fire ticks happen, each with 8
    /// horizontal candidates at the 2-in-100 rate above, so tens of ignitions are
    /// expected and `>= 2 distinct cells` cannot be luck.
    #[test]
    fn fire_spreads_across_a_plank_floor_over_time() {
        let rig = Rig::with_floor("minecraft:oak_planks", 0);
        let pos = BlockPos::new(0, 1, 0);
        fire_at(&rig, pos, 0);
        let mut r = rng();
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        queue.schedule((pos.x, pos.y, pos.z), TICK_FIRE.to_owned(), 0, TickPriority::Normal);
        let mut ever_burnt: std::collections::BTreeSet<(i32, i32, i32)> = Default::default();
        let mut ever_lit: std::collections::BTreeSet<(i32, i32, i32)> = Default::default();
        for tick in 0..8000u64 {
            for entry in queue.drain_due(tick, usize::MAX) {
                let mut changes = Vec::new();
                run_scheduled_tick(
                    &rig,
                    FireEnv::OVERWORLD,
                    BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2),
                    &mut queue,
                    tick,
                    &mut r,
                    &mut changes,
                );
                for (at, state) in changes {
                    if is_fire(&state) {
                        ever_lit.insert((at.x, at.y, at.z));
                    } else if state == crate::chunk::AIR {
                        ever_burnt.insert((at.x, at.y, at.z));
                    }
                }
            }
        }
        assert!(
            ever_lit.len() >= 2,
            "fire must have lit cells beyond its origin; lit={ever_lit:?}"
        );
        assert!(
            !ever_burnt.is_empty(),
            "checkBurnOut must have consumed at least one plank"
        );
    }

    /// Negative control for the spread gate: the identical scene with
    /// `spread_allowed = false` (vanilla's `fire_spread_radius_around_player` of
    /// `-1` with no player near) leaves the floor untouched, and consumes exactly
    /// one draw per tick — the reschedule.
    #[test]
    fn a_frozen_fire_neither_spreads_nor_burns_and_draws_once() {
        let env = FireEnv {
            spread_allowed: false,
            ..FireEnv::OVERWORLD
        };
        let rig = Rig::with_floor("minecraft:oak_planks", 0);
        let pos = BlockPos::new(0, 1, 0);
        fire_at(&rig, pos, 0);
        let mut r = rng();
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        for tick in 0..50u64 {
            let mut changes = Vec::new();
            run_scheduled_tick(&rig, env, pos, &mut queue, tick, &mut r, &mut changes);
            assert!(changes.is_empty(), "a frozen fire changes nothing");
            queue.drain_due(u64::MAX, usize::MAX);
        }
        let mut reference = rng();
        for _ in 0..50 {
            reference.next_int(TICK_DELAY_JITTER);
        }
        assert_eq!(
            reference.next_int(1_000_000),
            r.next_int(1_000_000),
            "a frozen fire draws exactly one value per tick"
        );
        for z in -2..=2 {
            for x in -2..=2 {
                assert_eq!(rig.block_state(x, 0, z), "minecraft:oak_planks");
            }
        }
    }

    /// `getStateForPlacement`'s two branches. Over a sturdy floor the fire is the
    /// plain default; in mid-air beside a plank wall it connects to that face and
    /// no other.
    #[test]
    fn placement_state_connects_only_to_burnable_faces() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let over_stone = state_for_placement(&rig, FireEnv::OVERWORLD, BlockPos::new(2, 1, 2));
        assert_eq!(
            over_stone,
            format!("{FIRE}[age=0,east=false,north=false,south=false,up=false,west=false]")
        );

        let rig = Rig::new();
        rig.set_block(3, 100, 2, "minecraft:oak_planks");
        let mid_air = state_for_placement(&rig, FireEnv::OVERWORLD, BlockPos::new(2, 100, 2));
        assert_eq!(
            mid_air,
            format!("{FIRE}[age=0,east=true,north=false,south=false,up=false,west=false]")
        );
    }

    /// Soul sand and soul soil produce soul fire, and nothing else does.
    #[test]
    fn soul_fire_base_blocks_produce_soul_fire() {
        for base in ["minecraft:soul_sand", "minecraft:soul_soil"] {
            let rig = Rig::new();
            rig.set_block(2, 99, 2, base);
            assert_eq!(state_at(&rig, FireEnv::OVERWORLD, BlockPos::new(2, 100, 2)), SOUL_FIRE);
        }
        let rig = Rig::with_floor("minecraft:stone", 0);
        assert!(is_ordinary_fire(&state_at(
            &rig,
            FireEnv::OVERWORLD,
            BlockPos::new(2, 1, 2)
        )));
    }

    /// The floor hazard: a fire on the lowest addressable row reads `min_y - 1`
    /// for its support and every burn-out check reads one below that. Without
    /// [`block_at`]'s guard this panics on the tick thread.
    #[test]
    fn fire_on_the_world_floor_does_not_panic() {
        let rig = Rig::new();
        for z in -2..=2 {
            for x in -2..=2 {
                rig.set_block(x, MIN_Y, z, "minecraft:netherrack");
            }
        }
        let pos = BlockPos::new(0, MIN_Y + 1, 0);
        fire_at(&rig, pos, 0);
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut r = rng();
        let mut changes = Vec::new();
        run_scheduled_tick(&rig, FireEnv::OVERWORLD, pos, &mut queue, 0, &mut r, &mut changes);
        assert!(is_fire(&rig.block_state(pos.x, pos.y, pos.z)));
    }

    /// A fire below the world's lowest row is a no-op rather than a panic — the
    /// stale-tick path.
    #[test]
    fn a_tick_below_build_height_is_a_no_op() {
        let rig = Rig::new();
        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let mut r = rng();
        let mut changes = Vec::new();
        run_scheduled_tick(
            &rig,
            FireEnv::OVERWORLD,
            BlockPos::new(0, MIN_Y - 1, 0),
            &mut queue,
            0,
            &mut r,
            &mut changes,
        );
        assert!(changes.is_empty());
        assert!(queue.is_empty(), "a stale tick must not reschedule");
        let mut reference = rng();
        assert_eq!(reference.next_int(1_000_000), r.next_int(1_000_000), "and must draw nothing");
    }

    /// `ticks_after_edit` covers the edited cell alone, at the fixed base delay.
    #[test]
    fn ticks_after_edit_covers_only_the_edited_cell() {
        let pending = ticks_after_edit(BlockPos::new(4, 5, 6));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].pos, (4, 5, 6));
        assert_eq!(pending[0].kind, TICK_FIRE);
        assert_eq!(pending[0].trigger_tick, TICK_DELAY_BASE);
    }

    /// A waterlogged flammable block cannot catch, which is the one state-level
    /// rule the block-keyed odds table cannot express on its own.
    #[test]
    fn a_waterlogged_fence_cannot_catch_fire() {
        assert!(can_burn("minecraft:oak_fence[waterlogged=false]"));
        assert!(!can_burn("minecraft:oak_fence[waterlogged=true]"));
    }

    /// Determinism: two independently built runs from one seed agree cell for
    /// cell. Two fresh generators and two fresh worlds, not one queried twice.
    #[test]
    fn two_independent_fire_runs_from_one_seed_agree() {
        let build = || {
            let rig = Rig::with_floor("minecraft:oak_planks", 0);
            let pos = BlockPos::new(0, 1, 0);
            fire_at(&rig, pos, 0);
            let mut r = rng();
            let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
            queue.schedule((pos.x, pos.y, pos.z), TICK_FIRE.to_owned(), 0, TickPriority::Normal);
            for tick in 0..200u64 {
                for entry in queue.drain_due(tick, usize::MAX) {
                    let mut changes = Vec::new();
                    run_scheduled_tick(
                        &rig,
                        FireEnv::OVERWORLD,
                        BlockPos::new(entry.pos.0, entry.pos.1, entry.pos.2),
                        &mut queue,
                        tick,
                        &mut r,
                        &mut changes,
                    );
                }
            }
            let mut snapshot = Vec::new();
            for z in -4..=4 {
                for x in -4..=4 {
                    for y in 0..=2 {
                        snapshot.push(rig.block_state(x, y, z));
                    }
                }
            }
            snapshot
        };
        assert_eq!(build(), build());
    }
}
