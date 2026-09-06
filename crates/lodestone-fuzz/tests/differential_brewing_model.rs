//! Bounded, fixed-seed model checking for the server brewing chain.
//!
//! The production path combines slot writes, fuel refill, eligibility, a
//! 400-tick timer, ingredient-change cancellation, and all-three-bottle
//! transitions. This test drives those public operations with a finite,
//! shrinkable action script and compares every observable state field with an
//! independent enum model. The fixed prefix includes a cancellation, an
//! ordinary potion transition, and a container promotion.
//!
//! The detector control intentionally ignores the locked-ingredient check.
//! Its fixed cancellation witness must diverge, and the fixed-seed shrink run
//! proves that a comparison which accepts every trace cannot pass.

use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

use lodestone_server::{BREW_TIME_TICKS, Bottle, BottleKind, BrewTick, BrewingStand};

const CASES: u32 = 160;
const SEED: u64 = 0x42_52_45_57_31_33;
const MAX_SHRINK_ITERS: u32 = 256;
const BOTTLE_SLOTS: usize = 3;
const FUEL_USES: i32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Potion {
    Water,
    Mundane,
    Thick,
    Awkward,
    Swiftness,
    LongSwiftness,
    NightVision,
    LongNightVision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ingredient {
    NetherWart,
    Redstone,
    GlowstoneDust,
    Sugar,
    GoldenCarrot,
    Gunpowder,
    DragonBreath,
    Junk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fuel {
    BlazePowder,
    Junk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RefBottle {
    kind: BottleKind,
    potion: Potion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RefStack<T> {
    item: T,
    count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrewOp {
    SetBottle {
        slot: usize,
        bottle: Option<RefBottle>,
    },
    SetIngredient(Option<RefStack<Ingredient>>),
    SetFuel(Option<RefStack<Fuel>>),
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefWorld {
    bottles: [Option<RefBottle>; BOTTLE_SLOTS],
    ingredient: Option<RefStack<Ingredient>>,
    fuel: Option<RefStack<Fuel>>,
    fuel_charges: i32,
    brew_time: i32,
    locked_ingredient: Option<Ingredient>,
}

impl Default for RefWorld {
    fn default() -> Self {
        Self {
            bottles: [None; BOTTLE_SLOTS],
            ingredient: None,
            fuel: None,
            fuel_charges: 0,
            brew_time: 0,
            locked_ingredient: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefObservation {
    tick: Option<BrewTick>,
    bottles: [Option<RefBottle>; BOTTLE_SLOTS],
    ingredient: Option<RefStack<Ingredient>>,
    fuel: Option<RefStack<Fuel>>,
    fuel_charges: i32,
    brew_time: i32,
    locked_ingredient: Option<Ingredient>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    tick: Option<BrewTick>,
    bottles: [Option<RefBottle>; BOTTLE_SLOTS],
    ingredient: Option<RefStack<Ingredient>>,
    fuel: Option<RefStack<Fuel>>,
    fuel_charges: i32,
    brew_time: i32,
    locked_ingredient: Option<Ingredient>,
}

fn potion_name(potion: Potion) -> &'static str {
    match potion {
        Potion::Water => "minecraft:water",
        Potion::Mundane => "minecraft:mundane",
        Potion::Thick => "minecraft:thick",
        Potion::Awkward => "minecraft:awkward",
        Potion::Swiftness => "minecraft:swiftness",
        Potion::LongSwiftness => "minecraft:long_swiftness",
        Potion::NightVision => "minecraft:night_vision",
        Potion::LongNightVision => "minecraft:long_night_vision",
    }
}

fn ingredient_name(ingredient: Ingredient) -> &'static str {
    match ingredient {
        Ingredient::NetherWart => "minecraft:nether_wart",
        Ingredient::Redstone => "minecraft:redstone",
        Ingredient::GlowstoneDust => "minecraft:glowstone_dust",
        Ingredient::Sugar => "minecraft:sugar",
        Ingredient::GoldenCarrot => "minecraft:golden_carrot",
        Ingredient::Gunpowder => "minecraft:gunpowder",
        Ingredient::DragonBreath => "minecraft:dragon_breath",
        Ingredient::Junk => "minecraft:diamond",
    }
}

fn fuel_name(fuel: Fuel) -> &'static str {
    match fuel {
        Fuel::BlazePowder => "minecraft:blaze_powder",
        Fuel::Junk => "minecraft:diamond",
    }
}

fn parse_potion(name: &str) -> Potion {
    match name {
        "minecraft:water" => Potion::Water,
        "minecraft:mundane" => Potion::Mundane,
        "minecraft:thick" => Potion::Thick,
        "minecraft:awkward" => Potion::Awkward,
        "minecraft:swiftness" => Potion::Swiftness,
        "minecraft:long_swiftness" => Potion::LongSwiftness,
        "minecraft:night_vision" => Potion::NightVision,
        "minecraft:long_night_vision" => Potion::LongNightVision,
        other => panic!("production emitted an unmodeled potion {other}"),
    }
}

fn parse_ingredient(name: &str) -> Ingredient {
    match name {
        "minecraft:nether_wart" => Ingredient::NetherWart,
        "minecraft:redstone" => Ingredient::Redstone,
        "minecraft:glowstone_dust" => Ingredient::GlowstoneDust,
        "minecraft:sugar" => Ingredient::Sugar,
        "minecraft:golden_carrot" => Ingredient::GoldenCarrot,
        "minecraft:gunpowder" => Ingredient::Gunpowder,
        "minecraft:dragon_breath" => Ingredient::DragonBreath,
        "minecraft:diamond" => Ingredient::Junk,
        other => panic!("production emitted an unmodeled ingredient {other}"),
    }
}

fn parse_fuel(name: &str) -> Fuel {
    match name {
        "minecraft:blaze_powder" => Fuel::BlazePowder,
        "minecraft:diamond" => Fuel::Junk,
        other => panic!("production emitted an unmodeled fuel {other}"),
    }
}

/// Independent potion/container transition table for the generated domain.
/// No production lookup is used by this function.
fn model_mix(bottle: RefBottle, ingredient: Ingredient) -> RefBottle {
    let promoted = match (bottle.kind, ingredient) {
        (BottleKind::Potion, Ingredient::Gunpowder) => Some(BottleKind::Splash),
        (BottleKind::Splash, Ingredient::DragonBreath) => Some(BottleKind::Lingering),
        _ => None,
    };
    if let Some(kind) = promoted {
        return RefBottle { kind, ..bottle };
    }

    let potion = match (bottle.potion, ingredient) {
        (Potion::Water, Ingredient::NetherWart) => Some(Potion::Awkward),
        (Potion::Water, Ingredient::Redstone) => Some(Potion::Mundane),
        (Potion::Water, Ingredient::GlowstoneDust) => Some(Potion::Thick),
        (Potion::Water, Ingredient::Sugar) => Some(Potion::Mundane),
        (Potion::Awkward, Ingredient::Sugar) => Some(Potion::Swiftness),
        (Potion::Awkward, Ingredient::GoldenCarrot) => Some(Potion::NightVision),
        (Potion::Swiftness, Ingredient::Redstone) => Some(Potion::LongSwiftness),
        (Potion::NightVision, Ingredient::Redstone) => Some(Potion::LongNightVision),
        _ => None,
    };
    potion.map_or(bottle, |potion| RefBottle { potion, ..bottle })
}

fn model_is_ingredient(ingredient: Ingredient) -> bool {
    !matches!(ingredient, Ingredient::Junk)
}

fn model_has_mix(bottle: RefBottle, ingredient: Ingredient) -> bool {
    model_mix(bottle, ingredient) != bottle
}

fn model_brewable(world: &RefWorld) -> bool {
    let Some(ingredient) = world.ingredient else {
        return false;
    };
    model_is_ingredient(ingredient.item)
        && world
            .bottles
            .iter()
            .flatten()
            .copied()
            .any(|bottle| model_has_mix(bottle, ingredient.item))
}

fn model_tick(world: &mut RefWorld, check_locked_ingredient: bool) -> BrewTick {
    let mut out = BrewTick::default();

    if world.fuel_charges <= 0
        && matches!(world.fuel, Some(RefStack { item: Fuel::BlazePowder, .. }))
    {
        world.fuel_charges = FUEL_USES;
        let fuel = world.fuel.as_mut().expect("fuel checked above");
        fuel.count -= 1;
        if fuel.count == 0 {
            world.fuel = None;
        }
        out.fuel_refilled = true;
    }

    let brewable = model_brewable(world);
    if world.brew_time > 0 {
        world.brew_time -= 1;
        let done = world.brew_time == 0;
        let ingredient_swapped = check_locked_ingredient
            && world.locked_ingredient != world.ingredient.map(|stack| stack.item);
        if done && brewable {
            let ingredient = world.ingredient.expect("brewable implies ingredient").item;
            for bottle in &mut world.bottles {
                if let Some(current) = bottle {
                    *current = model_mix(*current, ingredient);
                }
            }
            let stack = world.ingredient.as_mut().expect("brewable implies ingredient");
            stack.count -= 1;
            if stack.count == 0 {
                world.ingredient = None;
            }
            world.locked_ingredient = None;
            out.brewed = true;
        } else if !brewable || ingredient_swapped {
            world.brew_time = 0;
            world.locked_ingredient = None;
        }
    } else if brewable && world.fuel_charges > 0 {
        world.fuel_charges -= 1;
        world.brew_time = BREW_TIME_TICKS;
        world.locked_ingredient = world.ingredient.map(|stack| stack.item);
        out.started = true;
    }

    out
}

fn stack_strategy<T: Copy + std::fmt::Debug + 'static>(
    items: &'static [T],
) -> impl Strategy<Value = Option<RefStack<T>>> {
    prop_oneof![
        Just(None),
        (prop::sample::select(items), 1_u32..=4).prop_map(|(item, count)| Some(RefStack { item, count })),
    ]
}

fn bottle_strategy() -> impl Strategy<Value = Option<RefBottle>> {
    prop_oneof![
        Just(None),
        (
            prop::sample::select(&[BottleKind::Potion, BottleKind::Splash, BottleKind::Lingering]),
            prop::sample::select(&[
                Potion::Water,
                Potion::Mundane,
                Potion::Thick,
                Potion::Awkward,
                Potion::Swiftness,
                Potion::LongSwiftness,
                Potion::NightVision,
                Potion::LongNightVision,
            ]),
        )
            .prop_map(|(kind, potion)| Some(RefBottle { kind, potion })),
    ]
}

fn operation_strategy() -> impl Strategy<Value = BrewOp> {
    prop_oneof![
        (0..BOTTLE_SLOTS, bottle_strategy()).prop_map(|(slot, bottle)| BrewOp::SetBottle { slot, bottle }),
        stack_strategy(&[
            Ingredient::NetherWart,
            Ingredient::Redstone,
            Ingredient::Sugar,
            Ingredient::GoldenCarrot,
            Ingredient::Gunpowder,
            Ingredient::DragonBreath,
            Ingredient::Junk,
        ])
            .prop_map(BrewOp::SetIngredient),
        stack_strategy(&[Fuel::BlazePowder, Fuel::Junk]).prop_map(BrewOp::SetFuel),
        Just(BrewOp::Tick),
    ]
}

fn scenario_strategy() -> impl Strategy<Value = Vec<BrewOp>> {
    collection::vec(operation_strategy(), 1..=24)
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        max_shrink_iters: MAX_SHRINK_ITERS,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    })
}

fn prefix() -> Vec<BrewOp> {
    let mut ops = vec![
        BrewOp::SetBottle {
            slot: 0,
            bottle: Some(RefBottle {
                kind: BottleKind::Potion,
                potion: Potion::Water,
            }),
        },
        BrewOp::SetBottle {
            slot: 1,
            bottle: Some(RefBottle {
                kind: BottleKind::Potion,
                potion: Potion::Awkward,
            }),
        },
        BrewOp::SetBottle {
            slot: 2,
            bottle: Some(RefBottle {
                kind: BottleKind::Splash,
                potion: Potion::Swiftness,
            }),
        },
        BrewOp::SetIngredient(Some(RefStack {
            item: Ingredient::NetherWart,
            count: 2,
        })),
        BrewOp::SetFuel(Some(RefStack {
            item: Fuel::BlazePowder,
            count: 2,
        })),
        BrewOp::Tick,
    ];
    ops.extend(std::iter::repeat_n(BrewOp::Tick, 3));
    ops.push(BrewOp::SetIngredient(Some(RefStack {
        item: Ingredient::Redstone,
        count: 2,
    })));
    ops.push(BrewOp::Tick);
    ops.push(BrewOp::SetIngredient(Some(RefStack {
        item: Ingredient::NetherWart,
        count: 2,
    })));
    ops.push(BrewOp::Tick);
    ops.extend(std::iter::repeat_n(BrewOp::Tick, BREW_TIME_TICKS as usize));
    ops.push(BrewOp::SetIngredient(Some(RefStack {
        item: Ingredient::Gunpowder,
        count: 1,
    })));
    ops.push(BrewOp::Tick);
    ops.extend(std::iter::repeat_n(BrewOp::Tick, BREW_TIME_TICKS as usize));
    ops
}

fn production_bottle(bottle: Option<RefBottle>) -> Option<Bottle> {
    bottle.map(|bottle| {
        Bottle::from_potion_name(bottle.kind, potion_name(bottle.potion))
            .expect("modeled potion has a production registry id")
    })
}

fn production_stack<T>(stack: Option<RefStack<T>>, name: fn(T) -> &'static str) -> Option<(String, u32)> {
    stack.map(|stack| (name(stack.item).to_owned(), stack.count))
}

fn production_observation(stand: &BrewingStand, tick: Option<BrewTick>) -> Observation {
    let bottles = std::array::from_fn(|slot| {
        stand.bottle(slot).map(|bottle| RefBottle {
            kind: bottle.kind,
            potion: parse_potion(lodestone_data::potion::potion_name(bottle.potion)),
        })
    });
    let ingredient = stand.ingredient().map(|(name, count)| RefStack {
        item: parse_ingredient(name),
        count,
    });
    let fuel = stand.fuel_item().map(|(name, count)| RefStack {
        item: parse_fuel(name),
        count,
    });
    Observation {
        tick,
        bottles,
        ingredient,
        fuel,
        fuel_charges: stand.fuel_charges(),
        brew_time: stand.brew_progress(),
        locked_ingredient: stand.locked_ingredient().map(parse_ingredient),
    }
}

fn ref_observation(world: &RefWorld, tick: Option<BrewTick>) -> RefObservation {
    RefObservation {
        tick,
        bottles: world.bottles,
        ingredient: world.ingredient,
        fuel: world.fuel,
        fuel_charges: world.fuel_charges,
        brew_time: world.brew_time,
        locked_ingredient: world.locked_ingredient,
    }
}

fn production_trace(scenario: &[BrewOp]) -> Vec<Observation> {
    let mut stand = BrewingStand::new();
    let mut trace = Vec::new();
    let mut ops = prefix();
    ops.extend(scenario.iter().copied());
    for op in ops {
        let tick = match op {
            BrewOp::SetBottle { slot, bottle } => {
                stand.set_bottle(slot, production_bottle(bottle));
                None
            }
            BrewOp::SetIngredient(stack) => {
                stand.set_ingredient(production_stack(stack, ingredient_name));
                None
            }
            BrewOp::SetFuel(stack) => {
                stand.set_fuel_item(production_stack(stack, fuel_name));
                None
            }
            BrewOp::Tick => Some(stand.tick()),
        };
        trace.push(production_observation(&stand, tick));
    }
    trace
}

fn model_trace(scenario: &[BrewOp], check_locked_ingredient: bool) -> Vec<RefObservation> {
    let mut world = RefWorld::default();
    let mut trace = Vec::new();
    let mut ops = prefix();
    ops.extend(scenario.iter().copied());
    for op in ops {
        let tick = match op {
            BrewOp::SetBottle { slot, bottle } => {
                world.bottles[slot] = bottle;
                None
            }
            BrewOp::SetIngredient(stack) => {
                world.ingredient = stack;
                None
            }
            BrewOp::SetFuel(stack) => {
                world.fuel = stack;
                None
            }
            BrewOp::Tick => Some(model_tick(&mut world, check_locked_ingredient)),
        };
        trace.push(ref_observation(&world, tick));
    }
    trace
}

fn equivalent(actual: &[Observation], expected: &[RefObservation]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).all(|(actual, expected)| {
            actual.tick == expected.tick
                && actual.bottles == expected.bottles
                && actual.ingredient == expected.ingredient
                && actual.fuel == expected.fuel
                && actual.fuel_charges == expected.fuel_charges
                && actual.brew_time == expected.brew_time
                && actual.locked_ingredient == expected.locked_ingredient
        })
}

fn mismatch_index(actual: &[Observation], expected: &[RefObservation]) -> Option<usize> {
    if actual.len() != expected.len() {
        return Some(actual.len().min(expected.len()));
    }
    actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| {
            actual.tick != expected.tick
                || actual.bottles != expected.bottles
                || actual.ingredient != expected.ingredient
                || actual.fuel != expected.fuel
                || actual.fuel_charges != expected.fuel_charges
                || actual.brew_time != expected.brew_time
                || actual.locked_ingredient != expected.locked_ingredient
        })
}

#[test]
fn generated_brewing_traces_match_the_independent_model() {
    runner(CASES)
        .run(&scenario_strategy(), |scenario| {
            let actual = production_trace(&scenario);
            let expected = model_trace(&scenario, true);
            let mismatch = mismatch_index(&actual, &expected);
            prop_assert!(
                mismatch.is_none(),
                "brewing trace mismatch at {mismatch:?}: actual={:?} expected={:?}",
                mismatch.and_then(|index| actual.get(index)),
                mismatch.and_then(|index| expected.get(index)),
            );
            Ok(())
        })
        .expect("the production brewing chain must match the independent model");
}

#[test]
fn detector_control_rejects_ignoring_mid_brew_ingredient_swaps() {
    let evaluations = std::cell::Cell::new(0usize);
    let failure = runner(CASES)
        .run(&scenario_strategy(), |scenario| {
            evaluations.set(evaluations.get() + 1);
            let actual = production_trace(&scenario);
            let intentionally_wrong = model_trace(&scenario, false);
            prop_assert!(
                equivalent(&actual, &intentionally_wrong),
                "detector control that ignores the ingredient lock must disagree"
            );
            Ok(())
        })
        .expect_err("a model without ingredient-swap cancellation must be rejected");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(minimal.len() <= 24, "the shrunk control must remain bounded");
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(
        evaluations.get() > 1,
        "the fixed-seed control must evaluate shrink candidates after detecting a mismatch"
    );
}
