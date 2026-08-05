//! Issues #315 (repeaters/comparators) and #317 (observers), gated against a
//! **live vanilla 26.2 server** rather than against our own model.
//!
//! # The oracle
//!
//! Every expected number below was measured over RCON on the flat creative
//! oracle (`scripts/live-oracles/creative.sh`, game `:25570`, RCON `:25571`)
//! under `/tick freeze`, stepping one tick at a time and probing after each
//! step. Nothing here is derived from this crate's own formulas — a
//! self-authored expectation would be satisfied by two symmetric
//! misunderstandings, which is exactly the failure mode repeater timing has.
//!
//! ## How a tick-exact reading was taken
//!
//! `/tick freeze`, then `/tick step 1` repeatedly, confirming each step landed
//! by reading `time query gametime` (which advances by exactly one per step).
//! A block's `powered`/`locked` flag and a dust square's `power` are block
//! *states*, unreachable through `/data get block`, so they were read exactly
//! by probing `execute if block <pos> <block>[<property>=<value>]`.
//!
//! ## Three oracle traps that cost real time, all failing "safe"
//!
//! * **`pause-when-empty-seconds` defaults to `60`.** With no player
//!   connected the dedicated server pauses the entire world after a minute:
//!   `gameTime` stops dead, and since `ServerLevel.tick` runs
//!   `this.blockTicks.tick(this.getGameTime(), ...)`, **no scheduled block
//!   tick ever fires again**. Redstone then looks simply dead — dust still
//!   propagates (it is synchronous, inside the `setBlock` itself) while every
//!   repeater, comparator, observer and torch sits inert forever. The oracle
//!   world needs `pause-when-empty-seconds=0`. A falling-sand block is the
//!   cheapest control: if sand placed in the air does not land, nothing is
//!   ticking and every timing reading below is vacuous.
//! * **`/tick step N` *does* advance scheduled block ticks.** An earlier note
//!   in this repo said it did not. `TickRateManager.tick` sets
//!   `runGameElements = !isFrozen || frozenTicksToRun > 0`, and
//!   `ServerLevel.tick` gates `blockTicks.tick(...)` on exactly that value
//!   (`ServerLevel.java:358,386-389`), so a stepped tick runs them normally.
//!   The original observation was the paused-world symptom above.
//! * **`time query gametime` is only a tick counter while the world runs.**
//!   Under the pause it is a constant, which reads as "stepping does nothing".
//!
//! # What is gated here versus what is asserted as a value table
//!
//! The **scheduling decisions** (which position, which kind, which exact
//! trigger tick) are asserted on the return of
//! [`crate::random_tick::propagate_and_react`] — the production entry point
//! `crate::tick::run_tick_loop` itself calls. The **state transitions** those
//! scheduled ticks then perform are driven through the same per-family
//! functions `run_tick_loop`'s drain calls, in the same order; that drain
//! mirror is a re-implementation and is named as one rather than presented as
//! production coverage.

use crate::chunk::ChunkColumn;
use crate::neighbor_update::Direction;
use crate::random_tick::propagate_and_react;
use crate::scheduled_tick::ScheduledTickQueue;
use crate::{redstone, redstone_diode, redstone_observer, redstone_torch, redstone_wire};
use lodestone_model::BlockPos;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const FLOOR_Y: i32 = 0;
const Y: i32 = 1;
const ROW_Z: i32 = 8;
const NOW: u64 = 1_000;

// ---------------------------------------------------------------------------
// The oracle tables
// ---------------------------------------------------------------------------

/// Live 26.2: `(delay property, game ticks until the output changes)`.
///
/// Measured on both the **rising** and the **falling** edge, and the two agreed
/// exactly at every setting — the rising column is not assumed from the falling
/// one. Rig: `redstone_block` toggled at one end of a dust run feeding a
/// `repeater[facing=west]`, output read on the dust square past it.
///
/// ```text
///   delay=1  rising ON at tick 2   falling OFF at tick 2
///   delay=2  rising ON at tick 4   falling OFF at tick 4
///   delay=3  rising ON at tick 6   falling OFF at tick 6
///   delay=4  rising ON at tick 8   falling OFF at tick 8
/// ```
const ORACLE_REPEATER_DELAY: &[(u32, u64)] = &[(1, 2), (2, 4), (3, 6), (4, 8)];

/// Live 26.2: a comparator's delay is **2 game ticks**, on both edges and in
/// both modes (measured separately for `compare` and `subtract`; all four
/// readings were 2).
const ORACLE_COMPARATOR_DELAY: u64 = 2;

/// Live 26.2: an observer's back face is powered on ticks **2 and 3** after
/// the watched block changes, and unpowered from tick 4 on. So: the pulse
/// *starts* at tick 2 and is exactly **2 game ticks** wide.
///
/// Measured identically for three different triggers (block placed, block
/// removed, and a pure block-state change with no block swap).
const ORACLE_OBSERVER_PULSE_START: u64 = 2;
const ORACLE_OBSERVER_PULSE_WIDTH: usize = 2;

/// Live 26.2 comparator table: `(subtract, input, side, output, powered)`.
///
/// `input` and `side` were **read back** off the rig (by probing the dust
/// squares feeding the comparator), never assumed from the chain lengths;
/// `output` is the power of the dust square the comparator drives, and
/// `powered` its own block-state flag. 30 rows, 15 per mode.
#[rustfmt::skip]
const ORACLE_COMPARATOR_TABLE: &[(bool, u8, u8, u8, bool)] = &[
    // compare mode
    (false, 15,  0, 15, true ), (false, 15, 15, 15, true ), (false, 15, 12, 15, true ),
    (false, 15,  9, 15, true ), (false, 15,  3, 15, true ),
    (false, 12,  0, 12, true ), (false, 12, 15,  0, false), (false, 12, 12, 12, true ),
    (false, 12,  9, 12, true ), (false, 12,  3, 12, true ),
    (false,  9,  0,  9, true ), (false,  9, 15,  0, false), (false,  9, 12,  0, false),
    (false,  9,  9,  9, true ), (false,  9,  3,  9, true ),
    // subtract mode
    (true , 15,  0, 15, true ), (true , 15, 15,  0, false), (true , 15, 12,  3, true ),
    (true , 15,  9,  6, true ), (true , 15,  3, 12, true ),
    (true , 12,  0, 12, true ), (true , 12, 15,  0, false), (true , 12, 12,  0, false),
    (true , 12,  9,  3, true ), (true , 12,  3,  9, true ),
    (true ,  9,  0,  9, true ), (true ,  9, 15,  0, false), (true ,  9, 12,  0, false),
    (true ,  9,  9,  0, false), (true ,  9,  3,  6, true ),
];

// ---------------------------------------------------------------------------
// Rig helpers
// ---------------------------------------------------------------------------

fn column_with_floor() -> ChunkColumn {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            column.set_block(x, FLOOR_Y, z, "minecraft:stone");
        }
    }
    column
}

fn at(column: &ChunkColumn, x: i32, y: i32, z: i32) -> String {
    column.block_state(x, y, z).to_string()
}

/// Every entry `propagate_and_react` scheduled, as
/// `(pos, kind, trigger_tick)`. Drains far past `NOW` so nothing is missed.
fn scheduled(block_ticks: &mut ScheduledTickQueue<String>) -> Vec<((i32, i32, i32), String, u64)> {
    block_ticks
        .drain_due(NOW + 4_096, 4_096)
        .into_iter()
        .map(|t| (t.pos, t.kind, t.trigger_tick))
        .collect()
}

fn find(entries: &[((i32, i32, i32), String, u64)], pos: (i32, i32, i32), kind: &str) -> Option<u64> {
    entries.iter().find(|(p, k, _)| *p == pos && k == kind).map(|(_, _, t)| *t)
}

// ---------------------------------------------------------------------------
// #315 — repeaters
// ---------------------------------------------------------------------------

/// A repeater rig on one row: lit/unlit standing torch source at `x = 1`, dust
/// at `x = 2..=3`, the repeater at `x = 4` facing **west** (so its input is the
/// dust at `x = 3`, matching the live rig), output dust at `x = 5`.
///
/// Orientation is itself an oracle reading: on the live server a
/// `repeater[facing=west]` fed from the **west** powered its output to the
/// east, while the same rig with `facing=east` never powered at all. Our
/// `redstone::input_signal` reads `facing.relative(pos)`, which is that.
fn repeater_rig(delay: u32, powered: bool, source_lit: bool) -> ChunkColumn {
    let mut column = column_with_floor();
    column.set_block(1, Y, ROW_Z, &redstone_torch::set_standing_lit(source_lit));
    // The dust holds its PRE-flip powers: the torch has just changed and the
    // propagation under test is what re-derives them. Seeding the dust with
    // its settled value instead makes the rig vacuous — no dust power changes,
    // so nothing re-fans-out and the repeater is never notified at all. The
    // first draft of this file did exactly that and the locking test's control
    // arm is what caught it.
    let (near, far) = if source_lit { (0, 0) } else { (15, 14) };
    column.set_block(2, Y, ROW_Z, &redstone_wire::set_power(near));
    column.set_block(3, Y, ROW_Z, &redstone_wire::set_power(far));
    column.set_block(4, Y, ROW_Z, &redstone_diode::set_repeater(Direction::West, delay, false, powered));
    column.set_block(5, Y, ROW_Z, &redstone_wire::set_power(0));
    column
}

/// **The load-bearing #315 timing gate.** For every one of the four delay
/// settings, on both edges, the production path must schedule the repeater's
/// flip at exactly the tick the live server changed its output.
///
/// # Separating the wrong models
///
/// "The repeater eventually switched" passes for essentially any wrong timing
/// model, so three wrong hypotheses are computed here from outside constants
/// and each is required to disagree with the oracle:
///
/// | hypothesis | ticks at delay `d` | disagrees with oracle at |
/// |---|---|---|
/// | oracle (live 26.2) | `2d` | — |
/// | wrong: delay counted in *redstone* ticks, not game ticks | `d` | all 4 settings |
/// | wrong: off by one | `2d - 1` | all 4 settings |
/// | wrong: the delay applies on the falling edge only | `0` rising | all 4 settings |
///
/// The last row is why both edges are measured rather than one: a model that
/// switched on instantly and only delayed switching off reproduces the falling
/// column perfectly.
#[test]
fn repeater_delay_matches_the_live_server_at_every_setting_and_both_edges() {
    for &(delay, oracle_ticks) in ORACLE_REPEATER_DELAY {
        // -- rising edge: source torch goes lit, repeater is off ------------
        let mut column = repeater_rig(delay, false, true);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let _ = propagate_and_react(&mut column, 0, 0, 1, Y, ROW_Z, &mut block_ticks, NOW);
        let entries = scheduled(&mut block_ticks);
        let rising = find(&entries, (4, Y, ROW_Z), redstone::TICK_REPEATER).unwrap_or_else(|| {
            panic!(
                "delay={delay} RISING: no repeater tick was scheduled for the repeater at \
                 (x=4, y={Y}, z={ROW_Z}) at all; input dust at (x=3, y={Y}, z={ROW_Z}) is {:?} \
                 and the entries scheduled were {entries:?}",
                at(&column, 3, Y, ROW_Z)
            )
        });
        assert_eq!(
            rising - NOW,
            oracle_ticks,
            "repeater[delay={delay}] at (x=4, y={Y}, z={ROW_Z}) RISING edge: our model schedules \
             the flip {} tick(s) out, the live 26.2 server changed its output after {oracle_ticks}",
            rising - NOW
        );

        // -- falling edge: source torch goes unlit, repeater is on ----------
        let mut column = repeater_rig(delay, true, false);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let _ = propagate_and_react(&mut column, 0, 0, 1, Y, ROW_Z, &mut block_ticks, NOW);
        let entries = scheduled(&mut block_ticks);
        let falling = find(&entries, (4, Y, ROW_Z), redstone::TICK_REPEATER).unwrap_or_else(|| {
            panic!(
                "delay={delay} FALLING: no repeater tick scheduled at (x=4, y={Y}, z={ROW_Z}); \
                 entries were {entries:?}"
            )
        });
        assert_eq!(
            falling - NOW,
            oracle_ticks,
            "repeater[delay={delay}] at (x=4, y={Y}, z={ROW_Z}) FALLING edge: our model schedules \
             the flip {} tick(s) out, the live 26.2 server changed its output after {oracle_ticks}",
            falling - NOW
        );
    }

    // The gate must be able to tell the oracle apart from each wrong model.
    let redstone_ticks: Vec<u64> = ORACLE_REPEATER_DELAY.iter().map(|&(d, _)| u64::from(d)).collect();
    let off_by_one: Vec<u64> = ORACLE_REPEATER_DELAY.iter().map(|&(_, t)| t - 1).collect();
    let falling_only: Vec<u64> = ORACLE_REPEATER_DELAY.iter().map(|_| 0).collect();
    let oracle: Vec<u64> = ORACLE_REPEATER_DELAY.iter().map(|&(_, t)| t).collect();
    for (name, wrong) in [
        ("delay counted in redstone ticks", &redstone_ticks),
        ("off by one", &off_by_one),
        ("delay on the falling edge only", &falling_only),
    ] {
        let disagreements = oracle.iter().zip(wrong.iter()).filter(|(a, b)| a != b).count();
        assert_eq!(
            disagreements,
            ORACLE_REPEATER_DELAY.len(),
            "the '{name}' hypothesis must differ from the oracle at every delay setting, \
             otherwise this gate cannot separate them: oracle {oracle:?} vs {wrong:?}"
        );
    }
}

/// The flip a scheduled repeater tick performs, driven through the same
/// functions `run_tick_loop`'s drain calls: the state it produces must be the
/// `powered` value the live server showed, and the output dust must then carry
/// the signal.
#[test]
fn a_repeater_scheduled_tick_produces_the_powered_state_the_live_server_showed() {
    let mut column = repeater_rig(1, false, true);
    // Settle the dust first: the rig starts with the torch just lit and the
    // dust still at its pre-flip zero, so the input only exists after the
    // propagation that the timing gate above measures.
    let mut settle: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 1, Y, ROW_Z, &mut settle, NOW);

    let state = at(&column, 4, Y, ROW_Z);
    let should_on = redstone_diode::repeater_should_turn_on(
        &redstone::make_lookup(&column, 0, 0),
        BlockPos::new(4, Y, ROW_Z),
        Direction::West,
    );
    assert!(
        should_on,
        "PREMISE FAILED: the repeater at (x=4, y={Y}, z={ROW_Z}) reads no input from the dust at \
         (x=3, y={Y}, z={ROW_Z}) ({:?}), so this test would pass whatever the flip did",
        at(&column, 3, Y, ROW_Z)
    );
    match redstone_diode::run_scheduled_tick(&state, should_on) {
        redstone_diode::RepeaterTickOutcome::TurnedOn { new_state, reschedule } => {
            assert!(redstone::diode_powered(&new_state), "flip produced {new_state:?}, still unpowered");
            assert!(!reschedule, "input is still high, so no pulse-quantization reschedule is due");
            column.set_block(4, Y, ROW_Z, &new_state);
        }
        other => panic!("expected the repeater to turn on, got {other:?}"),
    }

    // ... and the output dust must then actually light up, at the value the
    // live server measured (a repeater's output dust reads 15).
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let events = propagate_and_react(&mut column, 0, 0, 4, Y, ROW_Z, &mut block_ticks, NOW);
    let out = redstone::wire_power(&at(&column, 5, Y, ROW_Z));
    assert_eq!(
        out, 15,
        "output dust at (x=5, y={Y}, z={ROW_Z}) carries power {out}; the live server measured 15 \
         on the dust square a powered repeater drives"
    );

    // Computed is not delivered: the value must also be published, or no
    // client ever sees it.
    let published = events.iter().find(|e| e.pos == (5, Y, ROW_Z)).unwrap_or_else(|| {
        panic!(
            "output dust at (x=5, y={Y}, z={ROW_Z}) reached power {out} in the column but was \
             never published as an event -- it would never reach a client. published: {:?}",
            events.iter().map(|e| e.pos).collect::<Vec<_>>()
        )
    });
    assert_eq!(redstone::wire_power(&published.to), 15);
}

/// **Repeater locking**, the trap issue #315 names by name.
///
/// Live 26.2, measured one side block at a time on an otherwise identical rig:
///
/// ```text
///   nothing                                   -> locked=false
///   powered repeater facing=south             -> locked=TRUE
///   UNpowered repeater facing=south           -> locked=false
///   powered repeater facing=north (wrong way) -> locked=false
///   lit redstone_torch                        -> locked=false
///   redstone_block                            -> locked=false
/// ```
///
/// The paired rows are what make this more than "a repeater can lock": the
/// same block unpowered, and the same block rotated, must both fail to lock,
/// and a lit torch — a perfectly good power source — must fail too, because
/// `RepeaterBlock.sideInputDiodesOnly()` is `true`.
#[test]
fn only_a_powered_diode_facing_the_right_way_locks_a_repeater() {
    // The main repeater faces west, so its side positions are north (z-1) and
    // south (z+1); a diode at the south position must have facing=south to be
    // seen, because DiodeBlock.getSignal only answers for its own FACING.
    let cases: &[(&str, Option<String>, bool)] = &[
        ("nothing", None, false),
        (
            "powered repeater facing=south",
            Some(redstone_diode::set_repeater(Direction::South, 1, false, true)),
            true,
        ),
        (
            "UNpowered repeater facing=south",
            Some(redstone_diode::set_repeater(Direction::South, 1, false, false)),
            false,
        ),
        (
            "powered repeater facing=north (wrong way)",
            Some(redstone_diode::set_repeater(Direction::North, 1, false, true)),
            false,
        ),
        ("lit redstone_torch", Some(redstone_torch::set_standing_lit(true)), false),
    ];

    for (label, side, oracle_locked) in cases {
        let mut column = repeater_rig(1, false, true);
        if let Some(side_state) = side {
            column.set_block(4, Y, ROW_Z + 1, side_state);
        }
        let locked = redstone_diode::is_locked(
            &redstone::make_lookup(&column, 0, 0),
            BlockPos::new(4, Y, ROW_Z),
            Direction::West,
        );
        assert_eq!(
            locked, *oracle_locked,
            "side block at (x=4, y={Y}, z={}) = {label}: our model says locked={locked}, the live \
             26.2 server measured locked={oracle_locked}",
            ROW_Z + 1
        );
    }
}

/// **The behavioural half of locking, with its own paired control.** On the
/// live server a repeater held locked by a sustained side diode stayed
/// unpowered for at least 12 ticks with its front input high, while the
/// identical rig with the side diode removed powered at tick 2.
///
/// Both arms are run here. Without the control arm, "nothing was scheduled"
/// is indistinguishable from a rig that never reached the repeater at all —
/// which is exactly how the first draft of the live measurement failed (the
/// side repeater had no source of its own, unpowered itself after 2 ticks, and
/// the lock silently evaporated mid-reading).
#[test]
fn a_locked_repeater_schedules_nothing_while_the_same_rig_unlocked_schedules_at_two_ticks() {
    // Locked arm.
    let mut column = repeater_rig(1, false, true);
    column.set_block(4, Y, ROW_Z + 1, &redstone_diode::set_repeater(Direction::South, 1, false, true));
    column.set_block(4, Y, ROW_Z, &redstone_diode::set_repeater(Direction::West, 1, true, false));
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 1, Y, ROW_Z, &mut block_ticks, NOW);
    let locked_entries = scheduled(&mut block_ticks);
    assert!(
        find(&locked_entries, (4, Y, ROW_Z), redstone::TICK_REPEATER).is_none(),
        "a LOCKED repeater at (x=4, y={Y}, z={ROW_Z}) scheduled a flip anyway; on the live server \
         it stayed unpowered for at least 12 ticks with the same input high. entries: \
         {locked_entries:?}"
    );

    // Control arm: identical rig, no side diode.
    let mut column = repeater_rig(1, false, true);
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 1, Y, ROW_Z, &mut block_ticks, NOW);
    let control_entries = scheduled(&mut block_ticks);
    let control = find(&control_entries, (4, Y, ROW_Z), redstone::TICK_REPEATER).unwrap_or_else(|| {
        panic!(
            "CONTROL FAILED: the unlocked arm scheduled nothing either, so the locked arm's \
             silence proves nothing about locking. entries: {control_entries:?}"
        )
    });
    assert_eq!(
        control - NOW,
        2,
        "control arm scheduled at {} tick(s); the live server powered the unlocked repeater at \
         tick 2",
        control - NOW
    );
}

// ---------------------------------------------------------------------------
// #315 — comparators
// ---------------------------------------------------------------------------

/// **The comparator value table, all 30 live-measured rows.**
///
/// Both the analog output and the `powered` flag are checked, because they
/// disagree in exactly the rows that matter: at a tie (`input == side`),
/// compare mode outputs the input and *is* powered while subtract mode outputs
/// zero and is *not*.
///
/// Two wrong models are computed from the same rows and required to disagree:
/// a comparator that always subtracts (ignoring the mode) and one that never
/// blocks on a stronger side input.
#[test]
fn comparator_output_matches_the_live_server_in_both_modes() {
    let mut wrong_always_subtract = 0usize;
    let mut wrong_never_blocks = 0usize;

    for &(subtract, input, side, oracle_out, oracle_powered) in ORACLE_COMPARATOR_TABLE {
        let mode = if subtract { "subtract" } else { "compare" };
        let out = redstone_diode::calculate_comparator_output(input, side, subtract);
        assert_eq!(
            out, oracle_out,
            "comparator[mode={mode}] with input={input}, side={side}: our model outputs {out}, \
             the live 26.2 server measured {oracle_out}"
        );
        let powered = redstone_diode::comparator_should_turn_on(input, side, subtract);
        assert_eq!(
            powered, oracle_powered,
            "comparator[mode={mode}] with input={input}, side={side}: our model says \
             powered={powered}, the live 26.2 server measured powered={oracle_powered}"
        );

        // Wrong model A: subtract regardless of mode.
        if redstone_diode::calculate_comparator_output(input, side, true) != oracle_out {
            wrong_always_subtract += 1;
        }
        // Wrong model B: never block on a stronger side input (plain
        // pass-through in compare mode, saturating subtract in subtract mode).
        let never_blocks = if subtract { input.saturating_sub(side) } else { input };
        if never_blocks != oracle_out {
            wrong_never_blocks += 1;
        }
    }

    assert!(
        wrong_always_subtract >= 8,
        "the 'always subtract' hypothesis disagrees with the oracle in only \
         {wrong_always_subtract} of {} rows -- this table would barely separate them",
        ORACLE_COMPARATOR_TABLE.len()
    );
    assert!(
        wrong_never_blocks >= 3,
        "the 'never blocks on a stronger side input' hypothesis disagrees in only \
         {wrong_never_blocks} rows"
    );
}

/// The comparator's own rig, end to end: input dust fed by a lit torch to the
/// west, an optional side arm fed by its own lit torch to the south, output
/// dust to the east.
fn comparator_rig(subtract: bool, with_side: bool) -> ChunkColumn {
    let mut column = column_with_floor();
    // Input arm (west): torch at x=6, dust at x=7 -> power 15.
    column.set_block(6, Y, ROW_Z, &redstone_torch::set_standing_lit(true));
    column.set_block(7, Y, ROW_Z, &redstone_wire::set_power(0));
    column.set_block(8, Y, ROW_Z, &redstone_diode::set_comparator(Direction::West, subtract, false, 0));
    column.set_block(9, Y, ROW_Z, &redstone_wire::set_power(0));
    if with_side {
        // Side arm (south = +z): torch at z+2, dust at z+1 -> power 15.
        column.set_block(8, Y, ROW_Z + 2, &redstone_torch::set_standing_lit(true));
        column.set_block(8, Y, ROW_Z + 1, &redstone_wire::set_power(0));
    }
    column
}

/// **End to end through the production path**, for the two live rows this rig
/// can reach exactly: input 15 with no side input, and input 15 with side 15.
///
/// | mode | input | side | live output |
/// |---|---|---|---|
/// | compare | 15 | 0 | 15 |
/// | compare | 15 | 15 | 15 |
/// | subtract | 15 | 0 | 15 |
/// | subtract | 15 | 15 | **0** |
///
/// The last row is the one that separates the modes, and it is the reason the
/// side arm exists: with no side input the two modes are indistinguishable.
#[test]
fn a_comparator_rig_reaches_the_live_output_through_the_production_path() {
    for (subtract, with_side, oracle_out) in [
        (false, false, 15u8),
        (false, true, 15),
        (true, false, 15),
        (true, true, 0),
    ] {
        let mode = if subtract { "subtract" } else { "compare" };
        let mut column = comparator_rig(subtract, with_side);
        let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // Drive from the input torch, exactly as a torch flip does in production.
        let _ = propagate_and_react(&mut column, 0, 0, 6, Y, ROW_Z, &mut block_ticks, NOW);

        let input = redstone::wire_power(&at(&column, 7, Y, ROW_Z));
        assert_eq!(
            input, 15,
            "PREMISE FAILED: input dust at (x=7, y={Y}, z={ROW_Z}) reads {input}, not 15, so the \
             row being checked is not the row the oracle measured"
        );
        let side = if with_side { redstone::wire_power(&at(&column, 8, Y, ROW_Z + 1)) } else { 0 };
        if with_side {
            assert_eq!(
                side, 15,
                "PREMISE FAILED: side dust at (x=8, y={Y}, z={}) reads {side}, not 15",
                ROW_Z + 1
            );
        }

        let entries = scheduled(&mut block_ticks);
        let due = find(&entries, (8, Y, ROW_Z), redstone::TICK_COMPARATOR).unwrap_or_else(|| {
            panic!("mode={mode}: no comparator tick scheduled at (x=8, y={Y}, z={ROW_Z}); entries {entries:?}")
        });
        assert_eq!(
            due - NOW,
            ORACLE_COMPARATOR_DELAY,
            "mode={mode}: our model schedules the comparator {} tick(s) out; the live 26.2 server \
             changed its output after {ORACLE_COMPARATOR_DELAY}",
            due - NOW
        );

        let state = at(&column, 8, Y, ROW_Z);
        let flipped = redstone_diode::run_scheduled_comparator_tick(&state, input, side);
        let out = flipped.as_deref().map_or_else(|| redstone::comparator_output(&state), redstone::comparator_output);
        assert_eq!(
            out, oracle_out,
            "comparator[mode={mode}] at (x=8, y={Y}, z={ROW_Z}) with input={input}, side={side}: \
             our model's scheduled tick produced output {out}, the live 26.2 server measured \
             {oracle_out}"
        );
    }
}

// ---------------------------------------------------------------------------
// #317 — observers
// ---------------------------------------------------------------------------

/// **The #317 pulse gate.** An observer at `x = 8` facing **west** watches
/// `x = 7` and drives `x = 9` out its back.
///
/// The live server powered the back face on ticks 2 and 3 and nothing else, so
/// three separate numbers are asserted — when it starts, how wide it is, and
/// that it ends — rather than "a pulse happened":
///
/// | hypothesis | starts | width | separated by |
/// |---|---|---|---|
/// | oracle (live 26.2) | tick 2 | 2 ticks | — |
/// | wrong: fires immediately | tick 0 | — | the start assertion |
/// | wrong: one game tick wide | tick 2 | 1 tick | the width assertion |
/// | wrong: stays powered | tick 2 | forever | the reschedule assertion |
#[test]
fn an_observer_pulse_starts_and_ends_exactly_where_the_live_server_measured() {
    let mut column = column_with_floor();
    column.set_block(8, Y, ROW_Z, &redstone_observer::set_observer(Direction::West, false));
    column.set_block(9, Y, ROW_Z, &redstone_wire::set_power(0));
    // The watched block changes: air -> stone at x = 7.
    column.set_block(7, Y, ROW_Z, "minecraft:stone");

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 7, Y, ROW_Z, &mut block_ticks, NOW);
    let entries = scheduled(&mut block_ticks);
    let start = find(&entries, (8, Y, ROW_Z), redstone::TICK_OBSERVER).unwrap_or_else(|| {
        panic!(
            "the observer at (x=8, y={Y}, z={ROW_Z}) was never scheduled after the block it \
             watches at (x=7, y={Y}, z={ROW_Z}) changed; entries {entries:?}"
        )
    });
    assert_eq!(
        start - NOW,
        ORACLE_OBSERVER_PULSE_START,
        "our model starts the observer pulse {} tick(s) after the change; the live 26.2 server's \
         back face first went high at tick {ORACLE_OBSERVER_PULSE_START}",
        start - NOW
    );

    // Width: drive the scheduled ticks the way `run_tick_loop`'s drain does
    // and count how many of them leave the observer powered.
    let mut state = at(&column, 8, Y, ROW_Z);
    let mut powered_ticks = 0usize;
    let mut reschedules = 0usize;
    for _ in 0..6 {
        let (next, reschedule) = redstone_observer::run_scheduled_tick(&state);
        state = next;
        if redstone::observer_powered(&state) {
            powered_ticks += 1;
        }
        if reschedule {
            reschedules += 1;
        } else {
            break;
        }
    }
    assert_eq!(
        powered_ticks, 1,
        "the observer entered the powered state {powered_ticks} time(s) across its pulse; it must \
         go high exactly once"
    );
    assert_eq!(
        reschedules, 1,
        "the observer rescheduled itself {reschedules} time(s); exactly one reschedule is what \
         makes the pulse {ORACLE_OBSERVER_PULSE_WIDTH} game ticks wide instead of endless"
    );
    assert!(
        !redstone::observer_powered(&state),
        "after its pulse the observer is still powered ({state:?}); the live server's back face \
         was unpowered from tick {} on",
        ORACLE_OBSERVER_PULSE_START + ORACLE_OBSERVER_PULSE_WIDTH as u64
    );
}

/// **Negative control for the gate above, both halves run.** The live server
/// did not pulse when a block *behind* the observer changed, and did not pulse
/// when a `/setblock` wrote the same state that was already there.
///
/// Only the first half is expressible here — this crate issues a notification
/// from a mutation, so a no-op mutation produces no notification at all — and
/// the direction half is the one that could actually be got wrong, since
/// `watch_direction` is an `opposite()` away from the naive reading.
#[test]
fn an_observer_does_not_pulse_when_a_block_behind_it_changes() {
    let mut column = column_with_floor();
    column.set_block(8, Y, ROW_Z, &redstone_observer::set_observer(Direction::West, false));
    // Change a block BEHIND the observer (east side) rather than in front.
    column.set_block(9, Y, ROW_Z, "minecraft:stone");

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 9, Y, ROW_Z, &mut block_ticks, NOW);
    let entries = scheduled(&mut block_ticks);
    assert!(
        find(&entries, (8, Y, ROW_Z), redstone::TICK_OBSERVER).is_none(),
        "the observer at (x=8, y={Y}, z={ROW_Z}) pulsed from a change BEHIND it at \
         (x=9, y={Y}, z={ROW_Z}); the live 26.2 server did not. entries {entries:?}"
    );

    // Control on the control: the same rig changed in FRONT must pulse, or the
    // silence above would prove nothing.
    let mut column = column_with_floor();
    column.set_block(8, Y, ROW_Z, &redstone_observer::set_observer(Direction::West, false));
    column.set_block(7, Y, ROW_Z, "minecraft:stone");
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 7, Y, ROW_Z, &mut block_ticks, NOW);
    let entries = scheduled(&mut block_ticks);
    assert!(
        find(&entries, (8, Y, ROW_Z), redstone::TICK_OBSERVER).is_some(),
        "CONTROL FAILED: the same observer did not pulse from a change in FRONT either, so the \
         negative result above is not about direction. entries {entries:?}"
    );
}
