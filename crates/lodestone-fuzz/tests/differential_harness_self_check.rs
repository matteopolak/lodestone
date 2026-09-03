//! Hermetic proof that `lodestone_fuzz::differential`'s comparison loop itself
//! is correct — no server, no network, no container, runs in milliseconds.
//!
//! The suggested order puts "prove the harness agrees on a script
//! with no known divergence" before ever adding randomness or a real oracle.
//! This file is that proof, done twice: once with two identical fake worlds
//! (must agree at every tick), and once with two fakes that are made to
//! disagree at a *known* tick (must be caught at exactly that tick, not
//! merely "eventually") — the tick-localisation property the whole module
//! exists for, and the negative control CLAUDE.md's evidence rules require
//! before trusting the positive result: an `Agreed` outcome from a harness
//! that can never report `Diverged` would be exactly the vacuous-control
//! species CLAUDE.md warns about, so this file's second test is not optional
//! decoration.

use lodestone_fuzz::differential::{Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential};
use std::collections::HashMap;

/// The simplest possible [`WorldOracle`]: a `HashMap` of positions to state
/// strings, with `advance_tick` an explicit hook a test can use to inject a
/// state change that a real server's own tick loop would otherwise cause —
/// standing in for "physics happened" without needing physics.
#[derive(Default)]
struct FakeWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    tick: u64,
    /// Applied once, when `advance_tick` crosses `at`: sets `pos` to `state`,
    /// simulating a delayed reaction (a torch's `TOGGLE_DELAY`, water
    /// spreading) that only this side experiences — the deliberate
    /// divergence the second test needs.
    scripted_reaction: Option<(u64, (i32, i32, i32), String)>,
}

impl WorldOracle for FakeWorld {
    type Error = std::convert::Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        match action {
            Action::SetBlock { pos, state } => {
                self.blocks.insert(*pos, state.clone());
            }
            Action::RunCommand(_) => {
                // Not supported by this fake; no test below uses it.
            }
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.tick += 1;
        if let Some((at, pos, ref state)) = self.scripted_reaction {
            if self.tick == at {
                self.blocks.insert(pos, state.clone());
            }
        }
        Ok(())
    }

    fn block_state(&mut self, pos: (i32, i32, i32), candidates: &[String]) -> Result<Option<String>, Self::Error> {
        let Some(actual) = self.blocks.get(&pos) else {
            return Ok(None);
        };
        Ok(candidates.iter().find(|c| *c == actual).cloned())
    }
}

fn placing_and_breaking_script() -> Script {
    Script::new(vec![
        ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: (0, 0, 0),
                state: "minecraft:stone".to_owned(),
            },
        },
        ScriptStep {
            tick: 3,
            action: Action::SetBlock {
                pos: (0, 0, 0),
                state: "minecraft:air".to_owned(),
            },
        },
        ScriptStep {
            tick: 5,
            action: Action::SetBlock {
                pos: (1, 0, 0),
                state: "minecraft:water[level=0]".to_owned(),
            },
        },
    ])
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    vec![
        (
            (0, 0, 0),
            vec!["minecraft:stone".to_owned(), "minecraft:air".to_owned()],
        ),
        (
            (1, 0, 0),
            vec!["minecraft:air".to_owned(), "minecraft:water[level=0]".to_owned()],
        ),
    ]
}

#[test]
fn identical_fake_worlds_never_diverge() {
    let script = placing_and_breaking_script();
    let mut left = FakeWorld::default();
    let mut right = FakeWorld::default();

    let outcome = run_differential(&script, &region(), &mut left, &mut right, 2);

    match outcome {
        DifferentialOutcome::Agreed => {}
        other => panic!("two identical fake worlds must agree at every tick, got {other:?}"),
    }
}

/// The negative control: `right` experiences a scripted reaction at tick 4
/// that `left` never gets (nothing in `placing_and_breaking_script` sets
/// `(0, 0, 0)` back to stone), so the two sides must disagree from tick 4
/// onward — and `run_differential` must report **exactly** tick 4, not tick 5
/// (when the region is next touched by the script) and not "some later tick
/// during settle". This is what proves the harness can actually see a
/// divergence at all, and that it localises to the right tick rather than to
/// wherever it happens to next re-check.
#[test]
fn a_scripted_divergence_is_caught_at_the_exact_tick_it_first_occurs() {
    let script = placing_and_breaking_script();
    let mut left = FakeWorld::default();
    let mut right = FakeWorld {
        // `advance_tick` increments its internal counter *before* comparing
        // against `at`, and `run_differential`'s outer loop calls
        // `advance_tick` once per iteration starting from iteration 0 — so the
        // Nth call happens during outer iteration `N - 1`. `at: 5` is what
        // actually fires during outer iteration (and therefore reported)
        // tick 4, which is what this test asserts below; getting this off by
        // one is exactly the kind of localisation bug this test exists to
        // catch, so it is spelled out rather than left implicit.
        scripted_reaction: Some((5, (0, 0, 0), "minecraft:stone".to_owned())),
        ..FakeWorld::default()
    };

    let outcome = run_differential(&script, &region(), &mut left, &mut right, 2);

    match outcome {
        DifferentialOutcome::Diverged(d) => {
            assert_eq!(d.tick, 4, "divergence must localise to the exact tick it first occurs, not a later one");
            assert_eq!(d.pos, (0, 0, 0));
            assert_eq!(d.left.as_deref(), Some("minecraft:air"));
            assert_eq!(d.right.as_deref(), Some("minecraft:stone"));
        }
        other => panic!(
            "expected a divergence at tick 4 (right's scripted reaction fires there, left never gets it), got {other:?}"
        ),
    }
}

/// A third, boring-but-necessary case: an empty script with an empty region
/// must trivially agree rather than panicking on an empty iterator anywhere
/// in the loop (an off-by-one in `last_tick`'s `unwrap_or(0)` would be
/// invisible in the two tests above, which both have a nonempty script).
#[test]
fn empty_script_and_empty_region_trivially_agree() {
    let script = Script::default();
    let mut left = FakeWorld::default();
    let mut right = FakeWorld::default();

    let outcome = run_differential(&script, &[], &mut left, &mut right, 0);

    match outcome {
        DifferentialOutcome::Agreed => {}
        other => panic!("empty script/region must trivially agree, got {other:?}"),
    }
}
