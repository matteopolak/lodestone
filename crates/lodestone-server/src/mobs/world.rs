//! [`ChunkWorld`]: the server's terrain adapted into a
//! [`lodestone_entity::pathfinding::PathWorld`]/[`RayView`] — moved out of
//! `mobs/mod.rs` verbatim as part of the `mobs.rs` file split (see
//! `docs/plans/crate-and-file-splits.md`). No `MobSim` dependency.

use std::collections::HashMap;

use lodestone_data::{
    block::Block,
    block_states::StateId,
    collision_shapes, path_types,
};
use lodestone_entity::pathfinding::{Aabb, BlockCues, Footing, PathType, PathWorld};
use lodestone_entity::RayView;
use lodestone_model::Vec3;

use crate::chunk::{ChunkColumn, ChunkSource};

use super::block_ids;

#[cfg(test)]
std::thread_local! {
    static SURFACE_BLOCK_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A [`PathWorld`] over the server's real per-block-state terrain.
///
/// Backed by a sparse map of [`ChunkColumn`]s keyed by chunk coordinate. Missing
/// columns and blocks outside the vertical range read as air. Each cell's
/// global block-state id is looked up in
/// [`lodestone_data::path_types`] / [`lodestone_data::collision_shapes`] — the
/// same 32,366-state census `WalkNodeEvaluator.getPathTypeFromState` produces
/// in vanilla — so water, lava, fences, doors, rails and damaging blocks
/// classify distinctly instead of collapsing to solid/air. A state that fails
/// to resolve (should not happen for anything this crate's own worldgen or
/// [`set_block`](ChunkWorld::set_block) ever writes) falls back to the old
/// solid/air guess rather than panicking mid-tick.
#[derive(Debug, Clone)]
pub struct ChunkWorld {
    columns: HashMap<(i32, i32), ChunkColumn>,
    // `min_y`/`height` are read directly (`world.min_y`, `world.height`) from
    // `mobs/mod.rs`'s `tick_with_terrain`/`tick_orbs`, so both need
    // to cross the `mobs::world` boundary — the only two-field visibility
    // promotion this split needed.
    pub(super) min_y: i32,
    pub(super) height: i32,
}

impl ChunkWorld {
    /// An empty world with the given vertical extent (world Y in
    /// `min_y..min_y + height`).
    #[must_use]
    pub fn new(min_y: i32, height: i32) -> Self {
        assert!(height > 0, "height must be positive");
        Self {
            columns: HashMap::new(),
            min_y,
            height,
        }
    }

    /// Snapshots a square region of chunk columns from a [`ChunkSource`] into an
    /// owned world the pathfinder can query.
    ///
    /// `cx_range`/`cz_range` are inclusive chunk-coordinate bounds. This is the
    /// bridge from the *streaming* terrain source the server sends to clients to
    /// the *static* view the pathfinder needs for the duration of a search.
    #[must_use]
    pub fn from_source<S: ChunkSource>(
        source: &S,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
    ) -> Self {
        let mut columns = HashMap::new();
        let mut extent: Option<(i32, i32)> = None;
        for cz in cz_range {
            for cx in cx_range.clone() {
                let col = source.column(cx, cz);
                extent = Some((col.min_y, col.height));
                columns.insert((cx, cz), col);
            }
        }
        let (min_y, height) = extent.unwrap_or((0, 1));
        Self {
            columns,
            min_y,
            height,
        }
    }

    /// [`from_source`](Self::from_source) over columns that have **already been
    /// generated** — the same snapshot, assembled from a batch someone else
    /// fetched.
    ///
    /// This exists because `from_source`'s loop is *serial* and synchronous, and
    /// the whole subject is that 49 of those calls (~45 s at the 909 ms
    /// per composed column measured in `crate::chunk_store`) ran on the thread
    /// that opens a world. `crate::integrated` now fetches the same columns
    /// through [`crate::chunk::generate_columns_offloaded`] — parallel, on the
    /// blocking pool, and through the shared [`crate::chunk_store::ChunkStore`]
    /// so the connection path's copy of each column is the *same* generation —
    /// and hands the results here.
    ///
    /// The vertical extent is taken from the last column, exactly as
    /// `from_source` does; an empty iterator yields `(0, 1)`, again matching.
    #[must_use]
    pub fn from_columns(columns: impl IntoIterator<Item = ((i32, i32), ChunkColumn)>) -> Self {
        let mut map = HashMap::new();
        let mut extent: Option<(i32, i32)> = None;
        for (coord, col) in columns {
            extent = Some((col.min_y, col.height));
            map.insert(coord, col);
        }
        let (min_y, height) = extent.unwrap_or((0, 1));
        Self {
            columns: map,
            min_y,
            height,
        }
    }

    /// Sets a single block's solidity at world coordinates, creating the owning
    /// column on demand. The natural "place a block" primitive for building
    /// arenas and, later, applying server-side edits.
    pub fn set_solid(&mut self, x: i32, y: i32, z: i32, solid: bool) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let (min_y, height) = (self.min_y, self.height);
        let col = self
            .columns
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(min_y, height));
        col.set_solid(lx, y, lz, solid);
    }

    /// Test-only text fixture boundary. Production callers use [`set_block_id`].
    #[cfg(test)]
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let state = StateId::from_state_str(name)
            .unwrap_or_else(lodestone_data::block_states::air_state);
        self.set_block_id(x, y, z, state);
    }

    pub fn set_block_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let (min_y, height) = (self.min_y, self.height);
        let col = self
            .columns
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(min_y, height));
        col.set_block_id(lx, y, lz, state);
    }

    /// Whether the block at world coordinates is solid (neither air nor a
    /// fluid). This is [`ChunkColumn::is_solid`]'s coarse topology view, still
    /// used by [`collides`](PathWorld::collides) and as the fallback for a
    /// block state the census cannot resolve; [`base_path_type`](PathWorld::base_path_type)
    /// and [`collision_top`](PathWorld::collision_top) read the real per-state
    /// census instead.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.columns
            .get(&(cx, cz))
            .is_some_and(|col| col.is_solid(lx, y, lz))
    }

    #[must_use]
    pub fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.columns
            .get(&(cx, cz))
            .map_or_else(lodestone_data::block_states::air_state, |col| {
                col.block_state_id(lx, y, lz)
            })
    }

    /// The column at chunk coordinates, or `None` when this snapshot does not
    /// hold it. [`crate::natural_spawn`] needs the whole column, not one cell:
    /// the light engine runs over a volume.
    #[must_use]
    pub(crate) fn column(&self, cx: i32, cz: i32) -> Option<&ChunkColumn> {
        self.columns.get(&(cx, cz))
    }

    /// The biome name at world coordinates, or `None` for a missing column —
    /// the key [`lodestone_worldgen::spawners`]' per-biome lists are indexed by.
    #[must_use]
    pub(crate) fn biome_at(&self, x: i32, y: i32, z: i32) -> Option<String> {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.columns
            .get(&(cx, cz))
            .map(|col| col.biome_state_at(lx, y, lz).to_string())
    }

    /// Highest non-air world Y, or one below the floor for an empty column.
    /// Retained surface maps store relative first-free heights; missing columns return `None`.
    #[must_use]
    pub(crate) fn surface_y(&self, x: i32, z: i32) -> Option<i32> {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let col = self.columns.get(&(cx, cz))?;
        if let Some(map) = col.client_heightmaps().and_then(|maps| maps.get(1)) {
            return Some(col.min_y + map.get(lx as usize, lz as usize) as i32 - 1);
        }
        let top = col.min_y + col.height - 1;
        Some(
            (col.min_y..=top)
                .rev()
                .find(|&y| {
                    #[cfg(test)]
                    SURFACE_BLOCK_READS.with(|reads| reads.set(reads.get() + 1));
                    !matches!(col.block_state_id(lx, y, lz).block(), Block::Air | Block::CaveAir | Block::VoidAir)
                })
                .unwrap_or(col.min_y - 1),
        )
    }

    /// This snapshot's vertical floor — [`PathWorld::min_y`] without needing the
    /// trait in scope.
    #[must_use]
    pub(crate) fn floor_y(&self) -> i32 {
        self.min_y
    }
}

/// The captured friction and speed factor of a block state.
pub(super) fn footing_of(state: StateId) -> Footing {
    lodestone_data::movement::for_state(lodestone_data::version::GameDataVersion::V26_3, state)
        .map_or(Footing::DEFAULT, |movement| Footing {
            friction: movement.friction,
            speed_factor: movement.speed_factor,
        })
}

/// The pathfinder's classification of a block state.
pub(super) fn path_type_of(state: StateId) -> PathType {
    block_ids::census_to_pathfinding_type(path_types::path_type(state))
}

/// Top of a block state's collision shape within its cell: `1.0` for a cube,
/// `0.5` for a slab, `1.5` for a fence, `0.0` for an empty shape.
///
/// A moving piston has no census shape (it delegates to a block entity this
/// crate's discrete shove does not model) and is treated as a full cube so a
/// mob standing on a block mid-push does not fall through it.
/// Whether the state's collision shape is one full cube.
pub(super) fn is_full_cube(state: StateId) -> bool {
    matches!(collision_shapes::collision_boxes(state), [b] if b.min == [0.0; 3] && b.max == [1.0; 3])
}

pub(super) fn collision_top_of(state: StateId) -> f64 {
    if state.block().name() == "minecraft:moving_piston" {
        return 1.0;
    }
    collision_shapes::collision_boxes(state)
        .iter()
        .fold(0.0_f64, |top, b| top.max(f64::from(b.max[1])))
}

/// Block identity facts for goals. `edible_for_sheep` is membership in the
/// generated tag (`short_grass`, `short_dry_grass`, `tall_dry_grass`, `fern`),
/// resolved from the state's block id; the tag stores block ids, not state ids.
pub(super) fn cues_of(state: StateId) -> BlockCues {
    BlockCues {
        edible_for_sheep: lodestone_data::tool::block_tag_contains(
            "minecraft:edible_for_sheep",
            state.block(),
        ),
        grass_block: state.block().name() == "minecraft:grass_block",
    }
}

impl PathWorld for ChunkWorld {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        path_type_of(self.block_state_id(x, y, z))
    }

    fn block_cues(&self, x: i32, y: i32, z: i32) -> BlockCues {
        cues_of(self.block_state_id(x, y, z))
    }

    fn footing(&self, x: i32, y: i32, z: i32) -> Footing {
        footing_of(self.block_state_id(x, y, z))
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        collision_top_of(self.block_state_id(x, y, z))
    }

    fn is_roost(&self, x: i32, y: i32, z: i32) -> bool {
        is_full_cube(self.block_state_id(x, y, z))
    }

    fn collides(&self, aabb: Aabb) -> bool {
        // Full-cell occupancy, with the max edges pulled in so a box that
        // merely touches a cell face does not count.
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        (x0..=x1).any(|x| (y0..=y1).any(|y| (z0..=z1).any(|z| self.is_solid(x, y, z))))
    }
}

/// Terrain nobody has loaded: every cell a solid wall. A mob holds it between
/// ticks, so no stale snapshot can answer a path query.
pub(super) struct UnloadedWorld;

/// The world every mob is built with; real queries run against
/// [`LivePathWorld`] during the tick.
pub(super) static UNLOADED: UnloadedWorld = UnloadedWorld;

impl PathWorld for UnloadedWorld {
    fn min_y(&self) -> i32 {
        i32::MIN
    }

    fn base_path_type(&self, _: i32, _: i32, _: i32) -> PathType {
        PathType::Blocked
    }

    fn collision_top(&self, _: i32, _: i32, _: i32) -> f64 {
        1.0
    }

    fn collides(&self, _: Aabb) -> bool {
        true
    }
}

/// A [`PathWorld`] over the block states the server has loaded *right now*.
///
/// The single terrain authority for mob pathing and perception. A cell whose
/// column is absent reads as a solid full cube: a route never crosses terrain
/// that has not arrived, and an unloaded column is never mistaken for air.
pub(super) struct LivePathWorld<'a> {
    terrain: &'a super::collision::TerrainRead<'a>,
    min_y: i32,
}

impl<'a> LivePathWorld<'a> {
    pub(super) fn new(terrain: &'a super::collision::TerrainRead<'a>, min_y: i32) -> Self {
        Self { terrain, min_y }
    }
}

impl PathWorld for LivePathWorld<'_> {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        (self.terrain)(x, y, z).map_or(PathType::Blocked, path_type_of)
    }

    fn sees_sky(&self, x: i32, y: i32, z: i32) -> bool {
        const SCAN_LIMIT: i32 = 2048;
        if (self.terrain)(x, y, z).is_none() {
            return false;
        }
        (y..y + SCAN_LIMIT).all(|yy| {
            (self.terrain)(x, yy, z).is_none_or(|state| lodestone_data::light_props::light_props(state).0 == 0)
        })
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        (self.terrain)(x, y, z).map_or(1.0, collision_top_of)
    }

    fn is_roost(&self, x: i32, y: i32, z: i32) -> bool {
        (self.terrain)(x, y, z).is_some_and(is_full_cube)
    }

    fn collides(&self, aabb: Aabb) -> bool {
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        (x0..=x1).any(|x| {
            (y0..=y1).any(|y| {
                (z0..=z1).any(|z| match (self.terrain)(x, y, z) {
                    None => true,
                    Some(state) => cell_overlaps(state, (x, y, z), &aabb),
                })
            })
        })
    }

    fn block_cues(&self, x: i32, y: i32, z: i32) -> BlockCues {
        (self.terrain)(x, y, z).map_or(BlockCues::NONE, cues_of)
    }

    fn footing(&self, x: i32, y: i32, z: i32) -> Footing {
        (self.terrain)(x, y, z).map_or(Footing::DEFAULT, footing_of)
    }
}

/// Whether `aabb` overlaps any collision box of the block `state` placed at `cell`.
fn cell_overlaps(state: StateId, (x, y, z): (i32, i32, i32), aabb: &Aabb) -> bool {
    if state.block().name() == "minecraft:moving_piston" {
        return true;
    }
    let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
    collision_shapes::collision_boxes(state).iter().any(|b| {
        aabb.min_x < bx + f64::from(b.max[0])
            && aabb.max_x > bx + f64::from(b.min[0])
            && aabb.min_y < by + f64::from(b.max[1])
            && aabb.max_y > by + f64::from(b.min[1])
            && aabb.min_z < bz + f64::from(b.max[2])
            && aabb.max_z > bz + f64::from(b.min[2])
    })
}

impl RayView for ChunkWorld {
    /// A coarse but sound raymarch over [`is_solid`](ChunkWorld::is_solid):
    /// steps the segment at quarter-block spacing (fine enough that a
    /// full-block cell can never be skipped between samples) and reports
    /// blocked the moment any sample lands in a solid cell. This is not
    /// vanilla's exact voxel traversal (`ClipContext`), but it is a real
    /// terrain query — not the `OpenAir` stand-in [`explosion::seen_percent`]'s
    /// own tests use — which is what makes "a wall shields a mob from a blast"
    /// an observable, testable consequence rather than an assumption.
    fn is_clear(&self, from: Vec3, to: Vec3) -> bool {
        let delta = to - from;
        let dist = delta.length();
        if dist < 1e-9 {
            return true;
        }
        let steps = (dist / 0.25).ceil().max(1.0) as u32;
        for i in 0..=steps {
            let t = f64::from(i) / f64::from(steps);
            let p = from + delta.scale(t);
            if self.is_solid(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod surface_tests {
    use super::*;

    fn authored_column() -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 128);
        for (x, y, block) in [
            (0, -12, Block::GrassBlock), (0, -11, Block::OakLeaves), (0, -10, Block::Water),
            (1, -4, Block::OakLeaves), (2, -3, Block::Water),
            (3, -5, Block::GrassBlock), (3, -4, Block::ShortGrass),
        ] {
            column.set_block_id(x, y, 0, block.default_state());
        }
        for x in 0..5 {
            column.set_block_id(x, 20, 0, Block::CaveAir.default_state());
            column.set_block_id(x, 21, 0, Block::VoidAir.default_state());
        }
        column
    }

    #[test]
    fn retained_surface_map_uses_relative_first_free_without_reading_blocks() {
        let mut column = authored_column();
        let mut maps = [[0u16; 256]; 3];
        maps[0][..6].copy_from_slice(&[55, 61, 62, 61, 0, 0]);
        column.install_client_heightmaps_raw(maps);
        let world = ChunkWorld::from_columns([((-2, 3), column)]);
        SURFACE_BLOCK_READS.with(|reads| reads.set(0));
        for (x, expected) in [-10, -4, -3, -4, -65, -65].into_iter().enumerate() {
            assert_eq!(world.surface_y(-32 + x as i32, 48), Some(expected));
        }
        assert_eq!(SURFACE_BLOCK_READS.with(std::cell::Cell::get), 0);
        assert_eq!(world.surface_y(-16, 48), None);
        let fallback = ChunkWorld::from_columns([((-2, 3), authored_column())]);
        assert_eq!(fallback.surface_y(-32, 48), Some(-10));
        assert!(SURFACE_BLOCK_READS.with(std::cell::Cell::get) > 0, "the fallback control must exercise the read detector");
    }

    #[test]
    fn authored_surface_scan_includes_fluid_leaves_and_plants_but_excludes_every_air_variant() {
        let world = ChunkWorld::from_columns([((-2, 3), authored_column())]);
        for (x, expected) in [-10, -4, -3, -4, -65, -65].into_iter().enumerate() {
            assert_eq!(world.surface_y(-32 + x as i32, 48), Some(expected));
        }
    }
}
