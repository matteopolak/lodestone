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
//! `LODESTONE_DIFFERENTIAL_RCON` overrides the endpoint (`host:port`) for a
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
use lodestone_testsupport::RconClient;

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
const ORIGIN: (i32, i32, i32) = (5200, 120, 5200);

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

fn connect() -> RconClient {
    let addr = endpoint();
    RconClient::connect(&addr, PASSWORD).unwrap_or_else(|e| panic!("connect to {addr}: {e}. {REPAIR}"))
}

/// Carves the channel on the vanilla side and force-loads its chunk.
fn build_vanilla_rig(client: &mut RconClient) {
    let (ox, oy, oz) = ORIGIN;
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
        client
            .command(&command)
            .unwrap_or_else(|e| panic!("`{command}`: {e}. {REPAIR}"));
    }
}

fn tear_down_vanilla_rig(client: &mut RconClient) {
    let (ox, oy, oz) = ORIGIN;
    let _ = client.command(&format!(
        "fill {} {} {} {} {} {} minecraft:air",
        ox - 1,
        oy - 1,
        oz - 1,
        ox + CELLS + 2,
        oy + 1,
        oz + 1
    ));
    let _ = client.command(&format!("forceload remove {ox} {oz} {} {oz}", ox + CELLS + 4));
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
    let mut client = connect();
    build_vanilla_rig(&mut client);

    let mut oracle = RconOracle::connect(endpoint(), PASSWORD, ORIGIN).expect("connect the oracle");

    // The floor, one below the channel: stone by construction.
    let floor = (0, -1, 0);
    let stone = vec!["minecraft:stone".to_owned()];
    let water = vec!["minecraft:water".to_owned()];

    let positive = oracle.block_state(floor, &stone).expect("probe the floor for stone");
    let negative = oracle.block_state(floor, &water).expect("probe the floor for water");

    tear_down_vanilla_rig(&mut client);

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
/// # What this pins, and why it is an assertion rather than a report
///
/// The two disagree, and they disagree on the **first** tick: our
/// `ticks_after_edit` schedules the edited position *and its six neighbours*
/// one tick later, so a water source's neighbours run their own fluid tick in
/// the very same drain that first wrote them. The front is therefore already
/// two cells along after one elapsed tick, where the measured vanilla front
/// is still zero cells along and does not reach cell 1 until the 5th.
///
/// That head start is known and documented at its cause (that function's own
/// doc calls it "one cell of flow starting four ticks early once"); what the
/// live comparison adds is the external number it is early *against*, and
/// that it is two cells rather than one. Pinning it as an assertion rather
/// than printing it keeps it a tracked divergence: when the seeding is
/// corrected, this test fails and says so, which is the signal that the
/// harness's first real finding has been closed.
#[test]
#[ignore = "needs a live vanilla 26.2 RCON oracle"]
fn our_fluid_model_starts_a_water_front_four_ticks_ahead_of_vanilla() {
    let mut client = connect();
    build_vanilla_rig(&mut client);

    let mut vanilla = RconOracle::connect(endpoint(), PASSWORD, ORIGIN).expect("connect the oracle");
    let mut ours = FluidModelOracle::new(ORIGIN, -1, "minecraft:stone");
    build_our_rig(&mut ours);

    // Enough ticks that vanilla's front would reach the far end of the
    // channel (5 ticks per cell), so an `Agreed` outcome would mean the two
    // agreed over the whole spread and not merely before it started.
    let settle_ticks = u64::try_from(CELLS).expect("small") * 5 + 5;
    let outcome = run_differential(&script(), &region(), &mut ours, &mut vanilla, settle_ticks);

    tear_down_vanilla_rig(&mut client);

    match outcome {
        DifferentialOutcome::Diverged(divergence) => {
            // `Divergence::tick` is the 0-based index of the tick that had
            // just been run when the comparison failed, so `0` here means
            // "after exactly one elapsed tick" — the first tick either side
            // ran at all. Predicted as `1` while writing this and measured as
            // `0`; the physics reasoning was right and the loop's own
            // numbering is what was off, which is recorded because a
            // plausible off-by-one in a tick label is exactly the kind of
            // thing that later gets mistaken for a real timing difference.
            assert_eq!(
                (divergence.tick, divergence.pos),
                (0, (1, 0, 0)),
                "expected the first divergence one elapsed tick in, one cell along — got {divergence:?}"
            );
            assert_eq!(
                divergence.left.as_deref(),
                Some("minecraft:water"),
                "our side, after one elapsed tick"
            );
            assert_eq!(
                divergence.right.as_deref(),
                Some("minecraft:air"),
                "vanilla's side, after one elapsed tick — its front does not reach this cell \
                 until the 5th, per the measurement in this file's own doc"
            );
        }
        DifferentialOutcome::Agreed => panic!(
            "the two sides agreed, which contradicts a measured 4-tick head start on our \
             side. Either the seeding was corrected (delete this test's expectation and \
             assert agreement instead) or the rig is not ticking — check that the oracle \
             world has pause-when-empty-seconds=0"
        ),
        DifferentialOutcome::OracleFailed(failure) => {
            panic!("oracle failure rather than a comparison: {failure:?}. {REPAIR}")
        }
    }
}
