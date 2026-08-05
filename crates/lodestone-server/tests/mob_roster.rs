//! Does the per-species goal roster actually reach a mob in the running game?
//!
//! # What these gates are for
//!
//! `lodestone_entity::ai::roster` is a pure lookup, so it is trivially easy to
//! test in a way that proves nothing. `goals_for("cow", …)` returning the right
//! priorities is a **closed loop**: it says the table is well-formed, not that any
//! cow in the game ever consults it. That distinction is the whole subject of
//! issue #441, where eight goals had green unit tests against a `ScriptMob` test
//! fake that overrides every perception method, while their `can_use` was a
//! compile-time constant `false` in production.
//!
//! So every gate here goes through **`MobSim::spawn_species`** — the real spawn
//! path, with a real `NavigatingMob` over a real `ChunkWorld` — never through
//! `goals_for` directly, and never with a hand-added `add_goal`. A test that adds
//! the goal it is about to observe cannot see whether the roster installed it.
//!
//! The observable is **movement**, not a goal count. `SimMob` exposes no way to
//! enumerate its goals, deliberately: a gate that counted them would pass for a
//! roster wired to the wrong species, and would pass for goals that are installed
//! but never scheduled.
//!
//! # What was actually broken
//!
//! Before the roster, `spawn_species` installed `RandomStrollGoal`,
//! `RandomLookAroundGoal`, and — for a hostile species — `MeleeAttackGoal`.
//! `FloatGoal`, `PanicGoal`, `BreedGoal`, `TemptGoal` and `FollowParentGoal` had
//! **zero production call sites**: fully implemented, fully unit-tested, fed real
//! perception by `MobSim::tick` since #441, and installed by nothing but tests.
//! `a_cow_follows_food_because_the_roster_installed_temptgoal` is the gate that
//! says that stopped being true.

use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkWorld, MobSim, PlayerPerception};
use std::str::FromStr;

fn rk(name: &str) -> ResourceKey {
    ResourceKey::from_str(name).expect("valid key")
}

/// A flat floor at y = -1, wide enough that a mob strolling for a few seconds
/// stays on it.
fn pen() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -24..=24 {
        for z in -24..=24 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn holding(at: Vec3, item: &str) -> PlayerPerception {
    PlayerPerception {
        position: at,
        held_item: Some(rk(&format!("minecraft:{item}"))),
    }
}

fn empty_handed(at: Vec3) -> PlayerPerception {
    PlayerPerception {
        position: at,
        held_item: None,
    }
}

/// How far along +X a mob is from the player at `player`.
fn gap(sim: &MobSim<'_>, id: i32, player: Vec3) -> f64 {
    (player.x - sim.get(id).expect("alive").position().x).abs()
}

/// The headline gate: a cow spawned through the production path follows a player
/// holding wheat, because the roster installed `TemptGoal` — which no production
/// code path installed before it existed.
///
/// Two things make this non-vacuous. It never calls `add_goal`, so the only thing
/// that can have installed `TemptGoal` is `spawn_species` consulting the roster.
/// And its control is the *same cow in the same world with the same player*,
/// empty-handed: if a bare `RandomStrollGoal` happened to wander the cow toward
/// the player, the control would show it too.
#[test]
fn a_cow_follows_food_because_the_roster_installed_temptgoal() {
    let world = pen();
    let player = Vec3::new(9.0, 0.0, 0.0);

    // Control first: an empty-handed player must not draw the cow in. This is a
    // *precondition* on the measurement, not decoration — without it, "the cow
    // ended up closer" is satisfied by an unlucky random stroll.
    let mut control = MobSim::new(&world);
    let cid = control
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    control.set_players(vec![empty_handed(player)]);
    let control_before = gap(&control, cid, player);
    for _ in 0..120 {
        control.tick();
    }
    let control_after = gap(&control, cid, player);
    assert!(
        control_after > control_before - 3.0,
        "control failed its own premise: an untempted cow closed from \
         {control_before} to {control_after}, so the subject's assertion below \
         would be satisfied by strolling alone"
    );

    // Subject: identical, but the player holds wheat — `cow_food` is exactly
    // `[wheat]` (`.cache/mc/26.2/src/data/minecraft/tags/item/cow_food.json`).
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.set_players(vec![holding(player, "wheat")]);
    let before = gap(&sim, id, player);
    for _ in 0..120 {
        sim.tick();
    }
    let after = gap(&sim, id, player);

    assert!(
        after < before - 3.0,
        "a cow spawned through spawn_species must follow a player holding \
         wheat: gap went {before} -> {after} over 120 ticks. No goal was added \
         by this test, so a failure here means the roster is not reaching \
         spawn_species (or is keyed on the wrong species string)"
    );
}

/// The negative control the plan asks for, run as a real test rather than
/// described: a species with **no** roster entry gets `roster::FALLBACK`, which
/// has no `TemptGoal`, so it must fail the assertion above under identical
/// conditions.
///
/// This is the "empty the roster entry" control, expressed without editing the
/// roster: a llama is a real 26.2 species that no family claims, so it takes the
/// fallback path by construction, and `llama_food` exists in the jar so the
/// choice of item is not what makes it fail.
#[test]
fn a_species_with_no_roster_entry_does_not_follow_food() {
    let world = pen();
    let player = Vec3::new(9.0, 0.0, 0.0);
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(rk("minecraft:llama"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.set_players(vec![holding(player, "wheat")]);

    let before = gap(&sim, id, player);
    for _ in 0..120 {
        sim.tick();
    }
    let after = gap(&sim, id, player);

    assert!(
        after > before - 3.0,
        "a llama has no roster entry, so it must get FALLBACK (stroll + look) \
         and must NOT close on a player holding wheat: gap went {before} -> \
         {after}. If it did close, either the fallback is not the fallback or \
         every species is getting the same table"
    );
}

/// The roster's *keying* must be per species, observed behaviourally in one
/// world: a pig closes on a potato and a cow does not, because `pig_food`
/// contains potato and `cow_food` does not
/// (`.cache/mc/26.2/src/data/minecraft/tags/item/{pig,cow}_food.json`).
///
/// Both animals get a `TemptGoal` from the roster, so this is not a test of
/// whether the goal is installed — it is a test that the *perception* and the
/// roster agree on which species is which. An assertion that passed for both
/// would mean the species key is being ignored somewhere.
#[test]
fn one_item_tempts_a_pig_and_not_a_cow_through_the_same_spawn_path() {
    let world = pen();
    let player = Vec3::new(9.0, 0.0, 0.0);
    let mut sim = MobSim::new(&world);
    let pig = sim
        .spawn_species(rk("minecraft:pig"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let cow = sim
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 4.0))
        .id();
    sim.set_players(vec![holding(player, "potato")]);

    let pig_before = gap(&sim, pig, player);
    let cow_before = gap(&sim, cow, player);
    for _ in 0..120 {
        sim.tick();
    }
    let pig_after = gap(&sim, pig, player);
    let cow_after = gap(&sim, cow, player);

    assert!(
        pig_after < pig_before - 3.0,
        "a potato is in pig_food, so the pig must close: {pig_before} -> {pig_after}"
    );
    assert!(
        cow_after > cow_before - 3.0,
        "a potato is NOT in cow_food (which is exactly [wheat]), so the cow \
         must not close: {cow_before} -> {cow_after}"
    );
}

/// A creeper's roster is not a cow's, observed through the production path: only
/// the creeper gets `SwellGoal`, so only the creeper detonates when a target is
/// inside its swell range.
///
/// This is the one behaviour in the roster whose end-to-end path was already
/// proven to reach a real client before the roster existed
/// (`crates/protocol/v770/tests/server_creeper_metadata_and_explode.rs`), which
/// is why it is the right second species to gate: a regression here is visible to
/// a player, not only to a test.
///
/// It also gates the priority *renumbering*. The baseline this replaced put
/// `SwellGoal` at a private `-1` to outrank a `MeleeAttackGoal` at `2`; the
/// roster uses vanilla's own `2` and `4` (`monster/Creeper.java:66`, `:69`). If
/// the two numbers were transcribed in the wrong order, melee would hold MOVE and
/// the creeper would never swell.
#[test]
fn only_a_creeper_swells_and_vanillas_priority_order_is_preserved() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    // Within `SwellGoal`'s 9.0 squared proximity (`ai/goal/SwellGoal.java:20`).
    let target = Vec3::new(2.0, 0.0, 0.0);
    let creeper = sim
        .spawn_species(rk("minecraft:creeper"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let cow = sim
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 6.0))
        .id();
    for id in [creeper, cow] {
        sim.get_mut(id)
            .expect("just spawned")
            .set_attack_target(Some(target));
    }

    let mut creeper_swelled = false;
    let mut cow_swelled = false;
    for _ in 0..40 {
        sim.tick();
        if sim.get(creeper).map(|m| m.swell_dir()) == Some(1) {
            creeper_swelled = true;
        }
        if sim.get(cow).map(|m| m.swell_dir()) == Some(1) {
            cow_swelled = true;
        }
    }

    assert!(
        creeper_swelled,
        "a creeper spawned through spawn_species must get SwellGoal from the \
         roster and start swelling with a target 2 blocks away. If this fails \
         with the cow assertion below passing, check that Creeper.java's goal \
         priority 2 (Swell) is still lower than 4 (Melee) in the table — a \
         MeleeAttackGoal holding MOVE prevents the swell"
    );
    assert!(
        !cow_swelled,
        "a cow must not get SwellGoal — if it does, every species is being \
         handed the same table"
    );
}
