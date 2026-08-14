//! Does a wandering trader actually appear, in the running game?
//!
//! # What this gate is for
//!
//! `MobSim::spawn_wandering_trader` (the entity-creation half) already had its
//! own tests; `MobSim::run_wandering_trader_spawn_cycle` (the timer/delay/
//! chance cycle and the position search around it) did not, because it did
//! not exist until issue #240's spawn-cycle pass. This is the
//! `pillager_patrols.rs` shape applied to the trader: drive the cycle through
//! its own public API — never call `spawn_wandering_trader` directly — and
//! assert the world effect (a real trader and its llamas, leashed) rather
//! than that some function ran.

use lodestone_model::Vec3;
use lodestone_server::{ChunkWorld, MobSim, PerceivedPlayer, PlayerPerception, SpawnRng};

/// A flat floor wide enough that a trader spawned up to 48 blocks from the
/// origin, in any direction, still finds solid ground under it.
fn pen() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    for x in -64..=64 {
        for z in -64..=64 {
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
        },
    }
}

/// The smallest `SpawnRng` seed search that drives both of vanilla's nested
/// rolls to a hit: `next_int(100) <= 25` (the fresh `MIN_SPAWN_CHANCE`), then
/// `next_int(10) == 0` (`spawn`'s own extra gate). Both are drawn in that
/// order and only in that order — see `run_wandering_trader_spawn_cycle`'s
/// own doc comment for why the empty-players check has to sit between them.
fn seed_for_guaranteed_spawn() -> u64 {
    for seed in 1u64..200_000 {
        let mut probe = SpawnRng::new(seed);
        if probe.next_int(100) > 25 {
            continue;
        }
        if probe.next_int(10) != 0 {
            continue;
        }
        return seed;
    }
    panic!("no seed found with a guaranteed roll in its first two draws");
}

/// A sim staged so the very next `run_wandering_trader_spawn_cycle` call
/// passes both nested countdowns and reaches the roll — `set_trader_timers`
/// is the injection point for that, mirroring `pillager_patrols.rs`'
/// `staged_sim`'s use of `set_tick_count`.
fn staged_sim(world: &ChunkWorld) -> MobSim<'_> {
    let mut sim = MobSim::new(world);
    sim.set_trader_timers(0, 0);
    sim
}

/// A guaranteed roll, a connected player, open ground: a trader must
/// actually spawn, with exactly two llamas leashed to it.
#[test]
fn a_guaranteed_roll_spawns_a_trader_with_two_leashed_llamas() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_trader_rng(SpawnRng::new(seed_for_guaranteed_spawn()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let trader_id = sim
        .run_wandering_trader_spawn_cycle(&world, true)
        .expect("every gate was staged to pass and the roll was driven to a guaranteed hit");

    let trader = sim.get(trader_id).expect("the trader must actually exist");
    assert_eq!(trader.entity_type().path(), "wandering_trader");

    let llamas: Vec<_> = sim
        .iter()
        .filter(|m| m.entity_type().path() == "trader_llama")
        .collect();
    assert_eq!(llamas.len(), 2, "vanilla's spawn escorts exactly two llamas");
    for llama in &llamas {
        assert!(
            llama.leash_holder().is_some(),
            "each escort llama must be leashed to the trader, not merely standing near it"
        );
    }
}

/// Every gate but `spawn_wandering_traders` is satisfied; the rule alone
/// must be able to veto the whole cycle, exactly as `spawn_patrols` does for
/// patrols.
#[test]
fn the_spawn_wandering_traders_rule_off_spawns_nothing() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_trader_rng(SpawnRng::new(seed_for_guaranteed_spawn()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let spawned = sim.run_wandering_trader_spawn_cycle(&world, false);
    assert_eq!(
        spawned, None,
        "the spawn_wandering_traders rule must gate the cycle"
    );
    assert_eq!(sim.len(), 0, "nothing may exist in the sim after a vetoed cycle");
}

/// A world with no connected players must decline quietly — vanilla's
/// `getRandomPlayer() == null` arm — rather than panic on an empty player
/// list or spawn a trader anchored to nothing.
#[test]
fn no_players_spawns_nothing_and_does_not_panic() {
    let world = pen();
    let mut sim = staged_sim(&world);
    sim.set_trader_rng(SpawnRng::new(seed_for_guaranteed_spawn()));

    let spawned = sim.run_wandering_trader_spawn_cycle(&world, true);
    assert_eq!(spawned, None);
    assert_eq!(sim.len(), 0);
}

/// The two nested countdowns must both actually gate the cycle, not just the
/// outer one: even a guaranteed roll must not fire before the 1200-tick poll
/// and the 24000-tick delay have both elapsed.
#[test]
fn a_call_before_either_countdown_elapses_spawns_nothing() {
    let world = pen();
    let mut sim = MobSim::new(&world);
    // Every other gate staged to pass; only the countdowns are left at
    // their real vanilla defaults (1200 / 24000).
    sim.set_trader_rng(SpawnRng::new(seed_for_guaranteed_spawn()));
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);

    let spawned = sim.run_wandering_trader_spawn_cycle(&world, true);
    assert_eq!(
        spawned, None,
        "a single call against the real 1200-tick default must not reach the roll"
    );
    assert_eq!(sim.len(), 0);
}

/// A declined roll must leave the climbing `spawn_chance` in place for next
/// time (vanilla's `data.setSpawnChance(newSpawnChance)`, unconditional,
/// versus the `setSpawnChance(25)` reset that only follows an actual spawn).
///
/// One continuous `trader_rng` stream drives both cycles (never reseeded
/// between them, unlike every other gate here), so the second cycle's draw
/// is genuinely the *next* one in sequence — found by search: draw one
/// misses the fresh 25% floor, draw two sits within the climbed 50% ceiling
/// (25 + `SPAWN_CHANCE_INCREASE`) but would have missed 25%, and draw three
/// clears `spawn`'s own extra one-in-ten gate.
#[test]
fn a_declined_roll_leaves_the_climbed_chance_in_place_for_next_time() {
    let world = pen();

    let seed = (1u64..500_000)
        .find(|&seed| {
            let mut probe = SpawnRng::new(seed);
            let miss = probe.next_int(100);
            if !(26..=100).contains(&miss) {
                return false;
            }
            let hit = probe.next_int(100);
            if hit > 50 {
                return false;
            }
            probe.next_int(10) == 0
        })
        .expect("a seed with this three-draw shape exists in range");

    let mut sim = MobSim::new(&world);
    sim.set_players(vec![watching(Vec3::new(0.0, 0.0, 0.0))]);
    sim.set_trader_rng(SpawnRng::new(seed));

    sim.set_trader_timers(0, 0);
    let first = sim.run_wandering_trader_spawn_cycle(&world, true);
    assert_eq!(first, None, "the fresh 25% chance must miss the first draw");

    sim.set_trader_timers(0, 0);
    let second = sim.run_wandering_trader_spawn_cycle(&world, true);
    assert!(
        second.is_some(),
        "the second draw sits within the climbed 50% chance (25 + 25), so \
         it must now succeed — if this fails, the climbing chance is not \
         being carried between calls"
    );
}
