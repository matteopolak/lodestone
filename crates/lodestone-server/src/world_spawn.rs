//! World and per-player respawn selection.
//!
//! A fresh world searches a bounded spiral for the first safe surface and
//! persists the result. Bed respawns are validated again when used, falling
//! back to the world spawn when the bed or its clearance is no longer valid.

use lodestone_model::{BlockPos, Vec3};
use lodestone_data::block::Block;

use std::sync::atomic::{AtomicU64, Ordering};

use crate::chunk::{ChunkColumn, ChunkSource, is_air_or_fluid_id};

type BlockStateId = lodestone_data::block_states::StateId;

type SpawnAabb = lodestone_data::collision_shapes::Aabb;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpawnSearchMetrics {
    /// Completed searches.
    pub searches: u64,
    /// Candidate chunks inspected.
    pub candidate_chunks: u64,
    /// Full columns requested.
    pub columns_requested: u64,
    /// Cheap surface samples read before a full-column request.
    pub horizon_samples: u64,
    /// Searches that found a valid candidate.
    pub accepted: u64,
    /// Searches that used the fallback anchor.
    pub fallbacks: u64,
    /// Aggregate search time.
    pub elapsed_nanos: u64,
    /// Calls into the store's raw ensure boundary.
    pub raw_ensure_calls: u64,
    /// Requests that became the in-flight generation leader.
    pub request_session_leaders: u64,
    /// Request results supplied by an existing resident or persisted column.
    pub existing_hits: u64,
    /// Packet-neighbour columns committed with a generated request.
    pub packet_neighbour_admissions: u64,
}

static SEARCHES: AtomicU64 = AtomicU64::new(0);
static CANDIDATE_CHUNKS: AtomicU64 = AtomicU64::new(0);
static COLUMNS_REQUESTED: AtomicU64 = AtomicU64::new(0);
static HORIZON_SAMPLES: AtomicU64 = AtomicU64::new(0);
static ACCEPTED: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);
static ELAPSED_NANOS: AtomicU64 = AtomicU64::new(0);
static RAW_ENSURE_CALLS: AtomicU64 = AtomicU64::new(0);
static REQUEST_SESSION_LEADERS: AtomicU64 = AtomicU64::new(0);
static EXISTING_HITS: AtomicU64 = AtomicU64::new(0);
static PACKET_NEIGHBOUR_ADMISSIONS: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn spawn_search_metrics() -> SpawnSearchMetrics {
    SpawnSearchMetrics {
        searches: SEARCHES.load(Ordering::Relaxed),
        candidate_chunks: CANDIDATE_CHUNKS.load(Ordering::Relaxed),
        columns_requested: COLUMNS_REQUESTED.load(Ordering::Relaxed),
        horizon_samples: HORIZON_SAMPLES.load(Ordering::Relaxed),
        accepted: ACCEPTED.load(Ordering::Relaxed),
        fallbacks: FALLBACKS.load(Ordering::Relaxed),
        elapsed_nanos: ELAPSED_NANOS.load(Ordering::Relaxed),
        raw_ensure_calls: RAW_ENSURE_CALLS.load(Ordering::Relaxed),
        request_session_leaders: REQUEST_SESSION_LEADERS.load(Ordering::Relaxed),
        existing_hits: EXISTING_HITS.load(Ordering::Relaxed),
        packet_neighbour_admissions: PACKET_NEIGHBOUR_ADMISSIONS.load(Ordering::Relaxed),
    }
}

pub(crate) fn record_raw_ensure() {
    RAW_ENSURE_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_request_session_leader() {
    REQUEST_SESSION_LEADERS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_existing_hit() {
    EXISTING_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_packet_neighbour_admissions(count: usize) {
    PACKET_NEIGHBOUR_ADMISSIONS.fetch_add(
        u64::try_from(count).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

fn publish_spawn_search_metrics(metrics: SpawnSearchMetrics) {
    SEARCHES.fetch_add(metrics.searches, Ordering::Relaxed);
    CANDIDATE_CHUNKS.fetch_add(metrics.candidate_chunks, Ordering::Relaxed);
    COLUMNS_REQUESTED.fetch_add(metrics.columns_requested, Ordering::Relaxed);
    HORIZON_SAMPLES.fetch_add(metrics.horizon_samples, Ordering::Relaxed);
    ACCEPTED.fetch_add(metrics.accepted, Ordering::Relaxed);
    FALLBACKS.fetch_add(metrics.fallbacks, Ordering::Relaxed);
    ELAPSED_NANOS.fetch_add(metrics.elapsed_nanos, Ordering::Relaxed);
}

/// A persisted world spawn and the rotation applied during teleportation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WorldSpawn {
    /// World-space block-aligned anchor, in blocks.
    ///
    /// The saved level-data form is an integer block position. A player is
    /// placed at that block's bottom centre through
    /// [`player_position_for_spawn_anchor`], keeping the persisted anchor and
    /// the entity's feet position distinct.
    pub pos: Vec3,
    /// Spawn yaw in degrees.
    pub yaw: f32,
    /// Spawn pitch in degrees.
    pub pitch: f32,
}

/// Converts a persisted world-spawn block anchor into the feet position used
/// by the player entity and its initial teleport.
///
/// The spawn contract stores a [`BlockPos`] but places the player at
/// the block's horizontal centre. Keeping the conversion at this seam avoids
/// probing one column while placing the player on a block boundary, where the
/// player's 0.6-block body can overlap a neighbouring column.
#[must_use]
pub(crate) fn player_position_for_spawn_anchor(anchor: Vec3) -> Vec3 {
    Vec3::new(anchor.x.floor() + 0.5, anchor.y.floor(), anchor.z.floor() + 0.5)
}

/// The bed block used as a player's preferred respawn point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RespawnPoint {
    /// The bed block's position (the half the player clicked).
    pub pos: BlockPos,
}

/// Noise worlds use a fixed fallback two blocks above sea level.
const GENERATOR_SPAWN_HEIGHT: i32 = 64;

/// The safe standing height at local `(lx, lz)`, or `None` for an invalid column.
/// The scan rejects fluid above the first full support block and returns the
/// block immediately above that support.
fn get_level_respawn_pos(column: &ChunkColumn, lx: i32, lz: i32) -> Option<i32> {
    let top_y = column.min_y + column.height - 1;
    let scan_top = column
        .motion_blocking()
        .and_then(|map| map.get((lx + lz * 16) as usize).copied())
        .map(|stored| column.min_y + i32::from(stored) - 1)
        .map_or(top_y, |y| y.min(top_y));
    for y in (column.min_y..=scan_top).rev() {
        let state = column.block_state_id(lx, y, lz);
        if spawn_has_fluid_state(state) {
            return None;
        }
        if spawn_face_full_up(state) {
            return Some(y + 1);
        }
    }
    None
}

/// The authoritative collision boxes for a validated built-in state.
fn spawn_collision_boxes(state: BlockStateId) -> &'static [SpawnAabb] {
    lodestone_data::collision_shapes::collision_boxes(state)
}

/// Whether the player's standing body fits at an integer column position.
///
/// The position is the block centre at `(lx + 0.5, y, lz + 0.5)`, with the
/// measured player dimensions of `0.6` blocks wide and `1.8` blocks tall. The
/// The helper below performs the same test for saved positions
/// that may have fractional coordinates.
fn spawn_position_is_clear_in_column(
    column: &ChunkColumn,
    lx: i32,
    lz: i32,
    y: i32,
) -> bool {
    spawn_aabb_is_clear(
        |x, block_y, z| column.block_state_id(x, block_y, z),
        Vec3::new(
            f64::from(lx) + 0.5,
            f64::from(y),
            f64::from(lz) + 0.5,
        ),
    )
}

/// Whether a player-sized body at `pos` overlaps a known block collider or a
/// fluid cell.
///
/// This is the same geometric question used by the server's authoritative
/// player-placement check: collision boxes are translated from block-local
/// coordinates into world coordinates and compared with strict AABB overlap,
/// while any fluid in the body footprint rejects the position even when that
/// fluid has no collision boxes. A missing state in the census is fail-closed.
fn spawn_aabb_is_clear(
    mut state_at: impl FnMut(i32, i32, i32) -> BlockStateId,
    pos: Vec3,
) -> bool {
    const HALF_WIDTH: f64 = 0.3;
    const HEIGHT: f64 = 1.8;

    if !pos.x.is_finite() || !pos.y.is_finite() || !pos.z.is_finite() {
        return false;
    }

    let min_x = pos.x - HALF_WIDTH;
    let max_x = pos.x + HALF_WIDTH;
    let min_y = pos.y;
    let max_y = pos.y + HEIGHT;
    let min_z = pos.z - HALF_WIDTH;
    let max_z = pos.z + HALF_WIDTH;

    let x0 = min_x.floor() as i32;
    let x1 = max_x.ceil() as i32;
    let y0 = min_y.floor() as i32;
    let y1 = max_y.ceil() as i32;
    let z0 = min_z.floor() as i32;
    let z1 = max_z.ceil() as i32;

    for x in x0..x1 {
        for block_y in y0..y1 {
            for z in z0..z1 {
                let state = state_at(x, block_y, z);
                if spawn_has_fluid_state(state) {
                    return false;
                }
                let boxes = spawn_collision_boxes(state);
                for block in boxes {
                    let block_min_x = f64::from(x) + f64::from(block.min[0]);
                    let block_max_x = f64::from(x) + f64::from(block.max[0]);
                    let block_min_y = f64::from(block_y) + f64::from(block.min[1]);
                    let block_max_y = f64::from(block_y) + f64::from(block.max[1]);
                    let block_min_z = f64::from(z) + f64::from(block.min[2]);
                    let block_max_z = f64::from(z) + f64::from(block.max[2]);
                    if min_x < block_max_x
                        && max_x > block_min_x
                        && min_y < block_max_y
                        && max_y > block_min_y
                        && min_z < block_max_z
                        && max_z > block_min_z
                    {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Checks a world-space position against a source's live block states.
///
/// `pub(crate)` lets the integrated join path validate a restored player
/// position without duplicating the AABB/liquid logic used by the fresh-spawn
/// search. The source is queried only for the cells touched by the player's
/// body, not for a whole chunk.
pub(crate) fn is_spawn_position_clear<S: ChunkSource + ?Sized>(
    source: &S,
    pos: Vec3,
) -> bool {
    spawn_aabb_is_clear(|x, y, z| source.block_state_id(x, y, z), pos)
}

/// Whether a block state carries a fluid.
fn spawn_has_fluid_state(state: BlockStateId) -> bool {
    lodestone_data::snow_support::has_fluid_state(state)
}

/// Whether a block state has a full upper support face.
fn spawn_face_full_up(state: BlockStateId) -> bool {
    lodestone_data::snow_support::face_full_up(state)
}

/// Returns the first safe position in x-then-z order.
fn spawn_pos_in_column(column: &ChunkColumn, cx: i32, cz: i32) -> Option<BlockPos> {
    for lx in 0..16 {
        for lz in 0..16 {
            if let Some(y) = get_level_respawn_pos(column, lx, lz) {
                // The surface finder and the body-clearance predicate are
                // The surface and body checks are separate: a column's first
                // surface is the only candidate, and an obstructed body moves
                // on to the next column rather than searching for a lower
                // surface in this one.
                if spawn_position_is_clear_in_column(column, lx, lz, y) {
                    return Some(BlockPos::new(cx * 16 + lx, y, cz * 16 + lz));
                }
            }
        }
    }
    None
}

/// Whether the cheap horizon classifies every horizontal cell in this candidate
/// as water. A dry sample keeps the candidate in the exact block-state search,
/// while an unavailable sample is treated conservatively as unknown. This is
/// only a negative hint: the full spawn predicate still decides every candidate
/// that is not wholly classified as water.
fn horizon_is_all_water<S: ChunkSource + ?Sized>(
    source: &S,
    cx: i32,
    cz: i32,
    samples: &mut u64,
) -> bool {
    (0..16).all(|lx| {
        (0..16).all(|lz| {
            *samples += 1;
            source
                .horizon_sample(cx * 16 + lx, cz * 16 + lz)
                .is_some_and(|sample| sample.water_y.is_some())
        })
    })
}

/// [`spawn_pos_in_column`] for a chunk that is not yet in hand.
fn get_spawn_pos_in_chunk<S: ChunkSource + ?Sized>(
    source: &S,
    cx: i32,
    cz: i32,
    metrics: &mut SpawnSearchMetrics,
) -> Option<BlockPos> {
    // A fresh integrated world can have an ocean origin and a completely
    // water-filled ±5 search box. The horizon path is deliberately only a
    // negative hint: unknown sources and any dry sample still pay for the
    // authoritative column, while a fully-water hint avoids 120 full
    if horizon_is_all_water(source, cx, cz, &mut metrics.horizon_samples) {
        return None;
    }
    metrics.columns_requested += 1;
    spawn_pos_in_column(&source.column(cx, cz), cx, cz)
}

/// Finds a clear fallback height in the origin column.
///
/// The generator's preferred fallback remains the first choice so an
/// all-ocean world keeps its open-water spawn. If that height is blocked by
/// terrain, fluid, or an unknown state, the search climbs to the first height
/// where the complete player body fits. The top sentinel is one row above the
/// column, whose out-of-range state is air, so even a fully solid column gets
/// a finite, collision-free answer.
fn fallback_spawn_y(column: &ChunkColumn, lx: i32, lz: i32, preferred: i32) -> i32 {
    if spawn_position_is_clear_in_column(column, lx, lz, preferred) {
        return preferred;
    }

    let top = column.min_y.saturating_add(column.height);
    if preferred < top {
        for y in preferred.saturating_add(1)..=top {
            if spawn_position_is_clear_in_column(column, lx, lz, y) {
                return y;
            }
        }
    }
    if preferred > column.min_y {
        for y in (column.min_y..preferred).rev() {
            if spawn_position_is_clear_in_column(column, lx, lz, y) {
                return y;
            }
        }
    }
    top
}

/// The 121 offsets in canonical initial-spawn order.
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

/// Searches a bounded spiral from the origin and returns a safe fallback when
/// every candidate chunk is invalid.
pub(crate) fn find_initial_spawn<S: ChunkSource + ?Sized>(source: &S) -> WorldSpawn {
    use lodestone_time::Instant;

    let started = Instant::now();
    let mut metrics = SpawnSearchMetrics {
        searches: 1,
        ..SpawnSearchMetrics::default()
    };
    let result = (|| {
        metrics.columns_requested += 1;
        let origin = source.column(0, 0);
        let fallback_y = GENERATOR_SPAWN_HEIGHT;

        for (xo, zo) in spiral_chunk_offsets() {
            metrics.candidate_chunks += 1;
            let candidate = if (xo, zo) == (0, 0) {
                spawn_pos_in_column(&origin, 0, 0)
            } else {
                get_spawn_pos_in_chunk(source, xo, zo, &mut metrics)
            };
            if let Some(pos) = candidate {
                metrics.accepted += 1;
                return WorldSpawn {
                    pos: Vec3::new(pos.x as f64, pos.y as f64, pos.z as f64),
                    yaw: 0.0,
                    pitch: 0.0,
                };
            }
        }

        metrics.fallbacks += 1;
        let fallback_y = fallback_spawn_y(&origin, 8, 8, fallback_y);
        WorldSpawn {
            pos: Vec3::new(8.0, fallback_y as f64, 8.0),
            yaw: 0.0,
            pitch: 0.0,
        }
    })();
    metrics.elapsed_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    tracing::debug!(
        elapsed_ms = metrics.elapsed_nanos as f64 / 1_000_000.0,
        candidate_chunks = metrics.candidate_chunks,
        columns_requested = metrics.columns_requested,
        horizon_samples = metrics.horizon_samples,
        fallback = metrics.fallbacks != 0,
        spawn = ?result.pos,
        "initial spawn search complete"
    );
    publish_spawn_search_metrics(metrics);
    result
}

#[cfg(target_arch = "wasm32")]
async fn spawn_column_yielding<S: ChunkSource + ?Sized>(
    source: &S,
    cx: i32,
    cz: i32,
) -> ChunkColumn {
    use crate::worldgen_session::{GenerationRequest, GenerationRequestResult, GenerationSession};
    use lodestone_worldgen::stage_schedule::GenerationTarget;

    let target = GenerationTarget::Full;
    let request = GenerationRequest::new(
        source
            .dimension()
            .unwrap_or(crate::dimension::Dimension::Overworld)
            .into(),
        (cx, cz),
        target,
        source.generation_request_dependency_radius(target),
    );
    let mut session = GenerationSession::new(request);
    let generated = match source
        .request_generation_yielding(request, Some(&mut session))
        .await
    {
        Ok(Some(GenerationRequestResult::Existing(column))) => column,
        Ok(Some(GenerationRequestResult::Generated(snapshot))) => snapshot.column().clone(),
        Ok(None) => source.column(cx, cz),
        Err(error) => {
            tracing::warn!(cx, cz, %error, "yielding spawn column generation failed");
            source.column(cx, cz)
        }
    };
    source.resident_column(cx, cz).unwrap_or(generated)
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn find_initial_spawn_yielding<S: ChunkSource + ?Sized>(source: &S) -> WorldSpawn {
    use lodestone_time::Instant;

    let started = Instant::now();
    let mut metrics = SpawnSearchMetrics {
        searches: 1,
        ..SpawnSearchMetrics::default()
    };
    metrics.columns_requested += 1;
    let origin = spawn_column_yielding(source, 0, 0).await;
    let mut accepted = None;

    for (xo, zo) in spiral_chunk_offsets() {
        metrics.candidate_chunks += 1;
        let candidate = if (xo, zo) == (0, 0) {
            spawn_pos_in_column(&origin, 0, 0)
        } else if horizon_is_all_water(source, xo, zo, &mut metrics.horizon_samples) {
            None
        } else {
            metrics.columns_requested += 1;
            let column = spawn_column_yielding(source, xo, zo).await;
            spawn_pos_in_column(&column, xo, zo)
        };
        if let Some(pos) = candidate {
            metrics.accepted += 1;
            accepted = Some(WorldSpawn {
                pos: Vec3::new(pos.x as f64, pos.y as f64, pos.z as f64),
                yaw: 0.0,
                pitch: 0.0,
            });
            break;
        }
        crate::chunk::yield_to_browser().await;
    }

    let result = accepted.unwrap_or_else(|| {
        metrics.fallbacks += 1;
        WorldSpawn {
            pos: Vec3::new(
                8.0,
                fallback_spawn_y(&origin, 8, 8, GENERATOR_SPAWN_HEIGHT) as f64,
                8.0,
            ),
            yaw: 0.0,
            pitch: 0.0,
        }
    });
    metrics.elapsed_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    tracing::debug!(
        elapsed_ms = metrics.elapsed_nanos as f64 / 1_000_000.0,
        candidate_chunks = metrics.candidate_chunks,
        columns_requested = metrics.columns_requested,
        horizon_samples = metrics.horizon_samples,
        fallback = metrics.fallbacks != 0,
        spawn = ?result.pos,
        "initial spawn search complete"
    );
    publish_spawn_search_metrics(metrics);
    result
}

/// Whether a block state belongs to the bed family.
pub(crate) fn is_bed_block(state: BlockStateId) -> bool {
    matches!(
        state.block(),
        Block::WhiteBed
            | Block::OrangeBed
            | Block::MagentaBed
            | Block::LightBlueBed
            | Block::YellowBed
            | Block::LimeBed
            | Block::PinkBed
            | Block::GrayBed
            | Block::LightGrayBed
            | Block::CyanBed
            | Block::PurpleBed
            | Block::BlueBed
            | Block::BrownBed
            | Block::GreenBed
            | Block::RedBed
            | Block::BlackBed
    )
}

/// Whether right-clicking the bed at `bed` should set the player's respawn
/// point — beds/anchors need to be validated for a legal respawn spot before
/// being accepted.
///
/// A bed is usable when its block and clearance are valid and the player is in
/// reach when a player position is available.
pub(crate) fn is_legal_bed_respawn<S: ChunkSource + ?Sized>(
    source: &S,
    bed: BlockPos,
    player_pos: Option<Vec3>,
) -> bool {
    if !is_bed_block(source.block_state_id(bed.x, bed.y, bed.z)) {
        return false;
    }
    if !is_air_or_fluid_id(source.block_state_id(bed.x, bed.y + 1, bed.z)) {
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

/// Ordered offsets used to find a safe position beside a bed.
const BED_STAND_UP_OFFSETS: [(i32, i32); 12] = [
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
    (0, 0),
    (-1, 0),
];

/// The `(dx, dz)` step vector for a bed's `facing` property, or `None` for a
/// state carrying no recognisable facing.
fn bed_facing_steps(state: BlockStateId) -> Option<(i32, i32)> {
    let facing = state
        .properties()
        .iter()
        .find_map(|(key, value)| (*key == "facing").then_some(*value))?;
    match facing {
        "north" => Some((0, -1)),
        "south" => Some((0, 1)),
        "west" => Some((-1, 0)),
        "east" => Some((1, 0)),
        _ => None,
    }
}

/// Whether a player can stand at `pos` using the available block-state facts.
pub(crate) fn is_standable<S: ChunkSource + ?Sized>(source: &S, pos: BlockPos) -> bool {
    let feet = source.block_state_id(pos.x, pos.y, pos.z);
    let head = source.block_state_id(pos.x, pos.y + 1, pos.z);
    let below = source.block_state_id(pos.x, pos.y - 1, pos.z);
    is_air_or_fluid_id(feet) && is_air_or_fluid_id(head) && spawn_face_full_up(below)
}

/// Resolves a stored bed point by re-reading the bed and checking ordered
/// stand-up positions. Returns `None` when the bed or every candidate is unusable.
pub(crate) fn resolve_bed_respawn<S: ChunkSource + ?Sized>(
    source: &S,
    point: RespawnPoint,
) -> Option<Vec3> {
    let bed = point.pos;
    let state = source.block_state_id(bed.x, bed.y, bed.z);
    if !is_bed_block(state) {
        return None;
    }
    let (fx, fz) = bed_facing_steps(state).unwrap_or((0, -1));
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
    None
}

// Test source with explicit columns and air outside the fixture.
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

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> lodestone_data::block_states::StateId {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state_id(lx, y, lz)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {
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
                column.set_block_id(x, y, z, BlockStateId::from_state_str("minecraft:stone").unwrap());
            }
        }
    }
    column
}

/// An all-water column that rejects every spawn position.
#[cfg(test)]
fn ocean_column() -> ChunkColumn {
    let mut column = ChunkColumn::new(0, 128);
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..32 {
                column.set_block_id(x, y, z, BlockStateId::from_state_str("minecraft:water").unwrap());
            }
        }
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spiral_traverses_the_square_in_search_order() {
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

    /// The full-body clearance gate has one positive and three negative
    /// controls. A cave above solid ground is usable; a solid cell, fluid, or
    /// a partial collider in the body footprint is not.
    #[test]
    fn spawn_body_clearance_rejects_solid_fluid_and_partial_colliders() {
        let mut column = land_column(10);
        assert!(
            spawn_position_is_clear_in_column(&column, 0, 0, 11),
            "the open cave above the stone surface is a valid player body"
        );
        assert!(
            !spawn_position_is_clear_in_column(&column, 0, 0, 10),
            "a body whose feet cell is solid must be rejected"
        );

        column.set_block_id(0, 11, 0, BlockStateId::from_state_str("minecraft:water").unwrap());
        assert!(
            !spawn_position_is_clear_in_column(&column, 0, 0, 11),
            "fluid is unsafe even though water has no collision boxes"
        );

        column.set_block_id(0, 11, 0, BlockStateId::from_state_str("minecraft:oak_slab[type=bottom,waterlogged=false]").unwrap());
        let slab = spawn_collision_boxes(
            BlockStateId::from_state_str("minecraft:oak_slab[type=bottom,waterlogged=false]")
                .expect("the collision census must know the slab state"),
        );
        assert!(!slab.is_empty(), "the partial-collider premise must hold");
        assert!(
            !spawn_position_is_clear_in_column(&column, 0, 0, 11),
            "a slab in the player's body footprint must be rejected"
        );
        for x in 0..16 {
            for z in 0..16 {
                column.set_block_id(x, 11, z, BlockStateId::from_state_str("minecraft:oak_slab[type=bottom,waterlogged=false]").unwrap());
            }
        }
        assert_eq!(
            get_level_respawn_pos(&column, 0, 0),
            Some(11),
            "the surface finder still reports the first full support below the slab"
        );
        assert_eq!(
            spawn_pos_in_column(&column, 0, 0),
            None,
            "the body gate must reject this column instead of accepting the blocked surface"
        );
    }

    #[cfg(test)]
    fn spawn_aabb_is_clear_named(
        mut state_at: impl FnMut(i32, i32, i32) -> String,
        pos: Vec3,
    ) -> bool {
        let mut known = true;
        let clear = spawn_aabb_is_clear(
            |x, y, z| match BlockStateId::from_state_str(&state_at(x, y, z)) {
                Some(state) => state,
                None => {
                    known = false;
                    BlockStateId::from_state_str("minecraft:air").expect("air state")
                }
            },
            pos,
        );
        known && clear
    }

    /// Unknown states are not silently treated as air by the body check. This
    /// is the fail-closed control for custom/data-pack blocks that are outside
    /// the built-in state vocabulary.
    #[test]
    fn spawn_body_clearance_rejects_an_unknown_state() {
        assert!(
            !spawn_aabb_is_clear_named(
                |_, _, _| "minecraft:custom_unlisted_block".to_owned(),
                Vec3::new(0.5, 11.0, 0.5),
            ),
            "an unknown state must not be assumed empty"
        );
    }

    #[test]
    fn fallback_climbs_above_a_blocked_preferred_height() {
        let mut column = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                column.set_block_id(x, y, z, BlockStateId::from_state_str("minecraft:stone").unwrap());
                }
            }
        }
        let fallback = fallback_spawn_y(&column, 8, 8, 8);
        assert_eq!(fallback, 16, "the first clear row is the one above this solid column");
        assert!(spawn_position_is_clear_in_column(&column, 8, 8, fallback));
    }

    #[test]
    fn player_spawn_anchor_is_sent_at_the_block_bottom_center() {
        assert_eq!(
            player_position_for_spawn_anchor(Vec3::new(-3.0, 64.0, 7.0)),
            Vec3::new(-2.5, 64.0, 7.5),
            "an integer block anchor must become the external bottom-centre position"
        );
        assert_eq!(
            player_position_for_spawn_anchor(Vec3::new(2.75, 64.9, -1.25)),
            Vec3::new(2.5, 64.0, -1.5),
            "explicit spawn coordinates are normalised to their containing block"
        );
    }

    /// A spawn anchor must be converted before the player is placed. At the
    /// integer corner the player's body reaches into the diagonal neighbour;
    /// at the block centre the same terrain is clear. This is the causal
    /// distinction behind an apparently underground join at a chunk edge.
    #[test]
    fn centered_spawn_avoids_a_neighboring_column_wall() {
        let mut neighbour = land_column(10);
        neighbour.set_block_id(15, 11, 15, BlockStateId::from_state_str("minecraft:stone").unwrap());
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), land_column(10));
        columns.insert((-1, -1), neighbour);
        let source = MapSource { columns };

        assert!(
            is_spawn_position_clear(&source, Vec3::new(0.5, 11.0, 0.5)),
            "the selected origin cell is clear when the player is centred"
        );
        assert!(
            !is_spawn_position_clear(&source, Vec3::new(0.0, 11.0, 0.0)),
            "the old integer-corner placement overlaps the diagonal wall"
        );
    }

    /// A non-collidable surface decoration does not become the support block.
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
            column.set_block_id(0, SURFACE_Y, 0, BlockStateId::from_state_str("minecraft:grass_block[snowy=false]").unwrap());
            column.set_block_id(0, SURFACE_Y + 1, 0, BlockStateId::from_state_str(plant).unwrap());

            let correct = SURFACE_Y + 1;
            let suspected_wrong = SURFACE_Y + 2;
            assert_ne!(correct, suspected_wrong, "the two hypotheses must differ");

            let measured = get_level_respawn_pos(&column, 0, 0);
            assert_eq!(
                measured,
                Some(correct),
                "{plant} has no collision, so the player stands on the \
                 grass_block at {SURFACE_Y} (feet {correct}); {suspected_wrong} is invalid"
            );
        }
    }

    /// A full support block above the surface becomes the new support.
    #[test]
    fn a_face_full_block_above_the_surface_does_raise_the_spawn() {
        const SURFACE_Y: i32 = 70;
        let mut column = land_column(SURFACE_Y);
        column.set_block_id(0, SURFACE_Y + 1, 0, BlockStateId::from_state_str("minecraft:oak_leaves[distance=3]").unwrap());
        assert_eq!(
            get_level_respawn_pos(&column, 0, 0),
            Some(SURFACE_Y + 2),
            "oak_leaves provides full support, so the player stands on the leaf"
        );
    }

    /// Every generated surface state resolves to its own census entry.
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
                let state = block_states::StateId::new(id)
                    .expect("generated state-table index is valid");
                assert_eq!(
                    spawn_face_full_up(state),
                    snow_support::face_full_up(state),
                    "face_full_up disagrees with the census for state {id}"
                );
                assert_eq!(
                    spawn_has_fluid_state(state),
                    snow_support::has_fluid_state(state),
                    "has_fluid_state disagrees with the census for state {id}"
                );
            }
            assert!(states > 0, "{name} must exist in the 26.2 census");
            if states > 1 {
                multi_state_blocks += 1;
            }
            assert!(
                (0..snow_support::STATE_COUNT).any(|id| {
                    block_states::block_name(id) == Some(name)
                        && BlockStateId::new(id).is_some_and(BlockStateId::is_default)
                }),
                "the census must provide a default state for {name}"
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
    /// A valid origin chunk returns its first valid local position.
    #[test]
    fn plains_origin_chunk_yields_spawn_at_first_scanned_position() {
        let mut columns = std::collections::HashMap::new();
        columns.insert((0, 0), land_column(20));
        let spawn = find_initial_spawn(&MapSource { columns });

        assert_eq!(
            spawn.pos.x, 0.0,
            "spawn X is the first x in scan order"
        );
        assert_eq!(
            spawn.pos.z, 0.0,
            "spawn Z is the first z in scan order"
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

            fn block_state_id(&self, x: i32, y: i32, z: i32) -> BlockStateId {
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                self.column(cx, cz)
                    .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
            }

            fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
                let cx = x.div_euclid(16);
                let cz = z.div_euclid(16);
                self.column(cx, cz)
                    .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
                    .to_string()
            }

            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: BlockStateId) {}
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
             candidate"
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
        assert_eq!(spawn.pos.z, 0.0, "chunk (1, 0)'s first z");
        assert_eq!(spawn.pos.y, 16.0, "one above the land surface");
    }

    /// Every chunk in the spiral invalid returns the fixed fallback position.
    #[test]
    fn a_fully_invalid_box_falls_back_above_sea_level_not_into_the_bedrock_floor() {
        const MIN_Y: i32 = -64;
        const HEIGHT: i32 = 384;
        let mut ocean = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                ocean.set_block_id(x, MIN_Y, z, BlockStateId::from_state_str("minecraft:bedrock").unwrap());
                for y in (MIN_Y + 1)..=(MIN_Y + 3) {
                    ocean.set_block_id(x, y, z, BlockStateId::from_state_str("minecraft:deepslate").unwrap());
                }
                for y in (MIN_Y + 4)..=62 {
                    ocean.set_block_id(x, y, z, BlockStateId::from_state_str("minecraft:water").unwrap());
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
            "fallback height is 64; {suspected_wrong} is inside the solid floor"
        );

        assert!(
            spawn_face_full_up(source.block_state_id(8, MIN_Y + 1, 8)),
            "premise: `min_y + 1` must sit inside a collidable block for this gate to \
             mean anything (it is deepslate in the fixture)"
        );
        assert!(
            !spawn_face_full_up(source.block_state_id(8, spawn.pos.y as i32, 8)),
            "the fallback must not place the player inside a collidable block"
        );
        assert!(
            is_spawn_position_clear(&source, player_position_for_spawn_anchor(spawn.pos)),
            "the complete fallback player body must be clear of this ocean column"
        );
    }

    /// Validates spawn positions against generated columns for representative seeds.
    #[test]
    #[ignore = "composes generated columns; several seconds per seed"]
    fn generated_spawn_is_standable_or_fallback() {
        let mut took_fallback = 0usize;
        let mut found_in_box = 0usize;
        for seed in [0_i64, 42, 1234, -195764831] {
            let source = crate::worldgen_data::overworld_chunk_source(seed);
            let spawn = find_initial_spawn(&source);
            let player_pos = player_position_for_spawn_anchor(spawn.pos);
            let (sx, sy, sz) = (
                player_pos.x.floor() as i32,
                player_pos.y.floor() as i32,
                player_pos.z.floor() as i32,
            );

            let feet = source.block_state_id(sx, sy, sz);
            let head = source.block_state_id(sx, sy + 1, sz);
            let support = source.block_state_id(sx, sy - 1, sz);
            println!(
                "seed {seed}: spawn=({sx}, {sy}, {sz}) support={support:?} feet={feet:?} head={head:?}"
            );

            assert!(
                !spawn_face_full_up(feet),
                "seed {seed}: spawn feet at ({sx}, {sy}, {sz}) are inside {:?}", feet
            );
            assert!(
                !spawn_face_full_up(head),
                "seed {seed}: spawn head at ({sx}, {}, {sz}) is inside {:?}",
                sy + 1,
                head
            );
            assert!(
                is_spawn_position_clear(&source, player_pos),
                "seed {seed}: the complete player body at {player_pos:?} overlaps terrain"
            );

            if spawn_face_full_up(support) {
                found_in_box += 1;
            } else {
                assert_eq!(
                    (sx, sy, sz),
                    (8, GENERATOR_SPAWN_HEIGHT, 8),
                    "seed {seed}: a spawn with nothing standable beneath it must be \
                     exactly the `(8, 64, 8)` fallback, not a \
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
        column.set_block_id(8, 20, 8, BlockStateId::from_state_str("minecraft:red_bed[part=foot,facing=north]").unwrap());
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
        column.set_block_id(8, 20, 8, BlockStateId::from_state_str("minecraft:red_bed").unwrap());
        // A solid block directly above the bed blocks the sleeping AABB.
        column.set_block_id(8, 21, 8, BlockStateId::from_state_str("minecraft:stone").unwrap());
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
        column.set_block_id(8, 20, 8, BlockStateId::from_state_str("minecraft:red_bed").unwrap());
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
        let state = |value: &str| {
            BlockStateId::from_state_str(value).expect("test state is in the generated table")
        };
        assert!(is_bed_block(state("minecraft:red_bed[part=head,facing=north]")));
        assert!(is_bed_block(state("minecraft:white_bed")));
        assert!(is_bed_block(state("minecraft:black_bed")));
        assert!(!is_bed_block(state("minecraft:stone")));
        assert!(!is_bed_block(state("minecraft:air")));
        assert!(!is_bed_block(state("minecraft:bedrock")));
        assert!(!is_bed_block(state("minecraft:respawn_anchor")));
    }
    /// A bed on open ground resolves to the first ordered candidate.
    #[test]
    fn a_bed_on_open_ground_resolves_to_the_first_offset() {
        let mut column = land_column(20);
        column.set_block_id(8, 21, 8, BlockStateId::from_state_str("minecraft:red_bed[facing=north,part=foot]").unwrap());
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

    /// A bed that has been broken since the point was set resolves to `None`.
    #[test]
    fn a_broken_bed_resolves_to_nothing() {
        let mut column = land_column(20);
        column.set_block_id(8, 21, 8, BlockStateId::from_state_str("minecraft:stone").unwrap());
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

    /// A bed walled in on every side has no usable candidate.
    ///
    #[test]
    fn a_walled_in_bed_resolves_to_nothing() {
        let mut column = land_column(20);
        for x in 0..16 {
            for z in 0..16 {
                for y in 21..=23 {
                    column.set_block_id(x, y, z, BlockStateId::from_state_str("minecraft:stone").unwrap());
                }
            }
        }
        column.set_block_id(8, 21, 8, BlockStateId::from_state_str("minecraft:red_bed[facing=north,part=foot]").unwrap());
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

    /// Bed facing determines which side receives the player.
    #[test]
    fn the_beds_facing_decides_which_side_the_player_wakes_on() {
        let resolve = |facing: &str| {
            let mut column = land_column(20);
            let state = match facing {
                "north" => "minecraft:red_bed[facing=north,part=foot]",
                "south" => "minecraft:red_bed[facing=south,part=foot]",
                "east" => "minecraft:red_bed[facing=east,part=foot]",
                _ => unreachable!(),
            };
            column.set_block_id(8, 21, 8, BlockStateId::from_state_str(state).unwrap());
            let mut columns = std::collections::HashMap::new();
            columns.insert((0, 0), column);
            resolve_bed_respawn(&MapSource { columns }, RespawnPoint {
                pos: BlockPos::new(8, 21, 8),
            })
        };
        assert_eq!(resolve("north"), Some(Vec3::new(9.5, 21.0, 8.5)));
        assert_eq!(resolve("south"), Some(Vec3::new(7.5, 21.0, 8.5)));
        assert_eq!(resolve("east"), Some(Vec3::new(8.5, 21.0, 9.5)));
    }
}
