//! Our redstone model driven over the two-seam contraption in
//! [`contraption`], asserting the **exact game tick** each probed cell reaches
//! its predicted power.
//!
//! This is the half of the pair that runs with no server: it needs no
//! network, no live oracle and no feature flag, so it is what keeps the
//! numbers `differential_live_redstone_contraption.rs` measured from
//! regressing. The numbers themselves are not derived here — see
//! [`contraption::PREDICTED`] and that file's own measurement.
//!
//! Every assertion is a value at a position at a tick. The three cells were
//! chosen so the plausible wrong models land on different values *and*
//! different ticks (the module doc in `contraption` lists them), which is
//! also why the failure output below names the whole observed timeline: a
//! wrong model is identified by which timeline it produced, not by which
//! single assertion tripped first.

mod contraption;

use lodestone_fuzz::differential::redstone::RedstoneModelOracle;
use lodestone_fuzz::differential::{Action, WorldOracle};

/// Ticks to run past the last predicted arrival, so a model that is merely
/// *late* is distinguishable from one that is on time.
const SETTLE: u64 = 6;

/// Lays out the contraption and places the source, returning, per probed
/// cell, the first tick at which it read its predicted power (`None` if it
/// never did) alongside the whole per-tick power trace for that cell.
fn run() -> Vec<(Option<u64>, Vec<Option<u8>>)> {
    run_with(RedstoneModelOracle::new(
        contraption::origin_on_lane(0),
        contraption::FLOOR_Y,
        contraption::FLOOR_STATE,
    ))
}

fn run_with(mut ours: RedstoneModelOracle) -> Vec<(Option<u64>, Vec<Option<u8>>)> {
    for (pos, state) in contraption::components() {
        ours.place_static(pos, &state);
    }

    // Every power, not just the predicted one: the trace is the diagnostic,
    // and a cell that lands on the wrong value has to be readable as that
    // rather than as a silent `None`.
    let all_powers: Vec<String> = (0..=15)
        .map(|p| format!("minecraft:redstone_wire[power={p}]"))
        .collect();
    let read = |o: &mut RedstoneModelOracle, pos| {
        o.block_state(pos, &all_powers)
            .expect("infallible")
            .and_then(|s| {
                s.trim_end_matches(']')
                    .rsplit_once("power=")
                    .and_then(|(_, n)| n.parse::<u8>().ok())
            })
    };

    let last = contraption::PREDICTED
        .iter()
        .map(|&(_, _, tick)| tick)
        .max()
        .expect("three predictions");

    let mut first_hit: Vec<Option<u64>> = vec![None; contraption::PREDICTED.len()];
    let mut traces: Vec<Vec<Option<u8>>> = vec![Vec::new(); contraption::PREDICTED.len()];

    // The source is placed at tick 0, before any tick is advanced — the same
    // ordering `run_differential` uses for a step at tick 0, so the tick
    // numbers here and there mean the same thing.
    ours.apply(&Action::SetBlock {
        pos: contraption::SOURCE,
        state: contraption::SOURCE_STATE.to_owned(),
    })
    .expect("infallible");

    for (i, &(pos, power, _)) in contraption::PREDICTED.iter().enumerate() {
        let seen = read(&mut ours, pos);
        traces[i].push(seen);
        if seen == Some(power) {
            first_hit[i] = Some(0);
        }
    }
    for tick in 1..=last + SETTLE {
        ours.advance_tick().expect("infallible");
        for (i, &(pos, power, _)) in contraption::PREDICTED.iter().enumerate() {
            let seen = read(&mut ours, pos);
            traces[i].push(seen);
            if seen == Some(power) && first_hit[i].is_none() {
                first_hit[i] = Some(tick);
            }
        }
    }
    first_hit.into_iter().zip(traces).collect()
}

/// **The gate.** Each probed cell reaches its predicted power on its
/// predicted tick, and not before.
#[test]
fn each_probed_cell_reaches_its_predicted_power_on_its_predicted_tick() {
    let observed = run();
    let mut wrong = Vec::new();
    for (i, &(pos, power, tick)) in contraption::PREDICTED.iter().enumerate() {
        let (first, trace) = &observed[i];
        if *first != Some(tick) {
            wrong.push(format!(
                "cell {pos:?}: predicted power {power} on tick {tick}, first reached it on \
                 {first:?}\n    trace by tick: {trace:?}"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} probed cells disagree with the live-measured timeline:\n  {}",
        wrong.len(),
        contraption::PREDICTED.len(),
        wrong.join("\n  ")
    );
}

/// **The control for the gate above**, and it is not decoration: the gate is
/// an equality against a timeline, and an equality passes just as happily
/// against a rig that never carries a signal at all if the predicted powers
/// were also zero. They are not — so this asserts the negative half
/// explicitly, that each cell was unpowered on the tick *before* its
/// prediction.
///
/// The wrong models this separates are named in `contraption`'s module doc.
/// Two of them are timelines that arrive *earlier* (1/5/7 for a repeater
/// delay read as `delay` rather than `2 · delay`, 1/2/3 for the flat on-place
/// delay), so "not powered one tick early" is exactly the arm that fails for
/// them while the equality above might still be reached late.
#[test]
fn no_probed_cell_is_powered_on_the_tick_before_its_prediction() {
    let observed = run();
    for (i, &(pos, _, tick)) in contraption::PREDICTED.iter().enumerate() {
        let (_, trace) = &observed[i];
        // Cell 1 is fed in the placement itself, so there is no earlier tick
        // to be unpowered on — its own arrival tick is 0.
        if tick == 0 {
            continue;
        }
        let before = usize::try_from(tick - 1).expect("small tick");
        assert_eq!(
            trace[before],
            Some(0),
            "cell {pos:?} was already at {:?} on tick {} — one tick before its predicted \
             arrival on {tick}. trace by tick: {trace:?}",
            trace[before],
            tick - 1
        );
    }
}

/// **The watched failure.** The same layout, the same script and the same
/// probes, against an oracle whose world reports no neighbouring column
/// resident — the single-column reach this work replaced. Every probed cell
/// then stays at power 0 forever, including cell 1, which the source touches
/// on the row's very first hop.
///
/// Run and observed to fail the gate above, not merely described: the two
/// assertions here are the gate's own two conditions, negated. Without this,
/// "the signal crosses both seams" would rest on an equality that a rig
/// carrying no signal at all could also satisfy if the predictions happened
/// to be zero.
#[test]
fn a_model_with_no_cross_column_reach_never_powers_any_probed_cell() {
    let observed = run_with(RedstoneModelOracle::without_neighbours(
        contraption::origin_on_lane(0),
        contraption::FLOOR_Y,
        contraption::FLOOR_STATE,
    ));
    for (i, &(pos, power, tick)) in contraption::PREDICTED.iter().enumerate() {
        let (first, trace) = &observed[i];
        assert_eq!(
            *first, None,
            "cell {pos:?} reached its predicted power {power} on tick {first:?} with no \
             cross-column reach at all — the control cannot detect the very thing the gate \
             asserts, so the gate's own pass is worthless. Predicted tick was {tick}. \
             trace by tick: {trace:?}"
        );
        assert!(
            trace.iter().all(|seen| *seen == Some(0)),
            "cell {pos:?} carried some power without cross-column reach: trace by tick {trace:?}"
        );
    }
}
