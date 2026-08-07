//! The world spawn point and per-player respawn points (issue #329).
//!
//! Before this module, the world spawn was derived inline per connection
//! (`serve_connection`'s `ConfigurationFinished` arm): the origin column's
//! surface at local `(8, 8)`, i.e. always `(8, y, 8)` — issue #461 replaced
//! the Y with terrain, but the X/Z were still fixed, so a world whose origin
//! chunk is ocean spawned the player under water and no search ever moved
//! them. This module is vanilla's own search:
//!
//! * [`find_initial_spawn`] runs `MinecraftServer.setInitialSpawn`'s
//!   121-iteration, ±5-chunk spiral (`MinecraftServer.java:480-532`) over a
//!   [`ChunkSource`], stopping at the first chunk that contains a valid spawn
//!   position (`PlayerSpawnFinder.getSpawnPosInChunk`).
//! * A per-column candidate is vanilla's `getLevelRespawnPos`
//!   (`PlayerSpawnFinder.java:148`): the surface height, with a fluid between
//!   sky and ground (an ocean column) aborting the candidate.
//! * The **per-player** half — a player's bed respawn point
//!   ([`RespawnPoint`]) with the set-time legality check vanilla applies
//!   before accepting one ([`is_legal_bed_respawn`]) — mirrors
//!   `ServerPlayer.startSleepInBed`'s validation
//!   (`ServerPlayer.java:1186-1240`). The point is *stored*, never yet
//!   *used*: resolving a death against it (bed wins over the world spawn,
//!   then the placement teleport, the `respawn_radius` scatter and the async
//!   chunk-ticket search of `PlayerSpawnFinder.findSpawn`) is the deferred
//!   half — it needs shape-B player state and #297's ticket system (see
//!   `docs/plans/world-state.md` unit P2).

use lodestone_model::{BlockPos, Vec3};

use crate::chunk::{ChunkColumn, ChunkSource, is_air_or_fluid};

/// The world's spawn point — vanilla's `LevelData.RespawnData` for the
/// overworld (`PrimaryLevelData.java:250-267`): a position plus the yaw/pitch
/// a player is teleported with. The initial world spawn has both rotations
/// zero (`setInitialSpawn` passes `0.0F, 0.0F`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WorldSpawn {
    /// World-space feet position, in blocks.
    pub pos: Vec3,
    /// Spawn yaw in degrees.
    pub yaw: f32,
    /// Spawn pitch in degrees.
    pub pitch: f32,
}

/// A player's per-player respawn point — the bed they last slept in (issue
/// #329, the tracking half). Vanilla stores this per player as
/// `ServerPlayer.RespawnConfig` (a `RespawnData` plus a `forced` flag) and
/// consults it on death before falling back to the level spawn
/// (`ServerPlayer.java:1012`, `PlayerList.respawn`).
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

/// The height a player stands at for a `getLevelRespawnPos`-valid column
/// position `(lx, lz)` in `[0..16)`, or `None` when the column is invalid
/// there.
///
/// This is vanilla's `PlayerSpawnFinder.getLevelRespawnPos`
/// (`.cache/mc/26.2/src/net/minecraft/server/level/PlayerSpawnFinder.java:148`),
/// simplified to what a [`ChunkColumn`] can answer — it has no persisted
/// heightmaps, so the top-of-column scan below is the `MOTION_BLOCKING`
/// heightmap query's analogue:
///
/// 1. Scan downward from the top of the column.
/// 2. A fluid encountered before any solid block (an ocean column, or a lava
///    lake) aborts the candidate — vanilla's `break` on a non-empty fluid
///    state at `:168-171`, which yields `null`.
/// 3. The first solid block from the top is the surface; return one block
///    above it (`return pos.above().immutable()` at `:177`), the feet
///    position.
/// 4. A column with no solid block at all (air/void world) is `None`.
///
/// The "solid" test is [`is_air_or_fluid`]'s negation — the same block-name
/// solidity this crate uses for placement — rather than vanilla's full-top-
/// face collision shape, because this crate serves no generator output with
/// partial-block surfaces at spawn (the `worldgen_data` scope note: no
/// vegetation at surface).
fn get_level_respawn_pos(column: &ChunkColumn, lx: i32, lz: i32) -> Option<i32> {
    let top_y = column.min_y + column.height - 1;
    for y in (column.min_y..=top_y).rev() {
        let state = column.block_state(lx, y, lz);
        let base = state.split('[').next().unwrap_or(state);
        if matches!(base, "minecraft:water" | "minecraft:lava") {
            // Fluid between sky and ground — an ocean column. Fail-closed,
            // exactly like vanilla's `null`: the caller keeps searching.
            return None;
        }
        if !is_air_or_fluid(state) {
            return Some(y + 1);
        }
    }
    None
}

/// Scans one already-generated column's 256 block positions in vanilla's order
/// (`for x … for z`, `PlayerSpawnFinder.getSpawnPosInChunk`, `:182-190`) and
/// returns the first valid spawn position's world `BlockPos`, or `None` if the
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
fn get_spawn_pos_in_chunk<S: ChunkSource>(source: &S, cx: i32, cz: i32) -> Option<BlockPos> {
    spawn_pos_in_column(&source.column(cx, cz), cx, cz)
}

/// The 121 chunk offsets the initial-spawn spiral visits, in vanilla order.
///
/// Transcribed from `MinecraftServer.setInitialSpawn`'s loop
/// (`MinecraftServer.java:504-520`): it starts at `(0, 0)`, steps with an
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
/// `MinecraftServer.setInitialSpawn` (`MinecraftServer.java:480-532`).
///
/// Vanilla's first step — `chunkSource.randomState().sampler()
/// .findSpawnPosition()` — picks a spawn *chunk* from climate noise; that
/// sampler is `lodestone-worldgen` deep machinery this crate does not expose,
/// so the search centres on the origin chunk `(0, 0)`, which is exactly the
/// result a `Climate.Sampler` with an empty spawn target returns
/// (`Climate.java:501`). The consequence is honest and documented: with no
/// climate picker the *choice* of centre is fixed, but the spiral that finds
/// a valid surface *within* that ±5-chunk box is real, which is the piece
/// that was missing (an ocean origin chunk now moves the spawn to the nearest
/// land instead of spawning the player under water).
///
/// Returns the first valid spawn position in spiral order, or — when every
/// chunk in the box is invalid (a full-ocean box) — the origin column's
/// surface at local `(8, 8)`, vanilla's own fallback (`setInitialSpawn`
/// pre-seeds `levelData.setSpawn` with `offset(8, height, 8)` before the
/// loop and the loop only overrides it with a *valid* find).
///
/// # Column generations, because this is on the join critical path
///
/// This runs in `crate::server`'s `ConfigurationFinished` arm **before** the
/// chunk-streaming ring loop, so every column it generates is time the client
/// spends with no terrain (issue #453's time-to-first-chunk). For the normal case
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
pub(crate) fn find_initial_spawn<S: ChunkSource>(source: &S) -> WorldSpawn {
    let origin = source.column(0, 0);
    // `min_y + 1` when the origin is invalid (ocean/void) is the pre-#329
    // `spawn_surface_y` fallback, kept so a pathological world spawns at the
    // same `(8, min_y + 1, 8)` it did before the search existed — a silent
    // Y regression is worse than a fixed X/Z.
    let fallback_y = get_level_respawn_pos(&origin, 8, 8).unwrap_or(origin.min_y + 1);

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
/// point — issue #329's own requirement ("beds/anchors validated for a legal
/// respawn spot before being accepted").
///
/// This is the set-time half of vanilla's `ServerPlayer.startSleepInBed`
/// (`ServerPlayer.java:1186-1240`), reduced to what this crate's interaction
/// scope can answer:
///
/// 1. The clicked block is a bed ([`is_bed_block`]).
/// 2. The cell directly above the bed is clear — vanilla's obstruction
///    rejection, the `level.noCollision(boundingBox)` of the sleeping AABB at
///    `:1203`. A solid block above the bed makes the spot illegal.
/// 3. The player is in reach of the bed — vanilla's `bedInRange`
///    (`:1195-1198`), bed ±3 x/z and ±2 y. `player_pos` is `None` until the
///    first [`crate::server`]`::PlayerMoved` packet; a click before any move
///    skips the range test (cannot be wrong about a position it never had).
///
/// The fourth check vanilla applies — a `NOT_SAFE` monster within ±8 h / ±5 v
/// of the bed (`:1218-1227`), skipped only in creative — is the documented
/// remainder: it needs a mob-AABB query this crate's interaction scope does
/// not carry (shape-B world state; see this module's doc). Without it a bed
/// in monster range is accepted, a gap the placement half of P2 will close.
pub(crate) fn is_legal_bed_respawn<S: ChunkSource>(
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

    /// A valid origin chunk is accepted by the spiral's *first* candidate, and the
    /// position inside it is vanilla's scan order — local `(0, 0)`, **not** the
    /// centre.
    ///
    /// This test previously expected `(8, 8)` and named itself
    /// `plains_origin_chunk_yields_spawn_at_local_8_8`, transcribed from the
    /// pre-#329 hardcode it replaced. It had never passed: the search and the
    /// expectation landed in the same commit (`43e096b`), and `(8, 8)` is not what
    /// the search returns for a valid chunk.
    ///
    /// `(0, 0)` is read off `PlayerSpawnFinder.getSpawnPosInChunk`
    /// (`.cache/mc/26.2/src/net/minecraft/server/level/PlayerSpawnFinder.java:183-190`),
    /// which scans from `chunkPos.getMinBlockX()`/`getMinBlockZ()` and returns the
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

    #[test]
    fn fully_ocean_box_falls_back_to_the_origin_surface() {
        // Every chunk in the spiral invalid: the search must return vanilla's
        // own fallback, the origin column's surface at local (8, 8) — not a
        // panic, and not an underwater spawn below the waterline.
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), ocean_column());
        let spawn = find_initial_spawn(&MapSource { columns });

        assert_eq!(spawn.pos.x, 8.0);
        assert_eq!(spawn.pos.z, 8.0);
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
}
