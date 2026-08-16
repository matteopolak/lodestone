//! A benchmark harness that loads a real, publicly-downloaded redstone
//! contraption into the **real production tick loop**
//! (`lodestone_server::IntegratedServer::open_in_memory_with_mobs`, the same
//! constructor a live singleplayer world runs) and reports where its
//! per-tick cost goes. Built for issue #548 (the incrementally-invalidated
//! redstone dependency graph), which is gated on exactly this measurement —
//! a previous agent correctly refused to rewrite the execution model without
//! a number attributing cost to neighbour scanning.
//!
//! # What it measures, and what it does not
//!
//! `TickPhase::ScheduledAndPhysics` (`lodestone_server::tick`'s own doc:
//! "the scheduled block-tick drain, fire/redstone/fluid propagation, random
//! ticks, falling blocks, vehicles, TNT, minecarts, dragons") is **not**
//! reachable from outside this crate: `TickPhase` itself is defined in a
//! private module and is not re-exported, and
//! `IntegratedServer` exposes only the whole-loop [`TickStats`] snapshot
//! (`tick_stats()`), never the [`TickClock`] instance
//! `phase_stats(TickPhase::ScheduledAndPhysics)` needs. See "Brokered hunks"
//! below for the one-line fix. So this harness reports two things instead,
//! neither of which needs that hook:
//!
//! - **`TickStats`** (`mspt_avg_ms`, `tps`, `overrun_count`) — the whole
//!   tick's cost, not redstone's share of it alone. Reported as context, and
//!   explicitly caveated: this machine runs other agents' concurrent
//!   `cargo` builds, so an absolute millisecond figure here is a *duration*
//!   gathered under unknown load (`CLAUDE.md`'s own rule: prefer a counter,
//!   re-run a timing-shaped result alone before trusting it).
//! - **`lodestone_server::redstone_counters::snapshot()`** — process-global,
//!   load-independent counts (notifications issued, cell reads, signal
//!   queries, wire recomputes, schedule requests/dedupes) behind the
//!   `redstone-counters` feature this crate's `Cargo.toml` now turns on for
//!   its dev build. This **is** redstone-specific — see that module's own
//!   doc table — and it is a counter, so it means the same thing regardless
//!   of how busy this machine is. Reported per elapsed tick (a rate, derived
//!   from [`IntegratedServer::server_tick_count`]'s own before/after delta,
//!   not from wall-clock time), which is the number #548 actually needs.
//!
//! # Loading, and what it does *not* reproduce
//!
//! A contraption is stamped into a flat world with
//! [`lodestone_server::ChunkSource::set_block`] — a raw write. This is
//! **not** the same as a player placing each block, and two consequences
//! follow, both confirmed by reading `ChunkSource::set_block`'s own doc
//! comment and `BlockTickFeed`'s inbound-scheduling methods (both
//! `pub(crate)`, unreachable from here):
//!
//! 1. No neighbour-update cascade runs at load time — only
//!    `crate::server::apply_use_item_on`/`apply_block_action` (the real
//!    packet handlers) trigger that, and they are not this harness's path.
//! 2. A `.litematic` region's own `PendingBlockTicks`/`PendingFluidTicks`
//!    (a repeater mid-cycle, a scheduled fluid update) are parsed by
//!    [`lodestone_anvil::schematic`] but **not** re-injected — there is no
//!    public API to hand the running world's `ScheduledTickQueue` a request
//!    from outside `lodestone-server`.
//!
//! So a contraption loaded this way starts from its captured **steady
//! state** with nothing scheduled to perturb it — see "Findings" in
//! `docs/redstone-benchmark-harness.md` for what this measures in practice,
//! and why a near-zero `redstone_counters` reading on a real contraption is
//! the expected, reproduced result of loading it this way, not evidence the
//! contraption is inert.
//!
//! # Fixtures: fetch-or-skip
//!
//! Fixtures live in `.cache/redstone-benchmarks/` (gitignored, never
//! committed — see `docs/legal-notices.md`). A fresh clone has none, so
//! every `#[ignore]`d test here checks for files first and **skips with a
//! printed message** rather than failing when the directory is empty or
//! missing; see `docs/redstone-benchmark-harness.md` for the exact `curl`
//! commands and full provenance (source URL, credited author, licence
//! clarity) for every file this was written against.
//!
//! # Brokered hunks (not made here — `lodestone-server` is a live agent's)
//!
//! - `crates/lodestone-server/src/tick.rs`: re-export `TickPhase`,
//!   `PhaseStats`, `WorstPhaseWindow`, `TICK_PHASE_NAMES` at the crate root
//!   (add them to the existing `pub use tick::{BlockTickFeed, ExplosionFeed,
//!   TickClock, TickStats};` line in `lib.rs`), and give
//!   `IntegratedServer` a `pub fn tick_clock(&self) -> Option<&TickClock>`
//!   beside its existing `tick_stats()`. Together these are what let an
//!   external caller read `ScheduledAndPhysics`'s own percentiles instead of
//!   the whole tick's.
//! - `crates/lodestone-server/src/tick.rs`: `BlockTickFeed::request_scheduled_ticks`
//!   is `pub(crate)`. Making it `pub`, plus a way for a caller of
//!   `open_in_memory_with_mobs` to reach the feed instance the spawned loop
//!   drains, would let a harness like this one re-inject a `.litematic`
//!   region's `PendingBlockTicks`/`PendingFluidTicks` and resume a captured
//!   circuit mid-cycle instead of only from a perturbation-free steady state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_anvil::schematic::{self, Schematic, SchematicBlock};
use lodestone_core::State;
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
    redstone_counters,
};
use uuid::Uuid;

// Same packet ids `lodestone-server`'s own `tests/tick_loop_light.rs` uses —
// this harness's `MinimalProtocol` only has to get a connection to `Play`,
// nothing else rides the wire, so the ids only have to agree with
// themselves (this test drives both ends of the duplex).
const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;

/// 26.2's overworld vertical bounds — see `lodestone_server::worldgen_data`'s
/// own `flat_generator`, which reads these from the bundled `noise_settings`
/// rather than a literal for the same reason this harness would like to but
/// cannot: that resolver is crate-private.
const OVERWORLD_MIN_Y: i32 = -64;

/// How long to let the tick loop run once a contraption is loaded and
/// counters are reset. ~100 real ticks at 20 Hz when the loop keeps up;
/// fewer under load, which is exactly why every rate this harness reports is
/// normalised by the loop's own tick counter rather than by this duration.
const TICK_WINDOW: Duration = Duration::from_secs(5);

/// A protocol double whose only job is reaching Play. Every encoder this
/// harness does not need answers its trait default (`ServerDirective::None`
/// / no light) — see this module's own doc for which seven methods
/// `ServerProtocol` actually requires.
#[derive(Debug, Default)]
struct MinimalProtocol;

impl ServerProtocol for MinimalProtocol {
    fn decode(&self, state: State, packet_id: i32, _payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => {
                ServerBound::Handshake { next_state: State::Login }
            }
            State::Login if packet_id == LOGIN_START => ServerBound::LoginStart {
                username: "RedstoneBench".to_owned(),
                uuid: Uuid::nil(),
            },
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        vec![ServerDirective::Send { packet_id: LOGIN_SUCCESS, payload: Vec::new() }]
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

/// Every base block name this harness counts as a "redstone component" for
/// the report — the families `lodestone_server::redstone*` files model, plus
/// the handful of vanilla interactables a farm typically wires into them.
/// Deliberately a flat list rather than a prefix test: `redstone_block` and
/// `redstone_wire` share a prefix with `redstone_lamp`/`redstone_torch` but
/// not with `lever`/`hopper`/`dispenser`, so "starts with redstone_" would
/// undercount.
const REDSTONE_COMPONENT_NAMES: &[&str] = &[
    "minecraft:redstone_wire",
    "minecraft:repeater",
    "minecraft:comparator",
    "minecraft:redstone_torch",
    "minecraft:redstone_wall_torch",
    "minecraft:redstone_lamp",
    "minecraft:redstone_block",
    "minecraft:lever",
    "minecraft:tripwire_hook",
    "minecraft:tripwire",
    "minecraft:target",
    "minecraft:observer",
    "minecraft:piston",
    "minecraft:sticky_piston",
    "minecraft:piston_head",
    "minecraft:moving_piston",
    "minecraft:dispenser",
    "minecraft:dropper",
    "minecraft:hopper",
    "minecraft:note_block",
    "minecraft:daylight_detector",
    "minecraft:stone_pressure_plate",
    "minecraft:oak_pressure_plate",
    "minecraft:heavy_weighted_pressure_plate",
    "minecraft:light_weighted_pressure_plate",
    "minecraft:stone_button",
    "minecraft:oak_button",
    "minecraft:powered_rail",
    "minecraft:detector_rail",
    "minecraft:activator_rail",
];

fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn is_redstone_component(state: &str) -> bool {
    REDSTONE_COMPONENT_NAMES.contains(&base_name(state))
}

/// The `.cache/redstone-benchmarks/` directory, resolved from this crate's
/// own manifest dir rather than the process cwd — `cargo test` sets the cwd
/// to the crate root, but resolving it explicitly keeps this correct however
/// the binary is invoked.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/redstone-benchmarks")
}

/// Every fixture file this harness knows how to name — see
/// `docs/redstone-benchmark-harness.md` for the `curl` commands and full
/// provenance for each. Listed explicitly (not a directory scan) so a
/// missing file's *name* appears in the skip message, not just a count.
const FIXTURE_FILES: &[&str] = &[
    "raid_farm.litematic",
    "Raid_Farm_Schematic_2.litematic",
    "IanXO4_Practical_Stacking_Raid_Farm_suggested.litematic",
    "bee-and-crop-farm.litematic",
];

struct LoadedFixture {
    name: String,
    schematic: Schematic,
}

fn load_available_fixtures() -> Vec<LoadedFixture> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    for &name in FIXTURE_FILES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        match schematic::load_schematic_file(&path) {
            Ok(schematic) => out.push(LoadedFixture { name: name.to_owned(), schematic }),
            Err(err) => eprintln!("redstone_benchmark: {name} failed to parse, skipping: {err}"),
        }
    }
    out
}

/// Bounding box of a schematic's own non-air placements, in its local
/// coordinate space. `None` for an empty block list.
fn bounds(blocks: &[SchematicBlock]) -> Option<(i32, i32, i32, i32, i32, i32)> {
    let mut it = blocks.iter();
    let first = it.next()?;
    let mut b = (first.x, first.x, first.y, first.y, first.z, first.z);
    for block in it {
        b.0 = b.0.min(block.x);
        b.1 = b.1.max(block.x);
        b.2 = b.2.min(block.y);
        b.3 = b.3.max(block.y);
        b.4 = b.4.min(block.z);
        b.5 = b.5.max(block.z);
    }
    Some(b)
}

/// Places `blocks` (in the schematic's own local space) into `source`,
/// offset so the schematic's lowest point sits one cell above `floor_top_y`
/// at world `(origin_x, origin_z)`. Returns the world-space bounding box
/// actually written.
fn stamp(
    source: &lodestone_server::FlatChunkSource,
    blocks: &[SchematicBlock],
    origin_x: i32,
    origin_z: i32,
    floor_top_y: i32,
    local_min_y: i32,
) -> (i32, i32, i32, i32) {
    let dy = floor_top_y + 1 - local_min_y;
    let mut min_x = i32::MAX;
    let mut max_x = i32::MIN;
    let mut min_z = i32::MAX;
    let mut max_z = i32::MIN;
    for block in blocks {
        let (wx, wy, wz) = (origin_x + block.x, dy + block.y, origin_z + block.z);
        source.set_block(wx, wy, wz, &block.state);
        min_x = min_x.min(wx);
        max_x = max_x.max(wx);
        min_z = min_z.min(wz);
        max_z = max_z.max(wz);
    }
    (min_x, max_x, min_z, max_z)
}

/// Loads one contraption into a fresh in-memory flat world, runs the real
/// tick loop for [`TICK_WINDOW`], and prints a report line. Returns nothing
/// — this is a benchmark harness, not an assertion; see this module's own
/// doc for why (there is no outside expectation to assert redstone-cascade
/// counts against yet, which is exactly the gap issue #548 is open on).
async fn run_one(fixture: &LoadedFixture) {
    let LoadedFixture { name, schematic } = fixture;
    let Some((min_x, max_x, min_y, max_y, min_z, max_z)) = bounds(&schematic.blocks) else {
        println!("redstone_benchmark: {name}: no non-air blocks, skipping");
        return;
    };
    let component_count =
        schematic.blocks.iter().filter(|b| is_redstone_component(&b.state)).count();
    let mut by_name: BTreeMap<&str, usize> = BTreeMap::new();
    for block in &schematic.blocks {
        if is_redstone_component(&block.state) {
            *by_name.entry(base_name(&block.state)).or_default() += 1;
        }
    }

    println!("== redstone_benchmark: {name} ({:?}) ==", schematic.format);
    if let Some(author) = &schematic.author {
        println!("   author (from file metadata): {author}");
    }
    println!(
        "   declared size: {:?}, non-air blocks placed: {}, reported_total_blocks: {:?}",
        schematic.size,
        schematic.blocks.len(),
        schematic.reported_total_blocks,
    );
    println!(
        "   local bounds: x[{min_x}..{max_x}] y[{min_y}..{max_y}] z[{min_z}..{max_z}], \
         redstone components: {component_count}"
    );
    for (name, count) in &by_name {
        println!("     {name}: {count}");
    }

    let settings = lodestone_server::world_preset_flat_settings(false);
    let floor_top_y = OVERWORLD_MIN_Y + settings.total_height() as i32 - 1;
    let source = lodestone_server::flat_chunk_source(settings);

    let origin_x = 0;
    let origin_z = 0;
    let (wx0, wx1, wz0, wz1) = stamp(&source, &schematic.blocks, origin_x, origin_z, floor_top_y, min_y);

    let cx0 = wx0.div_euclid(16) - 1;
    let cx1 = wx1.div_euclid(16) + 1;
    let cz0 = wz0.div_euclid(16) - 1;
    let cz1 = wz1.div_euclid(16) + 1;
    let mob_center = ((wx0 + wx1) / 2, (wz0 + wz1) / 2);

    let (server, client) = IntegratedServer::open_in_memory_with_mobs(
        MinimalProtocol,
        source,
        (cx0..=cx1, cz0..=cz1),
        mob_center,
        0,
        4,
    );

    let mut client = Connection::new(client);
    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    client.write_packet(LOGIN_START, &[0]).await.expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    // Let the seeding task's own column-generation batch (see
    // `tick::INITIAL_RANDOM_TICK_DEFERRAL_TICKS`'s doc) finish before the
    // measurement window starts, so a cold `world.column()` regeneration
    // does not get attributed to steady-state ticking cost.
    tokio::time::sleep(Duration::from_millis(2_500)).await;

    redstone_counters::reset();
    let tick_before = server.server_tick_count().unwrap_or(0);
    let stats_before = server.tick_stats();

    tokio::time::sleep(TICK_WINDOW).await;

    let tick_after = server.server_tick_count().unwrap_or(0);
    let stats_after = server.tick_stats();
    let snapshot = redstone_counters::snapshot();
    server.shutdown().await;

    let ticks_elapsed = tick_after.saturating_sub(tick_before).max(1);
    println!(
        "   ticks elapsed in {TICK_WINDOW:?}: {ticks_elapsed} (before={tick_before} after={tick_after})"
    );
    if let (Some(before), Some(after)) = (stats_before, stats_after) {
        println!(
            "   TickStats [whole loop, duration-based — see module doc caveat]: \
             mspt_avg before={:.3}ms after={:.3}ms, tps after={:.2}, overrun_count \
             before={} after={}",
            before.mspt_avg_ms, after.mspt_avg_ms, after.tps, before.overrun_count, after.overrun_count
        );
    }
    println!(
        "   redstone_counters over {ticks_elapsed} ticks (totals / per-tick rate): \
         notifications_issued={} ({:.3}/tick), cell_reads={} ({:.3}/tick), \
         state_parses={} ({:.3}/tick), signal_queries={} ({:.3}/tick), \
         wire_recomputes={} ({:.3}/tick), schedules_requested={} schedules_deduped={}, \
         max_notifications_per_drain={}",
        snapshot.notifications_issued,
        snapshot.notifications_issued as f64 / ticks_elapsed as f64,
        snapshot.cell_reads,
        snapshot.cell_reads as f64 / ticks_elapsed as f64,
        snapshot.state_parses,
        snapshot.state_parses as f64 / ticks_elapsed as f64,
        snapshot.signal_queries,
        snapshot.signal_queries as f64 / ticks_elapsed as f64,
        snapshot.wire_recomputes,
        snapshot.wire_recomputes as f64 / ticks_elapsed as f64,
        snapshot.schedules_requested,
        snapshot.schedules_deduped,
        snapshot.max_notifications_per_drain,
    );
}

/// **The benchmark.** `#[ignore]`d: it needs `.cache/redstone-benchmarks/`
/// fixtures a fresh clone does not have, and it runs the real tick loop for
/// several real seconds per contraption. Run explicitly:
/// `cargo test -p lodestone-anvil --test redstone_benchmark -- --ignored --nocapture`
/// after fetching fixtures per `docs/redstone-benchmark-harness.md`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn redstone_contraptions_report() {
    let fixtures = load_available_fixtures();
    if fixtures.is_empty() {
        println!(
            "redstone_benchmark: no fixtures found under {} — this is expected on a fresh \
             clone (`.cache/` is gitignored); see docs/redstone-benchmark-harness.md for the \
             curl commands to fetch them, then re-run this test",
            fixtures_dir().display()
        );
        return;
    }
    for fixture in &fixtures {
        run_one(fixture).await;
    }
}

/// A cheap, always-runnable control: the loader itself needs no fixtures,
/// network, or tick loop, so this runs in every `cargo test` (not
/// `#[ignore]`d) and proves the parsers actually work end to end whether or
/// not `.cache/` is populated — see `crates/lodestone-anvil/src/schematic.rs`
/// for the format-level unit tests this complements.
#[test]
fn fixture_directory_is_optional_and_parsing_is_robust_to_its_absence() {
    // Not an assertion on file *contents* (those are format-level tests in
    // `schematic.rs`) — this only proves that an absent `.cache/` directory
    // is a zero-length list, never a panic, which is the property that keeps
    // `redstone_contraptions_report` fetch-or-skip rather than fetch-or-fail.
    let fixtures = load_available_fixtures();
    assert!(
        fixtures.len() <= FIXTURE_FILES.len(),
        "loaded more fixtures than are named in FIXTURE_FILES — impossible unless the loader \
         is reading the wrong directory"
    );
}
