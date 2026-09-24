use super::*;

fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

/// A host teleport command rewrites position immediately and
/// survives the next tick — an instant relocation, not a fast walk.
#[test]
fn teleport_to_moves_the_mob_instantly_and_survives_a_tick() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:enderman").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    // Inside this module's own solid floor (`-8..=8` on both axes) —
    // `MobSim::teleport_to` (unlike the goal-driven enderman blink) is
    // the raw, unvalidated primitive and always lands exactly on
    // target, but a target with no ground under it would now correctly
    // start falling on the very next tick (idle mobs have real gravity,
    // see `NavigatingMob::advance`), which is not what this test means
    // to exercise.
    let target = Vec3::new(5.0, 0.0, 5.0);
    sim.get_mut(id).expect("alive").teleport_to(target);
    assert_eq!(
        sim.position(id),
        Some(target),
        "teleport must move the mob to exactly the target"
    );

    sim.tick();
    assert_eq!(
        sim.position(id),
        Some(target),
        "a tick after teleport must not undo it"
    );
}

/// A `damage_self` request is drained by [`MobSim::tick`] and resolved into
/// real health change. A bee that damages itself for its full health is
/// gone at the end of the same tick.
#[test]
fn damage_self_is_resolved_into_a_real_self_kill() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:bee").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    let health = sim.get(id).expect("alive").health();

    sim.get_mut(id).expect("alive").damage_self(health);
    assert_eq!(
        sim.get(id).expect("alive").health(),
        health,
        "the request alone must not change health — only the tick drain resolves it"
    );
    sim.tick();
    assert!(
        sim.get(id).is_none(),
        "a mob that damaged itself for its full health must be removed by \
         the end of the tick"
    );
}

/// An owner id set on the host resolves to an owner *position*
/// across the seam each tick.
#[test]
fn owner_id_resolves_to_an_owner_position_across_the_seam() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let wolf = ResourceKey::from_str("minecraft:wolf").expect("valid key");
    let owner_id = sim.spawn_species(wolf.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
    let pet_id = sim.spawn_species(wolf, Vec3::new(3.0, 0.0, 3.0)).id();
    sim.get_mut(pet_id).expect("alive").set_owner_id(Some(owner_id));

    assert_eq!(
        sim.get(pet_id).expect("alive").owner_position(),
        None,
        "before the first tick the seam has not resolved the owner"
    );

    sim.tick();
    let owner_pos = sim.get(owner_id).expect("alive").position();
    assert_eq!(
        sim.get(pet_id).expect("alive").owner_position(),
        Some(owner_pos),
        "the feed must resolve the owner id to the owner's current position"
    );
}

/// The gaze feed reaches `is_being_stared_at` through the integrated
/// simulation, not only through isolated entity-level checks.
///
/// **The discriminating pair**: two sims, each with one enderman at the
/// *identical* position and one player at the *identical* position — the
/// only difference is the player's `view_direction`. A gate that only
/// varied position (closer/farther) could not tell a real gaze test from
/// a distance check; this one cannot vary anything else, because nothing
/// else differs.
#[test]
fn the_gaze_feed_reaches_is_being_stared_at_and_a_look_away_does_not() {
    let world = flat_world();
    let player_pos = Vec3::new(0.0, 0.0, 0.0);
    let enderman_pos = Vec3::new(0.0, 0.0, 10.0);

    // Resolve the real eye positions the feed itself uses, rather than
    // guessing them — `feed_perception`'s own formula for the mob eye
    // (`height * 0.85`) and `PLAYER_EYE_HEIGHT` for the player.
    let mut probe = MobSim::new(&world);
    let probe_id = probe
        .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
        .id();
    let mob_eye_height = f64::from(probe.get(probe_id).expect("spawned").shape().height) * 0.85;
    let mob_eye = Vec3::new(enderman_pos.x, enderman_pos.y + mob_eye_height, enderman_pos.z);
    let player_eye = Vec3::new(player_pos.x, player_pos.y + PLAYER_EYE_HEIGHT, player_pos.z);
    let delta = Vec3::new(mob_eye.x - player_eye.x, mob_eye.y - player_eye.y, mob_eye.z - player_eye.z);
    let dist = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
    let looking_at = Vec3::new(delta.x / dist, delta.y / dist, delta.z / dist);
    // Exactly opposite the enderman — as far outside the cone as a unit
    // vector can be (`dot == -1`), not a near-miss.
    let looking_away = Vec3::new(-looking_at.x, -looking_at.y, -looking_at.z);

    // The naive (non-distance-adjusted) hypothesis this feed's own doc
    // warns against: reading `coneSize` (0.025) as the tolerance
    // directly gives threshold `1.0 - 0.025 = 0.975`. At `looking_at`
    // (`dot == 1.0`) both the naive and the real (`1.0 - 0.025/dist`)
    // hypotheses agree — accepted either way — which is exactly why the
    // boundary case belongs to `lodestone_entity`'s own
    // `is_in_view_cone_boundary_at_the_endermans_own_cone_size` gate and
    // not here; this test's job is only "does the feed reach the goal
    // at all", which a dead-on look and a dead-opposite one already
    // settle without needing a razor's-edge input.
    assert!(dist > 1.0, "the fixture must not degenerate to zero distance: dist={dist}");

    let mut watched = MobSim::new(&world);
    let watched_id = watched
        .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
        .id();
    watched.set_players(vec![PlayerPerception {
        position: player_pos,
        held_item: None,
        view_direction: looking_at,
    }]);
    watched.tick();
    assert!(
        watched.get(watched_id).expect("alive").mob.is_being_stared_at(),
        "a player looking straight at the enderman must set is_being_stared_at"
    );

    let mut unwatched = MobSim::new(&world);
    let unwatched_id = unwatched
        .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
        .id();
    unwatched.set_players(vec![PlayerPerception {
        position: player_pos,
        held_item: None,
        view_direction: looking_away,
    }]);
    unwatched.tick();
    assert!(
        !unwatched.get(unwatched_id).expect("alive").mob.is_being_stared_at(),
        "a player looking directly away, from the identical position, must not"
    );
}
