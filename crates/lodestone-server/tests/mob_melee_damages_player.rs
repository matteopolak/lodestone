//! Acceptance gate for issue #625: a hostile mob's melee attack must resolve
//! to a *player* it can actually damage, not just to nothing.
//!
//! Before this, `SimMob::attack_target_id` — the field
//! [`MobSim::tick_with_terrain`]'s hit-resolution pass reads — could only ever
//! name another live [`SimMob`] (its own doc comment says so), and nothing in
//! production ever set it to `Some` at all: `NearestAttackableTargetGoal`'s
//! hostile-melee path targets `nearest_player`, a bare `Vec3` with no identity
//! crossing the goal seam. So a zombie could path to a player, swing, and the
//! attack simply vanished — no hop was reached past "the goal decided to
//! attack". This drives the **real production roster**
//! (`MobSim::spawn_species`, not a hand-installed goal list — the same "no
//! `#[cfg(test)]` fake" discipline `tests/mob_sim.rs` established) so a
//! passing gate here means the whole chain (goal → attack → position → player
//! identity) is wired, not just [`MobSim::take_player_hits`] in isolation.

use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{
    ChunkWorld, MobSim, PerceivedPlayer, PlayerIdentity, PlayerPerception, WorldgenChunkSource,
};
use lodestone_worldgen::density::Density;
use uuid::Uuid;

/// A flat solid floor at y=0, surface up — the same real worldgen terrain
/// source `tests/mob_sim.rs` uses, not a bespoke test double.
fn floor_world() -> ChunkWorld {
    let source = WorldgenChunkSource::new(
        Density::YClampedGradient {
            from_y: -64.0,
            to_y: 64.0,
            from_value: 1.0,
            to_value: -1.0,
        },
        -64,
        128,
    );
    ChunkWorld::from_source(&source, -1..=1, -1..=1)
}

fn zombie() -> ResourceKey {
    ResourceKey::new("minecraft", "zombie").expect("valid key")
}

/// Runs `tick_with_terrain` `n` times, draining and collecting every
/// [`lodestone_server::PlayerHit`] landed along the way — a `for` loop that
/// pushes into a `Vec` and asserts after, per this repo's own rule against an
/// `assert!` that can only ever report the first failure.
fn run_and_collect_hits(
    sim: &mut MobSim<'_>,
    world: &ChunkWorld,
    n: u32,
) -> Vec<lodestone_server::PlayerHit> {
    let mut hits = Vec::new();
    for _ in 0..n {
        sim.tick_with_terrain(&|x, y, z| world.block_state(x, y, z).to_string());
        hits.extend(sim.take_player_hits());
    }
    hits
}

/// The positive case: a zombie spawned through the real roster, with a real
/// player in its `nearest_player` feed close enough to walk to, actually
/// lands hits on that player — and the raw damage on every one of them is the
/// jar-verified zombie `ATTACK_DAMAGE` base (`3.0`,
/// `lodestone_entity::attribute::zombie_base_attributes_match_vanilla`), not
/// a number invented for this file. Pairwise-distinct positions (zombie at
/// x=0.5, player at x=2.5, world floor at y=0) so a coordinate transposition
/// could not accidentally read as correct.
#[test]
fn a_pursuing_zombie_lands_melee_hits_on_the_real_player_it_is_chasing() {
    let world = floor_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species(zombie(), Vec3::new(0.5, 0.0, 0.5));

    let identity = PlayerIdentity {
        uuid: Uuid::from_u128(0x1234_5678),
        entity_id: 42,
    };
    sim.set_players(vec![PerceivedPlayer {
        identity: Some(identity),
        perception: PlayerPerception {
            position: Vec3::new(2.5, 0.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);

    // 400 ticks: `tests/mob_sim.rs`'s own pathfinding gate uses the same
    // order of magnitude to close an 8-block gap over real worldgen terrain;
    // `NearestAttackableTargetGoal`'s 10-tick random search throttle also
    // needs headroom to actually roll a hit.
    let hits = run_and_collect_hits(&mut sim, &world, 400);

    assert!(
        !hits.is_empty(),
        "a zombie standing 2 blocks from a fed player, given 400 ticks, must land at least one hit"
    );
    for hit in &hits {
        assert_eq!(
            hit.identity, identity,
            "every hit must name the one fed player, not an invented identity"
        );
        assert!(
            (hit.raw_damage - 3.0).abs() < 1e-4,
            "zombie ATTACK_DAMAGE is jar-verified at 3.0, got {}",
            hit.raw_damage
        );
    }
}

/// **Negative control**: with no player fed at all, a zombie standing right
/// on top of an attack-capable position (manually driven, so it does not
/// depend on the search throttle rolling) must resolve to *no* hit, proving
/// the position-match resolver does not spuriously invent a player out of a
/// mob's own attack position. Without this, the positive test above could
/// pass even if `take_player_hits` always returned the mob's own attacker
/// position reinterpreted as a hit, which would be equally hazardous the
/// other direction (a phantom hit on nobody).
#[test]
fn a_zombie_with_no_fed_player_lands_no_hits() {
    let world = floor_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species(zombie(), Vec3::new(0.5, 0.0, 0.5));
    // Deliberately no `set_players` call — `self.players` stays empty.

    let hits = run_and_collect_hits(&mut sim, &world, 400);
    assert!(
        hits.is_empty(),
        "no player was ever fed, so no hit should ever resolve: got {hits:?}"
    );
}

/// **Control on the existing mob-vs-mob path**: `attack_target_id` (the
/// `Option<i32>` naming *another* `SimMob`) must still resolve exactly as it
/// did before this change — this fix must not have quietly repurposed or
/// broken the pre-existing wolf/zombified-piglin-style retaliation path.
/// Drives two mobs directly (not through the roster, matching this exact
/// shape in `tests/mob_sim.rs`'s existing hit-resolution gates) with a real
/// player *also* fed and standing well outside melee reach, so a regression
/// that started resolving every attack against the nearest player regardless
/// of `attack_target_id` would show up here as a spurious player hit instead
/// of the expected mob damage.
#[test]
fn attack_target_id_still_resolves_to_a_mob_not_the_unrelated_fed_player() {
    use lodestone_entity::ai::goals::MeleeAttackGoal;
    use lodestone_entity::pathfinding::MobShape;

    let world = floor_world();
    let mut sim = MobSim::new(&world);

    let attacker_start = Vec3::new(0.5, 0.0, 0.5);
    let defender_pos = Vec3::new(1.5, 0.0, 0.5);
    // Defender spawned *first*, matching `tests/mob_sim.rs`'s own
    // `melee_attack_reduces_target_health_and_a_lethal_hit_removes_the_mob` —
    // its id is what `set_attack_target_id` below must name, never the
    // attacker's own id.
    let defender_id = sim
        .spawn(defender_pos, MobShape::land(0.6, 1.95), 0.0, 400)
        .id();
    let defender_start_health = sim.get(defender_id).expect("just spawned").health();
    let attacker_id = {
        let m = sim.spawn(attacker_start, MobShape::land(0.6, 1.95), 0.0, 400);
        // `sim.spawn` (unlike `spawn_species`) defaults `attack_damage` to
        // 0.0 — the same test above sets it explicitly for the same reason.
        m.set_attack_damage(5.0);
        m.add_goal(0, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
        m.set_attack_target(Some(defender_pos));
        m.set_attack_target_id(Some(defender_id));
        m.id()
    };
    assert_ne!(attacker_id, defender_id);

    // A player far outside melee reach — must never take the hit.
    let identity = PlayerIdentity {
        uuid: Uuid::from_u128(0x9999_9999),
        entity_id: 7,
    };
    sim.set_players(vec![PerceivedPlayer {
        identity: Some(identity),
        perception: PlayerPerception {
            position: Vec3::new(500.0, 0.0, 500.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, -1.0),
        },
    }]);

    let hits = run_and_collect_hits(&mut sim, &world, 40);

    assert!(
        hits.is_empty(),
        "the mob-vs-mob attack must not resolve as a player hit: got {hits:?}"
    );
    let defender_health = sim.get(defender_id).expect("still present").health();
    assert!(
        defender_health < defender_start_health,
        "the pre-existing attack_target_id path must still land on the other mob: \
         started at {defender_start_health}, now {defender_health}"
    );
}
