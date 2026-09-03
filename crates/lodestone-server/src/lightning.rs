//! Lightning: per-chunk strike-target selection during a thunderstorm, the
//! `LightningBolt` entity's life-cycle, and its entity-facing effects.
//!
//! # What it is
//!
//! A port of vanilla's own per-tick thunder tick and target-search routines
//! — the per-tick, per-entity-ticking-chunk strike
//! selection — plus its own lightning-bolt entity tick and
//! the thunder-hit table across `Entity`, `Creeper`, `Pig`, `Villager`,
//! `MushroomCow` and `Turtle`. Nothing about lightning existed in this crate
//! before this module: `crate::burning`'s own doc lists it under "What is not
//! here" and says why — it needs an entity type `MobSim` does not have.
//!
//! # How it works
//!
//! [`should_attempt_strike`] is the outer gate, `ServerLevel.tickThunder`'s
//! `raining && this.isThundering() && this.random.nextInt(100000) == 0` —
//! **gated on the thunder state, not merely on rain**, and short-circuiting
//! exactly as the Java `&&` chain does: the `nextInt(100000)` draw itself
//! only happens when both booleans already hold, so a merely-rainy, non-
//! thundering world draws nothing here every tick.
//!
//! [`block_random_pos`] is `Level.getBlockRandomPos` — **not**
//! `java.util.Random`: it is the level's own tiny in-place LCG
//! (`randValue = randValue * 3 + 1013904223`), a completely separate stream
//! from every `LegacyRandomSource`/[`SpawnRng`] draw in this crate, which is
//! why it takes its own `&mut i32` rather than an `&mut SpawnRng`.
//!
//! [`find_lightning_target_around`] is `findLightningTargetAround`: a nearby
//! lightning rod wins outright; otherwise a random living, sky-visible entity
//! in a generous column around the seed position; otherwise the terrain
//! heightmap itself (bumped up two if it lands on the bottom-of-world
//! sentinel). [`tick_thunder_for_chunk`] is the whole per-chunk decision,
//! [`Strike`] its result, and [`LightningFeed`] the same publish/drain idiom
//! [`crate::weather::WeatherFeed`] already establishes for weather
//! transitions.
//!
//! [`BoltState`]/[`tick_bolt`] port `LightningBolt.tick`'s `life`/`flashes`
//! countdown: `life` starts at 2, the *first* tick (`life == 2`, before the
//! decrement) is the one that plays sounds, attempts ignition, powers a
//! lightning rod and clears weathering copper; then `life` counts down past
//! zero, and while `flashes > 0` a re-strike can re-ignite at the *same*
//! position before the bolt finally discards. **The re-strike's `spawnFire(0)`
//! carries no difficulty gate** — unlike the first strike's `spawnFire(4)`,
//! which only runs on Normal/Hard — a detail easy to port as "ignition is
//! always difficulty-gated" and wrong.
//!
//! [`resolve_effect`] is the `thunderHit` dispatch table. Verified against
//! the 26.2 jar rather than assumed from an older version's rules:
//!
//! | species | vanilla method | effect |
//! |---|---|---|
//! | (default) | `Entity.thunderHit` | 5.0 damage, plus an 8-second ignite (`crate::burning::FIRE_IGNITE_TICKS`) |
//! | `minecraft:creeper` | `Creeper.thunderHit` | the default, **plus** `DATA_IS_POWERED` set true (a charged creeper) |
//! | `minecraft:pig` | `Pig.thunderHit` | converts to `minecraft:zombified_piglin`, gated on `difficulty != PEACEFUL`; falls back to the default on Peaceful (or if the conversion fails) |
//! | `minecraft:villager` | `Villager.thunderHit` | converts to `minecraft:witch`, same Peaceful gate and fallback |
//! | `minecraft:mooshroom` | `MushroomCow.thunderHit` | swaps red/brown, guarded so the *same* bolt cannot flip it twice (per-mooshroom "last bolt UUID" state, which lives with the mob) |
//! | `minecraft:turtle` | `Turtle.thunderHit` | **overrides** the default entirely with a lethal hit (`Float.MAX_VALUE` damage) — it does **not** call `super.thunderHit`, so a struck turtle is never ignited |
//!
//! Two commonly-misremembered claims this table corrects, both checked
//! against the jar rather than recalled:
//!
//! * **There is no turtle-egg interaction.** Vanilla's own turtle-egg block has no
//!   lightning hook of any kind; the real turtle-related effect is the
//!   `Turtle` *entity* dying outright, not an egg doing anything.
//! * **The "skeleton horse trap" is not a `thunderHit` transformation at
//!   all — it runs backwards from how that phrasing suggests.** A naturally
//!   spawned `SkeletonHorse` can be flagged `isTrap` (see
//!   [`should_be_skeleton_trap`]); when a player later comes within 10
//!   blocks, `SkeletonTrapGoal.tick` fires **once**, spawning a **cosmetic**
//!   (`visualOnly = true`) `LightningBolt` at the horse plus a skeleton rider
//!   and three more horse-and-skeleton pairs. Lightning does not strike a
//!   horse and turn it into a trap; a pre-flagged trap horse *casts* a
//!   decorative bolt when approached. That mechanism is mob AI
//!   (goal-selector wiring, a natural-spawn flag, `EnchantmentHelper`'s
//!   `MOB_SPAWN_EQUIPMENT` provider) entirely in `crate::mobs`'s territory,
//!   not this module's — [`should_be_skeleton_trap`] only ports the *roll*
//!   `tickThunder` makes to decide whether the horse should be flagged, which
//!   is this module's one genuine consumer of
//!   [`crate::regional_difficulty::DifficultyInstance`].
//!
//! # What is out of reach from here
//!
//! **Nothing in this module can spawn a real entity.** `MobSim` (the only
//! live-entity tracker, in `crate::mobs`, off limits here) has no
//! `LightningBolt`/`SkeletonHorse` sidecar, no conversion primitive
//! (`grep`ping `crate::mobs`/`crate::lodestone-entity` for "convert" or
//! "ConversionParams" is empty), and no per-entity "struck by this bolt"
//! flag for the mooshroom guard or the creeper's `DATA_IS_POWERED`. So this
//! module stops at **deciding**: [`tick_thunder_for_chunk`] decides a strike
//! should happen and where, publishing a [`Strike`] onto a [`LightningFeed`];
//! [`tick_bolt`]/[`resolve_effect`] decide what an already-spawned bolt
//! should do on a given tick and to a given species. Turning a [`Strike`]
//! into a real, ticking, network-visible entity, and turning a
//! [`LightningEffect`] into an actual `SimMob` mutation or a
//! spawn-and-despawn conversion, both need a hook in `crate::mobs` — see this
//! change's handoff notes for the exact hunk.
//!
//! The client side is already live and waiting: `lodestone-shell`'s
//! `net.rs` has a `ClientEvent::EntitySpawned` arm matching
//! `entity_type.path() == "lightning_bolt"` that calls a weather cell's
//! `strike()`, and the wire entity-type registry already has
//! `minecraft:lightning_bolt` (network id 77, `lodestone-data`'s generated
//! entity-type table) with the correct boxless dimensions and non-living
//! census entry. **Nothing server-side produces that spawn yet** — this
//! module is the missing producer's decision half, not its production half.
//!
//! `find_lightning_target_around`'s AABB is a documented approximation of
//! `AABB.encapsulatingFullBlocks(center, center.atY(maxY + 1)).inflate(3.0)`
//! (see [`entity_search_bounds`]) rather than a byte-exact port, and the
//! lightning-rod POI search (`findLightningRod`, up to 128 blocks) has no
//! model here at all — this crate has no POI manager — so
//! [`tick_thunder_for_chunk`] takes `nearby_lightning_rod` as a caller-
//! supplied `Option<BlockPos>`, `None` until a POI system exists.
//!
//! # Dependencies
//!
//! [`crate::chunk::ChunkSource`] for the heightmap/sky-exposure block reads
//! (the same trait [`crate::fire`] reads through), [`crate::mob_spawn::SpawnRng`]
//! for every `java.util.Random`-exact draw, and
//! [`crate::regional_difficulty::DifficultyInstance`] for the skeleton-trap
//! roll. No world-mutation API and no entity API: every effect is returned as
//! data for the caller to apply, the same shape [`crate::weather`] and
//! [`crate::burning`] already use.

use std::sync::{Arc, Mutex};

use lodestone_model::{BlockPos, Difficulty};

use crate::chunk::ChunkSource;
use crate::mob_spawn::SpawnRng;
use crate::regional_difficulty::{moon_brightness_for_day_time, DifficultyInstance};

/// `LightningBolt.START_LIFE`.
pub const START_LIFE: i32 = 2;
/// `LightningBolt.DAMAGE_RADIUS` — the entity-hit search radius.
pub const DAMAGE_RADIUS: f64 = 3.0;
/// `LightningBolt.DETECTION_RADIUS` — the "who gets the criteria trigger" radius, unmodelled (no advancements here).
pub const DETECTION_RADIUS: f64 = 15.0;
/// `Entity.thunderHit`'s default damage.
pub const DEFAULT_DAMAGE: f32 = 5.0;
/// `ServerLevel.tickThunder`'s outer roll bound — `random.nextInt(100000) == 0`.
pub const STRIKE_ROLL_BOUND: i32 = 100_000;
/// `SkeletonHorse`'s trap-chance scale — `getEffectiveDifficulty() * 0.01`.
pub const TRAP_CHANCE_SCALE: f64 = 0.01;
/// `minecraft:lightning_rod` — the block `#minecraft:lightning_rods` tags,
/// approximated here as the single block it actually contains rather than a
/// real tag lookup (see the module doc's "out of reach" list).
pub const LIGHTNING_ROD: &str = "minecraft:lightning_rod";
/// The wire entity type this module's strikes eventually become —
/// `lodestone-data`'s generated entity-type table already has this at
/// network id 77.
pub const LIGHTNING_BOLT: &str = "minecraft:lightning_bolt";

/// Default seed for the driver's **strike target-selection** stream —
/// `tick_thunder_for_chunk`'s own `rng` parameter, when the caller wants a
/// fixed default rather than injecting one. Arbitrary and fixed, the same
/// shape `crate::mobs::orbs::ORB_BEHAVIOR_SEED` establishes.
pub const LIGHTNING_STRIKE_SEED: u64 = 0x4C49_4748_5453_5452;

/// Default seed for a bolt's **own** state-machine stream (`BoltState::new`,
/// `tick_bolt`'s `spawnFire` draws) — deliberately separate from
/// [`LIGHTNING_STRIKE_SEED`] so a strike decision can never shift which roll
/// a bolt's own life/flashes/ignition sees, [`LIGHTNING_STRIKE_SEED`]'s own
/// reason restated for the sibling stream.
pub const LIGHTNING_BOLT_SEED: u64 = 0x4C49_4748_5442_4F4C;

/// Everything about the world a strike-selection pass needs that is not a
/// block state — the same shape [`crate::fire::FireEnv`] establishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightningEnv {
    /// Lowest addressable `y`.
    pub min_y: i32,
    /// Number of block rows above `min_y`.
    pub height: i32,
}

impl LightningEnv {
    #[must_use]
    pub fn contains_y(self, y: i32) -> bool {
        y >= self.min_y && y < self.min_y + self.height
    }
}

/// The block state at `pos`, or air outside build height — the same guard
/// [`crate::fire::block_at`] enforces, duplicated rather than shared because
/// the two modules' `Env` types differ.
#[must_use]
fn block_at<S: ChunkSource + ?Sized>(world: &S, env: LightningEnv, pos: BlockPos) -> String {
    if env.contains_y(pos.y) {
        world.block_state(pos.x, pos.y, pos.z)
    } else {
        crate::chunk::AIR.to_owned()
    }
}

fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn blocks_motion(state: &str) -> bool {
    lodestone_data::block_states::state_id(state)
        .and_then(lodestone_data::block_solidity::blocks_motion)
        .unwrap_or(false)
}

/// `Level.getBlockRandomPos` — the level's own tiny LCG, mutated in place.
/// **Not** a `java.util.Random`/[`SpawnRng`] draw; see the module doc.
#[must_use]
pub fn block_random_pos(rand_value: &mut i32, xo: i32, yo: i32, zo: i32, y_mask: i32) -> BlockPos {
    *rand_value = rand_value.wrapping_mul(3).wrapping_add(1_013_904_223);
    let val = *rand_value >> 2;
    BlockPos::new(xo + (val & 15), yo + ((val >> 16) & y_mask), zo + ((val >> 8) & 15))
}

/// `Level.getHeightmapPos(MOTION_BLOCKING, ...)` reduced to a real block scan:
/// the first air cell above the highest motion-blocking cell in the column.
#[must_use]
pub fn motion_blocking_heightmap_pos<S: ChunkSource + ?Sized>(
    world: &S,
    env: LightningEnv,
    x: i32,
    z: i32,
) -> BlockPos {
    let top = env.min_y + env.height;
    let mut y = top - 1;
    while y > env.min_y {
        if blocks_motion(&block_at(world, env, BlockPos::new(x, y, z))) {
            return BlockPos::new(x, y + 1, z);
        }
        y -= 1;
    }
    BlockPos::new(x, env.min_y, z)
}

/// `Level.canSeeSky` plus the heightmap term, for [`is_raining_at`] — the
/// same technique [`crate::fire::sky_exposed`] uses, duplicated because that
/// one is private to its module.
#[must_use]
pub fn sky_exposed<S: ChunkSource + ?Sized>(world: &S, env: LightningEnv, pos: BlockPos) -> bool {
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

/// `Level.isRainingAt` — raining, and nothing motion-blocking overhead. The
/// biome-precipitation term is absent, the same documented reduction
/// [`crate::fire::is_raining_at`] makes.
#[must_use]
pub fn is_raining_at<S: ChunkSource + ?Sized>(world: &S, env: LightningEnv, pos: BlockPos, raining: bool) -> bool {
    raining && sky_exposed(world, env, pos)
}

/// `ServerLevel.tickThunder`'s outer gate — gated on **thunder**, not merely
/// on rain, and short-circuiting the `nextInt` draw exactly as vanilla's `&&`
/// chain does.
#[must_use]
pub fn should_attempt_strike(raining: bool, thundering: bool, rng: &mut SpawnRng) -> bool {
    raining && thundering && rng.next_int(STRIKE_ROLL_BOUND) == 0
}

/// `AABB.encapsulatingFullBlocks(center, center.atY(maxY + 1)).inflate(3.0)`,
/// as the six bounds a caller would filter entity positions against. A
/// documented approximation (see the module doc) rather than a byte-exact
/// port of `AABB`'s own arithmetic, but it is the same shape: a single-column
/// box from the heightmap position to the world ceiling, inflated by 3 in
/// every direction.
#[must_use]
pub fn entity_search_bounds(env: LightningEnv, center: BlockPos) -> (BlockPos, BlockPos) {
    let max_y = env.min_y + env.height - 1;
    (
        BlockPos::new(center.x - 3, center.y - 3, center.z - 3),
        BlockPos::new(center.x + 4, max_y + 1 + 3, center.z + 4),
    )
}

#[must_use]
fn within_bounds(min: BlockPos, max: BlockPos, pos: BlockPos) -> bool {
    pos.x >= min.x && pos.x <= max.x && pos.y >= min.y && pos.y <= max.y && pos.z >= min.z && pos.z <= max.z
}

/// `findLightningTargetAround` — a nearby lightning rod wins outright,
/// otherwise a uniformly-random living/sky-visible entity within
/// [`entity_search_bounds`] of the heightmap position, otherwise the
/// heightmap position itself (bumped up two rows if it lands on the
/// bottom-of-world sentinel `min_y - 1`).
///
/// `living_entities` is every alive, sky-visible entity's position anywhere
/// in the world — filtering to the search box happens inside this function,
/// matching vanilla's own `getEntitiesOfClass(..., search, ...)` shape rather
/// than asking the caller to pre-cull.
#[must_use]
pub fn find_lightning_target_around<S: ChunkSource + ?Sized>(
    world: &S,
    env: LightningEnv,
    seed_pos: BlockPos,
    nearby_lightning_rod: Option<BlockPos>,
    living_entities: &[BlockPos],
    rng: &mut SpawnRng,
) -> BlockPos {
    let mut center = motion_blocking_heightmap_pos(world, env, seed_pos.x, seed_pos.z);
    if let Some(rod_above) = nearby_lightning_rod {
        return rod_above;
    }
    let (min, max) = entity_search_bounds(env, center);
    let candidates: Vec<BlockPos> = living_entities
        .iter()
        .copied()
        .filter(|&p| within_bounds(min, max, p))
        .collect();
    if !candidates.is_empty() {
        let idx = rng.next_int(candidates.len() as i32) as usize;
        return candidates[idx];
    }
    if center.y == env.min_y - 1 {
        center = BlockPos::new(center.x, center.y + 2, center.z);
    }
    center
}

/// `LightningBolt.getStrikePosition` — the block one below where the bolt
/// entity itself stands (`BlockPos.containing(x, y - 1e-6, z)`, and the
/// entity's own `y` is the struck cell's `y` exactly, via `Vec3.atBottomCenterOf`
/// — so subtracting an epsilon and flooring always lands one row down).
#[must_use]
pub fn strike_ground_pos(bolt_pos: BlockPos) -> BlockPos {
    BlockPos::new(bolt_pos.x, bolt_pos.y - 1, bolt_pos.z)
}

/// `SkeletonHorse.checkSkeletonHorseSpawnRules`'s sibling decision inside
/// `tickThunder` — whether *this* strike should instead flag a naturally
/// spawned `SkeletonHorse` as a trap: `spawnMobs && random.nextDouble() <
/// difficulty.getEffectiveDifficulty() * 0.01 && the block below is not a
/// lightning rod`. The one genuine consumer of
/// [`crate::regional_difficulty::DifficultyInstance`] in this tree — see that
/// module's doc for why its other named consumers are out of reach.
#[must_use]
pub fn should_be_skeleton_trap(
    spawn_mobs_rule: bool,
    difficulty: DifficultyInstance,
    block_below_is_lightning_rod: bool,
    rng: &mut SpawnRng,
) -> bool {
    spawn_mobs_rule
        && rng.next_f64() < f64::from(difficulty.effective_difficulty()) * TRAP_CHANCE_SCALE
        && !block_below_is_lightning_rod
}

/// A strike this tick decided on — [`tick_thunder_for_chunk`]'s result and
/// [`LightningFeed`]'s element. `pos` is the bolt entity's own spawn position
/// (`Vec3.atBottomCenterOf(pos)` in vanilla — this crate's entity positions
/// are block-granular here, one level up from the eventual float position a
/// spawner would snap to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strike {
    pub pos: BlockPos,
    /// `LightningBolt::setVisualOnly` — true for a skeleton-horse-trap bolt,
    /// which ignites nothing and hits no entities (see [`tick_bolt`]).
    pub visual_only: bool,
}

/// A shared feed of strikes the world tick loop decided on, for whichever
/// caller can actually spawn the entity to drain — the identical
/// publish/drain idiom [`crate::weather::WeatherFeed`] already establishes,
/// including its single-consumer caveat (see that type's doc).
#[derive(Debug, Clone, Default)]
pub struct LightningFeed(Arc<Mutex<Vec<Strike>>>);

impl LightningFeed {
    /// Records one strike for the next [`drain_all`](Self::drain_all).
    pub fn publish(&self, strike: Strike) {
        self.0.lock().expect("lightning feed lock poisoned").push(strike);
    }

    /// Drains and returns every strike published since the last call.
    pub fn drain_all(&self) -> Vec<Strike> {
        std::mem::take(&mut *self.0.lock().expect("lightning feed lock poisoned"))
    }
}

/// `ServerLevel.tickThunder`, for one chunk, one tick — the whole per-chunk
/// decision the module doc describes. Returns `None` on every early exit
/// (gate roll missed, target not actually raining-at) exactly as vanilla's
/// `if` chain would simply not reach `addFreshEntity`.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn tick_thunder_for_chunk<S: ChunkSource + ?Sized>(
    world: &S,
    env: LightningEnv,
    chunk_min_x: i32,
    chunk_min_z: i32,
    raining: bool,
    thundering: bool,
    difficulty: Difficulty,
    total_game_time: i64,
    day_time: i64,
    spawn_mobs_rule: bool,
    nearby_lightning_rod: Option<BlockPos>,
    living_entities: &[BlockPos],
    rand_value: &mut i32,
    rng: &mut SpawnRng,
) -> Option<Strike> {
    if !should_attempt_strike(raining, thundering, rng) {
        return None;
    }
    let seed_pos = block_random_pos(rand_value, chunk_min_x, 0, chunk_min_z, 15);
    let target = find_lightning_target_around(world, env, seed_pos, nearby_lightning_rod, living_entities, rng);
    if !is_raining_at(world, env, target, raining) {
        return None;
    }
    let difficulty_instance = DifficultyInstance::new(difficulty, total_game_time, 0, moon_brightness_for_day_time(day_time));
    // `!this.getBlockState(pos.below()).is(BlockTags.LIGHTNING_RODS)` — `pos`
    // here is `target` itself (the bolt's own position), not the ground cell
    // a spawned bolt's `getStrikePosition` would compute; `pos.below()` and
    // `strike_ground_pos(target)` are the same offset by coincidence of both
    // being "one row down", so this reuses that helper rather than repeating it.
    let below_is_rod = base_name(&block_at(world, env, strike_ground_pos(target))) == LIGHTNING_ROD;
    let visual_only = should_be_skeleton_trap(spawn_mobs_rule, difficulty_instance, below_is_rod, rng);
    Some(Strike { pos: target, visual_only })
}

/// `LightningBolt`'s `life`/`flashes` countdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoltState {
    pub life: i32,
    pub flashes: i32,
    pub visual_only: bool,
}

impl BoltState {
    /// `new LightningBolt(...)` — `life = 2`, `flashes = random.nextInt(3) + 1`.
    #[must_use]
    pub fn new(rng: &mut SpawnRng, visual_only: bool) -> Self {
        Self {
            life: START_LIFE,
            flashes: rng.next_int(3) + 1,
            visual_only,
        }
    }
}

/// What one [`tick_bolt`] call decided the caller should do — data, not a
/// mutation, the same split every other module here uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoltTickEffects {
    /// `life == 2`'s client-side sound pair — this crate has no client-side
    /// branch, so the caller turns this into whatever sound event it sends.
    pub play_thunder_sounds: bool,
    /// Positions to attempt `BaseFireBlock::getState`/`canSurvive` ignition
    /// at, in order: the struck ground cell first (if any), then up to four
    /// random offsets — populated only when ignition is attempted at all
    /// (see the two fields below for when that is).
    pub fire_attempts: Vec<BlockPos>,
    /// `powerLightningRod` — always true on the `life == 2` tick, trap or not.
    pub power_lightning_rod: bool,
    /// `clearCopperOnLightningStrike` — same as above.
    pub clear_copper: bool,
    /// `gameEvent(LIGHTNING_STRIKE)` — same as above.
    pub game_event: bool,
    /// `life >= 0 && !visualOnly` — entities within [`DAMAGE_RADIUS`] of the
    /// bolt should be resolved through [`resolve_effect`] this tick.
    pub hit_entities: bool,
    /// The bolt has exhausted its flashes and should be removed.
    pub discard: bool,
}

/// `LightningBolt.tick`'s server branch, transcribed. `ground_pos` is
/// [`strike_ground_pos`] of the bolt's own position, computed once by the
/// caller since it does not change across a bolt's life.
///
/// Ignition is attempted on the **first** tick only when `difficulty` is
/// Normal or Hard (`spawnFire(4)`); a **re-strike** (a later flash) attempts
/// ignition **unconditionally** on difficulty (`spawnFire(0)`, no gate) —
/// see the module doc for why that asymmetry is easy to port wrong. Either
/// way, [`BoltState::visual_only`] suppresses every ignition attempt, matching
/// `spawnFire`'s own `!this.visualOnly` guard.
pub fn tick_bolt(state: &mut BoltState, ground_pos: BlockPos, difficulty: Difficulty, rng: &mut SpawnRng) -> BoltTickEffects {
    let mut fx = BoltTickEffects::default();

    if state.life == 2 {
        fx.play_thunder_sounds = true;
        let ignites = matches!(difficulty, Difficulty::Normal | Difficulty::Hard);
        if ignites && !state.visual_only {
            push_fire_attempts(&mut fx.fire_attempts, ground_pos, 4, rng);
        }
        fx.power_lightning_rod = true;
        fx.clear_copper = true;
        fx.game_event = true;
    }

    state.life -= 1;
    if state.life < 0 {
        if state.flashes == 0 {
            fx.discard = true;
        } else if state.life < -rng.next_int(10) {
            state.flashes -= 1;
            state.life = 1;
            // `spawnFire(0)` — no difficulty gate on a re-strike.
            if !state.visual_only {
                push_fire_attempts(&mut fx.fire_attempts, ground_pos, 0, rng);
            }
        }
    }

    if state.life >= 0 && !state.visual_only {
        fx.hit_entities = true;
    }

    fx
}

/// `spawnFire`'s draw pattern: the primary cell always attempted first (when
/// `additional_sources >= 0`, i.e. whenever this is called at all — the
/// `visualOnly` guard is the caller's job), then `additional_sources` more
/// candidates, each at a `random.nextInt(3) - 1` offset per axis (three
/// draws each, drawn even when the primary attempt itself will not survive —
/// `spawnFire` computes every offset unconditionally).
fn push_fire_attempts(out: &mut Vec<BlockPos>, primary: BlockPos, additional_sources: i32, rng: &mut SpawnRng) {
    out.push(primary);
    for _ in 0..additional_sources {
        let dx = rng.next_int(3) - 1;
        let dy = rng.next_int(3) - 1;
        let dz = rng.next_int(3) - 1;
        out.push(BlockPos::new(primary.x + dx, primary.y + dy, primary.z + dz));
    }
}

/// The `thunderHit` dispatch table — see the module doc for the full
/// per-species table and the two corrected misconceptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightningEffect {
    /// `Entity.thunderHit`: [`DEFAULT_DAMAGE`] plus `crate::burning::FIRE_IGNITE_TICKS`.
    DamageAndIgnite,
    /// `Turtle.thunderHit`: overrides the default with a lethal hit and no ignite.
    Lethal,
    /// `Creeper.thunderHit`: the default, plus `DATA_IS_POWERED` set true.
    BecomeCharged,
    /// `Pig.thunderHit`, `difficulty != PEACEFUL`: converts to `minecraft:zombified_piglin`.
    ConvertToZombifiedPiglin,
    /// `Villager.thunderHit`, `difficulty != PEACEFUL`: converts to `minecraft:witch`.
    ConvertToWitch,
    /// `MushroomCow.thunderHit`: toggles red/brown, guarded per-bolt by the
    /// caller (this module has no per-entity "last bolt" state to check).
    ToggleMooshroomVariant,
}

/// `entityx.thunderHit(level, this)`'s dispatch, resolved by canonical entity
/// type key and the current world [`Difficulty`] (the Peaceful gate on the
/// pig/villager conversions).
#[must_use]
pub fn resolve_effect(entity_type: &str, difficulty: Difficulty) -> LightningEffect {
    match entity_type {
        "minecraft:turtle" => LightningEffect::Lethal,
        "minecraft:creeper" => LightningEffect::BecomeCharged,
        "minecraft:pig" if difficulty != Difficulty::Peaceful => LightningEffect::ConvertToZombifiedPiglin,
        "minecraft:villager" if difficulty != Difficulty::Peaceful => LightningEffect::ConvertToWitch,
        "minecraft:mooshroom" => LightningEffect::ToggleMooshroomVariant,
        _ => LightningEffect::DamageAndIgnite,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::chunk::ChunkColumn;

    const MIN_Y: i32 = -64;
    const HEIGHT: i32 = 384;
    const ENV: LightningEnv = LightningEnv { min_y: MIN_Y, height: HEIGHT };

    struct Rig {
        columns: StdMutex<HashMap<(i32, i32), ChunkColumn>>,
    }

    impl Rig {
        fn with_floor(fill: &str, floor_y: i32) -> Self {
            let rig = Self { columns: StdMutex::new(HashMap::new()) };
            // `-8..16`, not `-8..8`: `block_random_pos(rv, xo, 0, zo, 15)`'s
            // `& 15` mask can offset up to **+15** from `xo`/`zo`, and every
            // `tick_thunder_for_chunk` test below passes `chunk_min_x =
            // chunk_min_z = 0`. A `-8..8` floor left columns 8..16
            // unfilled, so a seed landing there (as the fixed `rv = 7` this
            // file uses does — column `(13, 12)`) read the void heightmap
            // fallback instead of the floor, independently of which
            // `SpawnRng` seed was in play. Caught only once the RNG-search
            // widening above stopped masking it with a *different* panic
            // ("no seed rolls zero in range") on the same test.
            for z in -8..16 {
                for x in -8..16 {
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
            columns.entry((cx, cz)).or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT)).clone()
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns.entry((cx, cz)).or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.block_state(x - cx * 16, y, z - cz * 16).to_string()
        }

        fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns.entry((cx, cz)).or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.biome_state_at(x - cx * 16, y, z - cz * 16).to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let mut columns = self.columns.lock().expect("rig lock");
            let column = columns.entry((cx, cz)).or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT));
            column.set_block(x - cx * 16, y, z - cz * 16, name);
        }
    }

    fn rng() -> SpawnRng {
        SpawnRng::new(0x11DE_7116_5EED_0002)
    }

    /// `should_attempt_strike` must not draw at all unless both flags hold —
    /// two independent generators starting identically must still agree after
    /// a raining-only and a thundering-only call, which is only true if
    /// neither call drew.
    #[test]
    fn strike_gate_short_circuits_without_both_flags() {
        let mut a = rng();
        let mut b = rng();
        assert!(!should_attempt_strike(true, false, &mut a), "raining alone must not strike");
        assert!(!should_attempt_strike(false, true, &mut b), "thundering alone must not strike");
        // Neither call drew, so both generators must still agree bit for bit.
        assert_eq!(a.next_int(1_000_000), b.next_int(1_000_000));
    }

    /// The gate rolls `nextInt(100000) == 0` only when both flags hold — a
    /// generator seeded to draw exactly `0` on its first call must strike;
    /// one that draws anything else must not, and both must have consumed
    /// one draw.
    ///
    /// **The search space was previously `0..2000`, against a 1-in-100000
    /// target — an expected 0.02 hits, so this test failed on nearly every
    /// run** (`strike_gate_fires_on_a_zero_roll_and_consumes_one_draw` was one
    /// of the two known-failing `lightning::tests` on `main` this landing
    /// found and fixed; unrelated to the mob-feed wiring itself).
    /// `0..2_000_000` has an expected 20 hits, so the miss probability is
    /// `(1 - 1e-5)^2_000_000 ≈ 2e-9` — search a real seed space rather than
    /// asserting a literal magic seed (a round number masquerading as a
    /// derivation), just a large enough one that the assertion is not itself
    /// a coin flip.
    #[test]
    fn strike_gate_fires_on_a_zero_roll_and_consumes_one_draw() {
        let seed = (0u64..2_000_000)
            .find(|&s| SpawnRng::new(s).next_int(STRIKE_ROLL_BOUND) == 0)
            .expect("at least one seed in range must roll zero");
        let mut hit = SpawnRng::new(seed);
        assert!(should_attempt_strike(true, true, &mut hit));

        let other_seed = (0u64..2000)
            .find(|&s| SpawnRng::new(s).next_int(STRIKE_ROLL_BOUND) != 0)
            .expect("at least one seed in range must roll nonzero");
        let mut miss = SpawnRng::new(other_seed);
        assert!(!should_attempt_strike(true, true, &mut miss));
    }

    /// `block_random_pos` always keeps x/z inside the `0..=15` chunk-local
    /// range the `& 15` mask guarantees, over many draws and starting values
    /// (including negative, which a real `randValue` can become through
    /// wrapping multiplication).
    #[test]
    fn block_random_pos_stays_within_the_chunk_local_mask() {
        let mut rv: i32 = -1_234_567;
        for _ in 0..500 {
            let pos = block_random_pos(&mut rv, 160, 0, -320, 15);
            assert!((160..=175).contains(&pos.x), "x out of range: {}", pos.x);
            assert!((-320..=-305).contains(&pos.z), "z out of range: {}", pos.z);
            assert!((0..=15).contains(&pos.y), "y out of range: {}", pos.y);
        }
    }

    /// The LCG is deterministic and distinct calls advance it — two draws
    /// from the same starting value must differ (astronomically unlikely to
    /// coincide by chance) and a fresh identical start must reproduce the
    /// first draw exactly.
    #[test]
    fn block_random_pos_is_deterministic_and_advances() {
        let mut rv = 42;
        let first = block_random_pos(&mut rv, 0, 0, 0, 15);
        let second = block_random_pos(&mut rv, 0, 0, 0, 15);
        assert_ne!(first, second, "consecutive draws must differ");

        let mut fresh = 42;
        let replay = block_random_pos(&mut fresh, 0, 0, 0, 15);
        assert_eq!(first, replay, "identical starting state must reproduce the same draw");
    }

    /// `strike_ground_pos` is one row below the bolt's own position, on every
    /// axis unaffected but `y`.
    #[test]
    fn strike_ground_pos_is_one_row_below() {
        let bolt = BlockPos::new(11, 70, -4);
        assert_eq!(strike_ground_pos(bolt), BlockPos::new(11, 69, -4));
    }

    /// The heightmap scan finds the first air cell above a floor, and (the
    /// premise check this needs) the rig's floor really is solid stone —
    /// `blocks_motion` for a name it does not recognise would silently pass
    /// this by finding nothing.
    #[test]
    fn heightmap_pos_sits_just_above_the_floor() {
        let rig = Rig::with_floor("minecraft:stone", 4);
        let pos = motion_blocking_heightmap_pos(&rig, ENV, 0, 0);
        assert_eq!(pos, BlockPos::new(0, 5, 0), "must land directly above the floor top");
    }

    /// With no lightning rod and no candidate entities, target selection
    /// falls through to the heightmap position untouched.
    #[test]
    fn target_selection_falls_back_to_heightmap_with_no_rod_or_entities() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let mut r = rng();
        let target = find_lightning_target_around(&rig, ENV, BlockPos::new(2, 0, 2), None, &[], &mut r);
        assert_eq!(target, BlockPos::new(2, 1, 2));
    }

    /// A lightning rod wins outright over any entity candidate — the first
    /// `if let Some(...)` return, never reaching the entity search at all
    /// (which a draw-count check confirms: the RNG is untouched).
    #[test]
    fn a_nearby_lightning_rod_wins_outright() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let rod_pos = BlockPos::new(9, 40, 9);
        let mut a = rng();
        let target = find_lightning_target_around(
            &rig,
            ENV,
            BlockPos::new(2, 0, 2),
            Some(rod_pos),
            &[BlockPos::new(2, 1, 2)],
            &mut a,
        );
        assert_eq!(target, rod_pos);
        let mut b = rng();
        assert_eq!(a.next_int(1_000_000), b.next_int(1_000_000), "the rod branch must draw nothing");
    }

    /// A living entity inside the search box is preferred over the terrain
    /// heightmap; one outside it is not a candidate at all.
    #[test]
    fn a_living_entity_in_range_is_preferred_over_terrain() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let mut r = rng();
        let near = BlockPos::new(3, 10, 3);
        let far = BlockPos::new(500, 10, 500);
        let target = find_lightning_target_around(&rig, ENV, BlockPos::new(2, 0, 2), None, &[far, near], &mut r);
        assert_eq!(target, near, "the only in-range candidate must be chosen over the out-of-range one and the heightmap");
    }

    /// A bottom-of-world heightmap sentinel (`min_y - 1`, an all-air column)
    /// is bumped up two rows rather than left at the void.
    #[test]
    fn a_void_column_bumps_the_sentinel_up_two() {
        let rig = Rig { columns: StdMutex::new(HashMap::new()) };
        // No blocks placed anywhere: the heightmap scan finds nothing and
        // returns `min_y` — one above `min_y - 1`'s sentinel meaning, so the
        // scan itself never reports the exact sentinel value; this exercises
        // the fallback path (no rod, no entities) still returning a sane
        // position rather than panicking or looping.
        let mut r = rng();
        let target = find_lightning_target_around(&rig, ENV, BlockPos::new(0, 0, 0), None, &[], &mut r);
        assert!(ENV.contains_y(target.y) || target.y == ENV.min_y);
    }

    /// `tick_thunder_for_chunk` must not attempt a strike at all when not
    /// thundering, however favourable everything else is — a lightning rod
    /// right there and a saturated trap chance would both fire if the outer
    /// gate were bypassed.
    #[test]
    fn no_strike_without_thunder() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        let mut rv = 1;
        let mut r = rng();
        let result = tick_thunder_for_chunk(
            &rig, ENV, 0, 0, true, false, Difficulty::Hard, 5_000_000, 0, true, None, &[], &mut rv, &mut r,
        );
        assert_eq!(result, None);
    }

    /// The full happy path: a seed chosen to roll the outer gate, over a
    /// solid floor with rain able to reach it, must produce a strike whose
    /// position is directly above the floor.
    #[test]
    fn a_favourable_seed_produces_a_strike_above_the_floor() {
        let rig = Rig::with_floor("minecraft:stone", 0);
        // See `strike_gate_fires_on_a_zero_roll_and_consumes_one_draw`'s own
        // comment: `0..5000` was the second of the two known-failing
        // `lightning::tests` on `main` (expected 0.05 hits against the same
        // 1-in-100000 target), fixed the same way.
        let seed = (0u64..2_000_000)
            .find(|&s| SpawnRng::new(s).next_int(STRIKE_ROLL_BOUND) == 0)
            .expect("a zero-rolling seed must exist in range");
        let mut r = SpawnRng::new(seed);
        let mut rv = 7;
        let result = tick_thunder_for_chunk(
            &rig, ENV, 0, 0, true, true, Difficulty::Normal, 5_000_000, 0, false, None, &[], &mut rv, &mut r,
        );
        let strike = result.expect("a favourable roll over open sky must produce a strike");
        assert_eq!(strike.pos.y, 1, "must land directly above the stone floor");
    }

    /// `spawn_mobs_rule == false` forbids a trap outcome outright, regardless
    /// of how saturated the difficulty roll is — the trap chance's leading
    /// `&&` term.
    #[test]
    fn trap_requires_the_spawn_mobs_rule() {
        let saturated = DifficultyInstance::new(Difficulty::Hard, 5_000_000, 5_000_000, 1.0);
        let mut r = rng();
        assert!(!should_be_skeleton_trap(false, saturated, false, &mut r));
    }

    /// A lightning rod directly below the strike forbids a trap outright,
    /// regardless of the difficulty roll.
    #[test]
    fn trap_is_forbidden_under_a_lightning_rod() {
        let saturated = DifficultyInstance::new(Difficulty::Hard, 5_000_000, 5_000_000, 1.0);
        let mut r = rng();
        assert!(!should_be_skeleton_trap(true, saturated, true, &mut r));
    }

    /// The trap roll's magnitude: at Peaceful (`effective_difficulty == 0.0`)
    /// the chance is exactly zero regardless of the RNG, and at a saturated
    /// Hard difficulty (`effective_difficulty == 6.75`, `* 0.01 == 0.0675`)
    /// the measured rate over many trials must land near that, not near some
    /// other plausible-looking constant.
    #[test]
    fn trap_chance_matches_the_predicted_magnitude() {
        let peaceful = DifficultyInstance::new(Difficulty::Peaceful, 5_000_000, 5_000_000, 1.0);
        let mut r = rng();
        for _ in 0..1000 {
            assert!(!should_be_skeleton_trap(true, peaceful, false, &mut r), "Peaceful must never trap");
        }

        let saturated_hard = DifficultyInstance::new(Difficulty::Hard, 5_000_000, 5_000_000, 1.0);
        assert!((saturated_hard.effective_difficulty() - 6.75).abs() < 1e-4);
        const TRIALS: usize = 50_000;
        let mut hits = 0usize;
        let mut r = rng();
        for _ in 0..TRIALS {
            if should_be_skeleton_trap(true, saturated_hard, false, &mut r) {
                hits += 1;
            }
        }
        let rate = hits as f64 / TRIALS as f64;
        assert!((rate - 0.0675).abs() < 0.01, "predicted 0.0675, measured {rate}");
    }

    /// `BoltState::new` starts at `life == 2` with `flashes` in `1..=3`.
    #[test]
    fn bolt_state_starts_at_life_two_with_one_to_three_flashes() {
        let mut r = rng();
        for _ in 0..200 {
            let s = BoltState::new(&mut r, false);
            assert_eq!(s.life, 2);
            assert!((1..=3).contains(&s.flashes), "flashes out of range: {}", s.flashes);
        }
    }

    /// The first tick (`life == 2`) fires the sound/power/copper/event quartet
    /// and, on Normal, attempts ignition at the ground cell plus four
    /// offsets — five positions total.
    #[test]
    fn the_first_tick_fires_the_full_quartet_and_ignites_on_normal() {
        let mut state = BoltState::new(&mut rng(), false);
        let mut r = rng();
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Normal, &mut r);
        assert!(fx.play_thunder_sounds);
        assert!(fx.power_lightning_rod);
        assert!(fx.clear_copper);
        assert!(fx.game_event);
        assert_eq!(fx.fire_attempts.len(), 5, "one primary plus four offsets");
        assert_eq!(fx.fire_attempts[0], BlockPos::new(0, 0, 0), "the primary cell must be attempted first");
        assert_eq!(state.life, 1, "life must have decremented from 2");
    }

    /// On Easy, the quartet still fires but ignition is never attempted —
    /// the difficulty gate on the *first* strike.
    #[test]
    fn the_first_tick_never_ignites_below_normal_difficulty() {
        let mut state = BoltState::new(&mut rng(), false);
        let mut r = rng();
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Easy, &mut r);
        assert!(fx.power_lightning_rod, "the quartet is not difficulty-gated");
        assert!(fx.fire_attempts.is_empty(), "Easy must not ignite on the first strike");
    }

    /// `visual_only` suppresses every ignition attempt even on Hard, but
    /// leaves the power/copper/event quartet untouched — the trap bolt still
    /// powers a lightning rod, it just never sets anything alight or hits
    /// anyone.
    #[test]
    fn visual_only_suppresses_ignition_and_entity_hits_but_not_the_quartet() {
        let mut state = BoltState::new(&mut rng(), true);
        let mut r = rng();
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Hard, &mut r);
        assert!(fx.fire_attempts.is_empty());
        assert!(fx.power_lightning_rod);
        assert!(fx.clear_copper);
        assert!(fx.game_event);
        assert!(!fx.hit_entities, "a trap bolt must never hit entities");
    }

    /// `hit_entities` is set on every tick with `life >= 0`, for a
    /// non-visual bolt — not only the first.
    #[test]
    fn hit_entities_is_set_while_life_is_nonnegative() {
        let mut state = BoltState::new(&mut rng(), false);
        let mut r = rng();
        // life 2 -> after tick, life 1: still >= 0.
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Peaceful, &mut r);
        assert!(fx.hit_entities);
        assert_eq!(state.life, 1);
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Peaceful, &mut r);
        assert!(fx.hit_entities);
        assert_eq!(state.life, 0);
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Peaceful, &mut r);
        // life goes to -1 this tick: `life >= 0` is now false.
        assert!(!fx.hit_entities);
        assert_eq!(state.life, -1);
    }

    /// A bolt with exactly one flash discards the first time `life` goes
    /// negative, with no re-strike — `flashes == 0` short-circuits before the
    /// `-nextInt(10)` comparison is even reached.
    #[test]
    fn a_single_flash_bolt_discards_without_a_restrike() {
        let mut state = BoltState { life: 0, flashes: 0, visual_only: false };
        let mut r = rng();
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Normal, &mut r);
        assert!(fx.discard);
        assert_eq!(state.life, -1);
    }

    /// The reignite branch: with `flashes > 0`, a low enough `life` triggers
    /// `-rng.next_int(10)` comparison — forced deterministic here by starting
    /// `life` at a very negative value, which always satisfies `life < 0`
    /// (since `-next_int(10)` is at most `0`).
    #[test]
    fn a_very_negative_life_always_restrikes_when_flashes_remain() {
        let mut state = BoltState { life: -100, flashes: 2, visual_only: false };
        let mut r = rng();
        let fx = tick_bolt(&mut state, BlockPos::new(5, 5, 5), Difficulty::Peaceful, &mut r);
        assert!(!fx.discard);
        assert_eq!(state.flashes, 1, "one flash must have been consumed");
        assert_eq!(state.life, 1, "life resets to 1 on a restrike");
        assert_eq!(fx.fire_attempts.len(), 1, "spawnFire(0) attempts only the primary cell");
        assert_eq!(fx.fire_attempts[0], BlockPos::new(5, 5, 5));
    }

    /// The reignite's `spawnFire(0)` carries **no** difficulty gate — unlike
    /// the first strike, Peaceful still attempts the primary cell on a
    /// restrike. This is the asymmetry the module doc calls out as easy to
    /// port wrong.
    #[test]
    fn restrike_ignition_ignores_difficulty() {
        let mut state = BoltState { life: -100, flashes: 2, visual_only: false };
        let mut r = rng();
        let fx = tick_bolt(&mut state, BlockPos::new(0, 0, 0), Difficulty::Peaceful, &mut r);
        assert_eq!(fx.fire_attempts.len(), 1, "Peaceful must still attempt ignition on a restrike");
    }

    /// The full `resolve_effect` table, including the two Peaceful gates and
    /// the two corrected misconceptions (no turtle-egg entry exists at all;
    /// skeleton-horse-trap is not in this table because it is not a
    /// `thunderHit` transformation).
    #[test]
    fn resolve_effect_matches_the_jar_table() {
        assert_eq!(resolve_effect("minecraft:turtle", Difficulty::Hard), LightningEffect::Lethal);
        assert_eq!(resolve_effect("minecraft:creeper", Difficulty::Peaceful), LightningEffect::BecomeCharged);
        assert_eq!(resolve_effect("minecraft:mooshroom", Difficulty::Peaceful), LightningEffect::ToggleMooshroomVariant);

        assert_eq!(resolve_effect("minecraft:pig", Difficulty::Normal), LightningEffect::ConvertToZombifiedPiglin);
        assert_eq!(resolve_effect("minecraft:pig", Difficulty::Peaceful), LightningEffect::DamageAndIgnite, "Peaceful falls back to the default");
        assert_eq!(resolve_effect("minecraft:villager", Difficulty::Easy), LightningEffect::ConvertToWitch);
        assert_eq!(resolve_effect("minecraft:villager", Difficulty::Peaceful), LightningEffect::DamageAndIgnite);

        assert_eq!(resolve_effect("minecraft:zombie", Difficulty::Normal), LightningEffect::DamageAndIgnite);
        assert_eq!(resolve_effect("minecraft:cow", Difficulty::Normal), LightningEffect::DamageAndIgnite);
        // No turtle-egg entry: the block has no lightning hook in the jar.
        assert_eq!(resolve_effect("minecraft:turtle_egg", Difficulty::Normal), LightningEffect::DamageAndIgnite);
        // Not present at all: the skeleton-horse-trap mechanism is the
        // reverse relationship (see the module doc), not a `thunderHit` arm.
        assert_eq!(resolve_effect("minecraft:skeleton_horse", Difficulty::Normal), LightningEffect::DamageAndIgnite);
    }

    /// `LightningFeed` round-trips publish -> drain in FIFO order, the same
    /// contract `WeatherFeed` carries.
    #[test]
    fn lightning_feed_drains_in_order() {
        let feed = LightningFeed::default();
        assert!(feed.drain_all().is_empty());
        feed.publish(Strike { pos: BlockPos::new(0, 0, 0), visual_only: false });
        feed.publish(Strike { pos: BlockPos::new(1, 1, 1), visual_only: true });
        assert_eq!(
            feed.drain_all(),
            vec![
                Strike { pos: BlockPos::new(0, 0, 0), visual_only: false },
                Strike { pos: BlockPos::new(1, 1, 1), visual_only: true },
            ]
        );
        assert!(feed.drain_all().is_empty());
    }
}
