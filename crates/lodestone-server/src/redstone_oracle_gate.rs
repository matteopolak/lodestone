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
//! Reading 2 is the one the gates in the first half of this file reproduce.
//! Readings 1 and 2 produced byte-identical power profiles, which is what makes
//! the choice of source safe — and what lets the input-device gates at the end
//! of this file assert reading 1's `redstone_block` arm against the same table.
//!
//! `crate::redstone` now models the primary input devices too (lever, button,
//! pressure plates, tripwire hook, detector rail, target, daylight detector and
//! `redstone_block`), so those arms exist below. When this file was written they
//! did not, and every gate here used a torch for that reason; the note that a
//! lit torch was *the only* source this crate modelled was true then and is not
//! now.
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
/// **The second layer now lands, and this test asserts the live expectation.**
///
/// `crate::random_tick::wire_update_fan_out` implements the full seven-centre
/// set, and `propagate_and_react` applies it to the origin as well as to a
/// wire reached mid-cascade — the origin half being the piece a first attempt
/// at this fix still missed, because the propagator only ever fans out one
/// layer from whatever position it is handed.
///
/// # Why this rig carries its own source, and the trap that makes it necessary
///
/// The version of this test written while the gap was open seeded the dust at
/// power 15 by hand with nothing feeding it. That was harmless then: the first
/// layer alone never re-evaluated the dust. With the second layer live it is
/// fatal in the *safe-looking* direction — the cascade reaches the dust, the
/// dust correctly recomputes to 0 for want of a source, the attachment block
/// stops being powered, and the torch is then correctly **not** scheduled. The
/// test would have gone on passing while proving nothing, and its own premise
/// check would not have caught it, because the premise was true when it was
/// checked and false by the time it mattered.
///
/// So the rig here is fed by a real lit torch through a real dust run, and the
/// dust starts at zero so the propagation under test is what powers it.
#[test]
fn the_second_layer_fan_out_reaches_a_side_torch_and_inverts_it() {
    const SOURCE_X: i32 = 2;
    const ATTACH_X: i32 = 5;
    const TORCH_X: i32 = 6;
    const CURRENT_TICK: u64 = 100;

    let mut column = column_with_floor();
    // Source torch on its own support, feeding a dust run along y = DUST_Y+1.
    column.set_block(SOURCE_X, DUST_Y, ROW_Z, "minecraft:stone");
    column.set_block(SOURCE_X, DUST_Y + 1, ROW_Z, &redstone_torch::set_standing_lit(true));
    for x in (SOURCE_X + 1)..=ATTACH_X {
        column.set_block(x, DUST_Y + 1, ROW_Z, &redstone_wire::set_power(0));
    }
    // The inverter itself: stone with dust on top and a wall torch on its side.
    column.set_block(ATTACH_X, DUST_Y, ROW_Z, "minecraft:stone");
    let torch_state = redstone_torch::set_wall_lit(crate::neighbor_update::Direction::East, true);
    column.set_block(TORCH_X, DUST_Y, ROW_Z, &torch_state);

    // Drive it exactly as production does after a torch flip.
    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        SOURCE_X,
        DUST_Y + 1,
        ROW_Z,
        &mut block_ticks,
        CURRENT_TICK,
    );

    // Premise, checked AFTER propagation rather than before: the dust really
    // did power the attachment block, so a torch that is not scheduled would
    // be a fan-out failure and not a dead rig.
    let dust_on_attach = redstone::wire_power(column.block_state(ATTACH_X, DUST_Y + 1, ROW_Z));
    assert!(
        dust_on_attach > 0,
        "PREMISE FAILED: the dust on top of the attachment block at \
         (x={ATTACH_X}, y={}, z={ROW_Z}) settled at power 0, so the torch has nothing to react to \
         and this test would prove nothing about the fan-out",
        DUST_Y + 1
    );

    let due = block_ticks.drain_due(CURRENT_TICK + 64, 64);
    let torch_tick = due
        .iter()
        .find(|t| t.pos == (TORCH_X, DUST_Y, ROW_Z) && t.kind == redstone::TICK_TORCH)
        .unwrap_or_else(|| {
            panic!(
                "the torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}) was never notified, so it can \
                 never invert -- but the dust on its attachment block at \
                 (x={ATTACH_X}, y={}, z={ROW_Z}) is at power {dust_on_attach} and the live 26.2 \
                 server puts that torch out. scheduled entries: {:?}",
                DUST_Y + 1,
                due.iter().map(|t| (t.pos, &t.kind, t.trigger_tick)).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        torch_tick.trigger_tick,
        CURRENT_TICK + ORACLE_TORCH_TOGGLE_DELAY,
        "the side torch was scheduled for tick {}, but RedstoneTorchBlock.TOGGLE_DELAY = \
         {ORACLE_TORCH_TOGGLE_DELAY} puts it at {}",
        torch_tick.trigger_tick,
        CURRENT_TICK + ORACLE_TORCH_TOGGLE_DELAY
    );

    // And the flip itself must put it out -- the live reading was
    // `source present, settled: torch lit = FALSE`.
    let state = column.block_state(TORCH_X, DUST_Y, ROW_Z).to_string();
    let has_signal_now = {
        let lookup = redstone::make_lookup(&column, 0, 0);
        redstone_torch::has_neighbor_signal(
            &lookup,
            lodestone_model::BlockPos::new(TORCH_X, DUST_Y, ROW_Z),
            &state,
        )
    };
    let flipped = redstone_torch::run_scheduled_tick(&state, has_signal_now)
        .expect("the scheduled tick must produce a state change");
    assert!(
        !redstone::torch_lit(&flipped),
        "when its scheduled tick ran the side torch produced {flipped:?}, which is still lit; the \
         live 26.2 server measured lit=false with the source present"
    );
}

/// **Negative control for the gate above.** The identical inverter rig with an
/// *unlit* source torch must leave the side torch alone: the dust never
/// powers, the attachment block is never powered, and no torch tick is
/// scheduled. Without this, "the torch was scheduled" could be coming from
/// anything in the rig rather than from the source.
#[test]
fn the_side_torch_is_left_alone_when_the_source_is_unlit() {
    const SOURCE_X: i32 = 2;
    const ATTACH_X: i32 = 5;
    const TORCH_X: i32 = 6;
    const CURRENT_TICK: u64 = 100;

    let mut column = column_with_floor();
    column.set_block(SOURCE_X, DUST_Y, ROW_Z, "minecraft:stone");
    column.set_block(SOURCE_X, DUST_Y + 1, ROW_Z, &redstone_torch::set_standing_lit(false));
    for x in (SOURCE_X + 1)..=ATTACH_X {
        column.set_block(x, DUST_Y + 1, ROW_Z, &redstone_wire::set_power(0));
    }
    column.set_block(ATTACH_X, DUST_Y, ROW_Z, "minecraft:stone");
    column.set_block(
        TORCH_X,
        DUST_Y,
        ROW_Z,
        &redstone_torch::set_wall_lit(crate::neighbor_update::Direction::East, true),
    );

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(
        &mut column,
        0,
        0,
        SOURCE_X,
        DUST_Y + 1,
        ROW_Z,
        &mut block_ticks,
        CURRENT_TICK,
    );

    let due = block_ticks.drain_due(CURRENT_TICK + 64, 64);
    assert!(
        !due.iter().any(|t| t.pos == (TORCH_X, DUST_Y, ROW_Z) && t.kind == redstone::TICK_TORCH),
        "the side torch at (x={TORCH_X}, y={DUST_Y}, z={ROW_Z}) was scheduled with an UNLIT \
         source, so something other than the source is driving the gate above. entries: {:?}",
        due.iter().map(|t| (t.pos, &t.kind, t.trigger_tick)).collect::<Vec<_>>()
    );
    assert!(
        redstone::torch_lit(column.block_state(TORCH_X, DUST_Y, ROW_Z)),
        "the side torch changed state with no source present"
    );
}

// ---------------------------------------------------------------------------
// The primary input devices, on the same rig and against the same oracle table
//
// `dust_attenuates_exactly_as_the_live_server_does` above proves the dust
// evaluator against a lit torch. It cannot say anything about a lever: until the
// `powered=true` arms landed in `crate::redstone`, `is_signal_source` was
// `torch || diode || observer` and `weak_signal`/`direct_signal` had no arm for
// a lever, button or pressure plate at all — so every one of them emitted
// nothing, and a piston gate driven by a lever scheduled zero commits while the
// identical gate driven by a torch passed.
//
// These arms exist because that gap was invisible to every gate in this file:
// each one places a torch, and a torch is the single source for which the old
// implementation was correct. Same shape as a fixture corpus that shares one
// spawn point.
// ---------------------------------------------------------------------------

/// Lays `source` at `x = 0` and unpowered dust along `x = 1..=15`, all at
/// `z = ROW_Z` — `lay_torch_and_dust`'s source-agnostic twin, so the input
/// families are measured on byte-identical geometry rather than on a rig written
/// to suit them.
fn lay_source_and_dust(column: &mut ChunkColumn, source: &str) {
    column.set_block(0, DUST_Y, ROW_Z, source);
    for x in 1..16 {
        column.set_block(x, DUST_Y, ROW_Z, &redstone_wire::set_power(0));
    }
}

/// Drives the rig from the source position and asserts the resulting dust
/// profile equals [`ORACLE_DUST_ATTENUATION`] at **every** coordinate.
///
/// Mismatches are collected and reported together: an `assert_eq!` inside the
/// loop would name the first bad coordinate and leave the other fourteen as
/// arguments, so a neuter would demonstrate one square rather than the shape of
/// the whole run.
fn assert_source_reproduces_the_oracle_profile(source: &str) {
    let mut column = column_with_floor();
    lay_source_and_dust(&mut column, source);

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 0, DUST_Y, ROW_Z, &mut block_ticks, 0);

    let measured = dust_profile(&column);
    let mut wrong: Vec<String> = Vec::new();
    for (&(distance, oracle_power), &(x, measured_power)) in
        ORACLE_DUST_ATTENUATION.iter().zip(measured.iter())
    {
        assert_eq!(distance, x, "rig misalignment: oracle distance {distance} vs column x {x}");
        if measured_power != oracle_power {
            wrong.push(format!(
                "x={x} ({distance} from the source): ours {measured_power}, live 26.2 {oracle_power}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} dust squares disagree with the live server for source {source:?}:\n  {}\nfull \
         profile: {measured:?}",
        wrong.len(),
        ORACLE_DUST_ATTENUATION.len(),
        wrong.join("\n  ")
    );

    // The two wrong hypotheses this profile separates, restated for this source:
    // an off-by-one decay would give `15 - d`, and a decay-free relay would give
    // a flat 15. Both are ruled out by the equality above, and naming them here
    // is what keeps the assertion a prediction rather than a direction.
    let flat: Vec<(i32, u8)> = (1..16).map(|d| (d, 15u8)).collect();
    let off_by_one: Vec<(i32, u8)> = (1..16).map(|d| (d, u8::try_from(15 - d).unwrap_or(0))).collect();
    assert_ne!(measured, flat, "source {source:?} relays undecayed, matching the FLAT hypothesis");
    assert_ne!(measured, off_by_one, "source {source:?} matches the OFF-BY-ONE hypothesis");
}

/// **A `powered=true` lever drives the dust run to the live server's own
/// profile**, through the production propagation entry point.
///
/// Distance `1` reading 15 is not enough on its own: a source read one block away
/// cannot distinguish a decaying 15 from a flat one. The gate therefore asserts
/// all fifteen coordinates, where the decayed value differs from both the source
/// strength and from zero at fourteen of them.
#[test]
fn a_lever_drives_the_dust_run_exactly_as_the_live_server_does() {
    assert_source_reproduces_the_oracle_profile("minecraft:lever[face=floor,facing=north,powered=true]");
}

/// **The control, and it is the arm that used to fail.** The identical rig with
/// an unpowered lever must leave every square at 0.
///
/// Before the input-source arms landed, *both* of these tests measured this
/// result — a powered lever and an unpowered one were indistinguishable, which
/// is precisely why nothing was red.
#[test]
fn an_unpowered_lever_propagates_nothing() {
    let mut column = column_with_floor();
    lay_source_and_dust(&mut column, "minecraft:lever[face=floor,facing=north,powered=false]");

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 0, DUST_Y, ROW_Z, &mut block_ticks, 0);

    let mut powered: Vec<(i32, u8)> = Vec::new();
    for (x, power) in dust_profile(&column) {
        if power != 0 {
            powered.push((x, power));
        }
    }
    assert!(
        powered.is_empty(),
        "an UNPOWERED lever powered {} dust square(s): {powered:?} — something other than the \
         lever's own `powered` property is driving this run",
        powered.len()
    );
}

/// A pressed button drives the same run — the family that shares
/// `LeverBlock`'s signal shape but reaches it from a scheduled release rather
/// than a toggle, so its `powered=false` state is the one a player sees most of
/// the time.
#[test]
fn a_pressed_button_drives_the_dust_run_exactly_as_the_live_server_does() {
    assert_source_reproduces_the_oracle_profile(
        "minecraft:stone_button[face=floor,facing=north,powered=true]",
    );
}

/// A pressed pressure plate drives the same run.
#[test]
fn a_pressed_pressure_plate_drives_the_dust_run_exactly_as_the_live_server_does() {
    assert_source_reproduces_the_oracle_profile("minecraft:stone_pressure_plate[powered=true]");
}

/// **A `minecraft:redstone_block` drives the same run — and this arm has its own
/// live measurement rather than inheriting the torch's.**
///
/// Reading 1 of the oracle run described in this module's doc comment was a
/// `redstone_block` feeding a 20-long dust run, and it produced a profile
/// byte-identical to the lit torch's. So the expectation here comes from the
/// live server having been pointed at this exact source, not from assuming one
/// source behaves like another.
#[test]
fn a_redstone_block_drives_the_dust_run_exactly_as_the_live_server_does() {
    assert_source_reproduces_the_oracle_profile("minecraft:redstone_block");
}

/// A **weighted** pressure plate at `power=3` drives the run from 3, not from
/// 15 — the analog family, and the one whose value a boolean collapse would get
/// wrong while every gate above stayed green.
///
/// The expected profile is the oracle table shifted: vanilla's attenuation is
/// `source_strength - (d - 1)`, so a strength-3 source gives `[3, 2, 1, 0, ...]`.
/// That expectation is arithmetic applied to the measured rule, not a second
/// reading — and it is discriminating, because the flat hypothesis gives 3
/// everywhere and the boolean hypothesis gives the full 15-long ramp.
#[test]
fn a_weighted_pressure_plate_drives_the_run_from_its_own_analog_power() {
    let mut column = column_with_floor();
    lay_source_and_dust(&mut column, "minecraft:heavy_weighted_pressure_plate[power=3]");

    let mut block_ticks: ScheduledTickQueue<String> = ScheduledTickQueue::new();
    let _ = propagate_and_react(&mut column, 0, 0, 0, DUST_Y, ROW_Z, &mut block_ticks, 0);

    let expected: Vec<(i32, u8)> = (1..16)
        .map(|d| (d, u8::try_from((3 - (d - 1)).max(0)).unwrap_or(0)))
        .collect();
    let measured = dust_profile(&column);

    let mut wrong: Vec<String> = Vec::new();
    for (&(_, want), &(x, got)) in expected.iter().zip(measured.iter()) {
        if got != want {
            wrong.push(format!("x={x}: ours {got}, expected {want}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} square(s) disagree for a strength-3 source:\n  {}\nfull profile: {measured:?}",
        wrong.len(),
        wrong.join("\n  ")
    );

    // The boolean-collapse hypothesis is the full oracle ramp starting at 15.
    // Requiring the measurement to differ from it is what makes this gate about
    // the analog value rather than about propagation.
    let boolean_hypothesis: Vec<(i32, u8)> = ORACLE_DUST_ATTENUATION.to_vec();
    assert_ne!(
        measured, boolean_hypothesis,
        "a power=3 weighted plate drove the same profile as a strength-15 source, so its \
         analog POWER is being read as a boolean"
    );
    assert_eq!(
        measured.first().map(|(_, p)| *p),
        Some(3),
        "the dust adjacent to a strength-3 source must be 3"
    );
}
