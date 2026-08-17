//! Pending block and fluid ticks survive a world being closed and reopened
//! (issue [#468](https://github.com/matteopolak/lodestone/issues/468)).
//!
//! # What this gates that a round trip cannot
//!
//! Two fields of vanilla's `SavedTick` are shaped so that a writer and a reader
//! sharing one misunderstanding agree perfectly:
//!
//! * **`p` is the priority *value* in `-3..3`, not the ordinal.** Our
//!   `TickPriority` is declaration-ordered so `Ord` reproduces Java's
//!   `compareTo`, which makes `Normal`'s ordinal `3` and its value `0`. So this
//!   file asserts the **raw `Int` this crate writes to disk**, for two
//!   priorities at once: `Normal` must be `0` and `ExtremelyHigh` must be `-3`.
//!   An ordinal-writing implementation produces `3` and `0` — it would pass a
//!   check of either priority alone, and fails both here.
//! * **`t` is a signed delay** relative to game time at save. A tick already
//!   overdue writes a negative one, which is 1,584 of the 133,051 entries in
//!   the real vanilla worlds `chunk_extras_vanilla_oracle.rs` reads.
//!
//! Both expected values come from `TickPriority.java:6-12` and
//! `SavedTick.java:52` respectively, not from this crate.
//!
//! The complementary direction — that *Mojang's* bytes decode correctly — is
//! `tests/chunk_extras_vanilla_oracle.rs`. Neither file is sufficient alone.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use lodestone_anvil::region::{RegionFile, region_and_local};
use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_server::dimension::Dimension;
use lodestone_server::region_source::RegionChunkSource;
use lodestone_server::{ChunkColumn, ChunkSource, TickPriority};

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// The game tick session one saves at, and session two loads at. Deliberately
/// different, and deliberately far apart: if the delay were stored as an
/// absolute trigger tick by mistake, session two would rebase onto the wrong
/// base and this difference is what exposes it.
const SAVE_TICK: u64 = 1_000;
const LOAD_TICK: u64 = 7_000;

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
        // The column-regenerating form (correct, just not cheap); this fixture
        // is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this fixture
        // is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-tick-persist-q7v3-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

fn open(dir: &Path) -> RegionChunkSource<Flat> {
    RegionChunkSource::new(Flat, dir, Dimension::Overworld, MIN_Y, HEIGHT).expect("open world")
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// The chunk NBT this crate actually wrote, read straight back out of the
/// region file — not through `chunk_nbt`'s reader, which is the code that would
/// share a mistake with the writer.
fn read_chunk_nbt(dir: &Path, cx: i32, cz: i32) -> Nbt {
    let (rx, rz, lx, lz) = region_and_local(cx, cz);
    let region_dir = dir
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("region");
    let bytes =
        std::fs::read(region_dir.join(format!("r.{rx}.{rz}.mca"))).expect("region file exists");
    let region = RegionFile::parse(&bytes).expect("region parses");
    let raw = region
        .read_chunk_nbt_bytes_resolving_external(lx, lz, cx, cz, &region_dir)
        .expect("chunk reads")
        .expect("chunk is present");
    let mut reader = Reader::new(&raw);
    read_named_nbt(&mut reader).expect("chunk NBT parses").1
}

/// Every `(i, t, p)` triple in one of the two tick lists, straight off disk.
fn raw_ticks(nbt: &Nbt, list: &str) -> Vec<(String, i32, i32)> {
    let Some(Nbt::List { elements, .. }) = field(nbt, list) else {
        return Vec::new();
    };
    elements
        .iter()
        .map(|entry| {
            let kind = match field(entry, "i") {
                Some(Nbt::String(s)) => s.clone(),
                other => panic!("tick `i` must be a String, found {other:?}"),
            };
            let delay = match field(entry, "t") {
                Some(Nbt::Int(v)) => *v,
                other => panic!("tick `t` must be an Int, found {other:?}"),
            };
            let priority = match field(entry, "p") {
                Some(Nbt::Int(v)) => *v,
                other => panic!(
                    "tick `p` must be an Int — vanilla's TickPriority.CODEC is \
                     Codec.INT.xmap(byValue, getValue), not a string. Found {other:?}"
                ),
            };
            (kind, delay, priority)
        })
        .collect()
}

/// **The gate.** A pending block tick and a pending fluid tick are written with
/// the right `t` and `p`, and come back rebased onto the new session's clock.
#[test]
fn pending_ticks_survive_a_close_and_reopen_with_the_right_delay_and_priority() {
    let dir = tempdir("round-trip");

    // Both in chunk (0,0), so one chunk's NBT carries both lists populated.
    let block_pos = (5, 70, 5);
    let fluid_pos = (7, 12, 9);
    // One in the future, one already overdue at save time.
    let block_trigger: u64 = SAVE_TICK + 5;
    let fluid_trigger: u64 = SAVE_TICK - 100;

    {
        let world = open(&dir);
        let scheduled = world.scheduled_ticks();
        // Stands in for `tick::run_tick_loop`'s own per-tick store.
        scheduled.set_game_tick(SAVE_TICK);

        scheduled.with(|queues| {
            assert!(queues.block.schedule(
                block_pos,
                "minecraft:redstone_wire".to_owned(),
                block_trigger,
                TickPriority::Normal,
            ));
            assert!(queues.fluid.schedule(
                fluid_pos,
                "minecraft:flowing_lava".to_owned(),
                fluid_trigger,
                TickPriority::ExtremelyHigh,
            ));
        });

        // The chunk must be in the edit map for the save to encode it fresh,
        // which in production is always true: whatever scheduled the tick wrote
        // a block first.
        world.set_block(block_pos.0, block_pos.1, block_pos.2, "minecraft:redstone_wire");

        let handle = world.save_handle();
        handle.save().expect("save");
        assert_eq!(
            handle
                .stats()
                .scheduled_ticks_written
                .load(Ordering::Relaxed),
            2,
            "both pending ticks must be encoded — an empty `block_ticks` list \
             reads 0 here, which is what #468 measured"
        );
    }

    // -- what is actually on disk -----------------------------------------
    let nbt = read_chunk_nbt(&dir, 0, 0);

    // Expected delays: `delay = trigger - game_time_at_save`, the inverse of
    // `SavedTick::unpack`'s `trigger = current + delay`. Computed here from the
    // constants above, so the numbers are not copied from the implementation.
    let expected_block_delay = i32::try_from(block_trigger as i64 - SAVE_TICK as i64).unwrap();
    let expected_fluid_delay = i32::try_from(fluid_trigger as i64 - SAVE_TICK as i64).unwrap();
    assert_eq!(
        (expected_block_delay, expected_fluid_delay),
        (5, -100),
        "setup: one delay must be positive and the other negative, or this gate \
         cannot see the sign bug at all"
    );

    assert_eq!(
        raw_ticks(&nbt, "block_ticks"),
        vec![(
            "minecraft:redstone_wire".to_owned(),
            expected_block_delay,
            0, // TickPriority.NORMAL's VALUE. Its ordinal is 3.
        )],
        "the block tick's on-disk `t` and `p`"
    );
    assert_eq!(
        raw_ticks(&nbt, "fluid_ticks"),
        vec![(
            "minecraft:flowing_lava".to_owned(),
            expected_fluid_delay,
            -3, // TickPriority.EXTREMELY_HIGH's VALUE. Its ordinal is 0.
        )],
        "the fluid tick's on-disk `t` and `p` — a negative delay, written \
         verbatim rather than clamped or wrapped"
    );

    // **The magnitude check on `p`.** Asserting one priority proves little:
    // Normal's ordinal is 3 and ExtremelyHigh's ordinal is 0, so an
    // ordinal-writing implementation would have written 3 and 0 where the
    // value-writing one writes 0 and -3. Requiring both at once separates the
    // two hypotheses completely — and only the value hypothesis can produce a
    // negative number at all.
    let observed_priorities = (
        raw_ticks(&nbt, "block_ticks")[0].2,
        raw_ticks(&nbt, "fluid_ticks")[0].2,
    );
    assert_eq!(observed_priorities, (0, -3), "the value hypothesis");
    assert_ne!(
        observed_priorities,
        (3, 0),
        "these are the ordinals — writing them would silently demote every \
         normal tick in the world to EXTREMELY_LOW when a real server read it"
    );

    // -- session two: a new world, a different clock ------------------------
    let world = open(&dir);
    let scheduled = world.scheduled_ticks();
    scheduled.set_game_tick(LOAD_TICK);
    assert_eq!(
        scheduled.with(|q| (q.block.len(), q.fluid.len())),
        (0, 0),
        "setup: session two starts empty, so anything below came off disk"
    );

    let _ = world.column(0, 0);

    // Rebased onto LOAD_TICK, not restored to their old absolute triggers.
    let expected_block = LOAD_TICK as i64 + i64::from(expected_block_delay);
    let expected_fluid = LOAD_TICK as i64 + i64::from(expected_fluid_delay);
    let (block, fluid) = scheduled.with(|queues| {
        let b: Vec<(u64, TickPriority, String)> = queues
            .block
            .iter()
            .map(|t| (t.trigger_tick, t.priority, t.kind.clone()))
            .collect();
        let f: Vec<(u64, TickPriority, String)> = queues
            .fluid
            .iter()
            .map(|t| (t.trigger_tick, t.priority, t.kind.clone()))
            .collect();
        (b, f)
    });

    assert_eq!(
        block,
        vec![(
            expected_block as u64,
            TickPriority::Normal,
            "minecraft:redstone_wire".to_owned()
        )],
        "the block tick must be due {expected_block} — {LOAD_TICK} + {expected_block_delay}. \
         Its original absolute trigger was {block_trigger}, which is what a save that \
         stored the trigger rather than the delay would produce"
    );
    assert_eq!(
        fluid,
        vec![(
            expected_fluid as u64,
            TickPriority::ExtremelyHigh,
            "minecraft:flowing_lava".to_owned()
        )],
        "the overdue fluid tick must come back overdue by the same margin, and \
         keep its priority"
    );

    assert_eq!(
        world
            .save_handle()
            .stats()
            .scheduled_ticks_loaded
            .load(Ordering::Relaxed),
        2,
        "an absolute count, not a delta"
    );
}

/// An overdue tick whose delay would put its trigger below zero saturates to
/// "due immediately" rather than wrapping.
///
/// Reachable in a real world: a world saved at tick 40 with a tick 1,000 ticks
/// overdue, reopened before the clock has advanced. `u64` arithmetic on
/// `0 + (-1000)` wraps to about 18 quintillion, which schedules the tick past
/// the end of the universe rather than now — the failure is total and silent.
#[test]
fn an_overdue_tick_whose_delay_predates_the_clock_becomes_due_immediately() {
    let dir = tempdir("saturate");
    let pos = (3, 65, 3);

    {
        let world = open(&dir);
        let scheduled = world.scheduled_ticks();
        scheduled.set_game_tick(40);
        scheduled.with(|queues| {
            // Trigger 0 against a game tick of 40 is a delay of -40.
            assert!(queues.block.schedule(
                pos,
                "minecraft:sand".to_owned(),
                0,
                TickPriority::Normal
            ));
        });
        world.set_block(pos.0, pos.1, pos.2, "minecraft:sand");
        world.save_handle().save().expect("save");
    }

    let nbt = read_chunk_nbt(&dir, 0, 0);
    assert_eq!(
        raw_ticks(&nbt, "block_ticks")[0].1,
        -40,
        "the delay on disk is negative"
    );

    // Reopened with the clock back at 0: 0 + (-40) must saturate to 0.
    let world = open(&dir);
    let scheduled = world.scheduled_ticks();
    scheduled.set_game_tick(0);
    let _ = world.column(0, 0);
    let triggers: Vec<u64> = scheduled.with(|q| q.block.iter().map(|t| t.trigger_tick).collect());
    assert_eq!(
        triggers,
        vec![0],
        "an overdue tick must come back due immediately, not wrapped to a u64 \
         near 18446744073709551576"
    );
    // And it really is drainable, which is the behaviour that matters — a
    // trigger of 0 that never fires would be no better than a wrapped one.
    let drained = scheduled.with(|q| q.block.drain_due(0, 64).len());
    assert_eq!(drained, 1, "the restored tick must actually be due");
}

/// **The island check.** A persistent world's queues and the queues the tick
/// loop drains must be the same object.
///
/// The reason this needs its own test: the schema and the save path can both be
/// correct while `tick::run_tick_loop` keeps its own private queues, in which
/// case ticks fire correctly, save nothing, and no assertion above notices.
/// That is exactly the state this crate was in before this change — and it is
/// still the state until the tick loop takes the handle, which is why this
/// asserts the *seam* rather than the wiring.
#[test]
fn the_world_hands_out_one_shared_scheduled_tick_queue() {
    let dir = tempdir("one-queue");
    let world = open(&dir);
    let a = world.scheduled_ticks();
    let b = world.clone().scheduled_ticks();

    a.with(|q| {
        q.block
            .schedule((1, 2, 3), "minecraft:fire".to_owned(), 9, TickPriority::Low)
    });
    assert!(
        b.with(|q| q.block.has_scheduled((1, 2, 3), &"minecraft:fire".to_owned())),
        "a clone of the world must see the same queue"
    );

    a.set_game_tick(1234);
    assert_eq!(
        b.game_tick(),
        1234,
        "and the same game tick — the save path reads this to compute delays, so \
         two clocks here is issue #323's bug in a new place"
    );
}
