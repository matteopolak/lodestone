//! World persistence end to end, through the real production entry point:
//! [`IntegratedServer::open_persistent_with_mobs`] → mutate →
//! shutdown → **reopen** → the mutation is still there.
//!
//! # What this gate can and cannot evidence
//!
//! A save/load round trip through our own codec establishes lifecycle behavior,
//! but it cannot independently establish that the serialized bytes match the
//! external format. The table below assigns each claim to the test that can
//! actually check it.
//!
//! | claim | evidenced by |
//! |---|---|
//! | our decode of vanilla's bytes is correct | **externally**, in `chunk_nbt_vanilla_oracle.rs`, against vanilla's own heightmaps |
//! | our region *container* is correct | **externally**, by `lodestone-anvil`'s own real-`.mca` tests |
//! | a mutation survives close/reopen through the production path | **here**, and this part is a round trip through our own code |
//! | a real vanilla server can load what we write | `scripts/anvil-oracle/` — see this file's sibling gate |
//!
//! The third row represents lifecycle behavior only. The two codecs it composes
//! are externally pinned elsewhere, and the controls below distinguish a
//! broken save half from a broken load half.
//!
//! # The controls
//!
//! The two controls cover the save and load halves independently. Their
//! expected failure sets are intentionally different, so a failure identifies
//! which half of the lifecycle is broken:
//!
//! | control | edit | expected result |
//! |---|---|---|
//! | save disabled | `WorldSaveHandle::save` returns `Ok(0)` before writing | **all three** fail; reopen reads `minecraft:air` where `minecraft:diamond_block` was placed, and the column count reads 0 of an expected 3 |
//! | load disabled | `RegionChunkSource::load` returns `None` unconditionally | **two of three** fail — the same two read-back assertions, while `a_save_writes_one_column_per_mutated_chunk_and_nothing_else` still **passes**, because saving genuinely still works |
//!
//! The save control leaves the read path untouched, while the load control
//! leaves the write path untouched. This separation prevents a single broken
//! stage from masking the behavior of the other stage.

use std::path::Path;
use std::time::Duration;

use lodestone_core::State;
use lodestone_server::dimension::Dimension;
use lodestone_server::region_source::{PersistenceStats, RegionChunkSource};
use lodestone_server::{ChunkColumn, ChunkSource, ServerBound, ServerDirective, ServerProtocol};
use uuid::Uuid;

/// A `ServerProtocol` that lowers nothing. This gate is about the *disk*, and
/// the connection exists only so the real constructor is the one under test —
/// no client ever reads these bytes.
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

/// A cheap, deterministic terrain source.
///
/// **Not** `overworld_chunk_source`, and that is a deliberate trade worth
/// naming: the real generator measures ~909 ms per composed column, so a gate
/// that reopened a world through it would take minutes. What this gate tests
/// is the *disk round trip*, and the correctness of the schema those bytes are
/// written in is pinned externally in `chunk_nbt_vanilla_oracle.rs` against a
/// real Mojang region file. Substituting terrain here therefore costs no
/// evidence — but it would if this gate were the only one, which is exactly the
/// "world" species of vacuous test, so: it is not the only one.
///
/// It still produces a real palette (three distinct states, plus a per-chunk
/// variation) rather than uniform stone, so palette remapping and multi-entry
/// bit packing are genuinely exercised.
#[derive(Debug)]
struct LayeredWorld {
    seed: i64,
}

impl ChunkSource for LayeredWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        let bump = ((cx.wrapping_mul(31) ^ cz.wrapping_mul(17) ^ self.seed as i32) % 5).abs();
        for z in 0..16 {
            for x in 0..16 {
                for y in MIN_Y..(60 + bump) {
                    column.set_block(x, y, z, "minecraft:stone");
                }
                column.set_block(x, 60 + bump, z, "minecraft:dirt");
                column.set_block(x, 61 + bump, z, "minecraft:grass_block[snowy=false]");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); the round
        // trip writes whole regions, not single probes.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); the round
        // trip writes whole regions, not single probes.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves freshly generated columns and edits are
    // discarded by design (an edit a test needs to survive goes through a
    // source with real retention). The no-op is explicit so the fixture's
    // retention behavior is clear at the implementation boundary.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

fn world_source(seed: i64) -> LayeredWorld {
    LayeredWorld { seed }
}

/// The overworld's vertical extent, matching what `chunk_nbt` and the 26.2
/// worlds in `.cache/mc` use.
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// A block nothing in worldgen would ever place at this position, so its
/// presence after a reopen cannot be a coincidence of the generator being
/// deterministic. That distinction is the entire point: with a fixed seed a
/// *regenerated* column is byte-identical to a *loaded* one, so a gate that
/// asserted on ordinary terrain would pass with no persistence at all.
const MARKER: &str = "minecraft:diamond_block";
/// A second marker with properties, so the palette-entry round trip
/// (`Name` + sorted `Properties`) is exercised and not just bare names.
const MARKER_WITH_PROPS: &str = "minecraft:oak_log[axis=x]";

const SEED: i64 = 437;
/// Deliberately inside the same region file, and deliberately spread across
/// more than one chunk, so the "one region rewrite, N columns" accounting is
/// actually exercised.
const SPOTS: [(i32, i32, i32, &str); 4] = [
    (5, 70, 5, MARKER),
    (6, 70, 5, MARKER_WITH_PROPS),
    (21, 71, 9, MARKER),
    (-3, 72, -7, MARKER),
];

/// Opens a persistent world, hands back the live world handle plus the server.
async fn open(dir: &Path) -> (lodestone_server::IntegratedServer, RegionChunkSource<impl ChunkSource>)
{
    let (server, _client, world) = lodestone_server::IntegratedServer::open_persistent_with_mobs(
        TestProtocol,
        dir,
        world_source(SEED),
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");
    (server, world)
}

/// **The gate.** Open, mutate, shut down, reopen, and read the mutation back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutation_survives_close_and_reopen() {
    let dir = tempdir("round_trip");

    // --- session one -------------------------------------------------------
    let (server, world) = open(&dir).await;
    // Exactly what `crate::server`'s `apply_use_item_on` calls in production,
    // on the identical object the server's own `ChunkStore` wraps.
    for &(x, y, z, block) in &SPOTS {
        world.set_block(x, y, z, block);
    }
    // Read back *before* saving, so a failure here is a live-world defect and
    // not a persistence one — the two would otherwise be indistinguishable.
    for &(x, y, z, block) in &SPOTS {
        assert_eq!(
            world.block_state(x, y, z),
            block,
            "the live world lost the mutation before anything was even saved"
        );
    }
    // `shutdown` flushes; see `IntegratedServer::shutdown`'s ordering note.
    server.shutdown().await;

    // --- session two -------------------------------------------------------
    let (server, reopened) = open(&dir).await;
    for &(x, y, z, block) in &SPOTS {
        assert_eq!(
            reopened.block_state(x, y, z),
            block,
            "({x},{y},{z}) did not survive close and reopen"
        );
    }

    // Terrain *around* the marker must survive too. A save path that wrote
    // only the changed cell and left the rest air would pass every assertion
    // above and produce an unplayable world.
    let solid = (MIN_Y..MIN_Y + HEIGHT)
        .filter(|&y| {
            let state = reopened.block_state(5, y, 5);
            state != "minecraft:air" && state != "minecraft:cave_air"
        })
        .count();
    assert!(
        solid > 32,
        "only {solid} non-air blocks in the reloaded column at (5,5); the surrounding terrain \
         was not saved, only the edited cell"
    );

    server.shutdown().await;
}

/// **The count**, not a duration. A save is proportional to what was *mutated*,
/// never to what is resident.
///
/// This is the property that keeps autosave off the critical path, and it is
/// the one a timing could not establish: `ChunkStore` holds up to 512 columns
/// at ~192 KiB each, so a save proportional to residency would write ~100 MiB
/// for a player standing still. Predicting the exact number (rather than
/// asserting "it is small") is what makes this a magnitude check and not a
/// direction-only one — 3 chunks touched must write **3** columns, not 512 and
/// not 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_save_writes_one_column_per_mutated_chunk_and_nothing_else() {
    let dir = tempdir("counts");
    let (server, world) = open(&dir).await;

    // Make the store resident in far more columns than we mutate, so
    // "proportional to residency" and "proportional to mutation" give
    // different answers. Reading a column is what makes it resident.
    for cz in -3..=3 {
        for cx in -3..=3 {
            let _ = world.column(cx, cz);
        }
    }
    let resident = 7 * 7;

    // Nothing mutated yet.
    assert_eq!(
        server.dirty_chunk_count(),
        Some(0),
        "reading columns must not mark anything dirty"
    );
    assert_eq!(
        server.save_now().expect("save"),
        0,
        "a save with no mutations must write nothing at all"
    );

    // The four spots above live in exactly three distinct chunks:
    // (5,5) and (6,5) share chunk (0,0); (21,9) is chunk (1,0); (-3,-7) is
    // chunk (-1,-1).
    for &(x, y, z, block) in &SPOTS {
        world.set_block(x, y, z, block);
    }
    let expected_chunks = 3;
    assert_eq!(
        server.dirty_chunk_count(),
        Some(expected_chunks),
        "four edits across three chunks must dirty three chunks"
    );

    let written = server.save_now().expect("save");
    assert_eq!(
        written, expected_chunks,
        "expected {expected_chunks} columns written (one per mutated chunk); got {written}. \
         The residency hypothesis predicts {resident}, and the single-region hypothesis \
         predicts 1 — neither is what a correct save does."
    );

    // And a second save immediately after writes nothing: the dirty set was
    // consumed, not merely read.
    assert_eq!(
        server.save_now().expect("save"),
        0,
        "the dirty set must be consumed by a successful save"
    );

    let stats = server.persistence_stats().expect("persistent server");
    assert_eq!(
        stats
            .columns_written
            .load(std::sync::atomic::Ordering::Relaxed),
        expected_chunks as u64,
        "cumulative columns_written must match the one real save"
    );

    server.shutdown().await;
}

/// A reopened world must *load* rather than *regenerate*, and the counter says
/// which happened.
///
/// Asserting on block values alone cannot tell these apart, because generation
/// is deterministic per seed — a completely broken load path that silently fell
/// through to the generator would produce identical terrain everywhere except
/// the edited cells.
///
/// # Why the counters are read off a quiescent handle
///
/// [`lodestone_server::region_source::PersistenceStats`] are per-**world**
/// accumulators, never per-call results. A delta around a read is therefore
/// meaningful only when no other task can read the same world concurrently.
/// The live server owns background tasks that access resident columns, so its
/// counters are not an exclusive measure of the read performed by this gate.
///
/// Session two instead constructs a second [`RegionChunkSource`] over the same
/// directory with **no server attached**. Its counters start at zero and every
/// increment is caused by the explicit read in this test, making the assertions
/// absolute values rather than sampled deltas. The production reopen remains
/// asserted separately for the lifecycle behavior that counters cannot prove.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopened_world_reads_from_disk_instead_of_regenerating() {
    let dir = tempdir("load_counter");

    // --- session one: written through the production path -------------------
    let (server, world) = open(&dir).await;
    world.set_block(5, 70, 5, MARKER);
    server.save_now().expect("save");
    server.shutdown().await;

    // --- session two: an exclusive observer over the same directory ---------
    let observer = RegionChunkSource::new(world_source(SEED), &dir, Dimension::Overworld, MIN_Y, HEIGHT)
        .expect("reopen the saved world");
    let observer_handle = observer.save_handle();
    let stats = observer_handle.stats();
    assert_eq!(
        (disk_loads(stats), generator_falls(stats)),
        (0, 0),
        "a freshly constructed source must not have touched anything yet"
    );

    let saved = observer.column(0, 0);
    assert_eq!(
        (disk_loads(stats), generator_falls(stats)),
        (1, 0),
        "reading a saved column must come off disk exactly once, not from the generator"
    );
    // The counter alone cannot tell a load of the *right* column from a load of
    // some other one, so the marker is checked on the very column that was
    // counted rather than on a second read.
    assert_eq!(
        saved.block_state(5, 70, 5),
        MARKER,
        "the column the disk-load counter counted is not the one that was saved"
    );

    // A column that was never saved must still fall through to the generator,
    // which is the control for the counter above: if `loaded_from_disk`
    // incremented here too, it would be counting calls rather than loads.
    let _ = observer.column(40, 40);
    assert_eq!(
        (disk_loads(stats), generator_falls(stats)),
        (1, 1),
        "an unsaved column must be generated, not claimed as a disk load"
    );

    // --- and the production reopen still has to work ------------------------
    // The counters above live on a handle no server owns, so this is the half
    // that keeps `open_persistent_with_mobs` itself in the gate. It asserts a
    // block value, which is the one thing immune to who else is reading.
    let (server, reopened) = open(&dir).await;
    assert_eq!(
        reopened.block_state(5, 70, 5),
        MARKER,
        "the production reopen path did not see the saved mutation"
    );
    server.shutdown().await;
}

fn disk_loads(stats: &PersistenceStats) -> u64 {
    stats
        .loaded_from_disk
        .load(std::sync::atomic::Ordering::Relaxed)
}

fn generator_falls(stats: &PersistenceStats) -> u64 {
    stats.generated.load(std::sync::atomic::Ordering::Relaxed)
}

/// A unique scratch directory. `std::env::temp_dir` plus the test name and a
/// literal nonce — not a pid or a random, because the scratchpad is shared
/// between agents and a collision reads as a persistence bug.
fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-437-4m8k-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}
