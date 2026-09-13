use super::*;

/// A real floor — see `leash_tests::flat_world`'s own doc comment for
/// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
/// `24` on X covers this module's own `16.1`-block "just outside the
/// listener radius" control with margin.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=24 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn spawn(sim: &mut MobSim<'_>, species: &str, pos: Vec3) -> i32 {
    sim.spawn_species(format!("minecraft:{species}").parse().expect("valid key"), pos)
        .id()
}

/// Production-path proof for the door/float/malus shape fix: drives the
/// real `MobSim::spawn_species` entry point (not `species_shape` in
/// isolation) and reads back the `MobShape` a `NavigatingMob` would
/// actually path with. A vindicator opening a door is the headline case
/// from vanilla's own vindicator spawn-finalization's unconditional
/// navigation "can open doors" setter, and vanilla's own villager constructor
/// sets both `canOpenDoors` and `canFloat` unconditionally too.
#[test]
fn vindicator_and_villager_can_open_doors_and_float() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let vindicator = spawn(&mut sim, "vindicator", Vec3::new(0.0, 0.0, 0.0));
    let villager = spawn(&mut sim, "villager", Vec3::new(5.0, 0.0, 0.0));

    let vindicator_shape = sim.get(vindicator).expect("spawned").shape();
    assert!(vindicator_shape.can_open_doors);
    assert!(vindicator_shape.can_float);

    let villager_shape = sim.get(villager).expect("spawned").shape();
    assert!(villager_shape.can_open_doors);
    assert!(villager_shape.can_float);
}

/// Control: an ordinary land animal with no special-cased goals gets
/// neither flag; the defaults are off for all species in this case.
#[test]
fn a_plain_animal_still_cannot_open_doors() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let pig = spawn(&mut sim, "pig", Vec3::new(0.0, 0.0, 0.0));
    let shape = sim.get(pig).expect("spawned").shape();
    assert!(!shape.can_open_doors);
    // Pigs retain floating behavior, so this one is `true`.
    assert!(shape.can_float);
}

/// Vanilla's own bee spawn-finalization's malus table (`WATER` -1, `FENCE` -1) is the
/// path-malus behavior: `malus_overrides` has entries for
/// `.insert` calls anywhere in the workspace, so every mob pathed as if
/// nothing were dangerous. `PathType::malus`'s own default for `Water` is
/// `8.0` (costly but passable) and for `Fence` is `-1.0` already, so
/// `Water` is the discriminating field here — a bee must come back
/// strictly more averse to water than the vanilla default, not merely
/// non-zero.
#[test]
fn a_bee_s_malus_overrides_reach_the_navigating_shape() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let bee = spawn(&mut sim, "bee", Vec3::new(0.0, 0.0, 0.0));
    let shape = sim.get(bee).expect("spawned").shape();
    assert_eq!(shape.malus(PathType::Water), -1.0);
    assert_ne!(
        shape.malus(PathType::Water),
        PathType::Water.malus(),
        "bee must diverge from the un-overridden default, not coincide with it"
    );
    assert_eq!(shape.malus(PathType::Fence), -1.0);
}

/// Vanilla's own zombie spawn-finalization's door-breaking roll is a coin flip scaled by
/// regional difficulty (`random.nextFloat() < difficultyModifier * 0.1F`),
/// not a species constant — this is the control proving the roll is
/// actually wired to `spawn_special_multiplier` rather than a fixed
/// constant in either direction. At multiplier `0.0` the roll is
/// deterministically `false` for every draw (`x < 0.0` never holds for
/// `x` in `[0.0, 1.0)`), so this is exact, not statistical.
#[test]
fn zombie_door_roll_is_scaled_by_regional_difficulty_not_constant() {
    let world = flat_world();

    let mut off = MobSim::new(&world);
    off.set_spawn_difficulty(0.0, false);
    for i in 0..20 {
        let z = spawn(&mut off, "zombie", Vec3::new(i as f64 * 3.0, 0.0, 0.0));
        assert!(!off.get(z).expect("spawned").shape().can_open_doors);
    }

    // At the maximum multiplier every draw has a real (~10%) chance, so
    // spawning enough zombies must produce at least one `true` — the
    // reciprocal control to the all-`false` case above. `next_f32() <
    // 1.0 * 0.1` succeeds for roughly one in ten draws; 200 spawns makes
    // a run of all-`false` astronomically unlikely (`0.9^200 < 1e-9`)
    // without pinning to a specific seeded count.
    let mut on = MobSim::new(&world);
    on.set_spawn_difficulty(1.0, false);
    let ids: Vec<i32> = (0..200)
        .map(|i| spawn(&mut on, "husk", Vec3::new(i as f64 * 3.0, 0.0, 0.0)))
        .collect();
    let any_open = ids
        .iter()
        .any(|&id| on.get(id).expect("spawned").shape().can_open_doors);
    assert!(any_open, "expected at least one husk to roll door-breaking true at multiplier 1.0");
}

/// The roll must survive [`SimMob::set_age`]'s baby/adult shape refresh —
/// The age transition must preserve the sampled roll rather than re-derive the
/// static species default and discard a `true` value.
#[test]
fn a_zombie_s_door_roll_survives_growing_up() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_spawn_difficulty(1.0, false);
    let ids: Vec<i32> = (0..200)
        .map(|i| spawn(&mut sim, "zombie", Vec3::new(i as f64 * 3.0, 0.0, 0.0)))
        .collect();
    let id = *ids
        .iter()
        .find(|&&id| sim.get(id).expect("spawned").shape().can_open_doors)
        .expect("expected at least one zombie to roll door-breaking true at multiplier 1.0");

    sim.get_mut(id).expect("spawned").set_age(BABY_START_AGE);
    sim.get_mut(id).expect("spawned").set_age(0);

    assert!(
        sim.get(id).expect("spawned").shape().can_open_doors,
        "growing up must not reset a rolled-true door flag back to the static default"
    );
}

/// Zombie reinforcement: only the *roll* is this sim's job — see
/// `ReinforcementCall` for the decide-here/place-there split. Hard
/// difficulty, `spawn_mobs` enabled,
/// and `reinforcement_chance` pinned to `1.0` (`next_f32() < 1.0` always
/// holds in `[0.0, 1.0)`, so this is exact, not statistical) must queue
/// exactly one call carrying the zombie's own type, position and — no AI
/// target set on this mob — the attacking player's own entity id as the
/// fallback when no live target is available.
#[test]
fn a_hurt_zombie_calls_a_reinforcement_when_the_roll_passes() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_spawn_difficulty(0.0, true);
    sim.set_spawn_monsters_enabled(true);
    let id = spawn(&mut sim, "zombie", Vec3::new(0.0, 0.0, 0.0));
    sim.get_mut(id).expect("spawned").reinforcement_chance = 1.0;
    let attacker = PlayerIdentity { uuid: Uuid::new_v4(), entity_id: 777 };

    let outcome = sim.attack_from_player(
        id,
        Some(attacker),
        Vec3::new(1.0, 0.0, 0.0),
        1.0,
        DamageFlags::default(),
        0.0,
    );
    assert!(outcome.is_some_and(|o| !o.killed), "one point of damage must not kill a zombie");

    let calls = sim.take_reinforcement_calls();
    assert_eq!(calls.len(), 1, "the roll was pinned to 1.0 — it must always fire");
    assert_eq!(calls[0].entity_type.path(), "zombie");
    // Near the spawn point, not exactly on it — the hit's own mandatory
    // knockback moves the zombie before this roll reads its position, the
    // same `dealDefaultKnockback` every landed hit applies.
    let dist_sqr = calls[0].position.x.powi(2) + calls[0].position.y.powi(2) + calls[0].position.z.powi(2);
    assert!(dist_sqr < 4.0, "expected the caller's position near its spawn point, got {:?}", calls[0].position);
    assert_eq!(calls[0].target_id, 777, "falls back to the attacker with no AI target set");
}

/// **Control:** the identical setup, but the mob is only skeleton-family
/// — `reinforcement_chance` stays `0.0` for every species outside the
/// zombie family, so even a Hard-difficulty hit queues nothing. The
/// discriminating control against "the gate is difficulty alone".
#[test]
fn only_the_zombie_family_ever_calls_for_reinforcements() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_spawn_difficulty(1.0, true);
    sim.set_spawn_monsters_enabled(true);
    let id = spawn(&mut sim, "skeleton", Vec3::new(0.0, 0.0, 0.0));
    assert_eq!(
        sim.get(id).expect("spawned").reinforcement_chance(),
        0.0,
        "only the zombie family rolls a nonzero chance at spawn"
    );

    sim.attack_from_player(
        id,
        Some(PlayerIdentity { uuid: Uuid::new_v4(), entity_id: 777 }),
        Vec3::new(1.0, 0.0, 0.0),
        1.0,
        DamageFlags::default(),
        0.0,
    );
    assert!(sim.take_reinforcement_calls().is_empty());
}

/// **Control:** the identical zombie/roll setup below Hard difficulty
/// must queue nothing — `level.getDifficulty() == Difficulty.HARD` is a
/// hard gate in vanilla, not folded into the continuous chance roll, so
/// a saturated `special_multiplier` (`1.0`, Normal/Easy's ceiling) must
/// not substitute for it.
#[test]
fn a_hurt_zombie_calls_no_reinforcement_below_hard_difficulty() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_spawn_difficulty(1.0, false);
    sim.set_spawn_monsters_enabled(true);
    let id = spawn(&mut sim, "zombie", Vec3::new(0.0, 0.0, 0.0));
    sim.get_mut(id).expect("spawned").reinforcement_chance = 1.0;

    sim.attack_from_player(
        id,
        Some(PlayerIdentity { uuid: Uuid::new_v4(), entity_id: 777 }),
        Vec3::new(1.0, 0.0, 0.0),
        1.0,
        DamageFlags::default(),
        0.0,
    );
    assert!(
        sim.take_reinforcement_calls().is_empty(),
        "Hard is a hard gate, not part of the continuous chance roll"
    );
}

/// The headline case: a mob dies within 16 blocks of a warden, and the
/// same tick's `resolve_vibrations` (run after `reap_dead` posts) hands
/// the warden the death's own position as an `EntityDie` vibration.
#[test]
fn a_warden_hears_a_nearby_death_the_same_tick_it_happens() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
    let victim = spawn(&mut sim, "zombie", Vec3::new(10.0, 0.0, 0.0));
    sim.get_mut(victim).expect("spawned").health = 0.0;

    sim.tick();

    let heard = sim.get(warden).expect("spawned").nearest_vibration();
    assert_eq!(
        heard,
        Some(PostedVibration {
            position: Vec3::new(10.0, 0.0, 0.0),
            event: VibrationEvent::EntityDie,
            source: Some(victim),
        }),
        "the warden must hear the death at the victim's own position, the same tick"
    );
}

/// A death just outside the 16-block listener radius must not be heard —
/// the discriminating control against "the warden hears everything".
#[test]
fn a_death_just_outside_the_listener_radius_is_not_heard() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
    let victim = spawn(&mut sim, "zombie", Vec3::new(16.1, 0.0, 0.0));
    sim.get_mut(victim).expect("spawned").health = 0.0;

    sim.tick();

    assert_eq!(
        sim.get(warden).expect("spawned").nearest_vibration(),
        None,
        "16.1 blocks away must not be audible at the warden's 16.0 radius"
    );
}

/// A non-listener species standing right next to the same death must
/// never receive a vibration — the species filter is load-bearing, the
/// same control every other search in this file runs for its own gate.
#[test]
fn a_non_listener_species_never_receives_a_vibration() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let bystander = spawn(&mut sim, "zombie", Vec3::new(0.0, 0.0, 0.0));
    let victim = spawn(&mut sim, "zombie", Vec3::new(1.0, 0.0, 0.0));
    sim.get_mut(victim).expect("spawned").health = 0.0;

    sim.tick();

    assert_eq!(sim.get(bystander).expect("spawned").nearest_vibration(), None);
}

/// The posted log must not leak into the next tick: a warden that hears
/// a death on tick 1 must hear nothing new (and retain no stale answer)
/// on tick 2, when nothing else has died.
#[test]
fn the_posted_log_does_not_leak_into_the_next_tick() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
    let victim = spawn(&mut sim, "zombie", Vec3::new(5.0, 0.0, 0.0));
    sim.get_mut(victim).expect("spawned").health = 0.0;

    sim.tick();
    assert!(sim.get(warden).expect("spawned").nearest_vibration().is_some());

    sim.tick();
    assert_eq!(
        sim.get(warden).expect("spawned").nearest_vibration(),
        None,
        "a vibration from a prior tick must not persist once nothing new was posted"
    );
}
