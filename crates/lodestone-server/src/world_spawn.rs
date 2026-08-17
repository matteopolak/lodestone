//! The world spawn point and per-player respawn points.
//!
//! Before this module, the world spawn was derived inline per connection
//! (`serve_connection`'s `ConfigurationFinished` arm): the origin column's
//! surface at local `(8, 8)`, i.e. always `(8, y, 8)` — a later fix replaced
//! the Y with terrain, but the X/Z were still fixed, so a world whose origin
//! chunk is ocean spawned the player under water and no search ever moved
//! them. This module is vanilla's own search:
//!
//! * [`find_initial_spawn`] runs `MinecraftServer.setInitialSpawn`'s
//!   121-iteration, ±5-chunk spiral over a [`ChunkSource`], stopping at the
//!   first chunk that contains a valid spawn position
//!   (`PlayerSpawnFinder.getSpawnPosInChunk`).
//! * A per-column candidate is vanilla's `PlayerSpawnFinder.getLevelRespawnPos`:
//!   the surface height, with a fluid between sky and ground (an ocean
//!   column) aborting the candidate.
//! * The **per-player** half — a player's bed respawn point
//!   ([`RespawnPoint`]) with the set-time legality check vanilla applies
//!   before accepting one ([`is_legal_bed_respawn`]), and the *read* that
//!   resolves a death against it ([`resolve_bed_respawn`], vanilla's
//!   `ServerPlayer.findRespawnAndUseSpawnBlock`). The set-time check mirrors
//!   `ServerPlayer.startSleepInBed`'s validation.
//!
//!   **Both halves are needed and the read is the one that was missing.** The
//!   point used to be stored and consulted by nothing, so `PERFORM_RESPAWN`
//!   healed the player and left them where they died. The read re-examines the
//!   block at the stored position rather than trusting it: a broken or walled-in
//!   bed answers `None` and the caller falls back to the world spawn, which is
//!   vanilla's own `Optional.empty()` arm.
//!
//!   Still deferred: the `respawn_radius` scatter around the world spawn and the
//!   async chunk-ticket search of `PlayerSpawnFinder.findSpawn`, which need
//!   shape-B player state and the ticket system (see
//!   `docs/plans/world-state.md` unit P2).
//!
//! # Where the world spawn is stored
//!
//! In [`crate::world_state::WorldStateHandle`], persisted to `level.dat`'s nested
//! `spawn` compound — **not** re-derived per connection. [`find_initial_spawn`] is
//! vanilla's `setInitialSpawn`, which runs once at world creation; running it per
//! join re-paid a 121-column search every time and meant the persisted value was
//! written and read by nothing. `None` there means "not searched yet", which is
//! what a fresh world is.

use lodestone_model::{BlockPos, Vec3};

use crate::chunk::{ChunkColumn, ChunkSource, is_air_or_fluid};

/// The world's spawn point — vanilla's `LevelData.RespawnData` for the
/// overworld: a position plus the yaw/pitch a player is teleported with. The
/// initial world spawn has both rotations zero (`setInitialSpawn` passes
/// `0.0F, 0.0F`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WorldSpawn {
    /// World-space feet position, in blocks.
    pub pos: Vec3,
    /// Spawn yaw in degrees.
    pub yaw: f32,
    /// Spawn pitch in degrees.
    pub pitch: f32,
}

/// A player's per-player respawn point — the bed they last slept in, the
/// tracking half. Vanilla stores this per player as
/// `ServerPlayer.RespawnConfig` (a `RespawnData` plus a `forced` flag) and
/// consults it on death before falling back to the level spawn
/// (`ServerPlayer.findRespawnPositionAndUseSpawnBlock`, `PlayerList.respawn`).
///
/// Position only for now: vanilla also records the facing at bed-entry time
/// and spawns the player with it, but this crate's bed interaction is the
/// plain right-click in [`crate::server`]'s `apply_use_item_on`, which has no
/// player rotation in scope; the respawn teleport therefore uses the world
/// spawn's facing. Cosmetic, documented as a follow-up when rotation is
/// threaded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RespawnPoint {
    /// The bed block's position (the half the player clicked).
    pub pos: BlockPos,
}

/// Vanilla's `ChunkGenerator.getSpawnHeight`:
///
/// ```java
/// public int getSpawnHeight(final LevelHeightAccessor heightAccessor) {
///    return 64;
/// }
/// ```
///
/// A literal `64`, and `NoiseBasedChunkGenerator` does **not** override it — only
/// `FlatLevelSource` does — so this is the value in force for the overworld this
/// crate serves. `MinecraftServer.setInitialSpawn` reads it, keeps it because
/// `64 >= level.getMinY()`, and pre-seeds the world spawn at `offset(8, 64, 8)`;
/// the `WORLD_SURFACE` heightmap branch beside it is dead code for any noise
/// generator.
///
/// # Why a literal is the right answer and a surface query is not
///
/// It is two blocks above the generator's sea level of 62, so a fully-ocean
/// search box lands the player in open water — visible, breathable at the
/// surface, and no fall damage from a two-block drop. The surface-derived value
/// this replaced was strictly worse in the case that actually fires: see
/// [`find_initial_spawn`]'s own comment.
const GENERATOR_SPAWN_HEIGHT: i32 = 64;

/// The height a player stands at for a `getLevelRespawnPos`-valid column
/// position `(lx, lz)` in `[0..16)`, or `None` when the column is invalid
/// there.
///
/// This is vanilla's `PlayerSpawnFinder.getLevelRespawnPos`, simplified to
/// what a [`ChunkColumn`] can answer — it has no persisted heightmaps, so the
/// top-of-column scan below is the `MOTION_BLOCKING` heightmap query's
/// analogue:
///
/// 1. Scan downward from the top of the column.
/// 2. A fluid encountered before any solid block (an ocean column, or a lava
///    lake) aborts the candidate — vanilla's `break` on a non-empty fluid
///    state, which yields `null`.
/// 3. The first solid block from the top is the surface; return one block
///    above it (`return pos.above().immutable()`), the feet position.
/// 4. A column with no solid block at all (air/void world) is `None`.
///
/// The two tests are vanilla's own, and **not** [`is_air_or_fluid`]'s negation,
/// which is what this function used until the measurement in DESIGN.md §12.129:
///
/// | vanilla expression | here |
/// |---|---|
/// | `!blockState.getFluidState().isEmpty()` | [`spawn_has_fluid_state`] |
/// | `Block.isFaceFull(state.getCollisionShape(…), UP)` | [`spawn_face_full_up`] |
///
/// The old form's justification was a *stale* `worldgen_data` scope note ("no
/// vegetation at surface"). The generator now places `short_grass`,
/// `dandelion`, `poppy` and snow layers, and `is_air_or_fluid` says all four are
/// ground — so the spawn Y came out **one block above** the block a player can
/// actually stand on, which is the "I spawn in the air" report. Measured
/// `face_full_up`: `short_grass`/`dandelion`/`poppy`/`snow` are all `false`,
/// `grass_block`/`stone`/`oak_log`/`oak_leaves` are all `true` — so the jar's own
/// predicate scans past the vegetation and stops on the ground, and a treetop
/// spawn (leaves are genuinely face-full) stays faithful rather than being
/// "fixed" into a divergence.
///
/// The vanilla pre-check in `getLevelRespawnPos` — the `WORLD_SURFACE` /
/// `MOTION_BLOCKING` / `OCEAN_FLOOR` heightmap comparison — is deliberately not
/// reproduced: it is
/// an early-out over three persisted heightmaps a [`ChunkColumn`] does not
/// carry, and the loop below reaches the same verdict for the case it exists to
/// catch (a water column aborts on the fluid test before any ground is found).
fn get_level_respawn_pos(column: &ChunkColumn, lx: i32, lz: i32) -> Option<i32> {
    let top_y = column.min_y + column.height - 1;
    for y in (column.min_y..=top_y).rev() {
        let state = column.block_state(lx, y, lz);
        if spawn_has_fluid_state(state) {
            // Fluid between sky and ground — an ocean column. Fail-closed,
            // exactly like vanilla's `null`: the caller keeps searching.
            return None;
        }
        if spawn_face_full_up(state) {
            return Some(y + 1);
        }
    }
    None
}

/// The two lookup tables joining a block-state *string* to a census state id:
/// exact canonical state first, base-name default second.
///
/// [`lodestone_data::snow_support`]'s bitsets are keyed by block-state id and
/// this crate only ever holds a string, so the two have to be joined. **Both**
/// halves are load-bearing and each covers a case the other gets wrong:
///
/// * The **exact** map is what makes `oak_leaves[waterlogged=true]` answer
///   `has_fluid_state = true`. A base-name-only join reports its *default*
///   state's `false`, and
///   `spawn_state_resolution_agrees_with_the_census_for_every_surface_state`
///   caught exactly that — it is not a hypothetical, it is why this map exists.
/// * The **base-name** map is what makes bare `minecraft:water` resolve at all:
///   `lodestone-worldgen` emits fluids without their `level` property
///   (`docs/worldgen-parity.md`'s "Known representation gap", the same reason
///   [`crate::worldgen_data`]'s `freeze_facts` keys its document by default
///   state), so a generated column's water is a string no exact map contains.
///
/// Built once per process from two static tables.
fn spawn_state_tables() -> &'static (
    std::collections::HashMap<String, u32>,
    std::collections::HashMap<&'static str, u32>,
) {
    use std::sync::OnceLock;
    #[allow(clippy::type_complexity)]
    static TABLES: OnceLock<(
        std::collections::HashMap<String, u32>,
        std::collections::HashMap<&'static str, u32>,
    )> = OnceLock::new();
    TABLES.get_or_init(|| {
        use lodestone_data::{block_states, snow_support};
        let mut exact = std::collections::HashMap::new();
        let mut defaults = std::collections::HashMap::new();
        for id in 0..snow_support::STATE_COUNT {
            let Some(name) = block_states::block_name(id) else {
                continue;
            };
            exact.insert(spawn_canonical_state(id), id);
            if snow_support::is_default_state(id) == Some(true) {
                defaults.insert(name, id);
            }
        }
        (exact, defaults)
    })
}

/// `name[k=v,…]` with properties in `block_states::properties`' own sorted
/// order — the spelling `lodestone_worldgen::feature::canon_state` produces and
/// therefore the spelling a generated [`ChunkColumn`] holds.
///
/// A near-duplicate of [`crate::worldgen_data`]'s private `canonical_state`, kept
/// separate rather than made shared: that one is a build input for the freeze
/// document and this one is a runtime lookup key, and coupling them would make a
/// change to either reach the other for no reason.
fn spawn_canonical_state(id: u32) -> String {
    use lodestone_data::block_states;
    let name = block_states::block_name(id).unwrap_or("minecraft:air");
    let props = block_states::properties(id).unwrap_or(&[]);
    if props.is_empty() {
        return name.to_owned();
    }
    let body: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{name}[{}]", body.join(","))
}

/// The census state id for a block-state string: the exact state where the
/// census has it, otherwise the block's default state.
fn spawn_state_id(state: &str) -> Option<u32> {
    let (exact, defaults) = spawn_state_tables();
    if let Some(&id) = exact.get(state) {
        return Some(id);
    }
    let base = state.split('[').next().unwrap_or(state);
    defaults.get(base).copied()
}

/// Vanilla `!blockState.getFluidState().isEmpty()`
/// ([`lodestone_data::snow_support::has_fluid_state`]) for a block-state string.
///
/// An unknown block name answers `false`: a name the census has never heard of
/// cannot be a fluid, and answering `true` would abort the spawn search for the
/// whole column.
fn spawn_has_fluid_state(state: &str) -> bool {
    spawn_state_id(state)
        .and_then(lodestone_data::snow_support::has_fluid_state)
        .unwrap_or(false)
}

/// Vanilla `Block.isFaceFull(state.getCollisionShape(…), UP)`
/// ([`lodestone_data::snow_support::face_full_up`]) for a block-state string.
///
/// An unknown block name answers `false` — fail-closed, so a name the census
/// does not carry can never become the block a player is stood on top of. That
/// is the safe direction: the search moves on instead of placing the player on
/// something it cannot prove is solid.
fn spawn_face_full_up(state: &str) -> bool {
    spawn_state_id(state)
        .and_then(lodestone_data::snow_support::face_full_up)
        .unwrap_or(false)
}

/// Scans one already-generated column's 256 block positions in vanilla's order
/// (`for x … for z`, `PlayerSpawnFinder.getSpawnPosInChunk`) and returns the
/// first valid spawn position's world `BlockPos`, or `None` if the
/// whole chunk is invalid (all ocean, all void, …).
///
/// Takes the column rather than the source so [`find_initial_spawn`] can reuse
/// the origin column it has already paid for — see its own doc comment.
fn spawn_pos_in_column(column: &ChunkColumn, cx: i32, cz: i32) -> Option<BlockPos> {
    for lx in 0..16 {
        for lz in 0..16 {
            if let Some(y) = get_level_respawn_pos(column, lx, lz) {
                return Some(BlockPos::new(cx * 16 + lx, y, cz * 16 + lz));
            }
        }
    }
    None
}

/// [`spawn_pos_in_column`] for a chunk that is not yet in hand.
fn get_spawn_pos_in_chunk<S: ChunkSource + ?Sized>(source: &S, cx: i32, cz: i32) -> Option<BlockPos> {
    spawn_pos_in_column(&source.column(cx, cz), cx, cz)
}

/// The 121 chunk offsets the initial-spawn spiral visits, in vanilla order.
///
/// Transcribed from `MinecraftServer.setInitialSpawn`'s loop: it starts at
/// `(0, 0)`, steps with an
/// initial direction `(0, -1)`, and turns right (swapping `dX`/`dZ` with the
/// negation) whenever it reaches a square's corner — the three-arm
/// `xChunkOffset == zChunkOffset || (xChunkOffset < 0 && xChunkOffset ==
/// -zChunkOffset) || (xChunkOffset > 0 && xChunkOffset == 1 - zChunkOffset)`
/// turn test. Kept as an explicit sequence so the traversal order is a named,
/// testable fact (the spiral's *first* candidate is not the nearest land
/// chunk; it is the origin, then `(1,0)`, then `(1,1)`, …).
fn spiral_chunk_offsets() -> Vec<(i32, i32)> {
    let mut out = Vec::with_capacity(11 * 11);
    let (mut xo, mut zo) = (0i32, 0i32);
    let (mut dx, mut dz) = (0i32, -1i32);
    for _ in 0..(11 * 11) {
        out.push((xo, zo));
        if xo == zo || (xo < 0 && xo == -zo) || (xo > 0 && xo == 1 - zo) {
            let old_dx = dx;
            dx = -dz;
            dz = old_dx;
        }
        xo += dx;
        zo += dz;
    }
    out
}

/// Searches the world spawn point from the origin chunk, mirroring
/// `MinecraftServer.setInitialSpawn`.
///
/// Vanilla's first step — `chunkSource.randomState().sampler()
/// .findSpawnPosition()` — picks a spawn *chunk* from climate noise; that
/// sampler is `lodestone-worldgen` deep machinery this crate does not expose,
/// so the search centres on the origin chunk `(0, 0)`, which is exactly the
/// result `Climate.Sampler.findSpawnPosition` returns for an empty spawn
/// target. The consequence is honest and documented: with no
/// climate picker the *choice* of centre is fixed, but the spiral that finds
/// a valid surface *within* that ±5-chunk box is real, which is the piece
/// that was missing (an ocean origin chunk now moves the spawn to the nearest
/// land instead of spawning the player under water).
///
/// Returns the first valid spawn position in spiral order, or — when every
/// chunk in the box is invalid (a full-ocean box) — `(8, `[`GENERATOR_SPAWN_HEIGHT`]`, 8)`,
/// which is vanilla's own pre-seed (`setInitialSpawn` calls `levelData.setSpawn`
/// with `offset(8, height, 8)` before the loop and the loop only overrides it
/// with a *valid* find).
///
/// # Column generations, because this is on the join critical path
///
/// This runs in `crate::server`'s `ConfigurationFinished` arm **before** the
/// chunk-streaming ring loop, so every column it generates is time the client
/// spends with no terrain — this is the time-to-first-chunk cost. For the normal case
/// — a valid origin chunk — that cost is exactly **one** column: the spiral's
/// first offset is `(0, 0)`, which is the column the `fallback_y` query already
/// generated, so it is reused rather than re-requested. It used to be asked for
/// twice, which made `serve_play`'s "at most 2 columns before the first encode"
/// bound unsatisfiable at 3 against a store-less source.
///
/// For an *invalid* origin the spiral genuinely walks up to 121 columns first.
/// That is vanilla's own search and it is not a leak — the ±5-chunk box sits
/// inside the ±9 join view, so a [`crate::ChunkStore`]-wrapped source serves
/// those columns from cache when the ring loop reaches them.
pub(crate) fn find_initial_spawn<S: ChunkSource + ?Sized>(source: &S) -> WorldSpawn {
    let origin = source.column(0, 0);
    // Vanilla's own pre-seed, and **not** what this line used to say. See
    // [`GENERATOR_SPAWN_HEIGHT`]: the previous form was
    // `get_level_respawn_pos(&origin, 8, 8).unwrap_or(origin.min_y + 1)`, and the
    // `min_y + 1` arm put the player at `y = -63` — *inside the bedrock floor*,
    // under an ocean, in the dark. Measured on two of four probe seeds
    // (`1234` and `-195764831`), where the whole ±5 box is ocean and the fallback
    // is the arm that fires. `64` is `ChunkGenerator.getSpawnHeight`, two blocks
    // above the generator's sea level of 62, so the same pathological world now
    // drops the player into open water instead of burying them.
    let fallback_y = GENERATOR_SPAWN_HEIGHT;

    for (xo, zo) in spiral_chunk_offsets() {
        // `(0, 0)` is always the spiral's first offset, and `origin` is already
        // in hand: asking the source for it again doubles the common case's
        // pre-streaming generation cost. See this function's doc comment.
        let candidate = if (xo, zo) == (0, 0) {
            spawn_pos_in_column(&origin, 0, 0)
        } else {
            get_spawn_pos_in_chunk(source, xo, zo)
        };
        if let Some(pos) = candidate {
            return WorldSpawn {
                pos: Vec3::new(pos.x as f64, pos.y as f64, pos.z as f64),
                yaw: 0.0,
                pitch: 0.0,
            };
        }
    }

    WorldSpawn {
        pos: Vec3::new(8.0, fallback_y as f64, 8.0),
        yaw: 0.0,
        pitch: 0.0,
    }
}

/// Returns `true` for the sixteen bed block ids (`minecraft:white_bed` …
/// `minecraft:red_bed`, including their block-state suffix). Right-clicking
/// one is a "set my respawn point" interaction, not a placement — see
/// [`crate::server`]'s `apply_use_item_on`.
pub(crate) fn is_bed_block(name: &str) -> bool {
    let base = name.split('[').next().unwrap_or(name);
    matches!(
        base,
        "minecraft:white_bed"
            | "minecraft:orange_bed"
            | "minecraft:magenta_bed"
            | "minecraft:light_blue_bed"
            | "minecraft:yellow_bed"
            | "minecraft:lime_bed"
            | "minecraft:pink_bed"
            | "minecraft:gray_bed"
            | "minecraft:light_gray_bed"
            | "minecraft:cyan_bed"
            | "minecraft:purple_bed"
            | "minecraft:blue_bed"
            | "minecraft:brown_bed"
            | "minecraft:green_bed"
            | "minecraft:red_bed"
            | "minecraft:black_bed"
    )
}

/// Whether right-clicking the bed at `bed` should set the player's respawn
/// point — beds/anchors need to be validated for a legal respawn spot before
/// being accepted.
///
/// This is the set-time half of vanilla's `ServerPlayer.startSleepInBed`,
/// reduced to what this crate's interaction scope can answer:
///
/// 1. The clicked block is a bed ([`is_bed_block`]).
/// 2. The cell directly above the bed is clear — vanilla's obstruction
///    rejection, `ServerPlayer.bedBlocked`'s check that the space above the
///    bed is free. A solid block above the bed makes the spot illegal.
/// 3. The player is in reach of the bed — vanilla's `ServerPlayer.bedInRange`,
///    bed ±3 x/z and ±2 y. `player_pos` is `None` until the first
///    [`crate::server`]`::PlayerMoved` packet; a click before any move skips
///    the range test (cannot be wrong about a position it never had).
///
/// The fourth check vanilla applies — a `NOT_SAFE` monster within ±8 h / ±5 v
/// of the bed, checked inline in `ServerPlayer.startSleepInBed` and skipped
/// only in creative — is the documented remainder: it needs a mob-AABB query
/// this crate's interaction scope does not carry (shape-B world state; see
/// this module's doc). Without it a bed in monster range is accepted, a gap
/// the placement half of P2 will close.
pub(crate) fn is_legal_bed_respawn<S: ChunkSource + ?Sized>(
    source: &S,
    bed: BlockPos,
    player_pos: Option<Vec3>,
) -> bool {
    if !is_bed_block(&source.block_state(bed.x, bed.y, bed.z)) {
        return false;
    }
    // Obstructed: a solid block above the bed blocks the sleeping AABB
    // (vanilla's `noCollision`), so the spot is illegal even though the bed
    // itself is present.
    if !is_air_or_fluid(&source.block_state(bed.x, bed.y + 1, bed.z)) {
        return false;
    }
    if let Some(player) = player_pos {
        let dx = player.x - f64::from(bed.x);
        let dy = player.y - f64::from(bed.y);
        let dz = player.z - f64::from(bed.z);
        if dx.abs() > 3.0 || dz.abs() > 3.0 || dy.abs() > 2.0 {
            return false;
        }
    }
    true
}

/// Vanilla's `bedSurroundStandUpOffsets` followed by `bedAboveStandUpOffsets`
/// (`BedBlock.bedStandUpOffsets`), as `(forward_steps, side_steps)` multipliers
/// rather than resolved `(dx, dz)` — the direction vectors are substituted by
/// [`resolve_bed_respawn`], which knows the bed's `facing`.
///
/// The **order is the specification**, not an implementation detail: vanilla
/// returns the first offset that yields a safe dismount location, so a different
/// order puts the player somewhere else for the same bed. Transcribed in vanilla's
/// own sequence, `side` first and the two on-bed cells last.
const BED_STAND_UP_OFFSETS: [(i32, i32); 12] = [
    // bedSurroundStandUpOffsets(forward, side)
    (0, 1),
    (-1, 1),
    (-2, 1),
    (-2, 0),
    (-2, -1),
    (-1, -1),
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    // bedAboveStandUpOffsets(forward) — the bed's own cell, and its foot.
    (0, 0),
    (-1, 0),
];

/// The `(dx, dz)` step vector for a bed's `facing` property, or `None` for a
/// state carrying no recognisable facing.
fn bed_facing_steps(state: &str) -> Option<(i32, i32)> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    let facing = props.split(',').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k.trim() == "facing").then(|| v.trim())
    })?;
    match facing {
        "north" => Some((0, -1)),
        "south" => Some((0, 1)),
        "west" => Some((-1, 0)),
        "east" => Some((1, 0)),
        _ => None,
    }
}

/// Whether a player can stand at `pos` — the reduction of
/// `DismountHelper.findSafeDismountLocation` this crate can actually answer.
///
/// Vanilla walks the player's collision shape against the level and additionally
/// rejects "dangerous" blocks (fire, lava, magma, a cactus) on its first pass.
/// This crate has no player AABB sweep, so the test is the two facts the census
/// does carry: **two** clear cells of body room (a player is 1.8 blocks tall, so
/// the cell above matters), standing on something whose up-face is full.
///
/// Fail-closed on both halves, which is the safe direction here: an unrecognised
/// block is not somewhere the search will place a player, so it moves on to the
/// next offset rather than dropping them into it.
fn is_standable<S: ChunkSource + ?Sized>(source: &S, pos: BlockPos) -> bool {
    let feet = source.block_state(pos.x, pos.y, pos.z);
    let head = source.block_state(pos.x, pos.y + 1, pos.z);
    let below = source.block_state(pos.x, pos.y - 1, pos.z);
    is_air_or_fluid(&feet) && is_air_or_fluid(&head) && spawn_face_full_up(&below)
}

/// Resolves a stored per-player [`RespawnPoint`] into the position a death should
/// return the player to, or `None` if the point is no longer usable.
///
/// This is `ServerPlayer.findRespawnAndUseSpawnBlock`'s bed branch, and the
/// `None` arm is the load-bearing half rather than an error case:
///
/// ```java
/// BlockState blockState = level.getBlockState(pos);
/// if (block instanceof BedBlock && …canSetSpawn(level)) {
///    return BedBlock.findStandUpPosition(EntityTypes.PLAYER, level, pos, blockState.getValue(FACING), yaw)
///       .map(p -> RespawnPosAngle.of(p, pos, 0.0F));
/// }
/// if (!forced) { return Optional.empty(); }
/// ```
///
/// So the block at the stored position is **re-read at death time**, not trusted
/// from when it was set: a bed that has since been broken yields `Optional.empty()`,
/// vanilla sends `NO_RESPAWN_BLOCK_AVAILABLE`, and the player lands at the world
/// spawn. Storing the point at set time and never re-validating it would respawn a
/// player inside whatever replaced their bed. That is the whole reason this
/// function takes the source rather than just the point.
///
/// The candidate walk is vanilla's [`BED_STAND_UP_OFFSETS`] in vanilla's own
/// order, with [`is_standable`] standing in for `DismountHelper` — see its doc for
/// what that costs. Vanilla's second, `checkDangerous = false` pass over the same
/// offsets is not reproduced: the only difference between the two passes is the
/// danger check, which `is_standable` does not model, so a second pass would test
/// exactly the same predicate and find exactly the same answer.
///
/// The returned position is the cell's centre in x/z, its floor in y — vanilla's
/// `DismountHelper` returns a `Vec3` at the block's centre-bottom.
pub(crate) fn resolve_bed_respawn<S: ChunkSource + ?Sized>(
    source: &S,
    point: RespawnPoint,
) -> Option<Vec3> {
    let bed = point.pos;
    let state = source.block_state(bed.x, bed.y, bed.z);
    if !is_bed_block(&state) {
        // The bed is gone. Vanilla's `Optional.empty()`.
        return None;
    }
    // A bed with no readable `facing` cannot have its offsets resolved; treat the
    // head/foot axis as north-south, which is the default state's own facing, so
    // the walk still happens rather than the point being silently discarded.
    let (fx, fz) = bed_facing_steps(&state).unwrap_or((0, -1));
    // `side` is `forward.getClockWise()` — vanilla picks between it and its
    // opposite using the sleeper's yaw, which this crate does not record at bed
    // entry (see [`RespawnPoint`]'s own doc). The clockwise choice is taken, which
    // only decides *which* side of the bed a player wakes on.
    let (sx, sz) = (-fz, fx);
    for (forward_steps, side_steps) in BED_STAND_UP_OFFSETS {
        let candidate = BlockPos::new(
            bed.x + fx * forward_steps + sx * side_steps,
            bed.y,
            bed.z + fz * forward_steps + sz * side_steps,
        );
        if is_standable(source, candidate) {
            return Some(Vec3::new(
                f64::from(candidate.x) + 0.5,
                f64::from(candidate.y),
                f64::from(candidate.z) + 0.5,
            ));
        }
    }
    // Every offset obstructed. Vanilla's `Optional.empty()` again — the bed is
    // walled in, and the player goes to the world spawn.
    None
}

// A small `ChunkSource` for the spiral gates: a fixed map of columns, with any
// chunk outside it answering an all-air (therefore spawn-invalid) column, so a
// gate can exercise exactly the terrain it cares about without generating 121
// real columns.
#[cfg(test)]
struct MapSource {
    columns: std::collections::HashMap<(i32, i32), ChunkColumn>,
}

#[cfg(test)]
impl ChunkSource for MapSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.columns
            .get(&(cx, cz))
            .cloned()
            .unwrap_or_else(|| ChunkColumn::new(0, 128))
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // Fixture: the gates never edit terrain.
    }
}

/// Builds a solid-stone column with its surface at `surface_y`, plus a plain
/// `min_y = 0`, `height = 128` vertical extent.
#[cfg(test)]
fn land_column(surface_y: i32) -> ChunkColumn {
    let mut column = ChunkColumn::new(0, 128);
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..=surface_y {
                column.set_block(x, y, z, "minecraft:stone");
            }
        }
    }
    column
}

/// An all-water column (`height` rows of water) — `getLevelRespawnPos` must
/// reject every position in it, so the spiral moves on.
#[cfg(test)]
fn ocean_column() -> ChunkColumn {
    let mut column = ChunkColumn::new(0, 128);
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..32 {
                column.set_block(x, y, z, "minecraft:water");
            }
        }
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spiral_traverses_the_square_in_vanilla_order() {
        // The first twelve offsets, transcribed from `setInitialSpawn`'s loop
        // (verified by hand against the Java above).
        let expected: [(i32, i32); 12] = [
            (0, 0),
            (1, 0),
            (1, 1),
            (0, 1),
            (-1, 1),
            (-1, 0),
            (-1, -1),
            (0, -1),
            (1, -1),
            (2, -1),
            (2, 0),
            (2, 1),
        ];
        let offsets = spiral_chunk_offsets();
        assert_eq!(&offsets[..expected.len()], &expected[..]);
        assert_eq!(offsets.len(), 121, "Mth.square(11) = 121 iterations");
        // Every offset in the ±5 box appears exactly once.
        let mut seen = std::collections::HashSet::new();
        for &(x, z) in &offsets {
            assert!((-5..=5).contains(&x) && (-5..=5).contains(&z));
            assert!(seen.insert((x, z)), "duplicate offset ({x}, {z})");
        }
        assert_eq!(seen.len(), 121);
    }

    #[test]
    fn level_respawn_pos_rejects_ocean_and_accepts_land() {
        let land = land_column(10);
        assert_eq!(get_level_respawn_pos(&land, 0, 0), Some(11), "stand one above the surface");

        let ocean = ocean_column();
        assert_eq!(get_level_respawn_pos(&ocean, 0, 0), None, "a fluid above the surface aborts");

        let void = ChunkColumn::new(0, 128);
        assert_eq!(get_level_respawn_pos(&void, 0, 0), None, "no solid block at all");
    }

    /// **The spawn-in-the-air defect, as a magnitude gate.** A plains column with
    /// vegetation on it must spawn the player *on the ground block*, not on the
    /// flower standing on it.
    ///
    /// Both hypotheses are computed from outside constants rather than one being
    /// asserted: `is_air_or_fluid`'s negation (the predicate this function used
    /// until DESIGN.md §12.129) calls `short_grass` ground and yields `surface +
    /// 2`; the jar's `Block.isFaceFull(…, UP)` scans past it and yields `surface +
    /// 1`. A sign-only check ("the spawn is above the surface") passes under
    /// both, which is exactly why the bug survived.
    #[test]
    fn vegetation_on_the_surface_is_not_what_the_player_stands_on() {
        const SURFACE_Y: i32 = 70;
        for plant in [
            "minecraft:short_grass",
            "minecraft:dandelion",
            "minecraft:poppy",
            "minecraft:snow[layers=1]",
        ] {
            let mut column = land_column(SURFACE_Y);
            // The generator's own shape: a solid surface block with one
            // non-collidable decoration standing on it.
            column.set_block(0, SURFACE_Y, 0, "minecraft:grass_block[snowy=false]");
            column.set_block(0, SURFACE_Y + 1, 0, plant);

            let correct = SURFACE_Y + 1;
            let suspected_wrong = SURFACE_Y + 2;
            assert_ne!(correct, suspected_wrong, "the two hypotheses must differ");

            let measured = get_level_respawn_pos(&column, 0, 0);
            assert_eq!(
                measured,
                Some(correct),
                "{plant} has no collision, so vanilla stands the player on the \
                 grass_block at {SURFACE_Y} (feet {correct}); {suspected_wrong} is the \
                 `is_air_or_fluid` answer that put the player one block in the air"
            );
        }
    }

    /// **Control for the gate above**: the predicate is not simply "skip
    /// everything above the stone", which would also produce `SURFACE_Y + 1`.
    /// A block that *is* face-full — leaves, which vanilla genuinely lets a
    /// player spawn on top of — must still raise the spawn.
    ///
    /// Without this, `vegetation_on_the_surface_is_not_what_the_player_stands_on`
    /// would pass against an implementation that ignored the block census
    /// entirely and always answered `stone_top + 1`.
    #[test]
    fn a_face_full_block_above_the_surface_does_raise_the_spawn() {
        const SURFACE_Y: i32 = 70;
        let mut column = land_column(SURFACE_Y);
        column.set_block(0, SURFACE_Y + 1, 0, "minecraft:oak_leaves[distance=3]");
        assert_eq!(
            get_level_respawn_pos(&column, 0, 0),
            Some(SURFACE_Y + 2),
            "oak_leaves measures face_full_up = true, so the player stands on the leaf"
        );
    }

    /// **The join gate.** [`spawn_state_id`] must give the census's own answer for
    /// **every** state of every block a generated surface can carry — not just the
    /// default state, and not just the states this module happens to name in a
    /// fixture.
    ///
    /// This test found a real defect on its first run: the join was base-name-only
    /// and `minecraft:oak_leaves[…,waterlogged=true]` (state id 253) resolved to
    /// its `waterlogged=false` default, reporting `has_fluid_state = false` for a
    /// block that vanilla says holds water. A waterlogged canopy would then have
    /// been treated as standable ground instead of aborting the column.
    #[test]
    fn spawn_state_resolution_agrees_with_the_census_for_every_surface_state() {
        use lodestone_data::{block_states, snow_support};
        // Every block the overworld generator can leave at or above a surface.
        const SURFACE_BLOCKS: &[&str] = &[
            "minecraft:water",
            "minecraft:lava",
            "minecraft:grass_block",
            "minecraft:short_grass",
            "minecraft:dandelion",
            "minecraft:poppy",
            "minecraft:snow",
            "minecraft:oak_leaves",
            "minecraft:birch_leaves",
            "minecraft:oak_log",
            "minecraft:stone",
            "minecraft:sand",
            "minecraft:gravel",
        ];
        let mut states_checked = 0usize;
        let mut multi_state_blocks = 0usize;
        for &name in SURFACE_BLOCKS {
            let mut states = 0usize;
            for id in 0..snow_support::STATE_COUNT {
                if block_states::block_name(id) != Some(name) {
                    continue;
                }
                states += 1;
                states_checked += 1;
                let canonical = spawn_canonical_state(id);
                assert_eq!(
                    spawn_state_id(&canonical),
                    Some(id),
                    "{canonical} must resolve to its own state id, not another state's"
                );
                assert_eq!(
                    spawn_face_full_up(&canonical),
                    snow_support::face_full_up(id) == Some(true),
                    "face_full_up disagrees with the census for {canonical}"
                );
                assert_eq!(
                    spawn_has_fluid_state(&canonical),
                    snow_support::has_fluid_state(id) == Some(true),
                    "has_fluid_state disagrees with the census for {canonical}"
                );
            }
            assert!(states > 0, "{name} must exist in the 26.2 census");
            if states > 1 {
                multi_state_blocks += 1;
            }
            // The generator's fluid spelling: bare, no `level`. It must still
            // resolve, via the base-name half.
            assert!(
                spawn_state_id(name).is_some(),
                "the bare base name {name} must resolve through the default-state map"
            );
        }
        // Preconditions. The first would be a vacuous pass with an empty list; the
        // second is what makes the *exact* half of the join actually exercised —
        // a corpus of single-state blocks could not tell the two maps apart.
        assert_eq!(states_checked > 0, true);
        assert!(
            multi_state_blocks >= 5,
            "only {multi_state_blocks} of the surface blocks have more than one state; \
             the exact-state half of the join would be barely covered"
        );
    }

    /// A valid origin chunk is accepted by the spiral's *first* candidate, and the
    /// position inside it is vanilla's scan order — local `(0, 0)`, **not** the
    /// centre.
    ///
    /// This test previously expected `(8, 8)` and named itself
    /// `plains_origin_chunk_yields_spawn_at_local_8_8`, transcribed from the
    /// hardcoded-spawn-point behaviour it replaced. It had never passed: the search
    /// and the expectation landed in the same commit (`43e096b`), and `(8, 8)` is
    /// not what the search returns for a valid chunk.
    ///
    /// `(0, 0)` is read off `PlayerSpawnFinder.getSpawnPosInChunk`, which scans
    /// from `chunkPos.getMinBlockX()`/`getMinBlockZ()` and returns the
    /// first valid `(x, z)` — for chunk `(0, 0)` that is world `(0, 0)`. Its
    /// sibling [`ocean_origin_chunk_moves_the_spawn_to_the_nearest_land`] already
    /// encoded the same rule (`x = 16, z = 0` for chunk `(1, 0)`, i.e. local
    /// `(0, 0)`), so the two were mutually contradictory.
    ///
    /// `(8, 8)` is not lost: it is the *fallback* for a fully-invalid box, which
    /// [`fully_ocean_box_falls_back_to_the_origin_surface`] pins.
    #[test]
    fn plains_origin_chunk_yields_spawn_at_vanillas_first_scanned_position() {
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), land_column(20));
        let spawn = find_initial_spawn(&MapSource { columns });

        assert_eq!(
            spawn.pos.x, 0.0,
            "spawn X is the first x vanilla's chunk scan visits, chunkPos.getMinBlockX()"
        );
        assert_eq!(
            spawn.pos.z, 0.0,
            "spawn Z is the first z of that scan, chunkPos.getMinBlockZ()"
        );
        assert_eq!(spawn.pos.y, 21.0, "spawn Y is one above the surface");
        assert_eq!((spawn.yaw, spawn.pitch), (0.0, 0.0));
    }

    /// The origin column is generated **once**, not twice, for a valid origin.
    ///
    /// [`find_initial_spawn`] queries it for `fallback_y` and then meets it again
    /// as the spiral's first offset. Asking the source twice is invisible behind a
    /// [`crate::ChunkStore`] and very visible without one: it is a doubling of the
    /// generation a joining client waits through before its first chunk, and it
    /// made `tests/serve_play.rs`'s "at most 2 columns before the first encode"
    /// bound unreachable.
    ///
    /// A count, not a duration, and the two hypotheses are exact: `1` if the
    /// column is reused, `2` if it is re-requested.
    #[test]
    fn a_valid_origin_column_is_generated_exactly_once() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingMapSource {
            columns: Mutex<std::collections::HashMap<(i32, i32), ChunkColumn>>,
            calls: AtomicUsize,
        }

        impl ChunkSource for CountingMapSource {
            fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.columns
                    .lock()
                    .expect("map poisoned")
                    .get(&(cx, cz))
                    .cloned()
                    .unwrap_or_else(|| ChunkColumn::new(0, 128))
            }

            fn block_state(&self, x: i32, y: i32, z: i32) -> String {
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                self.column(cx, cz)
                    .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_string()
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                self.column(cx, cz)
                    .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_string()
            }

            fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
        }

        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), land_column(20));
        let source = CountingMapSource {
            columns: Mutex::new(columns),
            calls: AtomicUsize::new(0),
        };

        let spawn = find_initial_spawn(&source);
        // Precondition: the search really did accept the origin. If it fell
        // through to the fallback the count below would be 122 and the reuse
        // would be untested.
        assert_eq!(
            (spawn.pos.x, spawn.pos.z),
            (0.0, 0.0),
            "precondition: the origin chunk must be the accepted candidate"
        );
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            1,
            "the origin column must be generated once and reused for the spiral's (0, 0) \
             candidate; 2 means the reuse was reverted"
        );
    }

    #[test]
    fn ocean_origin_chunk_moves_the_spawn_to_the_nearest_land() {
        // The negative control: an ocean origin must NOT spawn the player
        // under water. Chunk (0, 0) is all water; chunk (1, 0) is land — the
        // second spiral candidate. The spawn must land on that chunk.
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), ocean_column());
        columns.insert((1, 0), land_column(15));
        let spawn = find_initial_spawn(&MapSource { columns });

        assert_eq!(spawn.pos.x, 16.0, "chunk (1, 0) starts at world x=16");
        assert_eq!(spawn.pos.z, 0.0, "chunk (1, 0)'s first z (vanilla's x-then-z scan)");
        assert_eq!(spawn.pos.y, 16.0, "one above the land surface");
    }

    /// Every chunk in the spiral invalid: the search must return vanilla's own
    /// pre-seed, `(8, 64, 8)`.
    ///
    /// **This is the bedrock-burial gate.** The Y assertion is the whole point
    /// and it was missing — this test asserted X and Z only, so the arm that
    /// returned `min_y + 1` was completely uncovered and shipped. Two of four
    /// probe seeds against the real generator take this arm.
    ///
    /// Both hypotheses computed from outside constants: `min_y + 1` is `-63` for
    /// a `(-64, 384)` world (inside the bedrock floor), and
    /// `ChunkGenerator.getSpawnHeight` is `64`.
    #[test]
    fn a_fully_invalid_box_falls_back_above_sea_level_not_into_the_bedrock_floor() {
        const MIN_Y: i32 = -64;
        const HEIGHT: i32 = 384;
        let mut ocean = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                // A real ocean column: bedrock floor, stone, then water to sea
                // level. The bedrock is what the old fallback landed inside.
                ocean.set_block(x, MIN_Y, z, "minecraft:bedrock");
                for y in (MIN_Y + 1)..=(MIN_Y + 3) {
                    ocean.set_block(x, y, z, "minecraft:deepslate");
                }
                for y in (MIN_Y + 4)..=62 {
                    ocean.set_block(x, y, z, "minecraft:water");
                }
            }
        }
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), ocean);
        let source = MapSource { columns };
        let spawn = find_initial_spawn(&source);

        let suspected_wrong = f64::from(MIN_Y + 1);
        let correct = f64::from(GENERATOR_SPAWN_HEIGHT);
        assert_ne!(correct, suspected_wrong, "the two hypotheses must differ");

        assert_eq!(spawn.pos.x, 8.0);
        assert_eq!(spawn.pos.z, 8.0);
        assert_eq!(
            spawn.pos.y, correct,
            "vanilla's `getSpawnHeight` is 64; {suspected_wrong} is `min_y + 1`, which is \
             inside the bedrock floor"
        );

        // The user-visible property, read out of the **same** fixture the search
        // ran against rather than a fresh empty one — and with its own premise
        // check, because "not inside a solid block" is trivially true of any
        // coordinate in an all-air source.
        //
        // Premise: the wrong answer really is inside something solid. If this
        // assertion stops holding the fixture no longer reproduces the defect and
        // the one below proves nothing.
        assert!(
            spawn_face_full_up(&source.block_state(8, MIN_Y + 1, 8)),
            "premise: `min_y + 1` must sit inside a collidable block for this gate to \
             mean anything (it is deepslate in the fixture)"
        );
        assert!(
            !spawn_face_full_up(&source.block_state(8, spawn.pos.y as i32, 8)),
            "the fallback must not place the player inside a collidable block"
        );
    }

    /// **The world-species gate.** Every hermetic fixture above is a column this
    /// module's own test code wrote, so none of them can exercise the thing that
    /// actually broke: what the *production generator* leaves at the surface.
    /// Both defects DESIGN.md §12.129 records were invisible to the whole
    /// fixture suite and visible on the first real seed.
    ///
    /// The expected value originates outside this module in both halves: the
    /// standability predicate is `lodestone-data`'s jar-dumped
    /// `snow_support::face_full_up`, and the fallback height is the literal read
    /// off `ChunkGenerator.getSpawnHeight`. Nothing here compares the search
    /// against another copy of itself.
    ///
    /// `#[ignore]`d: it composes real columns (measured ~1.5 s per seed in
    /// release, and an all-ocean box walks all 121).
    ///
    /// ```text
    /// cargo test --release -p lodestone-server --lib real_generator_spawn -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "composes real generator columns; several seconds per seed"]
    fn real_generator_spawn_is_always_standable_or_the_documented_fallback() {
        // Four seeds chosen because they cover both arms: 0 and 42 find a valid
        // chunk in the box, 1234 and -195764831 have a fully-ocean ±5 box and take
        // the fallback. A single-seed version of this gate would be the *world*
        // species all over again.
        let mut took_fallback = 0usize;
        let mut found_in_box = 0usize;
        for seed in [0_i64, 42, 1234, -195764831] {
            let source = crate::worldgen_data::overworld_chunk_source(seed);
            let spawn = find_initial_spawn(&source);
            let (sx, sy, sz) = (spawn.pos.x as i32, spawn.pos.y as i32, spawn.pos.z as i32);

            let feet = source.block_state(sx, sy, sz);
            let head = source.block_state(sx, sy + 1, sz);
            let support = source.block_state(sx, sy - 1, sz);
            println!(
                "seed {seed}: spawn=({sx}, {sy}, {sz}) support={support} feet={feet} head={head}"
            );

            // Holds on **both** arms, and it is the property the owner's report was
            // about: the player is never inside terrain.
            assert!(
                !spawn_face_full_up(&feet),
                "seed {seed}: spawn feet at ({sx}, {sy}, {sz}) are inside {feet}"
            );
            assert!(
                !spawn_face_full_up(&head),
                "seed {seed}: spawn head at ({sx}, {}, {sz}) is inside {head}",
                sy + 1
            );

            if spawn_face_full_up(&support) {
                // The search-found arm: the block under the player's feet is
                // something vanilla's own `isFaceFull` accepts as standable.
                found_in_box += 1;
            } else {
                // The fallback arm. It is only reached when the whole box is
                // invalid, and then the height is not a search result at all — it
                // is `getSpawnHeight`.
                assert_eq!(
                    (sx, sy, sz),
                    (8, GENERATOR_SPAWN_HEIGHT, 8),
                    "seed {seed}: a spawn with nothing standable beneath it must be \
                     exactly the documented `(8, getSpawnHeight, 8)` fallback, not a \
                     search result hanging in the air"
                );
                took_fallback += 1;
            }
        }
        // Preconditions, not decoration: a run that exercised only one arm would
        // pass while leaving the other completely untested.
        assert!(found_in_box > 0, "no seed exercised the search-found arm");
        assert!(took_fallback > 0, "no seed exercised the fallback arm");
    }

    #[test]
    fn legal_bed_accepts_a_clear_bed_in_reach() {
        let mut column = land_column(20);
        column.set_block(8, 20, 8, "minecraft:red_bed[part=foot,facing=north]");
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), column);
        let source = MapSource { columns };
        let bed = BlockPos::new(8, 20, 8);

        assert!(
            is_legal_bed_respawn(&source, bed, Some(Vec3::new(9.0, 21.0, 8.0))),
            "clear cell above the bed, player one block off it in reach"
        );
        assert!(
            is_legal_bed_respawn(&source, bed, None),
            "no player position yet skips the range test, never rejects"
        );
    }

    #[test]
    fn obstructed_bed_is_illegal() {
        let mut column = land_column(20);
        column.set_block(8, 20, 8, "minecraft:red_bed");
        // A solid block directly above the bed blocks the sleeping AABB.
        column.set_block(8, 21, 8, "minecraft:stone");
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), column);
        let source = MapSource { columns };

        assert!(
            !is_legal_bed_respawn(&source, BlockPos::new(8, 20, 8), Some(Vec3::new(8.0, 21.0, 8.0))),
            "a bed with a block above it must not be accepted"
        );
    }

    #[test]
    fn out_of_reach_bed_is_illegal() {
        let mut column = land_column(20);
        column.set_block(8, 20, 8, "minecraft:red_bed");
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), column);
        let source = MapSource { columns };

        assert!(
            !is_legal_bed_respawn(&source, BlockPos::new(8, 20, 8), Some(Vec3::new(8.0, 20.0, 40.0))),
            "a bed 32 blocks away in z is far past the ±3 reach"
        );
        assert!(
            !is_legal_bed_respawn(&source, BlockPos::new(8, 20, 8), Some(Vec3::new(8.0, 40.0, 8.0))),
            "a bed 20 blocks below is far past the ±2 vertical reach"
        );
    }

    #[test]
    fn non_bed_click_is_never_a_respawn_point() {
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), land_column(20));
        let source = MapSource { columns };

        assert!(
            !is_legal_bed_respawn(&source, BlockPos::new(8, 20, 8), Some(Vec3::new(8.0, 21.0, 8.0))),
            "plain stone at the click is not a bed, even clear and in reach"
        );
    }

    #[test]
    fn is_bed_block_recognises_every_bed_and_nothing_else() {
        assert!(is_bed_block("minecraft:red_bed[part=head,facing=north]"));
        assert!(is_bed_block("minecraft:white_bed"));
        assert!(is_bed_block("minecraft:black_bed"));
        assert!(!is_bed_block("minecraft:stone"));
        assert!(!is_bed_block("minecraft:air"));
        assert!(!is_bed_block("minecraft:bedrock"), "bedrock ends in -rock, not -bed");
        assert!(!is_bed_block("minecraft:respawn_anchor"));
    }
    // ---------------------------------------------------------------------
    // `resolve_bed_respawn` — the *read* half. The point was already written
    // and validated at set time; nothing consulted it on death, so a player
    // always woke where they died.
    // ---------------------------------------------------------------------

    /// A bed on open stone resolves to a standable cell beside it — and to the
    /// **first** offset in vanilla's own order, not just to any of the twelve.
    ///
    /// `side` is `forward.getClockWise()`, so for a north-facing bed
    /// (`forward = (0, -1)`) `side` is `(1, 0)` and the first offset
    /// `{side.getStepX(), side.getStepZ()}` is one cell east. That is a value
    /// predicted from the transcribed offset table, not from running this code:
    /// an implementation that iterated the offsets in any other order would land
    /// somewhere else and fail here.
    #[test]
    fn a_bed_on_open_ground_resolves_to_the_first_vanilla_offset() {
        let mut column = land_column(20);
        column.set_block(8, 21, 8, "minecraft:red_bed[facing=north,part=foot]");
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), column);
        let source = MapSource { columns };

        let resolved = resolve_bed_respawn(&source, RespawnPoint {
            pos: BlockPos::new(8, 21, 8),
        });
        assert_eq!(
            resolved,
            Some(Vec3::new(9.5, 21.0, 8.5)),
            "forward=(0,-1) makes side=(1,0), so the first offset is one cell east \
             of the bed at the bed's own y, centred in x/z"
        );
    }

    /// **The load-bearing case, and the one the missing read produced.** A bed
    /// that has been broken since the point was set must resolve to `None`, so the
    /// caller falls back to the world spawn — vanilla's `Optional.empty()`, which
    /// it answers with `NO_RESPAWN_BLOCK_AVAILABLE`.
    ///
    /// Trusting the stored point instead would teleport the player into whatever
    /// replaced their bed. Note the position here is *identical* to the passing
    /// case above: only the block changed, which is what makes this a test of the
    /// re-read rather than of the coordinates.
    #[test]
    fn a_broken_bed_resolves_to_nothing() {
        let mut column = land_column(20);
        // Where the bed used to be. Someone has since built a wall there.
        column.set_block(8, 21, 8, "minecraft:stone");
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), column);
        let source = MapSource { columns };

        assert_eq!(
            resolve_bed_respawn(&source, RespawnPoint {
                pos: BlockPos::new(8, 21, 8)
            }),
            None,
            "a stored point whose bed is gone must be refused, not used"
        );
    }

    /// A bed walled in on every side resolves to `None` too — the other
    /// `Optional.empty()` arm, and the control that proves the offset walk is
    /// actually testing each candidate rather than returning the first one
    /// unconditionally.
    ///
    /// Filled with stone from the bed's own y upward across the whole column, so
    /// every one of the twelve offsets (including the two on the bed itself) has a
    /// solid cell where the player's feet would go.
    #[test]
    fn a_walled_in_bed_resolves_to_nothing() {
        let mut column = land_column(20);
        for x in 0..16 {
            for z in 0..16 {
                for y in 21..=23 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        column.set_block(8, 21, 8, "minecraft:red_bed[facing=north,part=foot]");
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), column);
        let source = MapSource { columns };

        assert_eq!(
            resolve_bed_respawn(&source, RespawnPoint {
                pos: BlockPos::new(8, 21, 8)
            }),
            None,
            "every offset is obstructed, so there is nowhere to stand"
        );
    }

    /// The bed's `facing` really is read: a south-facing bed's `side` is the
    /// opposite of a north-facing one's, so the same bed at the same position
    /// resolves to a *different* cell. Without this, a hardcoded offset would pass
    /// the first gate above.
    #[test]
    fn the_beds_facing_decides_which_side_the_player_wakes_on() {
        let resolve = |facing: &str| {
            let mut column = land_column(20);
            column.set_block(8, 21, 8, &format!("minecraft:red_bed[facing={facing},part=foot]"));
            let mut columns = std::collections::HashMap::new();
            columns.insert((0, 0), column);
            resolve_bed_respawn(&MapSource { columns }, RespawnPoint {
                pos: BlockPos::new(8, 21, 8),
            })
        };
        // north: forward=(0,-1), side=clockwise=(1,0)  -> east
        assert_eq!(resolve("north"), Some(Vec3::new(9.5, 21.0, 8.5)));
        // south: forward=(0,1),  side=clockwise=(-1,0) -> west
        assert_eq!(resolve("south"), Some(Vec3::new(7.5, 21.0, 8.5)));
        // east:  forward=(1,0),  side=clockwise=(0,1)  -> south
        assert_eq!(resolve("east"), Some(Vec3::new(8.5, 21.0, 9.5)));
    }
}
