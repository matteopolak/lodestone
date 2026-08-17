//! A restart must not turn a lit portal into a duplicate — the gap
//! `crate::portal::PortalIndex`'s own doc used to name as "the one real
//! gap", now closed by wiring `crate::poi_storage::PoiStorage` into
//! `IntegratedServer::open_persistent_with_mobs`, its autosave task and its
//! shutdown path (issue [#303](https://github.com/matteopolak/lodestone/issues/303)'s
//! second half).
//!
//! # The chain this exercises
//!
//! portal lit → `PortalIndex` entry → POI record → disk → **restart** →
//! reload → `PortalIndex` entry → a return trip's `find_exit_portal` reuses
//! it. Every link is production code except the very first: a portal is "lit"
//! here by calling the same two things `crate::server`'s flint-and-steel
//! branch calls — `portal::ignite` for the cells, `PortalIndex::extend` to
//! publish them — rather than by driving a live connection through 81 ticks
//! of standing in fire, which is a protocol concern this file has no stake in.
//!
//! # Both dimensions, on purpose
//!
//! `PoiStorage` is per-dimension (`crate::poi_storage`'s own doc explains
//! why), so an implementation that restores only the overworld half compiles,
//! passes a single-dimension gate, and silently loses every Nether portal on
//! restart. The overworld half below drives the *real* persistent terrain
//! `IntegratedServer` returns, through the identical object
//! `crate::server::apply_use_item_on` writes through (see
//! `world_persistence_round_trip.rs`'s own comment on that object). The
//! Nether half uses a hand-rolled block map rather than a real
//! `NetherGenerator` — `nether_portal_round_trip.rs` reserves that generator
//! for an `#[ignore]`d gate because it is too slow to build here — but it
//! shares the **one real `PortalIndex`** `server.portals()` hands back, so
//! what is under test is exactly the property the module doc above names:
//! whether that index carries the Nether's cells across a restart at all, not
//! whether the Nether's terrain generates.
//!
//! # The control
//!
//! Without the restored index, the identical distant return trip finds
//! nothing, and [`portal::create_portal`] — what `resolve_destination` calls
//! next — builds a second portal whose cells are disjoint from the first.
//! That is the duplicate the bug report named, reproduced directly rather
//! than asserted from a doc comment.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use lodestone_core::State;
use lodestone_model::BlockPos;
use lodestone_server::dimension::Dimension;
use lodestone_server::portal::{self, Axis, PortalIndex};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

/// See `entity_persistence_round_trip.rs`'s identical stub — this gate is
/// about the disk and the portal index, not the wire.
#[derive(Debug)]
struct TestProtocol;

impl ServerProtocol for TestProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }
    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }
    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// Flat, cheap, deterministic overworld terrain — the same shape as
/// `entity_persistence_round_trip.rs`'s `FlatWorld`.
#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in MIN_Y..63 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
                column.set_block(x, 63, z, "minecraft:grass_block[snowy=false]");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(lx, y, lz)
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(lx, y, lz)
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage on the seed source itself — `RegionChunkSource` is what
        // retains an edit. See `world_persistence_round_trip.rs`'s identical
        // fixture for the same reasoning.
    }
}

/// A hand-rolled block map for the Nether half — see the module doc for why
/// this is not a real `NetherGenerator`. Identical shape to
/// `nether_portal_round_trip.rs`'s `TestWorld`.
struct BlockMapWorld {
    dimension: Dimension,
    floor_top: i32,
    filler: &'static str,
    blocks: Mutex<HashMap<(i32, i32, i32), String>>,
}

impl BlockMapWorld {
    fn new(dimension: Dimension, floor_top: i32, filler: &'static str) -> Self {
        Self {
            dimension,
            floor_top,
            filler,
            blocks: Mutex::new(HashMap::new()),
        }
    }
}

impl ChunkSource for BlockMapWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(self.dimension.min_y(), self.dimension.height())
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if let Some(state) = self.blocks.lock().unwrap().get(&(x, y, z)) {
            return state.clone();
        }
        if y <= self.floor_top && y >= self.dimension.min_y() {
            return self.filler.to_owned();
        }
        "minecraft:air".to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.blocks
            .lock()
            .unwrap()
            .insert((x, y, z), name.to_owned());
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(self.dimension)
    }
}

/// An obsidian frame — `TestWorld::frame` in `nether_portal_round_trip.rs`,
/// generalised to any [`ChunkSource`] so the same helper drives both the real
/// persistent overworld and the synthetic Nether stub.
fn build_frame(
    world: &impl ChunkSource,
    x: i32,
    y: i32,
    z: i32,
    axis: Axis,
    width: i32,
    height: i32,
) {
    let (ax, az) = match axis {
        Axis::X => (1, 0),
        Axis::Z => (0, 1),
    };
    for across in -1..=width {
        for up in -1..=height {
            if across == -1 || across == width || up == -1 || up == height {
                world.set_block(x + ax * across, y + up, z + az * across, "minecraft:obsidian");
            }
        }
    }
}

/// Lights a portal exactly as `crate::server`'s flint-and-steel branch does:
/// `portal::ignite` for the cells, `set_block` for each, `PortalIndex::extend`
/// to publish them — see that call site's own comment on why the index update
/// is "not bookkeeping". Returns the lit cells.
fn light_portal(
    world: &impl ChunkSource,
    portals: &PortalIndex,
    dimension: Dimension,
    at: BlockPos,
) -> Vec<BlockPos> {
    let cells = portal::ignite(world, dimension, at).expect("a hand-built frame lights");
    for (pos, state) in &cells {
        world.set_block(pos.x, pos.y, pos.z, state);
    }
    let positions: Vec<BlockPos> = cells.iter().map(|(pos, _)| *pos).collect();
    portals.extend(dimension, positions.iter().copied());
    positions
}

fn sorted(mut cells: Vec<BlockPos>) -> Vec<BlockPos> {
    cells.sort_by_key(|p| (p.x, p.y, p.z));
    cells
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-303-portal-restart-k9v-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

async fn open(
    dir: &Path,
) -> (
    IntegratedServer,
    lodestone_server::region_source::RegionChunkSource<impl ChunkSource>,
) {
    let (server, _client, world) = IntegratedServer::open_persistent_with_mobs(
        TestProtocol,
        dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        1,
        // An hour: this gate's saves are the explicit ones at shutdown, so a
        // timer firing mid-assertion cannot be mistaken for the thing under
        // test.
        Duration::from_secs(3600),
    )
    .expect("open persistent world");
    (server, world)
}

/// Pairwise-distinct, far from the origin in both dimensions.
const OVERWORLD_PORTAL: (i32, i32, i32) = (340, 70, -115);
const NETHER_PORTAL: (i32, i32, i32) = (-208, 90, 311);

/// **The gate.** Light a portal in each dimension, shut down, reopen, and
/// confirm a distant return trip reuses each one rather than building a
/// second beside it — with a control proving the same scenario *does* build a
/// duplicate without the restore.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_portal_survives_restart_in_both_dimensions_and_is_reused_not_duplicated() {
    let dir = tempdir("main");

    // --- session one ---------------------------------------------------
    let (server, world) = open(&dir).await;
    let portals = server
        .portals()
        .expect("a persistent world has a portal index")
        .clone();

    build_frame(&world, OVERWORLD_PORTAL.0, OVERWORLD_PORTAL.1, OVERWORLD_PORTAL.2, Axis::X, 2, 3);
    let overworld_cells = light_portal(
        &world,
        &portals,
        Dimension::Overworld,
        BlockPos::new(OVERWORLD_PORTAL.0, OVERWORLD_PORTAL.1, OVERWORLD_PORTAL.2),
    );
    assert_eq!(overworld_cells.len(), 6, "a 2x3 interior is 6 cells");

    // The Nether half: a synthetic terrain, but the **one real** portal
    // index — see the module doc for why this still exercises the property
    // under test.
    let nether_seed_world = BlockMapWorld::new(Dimension::Nether, 31, "minecraft:netherrack");
    build_frame(&nether_seed_world, NETHER_PORTAL.0, NETHER_PORTAL.1, NETHER_PORTAL.2, Axis::X, 2, 3);
    let nether_cells = light_portal(
        &nether_seed_world,
        &portals,
        Dimension::Nether,
        BlockPos::new(NETHER_PORTAL.0, NETHER_PORTAL.1, NETHER_PORTAL.2),
    );
    assert_eq!(nether_cells.len(), 6);

    assert_eq!(portals.cells(Dimension::Overworld).len(), 6);
    assert_eq!(portals.cells(Dimension::Nether).len(), 6);

    // Flushes the portal index to `poi/`, for both dimensions — see
    // `IntegratedServer::shutdown`'s own ordering comment for why this runs
    // after the tick and connection tasks have stopped.
    server.shutdown().await;

    // The files must actually exist — without this, a reopen that restored
    // nothing *and* saved nothing would fail below with a confusing message
    // about the load path when the real defect was the save.
    for dim_folder in ["overworld", "the_nether"] {
        let poi_dir = dir
            .join("dimensions")
            .join("minecraft")
            .join(dim_folder)
            .join("poi");
        let files: Vec<_> = std::fs::read_dir(&poi_dir)
            .unwrap_or_else(|err| panic!("{}: {err}", poi_dir.display()))
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("mca"))
            .collect();
        assert!(
            !files.is_empty(),
            "nothing was written to {}; the poi save never ran",
            poi_dir.display()
        );
    }

    // --- session two: restart --------------------------------------------
    let (server2, world2) = open(&dir).await;
    let restored = server2
        .portals()
        .expect("a persistent world has a portal index");

    // **The link this file exists to prove**: portal lit -> index entry ->
    // POI record -> disk -> reload -> index entry, for *both* dimensions.
    assert_eq!(
        sorted(restored.cells(Dimension::Overworld)),
        sorted(overworld_cells.clone()),
        "the overworld portal's cells did not survive the restart"
    );
    assert_eq!(
        sorted(restored.cells(Dimension::Nether)),
        sorted(nether_cells.clone()),
        "the Nether portal's cells did not survive the restart — an \
         implementation that only loops Dimension::Overworld would fail \
         exactly here"
    );

    // The real terrain persisted too (region/ saves, not this change's own
    // wiring). Checked with **targeted** reads at the exact cells — not a
    // search — because `RegionChunkSource` has no `ChunkStore` cache in front
    // of it here (this crate's own docs measure ~909 ms per column through
    // that path), and the reuse/control checks below run a search touching
    // hundreds of columns; doing that against the bare persistent source
    // turned this test into a multi-minute disk-bound scan on first sight.
    for cell in &overworld_cells {
        let state = world2.block_state(cell.x, cell.y, cell.z);
        assert!(portal::is_portal(&state), "overworld cell {cell:?} lost its block: {state}");
    }
    // The searches below run against cheap in-memory stand-ins instead,
    // seeded from the disk-confirmed positions — the same substitution
    // `nether_portal_round_trip.rs` makes throughout for portal *search*
    // logic. What is under test past this point is whether the restored
    // index changes `find_exit_portal`'s answer, not whether terrain I/O is
    // fast; `world_persistence_round_trip.rs` already covers the terrain
    // round trip itself.
    let overworld_terrain2 = BlockMapWorld::new(Dimension::Overworld, 62, "minecraft:stone");
    for cell in &overworld_cells {
        overworld_terrain2.set_block(cell.x, cell.y, cell.z, &portal::portal_state(Axis::X));
    }
    let nether_terrain2 = BlockMapWorld::new(Dimension::Nether, 31, "minecraft:netherrack");
    for cell in restored.cells(Dimension::Nether) {
        nether_terrain2.set_block(cell.x, cell.y, cell.z, &portal::portal_state(Axis::X));
    }

    // --- reuse, not duplication: overworld --------------------------------
    // 40 blocks away in x, 3 in z: beyond `FALLBACK_SCAN_RADIUS` (8) but well
    // inside `OVERWORLD_SEARCH_RADIUS` (128) — precisely the band the module
    // doc's bug report names: too far for the fallback scan, reachable only
    // through the index.
    let overworld_origin = BlockPos::new(OVERWORLD_PORTAL.0 + 40, OVERWORLD_PORTAL.1, OVERWORLD_PORTAL.2 + 3);
    let reused = portal::find_exit_portal(
        &overworld_terrain2,
        Dimension::Overworld,
        Some(restored),
        overworld_origin,
    );
    assert!(
        reused.is_some_and(|pos| overworld_cells.contains(&pos)),
        "the restored index must let the return trip find the portal it lit, \
         not build a second one: got {reused:?}"
    );

    // --- the control: without the restored index, the same trip misses and
    // production would build a duplicate --------------------------------
    let missed = portal::find_exit_portal(&overworld_terrain2, Dimension::Overworld, None, overworld_origin);
    assert_eq!(
        missed, None,
        "control premise failed: the fallback scan alone must not reach a \
         portal 40 blocks away, or this control proves nothing"
    );
    let duplicate =
        portal::create_portal(&overworld_terrain2, Dimension::Overworld, overworld_origin, Axis::X)
            .expect("control: with no index, production proceeds to build a fresh portal");
    assert!(
        duplicate
            .portal_cells
            .iter()
            .all(|cell| !overworld_cells.contains(cell)),
        "the control's 'duplicate' portal must not merely be the original found \
         by accident — its cells must be genuinely new, or this is not a \
         duplicate at all"
    );

    // --- reuse, not duplication: the Nether ------------------------------
    // 12 blocks away in x, 1 in z: beyond `FALLBACK_SCAN_RADIUS` (8) but
    // inside `NETHER_SEARCH_RADIUS` (16) — the Nether's index search radius
    // is much smaller than the overworld's (a Nether portal serves a
    // 128-block overworld area under the 8:1 scale, so its own radius does
    // not need to be as wide), so the offset that separated the two arms for
    // the overworld above would sit outside the Nether's index radius too and
    // prove nothing.
    let nether_origin = BlockPos::new(NETHER_PORTAL.0 + 12, NETHER_PORTAL.1, NETHER_PORTAL.2 + 1);
    let nether_reused = portal::find_exit_portal(
        &nether_terrain2,
        Dimension::Nether,
        Some(restored),
        nether_origin,
    );
    assert!(
        nether_reused.is_some_and(|pos| nether_cells.contains(&pos)),
        "the Nether portal must be reused after restart too: got {nether_reused:?}"
    );
    let nether_missed =
        portal::find_exit_portal(&nether_terrain2, Dimension::Nether, None, nether_origin);
    assert_eq!(
        nether_missed, None,
        "control premise failed for the Nether half"
    );

    server2.shutdown().await;
}
