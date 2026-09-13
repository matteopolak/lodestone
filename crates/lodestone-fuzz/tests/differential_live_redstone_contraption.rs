//! The two-seam redstone contraption run against a **live vanilla server**:
//! one action, our redstone model on one side, real vanilla on the other,
//! three cells compared after every tick.
//!
//! `differential_live_fluid_spread.rs` is the same shape over the fluid
//! model, and this file deliberately reuses its rig conventions. What it adds
//! is a comparison whose subject is *ordering in time*: the fluid script
//! diverges on the first tick, so alignment barely mattered there, while a
//! repeater chain is nothing but alignment.
//!
//! # Running it
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-fuzz --features rcon-oracle \
//!     --test differential_live_redstone_contraption -- --ignored --nocapture
//! ```
//!
//! `LODESTONE_DIFFERENTIAL_RCON` overrides the endpoint (`IP:port`). The rig
//! is built from scratch with `/fill` and `/setblock`, so any oracle world
//! works; the coordinates are disjoint from the fluid rig's, so both can run
//! against one live server.
//!
//! # Where the expected values come from
//!
//! Not from this workspace. Two independent outside sources, and they answer
//! different halves:
//!
//! * **Powers.** The dust attenuation table this rig's decayed cell is
//!   predicted from was probed cell by cell on a live 26.2 server with
//!   `execute if block <pos> minecraft:redstone_wire[power=N]`, and is
//!   recorded in `crates/lodestone-server/src/redstone_oracle_gate.rs`.
//! * **Ticks.** [`vanilla_powers_the_far_cell_fourteen_game_ticks_after_the_source`]
//!   below measures the arrival tick against the live server's **own** tick
//!   counter rather than against elapsed wall time, which is the part that
//!   makes it a number rather than an estimate.
//!
//! # The alignment budget, stated rather than assumed
//!
//! Our side steps exactly. The RCON side cannot: it has no usable
//! single-tick primitive (see `lodestone_fuzz::differential`'s module doc for
//! the measurement — 25 `/tick freeze` + `/tick step 1` pairs advanced a
//! water front zero cells), so it sleeps one tick interval and lets the
//! server's own loop advance. Each probe is a round trip on top of that
//! sleep, so the harness's tick counter drifts *behind* the server's over a
//! long script.
//!
//! That is why the comparison below probes three cells with two candidates
//! each and not sixteen: the drift is proportional to the probe count. And it
//! is why the two tests split the way they do — the differential answers
//! "does the signal arrive at all, at these values, at these places, in this
//! order", which drift cannot fake, and the gametime measurement answers
//! "on which tick", against a clock that is not the harness's.
#![cfg(feature = "rcon-oracle")]

mod contraption;

use std::time::{Duration, Instant};

use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::redstone::RedstoneModelOracle;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use lodestone_testsupport::RconClient;

/// The flat/creative oracle's own documented endpoint and password —
/// `scripts/live-oracles/creative.sh`'s values, not chosen here.
const DEFAULT_ADDR: &str = "127.0.0.1:25571";
const PASSWORD: &str = "lodestone";

const REPAIR: &str = "start a live 26.2 oracle first (./scripts/live-oracles/creative.sh, \
    RCON on 127.0.0.1:25571 password \"lodestone\"), or point \
    LODESTONE_DIFFERENTIAL_RCON at another one";

fn endpoint() -> String {
    std::env::var("LODESTONE_DIFFERENTIAL_RCON").unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
}

fn connect() -> RconClient {
    let addr = endpoint();
    RconClient::connect(&addr, PASSWORD).unwrap_or_else(|e| panic!("connect to {addr}: {e}. {REPAIR}"))
}

/// Clears a box around the row, lays a stone floor under it, and force-loads
/// the three chunks the row spans.
///
/// Force-loading is not tidiness: a chunk with nobody near it does not tick,
/// and a world that is not ticking is exactly the failure mode that reports
/// "nothing diverged".
fn build_vanilla_rig(client: &mut RconClient, origin: (i32, i32, i32)) {
    let (ox, oy, oz) = origin;
    let last = ox + contraption::LAST_CELL;
    let mut commands = vec![
        format!("forceload add {ox} {oz} {} {oz}", last + 2),
        format!(
            "fill {} {} {} {} {} {} minecraft:air",
            ox - 1,
            oy + contraption::ROW_Y,
            oz - 1,
            last + 1,
            oy + contraption::ROW_Y + 1,
            oz + 1
        ),
        format!(
            "fill {} {} {} {} {} {} {}",
            ox - 1,
            oy + contraption::FLOOR_Y,
            oz - 1,
            last + 1,
            oy + contraption::FLOOR_Y,
            oz + 1,
            contraption::FLOOR_STATE
        ),
    ];
    for ((dx, dy, dz), state) in contraption::components() {
        commands.push(format!("setblock {} {} {} {state}", ox + dx, oy + dy, oz + dz));
    }
    for command in commands {
        client
            .command(&command)
            .unwrap_or_else(|e| panic!("`{command}`: {e}. {REPAIR}"));
    }
    // Settle before anything energises the row, and settle by *checking*
    // rather than by sleeping — see [`settle_until_quiescent`], which also
    // records why a plain sleep is not enough.
    settle_until_quiescent(client, origin);

    // Then hand back mid-tick, so the caller's 50 ms sleeps sample the
    // server's ticks away from their boundaries.
    wait_for_tick_edge(client);
}

/// Blocks until the row has been demonstrably inert for twelve consecutive
/// server ticks, and panics if it never is.
///
/// # Why this is a check and not a sleep
///
/// Tearing a circuit down to air does not retract the block ticks its
/// components had already scheduled at those coordinates, so a rebuild — a
/// second run of this file, or another test on this lane — drops fresh
/// repeaters onto a queue that still holds stale entries for them.
///
/// The reason that is *destructive* rather than merely untidy is the shape of
/// a repeater's own scheduled-tick rule: an unpowered repeater that is ticked
/// turns **on**, unconditionally, and only then schedules the tick that will
/// turn it back off `2 · delay` later. That is deliberate in the real engine
/// — it is what shortens a too-short pulse into a full-length one — but it
/// means a stale entry does not expire harmlessly. It fires a spurious pulse.
/// A comparison started during one reads a circuit that is already carrying a
/// signal, and reports the far cell powered on its very first tick, which no
/// correct model of this row can produce.
///
/// A fixed sleep was tried first and is not enough: ten ticks of it left the
/// same failure appearing in four runs out of five, because the stale entries
/// do not fire until the chunk itself starts ticking and the pulse they start
/// outlives the rest of the wait. Twelve consecutive quiet ticks is a
/// positive observation instead — longer than the longest pulse this row can
/// hold (`delay=4`, eight ticks).
fn settle_until_quiescent(client: &mut RconClient, origin: (i32, i32, i32)) {
    const REQUIRED_QUIET_TICKS: u32 = 12;
    let (ox, oy, oz) = origin;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut quiet_ticks = 0;
    while quiet_ticks < REQUIRED_QUIET_TICKS {
        advance_one_tick(client);
        let quiet = contraption::REPEATER_CELLS.iter().all(|&dx| {
            let response = client
                .command(&format!(
                    "execute if block {} {} {} minecraft:repeater[powered=false]",
                    ox + dx,
                    oy + contraption::ROW_Y,
                    oz
                ))
                .unwrap_or_else(|e| panic!("probe a repeater: {e}. {REPAIR}"));
            response.trim().starts_with("Test passed")
        });
        quiet_ticks = if quiet { quiet_ticks + 1 } else { 0 };
        assert!(
            Instant::now() < deadline,
            "the row never went quiet for {REQUIRED_QUIET_TICKS} consecutive ticks — something \
             is still driving it, and a comparison started now would read a circuit that is \
             already carrying a signal. {REPAIR}"
        );
    }
}

/// Blocks until the live server's own tick counter advances.
fn advance_one_tick(client: &mut RconClient) {
    let start = gametime(client);
    let deadline = Instant::now() + Duration::from_secs(5);
    while gametime(client) == start {
        assert!(
            Instant::now() < deadline,
            "gametime never advanced — the world is not ticking, which is the failure mode \
             that reports \"nothing diverged\". Check this oracle world's \
             pause-when-empty-seconds. {REPAIR}"
        );
    }
}

/// Blocks until the live server's own tick counter advances, then a further
/// **half** a tick.
///
/// The half tick is the load-bearing part. Returning exactly on a boundary
/// leaves the caller's 50 ms sleeps landing on subsequent boundaries, where a
/// millisecond of jitter decides whether a probe sees `k` or `k + 1` game
/// ticks — and every tick label in the comparison shifts with it. That was
/// observed directly: the same test alternated between agreeing and reporting
/// a lead for the server, and three extra round trips before the run were
/// enough to flip it. Handing back mid-tick puts a 25 ms margin on either
/// side of every sample instead.
fn wait_for_tick_edge(client: &mut RconClient) {
    advance_one_tick(client);
    std::thread::sleep(lodestone_fuzz::differential::TICK_MILLIS / 2);
}

fn clear_vanilla_source(client: &mut RconClient, origin: (i32, i32, i32)) {
    let (ox, oy, oz) = origin;
    let (dx, dy, dz) = contraption::SOURCE;
    let _ = client.command(&format!("setblock {} {} {} minecraft:air", ox + dx, oy + dy, oz + dz));
}

fn tear_down_vanilla_rig(client: &mut RconClient, origin: (i32, i32, i32)) {
    let (ox, oy, oz) = origin;
    let last = ox + contraption::LAST_CELL;
    let _ = client.command(&format!(
        "fill {} {} {} {} {} {} minecraft:air",
        ox - 1,
        oy + contraption::FLOOR_Y,
        oz - 1,
        last + 1,
        oy + contraption::ROW_Y + 1,
        oz + 1
    ));
    let _ = client.command(&format!("forceload remove {ox} {oz} {} {oz}", last + 2));
}

/// Lays the contraption out on our side, without firing it.
fn build_our_rig(oracle: &mut RedstoneModelOracle) {
    for (pos, state) in contraption::components() {
        oracle.place_static(pos, &state);
    }
}

/// The one action: the source at the row's closed end.
fn script() -> Script {
    Script::new(vec![ScriptStep {
        tick: 0,
        action: Action::SetBlock {
            pos: contraption::SOURCE,
            state: contraption::SOURCE_STATE.to_owned(),
        },
    }])
}

/// **The read primitive's own control, for a state *property*.**
///
/// The fluid file's control proves the probe discriminates two different
/// blocks. That is not the capability this file needs. Every probe here reads
/// the same block — dust — and differs only in a numeric property, so what
/// has to be established is that `execute if block` discriminates on
/// `power=N` rather than matching any dust. A probe that matched the base
/// name would report the predicted power at every cell on every tick, and the
/// comparison would agree unconditionally: the same vacuous agreement the
/// `… run say <marker>` form produced, arriving by a different route.
///
/// Both arms are asserted, against a cell whose power is known by
/// construction: the dust directly beside the source reads 15, and must not
/// read 0.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn the_rcon_read_primitive_discriminates_a_dust_power_level() {
    let origin = contraption::origin_on_lane(1);
    let mut client = connect();
    build_vanilla_rig(&mut client, origin);
    let (ox, oy, oz) = origin;
    let (sx, sy, sz) = contraption::SOURCE;
    client
        .command(&format!(
            "setblock {} {} {} {}",
            ox + sx,
            oy + sy,
            oz + sz,
            contraption::SOURCE_STATE
        ))
        .expect("place the source");

    let mut oracle = RconOracle::connect(endpoint(), PASSWORD, origin).expect("connect the oracle");

    let cell = (1, contraption::ROW_Y, 0);
    let powered = vec!["minecraft:redstone_wire[power=15]".to_owned()];
    let unpowered = vec!["minecraft:redstone_wire[power=0]".to_owned()];
    let any_dust = vec!["minecraft:redstone_wire".to_owned()];

    let positive = oracle.block_state(cell, &powered).expect("probe for power 15");
    let negative = oracle.block_state(cell, &unpowered).expect("probe for power 0");
    let base = oracle.block_state(cell, &any_dust).expect("probe for any dust");

    clear_vanilla_source(&mut client, origin);
    tear_down_vanilla_rig(&mut client, origin);

    assert_eq!(
        base.as_deref(),
        Some("minecraft:redstone_wire"),
        "the probe cannot even see dust at a cell that holds it — the rig did not build, and \
         every outcome from this file is meaningless until it does"
    );
    assert_eq!(
        positive.as_deref(),
        Some("minecraft:redstone_wire[power=15]"),
        "dust directly beside a redstone block did not read power 15 — the probe does not \
         discriminate on the property, so every `Agreed` outcome here would be vacuous"
    );
    assert_eq!(
        negative, None,
        "the probe reported power 0 at a cell reading power 15 — it is matching on the base \
         name alone, which is the same vacuous-agreement failure in the other direction"
    );
}

/// **The external tick number**, measured against the live server's own tick
/// counter rather than against wall time or against the harness's sleep.
///
/// `time query gametime` is a real monotonic tick counter, which is what makes
/// this a measurement rather than an estimate: the harness's own tick label
/// drifts behind the server once probes are added to the sleep, and elapsed
/// milliseconds only recover a tick count if the server is exactly on 20 TPS.
///
/// The predicted value is **14**, and the two plausible wrong models land far
/// from it: reading a repeater's delay as `delay` game ticks rather than
/// `2 · delay` gives 7, and reading the flat one-tick on-place delay gives 3.
/// A window of ±1 around 14 therefore separates all three, and the window is
/// needed for one honest reason: the poll that notices the arrival can land in
/// the tick after it.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn vanilla_powers_the_far_cell_fourteen_game_ticks_after_the_source() {
    let origin = contraption::origin_on_lane(2);
    let mut client = connect();
    build_vanilla_rig(&mut client, origin);
    let (ox, oy, oz) = origin;
    let (fx, fy, fz) = contraption::PREDICTED[2].0;
    let far = (ox + fx, oy + fy, oz + fz);
    let far_power = contraption::PREDICTED[2].1;

    // Start on a tick edge, so the anchor is not read halfway through one.
    wait_for_tick_edge(&mut client);
    let anchor = gametime(&mut client);

    let (sx, sy, sz) = contraption::SOURCE;
    client
        .command(&format!(
            "setblock {} {} {} {}",
            ox + sx,
            oy + sy,
            oz + sz,
            contraption::SOURCE_STATE
        ))
        .expect("place the source");

    let deadline = Instant::now() + Duration::from_secs(10);
    let arrival = loop {
        let response = client
            .command(&format!(
                "execute if block {} {} {} minecraft:redstone_wire[power={far_power}]",
                far.0, far.1, far.2
            ))
            .expect("probe the far cell");
        if response.trim().starts_with("Test passed") {
            break gametime(&mut client);
        }
        assert!(
            response.trim().starts_with("Test failed"),
            "`execute if block` answered {response:?}, which is neither `Test passed` nor \
             `Test failed` — an oracle failure, not a non-match"
        );
        assert!(
            Instant::now() < deadline,
            "the far cell never reached power {far_power}. Either the signal does not cross the \
             row on a real server (which would make the whole prediction wrong, not just the \
             timing) or the rig did not build. {REPAIR}"
        );
    };

    clear_vanilla_source(&mut client, origin);
    tear_down_vanilla_rig(&mut client, origin);

    let delta = arrival - anchor;
    let predicted = i64::try_from(contraption::PREDICTED[2].2).expect("small tick");
    assert!(
        (predicted - 1..=predicted + 1).contains(&delta),
        "the far cell reached power {far_power} {delta} game ticks after the source, not the \
         predicted {predicted}. 7 would mean a repeater's delay is `delay` game ticks rather \
         than `2 · delay`; 3 would mean the flat on-place delay; a much larger number means the \
         server was not keeping up and this measurement should be retaken"
    );
}

/// Reads the live server's own tick counter.
fn gametime(client: &mut RconClient) -> i64 {
    let response = client
        .command("time query gametime")
        .unwrap_or_else(|e| panic!("`time query gametime`: {e}. {REPAIR}"));
    response
        .split_whitespace()
        .filter_map(|token| token.trim_end_matches('.').parse::<i64>().ok())
        .next_back()
        .unwrap_or_else(|| {
            panic!("`time query gametime` answered {response:?}, with no tick count in it. {REPAIR}")
        })
}

/// **Our redstone model against real vanilla, compared after every tick.**
///
/// The three probed cells carry the prediction in their own candidate
/// alphabets (see `contraption::candidates`): the predicted power and an
/// unpowered one, and nothing else. So a side that writes any *other* power
/// answers `None` and diverges at that position, rather than matching some
/// looser pattern and agreeing.
///
/// Read `Agreed` here as: across the whole run, both sides left each of the
/// three cells unpowered until the signal reached it, and then both wrote the
/// same predicted power there — cell 1 one hop past the first seam, cell 16
/// seven cells of decay later, cell 18 one hop past the second seam. A
/// divergence names the tick and the position, which is the point of running
/// per tick rather than comparing settled states: three of the four wrong
/// models this rig separates produce the *same* settled state and differ only
/// in when each cell got there.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn our_redstone_crosses_two_chunk_seams_as_vanilla_does() {
    let origin = contraption::origin_on_lane(0);
    let mut client = connect();
    build_vanilla_rig(&mut client, origin);

    let mut vanilla = RconOracle::connect(endpoint(), PASSWORD, origin).expect("connect the oracle");
    let mut ours = RedstoneModelOracle::new(
        origin,
        contraption::FLOOR_Y,
        contraption::FLOOR_STATE,
    );
    build_our_rig(&mut ours);

    // Past the last predicted arrival, so agreement covers the whole
    // propagation and not merely the part before it started.
    let settle_ticks = contraption::PREDICTED
        .iter()
        .map(|&(_, _, tick)| tick)
        .max()
        .expect("three predictions")
        + 6;
    let outcome = run_differential(&script(), &contraption::region(), &mut ours, &mut vanilla, settle_ticks);

    clear_vanilla_source(&mut client, origin);
    tear_down_vanilla_rig(&mut client, origin);

    // The instrument, before the system. Every tick label below is only worth
    // as much as the real-time side's alignment, and that alignment fails
    // silently — as a plausible-looking timing divergence — the moment one
    // tick's probes cost more than a tick.
    assert_eq!(
        vanilla.missed_deadlines(),
        0,
        "the real-time side fell behind its own tick schedule {} times, so it advanced fewer \
         harness ticks than the server advanced game ticks and no tick label from this run \
         means anything. Probe fewer positions or fewer candidates per position",
        vanilla.missed_deadlines()
    );

    match outcome {
        DifferentialOutcome::Agreed => {}
        DifferentialOutcome::Diverged(divergence) => panic!(
            "our redstone model and vanilla first disagree on tick {} at relative position \
             {:?}: ours {:?}, vanilla {:?}. A `None` on either side means that side wrote a \
             power this rig's alphabet does not predict — see `contraption::candidates`",
            divergence.tick, divergence.pos, divergence.left, divergence.right
        ),
        DifferentialOutcome::OracleFailed(failure) => {
            panic!("oracle failure rather than a comparison: {failure:?}. {REPAIR}")
        }
    }
}

/// **The comparison's own watched failure**, against the same live server.
///
/// `our_redstone_crosses_two_chunk_seams_as_vanilla_does` asserts agreement,
/// and an agreement is only evidence if a disagreement was reachable. So this
/// runs the identical script and region against a model whose world reports
/// no neighbouring column resident — the single-column reach this work
/// replaced — and requires the comparison to catch it, on the first tick, at
/// the first cell past the first seam.
///
/// It names the position and the tick rather than just "diverged", because
/// that is the property being checked: a comparison that noticed only at the
/// end of the run would be no use for locating an ordering bug.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn a_model_with_no_cross_column_reach_is_caught_on_the_first_tick() {
    let origin = contraption::origin_on_lane(3);
    let mut client = connect();
    build_vanilla_rig(&mut client, origin);

    let mut vanilla = RconOracle::connect(endpoint(), PASSWORD, origin).expect("connect the oracle");
    let mut ours = RedstoneModelOracle::without_neighbours(
        origin,
        contraption::FLOOR_Y,
        contraption::FLOOR_STATE,
    );
    build_our_rig(&mut ours);

    let outcome = run_differential(&script(), &contraption::region(), &mut ours, &mut vanilla, 2);

    clear_vanilla_source(&mut client, origin);
    tear_down_vanilla_rig(&mut client, origin);

    match outcome {
        DifferentialOutcome::Diverged(divergence) => {
            assert_eq!(
                (divergence.tick, divergence.pos),
                (0, contraption::PREDICTED[0].0),
                "expected the miss on the first tick at the first cell past the first seam — \
                 got {divergence:?}"
            );
            assert_eq!(
                divergence.left.as_deref(),
                Some("minecraft:redstone_wire[power=0]"),
                "the no-reach model should have left this cell unpowered"
            );
            assert_eq!(
                divergence.right.as_deref(),
                Some("minecraft:redstone_wire[power=15]"),
                "vanilla powers the cell directly beside the source in the same tick"
            );
        }
        DifferentialOutcome::Agreed => panic!(
            "a model that cannot propagate past a chunk boundary agreed with a real server on a \
             row that crosses two of them. The comparison is not detecting anything, so the \
             agreement its sibling test asserts is worth nothing"
        ),
        DifferentialOutcome::OracleFailed(failure) => {
            panic!("oracle failure rather than a comparison: {failure:?}. {REPAIR}")
        }
    }
}
