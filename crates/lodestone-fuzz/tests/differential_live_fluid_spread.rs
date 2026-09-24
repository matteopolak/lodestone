//! The differential harness driven end to end against a **live vanilla
//! server**: one action script, our fluid model on one side, real vanilla on
//! the other, block states compared after every tick.
//!
//! This is the comparison the whole `differential` module exists to make.
//! `differential_harness_self_check.rs` proves the comparison *loop* against
//! two fakes; this file proves the loop plus both real oracles plus the tick
//! alignment between them, and it answers a question no in-workspace test
//! can: does our fluid model advance a spreading water front on the same
//! ticks a real 26.2 server does?
//!
//! # Running it
//!
//! ```text
//! ./scripts/live-oracles/creative.sh
//! cargo test -p lodestone-fuzz --features rcon-oracle \
//!     --test differential_live_fluid_spread -- --ignored --nocapture
//! ```
//!
//! `LODESTONE_DIFFERENTIAL_RCON` overrides the endpoint (`IP:port`) for a
//! run against a different live oracle — every oracle script in
//! `scripts/live-oracles/` exposes RCON on its own port with the same
//! password, and the rig below is `/fill`ed from scratch, so any of them
//! works. The default is the flat/creative oracle's own documented endpoint.
//!
//! # The rig, and why it is built rather than found
//!
//! A closed stone channel: floor, roof and both side walls, air along the
//! `+x` axis only. Built identically on both sides, so neither side's
//! terrain, biome or lighting participates. That matters more than it
//! sounds: an open plane lets a source spread in four directions and a
//! nearby drop makes vanilla prefer the direction of the fall, which is not
//! a divergence but reads as one.
//!
//! The rig is built **outside** the script, not as `Action::RunCommand`
//! steps. A `/fill` string means nothing to the in-process oracle, and
//! reproducing vanilla's command grammar there just to build a wall would
//! make that oracle a second command implementation to keep in step.
//!
//! # The external expectation
//!
//! Measured on a live 26.2 server over RCON, twice independently, before any
//! of this code existed: with a water source placed at one end of exactly
//! this channel, cell *N* along the channel first reads as water at
//! 249·*N* ms — one cell per 5 ticks, matching water's own 5-tick spread
//! delay, with real-time alignment good to well under a tick against a
//! 250·*N* ms prediction. The control for that measurement is in the same
//! record: on the same rig, 25 consecutive `/tick freeze` + `/tick step 1`
//! pairs advanced the front zero cells, which is why every timing here is
//! taken in real time.
#![cfg(feature = "rcon-oracle")]

use lodestone_fuzz::differential::fluid::FluidModelOracle;
use lodestone_fuzz::differential::rcon::RconOracle;
use lodestone_fuzz::differential::{Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential};

/// The flat/creative oracle's own documented endpoint and password —
/// `scripts/live-oracles/creative.sh`'s values, not chosen here.
const DEFAULT_ADDR: &str = "127.0.0.1:25571";
const PASSWORD: &str = "lodestone";

const REPAIR: &str = "start a live 26.2 oracle first (./scripts/live-oracles/creative.sh, \
    RCON on 127.0.0.1:25571 password \"lodestone\"), or point \
    LODESTONE_DIFFERENTIAL_RCON at another one";

/// Far from any oracle world's spawn, and above every one of their terrain
/// heights, so the channel is carved out of air rather than out of a build
/// somebody else is using. The chunk is force-loaded before use and released
/// after, because a chunk with nobody near it does not tick and a frozen
/// world is exactly the failure mode that reports "nothing diverged".
///
/// **A fresh coordinate every run, not a fixed constant** — measured
/// directly, and the reason is load-bearing rather than tidiness. The
/// server's own `time query gametime` counter (not wall-clock guessing) says
/// a water source's immediate neighbour wets after exactly 5 real ticks, on
/// every trial, the *first* several times a coordinate is used. Reuse the
/// *same* coordinate across enough runs, though, and it starts wetting after
/// as few as 2 or 3 real ticks, with no contention and a clean, steady
/// real-tick cadence in every case — ruling out the live-oracle-vs-wall-clock
/// alignment failure this file's own harness otherwise guards against. The
/// oracle world persists across container restarts (see
/// `scripts/live-oracles/creative.sh`), and this file's `/fill`-based
/// teardown clears block state but cannot cancel an already-scheduled fluid
/// tick, which does not care that the block under it changed in the
/// meantime — so a coordinate this file has run at enough times accumulates
/// pending ticks from earlier runs that let a *new* placement resolve early,
/// coincidentally. A fixed `ORIGIN` therefore goes stale under exactly the
/// repetition a test suite is supposed to survive. Deriving it from the
/// current time means every run gets coordinates this file has not touched
/// before.
fn origin() -> (i32, i32, i32) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_nanos();
    // Slots spaced `CELLS + 8` blocks apart along x so that even two runs
    // landing in the same nanosecond-modulo slot never overlap corridors;
    // 1,000,000 slots make that collision astronomically unlikely regardless.
    let slot = i32::try_from(nanos % 1_000_000).unwrap_or(0);
    (20_000 + slot * (CELLS + 8), 200, 20_000)
}

/// Channel length in cells past the source. Seven is the reach of a water
/// source on a flat floor (level 1..=7), so nothing here is capped by the
/// fluid's own range mid-comparison — a capped front would make both
/// hypotheses agree again.
const CELLS: i32 = 6;

fn endpoint() -> String {
    std::env::var("LODESTONE_DIFFERENTIAL_RCON").unwrap_or_else(|_| DEFAULT_ADDR.to_owned())
}

fn channel_candidates() -> Vec<String> {
    // Base names only, deliberately: `execute if block` matches on the
    // properties a pattern spells out, so `minecraft:water` covers every
    // flowing level, and the comparison is then about *which cell is wet on
    // which tick* rather than about level bookkeeping. Air is listed so a dry
    // cell answers `Some("minecraft:air")` rather than `None`, keeping
    // "nothing matched" available as a genuine signal that the rig broke.
    vec!["minecraft:water".to_owned(), "minecraft:air".to_owned()]
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    (1..=CELLS).map(|d| ((d, 0, 0), channel_candidates())).collect()
}

/// The one action: a water source at the channel's closed end.
fn script() -> Script {
    Script::new(vec![ScriptStep {
        tick: 0,
        action: Action::SetBlock {
            pos: (0, 0, 0),
            state: "minecraft:water[level=0]".to_owned(),
        },
    }])
}

/// Carves the channel on the vanilla side and force-loads its chunk.
///
/// Takes the same [`RconOracle`] the comparison itself drives, deliberately
/// — **not** a second, separate RCON connection. That used to be two
/// connections (this rig-builder on one, the timed comparison on another),
/// and collapsing them to one was a real, worthwhile simplification: fewer
/// round trips, one place that owns the tick baseline. It was *not*,
/// however, the fix for the early-wetting effect measured during this file's
/// development — later, more careful trials at a single reused coordinate
/// reproduced early wetting on one connection just as well as on two, and a
/// brand-new coordinate gave a clean 5-tick result regardless of connection
/// count. See `origin`'s own doc for the effect that *was* the cause (stale
/// scheduled ticks at a reused coordinate in the persistent oracle world).
/// Routing every command through one [`RconOracle`] (via
/// [`Action::RunCommand`]) still removes a second connection worth removing,
/// it just is not what makes the tick count trustworthy.
fn build_vanilla_rig(vanilla: &mut RconOracle, origin: (i32, i32, i32)) {
    let (ox, oy, oz) = origin;
    let commands = [
        format!("forceload add {ox} {oz} {} {oz}", ox + CELLS + 4),
        format!(
            "fill {} {} {} {} {} {} minecraft:stone",
            ox - 1,
            oy - 1,
            oz - 1,
            ox + CELLS + 2,
            oy + 1,
            oz + 1
        ),
        format!(
            "fill {ox} {oy} {oz} {} {oy} {oz} minecraft:air",
            ox + CELLS + 1
        ),
    ];
    for command in commands {
        vanilla
            .apply(&Action::RunCommand(command.clone()))
            .unwrap_or_else(|e| panic!("`{command}`: {e}. {REPAIR}"));
    }
    // These three round trips are real wall-clock time on `vanilla`'s own
    // connection, taken *before* `RconOracle::connect` had anything to
    // anchor its nominal tick ladder against — re-anchor now, right before
    // the script's first action, so none of that time is folded silently
    // into "tick 0". See `RconOracle::reset_baseline`'s own doc.
    vanilla
        .reset_baseline()
        .unwrap_or_else(|e| panic!("resetting the tick baseline after rig-building: {e}. {REPAIR}"));
}

fn tear_down_vanilla_rig(vanilla: &mut RconOracle, origin: (i32, i32, i32)) {
    let (ox, oy, oz) = origin;
    let _ = vanilla.apply(&Action::RunCommand(format!(
        "fill {} {} {} {} {} {} minecraft:air",
        ox - 1,
        oy - 1,
        oz - 1,
        ox + CELLS + 2,
        oy + 1,
        oz + 1
    )));
    let _ = vanilla.apply(&Action::RunCommand(format!(
        "forceload remove {ox} {oz} {} {oz}",
        ox + CELLS + 4
    )));
}

/// The same channel on our side: a floor one below the script's `y`, plus
/// side walls and a roof, all written without scheduling a fluid tick.
fn build_our_rig(oracle: &mut FluidModelOracle) {
    for d in -1..=CELLS + 2 {
        for dz in [-1, 1] {
            for dy in [0, 1] {
                oracle.place_static((d, dy, dz), "minecraft:stone");
            }
        }
        oracle.place_static((d, 1, 0), "minecraft:stone");
    }
}

/// **The read primitive's own control**, and it is not optional decoration.
///
/// `RconOracle::block_state` answers by probing candidates. A probe that
/// never matches anything makes two oracles agree on every position of every
/// tick, so an `Agreed` outcome from the comparison below is worth nothing
/// unless the probe is known to discriminate. That is not hypothetical: the
/// `execute if block … run say <marker>` form reads plausibly and is
/// measurably useless over RCON, because `say` broadcasts to chat and sends
/// the command source no feedback at all — both the matching and the
/// non-matching case come back as an empty response body.
///
/// So this test asserts both arms against a live server: a position known to
/// hold stone must report stone, and must NOT report water.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn the_rcon_read_primitive_distinguishes_two_known_states() {
    let origin = origin();
    let mut oracle = RconOracle::connect(endpoint(), PASSWORD, origin).expect("connect the oracle");
    build_vanilla_rig(&mut oracle, origin);

    // The floor, one below the channel: stone by construction.
    let floor = (0, -1, 0);
    let stone = vec!["minecraft:stone".to_owned()];
    let water = vec!["minecraft:water".to_owned()];

    let positive = oracle.block_state(floor, &stone).expect("probe the floor for stone");
    let negative = oracle.block_state(floor, &water).expect("probe the floor for water");

    tear_down_vanilla_rig(&mut oracle, origin);

    assert_eq!(
        positive.as_deref(),
        Some("minecraft:stone"),
        "the floor was just filled with stone and the probe did not see it — \
         the read primitive is broken, and every `Agreed` outcome from this \
         harness is vacuous until it is fixed"
    );
    assert_eq!(
        negative, None,
        "the probe reported water at a position holding stone — it is matching \
         unconditionally, which is the same vacuous-agreement failure in the \
         other direction"
    );
}

/// Our fluid model against real vanilla, compared after every tick.
///
/// # History: this used to pin a 4-tick head start
///
/// `ticks_after_edit` used to schedule the edited position *and its six
/// neighbours* one tick later, unconditionally — so a water source's
/// neighbours ran their own fluid tick in the very same drain that first
/// wrote them, and the front was already two cells along after one elapsed
/// tick where vanilla's is still zero cells along and does not reach cell 1
/// until the 5th. That bug is what this test originally pinned. It now
/// seeds only a position that already holds a fluid at edit time, at that
/// fluid's own tick delay — see `lodestone_server::fluid::ticks_after_edit`'s
/// doc — and this test's job is to confirm that fix against the live oracle
/// rather than against this crate's own understanding of itself.
///
/// # Why the outcome is gated on `missed_deadlines`
///
/// A live vanilla server can fall behind its own 20 TPS target under host
/// contention and then burst through several backlogged ticks with no
/// per-tick delay once CPU is available again — measured directly on this
/// rig: [`RconOracle::missed_deadlines`] jumped from 0 to 4 within two
/// `advance_tick` calls on a loaded machine, and the very same rig that
/// reported a divergence at nominal tick 1 had, by the *real* tick counter,
/// already reached the fifth tick the water front needs. That is a
/// real-time alignment failure, not a fluid-model one — the deterministic,
/// live-oracle-free unit test right next to this one
/// (`lodestone_server::fluid::tests::a_water_source_s_neighbour_wets_on_water_s_own_tick_delay`)
/// pins the same position at the same tick count with no live server
/// involved at all, and a hand-driven RCON probe with real timestamps
/// (bypassing every tick-counting assumption this harness makes) measured
/// vanilla's own neighbour at 247 ms, matching the 250 ms / 5-tick
/// prediction. So: trust an `Agreed` verdict only when `missed_deadlines()`
/// is zero, meaning every nominal tick this run reported was backed by
/// exactly one real tick and nothing was bursted past unobserved. A non-zero
/// count means this run's tick labels are not to be trusted either way —
/// rerun once the machine quiets down rather than reading it as a
/// divergence *or* as an agreement.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn our_fluid_model_matches_vanilla_s_water_front() {
    let origin = origin();
    let mut vanilla = RconOracle::connect(endpoint(), PASSWORD, origin).expect("connect the oracle");
    build_vanilla_rig(&mut vanilla, origin);

    let mut ours = FluidModelOracle::new(origin, -1, "minecraft:stone");
    build_our_rig(&mut ours);

    // Enough ticks that vanilla's front would reach the far end of the
    // channel (5 ticks per cell), so an `Agreed` outcome would mean the two
    // agreed over the whole spread and not merely before it started.
    let settle_ticks = u64::try_from(CELLS).expect("small") * 5 + 5;
    let outcome = run_differential(&script(), &region(), &mut ours, &mut vanilla, settle_ticks);

    tear_down_vanilla_rig(&mut vanilla, origin);

    // The instrument, before the system — see this test's own doc. A
    // non-zero count here means the run's tick labels cannot be trusted in
    // either direction, so check it before reading the outcome at all.
    assert_eq!(
        vanilla.missed_deadlines(),
        0,
        "the live oracle fell behind its own tick schedule and then bursted forward {} tick(s) \
         worth, so no tick label from this run means anything — rerun once the machine is \
         quieter rather than trusting this outcome: {outcome:?}",
        vanilla.missed_deadlines()
    );

    match outcome {
        DifferentialOutcome::Agreed => {}
        DifferentialOutcome::Diverged(divergence) => panic!(
            "our fluid model and vanilla first disagree at tick {}, position {:?}: ours {:?}, \
             vanilla {:?}. `missed_deadlines` was 0, so this run's tick labels are trustworthy \
             and this is a real divergence, not a live-oracle timing artifact",
            divergence.tick, divergence.pos, divergence.left, divergence.right
        ),
        DifferentialOutcome::OracleFailed(failure) => {
            panic!("oracle failure rather than a comparison: {failure:?}. {REPAIR}")
        }
    }
}
