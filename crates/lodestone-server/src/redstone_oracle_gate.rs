//! Issue #314's end-to-end gate: redstone propagation driven through the
//! **production** entry point, against expected values measured on a **real
//! 26.2 server**.
//!
//! # Why this module exists separately from the per-family unit tests
//!
//! `crate::redstone{,_wire,_torch,_diode,_observer}` each carry their own
//! `#[cfg(test)]` tests — 55 of them — and every one calls the redstone
//! functions *directly*. That is a closed loop: it can be entirely green
//! while nothing in the production tick ever calls any of it (this subsystem
//! was in exactly that state until `ac5d2b7` added the five missing `mod`
//! lines to `lib.rs`). The gates here instead call
//! [`crate::random_tick::propagate_and_react`] — the same function
//! `crate::tick::run_tick_loop` calls when a scheduled block tick drains —
//! and assert on the `RandomTickEvent`s it returns, which are precisely what
//! `run_tick_loop` publishes to [`crate::tick::BlockTickFeed`] and thence to
//! a connected client.
//!
//! # The external oracle
//!
//! Every expected value below was **measured on a live vanilla 26.2 server**
//! (the flat/creative oracle, `scripts/live-oracles/creative.sh`, driven over
//! RCON), not derived from this crate's own model. `decode(encode(x)) == x`
//! is satisfied by two symmetric misunderstandings, so an expected value
//! this crate could have produced itself is worth very little here.
//!
//! ## What was measured, and how
//!
//! Dust power is a *block state* property, so it is not reachable through
//! `/data get block` (which serves block entities only). It was read by
//! probing `execute if block <pos> minecraft:redstone_wire[power=N]` for
//! `N` in `0..=15` — an exact read, not a sampled one.
//!
//! **Attenuation** ([`ORACLE_DUST_ATTENUATION`]) was measured three times,
//! independently, and agreed exactly each time:
//!
//! 1. a `minecraft:redstone_block` source feeding a 20-long dust run,
//! 2. a lit `minecraft:redstone_torch` source feeding a 19-long dust run,
//! 3. a `redstone_block` feeding a 6-long run on a raised platform.
//!
//! Reading 2 is the one reproduced here, because a lit torch is the only
//! power *source* this crate models (`crate::redstone`'s own module doc
//! explains why `redstone_block`, levers and buttons deliberately are not).
//! Readings 1 and 2 produced byte-identical power profiles, which is what
//! makes the choice of source safe.
//!
//! **Timing.** Two facts, both measured rather than assumed:
//!
//! * Dust propagation is **synchronous — zero ticks**. With the server under
//!   `/tick freeze`, a `/setblock` of the source drove the entire 6-long run
//!   to its final powers `[15, 14, 13, 12, 11, 10]` *inside that one
//!   command*, before any tick was stepped.
//! * A torch's inversion is **not** synchronous: under the same freeze the
//!   torch did not flip, and only flipped once the server was allowed to
//!   tick. Its delay is `RedstoneTorchBlock.TOGGLE_DELAY = 2`
//!   (`.cache/mc/26.2/src/net/minecraft/world/level/block/RedstoneTorchBlock.java:31`),
//!   read from the jar's own source rather than counted by hand.
//!
//! ## Two oracle traps worth not re-paying for
//!
//! Both cost real time while building this, and both fail in the
//! *safe-looking* direction — the rig reports a plausible "nothing happened"
//! rather than an error:
//!
//! * **`/setblock` does not reproduce a power source's natural update
//!   fan-out.** `LeverBlock.updateNeighbours` — which notifies the attached
//!   block *and that block's own neighbours*, the reach a torch two blocks
//!   away needs — runs from the lever's use/removal handlers, not from
//!   `setBlock`. A lever flipped with `/setblock` therefore powers its block
//!   but never notifies the torch, which sits there lit forever. Redstone
//!   **dust** is the correct trigger: its evaluator performs that fan-out on
//!   every power change, through the ordinary neighbour-update path.
//! * **`/tick step N` does not advance scheduled *block* ticks.** This is the
//!   documented `tick step` / `tick sprint` trap, and it extends past entity
//!   physics. Measured against a rig *proven* to work: settling with two
//!   seconds of real time inverted the torch every time, while eight
//!   consecutive `/tick step 1` calls on the identical rig never did. For a
//!   redstone timing measurement that off-by-N is the entire result, so the
//!   timing facts above were established with real time and with the jar
//!   constant, never by counting `tick step`s.

use crate::chunk::ChunkColumn;
use crate::random_tick::propagate_and_react;
use crate::scheduled_tick::ScheduledTickQueue;
use crate::{redstone, redstone_torch, redstone_wire};

/// Live-measured dust attenuation: `(distance_from_source, power)`.
///
/// Distance is in blocks of dust from the power source, so distance `1` is
/// the dust square immediately adjacent to it. Transcribed verbatim from the
/// 26.2 oracle run described in this module's doc comment (reading 2, the
/// lit-torch source); the `redstone_block` source produced the same profile.
const ORACLE_DUST_ATTENUATION: &[(i32, u8)] = &[
    (1, 15),
    (2, 14),
    (3, 13),
    (4, 12),
    (5, 11),
    (6, 10),
    (7, 9),
    (8, 8),
    (9, 7),
    (10, 6),
    (11, 5),
    (12, 4),
    (13, 3),
    (14, 2),
    (15, 1),
];

/// `RedstoneTorchBlock.TOGGLE_DELAY` (`RedstoneTorchBlock.java:31`), read
/// from the 26.2 jar's own source.
const ORACLE_TORCH_TOGGLE_DELAY: u64 = 2;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// The row the dust runs along. Every rig here lives inside a single
/// column's 16x16 footprint, because `propagate_and_react` deliberately
/// skips neighbours outside it.
const ROW_Z: i32 = 8;
const FLOOR_Y: i32 = 0;
const DUST_Y: i32 = 1;

/// Builds a column with a stone floor under the whole footprint, so no rig
/// below accidentally reads air where vanilla would have had ground.
fn column_with_floor() -> ChunkColumn {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            column.set_block(x, FLOOR_Y, z, "minecraft:stone");
        }
    }
    column
}

/// Lays a lit standing torch at `x = 0` and unpowered dust along
/// `x = 1..=15`, all at `z = ROW_Z`.
fn lay_torch_and_dust(column: &mut ChunkColumn, torch_lit: bool) {
    column.set_block(0, DUST_Y, ROW_Z, &redstone_torch::set_standing_lit(torch_lit));
    for x in 1..16 {
        column.set_block(x, DUST_Y, ROW_Z, &redstone_wire::set_power(0));
    }
}

/// The final power of the dust at each `x`, read straight out of the column.
fn dust_profile(column: &ChunkColumn) -> Vec<(i32, u8)> {
    (1..16)
        .map(|x| (x, redstone::wire_power(column.block_state(x, DUST_Y, ROW_Z))))
        .collect()
}

/// **The load-bearing gate.** A lit torch feeding a 15-long dust run must
/// reproduce the live server's attenuation profile exactly, at every
/// coordinate, when driven through the production propagation entry point.
///
/// # Predicting the value, not the sign
///
/// "The signal propagated" passes for almost any wrong model, so this gate
/// computes two *wrong* hypotheses from outside constants and requires the
/// measurement to land on the oracle and on neither of them:
///
/// | hypothesis | power at distance `d` | at `d = 1` |
/// |---|---|---|
/// | oracle (live 26.2) | `16 - d` | 15 |
/// | wrong: decay applied at the source too | `15 - d` | 14 |
/// | wrong: dust carries the source's power undecayed | `15` | 15 |
///
/// The off-by-one hypothesis differs from the oracle at **every** one of the
/// 15 coordinates; the no-decay hypothesis differs at 14 of them. A gate
/// asserting only "power decreases with distance" would pass under the
/// off-by-one model, which is exactly the failure this table rules out.
#[test]
fn dust_attenuates_exactly_as_the_live_server_does() {
    let mut column = column_with_floor();
    lay_torch_and_dust(&mut column, true);

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let events = propagate_and_react(&mut column, 0, 0, 0, DUST_Y, ROW_Z, &mut block_ticks, 0);

    let measured = dust_profile(&column);
    let expected: Vec<(i32, u8)> = ORACLE_DUST_ATTENUATION.to_vec();

    // Report by location, never as an aggregate: name the first coordinate
    // that disagrees and what was there.
    for (&(distance, oracle_power), &(x, measured_power)) in expected.iter().zip(measured.iter()) {
        assert_eq!(distance, x, "rig misalignment: oracle distance {distance} vs column x {x}");
        assert_eq!(
            measured_power, oracle_power,
            "dust at (x={x}, y={DUST_Y}, z={ROW_Z}), {distance} block(s) from the torch: \
             our model says power={measured_power}, the live 26.2 server measured power={oracle_power}. \
             full profile: {measured:?}"
        );
    }

    // Separate the oracle from each wrong hypothesis, so this gate is known
    // to be able to tell them apart rather than merely agreeing with one.
    let off_by_one: Vec<(i32, u8)> =
        (1..16).map(|d| (d, u8::try_from(15 - d).unwrap_or(0))).collect();
    let no_decay: Vec<(i32, u8)> = (1..16).map(|d| (d, 15u8)).collect();

    let disagreements_off_by_one =
        expected.iter().zip(off_by_one.iter()).filter(|(a, b)| a.1 != b.1).count();
    let disagreements_no_decay =
        expected.iter().zip(no_decay.iter()).filter(|(a, b)| a.1 != b.1).count();
    assert_eq!(
        disagreements_off_by_one, 15,
        "the off-by-one hypothesis must differ from the oracle at every coordinate, \
         otherwise this gate cannot separate them"
    );
    assert_eq!(
        disagreements_no_decay, 14,
        "the no-decay hypothesis must differ from the oracle at 14 of 15 coordinates"
    );
    assert_ne!(measured, off_by_one, "our model matches the OFF-BY-ONE hypothesis, not the oracle");
    assert_ne!(measured, no_decay, "our model matches the NO-DECAY hypothesis, not the oracle");

    // The events are what `run_tick_loop` publishes to `BlockTickFeed`, i.e.
    // what actually reaches a client. Every dust square whose power changed
    // must appear, carrying its final value -- a correct power that is never
    // published is still an invisible one.
    let mut published: std::collections::HashMap<(i32, i32, i32), String> =
        std::collections::HashMap::new();
    for event in &events {
        published.insert(event.pos, event.to.clone());
    }
    for &(distance, oracle_power) in ORACLE_DUST_ATTENUATION {
        let key = (distance, DUST_Y, ROW_Z);
        let to = published.get(&key).unwrap_or_else(|| {
            panic!(
                "dust at (x={distance}, y={DUST_Y}, z={ROW_Z}) reached power {oracle_power} in the \
                 column but was never published as an event -- it would never reach a client. \
                 published coordinates: {:?}",
                published.keys().collect::<Vec<_>>()
            )
        });
        assert_eq!(
            redstone::wire_power(to),
            oracle_power,
            "published event for (x={distance}, y={DUST_Y}, z={ROW_Z}) carries {to:?}, \
             but the live server measured power={oracle_power}"
        );
    }
}

/// **Negative control for the gate above.** The identical rig with an
/// *unlit* torch must leave every dust square at 0. If this ever passes with
/// the torch lit, or fails with it unlit, the gate above is measuring
/// something other than propagation from the source.
#[test]
fn an_unlit_torch_propagates_nothing() {
    let mut column = column_with_floor();
    lay_torch_and_dust(&mut column, false);

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 0, DUST_Y, ROW_Z, &mut block_ticks, 0);

    for (x, power) in dust_profile(&column) {
        assert_eq!(
            power, 0,
            "dust at (x={x}, y={DUST_Y}, z={ROW_Z}) carries power {power} with an UNLIT torch as \
             the only source -- something other than the torch is powering this run"
        );
    }
}

/// **A conductor gap must break the run**, at the exact coordinate it is
/// placed. Replacing the dust at `x = 8` with stone must leave `x = 1..=7`
/// exactly as the oracle measured and `x = 9..=15` at zero -- not merely
/// "lower", and not a uniformly dimmer run.
#[test]
fn a_gap_in_the_dust_stops_the_signal_at_that_coordinate() {
    const GAP_X: i32 = 8;

    let mut column = column_with_floor();
    lay_torch_and_dust(&mut column, true);
    column.set_block(GAP_X, DUST_Y, ROW_Z, "minecraft:stone");

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 0, DUST_Y, ROW_Z, &mut block_ticks, 0);

    for &(distance, oracle_power) in ORACLE_DUST_ATTENUATION {
        if distance >= GAP_X {
            continue;
        }
        let measured = redstone::wire_power(column.block_state(distance, DUST_Y, ROW_Z));
        assert_eq!(
            measured, oracle_power,
            "before the gap, dust at x={distance} must still match the oracle exactly \
             (got {measured}, oracle {oracle_power})"
        );
    }
    for x in (GAP_X + 1)..16 {
        let measured = redstone::wire_power(column.block_state(x, DUST_Y, ROW_Z));
        assert_eq!(
            measured, 0,
            "dust at x={x} is past a stone gap at x={GAP_X} and must be unpowered, got {measured}"
        );
    }
}

/// A torch's inversion is **scheduled, not immediate**, and scheduled at
/// exactly `TOGGLE_DELAY = 2` ticks.
///
/// Both halves matter and they fail differently. The live server showed dust
/// reaching its final power *inside* a single frozen `/setblock` while the
/// torch did not flip at all until the server was allowed to tick — so a
/// model that flipped the torch synchronously would be wrong in a way no
/// steady-state assertion could see. The delay value separates `+2` from the
/// two adjacent off-by-one models.
///
/// Rig, mirroring the one validated live: stone attachment block at
/// `(5, 1, 8)`, a lit wall torch at `(6, 1, 8)` with `facing=east` (so it
/// watches the block to its west), and powered dust on top of the
/// attachment block at `(5, 2, 8)`, itself fed by a lit source torch so it
/// holds its power instead of decaying to zero when re-evaluated.
///
/// The notification originates at the **attachment block**, which is a
/// direct neighbour of the torch. Originating it at the dust instead is the
/// geometry covered by
/// [`the_second_layer_fan_out_gap_leaves_a_side_torch_unnotified`], and it
/// does *not* reach the torch today.
#[test]
fn a_torch_inversion_is_scheduled_at_exactly_two_ticks_not_applied_immediately() {
    const SOURCE_X: i32 = 3;
    const ATTACH_X: i32 = 5;
    const TORCH_X: i32 = 6;
    const CURRENT_TICK: u64 = 100;

    let mut column = column_with_floor();
    column.set_block(ATTACH_X, DUST_Y, ROW_Z, "minecraft:stone");
    let torch_state = redstone_torch::set_wall_lit(crate::neighbor_update::Direction::East, true);
    column.set_block(TORCH_X, DUST_Y, ROW_Z, &torch_state);
    // A lit source torch feeding the dust above the attachment block, so the
    // dust's own re-evaluation keeps it powered rather than resetting it.
    column.set_block(SOURCE_X, DUST_Y + 1, ROW_Z, &redstone_torch::set_standing_lit(true));
    column.set_block(ATTACH_X - 1, DUST_Y + 1, ROW_Z, &redstone_wire::set_power(15));
    column.set_block(ATTACH_X, DUST_Y + 1, ROW_Z, &redstone_wire::set_power(14));

    // Premise check: with this rig the torch must actually see a signal,
    // otherwise the rest of the test is vacuous (the earlier oracle rigs
    // failed in exactly this way and looked like a passing "nothing
    // happened").
    let has_signal = {
        let lookup = redstone::make_lookup(&column, 0, 0);
        redstone_torch::has_neighbor_signal(
            &lookup,
            lodestone_model::BlockPos::new(TORCH_X, DUST_Y, ROW_Z),
            &torch_state,
        )
    };
    assert!(
        has_signal,
        "PREMISE FAILED: the wall torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}) sees no signal from \
         its attachment block at x={ATTACH_X}, so this test would pass no matter what the delay is"
    );

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        ATTACH_X,
        DUST_Y,
        ROW_Z,
        &mut block_ticks,
        CURRENT_TICK,
    );

    // Half one: the torch must NOT have flipped yet.
    let after = column.block_state(TORCH_X, DUST_Y, ROW_Z).to_string();
    assert!(
        redstone::torch_lit(&after),
        "the torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}) was extinguished synchronously \
         ({after:?}); the live server left it lit until the server ticked"
    );

    // Half two: it must be scheduled at exactly CURRENT_TICK + 2.
    let due = block_ticks.drain_due(CURRENT_TICK + 64, 64);
    let torch_tick = due
        .iter()
        .find(|t| t.pos == (TORCH_X, DUST_Y, ROW_Z) && t.kind == redstone::TICK_TORCH)
        .unwrap_or_else(|| {
            panic!(
                "no {} tick was scheduled for the torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}); \
                 scheduled entries were {:?}",
                redstone::TICK_TORCH,
                due.iter().map(|t| (t.pos, &t.kind, t.trigger_tick)).collect::<Vec<_>>()
            )
        });

    let expected = CURRENT_TICK + ORACLE_TORCH_TOGGLE_DELAY;
    assert_eq!(
        torch_tick.trigger_tick, expected,
        "torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}) scheduled for tick {}, but \
         RedstoneTorchBlock.TOGGLE_DELAY = {ORACLE_TORCH_TOGGLE_DELAY} puts it at {expected} \
         (a one-tick error either way is {} or {})",
        torch_tick.trigger_tick,
        expected - 1,
        expected + 1
    );

    // And the flip itself, when that scheduled tick runs, must extinguish it
    // -- the value, not just the timing.
    let has_signal_now = {
        let lookup = redstone::make_lookup(&column, 0, 0);
        redstone_torch::has_neighbor_signal(
            &lookup,
            lodestone_model::BlockPos::new(TORCH_X, DUST_Y, ROW_Z),
            &after,
        )
    };
    let flipped = redstone_torch::run_scheduled_tick(&after, has_signal_now);
    let flipped = flipped.expect("the scheduled tick must produce a state change");
    assert!(
        !redstone::torch_lit(&flipped),
        "when its scheduled tick ran the torch produced {flipped:?}, which is still lit"
    );
}

/// **A named deviation from vanilla, pinned by a live measurement.**
///
/// `crate::random_tick::propagate_and_react`'s own doc comment records that
/// it implements only the *first* layer of
/// `DefaultRedstoneWireEvaluator`'s update fan-out — vanilla additionally
/// fans out from each of the six neighbours' own positions
/// (`DefaultRedstoneWireEvaluator.java:27-37`). That doc calls the omission
/// a corner case affecting "a diagonal-over-conductor corner update".
///
/// It is not a corner case. The geometry it misses is the **standard
/// torch-inverter**: dust on top of a block, with a torch on that block's
/// side. The torch is diagonal to the dust, so only the second layer ever
/// reaches it.
///
/// This was measured, not reasoned about. On a live 26.2 server with exactly
/// this rig — stone attachment block, `redstone_wall_torch[facing=east]` on
/// its east face, dust on top fed by a `redstone_block` — the torch inverted
/// reliably:
///
/// ```text
///   no source, settled:      torch lit = true,  dust above = 0
///   source present, settled: torch lit = FALSE, dust above = 10
///   source removed, settled: torch lit = true,  dust above = 0
/// ```
///
/// This test asserts what we do **today**: the torch is never notified, so
/// no tick is scheduled and it stays lit forever. It is deliberately written
/// as a passing test of current behaviour rather than an ignored aspiration,
/// so that whoever implements the second layer sees it fail and updates it
/// to the vanilla expectation above.
#[test]
fn the_second_layer_fan_out_gap_leaves_a_side_torch_unnotified() {
    const ATTACH_X: i32 = 5;
    const TORCH_X: i32 = 6;
    const CURRENT_TICK: u64 = 100;

    let mut column = column_with_floor();
    column.set_block(ATTACH_X, DUST_Y, ROW_Z, "minecraft:stone");
    let torch_state = redstone_torch::set_wall_lit(crate::neighbor_update::Direction::East, true);
    column.set_block(TORCH_X, DUST_Y, ROW_Z, &torch_state);
    column.set_block(ATTACH_X, DUST_Y + 1, ROW_Z, &redstone_wire::set_power(15));

    // Premise: the torch really does see a signal, so the only reason it is
    // not scheduled is that the notification never arrives. Without this the
    // test would pass for the wrong reason.
    let has_signal = {
        let lookup = redstone::make_lookup(&column, 0, 0);
        redstone_torch::has_neighbor_signal(
            &lookup,
            lodestone_model::BlockPos::new(TORCH_X, DUST_Y, ROW_Z),
            &torch_state,
        )
    };
    assert!(
        has_signal,
        "PREMISE FAILED: the torch sees no signal at all, so this test proves nothing about the \
         fan-out"
    );

    // Notify from the DUST position, as a real dust power change would.
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        ATTACH_X,
        DUST_Y + 1,
        ROW_Z,
        &mut block_ticks,
        CURRENT_TICK,
    );

    let due = block_ticks.drain_due(CURRENT_TICK + 64, 64);
    let scheduled_for_torch =
        due.iter().any(|t| t.pos == (TORCH_X, DUST_Y, ROW_Z) && t.kind == redstone::TICK_TORCH);
    assert!(
        !scheduled_for_torch,
        "the second-layer fan-out now reaches the torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}) -- \
         that is the vanilla behaviour this deviation was blocking, so update this test to assert \
         the live expectation (torch schedules at CURRENT_TICK + 2 and goes out)"
    );
    assert!(
        redstone::torch_lit(column.block_state(TORCH_X, DUST_Y, ROW_Z)),
        "the torch changed state without ever being scheduled"
    );
}
