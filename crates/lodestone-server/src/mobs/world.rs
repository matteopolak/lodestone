//! [`ChunkWorld`]: the server's terrain adapted into a
//! [`lodestone_entity::pathfinding::PathWorld`]/[`RayView`] — moved out of
//! `mobs/mod.rs` verbatim as part of the `mobs.rs` file split (see
//! `docs/plans/crate-and-file-splits.md`). No `MobSim` dependency.

use std::collections::HashMap;
use std::str::FromStr;

use lodestone_data::{block_states, collision_shapes, entity_dimensions, path_types};
use lodestone_entity::pathfinding::{Aabb, BlockCues, MobShape, PathType, PathWorld};
use lodestone_entity::{RayView, seen_percent};
use lodestone_model::{BlockPos, Vec3};

use crate::chunk::{AIR, ChunkColumn, ChunkSource};

use super::block_ids;

/// A [`PathWorld`] over the server's real per-block-state terrain.
///
/// Backed by a sparse map of [`ChunkColumn`]s keyed by chunk coordinate. Missing
/// columns and blocks outside the vertical range read as air. Each cell's
/// canonical block-state string (what [`ChunkColumn::block_state`] stores) is
/// resolved to its global block-state id and looked up in
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
    // `mobs/mod.rs`'s `tick_with_terrain`/`surface_y`/`tick_orbs`, so both need
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
    /// issue #454's whole subject is that 49 of those calls (~45 s at the 909 ms
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

    /// Sets a single block's canonical state (e.g. `"minecraft:water"`,
    /// `"minecraft:oak_fence"`, `"minecraft:oak_slab[type=bottom]"`) at world
    /// coordinates, creating the owning column on demand. The richer sibling of
    /// [`set_solid`](Self::set_solid): use this when a test or caller needs a
    /// specific census-distinguishable state (water vs. lava vs. a fence)
    /// rather than a bare solid/air bit.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, name: &str) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let (min_y, height) = (self.min_y, self.height);
        let col = self
            .columns
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(min_y, height));
        col.set_block(lx, y, lz, name);
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

    /// The canonical block-state string at world coordinates, or
    /// `"minecraft:air"` for a missing column or an out-of-range Y — matching
    /// [`ChunkColumn::block_state`]'s own out-of-range behaviour.
    ///
    /// **`pub`, alongside [`is_solid`](Self::is_solid), because it is what
    /// [`MobSim::tick_with_terrain`] now takes.** That oracle used to be a
    /// solid/air bit and is a state name now, so an external caller supplying a
    /// snapshot-backed one needs this. It is strictly more information than
    /// `is_solid` already exposed, not a new disclosure.
    #[must_use]
    pub fn block_state(&self, x: i32, y: i32, z: i32) -> &str {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.columns
            .get(&(cx, cz))
            .map_or(AIR, |col| col.block_state(lx, y, lz))
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

    /// The world Y of the highest non-air block in the column at `(x, z)`, or
    /// `None` for a missing column — vanilla's `WORLD_SURFACE` heightmap, which
    /// `getRandomPosWithin` picks its Y band against.
    #[must_use]
    pub(crate) fn surface_y(&self, x: i32, z: i32) -> Option<i32> {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let col = self.columns.get(&(cx, cz))?;
        let top = col.min_y + col.height - 1;
        Some(
            (col.min_y..=top)
                .rev()
                .find(|&y| col.block_state(lx, y, lz) != AIR)
                .unwrap_or(col.min_y),
        )
    }

    /// This snapshot's vertical floor — [`PathWorld::min_y`] without needing the
    /// trait in scope.
    #[must_use]
    pub(crate) fn floor_y(&self) -> i32 {
        self.min_y
    }

    /// Resolves the global block-state id at world coordinates through
    /// [`state_id_by_name`], if the state at that cell is one the 26.2 census
    /// knows about.
    #[must_use]
    fn state_id(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        block_ids::state_id_by_name()
            .get(self.block_state(x, y, z))
            .copied()
    }
}

impl PathWorld for ChunkWorld {
    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        // Real per-state classification: `WalkNodeEvaluator.getPathTypeFromState`
        // via `lodestone_data::path_types` (issue #204). Falls back to the old
        // solid/air guess only if the state string does not resolve to a known
        // 26.2 state id — not expected in practice, since every writer of this
        // world's terrain (worldgen, `set_solid`, `set_block`) emits canonical
        // vanilla state strings, but a tick must never panic on a lookup miss.
        self.state_id(x, y, z)
            .and_then(path_types::path_type)
            .map_or_else(
                || {
                    if self.is_solid(x, y, z) {
                        PathType::Blocked
                    } else {
                        PathType::Open
                    }
                },
                block_ids::census_to_pathfinding_type,
            )
    }

    /// Block *identity*, which [`base_path_type`](PathWorld::base_path_type)
    /// deliberately erases — `grass_block`, `dirt` and `stone` are one
    /// `Blocked` there (issue #456).
    ///
    /// # The tag is read from the jar's own census, never hand-written
    ///
    /// `#minecraft:edible_for_sheep` is resolved through
    /// [`lodestone_data::tool::block_tag_members`], which is generated from the
    /// jar. That is not fastidiousness: the obvious hand-written
    /// `short_grass | tall_grass` guess is **wrong in both directions**. The
    /// real tag (`data/minecraft/tags/block/edible_for_sheep.json`) is
    /// `short_grass`, `short_dry_grass`, `tall_dry_grass`, `fern` — so
    /// `tall_grass` is not a member at all, and three members would have been
    /// missing. A sheep would have refused to graze ferns and dry grass, and
    /// nothing would have failed. This repo has been wrong about a hand-written
    /// tag set twice before (`pig_food`/`chicken_food`).
    ///
    /// # Two id spaces, and mixing them is silent
    ///
    /// `block_tag_members` answers in **`minecraft:block` registry ids**, while
    /// [`state_id`](ChunkWorld::state_id) yields **block-*state*** ids — a
    /// 32,366-entry space against a ~1,100-entry one. Comparing one against the
    /// other compiles, type-checks, and matches whatever unrelated blocks happen
    /// to share those small integers. `tool::block_registry_id` is the bridge, so
    /// the lookup is state → block → tag.
    ///
    /// `grass_block` stays block equality, because vanilla's test is equality
    /// rather than a tag (`ai/goal/EatBlockGoal.java:34`, `:71`).
    fn block_cues(&self, x: i32, y: i32, z: i32) -> BlockCues {
        let state = self.block_state(x, y, z);
        // `block_state` yields a full state string; strip the property list so
        // `minecraft:short_grass[...]` compares as its block path.
        let path = state.split('[').next().unwrap_or(state);
        let edible_for_sheep = self
            .state_id(x, y, z)
            .and_then(lodestone_data::tool::block_registry_id)
            .is_some_and(|block| {
                lodestone_data::tool::block_tag_members("minecraft:edible_for_sheep")
                    .is_some_and(|members| members.binary_search(&block).is_ok())
            });
        BlockCues {
            edible_for_sheep,
            grass_block: path == "minecraft:grass_block",
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        // Vanilla asks exactly this at `WalkNodeEvaluator.getFloorLevel(level,
        // pos)` (.cache/mc/26.2/src/net/minecraft/world/level/pathfinder/
        // WalkNodeEvaluator.java:219-222): `shape.isEmpty() ? 0.0 :
        // shape.max(Direction.Axis.Y)` over the block's real collision shape —
        // not a naive "one block tall" assumption. So this is the max Y of the
        // state's collision boxes (`lodestone_data::collision_shapes`), which is
        // `1.0` for a full cube, `0.5` for a slab, `1.5` for a fence/wall (the
        // reason a 0.6 step height cannot mount one), and `0.0` for an empty
        // shape (air, water, lava, cobweb). Falls back to the old full-cell
        // solid/air guess on the same not-expected-in-practice lookup miss as
        // `base_path_type` above.
        self.state_id(x, y, z)
            .and_then(collision_shapes::collision_boxes)
            .map_or_else(
                || if self.is_solid(x, y, z) { 1.0 } else { 0.0 },
                |boxes| {
                    boxes
                        .iter()
                        .fold(0.0_f64, |acc, b| acc.max(f64::from(b.max[1])))
                },
            )
    }

    fn collides(&self, aabb: Aabb) -> bool {
        // Mirror the `noCollision` sweep: any solid full-block cell overlapping
        // the box collides. The `-1e-7` on the max edges keeps a box that merely
        // *touches* a block face (shares a boundary) from counting as a collision,
        // matching vanilla's strict-overlap semantics.
        //
        // Honest scope note: this still tests coarse [`is_solid`](ChunkWorld::is_solid)
        // full-cell occupancy, not the real per-state collision boxes
        // `base_path_type`/`collision_top` now read. Vanilla's own
        // `noCollision` does test real per-shape AABBs (a fence's `1.5`-tall box
        // included), so a jump-clearance/diagonal-reach check against, say, a
        // slab is coarser here than in vanilla. Issue #204 asked for real
        // path-type classification and collision *tops*, which this closes;
        // widening `collides` itself to real per-shape sweeps is a separate,
        // larger change (every caller of this method would need auditing for
        // the same "is a box in `Aabb` allowed to graze an over-tall shape"
        // question) and is not part of this issue's scope.
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if self.is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }
        false
    }

    // `is_water` is no longer overridden: the trait default
    // (`base_path_type(x, y, z) == PathType::Water`) is now correct, because
    // `base_path_type` reads the real census instead of a solid/air guess that
    // could never produce `PathType::Water` in the first place.
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
