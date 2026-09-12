// Also home to the death-loot gate, which reuses this module's
// `flat_world` rather than growing a second copy of it.
use super::*;

/// A floor wide enough for a mob at the origin and a player out past 36
/// blocks, so nothing here depends on a mob standing in the void.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=48 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

/// Spawns `species` at the origin through the **production** path
/// ([`MobSim::spawn_species`], what `seed_demo_mobs` calls), feeds one player
/// `distance` blocks away on +X, and reports whether the mob ever acquires a
/// target within `ticks`.
///
/// `attack_target()` is the observable, not `can_use`: target acquisition
/// is throttled, so this ticks a
/// generous bound and checks after each — a fixed single tick would measure
/// the throttle rather than the range.
fn acquires_at(species: &str, distance: f64, ticks: usize) -> bool {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.set_players(vec![PlayerPerception {
        position: Vec3::new(distance, 0.0, 0.0),
        held_item: None,
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }]);
    for _ in 0..ticks {
        sim.tick();
        if sim.get(id).expect("alive").attack_target().is_some() {
            return true;
        }
    }
    false
}

/// `push_impulse`'s exact vanilla formula, at an off-axis input chosen so
/// two plausible implementations diverge: Chebyshev normalisation
/// (`sqrt(max(|dx|, |dz|))`, what `Entity::push` actually does) against
/// the more obvious Euclidean one (`sqrt(dx² + dz²)`, what a port that
/// "corrected" the formula would produce). `dx=0.3, dz=0.4` was chosen
/// precisely because `max(0.3, 0.4) = 0.4 != dx² + dz² = 0.25`, so the
/// two hypotheses give different numbers and this input can actually
/// tell them apart — a symmetric or on-axis pair could not.
#[test]
fn push_impulse_matches_a_hand_computed_off_axis_example_not_the_euclidean_alternative() {
    let p_i = Vec3::new(0.0, 0.0, 0.0);
    let p_j = Vec3::new(0.3, 0.0, 0.4);

    // Hand-computed from `Entity::push`'s own arithmetic, independent of
    // `push_impulse`'s implementation:
    //   dd = max(0.3, 0.4) = 0.4; dd = sqrt(0.4) = 0.632455532...
    //   xa = 0.3 / dd = 0.474341649...; za = 0.4 / dd = 0.632455532...
    //   pow = min(1.0, 1.0 / dd) = 1.0 (1/dd ≈ 1.581 > 1)
    //   xa *= 0.05 = 0.023717082...; za *= 0.05 = 0.031622777...
    let dd = 0.4f64.sqrt();
    let expected_xa = (0.3 / dd) * 0.05;
    let expected_za = (0.4 / dd) * 0.05;

    // The wrong hypothesis, evaluated first: Euclidean normalisation
    // (dist = sqrt(0.3² + 0.4²) = 0.5) gives a different pair of numbers.
    let euclid_dist = (0.3f64 * 0.3 + 0.4 * 0.4).sqrt();
    let wrong_xa = (0.3 / euclid_dist) * 0.05;
    let wrong_za = (0.4 / euclid_dist) * 0.05;
    assert!(
        (expected_xa - wrong_xa).abs() > 1.0e-4 && (expected_za - wrong_za).abs() > 1.0e-4,
        "precondition: the chosen input must actually separate the two \
         hypotheses, got chebyshev=({expected_xa}, {expected_za}) \
         euclidean=({wrong_xa}, {wrong_za})"
    );

    let (impulse_i, impulse_j) =
        push_impulse(p_i, p_j, 1.0).expect("well within the touch threshold");
    assert!(
        (impulse_i.x - -expected_xa).abs() < 1.0e-9 && (impulse_i.z - -expected_za).abs() < 1.0e-9,
        "p_i must be pushed away from p_j by the Chebyshev-derived amount, \
         expected ({}, {}), got ({}, {})",
        -expected_xa,
        -expected_za,
        impulse_i.x,
        impulse_i.z
    );
    assert!(
        (impulse_j.x - expected_xa).abs() < 1.0e-9 && (impulse_j.z - expected_za).abs() < 1.0e-9,
        "p_j must be pushed away from p_i by the same magnitude, opposite \
         sign, expected ({expected_xa}, {expected_za}), got ({}, {})",
        impulse_j.x,
        impulse_j.z
    );
    assert!(
        (impulse_i.x - wrong_xa).abs() > 1.0e-4,
        "the Euclidean hypothesis must NOT match — if it does, the \
         normalisation silently changed to the wrong formula"
    );
}

/// The control an absence assertion needs: a pair separated just beyond
/// the touch threshold must produce no impulse at all, proving the
/// detector (the overlap test) actually fires rather than being
/// unconditionally permissive.
#[test]
fn push_impulse_is_none_just_outside_the_touch_threshold() {
    let p_i = Vec3::new(0.0, 0.0, 0.0);
    let p_j = Vec3::new(0.61, 0.0, 0.0);
    assert!(
        push_impulse(p_i, p_j, 0.6).is_none(),
        "0.61 blocks apart must not touch at a 0.6 threshold"
    );
    // And the positive control, one hair inside: proves 0.6 is not simply
    // always None.
    let p_k = Vec3::new(0.59, 0.0, 0.0);
    assert!(
        push_impulse(p_i, p_k, 0.6).is_some(),
        "0.59 blocks apart must touch at a 0.6 threshold"
    );
}

/// Wires `push_impulse` into the real per-tick sim: two overlapping mobs
/// must actually separate over real ticks, and a third mob spawned well
/// outside touch range is the control that must not move at all — an
/// absence assertion (mob-mob pushing is un-wired) needs a detector
/// proven to fire, which the near pair provides.
#[test]
fn overlapping_mobs_separate_over_real_ticks_and_a_distant_one_does_not_move() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:pig").expect("valid key");
    let near_a = sim.spawn_species(key.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
    let near_b = sim.spawn_species(key.clone(), Vec3::new(0.3, 0.0, 0.0)).id();
    // Inside `flat_world`'s own `-8..=48` solid floor (with margin, so an
    // idle pig standing on real ground does not also start falling —
    // idle mobs have real gravity now, see `NavigatingMob::advance` —
    // which would be a second, unrelated source of displacement this
    // control does not mean to exercise).
    let far = sim.spawn_species(key, Vec3::new(45.0, 0.0, 0.0)).id();

    for _ in 0..20 {
        sim.tick();
    }

    let gap = (sim.get(near_a).expect("alive").position()
        - sim.get(near_b).expect("alive").position())
    .x
    .abs();
    assert!(
        gap > 0.3,
        "two overlapping pigs must separate over 20 ticks of pushing, \
         gap only grew to {gap}"
    );
    assert_eq!(
        sim.get(far).expect("alive").position(),
        Vec3::new(45.0, 0.0, 0.0),
        "control: a pig with nothing nearby must not be displaced by the \
         push pass at all"
    );
}

/// The primary reported symptom ("I can't push entities like pigs"): a
/// player walking into a mob must shove it out of the way. Player recoil
/// is deliberately not asserted — see `MobSim::push_entities`'s own doc
/// comment for why that half needs a different seam.
#[test]
fn a_player_overlapping_a_mob_pushes_the_mob_away() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:pig").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.set_players(vec![PlayerPerception {
        position: Vec3::new(0.2, 0.0, 0.0),
        held_item: None,
        view_direction: Vec3::new(0.0, 0.0, 1.0),
    }]);

    for _ in 0..10 {
        sim.tick();
    }

    let moved = sim.get(id).expect("alive").position();
    assert!(
        moved.x < 0.0,
        "a player standing at +x inside the pig's body must push the pig \
         toward -x; pig ended at x={}",
        moved.x
    );
}

fn push_owner_fixture() -> MobSim<'static> {
    let world = Box::leak(Box::new(flat_world()));
    let mut sim = MobSim::new(world);
    let pig = ResourceKey::from_str("minecraft:pig").expect("valid key");
    for position in [
        Vec3::new(-0.4, 0.0, 0.0),
        Vec3::new(-0.1, 0.0, 0.0),
        Vec3::new(16.1, 0.0, 0.0),
        Vec3::new(16.4, 0.0, 0.0),
    ] {
        sim.spawn_species(pig.clone(), position);
    }
    sim
}

fn mob_velocities(sim: &MobSim<'_>) -> Vec<(i32, Vec3)> {
    sim.mobs.iter().map(|mob| (mob.id, mob.velocity())).collect()
}

#[test]
fn entity_push_owner_batches_restore_entity_order_after_reversed_completion() {
    let mut serial = push_owner_fixture();
    let serial_batches = serial.tick_entity_push_owner_batches();
    assert_eq!(
        serial_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        [
            EntityTickOwner::Chunk { cx: -1, cz: 0 },
            EntityTickOwner::Chunk { cx: 1, cz: 0 },
        ],
        "negative fractional positions use Euclidean chunk ownership"
    );
    serial.apply_entity_push_owner_batches(serial_batches);
    let expected = mob_velocities(&serial);

    let mut completed = push_owner_fixture();
    let mut batches = completed.tick_entity_push_owner_batches();
    batches.reverse();
    let raw_slots = batches
        .iter()
        .flat_map(|batch| batch.effects.iter())
        .map(|effect| effect.serial)
        .collect::<Vec<_>>();
    assert_ne!(raw_slots, vec![0, 1, 2, 3], "control must actually reorder completion slots");
    completed.apply_entity_push_owner_batches(batches);
    assert_eq!(mob_velocities(&completed), expected);
}

#[test]
#[should_panic(expected = "every tick-start owner batch exactly once")]
fn entity_push_owner_batch_merge_rejects_a_missing_owner() {
    let sim = push_owner_fixture();
    let mut batches = sim.tick_entity_push_owner_batches();
    batches.pop();
    let _ = merge_entity_push_owner_batches(batches);
}

#[test]
#[should_panic(expected = "may not contain one owner twice")]
fn entity_push_owner_batch_merge_rejects_a_duplicate_owner() {
    let sim = push_owner_fixture();
    let mut batches = sim.tick_entity_push_owner_batches();
    batches[1] = batches[0].clone();
    let _ = merge_entity_push_owner_batches(batches);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn bounded_owner_executor_runs_two_lanes_at_the_same_time() {
    use std::sync::{Arc, Barrier};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let barrier = Arc::new(Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let results = crate::tick_region::run_bounded_owner_jobs(vec![1_u8, 2], 2, &|job| {
        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
        maximum.fetch_max(now, Ordering::SeqCst);
        barrier.wait();
        active.fetch_sub(1, Ordering::SeqCst);
        job
    });
    assert_eq!(results, vec![1, 2], "the executor restores submission order");
    assert_eq!(maximum.load(Ordering::SeqCst), 2, "both worker lanes must overlap");
}

fn dense_push_owner_fixture(count: usize) -> MobSim<'static> {
    let world = Box::leak(Box::new(flat_world()));
    let mut sim = MobSim::new(world);
    let pig = ResourceKey::from_str("minecraft:pig").expect("valid key");
    for serial in 0..count {
        let base = [-0.4, 0.4, 16.4, 32.4][serial % 4];
        let offset = f64::from((serial / 4 % 5) as u8) * 0.01;
        sim.spawn_species(pig.clone(), Vec3::new(base + offset, 0.0, 0.0));
    }
    sim
}

fn serial_pair_push_impulses(sim: &MobSim<'_>) -> Vec<Vec3> {
    let positions: Vec<Vec3> = sim.mobs.iter().map(SimMob::position).collect();
    let widths: Vec<f64> = sim
        .mobs
        .iter()
        .map(|mob| f64::from(mob.shape().width))
        .collect();
    let mut impulses = vec![Vec3::default(); sim.mobs.len()];
    for first in 0..sim.mobs.len() {
        for second in (first + 1)..sim.mobs.len() {
            let touch = (widths[first] + widths[second]) / 2.0;
            if let Some((first_impulse, second_impulse)) =
                push_impulse(positions[first], positions[second], touch)
            {
                impulses[first].x += first_impulse.x;
                impulses[first].z += first_impulse.z;
                impulses[second].x += second_impulse.x;
                impulses[second].z += second_impulse.z;
            }
        }
    }
    impulses
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn parallel_entity_push_matches_the_serial_owner_plan() {
    let serial = dense_push_owner_fixture(160);
    let expected: Vec<_> = serial
        .mobs
        .iter()
        .map(|mob| mob.id)
        .zip(serial_pair_push_impulses(&serial))
        .collect();

    let mut parallel = dense_push_owner_fixture(160);
    let parallel_batches = parallel.tick_entity_push_owner_batches_with_workers(4);
    parallel.apply_entity_push_owner_batches(parallel_batches);
    assert_eq!(mob_velocities(&parallel), expected);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "manual dense-scene throughput measurement"]
fn measure_dense_entity_push_region_workers() {
    let sim = dense_push_owner_fixture(2_048);
    let started = lodestone_time::Instant::now();
    let _ = serial_pair_push_impulses(&sim);
    let serial = started.elapsed();
    let started = lodestone_time::Instant::now();
    let _ = sim.tick_entity_push_owner_batches_with_workers(4);
    let parallel = started.elapsed();
    eprintln!(
        "dense_entity_push entities=2048 owners=4 serial_ms={:.3} parallel_ms={:.3} speedup={:.3}",
        serial.as_secs_f64() * 1_000.0,
        parallel.as_secs_f64() * 1_000.0,
        serial.as_secs_f64() / parallel.as_secs_f64()
    );
}

fn burn_owner_fixture() -> MobSim<'static> {
    let world = Box::leak(Box::new(flat_world()));
    let mut sim = MobSim::new(world);
    let pig = ResourceKey::from_str("minecraft:pig").expect("valid key");
    for position in [
        Vec3::new(-0.4, 0.0, 0.0),
        Vec3::new(16.1, 0.0, 0.0),
        Vec3::new(-0.1, 0.0, 0.0),
        Vec3::new(16.4, 0.0, 0.0),
    ] {
        sim.spawn_species(pig.clone(), position)
            .ignite_for_seconds(1.0);
    }
    sim
}

fn burn_observation(sim: &MobSim<'_>) -> Vec<(i32, f32, i32)> {
    sim.mobs
        .iter()
        .map(|mob| (mob.id, mob.health, mob.burn.remaining()))
        .collect()
}

fn dense_burn_owner_fixture(count: usize) -> MobSim<'static> {
    let world = Box::leak(Box::new(flat_world()));
    let mut sim = MobSim::new(world);
    let pig = ResourceKey::from_str("minecraft:pig").expect("valid key");
    for serial in 0..count {
        let x = [-0.4, 16.1, 32.1, 48.1][serial % 4];
        sim.spawn_species(pig.clone(), Vec3::new(x, 0.0, (serial / 4 % 4) as f64))
            .ignite_for_seconds(1.0);
    }
    sim
}

#[test]
fn burn_owner_batches_restore_entity_order_after_reversed_completion() {
    let mut serial = burn_owner_fixture();
    let serial_batches = serial.tick_burning_owner_batches();
    assert_eq!(
        serial_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        [
            EntityTickOwner::Chunk { cx: -1, cz: 0 },
            EntityTickOwner::Chunk { cx: 1, cz: 0 },
        ],
        "negative fractional positions use Euclidean chunk ownership"
    );
    serial.apply_burning_owner_batches(serial_batches);
    let expected = burn_observation(&serial);
    assert!(
        expected
            .iter()
            .all(|(_, health, remaining)| *health == 9.0 && *remaining == 19),
        "a 20-tick ignition must damage at remaining 20, then decrement to 19: {expected:?}"
    );

    let mut completed = burn_owner_fixture();
    let mut batches = completed.tick_burning_owner_batches();
    batches.reverse();
    let raw_slots = batches
        .iter()
        .flat_map(|batch| batch.effects.iter())
        .map(|effect| effect.serial)
        .collect::<Vec<_>>();
    assert_eq!(raw_slots, vec![1, 3, 0, 2], "control must interleave owners");
    completed.apply_burning_owner_batches(batches);
    assert_eq!(burn_observation(&completed), expected);
}

#[test]
#[should_panic(expected = "every tick-start owner batch exactly once")]
fn burn_owner_batch_merge_rejects_a_missing_owner() {
    let mut sim = burn_owner_fixture();
    let mut batches = sim.tick_burning_owner_batches();
    batches.pop();
    let _ = merge_burn_tick_owner_batches(batches);
}

#[test]
#[should_panic(expected = "may not contain one owner twice")]
fn burn_owner_batch_merge_rejects_a_duplicate_owner() {
    let mut sim = burn_owner_fixture();
    let mut batches = sim.tick_burning_owner_batches();
    batches[1] = batches[0].clone();
    let _ = merge_burn_tick_owner_batches(batches);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn parallel_burn_owner_batches_match_one_lane_with_interleaved_negative_owners() {
    let mut serial = dense_burn_owner_fixture(256);
    serial.burn_owner_plan = 1;
    let serial_batches = serial.tick_burning_owner_batches_with_workers(1);
    serial.apply_burning_owner_batches(serial_batches);

    let mut parallel = dense_burn_owner_fixture(256);
    parallel.burn_owner_plan = 1;
    let parallel_batches = parallel.tick_burning_owner_batches_with_workers(4);
    assert_eq!(
        parallel_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        [
            EntityTickOwner::Chunk { cx: -1, cz: 0 },
            EntityTickOwner::Chunk { cx: 1, cz: 0 },
            EntityTickOwner::Chunk { cx: 2, cz: 0 },
            EntityTickOwner::Chunk { cx: 3, cz: 0 },
        ],
        "the parity scene must span an interleaved negative owner plus three positive owners"
    );
    parallel.apply_burning_owner_batches(parallel_batches);
    assert_eq!(
        burn_observation(&parallel),
        burn_observation(&serial),
        "four burn-counter owners must preserve the one-lane health and counter result"
    );
}

#[test]
#[should_panic(expected = "latest tick-start plan")]
fn burn_owner_batches_reject_stale_plan_completions() {
    let mut sim = burn_owner_fixture();
    let stale = sim.tick_burning_owner_batches();
    let _current = sim.tick_burning_owner_batches();
    sim.apply_burning_owner_batches(stale);
}

#[test]
#[should_panic(expected = "already applied tick-start plan")]
fn burn_owner_batches_reject_replayed_completions() {
    let mut sim = burn_owner_fixture();
    let batches = sim.tick_burning_owner_batches();
    sim.apply_burning_owner_batches(batches.clone());
    sim.apply_burning_owner_batches(batches);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "manual dense-scene throughput measurement"]
fn measure_dense_burn_owner_workers() {
    for mob_count in [128, 256, 512, 1_024, 2_048] {
        let sim = dense_burn_owner_fixture(mob_count);
        let started = lodestone_time::Instant::now();
        let _ = sim.tick_burning_owner_batches_with_workers(1);
        let serial = started.elapsed();
        let started = lodestone_time::Instant::now();
        let _ = sim.tick_burning_owner_batches_with_workers(4);
        let parallel = started.elapsed();
        eprintln!(
            "dense_burn owners=4 mobs={mob_count} serial_ms={:.3} parallel_ms={:.3} speedup={:.3}",
            serial.as_secs_f64() * 1_000.0,
            parallel.as_secs_f64() * 1_000.0,
            serial.as_secs_f64() / parallel.as_secs_f64()
        );
    }
}

/// A killed mob drops its loot table's items.
///
/// The expected values are independent of the roller: two pools use
/// `rolls: 1`, leather uses `uniform 0..2`, and beef uses `uniform 1..3`.
/// A kill therefore yields at least beef; the leather stack may be absent,
/// while the beef count is never zero.
#[test]
fn a_killed_cow_drops_its_loot_table() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(1.0);

    let outcome = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
        .expect("the cow is a live target");
    assert!(outcome.killed);

    let dropped = sim.dropped_items();
    assert!(
        !dropped.is_empty(),
        "a cow's death must drop something — entities/cow.json guarantees the beef pool"
    );
    for (item, count) in &dropped {
        assert!(
            matches!(item.as_str(), "minecraft:leather" | "minecraft:beef"),
            "cow.json names only leather and beef, got {item}"
        );
        assert!(*count > 0, "a zero-count stack must be filtered, got {item}");
    }
    assert!(
        dropped.iter().any(|(item, _)| item == "minecraft:beef"),
        "the beef pool is `rolls: 1` with `uniform 1..3`, so it is never absent: {dropped:?}"
    );
}

/// Armadillo roll-up, gated through the production path
/// (`MobSim::attack`, `crate::server::apply_attack`'s entry point) rather
/// than calling `apply_damage` on a bare `SimMob` — the
/// same standard this crate applies to every other combat gate. A first
/// hit lands at full strength and switches the armadillo to "scared";
/// a **second** hit, once the invulnerability window has cleared, is
/// halved via `(damage - 1) / 2`.
#[test]
fn armadillo_rolls_up_after_a_hit_and_halves_the_next_one() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:armadillo").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(100.0);

    assert!(
        !sim.get(id).expect("alive").armadillo_is_scared(),
        "precondition: a fresh armadillo is not scared"
    );

    let first = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
        .expect("the armadillo is a live target");
    assert_eq!(first.damage_dealt, 10.0, "the triggering hit itself is not reduced");
    assert!(
        sim.get(id).expect("alive").armadillo_is_scared(),
        "a hit that passes the i-frame gate must roll the armadillo up"
    );

    // Clear the 20-tick invulnerability window (see `HurtCooldown::on_hurt`)
    // so the second hit is not merely dropped as "not stronger".
    for _ in 0..11 {
        sim.tick();
    }
    assert!(
        sim.get(id).expect("alive").armadillo_is_scared(),
        "11 of 80 danger ticks must not have expired the scared state yet"
    );

    let second = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
        .expect("still a live target");
    assert_eq!(
        second.damage_dealt, 4.5,
        "Armadillo.hurtServer: (10.0 - 1.0) / 2.0 == 4.5, while scared"
    );
}

/// **Control**: the identical fixture and hit sequence against a cow —
/// a species `apply_damage`'s armadillo branch must never touch — proves
/// the halving is armadillo-specific rather than a general "second hit is
/// cheaper" bug the first test could not, by itself, distinguish from a
/// mistake in the i-frame/topup arithmetic.
#[test]
fn only_armadillo_halves_a_repeat_hit_a_cow_does_not() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(100.0);

    sim.attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
        .expect("live target");
    for _ in 0..11 {
        sim.tick();
    }
    let second = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
        .expect("still live");
    assert_eq!(second.damage_dealt, 10.0, "a cow never rolls up, so the second hit is unreduced");
}

/// The danger memory expires 80 ticks after the hit that (re)armed it,
/// with nothing further landing in between — `Armadillo`'s own
/// `DANGER_DETECTED_RECENTLY` memory duration — and the armadillo
/// un-scares on schedule rather than staying curled forever.
#[test]
fn armadillo_unscares_eighty_ticks_after_its_last_hit() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:armadillo").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(100.0);

    sim.attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
        .expect("live target");
    assert!(sim.get(id).expect("alive").armadillo_is_scared());

    for _ in 0..79 {
        sim.tick();
    }
    assert!(
        sim.get(id).expect("alive").armadillo_is_scared(),
        "the 79th tick must still be inside the 80-tick window"
    );
    sim.tick();
    assert!(
        !sim.get(id).expect("alive").armadillo_is_scared(),
        "the 80th tick with no further hit must let the timer reach zero"
    );
}

/// A floor with a real `minecraft:water` layer at `y=0` — distinct from
/// `flat_world`'s dry ground (`y=-1` stone, `y=0` air) so the axolotl
/// play-dead gate below can prove its own `in_water()` precondition
/// rather than assuming the fixture provides it.
fn water_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=48 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
            world.set_block(x, 0, z, "minecraft:water");
        }
    }
    world
}

/// Axolotl play-dead (`AXOLOTL_PLAY_DEAD_TICKS` = `200`), gated through the production path
/// (`MobSim::attack` → `SimMob::apply_damage`). The trigger is
/// probabilistic (`axolotl_play_dead_roll`'s own two `nextInt(3)`-shaped
/// draws), so — the "predict the value" standard, applied to a
/// probabilistic trigger instead of a fixed one — this searches a small
/// range of raw-damage values for one this mob's own roll stream
/// actually fires on, using the exact function `apply_damage` calls,
/// rather than looping the real attack call hoping for a hit.
#[test]
fn a_hurt_axolotl_in_water_plays_dead_on_a_winning_roll() {
    let world = water_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:axolotl").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(100.0);

    assert!(
        !sim.get(id).expect("alive").axolotl_is_playing_dead(),
        "precondition: a fresh axolotl is not playing dead"
    );
    assert!(
        sim.get(id).expect("alive").in_water(),
        "precondition: the fixture must actually place the axolotl in water"
    );

    let health_bits = 100.0_f32.to_bits();
    let raw_damage = (1..30)
        .map(|d| d as f32)
        .find(|&d| {
            let (a, b) = axolotl_play_dead_roll(id as u64, health_bits, d.to_bits());
            a == 0 && (b as f32) < d
        })
        .expect("a 1-in-3 draw over 29 tries must fire at least once");

    sim.attack(id, Vec3::new(1.0, 0.0, 0.0), raw_damage, DamageFlags::default(), 0.0)
        .expect("the axolotl is a live target");

    assert!(
        sim.get(id).expect("alive").axolotl_is_playing_dead(),
        "a hit whose own roll stream fires must set the play-dead window"
    );
}

/// **Control**: the identical fixture and winning-roll damage against a
/// dry axolotl (`flat_world`, no water) must never play dead — proving
/// `in_water()` is a real gate here, not dead code the roll bypasses.
#[test]
fn a_dry_axolotl_never_plays_dead_even_on_a_winning_roll() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:axolotl").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(100.0);
    assert!(!sim.get(id).expect("alive").in_water(), "precondition: dry ground");

    let health_bits = 100.0_f32.to_bits();
    let raw_damage = (1..30)
        .map(|d| d as f32)
        .find(|&d| {
            let (a, b) = axolotl_play_dead_roll(id as u64, health_bits, d.to_bits());
            a == 0 && (b as f32) < d
        })
        .expect("a 1-in-3 draw over 29 tries must fire at least once");

    sim.attack(id, Vec3::new(1.0, 0.0, 0.0), raw_damage, DamageFlags::default(), 0.0)
        .expect("the axolotl is a live target");

    assert!(
        !sim.get(id).expect("alive").axolotl_is_playing_dead(),
        "a dry axolotl must never enter the play-dead window regardless of the roll"
    );
}

/// Random-sitting camel behaviour, gated through the real production
/// tick path (`MobSim::tick`, the loop `camel_random_sitting` is called
/// from) rather than by calling the function on a bare `SimMob`. A camel
/// left alone long enough must eventually sit down — proving both the
/// state flip and that it reaches the wire as the real sitting-pose
/// ordinal (`10`) `MetadataField::Pose` already carries for the warden,
/// not merely a private bookkeeping bool nothing reads.
///
/// The wait is intentionally generous (`CAMEL_RANDOM_SITTING_MIN_TICKS`
/// eligibility plus a healthy multiple of `camel_sit_roll`'s ~1-in-2400
/// expected wait): this is a deterministic hash of `(id, tick_count)`,
/// not real-clock timing, so the same seed always produces the same
/// outcome on every run — there is nothing here for a busy machine to
/// perturb, unlike the timing hazards this crate's own docs warn about
/// elsewhere.
#[test]
fn a_camel_left_alone_eventually_sits_and_reports_the_real_pose() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:camel").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    assert!(
        !sim.get(id).expect("alive").camel_is_sitting(),
        "precondition: a freshly spawned camel is standing"
    );
    assert!(
        !sim.get(id)
            .expect("alive")
            .snapshot()
            .metadata
            .contains(&MetadataField::Pose(CAMEL_POSE_SITTING)),
        "precondition: a standing camel must not report the sitting pose"
    );

    let mut sat = false;
    for _ in 0..20_000 {
        sim.tick();
        if sim.get(id).expect("alive").camel_is_sitting() {
            sat = true;
            break;
        }
    }
    assert!(sat, "a camel left alone for 20,000 ticks never sat down");
    assert!(
        sim.get(id)
            .expect("alive")
            .snapshot()
            .metadata
            .contains(&MetadataField::Pose(CAMEL_POSE_SITTING)),
        "a sitting camel must report the real sitting-pose ordinal to the client"
    );
}

/// **Control**: the identical wait against a cow, a species
/// `MobSim::snapshot`'s camel branch never touches — proves the pose
/// report is camel-specific rather than every mob eventually gaining a
/// `Pose` field this test's positive twin could not, by itself,
/// distinguish from a species-blind bug in the metadata builder.
#[test]
fn only_a_camel_ever_reports_a_sitting_pose_a_cow_never_does() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    for _ in 0..20_000 {
        sim.tick();
        assert!(
            !sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::Pose(CAMEL_POSE_SITTING)),
            "a cow must never report the sitting pose, however long it stands around"
        );
    }
}

/// The camel dash path exercises the rider-jump handler and
/// rider-jump executor end to end through the real production path —
/// `MobSim::interact` (mounting) then `MobSim::trigger_camel_dash` (the
/// `ServerBound::PlayerInput` jump-bit consumer's own call), not a
/// hand-built double. Checks the whole real chain a rider drives: mount
/// succeeds, dash starts, the wire metadata reports it, and it resets
/// once `CAMEL_DASH_MINIMUM_DURATION_TICKS` have passed.
#[test]
fn camel_dash_triggers_through_a_real_mount_and_reaches_the_metadata_wire() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:camel").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    let rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 42 };

    let mounted = sim.interact(id, rider, None);
    assert_eq!(mounted, InteractOutcome::Mounted, "an empty-handed click on an adult camel must mount it");
    assert!(
        !sim.get(id)
            .expect("alive")
            .snapshot()
            .metadata
            .contains(&MetadataField::Dash(true)),
        "precondition: a freshly mounted camel is not yet dashing"
    );

    assert!(
        sim.trigger_camel_dash(rider.entity_id),
        "a jump press aboard a fresh camel (cooldown already at zero) must start a dash"
    );
    assert!(
        sim.get(id)
            .expect("alive")
            .snapshot()
            .metadata
            .contains(&MetadataField::Dash(true)),
        "a dashing camel must report the dash metadata flag as true to the wire"
    );

    for _ in 0..CAMEL_DASH_MINIMUM_DURATION_TICKS {
        sim.tick();
    }
    assert!(
        sim.get(id)
            .expect("alive")
            .snapshot()
            .metadata
            .contains(&MetadataField::Dash(false)),
        "the dash flag must reset to false once the minimum duration elapses — the client \
         must see the reset, not merely stop seeing `true`"
    );
}

/// Vanilla's own rider-jump handler's cooldown-at-or-below-zero gate: a second jump press
/// while still cooling down must not restart the dash window, and the
/// full 55-tick cooldown (`CAMEL_DASH_COOLDOWN_TICKS`) must actually
/// elapse — not merely the 5-tick minimum duration — before a third
/// press succeeds. A magnitude check on the real constant, not a
/// direction-only assertion.
#[test]
fn camel_dash_cannot_retrigger_until_the_full_cooldown_elapses() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:camel").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    let rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 42 };
    assert_eq!(sim.interact(id, rider, None), InteractOutcome::Mounted);

    assert!(sim.trigger_camel_dash(rider.entity_id), "first press must dash");
    assert!(
        !sim.trigger_camel_dash(rider.entity_id),
        "an immediate second press must not restart the cooldown"
    );

    for _ in 0..(CAMEL_DASH_COOLDOWN_TICKS - 1) {
        sim.tick();
        assert!(
            !sim.trigger_camel_dash(rider.entity_id),
            "a press before the full cooldown has elapsed must keep failing"
        );
    }
    sim.tick();
    assert!(
        sim.trigger_camel_dash(rider.entity_id),
        "a press once the full {CAMEL_DASH_COOLDOWN_TICKS}-tick cooldown has elapsed must succeed"
    );
}

/// **Controls**: a baby camel refuses to mount at all
/// (vanilla's own camel interaction override's own "is not a baby" gate), and a species that
/// bypasses `MobSim::interact` entirely — mounted directly through the
/// low-level, species-blind `mount_mob` — still never dashes, proving
/// `trigger_camel_dash`'s own species check is real and not merely
/// inherited from the mount path.
#[test]
fn a_baby_camel_refuses_to_mount_and_a_mounted_cow_never_dashes() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);

    let baby_key = ResourceKey::from_str("minecraft:camel").expect("valid key");
    let baby_id = sim.spawn_species(baby_key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(baby_id).expect("alive").set_age(BABY_START_AGE);
    let rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 42 };
    assert_eq!(
        sim.interact(baby_id, rider, None),
        InteractOutcome::Pass,
        "a baby camel must refuse an empty-handed mount attempt"
    );

    let cow_key = ResourceKey::from_str("minecraft:cow").expect("valid key");
    let cow_id = sim.spawn_species(cow_key, Vec3::new(5.0, 0.0, 0.0)).id();
    let cow_rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 43 };
    assert!(
        sim.mount_mob(cow_id, cow_rider.entity_id),
        "the low-level mount primitive is species-blind by its own doc"
    );
    assert!(
        !sim.trigger_camel_dash(cow_rider.entity_id),
        "a mounted cow must never dash — `trigger_camel_dash` must check the species itself"
    );
}

/// Ominous-bottle producer: a pillager patrol leader killed while
/// **not** a member of any active raid must drop `minecraft:ominous_bottle`.
/// Three controls on the same death path, each isolating one clause of
/// the predicate the real one needs both halves of:
///
/// * a pillager that is a captain but **is** in an active raid must not
///   drop one (`hasRaid` flips the gate),
/// * a pillager that is **not** a captain must not drop one even
///   uninvolved in any raid (`isCaptain` flips the gate),
/// * a vindicator patrol leader must not drop one — only
///   `entities/pillager.json` carries this loot pool in vanilla, even
///   though vindicators can lead patrols too.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_pillager_patrol_captain_without_a_raid_drops_an_ominous_bottle() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:pillager").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_patrol_leader(true);
    sim.get_mut(id).expect("alive").set_health(1.0);

    let outcome = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
        .expect("the pillager is a live target");
    assert!(outcome.killed);

    let dropped = sim.dropped_items();
    assert!(
        dropped.iter().any(|(item, count)| item == "minecraft:ominous_bottle" && *count == 1),
        "a patrol captain with no active raid must drop exactly one ominous bottle: {dropped:?}"
    );
}

/// **Control**: a pillager captain that belongs to an active raid must
/// not drop the bottle — `hasRaid` half of `CAPTAIN_WITHOUT_RAID`.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_pillager_captain_inside_an_active_raid_drops_no_ominous_bottle() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let raid_id = sim.start_raid(Vec3::new(0.0, 0.0, 0.0), Difficulty::Easy, 1).expect("Easy is not Peaceful");
    let key = ResourceKey::from_str("minecraft:pillager").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_patrol_leader(true);
    sim.get_mut(id).expect("alive").set_health(1.0);
    // Puts `id` on the raid's own raider list — the exact state
    // `raid_containing_raider` reads to decide `hasRaid`.
    sim.raids.get_mut(&raid_id).expect("just started").raiders.push(id);

    let outcome = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
        .expect("the pillager is a live target");
    assert!(outcome.killed);

    let dropped = sim.dropped_items();
    assert!(
        !dropped.iter().any(|(item, _)| item == "minecraft:ominous_bottle"),
        "a captain that belongs to an active raid must not drop the bottle: {dropped:?}"
    );
}

/// **Control**: a non-captain pillager, involved in no raid, drops no
/// bottle — `isCaptain` half of `CAPTAIN_WITHOUT_RAID`.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_non_captain_pillager_drops_no_ominous_bottle() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:pillager").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_health(1.0);
    assert!(!sim.get(id).expect("alive").is_patrol_leader(), "control precondition: not a leader");

    let outcome = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
        .expect("the pillager is a live target");
    assert!(outcome.killed);

    let dropped = sim.dropped_items();
    assert!(
        !dropped.iter().any(|(item, _)| item == "minecraft:ominous_bottle"),
        "a rank-and-file pillager must not drop the bottle: {dropped:?}"
    );
}

/// **Control**: a vindicator patrol leader, involved in no raid, drops
/// no bottle — only `entities/pillager.json` carries this loot pool in
/// vanilla, even though a vindicator can lead a patrol too
/// (`PatrollingMonster` is not species-specific).
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_vindicator_patrol_leader_drops_no_ominous_bottle() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:vindicator").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(id).expect("alive").set_patrol_leader(true);
    sim.get_mut(id).expect("alive").set_health(1.0);

    let outcome = sim
        .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
        .expect("the vindicator is a live target");
    assert!(outcome.killed);

    let dropped = sim.dropped_items();
    assert!(
        !dropped.iter().any(|(item, _)| item == "minecraft:ominous_bottle"),
        "only a pillager carries this loot pool in vanilla: {dropped:?}"
    );
}

const TICKS: usize = 80;

/// **Control for the attribute fallback.**
///
/// The miss case for `follow_range` is **32.0**, not `0.0`. `attr`'s
/// `unwrap_or(0.0)` reads like the fallback and is unreachable for any
/// attribute the registry knows, because `AttributeMap::value` already
/// substitutes `default_def(key).default` for an absent instance.
///
/// The distinction determines what a useful guard can test. A guard of the shape
/// `if r > 0.0 { r } else { DEFAULT }` is **dead code** — it never fires, and
/// an unlisted species keeps the registry's 32.0, which is precisely the one
/// number `follow_range` never legitimately holds (the generic
/// mob attribute builder
/// overrides it to 16.0 for every mob). The wrong value sits *inside* the
/// plausible range, so only instance presence can detect the miss.
///
/// Predicted from the `follow_range` default definition and
/// `AttributeMap::value`'s `else` branch, then measured. If this ever reads
/// 0.0, `attr` changed and the `attr_present` split is redundant.
#[test]
fn control_the_attribute_lookup_misses_to_the_registry_default_not_zero() {
    // **Structurally** unlistable, not merely unlisted.
    //
    // This id is outside the `minecraft` namespace, so
    // `default_attributes` must return `None` before it consults
    // `type_spec`. The miss case is structural rather than dependent on
    // which species tables are populated.
    let unlisted = Identifier::from_str("modded:not_a_vanilla_mob").expect("valid id");
    assert!(
        default_attributes(&unlisted).is_none(),
        "default_attributes must answer None outside the minecraft namespace, \
         or the miss case below is not reachable at all"
    );

    let empty = AttributeMap::new();
    assert_eq!(
        attr(&empty, "follow_range"),
        32.0,
        "the miss case is the registry default, so a `> 0.0` guard can never fire"
    );
    assert_eq!(
        attr_present(&empty, "follow_range"),
        None,
        "instance presence is the only reading that can see the miss"
    );

    // A listed case carries its own value, so the split above does not
    // discard every attribute.
    let zombie = default_attributes(&Identifier::from_str("minecraft:zombie").unwrap())
        .expect("zombie has a type_spec arm");
    assert_eq!(
        attr_present(&zombie, "follow_range"),
        Some(35.0),
        "vanilla's own zombie attribute builder sets FOLLOW_RANGE to 35.0"
    );
}

/// **The gate.** A zombie must acquire at its real 35.0, which requires
/// separating 35 from *both* wrong candidates rather than merely showing that
/// targeting works at all.
///
/// | distance | expected | what it rules out |
/// |---|---|---|
/// | 20 | acquires | `DEFAULT_FOLLOW_RANGE` 16.0 (the pre-fix value) |
/// | 34 | acquires | the registry's 32.0 as well |
/// | 36 | **no** | an unbounded feed, and blaze/enderman's 48/64 |
///
/// Asserting only "a zombie acquires a nearby player" passes at 16, at 32 and
/// at 35 alike, which is the magnitude-species vacuous test: right subject,
/// predicate too weak to distinguish the hypotheses.
#[test]
fn a_zombie_acquires_at_its_real_follow_range_not_16_or_32() {
    assert!(
        acquires_at("zombie", 20.0, TICKS),
        "a zombie must acquire a player at 20 blocks; failing here means the \
         controller is still on DEFAULT_FOLLOW_RANGE (16.0) and #455's host \
         half never landed"
    );
    assert!(
        acquires_at("zombie", 34.0, TICKS),
        "a zombie must acquire at 34 blocks — inside its real 35.0 but outside \
         both 16.0 and the registry's 32.0, so this is the assertion that pins \
         the value rather than merely the wiring"
    );
    assert!(
        !acquires_at("zombie", 36.0, TICKS),
        "a zombie must NOT acquire at 36 blocks: the cut is real and bounded at \
         35.0, not merely large. Without this the gate above passes for any \
         range >= 34, including an unbounded feed"
    );
}

/// The unlisted-species fallback is observable at spawn time.
///
/// A fixed per-mob seed keeps the acquisition boundary stable: the
/// 15-block hit succeeds and the 17-block hit fails.
///
/// The fallback case uses an id outside the roster. Such an id has no
/// target goal, so this assertion measures the range installed at spawn
/// rather than target acquisition.
///
/// [`MobSim::spawn_species`] reads
/// `attr_present(…).unwrap_or(DEFAULT_FOLLOW_RANGE)` for any key. The
/// test checks the generic fallback at the spawn seam directly.
#[test]
fn an_unlisted_species_still_falls_back_at_the_spawn_path() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    // Outside the `minecraft` namespace, so `default_attributes` answers
    // `None` structurally — see the control above.
    let key = ResourceKey::from_str("modded:not_a_vanilla_mob").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    let got = MobController::follow_range(&sim.get(id).expect("alive").mob);
    assert_eq!(
        got, DEFAULT_FOLLOW_RANGE,
        "an unlisted species must fall back to vanilla's own generic mob \
         attribute builder's 16.0"
    );
    assert_ne!(
        got, 32.0,
        "32.0 is the registry default and the one value follow_range never \
         legitimately holds — reading it here means `attr`'s registry \
         fallback is reaching the controller, the exact defect #455's \
         brokered patch would have left in place"
    );

    // Control: a *listed* species must read its own jar value through the
    // same accessor, so the assertions above are a property of the fallback
    // and not of `follow_range` always answering 16.
    let zombie = ResourceKey::from_str("minecraft:zombie").expect("valid key");
    let zid = sim.spawn_species(zombie, Vec3::new(2.0, 0.0, 0.0)).id();
    assert_eq!(
        MobController::follow_range(&sim.get(zid).expect("alive").mob),
        35.0,
        "vanilla's own zombie attribute builder — if this also reads 16.0 \
         the accessor is not observing what spawn_species installed"
    );
}
