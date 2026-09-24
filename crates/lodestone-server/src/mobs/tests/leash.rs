use super::*;

/// A real floor makes living mobs fall when idle (see
/// `NavigatingMob::advance`'s no-waypoint branch). `-8..=24`/`-8..=8` covers every
/// coordinate this module's own tests spawn a mob at, with margin.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=24 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn player_at(uuid: Uuid, pos: Vec3) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid, entity_id: 99 }),
        perception: PlayerPerception {
            position: pos,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

#[test]
fn attaching_a_lead_to_a_leashable_mob_holds_it() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
        .id();

    let outcome = sim.try_leash(id, holder, true, false);
    assert_eq!(outcome, LeashOutcome::Attached);
    assert_eq!(
        sim.get(id).expect("spawned").leash_holder(),
        Some(LeashHolder::Player(holder))
    );
}

/// **Control: a hostile species refuses a lead** — vanilla's own
/// generic "can be leashed" check is "not a hostile-tagged mob", so this is the
/// discriminating input against "every mob accepts a lead".
#[test]
fn control_a_hostile_mob_refuses_a_lead() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
    let id = sim
        .spawn_species(
            "minecraft:zombie".parse().expect("valid key"),
            Vec3::new(2.0, 0.0, 0.0),
        )
        .id();

    assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Refused);
    assert_eq!(sim.get(id).expect("spawned").leash_holder(), None);
}

#[test]
fn detaching_returns_the_lead_unless_creative() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
        .id();
    assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Attached);

    assert_eq!(
        sim.try_leash(id, holder, false, false),
        LeashOutcome::Detached { dropped_lead: true },
        "survival mode must drop a lead item"
    );
    assert_eq!(sim.get(id).expect("still spawned").leash_holder(), None);
    assert_eq!(sim.item_count(), 1, "the reported drop must be a real item, not just a flag");

    assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Attached);
    assert_eq!(
        sim.try_leash(id, holder, false, true),
        LeashOutcome::Detached {
            dropped_lead: false
        },
        "creative mode (infinite materials) must not drop a lead item"
    );
    assert_eq!(
        sim.item_count(),
        1,
        "creative detach must not spawn a second item on top of the survival one"
    );
}

/// One player cannot steal another's already-leashed mob just by
/// holding a lead — vanilla's `!(leashable.getLeashHolder() instanceof
/// Player)` guard.
#[test]
fn a_different_players_lead_cannot_steal_an_already_leashed_mob() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let owner = Uuid::new_v4();
    let thief = Uuid::new_v4();
    sim.set_players(vec![
        player_at(owner, Vec3::new(0.0, 0.0, 0.0)),
        player_at(thief, Vec3::new(1.0, 0.0, 0.0)),
    ]);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
        .id();
    assert_eq!(sim.try_leash(id, owner, true, false), LeashOutcome::Attached);

    assert_eq!(sim.try_leash(id, thief, true, false), LeashOutcome::Refused);
    assert_eq!(
        sim.get(id).expect("spawned").leash_holder(),
        Some(LeashHolder::Player(owner)),
        "the mob must still belong to its original holder"
    );
}

/// **The exact pull vector, predicted, at a discriminating distance**
/// (10 blocks: past `LEASH_ELASTIC_DIST` (6) and short of
/// `LEASH_TOO_FAR_DIST` (12), so both thresholds are exercised by the
/// same fixture in opposite directions). `excess = 10 - 6 = 4`, capped
/// to `1.0`, times the `0.3` scale this port uses — `0.3` exactly, along
/// `+x` since the holder is due east.
#[test]
fn a_leashed_mob_beyond_the_elastic_distance_is_pulled_toward_its_holder() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(10.0, 0.0, 0.0))]);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(id)
        .expect("spawned")
        .set_leash_holder(Some(LeashHolder::Player(holder)));

    sim.tick_leashes();

    let mob = sim.get(id).expect("still spawned");
    assert_eq!(
        mob.position(),
        Vec3::new(0.3, 0.0, 0.0),
        "pulled 0.3 blocks toward the holder, not teleported"
    );
    assert!(mob.is_leashed(), "still leashed — this is a pull, not a snap");
}

/// **Control: within the elastic distance, nothing happens at all.**
/// Discriminates against a pull formula that fires unconditionally.
#[test]
fn control_a_leashed_mob_within_the_elastic_distance_is_not_pulled() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(3.0, 0.0, 0.0))]);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(id)
        .expect("spawned")
        .set_leash_holder(Some(LeashHolder::Player(holder)));

    sim.tick_leashes();

    assert_eq!(
        sim.get(id).expect("still spawned").position(),
        Vec3::new(0.0, 0.0, 0.0),
        "3 blocks is inside LEASH_ELASTIC_DIST (6) — no force at all"
    );
}

/// Past `LEASH_TOO_FAR_DIST` (12), the lead snaps: the mob is freed and
/// a real `minecraft:lead` item appears at its position.
#[test]
fn a_leashed_mob_beyond_the_snap_distance_drops_its_lead() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(13.0, 0.0, 0.0))]);
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(id)
        .expect("spawned")
        .set_leash_holder(Some(LeashHolder::Player(holder)));
    assert_eq!(sim.item_count(), 0, "control: nothing dropped yet");

    sim.tick_leashes();

    assert!(
        !sim.get(id).expect("still spawned").is_leashed(),
        "13 blocks is past LEASH_TOO_FAR_DIST (12) — the lead must snap"
    );
    assert_eq!(sim.item_count(), 1, "a lead item must be spawned on snap");
}

/// A leash holder that cannot be resolved (a disconnected player) drops
/// the leash silently, with no item — the disclosed simplification
/// `tick_leashes`'s own doc comment names.
#[test]
fn control_an_unresolvable_holder_drops_the_leash_without_an_item() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    // No `set_players` call — `holder` cannot be resolved to a position.
    let id = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    sim.get_mut(id)
        .expect("spawned")
        .set_leash_holder(Some(LeashHolder::Player(holder)));

    sim.tick_leashes();

    assert!(!sim.get(id).expect("still spawned").is_leashed());
    assert_eq!(sim.item_count(), 0, "an unresolvable holder must not drop an item");
}

/// Right-clicking a fence with a lead re-parents every mob leashed to
/// the player onto that fence position — vanilla's own lead-item "bind player mobs" call.
#[test]
fn leashing_to_a_fence_reparents_every_mob_the_player_was_holding() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let holder = Uuid::new_v4();
    sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
    let a = sim
        .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(1.0, 0.0, 0.0))
        .id();
    let b = sim
        .spawn_species("minecraft:sheep".parse().expect("valid key"), Vec3::new(1.0, 0.0, 1.0))
        .id();
    // A third mob leashed to someone else must not be swept up.
    let other_holder = Uuid::new_v4();
    let c = sim
        .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.0, 0.0, 2.0))
        .id();
    sim.get_mut(a).expect("spawned").set_leash_holder(Some(LeashHolder::Player(holder)));
    sim.get_mut(b).expect("spawned").set_leash_holder(Some(LeashHolder::Player(holder)));
    sim.get_mut(c)
        .expect("spawned")
        .set_leash_holder(Some(LeashHolder::Player(other_holder)));

    let fence_pos = BlockPos::new(5, 0, 5);
    let mut moved = sim.try_leash_to_fence(holder, fence_pos);
    moved.sort_unstable();
    let mut expected = vec![a, b];
    expected.sort_unstable();
    assert_eq!(moved, expected, "only the calling player's own leashed mobs move");

    assert_eq!(
        sim.get(a).expect("spawned").leash_holder(),
        Some(LeashHolder::Fence(fence_pos))
    );
    assert_eq!(
        sim.get(c).expect("spawned").leash_holder(),
        Some(LeashHolder::Player(other_holder)),
        "a mob leashed to a different player must be untouched"
    );
}

fn leash_owner_fixture() -> MobSim<'static> {
    let world = Box::leak(Box::new(flat_world()));
    let mut sim = MobSim::new(world);
    let cow = ResourceKey::from_str("minecraft:cow").expect("valid key");
    for (position, fence) in [
        (Vec3::new(-0.4, 0.5, 0.5), BlockPos::new(-10, 0, 0)),
        (Vec3::new(16.1, 0.5, 0.5), BlockPos::new(25, 0, 0)),
        (Vec3::new(-0.1, 0.5, 0.5), BlockPos::new(-20, 0, 0)),
        (Vec3::new(16.4, 0.5, 0.5), BlockPos::new(16, 0, 0)),
    ] {
        sim.spawn_species(cow.clone(), position)
            .set_leash_holder(Some(LeashHolder::Fence(fence)));
    }
    sim
}

fn leash_observation(sim: &MobSim<'_>) -> (Vec<(i32, Vec3, Option<LeashHolder>)>, usize) {
    (
        sim.mobs
            .iter()
            .map(|mob| (mob.id, mob.position(), mob.leash_holder()))
            .collect(),
        sim.item_count(),
    )
}

#[test]
fn leash_owner_batches_restore_interleaved_actions_after_reversed_completion() {
    let mut serial = leash_owner_fixture();
    let serial_batches = serial.tick_leash_owner_batches();
    assert_eq!(
        serial_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        [
            EntityTickOwner::Chunk { cx: -1, cz: 0 },
            EntityTickOwner::Chunk { cx: 1, cz: 0 },
        ]
    );
    serial.apply_leash_owner_batches(serial_batches);
    let expected = leash_observation(&serial);
    assert!((expected.0[0].1.x - -0.7).abs() < 1e-12);
    assert!((expected.0[1].1.x - 16.4).abs() < 1e-12);
    assert_eq!(expected.0[2].2, None, "the distant negative-chunk leash snaps");
    assert!(expected.0[3].2.is_some(), "the nearby leash remains attached");
    assert_eq!(expected.1, 1, "exactly the snapped leash drops an item");

    let mut completed = leash_owner_fixture();
    let mut batches = completed.tick_leash_owner_batches();
    batches.reverse();
    let raw_slots = batches
        .iter()
        .flat_map(|batch| batch.effects.iter())
        .map(|effect| effect.serial)
        .collect::<Vec<_>>();
    assert_eq!(raw_slots, vec![1, 3, 0, 2], "control must interleave owners");
    completed.apply_leash_owner_batches(batches);
    assert_eq!(leash_observation(&completed), expected);
}

#[test]
#[should_panic(expected = "every tick-start owner batch exactly once")]
fn leash_owner_batch_merge_rejects_a_missing_owner() {
    let mut sim = leash_owner_fixture();
    let mut batches = sim.tick_leash_owner_batches();
    batches.pop();
    let _ = merge_leash_tick_owner_batches(batches);
}

#[test]
#[should_panic(expected = "may not contain one owner twice")]
fn leash_owner_batch_merge_rejects_a_duplicate_owner() {
    let mut sim = leash_owner_fixture();
    let mut batches = sim.tick_leash_owner_batches();
    batches[1] = batches[0].clone();
    let _ = merge_leash_tick_owner_batches(batches);
}

fn dense_leash_owner_fixture(count: usize) -> MobSim<'static> {
    let world = Box::leak(Box::new(flat_world()));
    let mut sim = MobSim::new(world);
    let cow = ResourceKey::from_str("minecraft:cow").expect("valid key");
    let mut ids = Vec::with_capacity(count);
    for serial in 0..count {
        let base = [-0.4, 16.1, 32.1, 48.1][serial % 4];
        ids.push(
            sim.spawn_species(cow.clone(), Vec3::new(base, 0.5, (serial / 4) as f64 * 0.01))
                .id(),
        );
    }
    for (serial, id) in ids.iter().copied().enumerate() {
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Mob(ids[(serial + 1) % ids.len()])));
    }
    sim
}

fn leash_effect_observation(batches: &[LeashTickOwnerBatch]) -> Vec<(usize, i32, LeashTickAction)> {
    let mut effects: Vec<_> = batches
        .iter()
        .flat_map(|batch| batch.effects.iter())
        .map(|effect| (effect.serial, effect.id, effect.action))
        .collect();
    effects.sort_unstable_by_key(|(serial, _, _)| *serial);
    effects
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn parallel_leash_owner_batches_match_one_lane_with_interleaved_negative_owners() {
    let serial = dense_leash_owner_fixture(256);
    let expected = leash_effect_observation(&serial.tick_leash_owner_batches_with_workers(1));

    let parallel = dense_leash_owner_fixture(256);
    let completed = parallel.tick_leash_owner_batches_with_workers(4);
    assert_eq!(
        leash_effect_observation(&completed),
        expected,
        "four source owners must produce the same serial slots and actions as one lane"
    );
    assert_eq!(
        completed.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        [
            EntityTickOwner::Chunk { cx: -1, cz: 0 },
            EntityTickOwner::Chunk { cx: 1, cz: 0 },
            EntityTickOwner::Chunk { cx: 2, cz: 0 },
            EntityTickOwner::Chunk { cx: 3, cz: 0 },
        ],
        "the parity scene must span an interleaved negative owner plus three positive owners"
    );
}

#[test]
#[should_panic(expected = "latest tick-start plan")]
fn leash_owner_batches_reject_stale_plan_completions() {
    let mut sim = leash_owner_fixture();
    let stale = sim.tick_leash_owner_batches();
    let _current = sim.tick_leash_owner_batches();
    sim.apply_leash_owner_batches(stale);
}

#[test]
#[should_panic(expected = "already applied tick-start plan")]
fn leash_owner_batches_reject_replayed_completions() {
    let mut sim = leash_owner_fixture();
    let batches = sim.tick_leash_owner_batches();
    sim.apply_leash_owner_batches(batches.clone());
    sim.apply_leash_owner_batches(batches);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "manual dense-scene throughput measurement"]
fn measure_dense_leash_owner_workers() {
    for leash_count in [256, 2_048] {
        let sim = dense_leash_owner_fixture(leash_count);
        let started = lodestone_time::Instant::now();
        let _ = sim.tick_leash_owner_batches_with_workers(1);
        let serial = started.elapsed();
        let started = lodestone_time::Instant::now();
        let _ = sim.tick_leash_owner_batches_with_workers(4);
        let parallel = started.elapsed();
        eprintln!(
            "dense_leash owners=4 leashes={leash_count} serial_ms={:.3} parallel_ms={:.3} speedup={:.3}",
            serial.as_secs_f64() * 1_000.0,
            parallel.as_secs_f64() * 1_000.0,
            serial.as_secs_f64() / parallel.as_secs_f64()
        );
    }
}
