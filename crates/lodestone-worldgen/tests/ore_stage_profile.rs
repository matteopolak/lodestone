//! Where the `ore` stage's cycles actually go — the profiling subject DESIGN.md
//! §12.143 left behind when it measured `ore` at **38.7%** of a steady-state
//! column and observed that nothing had ever profiled it.
//!
//! # What it is
//!
//! Two `#[ignore]`d probes, both driven by hand:
//!
//! * [`ore_only_workload`] is a **sampling-profiler subject**, not an assertion.
//!   It warms the store exactly the way
//!   `density_redundancy_probe::stage_share_of_a_steady_state_column` does and then
//!   re-runs `ore_stage` alone over the 100 interior columns of the same 12×12
//!   sweep, so a `samply` profile of it is ~all ore with none of `shape`'s
//!   98,304 `AquiferSystem::block_at` calls on top.
//!
//!   ```text
//!   cargo test --release -p lodestone-worldgen --test ore_stage_profile --no-run
//!   samply record --save-only -o /tmp/ore.json.gz <binary> --ignored --exact \
//!     ore_only_workload
//!   ```
//!
//! * [`ore_event_counts`] reports the *event* counts underneath that profile —
//!   candidate positions, region reads, tag tests, air probes, RNG draws — per
//!   interior column, from [`crate::…`]'s `ore_probe`. Counts rather than a
//!   duration, per DESIGN.md §12.19, and they are what makes a per-event
//!   instruction cost measurable instead of back-derived (§12.143's 40× lesson).
//!
//! # How it works
//!
//! **The warm-up is load-bearing and for the same reason it is in
//! `stage_share_of_a_steady_state_column`.** `ore_stage` for chunk `c` reads the
//! *pre-ore* product of all nine of `c ± 1`; on a cold store those nine pipelines
//! run inside the timed region and the profile becomes a profile of `shape`. The
//! sweep below calls `column()` over `-1..=SIDE` first, which leaves every
//! neighbour's `pre_ore` in the store, so the second pass is ore and nothing else.
//!
//! # How to change it
//!
//! `SIDE`, the seed and the interior definition must stay identical to
//! `benches/generation.rs`'s, or the numbers stop being comparable to `I_ss`.

use lodestone_worldgen::feature::ore_probe;

const SEED: i64 = 42;
const SIDE: i32 = 12;

/// Warms the store over `-1..=SIDE` so a later `ore_stage` call pays only its own
/// cost — see the module doc.
fn warmed() -> lodestone_worldgen::overworld::OverworldGenerator {
    let generator = lodestone_server::overworld_generator(SEED);
    for cz in -1..=SIDE {
        for cx in -1..=SIDE {
            std::hint::black_box(generator.column(cx, cz).non_air_count());
        }
    }
    generator
}

/// A workload that is ~all `ore_stage`, for a sampling profiler to attribute.
///
/// Prints nothing but a shape check; the output is the profile, not stdout.
#[test]
#[ignore = "profiling subject; driven by hand under samply"]
fn ore_only_workload() {
    let generator = warmed();
    let mut acc = 0u64;
    for round in 0..3 {
        for cz in 0..SIDE {
            for cx in 0..SIDE {
                acc += generator.ore_stage_for_profiling(cx, cz);
            }
        }
        println!("round {round} done, acc {acc}");
    }
    assert!(acc > 0, "the ore stage placed nothing — this is a vacuous profile");
}

/// The event counts under that profile, per interior column.
#[test]
#[ignore = "measurement probe; driven by hand"]
fn ore_event_counts() {
    let generator = warmed();

    ore_probe::reset();
    let mut columns = 0u64;
    for cz in 0..SIDE {
        for cx in 0..SIDE {
            if !(cx > 0 && cz > 0 && cx < SIDE - 1 && cz < SIDE - 1) {
                continue;
            }
            std::hint::black_box(generator.ore_stage_for_profiling(cx, cz));
            columns += 1;
        }
    }
    let s = ore_probe::snapshot();
    assert_eq!(columns, 100, "the interior definition drifted from the bench's");
    assert!(
        s.candidates > 0,
        "the probe recorded nothing — this build has no `gen-counters` feature, so \
         every number below would be a vacuous zero"
    );

    let n = columns as f64;
    println!("\n== ore stage events per interior column, {SIDE}x{SIDE} sweep, seed {SEED} ==");
    for (name, v) in s.rows() {
        println!("   {name:<28} {:>14.0}", v as f64 / n);
    }
    println!();
}
