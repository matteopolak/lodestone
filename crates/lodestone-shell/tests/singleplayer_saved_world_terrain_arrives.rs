//! A **saved** world must arrive with terrain — including for the columns it
//! never saved, which the generator has to supply.
//!
//! # The corpus blindness this closes
//!
//! `singleplayer_terrain_arrives.rs` proves a *freshly generated* world
//! delivers non-air blocks and meshes real geometry at the owner's render
//! distance. Every gate in it — and every other singleplayer gate in this crate
//! — opens either an in-memory world (`world_dir: None`) or a brand-new
//! directory. That is one shared property across the whole corpus, and it made
//! all of them structurally unable to see the defect the owner actually hit:
//! his worlds have region files, and `lodestone_server`'s chunk loader only
//! runs its saved-tick restore for a column that **exists on disk**. A world
//! with no region files returns from the load before it ever reaches that code.
//! The `world` species of vacuous test in `CLAUDE.md`: the flaw was in the
//! input, unreadable from any test's source.
//!
//! The specific defect was a self-deadlock — `tick::run_tick_loop` holds the
//! scheduled-tick queue lock across a section that reads the world, and the
//! restore took the same non-reentrant mutex — but the shape of this gate is
//! deliberately not about that mechanism. It asserts the *symptom*: open a
//! world that has been saved, and terrain must arrive. The mechanism is gated
//! precisely by `lodestone-server`'s
//! `tests/saved_world_ticks_from_inside_the_queue_lock.rs`, and **that** is the
//! gate to trust for the deadlock: which thread reaches a saved column first is
//! a race here (it was the tick thread in every measured run, but the join's own
//! blocking pool could win and warm the store instead). What this file asserts
//! deterministically is the other half, which no race touches — that terrain
//! generation is not disk-gated, i.e. a column absent from an *existing* region
//! file still reaches the client.
//!
//! # Why the fixture is built through `lodestone_server` rather than by playing
//!
//! Only *dirty* chunks are written, so a session that generates terrain and
//! quits without mutating anything saves nothing at all and would leave this
//! gate with the fresh-world corpus it exists to escape. Building the fixture
//! directly makes the precondition — "a region file exists, and the chunk in it
//! carries a pending tick" — assertable rather than hoped for.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_client::{BlockPos, ChunkPos};
use lodestone_server::region_source::RegionChunkSource;
use lodestone_server::{ChunkSource, TickPriority};

/// The owner's persisted `"render_distance": 32` plus the mesher's buffer ring,
/// the arithmetic `app::session::tick_render_distance` applies. Matching his is
/// the point: the report came from a real session at this radius.
const OWNER_VIEW_RADIUS: i32 = 33;

/// The world's seed, used for both the fixture and the session so the loaded
/// and generated halves belong to one world.
const SEED: i64 = 4242;

/// Written to disk by the fixture below.
const SAVED_CHUNK: (i32, i32) = (0, 0);

/// **Never** written to disk, and well inside the streamed square — the
/// generator is the only thing that can answer for it. This is the assertion
/// the fresh-world gates cannot make, because for them every column is this one.
const NEVER_SAVED_CHUNK: (i32, i32) = (3, 3);

/// Comfortably above the overworld ceiling, so a read here is air in every
/// world — used to learn the wire id of air without a registry.
const DEFINITELY_AIR_Y: i32 = 310;
const SEARCH_TOP: i32 = 300;
const SEARCH_BOTTOM: i32 = -64;

/// Generous: a debug-profile column is slow and this deadline must not be the
/// thing that fails on a loaded machine. The deadlock this guards against holds
/// forever, so no realistic value separates it from slowness incorrectly.
const DEADLINE: Duration = Duration::from_secs(240);

fn world_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("lodestone-saved-world-terrain-k4r2");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

/// Writes `SAVED_CHUNK` to disk with a pending block tick and a pending fluid
/// tick, and asserts it actually landed.
fn write_saved_world(dir: &Path) {
    let source = lodestone_server::overworld_chunk_source(SEED);
    let (min_y, height) = (source.min_y(), source.height());
    // The fixture is an overworld save: `source` is an `OverworldChunkSource`,
    // so its region store roots at the world directory itself rather than under
    // the `dimensions/minecraft/<id>/` subtree a Nether or End sibling uses.
    let world = RegionChunkSource::new(
        source,
        dir,
        lodestone_server::dimension::Dimension::Overworld,
        min_y,
        height,
    )
    .expect("open the fixture world");

    let scheduled = world.scheduled_ticks();
    scheduled.set_game_tick(100);
    scheduled.with(|queues| {
        assert!(queues.block.schedule(
            (5, 70, 5),
            "minecraft:redstone_wire".to_owned(),
            140,
            TickPriority::Normal,
        ));
        assert!(queues.fluid.schedule(
            (7, 62, 9),
            "minecraft:flowing_water".to_owned(),
            105,
            TickPriority::High,
        ));
    });
    // A pending tick's chunk is written, but the *edit* is what makes the save
    // encode a column rather than pass its old bytes through — and in production
    // whatever scheduled a tick had written a block first.
    world.set_block(5, 70, 5, "minecraft:redstone_wire");
    world.save_handle().save().expect("save the fixture world");

    let region = dir
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("region")
        .join("r.0.0.mca");
    assert!(
        region.is_file(),
        "precondition: the fixture must have written {region:?}. Without a region file this \
         gate is the fresh-world case the rest of the corpus already covers, and would pass \
         with the defect present"
    );
}

fn pump_until(net: &NetClient, what: &str, mut ready: impl FnMut(&NetClient) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    let mut errors: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        for update in net.poll() {
            match update {
                NetUpdate::Error(e) => errors.push(e),
                NetUpdate::Disconnected(reason) => errors.push(format!("disconnected: {reason:?}")),
                _ => {}
            }
        }
        if ready(net) {
            assert!(
                errors.is_empty(),
                "reached `{what}` but the session reported errors: {errors:?}"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out after {DEADLINE:?} waiting for `{what}` on a SAVED world. Errors reported: \
         {errors:?} — and note an empty list is the signature of the defect this gate exists \
         for: the server wedges on a lock, so there is no error, no disconnect and no panic, \
         just a client stuck on \"Loading terrain\" and then a void."
    );
}

/// Non-air blocks in the column through the centre of chunk `(cx, cz)`.
fn non_air_in_column(net: &NetClient, (cx, cz): (i32, i32), air: u32) -> usize {
    let (x, z) = (cx * 16 + 8, cz * 16 + 8);
    (SEARCH_BOTTOM..=SEARCH_TOP)
        .filter(|&y| net.block_at(BlockPos::new(x, y, z)).is_some_and(|id| id != air))
        .count()
}

/// **The gate.**
#[test]
fn a_saved_world_serves_terrain_for_a_column_it_never_saved() {
    let protocol = lodestone::Config::default().protocol;
    let Some(server_protocol) = lodestone_registry::server_protocol_for_protocol(protocol) else {
        assert!(!cfg!(feature = "live"), "the default build must host singleplayer");
        return;
    };

    let dir = world_dir();
    write_saved_world(&dir);

    let net = NetClient::open_singleplayer(
        server_protocol,
        protocol,
        SEED,
        OWNER_VIEW_RADIUS,
        None,
        Some(dir.clone()),
    );

    pump_until(&net, "the never-saved column", |net| {
        net.is_chunk_loaded(ChunkPos {
            x: NEVER_SAVED_CHUNK.0,
            z: NEVER_SAVED_CHUNK.1,
        }) && net.is_chunk_loaded(ChunkPos {
            x: SAVED_CHUNK.0,
            z: SAVED_CHUNK.1,
        })
    });

    let air = net
        .block_at(BlockPos::new(
            NEVER_SAVED_CHUNK.0 * 16 + 8,
            DEFINITELY_AIR_Y,
            NEVER_SAVED_CHUNK.1 * 16 + 8,
        ))
        .expect("a loaded chunk must answer for a y inside the world");

    let generated = non_air_in_column(&net, NEVER_SAVED_CHUNK, air);
    let saved = non_air_in_column(&net, SAVED_CHUNK, air);
    println!(
        "saved world at view_radius {OWNER_VIEW_RADIUS}: never-saved column \
         {NEVER_SAVED_CHUNK:?} has {generated} non-air, saved column {SAVED_CHUNK:?} has \
         {saved} non-air, air id {air}"
    );

    assert!(
        generated > 0,
        "the never-saved column {NEVER_SAVED_CHUNK:?} arrived entirely air. Terrain generation \
         is disk-gated — the loader only generates on a genuine miss — so this is the \
         empty-world report for every column the player has not previously visited"
    );
    assert!(
        saved > 0,
        "the saved column {SAVED_CHUNK:?} arrived entirely air, so what came back off disk is \
         not the world that was written"
    );
}
