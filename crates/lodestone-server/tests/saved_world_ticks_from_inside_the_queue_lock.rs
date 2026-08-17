//! A **saved** world must keep serving columns while the tick loop holds the
//! scheduled-tick queues.
//!
//! # The defect this exists for
//!
//! `tick::run_tick_loop` holds `ScheduledTickHandle::with` for its whole
//! scheduled-tick and random-tick section, and that section calls
//! `world.column`, `world.block_state` and `world.set_block`. On a persistent
//! world any of those can reach `region_source::RegionChunkSource::load`, which
//! restores the loaded chunk's saved ticks — and that restore used to take the
//! **same** `std::sync::Mutex`. A `std::sync::Mutex` is not reentrant, so this
//! was a self-deadlock on the tick thread: total, deterministic, and reached
//! the moment the world tick first touched a column that exists on disk.
//!
//! Observed symptom, at the owner's render distance of 32: the join wedged
//! before its first chunk batch, so the client showed "Loading terrain 1/4000",
//! the few outlines it had disappeared, and the player was left in a void — with
//! **no error, no disconnect and no panic**, because every thread involved was
//! simply parked on a lock.
//!
//! # Why no existing gate could see it
//!
//! Every singleplayer terrain gate in the tree opens either an *in-memory* world
//! (`world_dir: None`) or a *brand-new* directory. `RegionChunkSource::load`
//! returns `None` before it ever touches the scheduled-tick handle when the
//! chunk is not on disk, and `restore` returns early when the loaded chunk
//! carries no ticks — so the discriminating input is a **saved chunk that
//! carries a pending tick**, which no fixture in the corpus had. The
//! `world`-species blindness in `CLAUDE.md`: the flaw was in the input, and
//! unreadable from any test's source.
//!
//! # Shape of the gate
//!
//! The deadlock's natural failure is a *hang*, which is a bad gate — so the
//! re-entrant read runs on its own thread and the assertion is a bounded
//! `recv_timeout`. Before the fix that times out; after it, it answers.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use lodestone_server::dimension::Dimension;
use lodestone_server::region_source::RegionChunkSource;
use lodestone_server::{ChunkColumn, ChunkSource, TickPriority};

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// Generous enough that a loaded machine cannot fail this, and short enough
/// that a real deadlock does not stall the suite. The healthy path is a single
/// column read.
const DEADLOCK_DEADLINE: Duration = Duration::from_secs(30);

const SAVE_TICK: u64 = 1_000;
const LOAD_TICK: u64 = 7_000;

/// A saved chunk, and a column the save never covered — the generator has to
/// supply the second one. `(0, 0)` is written to disk below; `(9, 9)` is not,
/// and is far enough out that no region file in the fixture holds it either.
const SAVED_CHUNK: (i32, i32) = (0, 0);
const NEVER_SAVED_CHUNK: (i32, i32) = (9, 9);

/// Stands in for the bundled generator: cheap, and every column has terrain, so
/// "the generator ran" is a non-zero `solid_count` with no worldgen cost.
#[derive(Debug)]
struct Flat;

impl ChunkSource for Flat {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 60, z, "minecraft:stone");
            }
        }
        column
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
        // No storage; `RegionChunkSource` above is the retaining layer.
    }
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-saved-tick-reentry-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

fn open(dir: &Path) -> RegionChunkSource<Flat> {
    RegionChunkSource::new(Flat, dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world")
}

/// Writes a world whose chunk `SAVED_CHUNK` exists on disk **and carries a
/// pending block tick and a pending fluid tick**. Both halves are required: a
/// chunk with no ticks returns from `restore` before it touches the queues.
fn write_fixture(dir: &Path) {
    let world = open(dir);
    let scheduled = world.scheduled_ticks();
    scheduled.set_game_tick(SAVE_TICK);
    scheduled.with(|queues| {
        assert!(queues.block.schedule(
            (5, 70, 5),
            "minecraft:redstone_wire".to_owned(),
            SAVE_TICK + 5,
            TickPriority::Normal,
        ));
        assert!(queues.fluid.schedule(
            (7, 12, 9),
            "minecraft:flowing_lava".to_owned(),
            SAVE_TICK + 40,
            TickPriority::ExtremelyHigh,
        ));
    });
    // Whatever scheduled a tick in production wrote a block first; this is what
    // puts the column in the edit map so the save encodes it.
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
        "setup: the fixture must actually have written a region file, or this \
         gate degenerates into the fresh-world case every existing gate already covers"
    );
}

/// **The gate.** Reading the world from inside the queue lock must answer, and
/// must answer for both a saved column and one the generator has to supply.
#[test]
fn a_saved_worlds_columns_load_from_inside_the_tick_loops_own_queue_lock() {
    let dir = tempdir("gate");
    write_fixture(&dir);

    let world = open(&dir);
    let scheduled = world.scheduled_ticks();
    scheduled.set_game_tick(LOAD_TICK);

    let (tx, rx) = mpsc::channel();
    let probe = scheduled.clone();
    std::thread::spawn(move || {
        // Exactly `tick::run_tick_loop`'s shape: the queues are held for the
        // whole section, and the section reads and writes the world.
        let counts = probe.with(|_queues| {
            let saved = world.column(SAVED_CHUNK.0, SAVED_CHUNK.1).solid_count();
            let generated = world
                .column(NEVER_SAVED_CHUNK.0, NEVER_SAVED_CHUNK.1)
                .solid_count();
            (saved, generated)
        });
        let _ = tx.send(counts);
    });

    let (saved, generated) = rx.recv_timeout(DEADLOCK_DEADLINE).unwrap_or_else(|err| {
        panic!(
            "reading a saved world from inside `ScheduledTickHandle::with` did not answer \
             within {DEADLOCK_DEADLINE:?} ({err}). That is the self-deadlock: the chunk load \
             restores the chunk's saved ticks, and taking the queue lock again on the thread \
             that already holds it parks the tick loop forever. This is what left the owner \
             at \"Loading terrain 1/4000\" in a void with no error."
        );
    });

    assert!(
        saved > 0,
        "the saved column came back with no solid blocks at all"
    );
    assert!(
        generated > 0,
        "the never-saved column {NEVER_SAVED_CHUNK:?} came back empty — the generator did not \
         run for a column the save never covered, which is the other half of the empty-world \
         report"
    );

    // **The control on the deferral.** "It did not deadlock" is also satisfied
    // by a restore that simply throws the ticks away, so the ticks must be
    // *there* afterwards — and rebased onto this session's clock, not the one
    // they were saved under.
    let pending = scheduled.with(|queues| {
        let mut all: Vec<(u64, String)> = queues
            .block
            .iter()
            .chain(queues.fluid.iter())
            .map(|tick| (tick.trigger_tick, tick.kind.clone()))
            .collect();
        all.sort();
        all
    });
    assert_eq!(
        pending,
        vec![
            (LOAD_TICK + 5, "minecraft:redstone_wire".to_owned()),
            (LOAD_TICK + 40, "minecraft:flowing_lava".to_owned()),
        ],
        "both saved ticks must be live in the queues after the load, rebased onto \
         {LOAD_TICK}. An empty list here means the deadlock was 'fixed' by dropping \
         the world's pending ticks instead of deferring them"
    );
}
