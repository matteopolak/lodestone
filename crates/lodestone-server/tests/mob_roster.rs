//! Does the per-species goal roster actually reach a mob in the running game?
//!
//! # What these gates cover
//!
//! `lodestone_entity::ai::roster` is a pure lookup, so it is trivially easy to
//! test in a way that proves nothing. `goals_for("cow", …)` returning the right
//! priorities is a **closed loop**: it says the table is well-formed, not that any
//! cow in the game ever consults it. A test fake can also override every
//! perception method while the production controller leaves the corresponding
//! goals inactive.
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
//! # Production roster coverage
//!
//! `spawn_species` installs the common movement and look goals, the hostile
//! attack goal, and the species-specific goals exercised below. Those goals also
//! receive perception through `MobSim::tick`; the tests verify that the complete
//! path reaches a spawned mob rather than only a lookup table or test controller.

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
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }
}

fn empty_handed(at: Vec3) -> PlayerPerception {
    PlayerPerception {
        position: at,
        held_item: None,
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }
}

/// How far along +X a mob is from the player at `player`.
fn gap(sim: &MobSim<'_>, id: i32, player: Vec3) -> f64 {
    (player.x - sim.get(id).expect("alive").position().x).abs()
}

/// Where the single player stands in every arm below.
const PLAYER: Vec3 = Vec3::new(9.0, 0.0, 0.0);

/// Ticks per arm. Long enough for a `TemptGoal` to cross the 9 blocks at the
/// roster's follow speed, and 120 was the original figure.
const TICKS: usize = 120;

/// # Why the negative arms compare **positions**, not distances
///
/// The margin form these used — "an untempted mob must not close by 3 blocks in
/// 120 ticks" — is a *premise* about an unconstrained random walk, and it was true
/// only by accident of the RNG seed. Each mob now has its own stream seeded from
/// its id; for id 1 the first successful 1/120 stroll draw is draw **9**, so a
/// `RandomStrollGoal` can fire well inside the window and a ±10-block stroll can
/// carry a mob 3+ blocks toward a player it has no interest in. A distance margin
/// would therefore measure a random walk rather than the roster.
///
/// The property actually under test is not "stayed far away" — mobs wander — it
/// is **"its movement does not depend on what the player is holding."** So each
/// negative arm runs the same species twice from a fresh sim, identical in every
/// way except the held item, and requires the trajectories to be *bit-identical*.
/// A mob with no `TemptGoal` never reads `held_item`, and `TemptGoal::can_use`
/// consumes no RNG (it is a `mob.temptation()` lookup), so an untempted mob's
/// stream is unperturbed by the item. That makes the assertion exact — no
/// tolerance, no seed dependence, and strictly stronger than the 3-block margin,
/// since it catches *any* item-dependent movement rather than 3 blocks' worth.
///
/// Each negative arm carries its own positive control on the same comparison, so a
/// change that made every arm identical (perception never fed at all) fails rather
/// than passing twice.
///
/// Runs one species alone from a fresh sim and returns its end position.
fn run_species(species: &str, held: Option<&str>) -> Vec3 {
    let world = pen();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(rk(species), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.set_players(vec![match held {
        Some(item) => holding(PLAYER, item),
        None => empty_handed(PLAYER),
    }]);
    for _ in 0..TICKS {
        sim.tick();
    }
    sim.get(id).expect("alive").position()
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
    //
    // The control establishes the no-food trajectory before the subject arm.
    // An untempted cow may wander toward the player, so a fixed distance margin
    // would not isolate the roster. The relative comparison below keeps the two
    // simulations identical except for the held item.
    let mut control = MobSim::new(&world);
    let cid = control
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    control.set_players(vec![empty_handed(player)]);
    for _ in 0..TICKS {
        control.tick();
    }
    let control_after = gap(&control, cid, player);
    let control_end = control.get(cid).expect("alive").position();

    // Subject: identical, but the player holds wheat — `cow_food` is exactly
    // `[wheat]` (`.cache/mc/26.2/src/data/minecraft/tags/item/cow_food.json`).
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.set_players(vec![holding(player, "wheat")]);
    let before = gap(&sim, id, player);
    for _ in 0..TICKS {
        sim.tick();
    }
    let after = gap(&sim, id, player);
    let end = sim.get(id).expect("alive").position();

    assert!(
        after < before - 3.0,
        "a cow spawned through spawn_species must follow a player holding \
         wheat: gap went {before} -> {after} over {TICKS} ticks. No goal was added \
         by this test, so a failure here means the roster is not reaching \
         spawn_species (or is keyed on the wrong species string)"
    );
    assert!(
        after < control_after,
        "and it must end up closer than the same cow with an empty-handed player \
         ({after} vs {control_after}), or the approach is strolling rather than tempting"
    );
    // The load-bearing comparison: the two arms differ in exactly one input, so any
    // difference in outcome is attributable to it. Exact, and immune to whatever the
    // stroll RNG happens to do — unlike the 3-block margins this file used to rest on.
    assert_ne!(
        end, control_end,
        "the wheat must be what moved the cow: it ended at {end:?} with wheat and \
         {control_end:?} without"
    );
}

/// The negative control the plan asks for, run as a real test rather than
/// described: a species with **no** roster entry gets `roster::FALLBACK`, which
/// has no `TemptGoal`, so it must fail the assertion above under identical
/// conditions.
///
/// This is the "empty the roster entry" control, expressed without editing the
/// roster: a llama is a supported species with no explicit entry, so it takes the
/// fallback path by construction, and `llama_food` exists in the jar so the
/// choice of item is not what makes it fail.
///
/// See [`run_species`] for why this compares trajectories rather than distances.
#[test]
fn a_species_with_no_roster_entry_does_not_follow_food() {
    let tempted = run_species("minecraft:llama", Some("wheat"));
    let untempted = run_species("minecraft:llama", None);
    assert_eq!(
        tempted, untempted,
        "a llama has no roster entry, so it gets FALLBACK (stroll + look) and nothing in it \
         reads the player's held item — its trajectory must be identical whether or not the \
         player holds wheat. It ended at {tempted:?} with wheat and {untempted:?} without, so \
         either the fallback is not the fallback or every species is getting the same table"
    );

    // The control, on the identical comparison: a species the roster *does* give a
    // `TemptGoal` must not be item-blind. Without this, "the two arms matched"
    // is also satisfied by perception never being fed at all.
    let cow_tempted = run_species("minecraft:cow", Some("wheat"));
    let cow_untempted = run_species("minecraft:cow", None);
    assert_ne!(
        cow_tempted, cow_untempted,
        "control: a cow's roster has TemptGoal and wheat is cow_food, so wheat must change \
         where it ends up. If a cow is item-blind too, the assertion above is vacuous"
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
/// The pig's half stays a **magnitude** assertion — it must really cross the gap,
/// not merely move — while the cow's half is the exact trajectory comparison
/// [`run_species`]'s doc comment explains. A pig and a cow cannot perceive each
/// other (`BreedGoal`/`FollowParentGoal` are same-species, and nothing else in
/// either roster reads another mob), so the pig diverging between the two arms
/// cannot move the cow.
#[test]
fn one_item_tempts_a_pig_and_not_a_cow_through_the_same_spawn_path() {
    /// Both animals in one world, as the test's own premise requires.
    fn run(held: Option<&str>) -> (f64, f64, Vec3) {
        let world = pen();
        let mut sim = MobSim::new(&world);
        let pig = sim
            .spawn_species(rk("minecraft:pig"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let cow = sim
            .spawn_species(rk("minecraft:cow"), Vec3::new(0.0, 0.0, 4.0))
            .id();
        sim.set_players(vec![match held {
            Some(item) => holding(PLAYER, item),
            None => empty_handed(PLAYER),
        }]);
        let pig_before = gap(&sim, pig, PLAYER);
        for _ in 0..TICKS {
            sim.tick();
        }
        (
            pig_before,
            gap(&sim, pig, PLAYER),
            sim.get(cow).expect("alive").position(),
        )
    }

    let (pig_before, pig_after, cow_with_potato) = run(Some("potato"));
    let (_, pig_after_empty, cow_without) = run(None);

    assert!(
        pig_after < pig_before - 3.0,
        "a potato is in pig_food, so the pig must close: {pig_before} -> {pig_after}"
    );
    // The same arm read as a difference, so "the pig closed" cannot be satisfied by
    // a stroll that would have closed anyway with the player empty-handed.
    assert_ne!(
        pig_after, pig_after_empty,
        "the pig's approach must be caused by the potato: it ended {pig_after} from the \
         player with one and {pig_after_empty} without"
    );
    assert_eq!(
        cow_with_potato, cow_without,
        "a potato is NOT in cow_food (which is exactly [wheat]), so nothing the cow does may \
         depend on the player holding one — it ended at {cow_with_potato:?} with the potato \
         and {cow_without:?} without. A difference means the species key is being ignored \
         somewhere between the roster and the perception feed"
    );
}

/// A creeper's roster is not a cow's, observed through the production path: only
/// the creeper gets `SwellGoal`, so only the creeper detonates when a target is
/// inside its swell range.
///
/// This behavior is observable through the complete server path: a player can
/// see the creeper begin swelling while the cow remains unaffected. That makes
/// it a useful second species gate in addition to the item-driven movement
/// checks above.
///
/// It also gates the priority ordering. `SwellGoal` must run before
/// `MeleeAttackGoal` can hold MOVE; if the two priority numbers are transcribed
/// in the wrong order, the creeper never swells.
#[test]
fn only_a_creeper_swells_and_vanillas_priority_order_is_preserved() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    // Within `SwellGoal`'s 9.0 squared proximity.
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
         with the cow assertion below passing, check that vanilla's own creeper goal \
         priority 2 (Swell) is still lower than 4 (Melee) in the table — a \
         MeleeAttackGoal holding MOVE prevents the swell"
    );
    assert!(
        !cow_swelled,
        "a cow must not get SwellGoal — if it does, every species is being \
         handed the same table"
    );
}
