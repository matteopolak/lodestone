//! Bounded, fixed-seed model checking for the server-side entity effect store.
//!
//! [`lodestone_server::mob_effects::ActiveEffects`] is the production state
//! that the player tick consumes for health, hunger, and the movement-facing
//! effect seam. This test drives its public apply/tick/remove surface with a
//! finite shrinkable action script and compares each observable result with a
//! small independent model. The prefix exercises a remembered weaker effect;
//! the detector control deliberately stops ticking hidden durations, a
//! plausible rule that would resurface the tail at the wrong duration.

use std::collections::BTreeMap;

use lodestone_server::mob_effects::{ActiveEffects, EffectTick, INFINITE_DURATION};
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestCaseError, TestError, TestRunner};

const CASES: u32 = 160;
const SEED: u64 = 0x45_46_46_45_43_54_53;
const MAX_SHRINK_ITERS: u32 = 256;

const EFFECT_IDS: [&str; 5] = [
    "minecraft:poison",
    "minecraft:wither",
    "minecraft:regeneration",
    "minecraft:hunger",
    "minecraft:strength",
];
const HEALTH_VALUES: [f32; 5] = [0.5, 1.0, 1.5, 10.0, 20.0];
const MAX_HEALTH_VALUES: [f32; 3] = [1.0, 10.0, 20.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectOp {
    Apply {
        effect: usize,
        duration: i32,
        amplifier: u32,
    },
    Tick {
        entity_tick: i32,
        health: usize,
        max_health: usize,
    },
    Remove { effect: usize },
    Clear,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectView {
    id: String,
    duration: i32,
    amplifier: u32,
    hidden: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Observation {
    operation: EffectOp,
    changed: Option<bool>,
    tick: Option<EffectTick>,
    state: Vec<EffectView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferenceEffect {
    duration: i32,
    amplifier: u32,
    hidden: Option<Box<ReferenceEffect>>,
}

#[derive(Debug, Default)]
struct ReferenceEffects {
    effects: BTreeMap<String, ReferenceEffect>,
}

impl ReferenceEffect {
    fn new(duration: i32, amplifier: u32) -> Self {
        Self {
            duration,
            amplifier,
            hidden: None,
        }
    }

    fn is_infinite(&self) -> bool {
        self.duration == INFINITE_DURATION
    }

    fn has_remaining(&self) -> bool {
        self.is_infinite() || self.duration > 0
    }

    fn is_shorter_than(&self, other: &Self) -> bool {
        !self.is_infinite() && (self.duration < other.duration || other.is_infinite())
    }

    fn update(&mut self, incoming: &Self) -> bool {
        let mut changed = false;
        if incoming.amplifier > self.amplifier {
            if incoming.is_shorter_than(self) {
                let previous_hidden = self.hidden.take();
                let mut demoted = Self::new(self.duration, self.amplifier);
                demoted.hidden = previous_hidden;
                self.hidden = Some(Box::new(demoted));
            }
            self.amplifier = incoming.amplifier;
            self.duration = incoming.duration;
            changed = true;
        } else if self.is_shorter_than(incoming) {
            if incoming.amplifier == self.amplifier {
                self.duration = incoming.duration;
                changed = true;
            } else if self.hidden.is_none() {
                self.hidden = Some(Box::new(incoming.clone()));
            } else {
                self.hidden
                    .as_mut()
                    .expect("hidden checked above")
                    .update(incoming);
            }
        }
        changed
    }

    fn tick_down(&mut self, tick_hidden: bool) {
        if tick_hidden {
            if let Some(hidden) = self.hidden.as_mut() {
                hidden.tick_down(true);
            }
        }
        if !self.is_infinite() && self.duration != 0 {
            self.duration -= 1;
        }
    }

    fn downgrade(&mut self) -> bool {
        if self.duration == 0 && self.hidden.is_some() {
            let hidden = self.hidden.take().expect("hidden checked above");
            self.duration = hidden.duration;
            self.amplifier = hidden.amplifier;
            self.hidden = hidden.hidden;
            true
        } else {
            false
        }
    }
}

impl ReferenceEffects {
    fn apply(&mut self, id: &str, duration: i32, amplifier: u32) -> bool {
        let incoming = ReferenceEffect::new(duration, amplifier);
        match self.effects.get_mut(id) {
            Some(current) => current.update(&incoming),
            None => {
                self.effects.insert(id.to_owned(), incoming);
                true
            }
        }
    }

    fn remove(&mut self, id: &str) -> bool {
        self.effects.remove(id).is_some()
    }

    fn tick(
        &mut self,
        entity_tick: i32,
        health: f32,
        max_health: f32,
        tick_hidden: bool,
    ) -> EffectTick {
        let mut out = EffectTick::default();
        let mut expired = Vec::new();

        for (id, instance) in &mut self.effects {
            if !instance.has_remaining() {
                expired.push(id.clone());
                continue;
            }
            let tick_count = if instance.is_infinite() {
                entity_tick
            } else {
                instance.duration
            };
            if let Some((interval, action)) = reference_periodic_effect(id)
                && reference_should_apply(interval, instance.amplifier, tick_count)
            {
                match action {
                    ReferenceAction::Poison => {
                        if health > 1.0 {
                            out.poison_damage += 1.0;
                        }
                    }
                    ReferenceAction::Wither => out.wither_damage += 1.0,
                    ReferenceAction::Regeneration => {
                        if health < max_health {
                            out.heal += 1.0;
                        }
                    }
                    ReferenceAction::Hunger => {
                        out.exhaustion += 0.005 * (instance.amplifier + 1) as f32;
                    }
                }
            }
            instance.tick_down(tick_hidden);
            if instance.downgrade() {
                out.list_changed = true;
            }
            if !instance.has_remaining() {
                expired.push(id.clone());
            }
        }

        for id in expired {
            self.effects.remove(&id);
            out.list_changed = true;
        }
        out
    }

    fn snapshot(&self) -> Vec<EffectView> {
        self.effects
            .iter()
            .map(|(id, effect)| EffectView {
                id: id.clone(),
                duration: effect.duration,
                amplifier: effect.amplifier,
                hidden: effect.hidden.is_some(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy)]
enum ReferenceAction {
    Poison,
    Wither,
    Regeneration,
    Hunger,
}

fn reference_periodic_effect(id: &str) -> Option<(i32, ReferenceAction)> {
    match id {
        "minecraft:poison" => Some((25, ReferenceAction::Poison)),
        "minecraft:wither" => Some((40, ReferenceAction::Wither)),
        "minecraft:regeneration" => Some((50, ReferenceAction::Regeneration)),
        "minecraft:hunger" => Some((1, ReferenceAction::Hunger)),
        _ => None,
    }
}

fn reference_should_apply(base_interval: i32, amplifier: u32, tick_count: i32) -> bool {
    let interval = if amplifier >= 31 {
        0
    } else {
        base_interval >> amplifier
    };
    interval <= 0 || tick_count % interval == 0
}

fn production_snapshot(effects: &ActiveEffects) -> Vec<EffectView> {
    effects
        .active()
        .into_iter()
        .map(|(id, _)| {
            let effect = effects.get(id).expect("active id must be readable");
            EffectView {
                id: id.to_owned(),
                duration: effect.duration(),
                amplifier: effect.amplifier(),
                hidden: effect.has_hidden(),
            }
        })
        .collect()
}

fn production_trace(script: &[EffectOp]) -> Vec<Observation> {
    let mut effects = ActiveEffects::new();
    script
        .iter()
        .map(|&operation| {
            let (changed, tick) = match operation {
                EffectOp::Apply {
                    effect,
                    duration,
                    amplifier,
                } => (
                    Some(effects.apply(EFFECT_IDS[effect], duration, amplifier)),
                    None,
                ),
                EffectOp::Tick {
                    entity_tick,
                    health,
                    max_health,
                } => (
                    None,
                    Some(effects.tick(
                        entity_tick,
                        HEALTH_VALUES[health],
                        MAX_HEALTH_VALUES[max_health],
                    )),
                ),
                EffectOp::Remove { effect } => (Some(effects.remove(EFFECT_IDS[effect])), None),
                EffectOp::Clear => {
                    effects.clear();
                    (None, None)
                }
            };
            Observation {
                operation,
                changed,
                tick,
                state: production_snapshot(&effects),
            }
        })
        .collect()
}

fn reference_trace(script: &[EffectOp], tick_hidden: bool) -> Vec<Observation> {
    let mut effects = ReferenceEffects::default();
    script
        .iter()
        .map(|&operation| {
            let (changed, tick) = match operation {
                EffectOp::Apply {
                    effect,
                    duration,
                    amplifier,
                } => (
                    Some(effects.apply(EFFECT_IDS[effect], duration, amplifier)),
                    None,
                ),
                EffectOp::Tick {
                    entity_tick,
                    health,
                    max_health,
                } => (
                    None,
                    Some(effects.tick(
                        entity_tick,
                        HEALTH_VALUES[health],
                        MAX_HEALTH_VALUES[max_health],
                        tick_hidden,
                    )),
                ),
                EffectOp::Remove { effect } => (Some(effects.remove(EFFECT_IDS[effect])), None),
                EffectOp::Clear => {
                    effects.effects.clear();
                    (None, None)
                }
            };
            Observation {
                operation,
                changed,
                tick,
                state: effects.snapshot(),
            }
        })
        .collect()
}

fn runner() -> TestRunner {
    TestRunner::new(Config {
        cases: CASES,
        max_shrink_iters: MAX_SHRINK_ITERS,
        max_shrink_time: 0,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        ..Config::default()
    })
}

fn duration_strategy() -> impl Strategy<Value = i32> {
    prop_oneof![Just(INFINITE_DURATION), 0_i32..=96]
}

fn operation_strategy() -> impl Strategy<Value = EffectOp> {
    prop_oneof![
        (0_usize..EFFECT_IDS.len(), duration_strategy(), 0_u32..=4).prop_map(
            |(effect, duration, amplifier)| EffectOp::Apply {
                effect,
                duration,
                amplifier,
            }
        ),
        (0_i32..=120, 0_usize..HEALTH_VALUES.len(), 0_usize..MAX_HEALTH_VALUES.len()).prop_map(
            |(entity_tick, health, max_health)| EffectOp::Tick {
                entity_tick,
                health,
                max_health,
            }
        ),
        (0_usize..EFFECT_IDS.len()).prop_map(|effect| EffectOp::Remove { effect }),
        Just(EffectOp::Clear),
    ]
}

fn script_strategy() -> impl Strategy<Value = Vec<EffectOp>> {
    collection::vec(operation_strategy(), 0..=24).prop_map(|tail| {
        // The visible stronger effect runs for five ticks. The weaker longer
        // tail has already lost those five ticks by the time it resurfaces.
        let mut script = vec![
            EffectOp::Apply {
                effect: 4,
                duration: 5,
                amplifier: 1,
            },
            EffectOp::Apply {
                effect: 4,
                duration: 9,
                amplifier: 0,
            },
        ];
        script.extend((0..5).map(|_| EffectOp::Tick {
            entity_tick: 0,
            health: 4,
            max_health: 2,
        }));
        script.extend(tail);
        script
    })
}

#[test]
fn generated_effect_scripts_match_the_independent_model() {
    runner()
        .run(&script_strategy(), |script| {
            let actual = production_trace(&script);
            let expected = reference_trace(&script, true);
            if actual != expected {
                return Err(TestCaseError::fail(format!(
                    "production effect trace differs from independent model:\nactual={actual:?}\nexpected={expected:?}"
                )));
            }
            Ok(())
        })
        .expect("production ActiveEffects must match the independent effect model");
}

#[test]
fn hidden_duration_detector_is_rejected_and_shrunk() {
    let failure = runner()
        .run(&script_strategy(), |script| {
            let expected = production_trace(&script);
            // Intentional detector model: hidden effects wait without ticking.
            // The fixed prefix must expose the wrong resurfaced duration.
            let wrong = reference_trace(&script, false);
            prop_assert_eq!(wrong, expected, "hidden duration detector control");
            Ok(())
        })
        .expect_err("a model that does not tick hidden durations must be rejected");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(minimal.len() >= 7, "the stacking prefix must survive shrinking");
            assert_eq!(minimal[0], EffectOp::Apply {
                effect: 4,
                duration: 5,
                amplifier: 1,
            });
            assert_eq!(minimal[1], EffectOp::Apply {
                effect: 4,
                duration: 9,
                amplifier: 0,
            });
            assert!(minimal[2..7]
                .iter()
                .all(|operation| matches!(operation, EffectOp::Tick { .. })));
            let correct = reference_trace(&minimal, true);
            let wrong = reference_trace(&minimal, false);
            assert_ne!(correct, wrong, "detector control must have a real witness");
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
}
