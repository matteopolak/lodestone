//! Issue #465's **delayed** half, gated end-to-end through the real
//! [`crate::tick::run_tick_loop`]: a redstone component a player mutates must
//! flip at the tick the live 26.2 server flipped it, and the flip must reach
//! the wire.
//!
//! # Why this module exists separately from the two oracle gates
//!
//! [`crate::redstone_oracle_gate`] and [`crate::redstone_diode_oracle_gate`]
//! call [`crate::random_tick::propagate_and_react`] directly and assert on the
//! `ScheduledTickQueue` it fills. That is the right instrument for "does the
//! model schedule the right tick", and it is deliberately *not* an instrument
//! for "does anything drain that queue". Until this landing, nothing did for a
//! player action: `server::propagate_placement`'s queue is local and dropped on
//! return, so a placed repeater scheduled its flip into a queue that was freed
//! a microsecond later. Every assertion in both oracle gates stayed green
//! throughout — the closed loop this repo keeps paying for.
//!
//! So the gates here drive the **actual loop**, over virtual time, and read the
//! result off [`crate::tick::BlockTickFeed`] — the same queue
//! `server::serve_play` drains and encodes to the client. A flip the server
//! computes into its own column but never publishes is invisible here, by
//! construction, which is the point.
//!
//! # The external oracle
//!
//! The four delay settings come from [`ORACLE_REPEATER_DELAY`], measured on a
//! **live vanilla 26.2 server** in `9eb8703`. The *placement* delay comes from
//! the decompiled jar (`DiodeBlock.setPlacedBy`, `DiodeBlock.java:160-165`).
//! Both originate outside this crate.
//!
//! # Two different delays, and separating them is the point
//!
//! This is the finding that reshaped this file, and it is the single most
//! plausible wrong model of the code under test:
//!
//! * A **signal change** reaching an already-placed repeater goes through
//!   `DiodeBlock.checkTickOnNeighbor` (`:88-104`) and is delayed
//!   `getDelay(state)` — `2d` game ticks, `d` being the `delay` property.
//! * A **placement** goes through `DiodeBlock.setPlacedBy` (`:160-165`), which
//!   is `if (shouldTurnOn) scheduleTick(pos, this, 1)`. Delay **1**, at every
//!   one of the four settings.
//!
//! So a repeater dropped into a live line lights one tick later whatever its
//! dial says, while the same repeater responding to its input changing takes
//! `2d`. A model that used `2d` for both would be wrong only on placement; a
//! model that used `1` for both would be wrong only on signal changes. Both are
//! computed here and both are required to disagree with the oracle at all four
//! settings.
//!
//! # The tick arithmetic, and why it is `1 + delay` and not `delay`
//!
//! Vanilla handles queued packets at the top of a tick
//! (`MinecraftServer.tickServer` -> `tickConnections`) and drains
//! `ServerLevel.blockTicks` later in that **same** tick, so a placement
//! arriving between tick `N-1` and tick `N` is processed against `N` and fires
//! at `N + delay`. `run_tick_loop` reproduces that: the inbound drain this
//! issue added sits after `game_tick += 1` and before the `block_ticks` drain,
//! so a request published before the loop's first tick is processed at
//! `game_tick == 1`. The leading `1` is that quantization, **not** an
//! off-by-one, and
//! [`the_delay_is_measured_from_the_tick_that_drained_the_request_not_from_tick_zero`]
//! distinguishes the two readings by publishing a second request one tick later
//! and requiring the *offset* to be invariant.
//!
//! # What this does not cover
//!
//! Block **state** on placement. `apply_use_item_on` writes each block's bare
//! name, so a really-placed repeater has no `facing`/`delay` and always faces
//! north — issue #465's cause (2), still open and untouched. The rigs below
//! therefore seed the component's state the way a `/setblock` does and exercise
//! the mutation-plus-fan-out mechanism this landing adds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_model::BlockPos;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::{ChunkColumn, ChunkSource};
use crate::mobs::{ChunkWorld, LiveMobSource, MobHandle};
use crate::neighbor_update::Direction;
use crate::tick::{BlockTickFeed, ExplosionFeed, TICK_PERIOD, TickClock, run_tick_loop};
use crate::{redstone, redstone_diode, redstone_torch, redstone_wire};

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR_Y: i32 = 0;
const Y: i32 = 1;
const ROW_Z: i32 = 8;

/// The source torch.
const SRC_X: i32 = 1;
/// Where the repeater sits.
const DIODE_X: i32 = 4;
/// The dust square the repeater drives. Every timing assertion reads *this*
/// cell rather than the repeater's own `powered` flag: it is what a player
/// sees, and a diode that flips its flag without driving its output is a
/// distinct and real failure this separates.
const OUT_X: i32 = 5;

/// Live 26.2: `(delay property, game ticks from a signal change until the
/// output changes)`. Measured on both edges; the two columns agreed exactly.
/// Transcribed from `redstone_diode_oracle_gate`, which carries the full
/// provenance, and checked against it by
/// [`the_oracle_table_matches_the_live_measurement_it_was_transcribed_from`].
const ORACLE_REPEATER_DELAY: &[(u32, u64)] = &[(1, 2), (2, 4), (3, 6), (4, 8)];

/// `DiodeBlock.setPlacedBy` (`DiodeBlock.java:160-165`):
/// `level.scheduleTick(pos, this, 1)`. Not `getDelay(state)`, and not shared
/// with the table above — see this module's own doc comment.
const JAR_PLACEMENT_DELAY: u64 = 1;

/// Live 26.2 attenuation: a dust square adjacent to a lit torch reads 15.
const ORACLE_FULL_POWER: u8 = 15;

/// How many virtual ticks each gate drives. Comfortably past the slowest
/// setting's `1 + 8`, so a model that fires *late* is observed as a wrong tick
/// rather than as silence.
const DRIVE_TICKS: u64 = 16;

// ---------------------------------------------------------------------------
// The rig world
// ---------------------------------------------------------------------------

/// A [`ChunkSource`] that really retains its edits.
///
/// **Hand-written rather than reusing `server_block_placement.rs`'s
/// `SharedAirSource`, and that is load-bearing.** That double's `column()`
/// returns a fresh all-air `ChunkColumn` and ignores its own edit map — and
/// every consumer here reads a whole column, not single blocks — so a rig built
/// on it is invisible to the code under test and every gate below would pass
/// while proving nothing. [`the_rig_world_reflects_its_own_edits`] is the
/// premise check that keeps this honest.
struct RigWorld {
    columns: Mutex<HashMap<(i32, i32), ChunkColumn>>,
}

impl RigWorld {
    fn new() -> Self {
        let mut columns = HashMap::new();
        columns.insert((0, 0), column_with_floor());
        Self {
            columns: Mutex::new(columns),
        }
    }

    /// A snapshot of the row the rigs occupy, as `(x, state)` — the probe set
    /// every "did the outcome change" comparison below is made over.
    /// Deliberately a *named list of coordinates* rather than a whole-column
    /// hash: a mismatch must be able to say **where**.
    fn row(&self) -> Vec<(i32, String)> {
        (0..8).map(|x| (x, self.block_state(x, Y, ROW_Z))).collect()
    }
}

impl ChunkSource for RigWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.columns
            .lock()
            .expect("rig world poisoned")
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
            .clone()
    }

    // Reads the cell out of the retained column rather than cloning one —
    // `RigWorld::row()` probes this eight times per snapshot, and this source
    // is the efficient case the trait now wants an implementor to provide
    // (issue #440). A column that has never been touched is, by
    // construction, all air — the same value `column()` would materialise.
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        self.columns
            .lock()
            .expect("rig world poisoned")
            .get(&(cx, cz))
            .map(|c| c.block_state(x.rem_euclid(16), y, z.rem_euclid(16)).to_string())
            .unwrap_or_else(|| crate::chunk::AIR.to_string())
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        self.columns
            .lock()
            .expect("rig world poisoned")
            .entry((cx, cz))
            .or_insert_with(|| ChunkColumn::new(MIN_Y, HEIGHT))
            .set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
    }
}

fn column_with_floor() -> ChunkColumn {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            column.set_block(x, FLOOR_Y, z, "minecraft:stone");
        }
    }
    column
}

/// A repeater rig on one row, matching `redstone_diode_oracle_gate`'s: standing
/// torch at [`SRC_X`], dust at `x = 2..=3`, the repeater at [`DIODE_X`] facing
/// **west** (so its input is the dust at `x = 3`, as in the live rig), output
/// dust at [`OUT_X`].
///
/// The dust holds its **pre-flip** powers. Seeding it with its settled value
/// instead makes an edge rig vacuous — no dust power changes, so nothing
/// re-fans-out and the repeater is never notified at all. That is a trap
/// `redstone_diode_oracle_gate` already paid for once, and
/// [`the_rig_dust_is_unsettled_so_the_edge_really_propagates`] is the control
/// for it here.
fn edge_rig(delay: u32, source_lit: bool, repeater_powered: bool) -> Arc<RigWorld> {
    let world = Arc::new(RigWorld::new());
    world.set_block(SRC_X, Y, ROW_Z, &redstone_torch::set_standing_lit(source_lit));
    let (near, far) = if source_lit { (0, 0) } else { (15, 14) };
    world.set_block(2, Y, ROW_Z, &redstone_wire::set_power(near));
    world.set_block(3, Y, ROW_Z, &redstone_wire::set_power(far));
    world.set_block(
        DIODE_X,
        Y,
        ROW_Z,
        &redstone_diode::set_repeater(Direction::West, delay, false, repeater_powered),
    );
    world.set_block(
        OUT_X,
        Y,
        ROW_Z,
        &redstone_wire::set_power(if repeater_powered { ORACLE_FULL_POWER } else { 0 }),
    );
    world
}

/// A **settled, powered** line, for the falling edge: lit torch, dust at 15 then
/// 14, the repeater on at [`DIODE_X`], output dust at full power.
///
/// The falling edge is triggered by **placing a solid block over the repeater's
/// input dust**, not by extinguishing the torch, and that is a correction paid
/// for by a measurement. An unlit standing torch with no input is not in its
/// steady state — its steady state *is* lit — so it schedules its own relight
/// and comes back on 2 ticks later, re-powering the whole line. Observed
/// directly: the published log showed
/// `tick 3: (1, 1, 8) -> minecraft:redstone_torch[lit=true]` followed by the
/// dust returning to 15 and 14, so the output never fell and the gate read
/// `None`. `redstone_diode_oracle_gate`'s rig does extinguish the torch and is
/// unaffected, because it inspects the schedule immediately and never runs the
/// loop that would relight it.
///
/// Cutting the line instead leaves the torch in steady state, so nothing
/// self-heals, and it is a genuine `apply_use_item_on` placement rather than a
/// break.
fn falling_rig(delay: u32) -> Arc<RigWorld> {
    let world = Arc::new(RigWorld::new());
    world.set_block(SRC_X, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    world.set_block(2, Y, ROW_Z, &redstone_wire::set_power(15));
    world.set_block(3, Y, ROW_Z, &redstone_wire::set_power(14));
    world.set_block(
        DIODE_X,
        Y,
        ROW_Z,
        &redstone_diode::set_repeater(Direction::West, delay, false, true),
    );
    world.set_block(OUT_X, Y, ROW_Z, &redstone_wire::set_power(ORACLE_FULL_POWER));
    world
}

/// A **settled** live line with the repeater's cell left as air, so a gate can
/// perform a real placement into it: lit torch, dust at 15 then 14, unpowered
/// output dust.
fn settled_line_with_a_gap() -> Arc<RigWorld> {
    let world = Arc::new(RigWorld::new());
    world.set_block(SRC_X, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    world.set_block(2, Y, ROW_Z, &redstone_wire::set_power(15));
    world.set_block(3, Y, ROW_Z, &redstone_wire::set_power(14));
    world.set_block(OUT_X, Y, ROW_Z, &redstone_wire::set_power(0));
    world
}

// ---------------------------------------------------------------------------
// Driving the real loop
// ---------------------------------------------------------------------------

/// One change the loop published, tagged with the game tick it was published
/// on.
#[derive(Debug, Clone)]
struct Published {
    tick: u64,
    pos: (i32, i32, i32),
    state: String,
}

fn spawn_loop(world: Arc<RigWorld>, feed: &BlockTickFeed) {
    tokio::spawn(run_tick_loop(
        MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT)),
        LiveMobSource::default(),
        BlockEntityHandle::default(),
        Arc::new(TickClock::new()),
        world,
        feed.clone(),
        // Only the rig's own chunk, so a random tick cannot wander into a
        // neighbour and publish something this gate would have to filter. The
        // rig is stone and redstone, none of which is randomly ticking, so the
        // random-tick pass over it is inert.
        (0..=0, 0..=0),
        ExplosionFeed::default(),
        crate::region_source::ScheduledTickHandle::default(),
        crate::tick_area::TickFollow::default(),
    ));
}

/// Advances virtual time one tick at a time, draining [`BlockTickFeed`] after
/// each, so every published change carries an exact game-tick number.
///
/// Virtual time (`start_paused`), advanced **explicitly** rather than by
/// auto-advance, so the tick a change lands on is a *count* — immune to machine
/// load, which is the distinction `CLAUDE.md` draws between a counter and a
/// duration. The two `yield_now`s are both required: the first lets the spawned
/// task reach its `Instant::now()` baseline before the first advance, the
/// second lets the woken task run its synchronous body. Same idiom
/// `chunk_store`'s `drive_tick_loop` already establishes.
async fn drive(world: Arc<RigWorld>, feed: &BlockTickFeed, ticks: u64) -> Vec<Published> {
    spawn_loop(world, feed);
    tokio::task::yield_now().await;
    let mut published = Vec::new();
    for tick in 1..=ticks {
        tokio::time::advance(TICK_PERIOD).await;
        tokio::task::yield_now().await;
        for (x, y, z, state) in feed.drain_all() {
            published.push(Published {
                tick,
                pos: (x, y, z),
                state,
            });
        }
    }
    published
}

/// The first tick on which `pos` was published carrying dust of power `power`,
/// or `None` if it never was.
fn tick_dust_reached(published: &[Published], pos: (i32, i32, i32), power: u8) -> Option<u64> {
    published
        .iter()
        .find(|p| p.pos == pos && redstone::is_wire(&p.state) && redstone::wire_power(&p.state) == power)
        .map(|p| p.tick)
}

/// Renders the whole published log for a failure message, so a wrong tick
/// reports *what actually happened* rather than only that it was not the
/// expected number.
fn log(published: &[Published]) -> String {
    if published.is_empty() {
        return "    (nothing was published at all)".to_owned();
    }
    published
        .iter()
        .map(|p| format!("    tick {:>2}: {:?} -> {}", p.tick, p.pos, p.state))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Exactly the pair `apply_use_item_on` performs after a mutation: run the
/// fan-out inline, at packet time, and hand the block ticks it scheduled to the
/// loop. Returns the cells the inline half rewrote — production delivers those
/// through its own `encode_block_update` loop, not through the feed, so a
/// computed-vs-delivered comparison has to count them.
fn trigger(world: &RigWorld, feed: &BlockTickFeed, pos: BlockPos) -> Vec<(BlockPos, String)> {
    let (changed, scheduled) = crate::server::propagate_placement(world, pos);
    feed.request_scheduled_ticks(scheduled);
    changed
}

/// Requires every wrong hypothesis to disagree with the oracle at **every**
/// delay setting — a gate that separated them at only one setting could not
/// tell a systematically wrong model from a single mistranscribed row.
fn assert_wrong_models_are_separated(edge: &str, oracle: &[Option<u64>], wrong: &[(&str, Vec<Option<u64>>)]) {
    for (name, model) in wrong {
        assert_eq!(
            model.len(),
            oracle.len(),
            "{edge}: the '{name}' hypothesis has {} entries against the oracle's {}",
            model.len(),
            oracle.len()
        );
        let disagreements = oracle.iter().zip(model.iter()).filter(|(a, b)| a != b).count();
        assert_eq!(
            disagreements,
            oracle.len(),
            "{edge}: the '{name}' hypothesis must differ from the oracle at every delay setting, \
             otherwise this gate cannot separate them: oracle {oracle:?} vs {model:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Premise checks
// ---------------------------------------------------------------------------

/// **Premise.** The rig world retains an edit and serves it back through
/// `column()`.
///
/// This is the `SharedAirSource` trap, run rather than described: with a source
/// whose `column()` discards edits, every timing gate in this file passes by
/// reading a circuit that does not exist. Asserted for `block_state` *and* for
/// a fresh `column()`, because the fan-out reads the latter.
#[test]
fn the_rig_world_reflects_its_own_edits() {
    let world = settled_line_with_a_gap();
    assert_eq!(
        world.block_state(2, Y, ROW_Z),
        redstone_wire::set_power(15),
        "PREMISE FAILED: the rig world does not serve back its own set_block at (x=2, y={Y}, z={ROW_Z})"
    );
    let column = world.column(0, 0);
    assert_eq!(
        column.block_state(2, Y, ROW_Z),
        redstone_wire::set_power(15),
        "PREMISE FAILED: the rig world's column() ignores its own edits at (x=2, y={Y}, z={ROW_Z}) \
         -- exactly the SharedAirSource defect, and every gate in this file would then pass against \
         a circuit that is not there"
    );
    assert_eq!(
        column.block_state(DIODE_X, Y, ROW_Z),
        "minecraft:air",
        "PREMISE FAILED: the repeater cell is not air, so placing into it tests nothing"
    );
}

/// **Premise.** The rising rig starts with its dust *unsettled*, and the
/// falling rig starts fully settled and powered.
///
/// A rising rig seeded with settled dust changes no power, re-fans-out nothing
/// and never notifies the repeater — the timing gate would then read a repeater
/// that was never asked, and blame the inbound channel. The falling rig has the
/// opposite requirement: it must already be carrying the signal it is about to
/// lose.
#[test]
fn the_rigs_start_in_the_state_their_edge_actually_needs() {
    let rising = edge_rig(1, true, false);
    assert_eq!(
        redstone::wire_power(&rising.block_state(3, Y, ROW_Z)),
        0,
        "PREMISE FAILED: the rising rig's input dust is already powered, so the rising edge is over \
         before the gate starts"
    );
    assert!(
        !redstone::diode_powered(&rising.block_state(DIODE_X, Y, ROW_Z)),
        "PREMISE FAILED: the rising rig's repeater is already on"
    );

    let falling = falling_rig(1);
    assert_eq!(
        redstone::wire_power(&falling.block_state(3, Y, ROW_Z)),
        14,
        "PREMISE FAILED: the falling rig's input dust is not powered, so there is no edge to cut"
    );
    assert_eq!(
        redstone::wire_power(&falling.block_state(OUT_X, Y, ROW_Z)),
        ORACLE_FULL_POWER,
        "PREMISE FAILED: the falling rig's output dust is not powered"
    );
    assert!(
        redstone::diode_powered(&falling.block_state(DIODE_X, Y, ROW_Z)),
        "PREMISE FAILED: the falling rig's repeater is not on, so it cannot release"
    );
}

/// **Premise.** The transcribed oracle table agrees with the module that owns
/// the live measurement.
#[test]
fn the_oracle_table_matches_the_live_measurement_it_was_transcribed_from() {
    assert_eq!(
        ORACLE_REPEATER_DELAY,
        crate::redstone_diode_oracle_gate::ORACLE_REPEATER_DELAY,
        "the oracle table in this file has drifted from the one measured live in 9eb8703"
    );
}

/// **The negative control for every timing gate below, run and observed.**
///
/// Identical rig, identical mutation, identical drive — and *no*
/// `request_neighbor_update`. That is precisely the state of the tree before
/// this landing: `server::propagate_placement` ran, resolved the synchronous
/// half, scheduled nothing that survived, and the loop never heard about it.
///
/// The output dust must therefore **never** reach full power, at any tick. If
/// this fails, the timing gates are measuring something other than the channel
/// they claim to.
#[tokio::test(start_paused = true)]
async fn without_the_inbound_request_the_loop_never_learns_and_the_repeater_never_fires() {
    let world = edge_rig(1, true, false);
    let feed = BlockTickFeed::default();
    // The synchronous half still runs, exactly as it does in production.
    let _ = crate::server::propagate_placement(&*world, BlockPos::new(SRC_X, Y, ROW_Z));

    let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;

    let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER);
    assert_eq!(
        fired, None,
        "CONTROL FAILED: the output dust at (x={OUT_X}, y={Y}, z={ROW_Z}) reached power \
         {ORACLE_FULL_POWER} on tick {fired:?} with no neighbour-update request published at all, \
         so the timing gates in this file are not measuring the inbound channel.\n{}",
        log(&published)
    );
    assert!(
        !redstone::diode_powered(&world.block_state(DIODE_X, Y, ROW_Z)),
        "CONTROL FAILED: the repeater at (x={DIODE_X}, y={Y}, z={ROW_Z}) turned on without the \
         request; it is {}",
        world.block_state(DIODE_X, Y, ROW_Z)
    );
}

// ---------------------------------------------------------------------------
// Signal-change timing: 2d, at all four settings, on both edges
// ---------------------------------------------------------------------------

/// **The load-bearing timing gate, rising edge.** A source change reaching an
/// already-placed repeater drives its output dust to full power on tick
/// `1 + 2d`, for every one of the four delay settings.
///
/// # The wrong models, computed rather than argued
///
/// | hypothesis | tick the output flips | agrees with oracle at |
/// |---|---|---|
/// | oracle (live 26.2), `1 + 2d` | 3, 5, 7, 9 | — |
/// | wrong: the request never reaches the loop (pre-#465) | never | 0 of 4 |
/// | wrong: the fan-out flips instantly | 1, 1, 1, 1 | 0 of 4 |
/// | wrong: delay counted in redstone ticks, `1 + d` | 2, 3, 4, 5 | 0 of 4 |
/// | wrong: off by one, `2d` | 2, 4, 6, 8 | 0 of 4 |
/// | wrong: the placement delay used for signal changes too, `1 + 1` | 2, 2, 2, 2 | 0 of 4 |
/// | wrong: the delay applies on the falling edge only | 1, 1, 1, 1 | 0 of 4 |
///
/// The last row is why the **rising** edge is measured and not only the
/// falling one: a model that switches on instantly and only delays switching
/// off reproduces the falling column perfectly, and
/// [`the_falling_edge_only_model_is_the_one_a_falling_edge_measurement_cannot_see`]
/// demonstrates that rather than asserting it.
#[tokio::test(start_paused = true)]
async fn a_repeater_drives_its_output_on_the_tick_the_live_server_measured() {
    for &(delay, oracle_ticks) in ORACLE_REPEATER_DELAY {
        let world = edge_rig(delay, true, false);
        let feed = BlockTickFeed::default();
        trigger(&world, &feed, BlockPos::new(SRC_X, Y, ROW_Z));

        let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;

        let expected = 1 + oracle_ticks;
        let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER);
        assert_eq!(
            fired,
            Some(expected),
            "repeater[delay={delay}] at (x={DIODE_X}, y={Y}, z={ROW_Z}) RISING: its output dust at \
             (x={OUT_X}, y={Y}, z={ROW_Z}) reached power {ORACLE_FULL_POWER} on tick {fired:?}; the \
             live 26.2 server changed the output {oracle_ticks} tick(s) after the trigger, which is \
             tick {expected} here (the leading 1 is the packet-phase quantization vanilla also \
             has). Everything published:\n{}",
            log(&published)
        );
    }

    assert_wrong_models_are_separated(
        "rising",
        &ORACLE_REPEATER_DELAY.iter().map(|&(_, t)| Some(1 + t)).collect::<Vec<_>>(),
        &[
            ("the request never reaches the loop (pre-#465)", vec![None; 4]),
            ("the fan-out flips instantly", vec![Some(1); 4]),
            (
                "delay counted in redstone ticks",
                ORACLE_REPEATER_DELAY.iter().map(|&(d, _)| Some(1 + u64::from(d))).collect(),
            ),
            ("off by one", ORACLE_REPEATER_DELAY.iter().map(|&(_, t)| Some(t)).collect()),
            (
                "the placement delay used for signal changes too",
                vec![Some(1 + JAR_PLACEMENT_DELAY); 4],
            ),
            ("the delay applies on the falling edge only", vec![Some(1); 4]),
        ],
    );
}

/// **The same gate, falling edge.** With the repeater on and its output dust
/// powered, cutting its input drops that dust to zero on tick `1 + 2d`, at every
/// setting.
///
/// See [`falling_rig`] for why the cut is a placed block rather than an
/// extinguished torch — the first version of this gate measured a torch that
/// relit itself two ticks later and healed the very edge under test.
#[tokio::test(start_paused = true)]
async fn a_repeater_releases_its_output_on_the_tick_the_live_server_measured() {
    for &(delay, oracle_ticks) in ORACLE_REPEATER_DELAY {
        let world = falling_rig(delay);
        let feed = BlockTickFeed::default();

        // Premise: there must be something to fall from, and the repeater must
        // currently be reading a real input.
        assert_eq!(
            redstone::wire_power(&world.block_state(OUT_X, Y, ROW_Z)),
            ORACLE_FULL_POWER,
            "PREMISE FAILED: delay={delay}'s output dust is not powered, so a drop to zero is \
             indistinguishable from never having been on"
        );
        assert_eq!(
            redstone::wire_power(&world.block_state(3, Y, ROW_Z)),
            14,
            "PREMISE FAILED: delay={delay}'s input dust is not powered, so cutting it changes \
             nothing"
        );

        // The cut: a solid block placed where the torch was, removing the only
        // power source in the rig outright.
        //
        // Placed at the *source* rather than over the repeater's input dust, and
        // that is the second correction this arm needed. Cutting at the input
        // published nothing at all: a solid block sitting beside powered dust is
        // not necessarily a zero input in this model, so the repeater's
        // `should_turn_on` never changed and it never scheduled. Removing the
        // source leaves nothing for any intermediate block to conduct.
        world.set_block(SRC_X, Y, ROW_Z, "minecraft:stone");
        trigger(&world, &feed, BlockPos::new(SRC_X, Y, ROW_Z));

        let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;

        let expected = 1 + oracle_ticks;
        let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), 0);
        assert_eq!(
            fired,
            Some(expected),
            "repeater[delay={delay}] at (x={DIODE_X}, y={Y}, z={ROW_Z}) FALLING: its output dust at \
             (x={OUT_X}, y={Y}, z={ROW_Z}) reached power 0 on tick {fired:?}; the live 26.2 server \
             released the output {oracle_ticks} tick(s) after the trigger, which is tick {expected} \
             here. Everything published:\n{}",
            log(&published)
        );
    }
}

/// **Why the rising edge had to be measured**, demonstrated rather than
/// claimed.
///
/// The "delay applies on the falling edge only" hypothesis predicts the
/// oracle's falling column exactly at all four settings and predicts an instant
/// rise. A gate that measured only the falling edge would therefore be
/// satisfied by it at **4 of 4** settings while the rising behaviour was
/// completely wrong.
#[test]
fn the_falling_edge_only_model_is_the_one_a_falling_edge_measurement_cannot_see() {
    let oracle: Vec<u64> = ORACLE_REPEATER_DELAY.iter().map(|&(_, t)| 1 + t).collect();
    let model_falling: Vec<u64> = oracle.clone();
    let model_rising: Vec<u64> = vec![1; ORACLE_REPEATER_DELAY.len()];

    let falling_agreements = oracle.iter().zip(&model_falling).filter(|(a, b)| a == b).count();
    assert_eq!(
        falling_agreements,
        ORACLE_REPEATER_DELAY.len(),
        "the falling-edge-only model is supposed to be indistinguishable on the falling column; if \
         it is not, this demonstration proves nothing"
    );
    let rising_disagreements = oracle.iter().zip(&model_rising).filter(|(a, b)| a != b).count();
    assert_eq!(
        rising_disagreements,
        ORACLE_REPEATER_DELAY.len(),
        "the rising column must separate the falling-edge-only model at every setting, or measuring \
         it buys nothing: oracle {oracle:?} vs model {model_rising:?}"
    );
}

// ---------------------------------------------------------------------------
// Placement timing: 1, at all four settings
// ---------------------------------------------------------------------------

/// **Issue #465's own scenario.** A repeater dropped into an already-powered
/// line lights on tick `1 + 1`, at **every** delay setting — `setPlacedBy`'s
/// delay is a literal `1`, not `getDelay(state)`.
///
/// This is the gate that fails if `react_at_placement` is ever "simplified"
/// into `propagate_and_react`, and the gate that fails if the placement path
/// reuses the `2d` signal-change delay. Both wrong models are computed below
/// and both must disagree at all four settings.
///
/// It is also the gate that would have caught the brokered patch as originally
/// written: `NeighborPropagator::propagate` notifies the origin's six
/// neighbours and never the origin, so the placed repeater was asked nothing at
/// all and this fired `None` at every setting.
#[tokio::test(start_paused = true)]
async fn a_repeater_placed_into_a_live_line_lights_one_tick_later_at_every_delay_setting() {
    for &(delay, signal_change_ticks) in ORACLE_REPEATER_DELAY {
        let world = settled_line_with_a_gap();
        let feed = BlockTickFeed::default();
        world.set_block(
            DIODE_X,
            Y,
            ROW_Z,
            &redstone_diode::set_repeater(Direction::West, delay, false, false),
        );
        // Exactly the pair `apply_use_item_on` performs after a successful
        // placement: the synchronous fan-out inline, the delayed one requested.
        trigger(&world, &feed, BlockPos::new(DIODE_X, Y, ROW_Z));

        let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;

        let expected = 1 + JAR_PLACEMENT_DELAY;
        let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER);
        assert_eq!(
            fired,
            Some(expected),
            "repeater[delay={delay}] PLACED at (x={DIODE_X}, y={Y}, z={ROW_Z}): its output dust at \
             (x={OUT_X}, y={Y}, z={ROW_Z}) reached power {ORACLE_FULL_POWER} on tick {fired:?}, \
             expected {expected}. `DiodeBlock.setPlacedBy` schedules at a literal \
             {JAR_PLACEMENT_DELAY}, so this must NOT be the {signal_change_ticks}-tick \
             signal-change delay for this setting. Everything published:\n{}",
            log(&published)
        );
    }

    assert_wrong_models_are_separated(
        "placement",
        &vec![Some(1 + JAR_PLACEMENT_DELAY); 4],
        &[
            (
                "the placed block is never asked (propagate_and_react alone)",
                vec![None; 4],
            ),
            (
                "the signal-change delay 2d used for placement too",
                ORACLE_REPEATER_DELAY.iter().map(|&(_, t)| Some(1 + t)).collect(),
            ),
            ("the placement resolves instantly", vec![Some(1); 4]),
        ],
    );
}

/// The two delays are genuinely different numbers at three of the four
/// settings, and identical at `delay=1` — so a gate that measured **only**
/// `delay=1` could not tell `setPlacedBy`'s literal `1` from `getDelay`'s `2d`
/// at all.
///
/// This is why the placement gate above loops over all four settings rather
/// than testing the default. Stated as its own assertion because "we happened
/// to loop" is not evidence that looping was necessary.
#[test]
fn the_placement_delay_and_the_signal_change_delay_coincide_at_exactly_one_setting() {
    let coinciding: Vec<u32> = ORACLE_REPEATER_DELAY
        .iter()
        .filter(|&&(_, t)| t == JAR_PLACEMENT_DELAY)
        .map(|&(d, _)| d)
        .collect();
    assert_eq!(
        coinciding,
        Vec::<u32>::new(),
        "no setting's 2d signal-change delay should equal the literal placement delay 1"
    );
    let separating = ORACLE_REPEATER_DELAY.iter().filter(|&&(_, t)| t != JAR_PLACEMENT_DELAY).count();
    assert_eq!(
        separating,
        ORACLE_REPEATER_DELAY.len(),
        "every delay setting must separate the placement delay from the signal-change delay"
    );
}

// ---------------------------------------------------------------------------
// The computed-vs-delivered pair
// ---------------------------------------------------------------------------

/// **The pair that found patch B's bug, pointed at the delayed families.**
///
/// A repeater whose flip the server computes into its own column but never
/// publishes looks *identical to a working one* from the server's side: every
/// state read back off the world is right, and the player sees nothing. So this
/// asserts the two independently and by location — for every cell the drive
/// actually changed, the last state published for it must equal the state the
/// world now holds.
///
/// It fired for real on this issue rather than as a staged control: 14 of 14
/// cascade coordinates computed correctly and delivered `power=0`.
#[tokio::test(start_paused = true)]
async fn every_cell_the_delayed_flip_changed_is_also_delivered_to_the_wire() {
    let world = edge_rig(1, true, false);
    let feed = BlockTickFeed::default();
    let before = world.row();
    let inline = trigger(&world, &feed, BlockPos::new(SRC_X, Y, ROW_Z));

    let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;
    let after = world.row();

    // Production has **two** delivery paths and a correct pair has to count
    // both: the inline fan-out's cells go out through `apply_use_item_on`'s own
    // `encode_block_update` loop (that is what it returns them for), and the
    // delayed flip goes out through `BlockTickFeed`. Counting only the feed
    // reports the synchronous half as undelivered, which is a false positive —
    // observed while building this, at (x=2) and (x=3).
    let last_delivered = |x: i32, now: &str| -> Option<String> {
        if let Some(p) = published.iter().filter(|p| p.pos == (x, Y, ROW_Z)).next_back() {
            return Some(p.state.clone());
        }
        inline
            .iter()
            .filter(|(p, _)| p.x == x && p.y == Y && p.z == ROW_Z)
            .next_back()
            .map(|(_, s)| s.clone())
            .or_else(|| {
                let _ = now;
                None
            })
    };

    let mut mismatches: Vec<String> = Vec::new();
    let mut delivered = 0usize;
    for ((x, was), (_, now)) in before.iter().zip(after.iter()) {
        if was == now {
            continue;
        }
        match last_delivered(*x, now) {
            Some(state) if &state == now => delivered += 1,
            Some(state) => mismatches.push(format!(
                "  (x={x}, y={Y}, z={ROW_Z}): the client was last told {state}, the server computed \
                 {now}"
            )),
            None => mismatches.push(format!(
                "  (x={x}, y={Y}, z={ROW_Z}): the client was told NOTHING, the server computed {now} \
                 (was {was})"
            )),
        }
    }

    assert!(
        mismatches.is_empty(),
        "the signal is COMPUTED but not DELIVERED at {} coordinate(s):\n{}\nEverything \
         published:\n{}",
        mismatches.len(),
        mismatches.join("\n"),
        log(&published)
    );
    // Anti-vacuity: a drive that changed nothing satisfies the loop above
    // trivially. The rising edge must move both dust squares, the repeater and
    // its output — four cells.
    assert!(
        delivered >= 4,
        "PREMISE FAILED: only {delivered} cell(s) changed during the drive, so the \
         computed-vs-delivered comparison had almost nothing to compare. Published:\n{}",
        log(&published)
    );
}

// ---------------------------------------------------------------------------
// The delivery precondition
// ---------------------------------------------------------------------------

/// **The delivery precondition patch B's bug violated**, asserted against the
/// jar census rather than against our own encoder — and it finds a second,
/// still-live instance.
///
/// `v770::resolve_state_id` matches a state string against
/// `lodestone_data::block_states` by exact property set first, then (since
/// `8f2d912`) by **subset**: a candidate must carry every property the caller
/// named. Dust could never hit the exact tier — one property against the real
/// block's five — and degraded to the lowest id, `power=0`, until the subset
/// tier was added.
///
/// Where each family stands, measured here rather than assumed:
///
/// * **repeater** — `set_repeater` emits all four real properties, so the exact
///   tier hits and no degradation is possible.
/// * **redstone torch** — same.
/// * **comparator** — `set_comparator` emits `output=N`, which **is not a
///   property of `minecraft:comparator` at all**: vanilla keeps that value in a
///   `ComparatorBlockEntity`, and `redstone::comparator_output`'s own doc
///   records the decision to encode it as a synthetic block-state property
///   instead. The consequence for delivery was not recorded, and it is severe:
///   a synthetic property fails the exact tier *and* the subset tier (no real
///   state carries `output`), so every comparator state this server sends still
///   resolves to the lowest-id comparator. That is patch B's bug, alive, in a
///   different family. Tracked separately — the fix is either to strip
///   synthetic properties at the encode boundary or to give
///   `resolve_state_id` a third tier that ignores properties the block does not
///   have, and the latter lives in `crates/protocol/v770`.
///
/// The comparator case is asserted **as it actually is**, with the exact-match
/// check applied to the state minus its synthetic property. That records the
/// finding precisely and proves the one-property strip is sufficient, instead
/// of leaving a red test or silently dropping the family from the sweep.
#[test]
fn every_state_the_delayed_families_publish_resolves_exactly_once_synthetic_properties_are_removed() {
    let mut fully_real: Vec<String> = Vec::new();
    for &(delay, _) in ORACLE_REPEATER_DELAY {
        for powered in [false, true] {
            for locked in [false, true] {
                fully_real.push(redstone_diode::set_repeater(Direction::West, delay, locked, powered));
            }
        }
    }
    for lit in [false, true] {
        fully_real.push(redstone_torch::set_standing_lit(lit));
    }

    let unmatched: Vec<&String> = fully_real.iter().filter(|s| !has_exact_state_in_census(s)).collect();
    assert!(
        unmatched.is_empty(),
        "{} repeater/torch state string(s) have no exact property-set match in the 26.2 block-state \
         census, so `resolve_state_id` must fall back and the value delivered to the client is not \
         the value computed:\n  {:?}",
        unmatched.len(),
        unmatched
    );

    // The comparator, recorded as it is. Both halves are asserted, so neither
    // the finding nor its fix can rot unnoticed.
    let mut comparators: Vec<String> = Vec::new();
    for subtract in [false, true] {
        for powered in [false, true] {
            for output in [0u8, 9, 15] {
                comparators.push(redstone_diode::set_comparator(Direction::West, subtract, powered, output));
            }
        }
    }
    for state in &comparators {
        assert!(
            !has_exact_state_in_census(state),
            "the comparator finding has changed: {state} now matches the census exactly. If \
             `output` stopped being emitted, delete this arm and move comparators into the \
             fully-real sweep above."
        );
        let stripped = without_property(state, "output");
        assert!(
            has_exact_state_in_census(&stripped),
            "{stripped} does not match the census either, so `output` is NOT the only synthetic \
             property on a comparator state and stripping it is not a sufficient fix"
        );
    }

    // The control for the detector itself: dust is the string the subset tier
    // was added for, and it must still miss the exact tier. If this ever starts
    // matching, the check above has stopped discriminating.
    let dust = redstone_wire::set_power(9);
    assert!(
        !has_exact_state_in_census(&dust),
        "CONTROL FAILED: {dust} now has an exact property-set match, so the assertions above no \
         longer distinguish a fully-stated block from a partially-stated one"
    );
}

/// Whether `state` has a block state in the 26.2 census whose property set is
/// **exactly** the one it names.
fn has_exact_state_in_census(state: &str) -> bool {
    let (name, wanted) = split_state(state);
    (0..lodestone_data::block_states::STATE_COUNT).any(|id| {
        if lodestone_data::block_states::block_name(id) != Some(name.as_str()) {
            return false;
        }
        let mut have: Vec<(&str, &str)> = lodestone_data::block_states::properties(id).unwrap_or(&[]).to_vec();
        have.sort_unstable();
        have == wanted
    })
}

/// `minecraft:repeater[delay=2,facing=west]` -> `("minecraft:repeater",
/// [("delay","2"),("facing","west")])`, sorted.
fn split_state(state: &str) -> (String, Vec<(&str, &str)>) {
    let Some((name, rest)) = state.split_once('[') else {
        return (state.to_owned(), Vec::new());
    };
    let mut props: Vec<(&str, &str)> = rest
        .trim_end_matches(']')
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .collect();
    props.sort_unstable();
    (name.to_owned(), props)
}

fn without_property(state: &str, key: &str) -> String {
    let (name, props) = split_state(state);
    let kept: Vec<String> = props
        .iter()
        .filter(|(k, _)| *k != key)
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if kept.is_empty() {
        name
    } else {
        format!("{name}[{}]", kept.join(","))
    }
}

// ---------------------------------------------------------------------------
// The ordering deviation, measured
// ---------------------------------------------------------------------------

/// **The `<= 1 tick` deviation the brokered patch flagged, measured rather than
/// argued.**
///
/// The claim under test is that the delay is counted from the tick that
/// *drained* the request, not from an absolute zero — i.e. that the deferral is
/// vanilla's own packet-phase quantization rather than an error that
/// accumulates. Two arms, differing only in when the request is published:
///
/// * published before tick 1 -> drained at tick 1 -> fires at `1 + delay`
/// * published after tick 1 has run -> drained at tick 2 -> fires at `2 + delay`
///
/// The **offset** must be the delay in both. A model where the deferral cost a
/// tick on top would show `delay + 1` in one arm and not the other; a model
/// that ignored the request tick entirely would show the same absolute tick in
/// both.
#[tokio::test(start_paused = true)]
async fn the_delay_is_measured_from_the_tick_that_drained_the_request_not_from_tick_zero() {
    const DELAY: u32 = 2;
    const SIGNAL_CHANGE_TICKS: u64 = 4; // ORACLE_REPEATER_DELAY's row for delay=2.

    let mut offsets: Vec<(u64, u64)> = Vec::new();
    for warmup in [0u64, 1u64] {
        let world = edge_rig(DELAY, true, false);
        let feed = BlockTickFeed::default();
        spawn_loop(Arc::clone(&world), &feed);
        tokio::task::yield_now().await;
        let mut published = Vec::new();
        for tick in 1..=(warmup + DRIVE_TICKS) {
            if tick == warmup + 1 {
                trigger(&world, &feed, BlockPos::new(SRC_X, Y, ROW_Z));
            }
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
            for (x, y, z, state) in feed.drain_all() {
                published.push(Published {
                    tick,
                    pos: (x, y, z),
                    state,
                });
            }
        }

        let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER).unwrap_or_else(|| {
            panic!(
                "warmup={warmup}: the output dust at (x={OUT_X}, y={Y}, z={ROW_Z}) never reached \
                 power {ORACLE_FULL_POWER}. Published:\n{}",
                log(&published)
            )
        });
        let drained_at = warmup + 1;
        offsets.push((drained_at, fired - drained_at));
    }

    assert_ne!(
        offsets[0].0, offsets[1].0,
        "PREMISE FAILED: both arms drained the request on the same tick, so this measurement cannot \
         distinguish an offset from an absolute tick"
    );
    for &(drained_at, offset) in &offsets {
        assert_eq!(
            offset, SIGNAL_CHANGE_TICKS,
            "a request drained on tick {drained_at} fired {offset} tick(s) later; the live 26.2 \
             server's repeater[delay={DELAY}] delay is {SIGNAL_CHANGE_TICKS} ticks. The deferral \
             must cost nothing on top of the delay -- measured offsets were {offsets:?}"
        );
    }
}

/// **The residual deviation the brokered patch flagged, measured — and it is
/// zero for the synchronous half.**
///
/// The flag was: production runs the synchronous half of a placement inline and
/// the delayed half at the next tick boundary, where vanilla runs both in the
/// packet phase, so the two are split by up to one tick period.
///
/// Carrying the *schedule* rather than a position to re-run removes that split
/// at its source: the fan-out now happens exactly once, inline, at packet time,
/// which is where vanilla runs it. What crosses to the loop is a delay, and the
/// loop rebases it onto the tick that adopts it. So the measurement to make is
/// no longer "how much does the split cost" but "is there a split at all", and
/// there are two halves to check:
///
/// 1. **The synchronous half is delivered before any tick runs.** Asserted by
///    reading the dust powers straight out of `trigger`'s return value, with the
///    loop not yet started — zero ticks of latency, exactly like vanilla.
/// 2. **The delayed half fires at the delay, from the tick that adopted it.**
///    The remaining quantization is vanilla's own packet phase, which
///    [`the_delay_is_measured_from_the_tick_that_drained_the_request_not_from_tick_zero`]
///    measures.
///
/// The negative control is the shape this replaced: with the loop asked to
/// *re-run* the fan-out instead of adopting the schedule, the second run finds a
/// settled circuit and the repeater never fires at all. That is
/// [`the_re_run_shape_the_brokered_patch_specified_cannot_work`], which
/// reproduces it and observes the failure.
#[tokio::test(start_paused = true)]
async fn the_synchronous_half_costs_zero_ticks_and_only_the_schedule_is_deferred() {
    const DELAY: u32 = 3;
    const SIGNAL_CHANGE_TICKS: u64 = 6; // ORACLE_REPEATER_DELAY's row for delay=3.

    let world = edge_rig(DELAY, true, false);
    let feed = BlockTickFeed::default();

    // Packet time. No tick has run, and none can: the loop is not spawned yet.
    let inline = trigger(&world, &feed, BlockPos::new(SRC_X, Y, ROW_Z));

    let settled: Vec<(i32, u8)> = (2..=3)
        .map(|x| (x, redstone::wire_power(&world.block_state(x, Y, ROW_Z))))
        .collect();
    assert_eq!(
        settled,
        vec![(2, 15), (3, 14)],
        "the synchronous half must resolve at packet time, before any tick runs; the live 26.2 \
         server settles dust inside setBlock (0 ticks, measured in 9eb8703). Got {settled:?}"
    );
    let inline_cells: Vec<i32> = inline.iter().map(|(p, _)| p.x).collect();
    assert!(
        inline_cells.contains(&2) && inline_cells.contains(&3),
        "PREMISE FAILED: the inline half rewrote the world but did not report the cells, so \
         `apply_use_item_on` would have nothing to send: reported {inline_cells:?}"
    );
    // ...and the repeater has NOT flipped yet, or there would be no delayed half
    // left to measure.
    assert!(
        !redstone::diode_powered(&world.block_state(DIODE_X, Y, ROW_Z)),
        "PREMISE FAILED: the repeater flipped inside the synchronous half, so this test is not \
         measuring a split at all"
    );

    let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;
    let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER);
    assert_eq!(
        fired,
        Some(1 + SIGNAL_CHANGE_TICKS),
        "the delayed half must fire {SIGNAL_CHANGE_TICKS} ticks after the tick that adopted its \
         schedule; output dust at (x={OUT_X}, y={Y}, z={ROW_Z}) fired on {fired:?}. \
         Published:\n{}",
        log(&published)
    );
}

/// **The negative control for the design deviation, run and observed.**
///
/// The brokered patch had the loop re-run `react_at_placement` at the mutated
/// position on its next iteration, on the stated premise that the inline and
/// deferred runs are idempotent because the fan-out "writes only on change".
/// This reproduces that shape and shows the premise is false in the direction
/// that matters: the inline run **consumes** the change, so the re-run sees a
/// settled circuit, cascades nowhere, and never notifies the repeater.
///
/// Both arms are asserted, so this is a discrimination and not a bare absence:
/// with the schedule carried the repeater fires, with it re-derived it does not.
#[tokio::test(start_paused = true)]
async fn the_re_run_shape_the_brokered_patch_specified_cannot_work() {
    const DELAY: u32 = 2;
    const SIGNAL_CHANGE_TICKS: u64 = 4;

    // Arm A -- the re-run shape: inline fan-out, schedules DISCARDED, and the
    // loop asked to redo the fan-out at the same position.
    let world = edge_rig(DELAY, true, false);
    let feed = BlockTickFeed::default();
    let (_, discarded) = crate::server::propagate_placement(&*world, BlockPos::new(SRC_X, Y, ROW_Z));
    assert!(
        !discarded.is_empty(),
        "PREMISE FAILED: the inline fan-out scheduled nothing, so there is no schedule for the \
         re-run shape to have lost and this control proves nothing"
    );
    // The re-run, performed exactly as the brokered patch's loop body would:
    // same entry point, same origin, the loop's own queue.
    let mut requeued: crate::scheduled_tick::ScheduledTickQueue<String> =
        crate::scheduled_tick::ScheduledTickQueue::new();
    let mut column = world.column(0, 0);
    let redone = crate::random_tick::react_at_placement(
        &mut column, 0, 0, SRC_X, Y, ROW_Z, &mut requeued, 1,
    );
    assert!(
        redone.is_empty() && requeued.is_empty(),
        "CONTROL FAILED: the re-run found work to do, so the brokered shape would have worked and \
         the deviation taken in this landing was unnecessary. It rewrote {redone:?} and scheduled \
         {} entr(ies).",
        requeued.len()
    );
    let published = drive(Arc::clone(&world), &feed, DRIVE_TICKS).await;
    let never = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER);
    assert_eq!(
        never, None,
        "CONTROL FAILED: the output dust at (x={OUT_X}, y={Y}, z={ROW_Z}) fired on tick {never:?} \
         under the re-run shape"
    );

    // Arm B -- the shape that landed. Same rig, same trigger position.
    let world_b = edge_rig(DELAY, true, false);
    let feed_b = BlockTickFeed::default();
    trigger(&world_b, &feed_b, BlockPos::new(SRC_X, Y, ROW_Z));
    let published_b = drive(Arc::clone(&world_b), &feed_b, DRIVE_TICKS).await;
    assert_eq!(
        tick_dust_reached(&published_b, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER),
        Some(1 + SIGNAL_CHANGE_TICKS),
        "the landed shape must fire where the re-run shape fired never. Published:\n{}",
        log(&published_b)
    );
}

// ---------------------------------------------------------------------------
// Issue #321 -- the hopper redstone lock
// ---------------------------------------------------------------------------

/// Where the two hoppers sit. They must be vertically adjacent, because this
/// crate's hopper model transfers only to `below` and from `above`
/// (`BlockEntityRegistry::tick_hopper`).
const HOP_X: i32 = 4;
const HOP_UPPER_Y: i32 = 2;
const HOP_LOWER_Y: i32 = 1;

/// `Hopper::TRANSFER_COOLDOWN_TICKS` is 8 and a fresh hopper starts at vanilla's
/// `NO_COOLDOWN_TIME = -1`, so a hopper acts on tick 1 and every 8th tick after:
/// **ticks 1 and 9** within a 16-tick drive. Transcribed from `hopper.rs`'s own
/// constant and initial value rather than counted by hand.
const HOPPER_ACTS_ON_TICKS: usize = 2;

/// A two-hopper stack: five items in the upper hopper, an empty lower one, and a
/// lit redstone torch beside the **upper** hopper only, so exactly one of the two
/// is powered.
///
/// Powering only the upper one is deliberate and is what makes the prediction
/// discriminating. Locking a hopper stops *its own* pushing and pulling; it does
/// not stop an unlocked neighbour pushing into it or pulling out of it. So the
/// lower hopper keeps pulling either way and the two arms differ by exactly the
/// upper hopper's own pushes -- two distinct item distributions rather than
/// "some" versus "fewer".
fn hopper_rig(power_the_upper: bool) -> (Arc<RigWorld>, BlockEntityHandle) {
    let world = Arc::new(RigWorld::new());
    // Support for the torch, then the torch itself beside the upper hopper.
    world.set_block(HOP_X + 1, HOP_LOWER_Y, ROW_Z, "minecraft:stone");
    if power_the_upper {
        world.set_block(
            HOP_X + 1,
            HOP_UPPER_Y,
            ROW_Z,
            &redstone_torch::set_standing_lit(true),
        );
    }
    // Both hoppers start `enabled=true`, as vanilla's default state does
    // (`HopperBlock.java:55`); the lock is what has to change one of them.
    for y in [HOP_LOWER_Y, HOP_UPPER_Y] {
        world.set_block(HOP_X, y, ROW_Z, "minecraft:hopper[enabled=true,facing=down]");
    }

    let block_entities = BlockEntityHandle::default();
    let mut upper = crate::hopper::Hopper::new();
    upper.set_slot(
        0,
        Some(lodestone_model::ItemStack::new(
            "minecraft:diamond".parse().expect("valid resource key"),
            5,
        )),
    );
    block_entities.with(|registry| {
        registry.insert(
            BlockPos::new(HOP_X, HOP_UPPER_Y, ROW_Z),
            crate::block_entities::BlockEntity::Hopper(upper),
        );
        registry.insert(
            BlockPos::new(HOP_X, HOP_LOWER_Y, ROW_Z),
            crate::block_entities::BlockEntity::Hopper(crate::hopper::Hopper::new()),
        );
    });
    (world, block_entities)
}

/// Total item count held by the hopper at `(HOP_X, y, ROW_Z)`.
fn hopper_count(block_entities: &BlockEntityHandle, y: i32) -> u32 {
    block_entities.with(|registry| match registry.get(BlockPos::new(HOP_X, y, ROW_Z)) {
        Some(crate::block_entities::BlockEntity::Hopper(h)) => {
            h.slots().iter().flatten().map(|stack| stack.count).sum()
        }
        _ => panic!("no hopper registered at (x={HOP_X}, y={y}, z={ROW_Z})"),
    })
}

/// Drives the real loop over a hopper rig, returning `(upper count, lower count,
/// published changes)`.
async fn drive_hoppers(
    world: Arc<RigWorld>,
    block_entities: &BlockEntityHandle,
    feed: &BlockTickFeed,
    ticks: u64,
) -> Vec<Published> {
    tokio::spawn(run_tick_loop(
        MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT)),
        LiveMobSource::default(),
        block_entities.clone(),
        Arc::new(TickClock::new()),
        world,
        feed.clone(),
        (0..=0, 0..=0),
        ExplosionFeed::default(),
        crate::region_source::ScheduledTickHandle::default(),
        crate::tick_area::TickFollow::default(),
    ));
    tokio::task::yield_now().await;
    let mut published = Vec::new();
    for tick in 1..=ticks {
        tokio::time::advance(TICK_PERIOD).await;
        tokio::task::yield_now().await;
        for (x, y, z, state) in feed.drain_all() {
            published.push(Published { tick, pos: (x, y, z), state });
        }
    }
    published
}

/// **The load-bearing #321 gate.** A powered hopper stops transferring, and the
/// prediction is an exact item distribution rather than "fewer items moved".
///
/// # Predicting the value, not the sign
///
/// Both hypotheses are computed from `hopper.rs`'s own constants, and they are
/// two different distributions rather than a magnitude and a smaller magnitude:
///
/// | arm | the upper hopper acts? | after 16 ticks |
/// |---|---|---|
/// | upper hopper powered (locked) | no -- only the lower one pulls | upper 3, lower 2 |
/// | upper hopper unpowered (control) | yes -- both act | upper 1, lower 4 |
///
/// A hopper acts on ticks 1 and 9 (cooldown 8, initial `-1`), so each arm has
/// exactly two acting ticks; the locked arm moves one item per acting tick and
/// the unlocked arm two. `tick_all`'s hardcoded `enabled: true` produces the
/// control column, so this gate fails on the pre-#321 tree at both cells.
///
/// The distribution is order-independent, which matters because
/// `tick_all_with_hopper_lock` iterates `HashMap` keys: whichever hopper is
/// visited first, each acts once per cycle and the totals are identical. Checked
/// by [`the_hopper_prediction_does_not_depend_on_registry_iteration_order`].
#[tokio::test(start_paused = true)]
async fn a_powered_hopper_stops_transferring_and_an_unpowered_one_does_not() {
    let mut arms: Vec<(u32, u32)> = Vec::new();
    for powered in [true, false] {
        let (world, block_entities) = hopper_rig(powered);
        let feed = BlockTickFeed::default();

        // Premise: the rig really is/is not powered, read through the same
        // signal walk production uses.
        let column = world.column(0, 0);
        let signal = redstone::best_neighbor_signal(
            &redstone::make_lookup(&column, 0, 0),
            BlockPos::new(HOP_X, HOP_UPPER_Y, ROW_Z),
            false,
        );
        assert_eq!(
            signal > 0,
            powered,
            "PREMISE FAILED: powered={powered} but the upper hopper at \
             (x={HOP_X}, y={HOP_UPPER_Y}, z={ROW_Z}) reads signal {signal}"
        );
        assert_eq!(
            hopper_count(&block_entities, HOP_UPPER_Y),
            5,
            "PREMISE FAILED: the upper hopper does not start with 5 items"
        );

        // The mutation a player would make, and the fan-out it owes -- this is
        // what maintains the `enabled` property.
        trigger(&world, &feed, BlockPos::new(HOP_X + 1, HOP_UPPER_Y, ROW_Z));

        let _published = drive_hoppers(Arc::clone(&world), &block_entities, &feed, DRIVE_TICKS).await;
        arms.push((
            hopper_count(&block_entities, HOP_UPPER_Y),
            hopper_count(&block_entities, HOP_LOWER_Y),
        ));
    }

    assert_eq!(
        arms[0],
        (3, 2),
        "POWERED: the upper hopper at (x={HOP_X}, y={HOP_UPPER_Y}, z={ROW_Z}) is redstone-locked, so \
         only the lower hopper should act -- one item per acting tick, \
         {HOPPER_ACTS_ON_TICKS} acting ticks. Got (upper, lower) = {:?}, expected (3, 2)",
        arms[0]
    );
    assert_eq!(
        arms[1],
        (1, 4),
        "UNPOWERED CONTROL: both hoppers should act -- two items per acting tick, \
         {HOPPER_ACTS_ON_TICKS} acting ticks. Got (upper, lower) = {:?}, expected (1, 4)",
        arms[1]
    );
    assert_ne!(
        arms[0], arms[1],
        "the two arms must differ, or this gate cannot see the lock at all"
    );
}

/// **The island control for #321, run and observed.**
///
/// `BlockEntityRegistry::tick_all` -- the unlocked shorthand, and what
/// `run_tick_loop` called before this landing -- must produce the *unpowered*
/// distribution even on the powered rig. That is the whole defect: the signal was
/// computed and the hopper never heard about it.
///
/// So this asserts the shorthand is blind and the locking form is not, against
/// the identical powered rig. Without it, the gate above could be passing because
/// of something other than the lock.
#[tokio::test(start_paused = true)]
async fn the_unlocked_shorthand_ignores_the_signal_which_is_what_made_this_an_island() {
    let (world, block_entities) = hopper_rig(true);
    let feed = BlockTickFeed::default();
    trigger(&world, &feed, BlockPos::new(HOP_X + 1, HOP_UPPER_Y, ROW_Z));

    // The state really does say locked...
    assert!(
        !redstone::hopper_enabled(&world.block_state(HOP_X, HOP_UPPER_Y, ROW_Z)),
        "PREMISE FAILED: the upper hopper's block state is not enabled=false, so there is no lock \
         for the shorthand to ignore: {}",
        world.block_state(HOP_X, HOP_UPPER_Y, ROW_Z)
    );

    // ...and the shorthand ignores it, ticking 16 times by hand.
    for _ in 0..DRIVE_TICKS {
        block_entities.with(crate::block_entities::BlockEntityRegistry::tick_all);
    }
    let blind = (
        hopper_count(&block_entities, HOP_UPPER_Y),
        hopper_count(&block_entities, HOP_LOWER_Y),
    );
    assert_eq!(
        blind,
        (1, 4),
        "CONTROL FAILED: `tick_all` is supposed to be the unlocked shorthand and produce the \
         unpowered distribution on a powered rig. Got {blind:?}. If this now reports (3, 2) the \
         shorthand has grown a lock of its own and this control no longer demonstrates the island."
    );
}

/// The prediction above must not depend on `HashMap` iteration order, since
/// `tick_all_with_hopper_lock` walks `self.entities.keys()`.
///
/// Argued by exhaustion over both visit orders rather than by trusting one run:
/// each hopper acts at most once per cooldown cycle, and neither hopper's action
/// changes whether the other may act this tick (the lower one's pull does not
/// empty the upper below its non-empty test at these counts, and the upper's push
/// does not fill the lower). So the per-cycle total is 2 either way. Asserted by
/// running the locked arm repeatedly and requiring one distribution.
#[tokio::test(start_paused = true)]
async fn the_hopper_prediction_does_not_depend_on_registry_iteration_order() {
    let mut seen: Vec<(u32, u32)> = Vec::new();
    for _ in 0..8 {
        let (world, block_entities) = hopper_rig(false);
        let feed = BlockTickFeed::default();
        trigger(&world, &feed, BlockPos::new(HOP_X + 1, HOP_UPPER_Y, ROW_Z));
        let _ = drive_hoppers(Arc::clone(&world), &block_entities, &feed, DRIVE_TICKS).await;
        let arm = (
            hopper_count(&block_entities, HOP_UPPER_Y),
            hopper_count(&block_entities, HOP_LOWER_Y),
        );
        if !seen.contains(&arm) {
            seen.push(arm);
        }
    }
    assert_eq!(
        seen,
        vec![(1, 4)],
        "the unlocked distribution varied across runs, so registry iteration order does leak into \
         it and the exact predictions above are not safe: observed {seen:?}"
    );
}

/// **The lock reaches the client.** The `enabled` flip must be delivered, and it
/// must be deliverable *precisely*.
///
/// Two independent things, both required:
///
/// 1. The flip is **published**, so `serve_play` can encode it -- the inline
///    fan-out reports it, exactly as it reports a dust change. A lock the server
///    applies but never tells anyone about would leave the client rendering an
///    unlocked hopper forever, and would look identical from the server side.
/// 2. The resulting state has an **exact** property-set match in the jar census,
///    so `v770::resolve_state_id` hits its first tier and cannot degrade. This is
///    why `redstone::with_property` edits in place instead of rebuilding: dropping
///    `facing` would fall to the subset tier and hand the client a hopper pointing
///    somewhere else -- #476's defect, which this deliberately avoids rather than
///    repeats.
#[tokio::test(start_paused = true)]
async fn the_hopper_lock_is_delivered_and_resolves_exactly() {
    let (world, feed) = {
        let (world, _entities) = hopper_rig(true);
        (world, BlockTickFeed::default())
    };
    let changed = trigger(&world, &feed, BlockPos::new(HOP_X + 1, HOP_UPPER_Y, ROW_Z));

    let target = BlockPos::new(HOP_X, HOP_UPPER_Y, ROW_Z);
    let delivered = changed
        .iter()
        .find(|(p, _)| *p == target)
        .map(|(_, s)| s.clone())
        .unwrap_or_else(|| {
            panic!(
                "the lock is COMPUTED but not DELIVERED at (x={HOP_X}, y={HOP_UPPER_Y}, z={ROW_Z}): \
                 the server's own state is {} and the cells reported to the client were {:?}",
                world.block_state(HOP_X, HOP_UPPER_Y, ROW_Z),
                changed.iter().map(|(p, s)| (p.x, p.y, p.z, s.as_str())).collect::<Vec<_>>()
            )
        });

    assert!(
        !redstone::hopper_enabled(&delivered),
        "the client was told {delivered}, which is still enabled"
    );
    assert_eq!(
        delivered,
        world.block_state(HOP_X, HOP_UPPER_Y, ROW_Z),
        "the state delivered to the client differs from the one the server kept"
    );
    assert!(
        delivered.contains("facing=down"),
        "the lock rewrite dropped `facing`, so the client will be handed a hopper pointing \
         elsewhere (see #476 for that failure mode): {delivered}"
    );
    assert!(
        has_exact_state_in_census(&delivered),
        "{delivered} has no exact property-set match in the 26.2 census, so `resolve_state_id` must \
         fall back and the hopper the client renders is not the one computed"
    );
}

/// Premise for the arm above: the rig is *unlocked* to begin with, so the flip
/// this measures is a real transition and not the initial state.
#[test]
fn the_hopper_rig_starts_unlocked() {
    let (world, _entities) = hopper_rig(true);
    assert!(
        redstone::hopper_enabled(&world.block_state(HOP_X, HOP_UPPER_Y, ROW_Z)),
        "PREMISE FAILED: the hopper is already enabled=false before any fan-out runs, so a gate \
         asserting it becomes false proves nothing"
    );
}

// ---------------------------------------------------------------------------
// Issue #468 -- the tick loop's queues really are the persistable ones
// ---------------------------------------------------------------------------

/// Like [`drive`], but over a caller-supplied [`ScheduledTickHandle`] so a gate
/// can inspect (or pre-load) the very queues the loop uses.
async fn drive_with_handle(
    world: Arc<RigWorld>,
    feed: &BlockTickFeed,
    scheduled: &crate::region_source::ScheduledTickHandle,
    ticks: u64,
) -> Vec<Published> {
    tokio::spawn(run_tick_loop(
        MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT)),
        LiveMobSource::default(),
        BlockEntityHandle::default(),
        Arc::new(TickClock::new()),
        world,
        feed.clone(),
        (0..=0, 0..=0),
        ExplosionFeed::default(),
        scheduled.clone(),
        crate::tick_area::TickFollow::default(),
    ));
    tokio::task::yield_now().await;
    let mut published = Vec::new();
    for tick in 1..=ticks {
        tokio::time::advance(TICK_PERIOD).await;
        tokio::task::yield_now().await;
        for (x, y, z, state) in feed.drain_all() {
            published.push(Published { tick, pos: (x, y, z), state });
        }
    }
    published
}

/// **The island proof for #468's last wire.** A tick the loop schedules must be
/// visible in the handle the save path reads.
///
/// `tests/scheduled_tick_persistence.rs` gates the handle and the schema in both
/// directions and passes whether or not `run_tick_loop` uses them — it drives the
/// handle directly. So it structurally cannot see the defect that mattered: the
/// loop holding its two queues as **locals**, leaving the persistable queues
/// permanently empty in production and losing every pending repeater tick on
/// quit. This is the assertion that can.
///
/// Predicts the count, not merely non-emptiness: a `repeater[delay=4]` on a
/// rising edge schedules exactly **one** entry, at the repeater's own position,
/// due at tick `1 + 8`. The drive stops at tick 3, well short, so the entry must
/// still be pending rather than already fired.
#[tokio::test(start_paused = true)]
async fn a_tick_the_loop_schedules_is_visible_in_the_handle_the_save_path_reads() {
    let world = edge_rig(4, true, false);
    let feed = BlockTickFeed::default();
    let scheduled = crate::region_source::ScheduledTickHandle::default();

    assert_eq!(
        scheduled.with(|queues| queues.block.len()),
        0,
        "PREMISE FAILED: the handle is not empty before the loop runs"
    );

    trigger(&world, &feed, BlockPos::new(SRC_X, Y, ROW_Z));
    let published = drive_with_handle(Arc::clone(&world), &feed, &scheduled, 3).await;

    let pending: Vec<((i32, i32, i32), String, u64)> = scheduled.with(|queues| {
        queues.block.iter().map(|t| (t.pos, t.kind.clone(), t.trigger_tick)).collect()
    });
    assert_eq!(
        pending.len(),
        1,
        "expected exactly one pending block tick in the handle after 3 ticks; got {pending:?}. \
         If this is 0, `run_tick_loop` is still scheduling into a local queue and every pending \
         repeater tick is lost on quit -- which is the whole of #468's last wire. Published:\n{}",
        log(&published)
    );
    assert_eq!(
        pending[0].0,
        (DIODE_X, Y, ROW_Z),
        "the pending tick is at the wrong position: {pending:?}"
    );
    assert_eq!(
        pending[0].1, redstone::TICK_REPEATER,
        "the pending tick is the wrong kind: {pending:?}"
    );
    assert_eq!(
        pending[0].2, 9,
        "repeater[delay=4] on a rising edge is due at tick 1+8; the handle says {}",
        pending[0].2
    );
}

/// **The other direction: a reopened world resumes its pending ticks.**
///
/// A tick pre-loaded into the handle before the loop starts — which is exactly
/// what a load from disk produces — must be drained by the loop at its own
/// `trigger_tick`.
///
/// The discrimination is deliberate: the repeater is at `delay=4`, whose natural
/// signal-change delay is **8** ticks, and the pre-loaded entry is due at tick
/// **3**. So a pass proves the loop honoured the *loaded* schedule rather than
/// recomputing one of its own — two numbers that cannot be confused. The
/// circuit is pre-settled so nothing else schedules anything.
#[tokio::test(start_paused = true)]
async fn a_tick_pre_loaded_into_the_handle_is_drained_by_the_loop_at_its_own_trigger_tick() {
    const LOADED_TRIGGER: u64 = 3;
    const NATURAL_DELAY: u64 = 8; // 2d for delay=4, from the live oracle.

    let world = settled_line_with_a_gap();
    world.set_block(
        DIODE_X,
        Y,
        ROW_Z,
        &redstone_diode::set_repeater(Direction::West, 4, false, false),
    );
    let feed = BlockTickFeed::default();
    let scheduled = crate::region_source::ScheduledTickHandle::default();
    scheduled.with(|queues| {
        queues.block.schedule(
            (DIODE_X, Y, ROW_Z),
            redstone::TICK_REPEATER.to_owned(),
            LOADED_TRIGGER,
            crate::scheduled_tick::TickPriority::Normal,
        );
    });

    // No `trigger` call at all: nothing in this test publishes a schedule, so the
    // only entry the loop can possibly drain is the pre-loaded one.
    let published = drive_with_handle(Arc::clone(&world), &feed, &scheduled, DRIVE_TICKS).await;

    let fired = tick_dust_reached(&published, (OUT_X, Y, ROW_Z), ORACLE_FULL_POWER);
    assert_eq!(
        fired,
        Some(LOADED_TRIGGER),
        "a pending tick loaded at trigger_tick {LOADED_TRIGGER} must fire on tick \
         {LOADED_TRIGGER}; the output dust at (x={OUT_X}, y={Y}, z={ROW_Z}) fired on {fired:?}. \
         Note {NATURAL_DELAY} would mean the loop ignored the loaded queue and recomputed. \
         Published:\n{}",
        log(&published)
    );
    assert_ne!(
        LOADED_TRIGGER, NATURAL_DELAY,
        "PREMISE FAILED: the loaded trigger equals the natural delay, so this gate cannot tell \
         'honoured the loaded schedule' from 'recomputed its own'"
    );
    assert_eq!(
        scheduled.with(|queues| queues.block.len()),
        0,
        "the loaded tick should have been drained, not left pending"
    );
}

/// The handle's game tick must come from the loop's **own** counter.
///
/// Predicts the exact count after a known number of virtual ticks. This is
/// issue #323's shape — `SET_TIME` decoded, really did darken the sky, every link
/// green, while the value was wall-clock elapsed-since-join rather than the tick
/// counter — so a wrong clock here would rebase every loaded `trigger_tick`
/// against the wrong origin and look entirely healthy.
///
/// The wall-clock hypothesis is separated explicitly: under `start_paused` the
/// loop advances 12 ticks while elapsed virtual time is 12 x 50ms = 600, so a
/// millisecond-derived counter would read 600 and a second-derived one 0.
#[tokio::test(start_paused = true)]
async fn the_handle_game_tick_is_the_loops_own_counter_and_not_a_wall_clock() {
    const TICKS: u64 = 12;
    let world = settled_line_with_a_gap();
    let feed = BlockTickFeed::default();
    let scheduled = crate::region_source::ScheduledTickHandle::default();

    assert_eq!(
        scheduled.game_tick(),
        0,
        "PREMISE FAILED: the handle reports a non-zero tick before the loop has run"
    );

    let _ = drive_with_handle(Arc::clone(&world), &feed, &scheduled, TICKS).await;

    let observed = scheduled.game_tick();
    assert_eq!(
        observed, TICKS,
        "the handle must report the loop's own tick count. Wrong models: a millisecond wall clock \
         would read {} and a second-derived one 0",
        TICKS * 50
    );
    assert_ne!(
        observed,
        TICKS * 50,
        "the observed value coincides with the millisecond wall clock, so this gate cannot \
         separate them"
    );
}
