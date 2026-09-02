//! Does a pillager patrol actually spawn, march, and hold together, in the
//! running game?
//!
//! # What these gates are for
//!
//! `MobSim::run_patrol_spawn_cycle` and `LongDistancePatrolGoal` are each unit
//! tested on their own (`lodestone_entity`'s `roster::ranged` tests drive the
//! goal directly; `mobs.rs`'s own module carries the spawn-cycle's structural
//! citations). Neither of those proves the two actually compose in production:
//! a spawn cycle that places real mobs and a goal that moves a mob fed the
//! right state are each a closed loop until something drives them through the
//! same public API a real tick would. Every gate here goes through
//! `MobSim::run_patrol_spawn_cycle` for the spawn half and `MobSim::tick_for`
//! for the movement half — never a hand-built `SimMob` or a goal added
//! directly.

use lodestone_model::{Difficulty, Vec3};
use lodestone_server::{ChunkWorld, MobSim, PerceivedPlayer, PlayerPerception, SimMob, SpawnRng};

/// A flat floor wide enough that a patrol spawned near the origin, and a
/// player up to 48 blocks off in any direction, both stand on solid ground.
fn pen() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -80..=80 {
        for z in -80..=80 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn watching(at: Vec3) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: at,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

/// The smallest `SpawnRng` seed search: finds a seed whose draw sequence
/// makes the patrol-attempt roll succeed, so a gate can drive the spawn
/// cycle to a guaranteed attempt instead of asserting "eventually, probably".
///
/// `run_patrol_spawn_cycle` draws **twice** before anything can spawn — the
/// countdown re-arm (`next_int(1_200)`) always runs first, unconditionally,
/// and only then the roll itself (`next_int(5)`) — so this must consume one
/// throwaway draw before checking the second, or it is testing a draw order
/// the real method never makes.
fn seed_for_guaranteed_roll() -> u64 {
    for seed in 1u64..100_000 {
        let mut probe = SpawnRng::new(seed);
        let _rearm = probe.next_int(1_200);
        if probe.next_int(5) == 0 {
            return seed;
        }
    }
    panic!("no seed found with a guaranteed roll in its second draw");
}

/// A sim staged so the very next `run_patrol_spawn_cycle` call passes every
/// gate but the RNG roll itself, which the caller still controls via the
/// seed argument.
fn staged_sim(world: &ChunkWorld) -> MobSim<'_> {
    let mut sim = MobSim::new(world);
    sim.set_tick_count(120_000);
    sim
}

// ---------------------------------------------------------------------------
// The spawn cycle
// ---------------------------------------------------------------------------

/// A guaranteed roll, a connected player, open ground: a patrol must actually
/// spawn, with exactly one leader and the rest followers, both flagged as
/// `is_patrolling`.
#[test]
fn a_guaranteed_roll_spawns_a_patrol_with_exactly_one_leader() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let spawned = sim.run_patrol_spawn_cycle(&world, true, true, Difficulty::Normal);
    assert!(
        spawned > 0,
        "every gate was staged to pass and the roll was driven to a \
         guaranteed hit; nothing spawned"
    );

    let patrol: Vec<_> = sim.iter().filter(|m| m.is_patrolling()).collect();
    assert_eq!(
        patrol.len(),
        spawned,
        "every mob this cycle placed must be flagged `is_patrolling`"
    );
    let leaders = patrol.iter().filter(|m| m.is_patrol_leader()).count();
    assert_eq!(
        leaders, 1,
        "exactly one member of a spawned patrol is the leader — got {leaders} \
         among {spawned}"
    );
    let leader = patrol
        .iter()
        .find(|m| m.is_patrol_leader())
        .expect("checked above");
    assert!(
        leader.patrol_target().is_some(),
        "`findPatrolTarget` must give the leader a real waypoint at spawn \
         time, not leave it for the next tick's census"
    );
    for follower in patrol.iter().filter(|m| !m.is_patrol_leader()) {
        assert!(
            follower.patrol_target().is_none(),
            "only the leader gets a target at spawn time — a follower's own \
             comes from the host census on its first real tick, not from \
             `run_patrol_spawn_cycle` itself"
        );
    }
}

/// Every gate but `spawn_patrols` is satisfied; the rule alone must be able
/// to veto the whole cycle.
#[test]
fn the_spawn_patrols_rule_off_spawns_nothing() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let spawned = sim.run_patrol_spawn_cycle(&world, false, true, Difficulty::Normal);
    assert_eq!(spawned, 0, "the spawn_patrols rule must gate the cycle");
}

/// Every gate but the timeline is satisfied; a world younger than the
/// `early_game.json` keyframe must never spawn a patrol, however the roll
/// lands.
#[test]
fn a_world_younger_than_the_timeline_gate_spawns_nothing() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    sim.set_tick_count(119_999);
    sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let spawned = sim.run_patrol_spawn_cycle(&world, true, true, Difficulty::Normal);
    assert_eq!(
        spawned, 0,
        "`early_game.json`'s `can_pillager_patrol_spawn` keyframe is false \
         before tick 120000 — one tick short must still refuse"
    );
}

/// `isBrightOutside` gates the whole cycle too — a night-time call must
/// refuse even with every other gate satisfied.
#[test]
fn night_time_spawns_nothing() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let spawned = sim.run_patrol_spawn_cycle(&world, true, false, Difficulty::Normal);
    assert_eq!(spawned, 0, "night must refuse regardless of every other gate");
}

/// Difficulty scales the group size — `ceil(effectiveDifficulty) + 1`
/// (vanilla's own `PatrolSpawner`). This is a *value* prediction across three
/// difficulties, not a "more mobs on harder difficulty" direction check: the
/// exact size at each one is what separates a correct table from a plausible
/// one (`docs/pillager-patrols.md` §4 has the disclosed approximation this
/// predicts against).
#[test]
fn group_size_scales_with_difficulty_as_the_jars_formula_predicts() {
    let world = pen();
    let cases = [(Difficulty::Easy, 2), (Difficulty::Normal, 3), (Difficulty::Hard, 4)];
    let mut mismatches = Vec::new();
    for (difficulty, want) in cases {
        let mut sim = staged_sim(&world);
        sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
        sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);
        let spawned = sim.run_patrol_spawn_cycle(&world, true, true, difficulty);
        if spawned != want {
            mismatches.push(format!("{difficulty:?}: got {spawned}, want {want}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "group size did not match the jar's formula: {mismatches:?}"
    );
}

// ---------------------------------------------------------------------------
// Marching: the goal reaching a real spawned mob
// ---------------------------------------------------------------------------

/// The leader spawned by a real cycle actually walks — driven by
/// `MobSim::tick_for`, the production loop, never a hand-installed goal.
#[test]
fn a_spawned_leader_marches_toward_its_own_target() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);
    sim.run_patrol_spawn_cycle(&world, true, true, Difficulty::Normal);

    let leader_id = sim
        .iter()
        .find(|m| m.is_patrol_leader())
        .map(SimMob::id)
        .expect("a patrol spawned with a leader");
    let start = sim.position(leader_id).expect("alive");

    sim.tick_for(200);

    let end = sim.position(leader_id).expect("alive");
    let moved = ((end.x - start.x).powi(2) + (end.z - start.z).powi(2)).sqrt();
    assert!(
        moved > 1.0,
        "a patrol leader with a real waypoint from `findPatrolTarget` must \
         actually walk over 200 ticks; moved only {moved} blocks"
    );
}

/// A follower spawned alongside a leader picks up the leader's target through
/// `MobSim`'s own per-tick census (`feed_perception`'s
/// `nearest_patrol_leader_target`) and marches too — the whole point of
/// #241a's group behaviour, and the one thing a leader-only test cannot see.
#[test]
fn a_spawned_follower_picks_up_the_leaders_target_and_marches() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_patrol_rng(SpawnRng::new(seed_for_guaranteed_roll()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);
    let spawned = sim.run_patrol_spawn_cycle(&world, true, true, Difficulty::Hard);
    assert!(
        spawned >= 2,
        "this gate needs at least one follower; Hard's group size ({spawned}) \
         must be at least 2 or the precondition itself is wrong"
    );

    let follower_id = sim
        .iter()
        .find(|m| m.is_patrolling() && !m.is_patrol_leader())
        .map(SimMob::id)
        .expect("a follower spawned alongside the leader");
    assert!(
        sim.get(follower_id).expect("alive").patrol_target().is_none(),
        "precondition: a follower has no target of its own until the census \
         runs"
    );
    let start = sim.position(follower_id).expect("alive");

    sim.tick_for(200);

    assert!(
        sim.get(follower_id)
            .expect("alive")
            .patrol_target()
            .is_some(),
        "the census must have adopted a target for the follower by now"
    );
    let end = sim.position(follower_id).expect("alive");
    let moved = ((end.x - start.x).powi(2) + (end.z - start.z).powi(2)).sqrt();
    assert!(
        moved > 1.0,
        "a follower that adopted a real target must actually walk over 200 \
         ticks; moved only {moved} blocks"
    );
}
