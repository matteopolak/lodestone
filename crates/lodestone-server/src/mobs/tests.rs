use super::*;

mod item_owner_tests {
    use super::*;

    fn fixture() -> MobSim<'static> {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -4..=20 {
            world.set_solid(x, 0, 0, true);
        }
        let world = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);
        let item = ResourceKey::from_str("minecraft:stone").expect("valid key");
        for position in [
            Vec3::new(-0.5, 2.0, 0.5),
            Vec3::new(16.5, 2.0, 0.5),
            Vec3::new(-0.25, 3.0, 0.5),
        ] {
            sim.spawn_item(
                item.clone(),
                position,
                Vec3::new(0.0, 0.0, 0.0),
                ItemLifecycle::newly_dropped(1, 64),
            );
        }
        sim
    }

    #[test]
    fn spawning_an_item_records_its_initial_chunk_owner() {
        let sim = fixture();
        let mut owners = sim
            .item_state
            .iter()
            .map(|(&id, state)| (id, state.owner))
            .collect::<Vec<_>>();
        owners.sort_unstable_by_key(|&(id, _)| id);

        assert_eq!(
            owners,
            vec![
                (1, ItemTickOwner::Chunk { cx: -1, cz: 0 }),
                (2, ItemTickOwner::Chunk { cx: 1, cz: 0 }),
                (3, ItemTickOwner::Chunk { cx: -1, cz: 0 }),
            ],
            "spawned items must start with the Euclidean chunk owner used by the first tick",
        );
    }

    fn item_state(sim: &MobSim<'_>) -> Vec<(i32, ItemLifecycle, ItemMotion, ItemTickOwner)> {
        sim.items
            .iter()
            .map(|tracked| {
                (
                    tracked.id,
                    tracked.lifecycle,
                    sim.item_state.get(&tracked.id).expect("matching motion").motion,
                    sim.item_state.get(&tracked.id).expect("matching owner").owner,
                )
            })
            .collect()
    }

    fn dense_item_owner_fixture(count: usize) -> MobSim<'static> {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -2..=66 {
            for z in -2..=4 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        let world = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);
        let item = ResourceKey::from_str("minecraft:stone").expect("valid key");
        for serial in 0..count {
            let x = [-0.5, 16.5, 32.5, 48.5][serial % 4];
            sim.spawn_item(
                item.clone(),
                Vec3::new(x, 3.0, (serial / 4 % 4) as f64 + 0.5),
                Vec3::new(0.0, 0.0, 0.0),
                ItemLifecycle::newly_dropped(1, 64),
            );
        }
        sim
    }

    #[test]
    fn item_owner_batches_restore_registration_order_after_reversed_completion() {
        let mut serial = fixture();
        let serial_world = serial.world;
        let (serial_batches, serial_probes) = serial.tick_item_owner_batches(&|x, y, z| {
            serial_world.block_state(x, y, z).to_owned()
        });
        assert_eq!(
            serial_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            [
                ItemTickOwner::Chunk { cx: -1, cz: 0 },
                ItemTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "negative fractional positions use Euclidean chunk ownership"
        );
        serial.apply_item_tick_owner_batches(serial_batches);
        let expected = item_state(&serial);

        let mut completed = fixture();
        let completed_world = completed.world;
        let (mut batches, completed_probes) = completed.tick_item_owner_batches(&|x, y, z| {
            completed_world.block_state(x, y, z).to_owned()
        });
        batches.reverse();
        let raw_slots = batches
            .iter()
            .flat_map(|batch| batch.effects.iter())
            .map(|effect| effect.serial)
            .collect::<Vec<_>>();
        assert_ne!(raw_slots, vec![0, 1, 2], "control must actually reorder owner completion");
        completed.apply_item_tick_owner_batches(batches);

        assert_eq!(item_state(&completed), expected);
        assert_eq!(completed_probes, serial_probes);
    }

    #[test]
    fn moving_items_crossing_negative_and_positive_boundaries_use_one_barrier_and_keep_ids() {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -4..=35 {
            world.set_solid(x, 0, 0, true);
        }
        let world = Box::leak(Box::new(world));
        let mut sim = MobSim::new(world);
        let item = ResourceKey::from_str("minecraft:stone").expect("valid key");
        let negative_to_zero = sim.spawn_item(
            item.clone(),
            Vec3::new(-0.1, 2.0, 0.5),
            Vec3::new(1.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(1, 64),
        );
        let zero_to_one = sim.spawn_item(
            item,
            Vec3::new(15.9, 2.0, 0.5),
            Vec3::new(1.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(1, 64),
        );
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let (mut batches, _) = sim.tick_item_owner_batches(&state_at);
        assert_eq!(
            batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            [
                ItemTickOwner::Chunk { cx: -1, cz: 0 },
                ItemTickOwner::Chunk { cx: 0, cz: 0 },
            ],
            "the source owner must use Euclidean division on both sides of zero"
        );
        assert_eq!(
            batches
                .iter()
                .flat_map(|batch| batch.effects.iter())
                .map(|effect| (effect.id, effect.destination))
                .collect::<Vec<_>>(),
            [
                (negative_to_zero, ItemTickOwner::Chunk { cx: 0, cz: 0 }),
                (zero_to_one, ItemTickOwner::Chunk { cx: 1, cz: 0 }),
            ],
            "each crossing must name its destination before central apply"
        );
        batches.reverse();
        sim.apply_item_tick_owner_batches(batches);
        assert_eq!(sim.item_handoff.pending(), 0, "destination admission closes each route");
        let mut owners = sim
            .item_state
            .iter()
            .map(|(&id, state)| (id, state.owner))
            .collect::<Vec<_>>();
        owners.sort_unstable_by_key(|&(id, _)| id);
        assert_eq!(
            owners,
            vec![
                (negative_to_zero, ItemTickOwner::Chunk { cx: 0, cz: 0 }),
                (zero_to_one, ItemTickOwner::Chunk { cx: 1, cz: 0 }),
            ]
        );
        let snapshot_ids: Vec<_> = sim.snapshots().into_iter().map(|snapshot| snapshot.id).collect();
        assert_eq!(
            snapshot_ids,
            vec![negative_to_zero, zero_to_one],
            "cross-owner publication keeps entity ids and serial order deterministic"
        );

        let (next_batches, _) = sim.tick_item_owner_batches(&state_at);
        assert_eq!(
            next_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            [
                ItemTickOwner::Chunk { cx: 0, cz: 0 },
                ItemTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "the destination, not the old source, owns the next tick"
        );
        sim.apply_item_tick_owner_batches(next_batches);
        assert_eq!(sim.item_count(), 2);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn serial_and_four_lane_boundary_crossing_have_identical_owner_and_id_results() {
        fn crossing_fixture() -> MobSim<'static> {
            let mut world = ChunkWorld::new(-64, 384);
            for x in -4..=67 {
                world.set_solid(x, 0, 0, true);
            }
            let world = Box::leak(Box::new(world));
            let mut sim = MobSim::new(world);
            let item = ResourceKey::from_str("minecraft:stone").expect("valid key");
            for x in [-0.1, 15.9, 31.9, 47.9] {
                sim.spawn_item(
                    item.clone(),
                    Vec3::new(x, 2.0, 0.5),
                    Vec3::new(1.0, 0.0, 0.0),
                    ItemLifecycle::newly_dropped(1, 64),
                );
            }
            sim
        }

        let mut serial = crossing_fixture();
        serial.item_owner_plan = 1;
        let serial_world = serial.world;
        let serial_state_at = |x, y, z| serial_world.block_state(x, y, z).to_owned();
        let serial_batches = serial.tick_item_owner_batches_with_workers(&serial_state_at, 1).0;
        serial.apply_item_tick_owner_batches(serial_batches);

        let mut parallel = crossing_fixture();
        parallel.item_owner_plan = 1;
        let parallel_world = parallel.world;
        let parallel_state_at = |x, y, z| parallel_world.block_state(x, y, z).to_owned();
        let parallel_batches = parallel.tick_item_owner_batches_with_workers(&parallel_state_at, 4).0;
        parallel.apply_item_tick_owner_batches(parallel_batches);

        assert_eq!(item_state(&parallel), item_state(&serial));
        assert_eq!(
            parallel.snapshots().into_iter().map(|snapshot| snapshot.id).collect::<Vec<_>>(),
            [1, 2, 3, 4],
            "four owner completions retain the stable entity-id publication order"
        );
    }

    #[test]
    #[should_panic(expected = "every tick-start owner batch exactly once")]
    fn item_owner_batch_merge_rejects_a_missing_owner() {
        let mut sim = fixture();
        let world = sim.world;
        let (mut batches, _) =
            sim.tick_item_owner_batches(&|x, y, z| world.block_state(x, y, z).to_owned());
        batches.pop();
        let _ = merge_item_tick_owner_batches(batches);
    }

    #[test]
    #[should_panic(expected = "may not contain one owner twice")]
    fn item_owner_batch_merge_rejects_a_duplicate_owner() {
        let mut sim = fixture();
        let world = sim.world;
        let (mut batches, _) =
            sim.tick_item_owner_batches(&|x, y, z| world.block_state(x, y, z).to_owned());
        batches[1] = batches[0].clone();
        let _ = merge_item_tick_owner_batches(batches);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn parallel_item_owner_batches_match_one_lane_with_interleaved_negative_owners() {
        let mut serial = dense_item_owner_fixture(256);
        serial.item_owner_plan = 1;
        let serial_world = serial.world;
        let serial_state_at = |x, y, z| serial_world.block_state(x, y, z).to_owned();
        let serial_batches = serial.tick_item_owner_batches_with_workers(&serial_state_at, 1).0;
        serial.apply_item_tick_owner_batches(serial_batches);

        let mut parallel = dense_item_owner_fixture(256);
        parallel.item_owner_plan = 1;
        let parallel_world = parallel.world;
        let parallel_state_at = |x, y, z| parallel_world.block_state(x, y, z).to_owned();
        let parallel_batches = parallel.tick_item_owner_batches_with_workers(&parallel_state_at, 4).0;
        assert_eq!(
            parallel_batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            [
                ItemTickOwner::Chunk { cx: -1, cz: 0 },
                ItemTickOwner::Chunk { cx: 1, cz: 0 },
                ItemTickOwner::Chunk { cx: 2, cz: 0 },
                ItemTickOwner::Chunk { cx: 3, cz: 0 },
            ],
            "the parity scene must span an interleaved negative owner plus three positive owners"
        );
        parallel.apply_item_tick_owner_batches(parallel_batches);

        assert_eq!(
            item_state(&parallel),
            item_state(&serial),
            "four terrain-collision owners must preserve the one-lane item result"
        );
    }

    #[test]
    #[should_panic(expected = "latest tick-start plan")]
    fn item_owner_batches_reject_stale_plan_completions() {
        let mut sim = fixture();
        let world = sim.world;
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let (stale, _) = sim.tick_item_owner_batches(&state_at);
        let _current = sim.tick_item_owner_batches(&state_at);
        sim.apply_item_tick_owner_batches(stale);
    }

    #[test]
    #[should_panic(expected = "already applied tick-start plan")]
    fn item_owner_batches_reject_replayed_completions() {
        let mut sim = fixture();
        let world = sim.world;
        let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
        let (batches, _) = sim.tick_item_owner_batches(&state_at);
        sim.apply_item_tick_owner_batches(batches.clone());
        sim.apply_item_tick_owner_batches(batches);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "manual dense-scene throughput measurement"]
    fn measure_dense_item_owner_workers() {
        for item_count in [256, 2_048] {
            let sim = dense_item_owner_fixture(item_count);
            let world = sim.world;
            let state_at = |x, y, z| world.block_state(x, y, z).to_owned();
            let started = lodestone_time::Instant::now();
            let _ = sim.tick_item_owner_batches_with_workers(&state_at, 1);
            let serial = started.elapsed();
            let started = lodestone_time::Instant::now();
            let _ = sim.tick_item_owner_batches_with_workers(&state_at, 4);
            let parallel = started.elapsed();
            eprintln!(
                "dense_items owners=4 items={item_count} serial_ms={:.3} parallel_ms={:.3} speedup={:.3}",
                serial.as_secs_f64() * 1_000.0,
                parallel.as_secs_f64() * 1_000.0,
                serial.as_secs_f64() / parallel.as_secs_f64()
            );
        }
    }
}

/// The `follow_range` attribute reaches the controller that bounds target
/// acquisition, including the no-target case.
#[cfg(test)]
mod follow_range_tests {
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
}
/// Host-resolved persistent-anger deadline tests.
#[cfg(test)]
mod anger_tests {
    use super::*;

    /// The grudge window in ticks, stated **independently of [`ANGER_TICKS`]**.
    /// Twenty-to-thirty-nine seconds at 20 ticks per second yields the
    /// inclusive range `[400, 780]`.
    ///
    /// These literals are load-bearing: reading the seconds as ticks would
    /// produce `[20, 39]`, and deriving them from `ANGER_TICKS` would allow a
    /// bad duration to move both the implementation and its expectation.
    const JAR_LO: u64 = 400;
    const JAR_HI: u64 = 780;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// Spawns one mob through [`MobSim::spawn_species`], hits it once, and
    /// reports the tick offset at which `angry_target` first reads `None`.
    ///
    /// Drives `MobSim` through its normal perception path, keeping the
    /// AI-goal and spawn-category behavior under test.
    ///
    /// The attacker position is placed well outside `flat_world`'s solid `±8`
    /// platform. The attacker remains outside the walkable platform, so the bee
    /// cannot path to it or clear `anger` through an attack. This function
    /// measures **grudge duration**, not "does the
    /// mob's own combat ever run" — an attacker outside the walkable platform
    /// (so no path exists to it, for any of the four species' plausible
    /// speeds) decouples the two.
    fn ticks_until_anger_clears(species: &str, limit: u64) -> Option<u64> {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(128.0, 0.0, 0.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("the mob must still be alive to hold a grudge");

        // One tick to run the feed, then poll.
        for elapsed in 0..limit {
            sim.tick();
            if sim.get(id).expect("alive").mob.angry_target().is_none() {
                return Some(elapsed);
            }
        }
        None
    }

    /// **The gate.** A grudge must expire inside the measured `[400, 780]`
    /// tick window. Twenty-to-thirty-nine seconds at 20 ticks per second
    /// yields this range; treating the seconds as ticks would yield `[20, 39]`.
    ///
    /// Predicting only "it eventually expires" is satisfied by both hypotheses
    /// and by an off-by-one on the inclusive upper bound. Both bounds are
    /// asserted so the inclusive interval is checked directly.
    #[test]
    fn anger_expires_inside_the_jars_tick_window() {
        let (lo, hi) = (JAR_LO, JAR_HI);
        // Generous headroom over `hi`, so "never expired" is distinguishable
        // from "expired late" rather than both timing out.
        let limit = hi * 2;

        for species in ["wolf", "bee", "enderman", "zombified_piglin"] {
            let elapsed = ticks_until_anger_clears(species, limit).unwrap_or_else(|| {
                panic!("{species}'s grudge never expired within {limit} ticks")
            });
            assert!(
                elapsed >= lo,
                "{species}'s grudge expired after {elapsed} ticks, before the jar's \
                 minimum of {lo}. A value in [20, 39] means rangeOfSeconds(20, 39) \
                 was read as seconds; it already returns ticks"
            );
            assert!(
                elapsed <= hi,
                "{species}'s grudge lasted {elapsed} ticks, past the jar's maximum \
                 of {hi}"
            );
        }
    }

    /// The grudge must be **live** immediately after the hit, and must name the
    /// attacker's position — not merely be non-`None` at some later point.
    ///
    /// Control for the test above: without this, a mob whose anger was never
    /// set at all would "expire" at tick 0 and only the lower-bound assertion
    /// would catch it, for the wrong reason.
    #[test]
    fn a_hit_starts_a_grudge_naming_the_attacker() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            None,
            "an unprovoked neutral mob must hold no grudge — if this is Some, \
             every neutral species is hostile on sight"
        );

        let attacker = Vec3::new(3.0, 0.0, 4.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");
        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            Some(attacker),
            "the grudge must name where the attacker was"
        );
    }

    /// The deadline is **absolute**, so a grudge refreshed by a second hit
    /// extends from the *new* tick rather than from the first.
    ///
    /// This is the assertion a decrementing counter passes only by accident:
    /// it pins that the stored value is compared against `tick_count` rather
    /// than decremented, by advancing the clock a long way between two hits and
    /// requiring the grudge to outlive the first deadline's worst case.
    #[test]
    fn a_second_hit_extends_the_deadline_from_the_new_tick() {
        let (lo, hi) = (JAR_LO, JAR_HI);
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(1.0, 0.0, 0.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");
        // Advance well past the first grudge's *minimum* but not its maximum,
        // then hit again.
        for _ in 0..lo {
            sim.tick();
        }
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");

        // The refreshed grudge must still be live `lo` ticks later, which the
        // first grudge could not guarantee: its worst case was `hi`, and we are
        // now at `lo + lo = 800 > hi`.
        for _ in 0..lo {
            sim.tick();
        }
        assert!(
            lo + lo > hi,
            "this test's arithmetic assumes 2*{lo} exceeds {hi}; if the window \
             changed, the schedule below no longer proves anything"
        );
        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            Some(attacker),
            "the second hit must extend the deadline from the tick it landed on; \
             a grudge that has already expired here means the deadline was not \
             recomputed against the current clock"
        );
    }

    /// Vanilla's own zombified-piglin alert-interval's own `[80, 120]` window, stated
    /// independently of [`PIGLIN_ALERT_INTERVAL_TICKS`] for the same reason
    /// [`JAR_LO`]/[`JAR_HI`] are stated independently of [`ANGER_TICKS`]
    /// above: a magnitude check that read the expectation off the constant
    /// under test would pass even if the constant itself were wrong. Drawn
    /// from a real spawned mob's own RNG stream (never a hand-rolled double),
    /// the same [`MobController`] seam production reads.
    #[test]
    fn piglin_alert_interval_rolls_inside_the_jars_80_to_120_window() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:zombified_piglin").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        for _ in 0..1000 {
            let draw = piglin_alert_interval(&mut sim.get_mut(id).expect("alive").mob);
            assert!(
                (80..=120).contains(&draw),
                "piglin_alert_interval drew {draw}, outside the jar's [80, 120] \
                 ALERT_INTERVAL window"
            );
        }
    }

    /// The ongoing piglin group-alert mechanism is isolated from the one-shot
    /// owner-group propagation it accompanies.
    ///
    /// The one-shot arm fires when [`MobSim::attack`] creates a new grudge.
    /// The neighbour is spawned after that event, so it cannot receive the
    /// one-shot notification. An ensuing grudge therefore demonstrates the
    /// periodic alert timer rather than the immediate group notification.
    #[test]
    fn a_piglin_holding_a_target_alerts_a_neighbour_that_did_not_exist_for_the_one_shot_alert() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:zombified_piglin").expect("valid key");

        let alerting = sim.spawn_species(key.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
        let attacker = Vec3::new(3.0, 0.0, 4.0);
        sim.attack(alerting, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");

        // The neighbour did not exist for the hit above, so the immediate
        // group census could not have reached it.
        let neighbour = sim.spawn_species(key, Vec3::new(5.0, 0.0, 0.0)).id();
        assert_eq!(
            sim.get(neighbour).expect("alive").mob.angry_target(),
            None,
            "precondition: a freshly spawned neighbour must start with no grudge"
        );

        // 120 ticks covers the interval's own worst case
        // (`PIGLIN_ALERT_INTERVAL_TICKS`'s upper bound), plus headroom for the
        // alerting piglin's anger-gated target row to turn its grudge into a
        // real `attack_target` (what `piglin_alert_ticks` reads to decide it
        // has something to alert about).
        for _ in 0..200 {
            sim.tick();
            if sim
                .get(neighbour)
                .expect("alive")
                .mob
                .angry_target()
                .is_some()
            {
                return;
            }
        }
        panic!(
            "the neighbour was never alerted within 200 ticks — the ongoing \
             maybeAlertOthers mechanism has no live producer"
        );
    }
}

/// MobSim seam primitive tests (instant relocation / self-damage / target
/// identity): the host half of the seam primitives in `lodestone-entity`. The
/// gaze feed is not supplied by this seam — see
/// [`PlayerPerception`]'s lack of a view vector.
#[cfg(test)]
mod primitives_tests {
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
}

/// Block-identity cues read from generated tag data, and the graze handoff out
/// of an immutably borrowed world.
#[cfg(test)]
mod block_cues_tests {
    use super::*;
    use lodestone_entity::pathfinding::PathWorld;

    /// The jar's real `#minecraft:edible_for_sheep` membership
    /// (`data/minecraft/tags/block/edible_for_sheep.json`), transcribed here
    /// **only as the expectation**. The implementation does not contain this
    /// list — it resolves the tag through `lodestone_data::tool`, which is
    /// generated from the jar — so this is an independent statement of the answer
    /// rather than a restatement of the code under test.
    const JAR_EDIBLE: &[&str] = &[
        "minecraft:short_grass",
        "minecraft:short_dry_grass",
        "minecraft:tall_dry_grass",
        "minecraft:fern",
    ];

    /// A single cell of `block` with air around it, at a fixed position.
    fn world_of(block: &str) -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(0, 0, 0, block);
        world
    }

    /// **The gate that a hand-written tag list fails.**
    ///
    /// Every member of the jar's tag must classify as edible. Three of the four
    /// would have been missed by the obvious `short_grass | tall_grass` guess:
    /// `short_dry_grass`, `tall_dry_grass` and `fern`. A sheep would have refused
    /// to graze a fern, and no test in the tree would have said so.
    #[test]
    fn every_jar_tag_member_classifies_as_edible_for_sheep() {
        for block in JAR_EDIBLE {
            let world = world_of(block);
            assert!(
                world.block_cues(0, 0, 0).edible_for_sheep,
                "{block} is in #minecraft:edible_for_sheep and must classify as edible — \
                 a hand-written list missing it is exactly how this stays silently wrong"
            );
        }
    }

    /// **The other half of the same mistake: the guess's false positive.**
    ///
    /// `tall_grass` is *not* in `#minecraft:edible_for_sheep` — the jar tag has
    /// four entries and that is not one of them. It is the block most likely to be
    /// added by anyone writing the list from memory, and asserting only the
    /// positives above would let it through.
    #[test]
    fn tall_grass_is_not_edible_for_sheep_despite_looking_like_it_should_be() {
        let world = world_of("minecraft:tall_grass");
        assert!(
            !world.block_cues(0, 0, 0).edible_for_sheep,
            "minecraft:tall_grass is absent from the jar's edible_for_sheep tag; \
             classifying it as edible means the tag is being guessed, not read"
        );
    }

    /// `grass_block` is the *equality* cue, not a tag member — vanilla's own
    /// "eat block" goal tests it
    /// with block equality. So it must set
    /// `grass_block` and must **not** set `edible_for_sheep`: a sheep standing on
    /// grass eats the block below, a sheep standing in short grass eats the block
    /// at its feet, and conflating the two would make either mechanism fire in the
    /// wrong place.
    #[test]
    fn grass_block_is_the_equality_cue_and_not_a_tag_member() {
        let cues = world_of("minecraft:grass_block").block_cues(0, 0, 0);
        assert!(cues.grass_block, "grass_block must set its own cue");
        assert!(
            !cues.edible_for_sheep,
            "grass_block is not in the edible tag — the two cues are independent"
        );
    }

    /// The negative control. Ordinary blocks and air must set neither cue,
    /// otherwise the positives above are satisfied by a classifier that says yes
    /// to everything.
    #[test]
    fn control_ordinary_blocks_set_no_cue_at_all() {
        for block in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_log"] {
            let cues = world_of(block).block_cues(0, 0, 0);
            assert!(
                !cues.edible_for_sheep && !cues.grass_block,
                "{block} must set no cue; a classifier that says yes to everything \
                 passes every positive assertion above"
            );
        }
    }

    /// Property strings must not defeat the lookup: `block_state` yields a full
    /// state string, so a cue keyed on the raw string would miss any block with
    /// properties. `tall_dry_grass` is a real tag member *and* carries a
    /// `half`/`facing`-style property list in some states, which is why this is a
    /// distinct case rather than a restatement of the first test.
    #[test]
    fn a_state_with_properties_still_classifies() {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(0, 0, 0, "minecraft:short_grass");
        assert!(world.block_cues(0, 0, 0).edible_for_sheep);
        // The `grass_block` arm goes through the same property strip.
        world.set_block(0, 1, 0, "minecraft:grass_block[snowy=false]");
        assert!(
            world.block_cues(0, 1, 0).grass_block,
            "a state with a property list must still match the equality cue — \
             `block_state` returns the full string, properties included"
        );
    }

    /// **The handoff gate.** A grazing mob's eat must survive `MobSim::tick` and
    /// emerge from [`MobSim::take_grazes`].
    ///
    /// The test supplies the goal directly, so the assertion covers only the
    /// `take_new_eaten` → `pending_grazes` → `take_grazes` handoff. The
    /// production roster intentionally has no sheep-eating goal.
    ///
    /// It is deliberately **not** an assertion about the eat interval. That is
    /// `lodestone-entity`'s `block_perception.rs` gate, which distinguishes 444
    /// predicted eats from 286 — and which also recorded that a rate measured in a
    /// mutating world measures grass scarcity instead. Nothing drains the world
    /// here, so supply is infinite and the tick budget only has to make "at least
    /// one eat" overwhelmingly likely: at the halved 1-in-500 adult interval,
    /// 20,000 ticks puts the probability of zero at about e^-40.
    #[test]
    fn a_grazing_mob_hands_its_eat_to_the_driver() {
        let mut world = ChunkWorld::new(-64, 384);
        // Grass to stand on, short grass to stand in — so both cues are live and
        // whichever arm fires, the handoff is exercised.
        //
        // Wide enough that idle wandering cannot walk the sheep off it in
        // 20,000 ticks. That is not padding: at 5×5 the sheep reached the edge and
        // grazed at (-2, 0, -2), and outside the patch there is no floor at all,
        // so a narrower world tests falling rather than grazing.
        for x in -24..=24 {
            for z in -24..=24 {
                world.set_block(x, -1, z, "minecraft:grass_block");
                world.set_block(x, 0, z, "minecraft:short_grass");
            }
        }

        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                ResourceKey::from_str("minecraft:sheep").expect("valid key"),
                Vec3::new(0.5, 0.0, 0.5),
            )
            .id();
        sim.get_mut(id).expect("just spawned").add_goal(
            5,
            Box::new(lodestone_entity::ai::goals::EatBlockGoal::new()),
        );

        assert!(
            sim.take_grazes().is_empty(),
            "precondition: nothing is pending before any tick, so the assertion \
             below cannot be satisfied by a stale entry"
        );

        let mut grazes = Vec::new();
        for _ in 0..20_000 {
            sim.tick();
            grazes.extend(sim.take_grazes());
            if !grazes.is_empty() {
                break;
            }
        }

        assert!(
            !grazes.is_empty(),
            "a sheep standing in short grass on a grass block must record an eat \
             that reaches take_grazes; empty means the handoff is broken and #238 \
             can never mutate the world"
        );
        // The recorded position must be the *mob's* cell, not the eaten block's —
        // the consumer resolves `AtFeet` as that cell and `Below` as one down, so
        // reporting the eaten cell would make the `Below` arm write dirt a block
        // too low.
        //
        // **`y` is the whole assertion.** `x`/`z` are identical for both
        // candidates, so they carry no information about which one this is; only
        // the height distinguishes the mob's feet (`0`) from the grass block it
        // stands on (`-1`). An earlier draft of this pinned the full triple to
        // `(0, 0, 0)` and failed at `(-2, 0, -2)` — idle wandering had walked
        // the sheep two blocks before it grazed, so that assertion was testing a
        // false premise (that the mob holds still) rather than the handoff.
        let (pos, _what) = grazes[0];
        assert_eq!(
            pos.y, 0,
            "the handoff must carry the mob's own feet cell (y=0), not the grass \
             block below it (y=-1) — the EatenBlock variants are relative to the mob"
        );
        assert!(
            (-24..=24).contains(&pos.x) && (-24..=24).contains(&pos.z),
            "the graze must be recorded somewhere on the prepared patch, got \
             ({}, {}) — off-patch means the sheep grazed a cell with no grass",
            pos.x,
            pos.z
        );

        // Draining really drains: a second read must not re-report the same eat,
        // or a slow consumer would apply it twice.
        assert!(
            sim.take_grazes().is_empty(),
            "take_grazes must drain, not merely read"
        );
    }
}

/// Age-scaled hitbox and baby-only movement modifier, including the
/// `species_shape`/`SimMob::set_age` path that applies `is_baby`.
#[cfg(test)]
mod baby_shape_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    /// **A baby zombie is the real `0.49×0.98` literal, not a halved adult.**
    ///
    /// `0.6×1.95` halved is `0.3×0.975` — close enough to the true value that
    /// an assertion only checking "shrank" would pass under either
    /// hypothesis. Predicting the exact literal is what separates a real
    /// `BABY_DIMENSIONS` port from the generic `getAgeScale()` fallback.
    #[test]
    fn a_baby_zombie_is_the_exact_vanilla_literal_not_a_halved_adult() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .id();
        {
            let adult = sim.get(id).expect("spawned");
            assert_eq!(adult.shape().width, 0.6, "adult zombie width");
            assert_eq!(adult.shape().height, 1.95, "adult zombie height");
        }

        let mob = sim.get_mut(id).expect("spawned");
        mob.set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let baby = sim.get(id).expect("still spawned");
        assert_eq!(
            baby.shape().width,
            0.49,
            "baby zombie width is the literal BABY_DIMENSIONS, not 0.6 * 0.5 = 0.3"
        );
        assert_eq!(
            baby.shape().height,
            0.98,
            "baby zombie height is the literal BABY_DIMENSIONS, not 1.95 * 0.5 = 0.975"
        );
    }

    /// Growing back up re-derives the adult shape — the boundary crossing
    /// runs in both directions, not just baby-ward.
    #[test]
    fn growing_up_restores_the_adult_shape() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        assert_eq!(sim.get(id).expect("spawned").shape().width, 0.45, "baby cow width");

        sim.get_mut(id).expect("spawned").set_age(0);
        let grown = sim.get(id).expect("still spawned");
        assert!(!grown.is_baby(), "age 0 is the cooldown-free adult reading");
        assert_eq!(grown.shape().width, 0.9, "adult cow width restored");
        assert_eq!(grown.shape().height, 1.4, "adult cow height restored");
    }

    /// **Control: a species with no `baby_dimensions` entry uses the real
    /// `LivingEntity` fallback (half size), not a made-up constant.**
    ///
    /// Skeletons never naturally have babies, but `is_baby()` only reads the
    /// age counter — nothing species-gates it — so this is the discriminating
    /// input for the fallback arm specifically: a skeleton's adult box is
    /// `0.6×1.99`, and the *wrong* hypothesis (no fallback at all, i.e. the
    /// shape not changing) would leave it at `0.6×1.99` where the fallback
    /// predicts `0.3×0.995`.
    #[test]
    fn control_a_species_with_no_baby_table_entry_uses_the_generic_age_scale() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:skeleton".parse().expect("valid key"), above_floor())
            .id();
        let adult_width = sim.get(id).expect("spawned").shape().width;
        let adult_height = sim.get(id).expect("spawned").shape().height;
        assert_eq!(adult_width, 0.6, "adult skeleton width");
        assert_eq!(adult_height, 1.99, "adult skeleton height");

        sim.get_mut(id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let baby = sim.get(id).expect("still spawned");
        assert_eq!(
            baby.shape().width,
            adult_width * 0.5,
            "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
        );
        assert_eq!(
            baby.shape().height,
            adult_height * 0.5,
            "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
        );
    }

    /// **The zombie family's baby speed boost is `base * 1.5`, and a cow's
    /// stays flat** — the discriminating pair the residue's "attribute
    /// change" half asks for. `step_per_tick` now reports the AI-driven
    /// kinematic-follower rate, not the bare attribute (see
    /// `ai_ground_speed`'s own doc): predicted here from the same outside
    /// constants (vanilla's default ground friction, `0.6 * 0.91`) in a
    /// separate expression, not by calling the function under test, so a
    /// shared bug cannot cancel out. `0.23 * 1.5 = 0.345` is still the
    /// attribute-level prediction; squaring and dividing by
    /// `1 - 0.6 * 0.91` is the extra step `ai_ground_speed` adds.
    #[test]
    fn baby_zombie_speeds_up_and_baby_cow_does_not() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let friction = 1.0 - 0.6 * 0.91;
        let predicted = |attribute: f64| attribute * attribute / friction;

        let zombie_id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .id();
        let zombie_adult_speed = sim.get(zombie_id).expect("spawned").step_per_tick();
        assert!(
            (zombie_adult_speed - predicted(0.23)).abs() < 1e-9,
            "adult zombie ground speed must be movement_speed(0.23) squared over \
             (1 - 0.6*0.91), got {zombie_adult_speed}, predicted {}",
            predicted(0.23)
        );
        sim.get_mut(zombie_id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let zombie_baby_speed = sim.get(zombie_id).expect("still spawned").step_per_tick();
        assert!(
            (zombie_baby_speed - predicted(0.23 * 1.5)).abs() < 1e-9,
            "baby zombie speed must be exactly ai_ground_speed(0.23 * 1.5), got \
             {zombie_baby_speed}, predicted {}",
            predicted(0.23 * 1.5)
        );
        assert!(
            zombie_baby_speed > zombie_adult_speed,
            "the baby boost must still win after the ground-speed conversion, not \
             just at the attribute level"
        );

        let cow_id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .id();
        let cow_adult_speed = sim.get(cow_id).expect("spawned").step_per_tick();
        assert!(
            (cow_adult_speed - predicted(0.2)).abs() < 1e-9,
            "adult cow ground speed must be movement_speed(0.2) squared over \
             (1 - 0.6*0.91), got {cow_adult_speed}, predicted {}",
            predicted(0.2)
        );
        sim.get_mut(cow_id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let cow_baby_speed = sim.get(cow_id).expect("still spawned").step_per_tick();
        assert!(
            (cow_baby_speed - cow_adult_speed).abs() < 1e-9,
            "a cow has no SPEED_MODIFIER_BABY — baby speed must equal adult speed exactly"
        );
    }

    /// **Control proving `ai_ground_speed` is load-bearing, not decorative**:
    /// with the bare `movement_speed` attribute used directly (the pre-fix
    /// behaviour this repo's own evidence standards require a control for),
    /// a pig's per-tick movement step is `0.25` — noticeably higher than the
    /// `ai_ground_speed(0.25)` this fix now produces, which is the measured
    /// direction of the "way too fast" report. If this control ever starts
    /// failing, `ai_ground_speed` has stopped changing the value it exists to
    /// change.
    #[test]
    fn removing_the_ground_speed_conversion_reproduces_the_too_fast_bug() {
        let attribute = 0.25;
        assert!(
            ai_ground_speed(attribute) < attribute,
            "control: the converted ground speed must be lower than the bare \
             attribute value, or the subject assertions above prove nothing \
             about the conversion firing"
        );
    }

    /// A bred child inherits the correct baby shape through
    /// `resolve_breeding`'s existing `child.set_age(BABY_START_AGE)` call —
    /// no separate wiring needed, because [`SimMob::set_age`] itself now
    /// re-derives the shape. This is the island check: a shape fold that only
    /// ran for a hand-called `set_age` in a test, and never for the
    /// production breeding path, would still look finished from the unit
    /// tests above alone.
    #[test]
    fn a_bred_child_spawns_with_the_baby_shape_already_applied() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(7.0, 1.0, 8.0))
            .id();
        let _b = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(9.0, 1.0, 8.0))
            .id();

        // Exercises `resolve_breeding`'s own partner search and
        // `child.set_age(BABY_START_AGE)` call directly — the real
        // production path a breeding goal completing feeds through
        // `MobSim::tick`, without re-driving sixty ticks of love-mode timing
        // just to reach it.
        sim.resolve_breeding(vec![(
            a,
            Vec3::new(8.0, 1.0, 8.0),
            "minecraft:cow".parse().expect("valid key"),
        )]);

        let child = sim
            .mobs
            .iter()
            .find(|m| m.is_baby())
            .expect("a child was spawned and is a baby");
        assert_eq!(
            child.shape().width,
            0.45,
            "the bred calf's shape must already be the baby literal, not the adult default"
        );
    }
}

/// `MetadataField::Baby`'s producer-side species switch in
/// [`SimMob::snapshot`] — the eligible species must match exactly the union
/// [`baby_dimensions`]/[`baby_speed_multiplier`] already scope "grows a
/// baby" to, and the ineligible species (index 16's other claimants) must
/// never see the field at all.
#[cfg(test)]
mod baby_metadata_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    fn baby_field(metadata: &[MetadataField]) -> Option<bool> {
        metadata.iter().find_map(|f| match f {
            MetadataField::Baby(b) => Some(*b),
            _ => None,
        })
    }

    /// **Positive arm**: every species this sim scopes ageing to
    /// (vanilla's own ageable-mob breedable-animal set plus the zombie family — see
    /// `SimMob::snapshot`'s own comment for the mechanical derivation off
    /// `.cache/mc/26.2/src/`) must push `MetadataField::Baby(false)` as a
    /// freshly-spawned adult.
    ///
    /// **Negative control**: index 16's other real claimants — `creeper`
    /// (vanilla's own swell-direction metadata field, an `INT`, already the producer for a
    /// different variant at this same index), `ghast`
    /// (vanilla's own "is charging" metadata field) and `phantom` (vanilla's
    /// own size metadata field) — must
    /// push no `Baby` field at all. These are exactly the entities a shared
    /// "is baby" encoder would corrupt: a ghast told `Baby(false)` reads to
    /// a real client as "not charging", and a phantom's size becomes `0`.
    ///
    /// Collected rather than asserted per-iteration so one run reports every
    /// wrong species, not just the first.
    #[test]
    fn eligible_species_emit_baby_and_only_those_do() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let mut wrong: Vec<String> = Vec::new();

        for species in [
            "cow",
            "mooshroom",
            "sheep",
            "pig",
            "chicken",
            "rabbit",
            "wolf",
            "zombie",
            "husk",
            "zombie_villager",
            "drowned",
            "zombified_piglin",
        ] {
            let key = format!("minecraft:{species}").parse().expect("valid key");
            let id = sim.spawn_species(key, above_floor()).id();
            let metadata = sim.get(id).expect("spawned").snapshot().metadata;
            if baby_field(&metadata) != Some(false) {
                wrong.push(format!(
                    "{species}: expected Baby(false), metadata was {metadata:?}"
                ));
            }
        }

        for species in ["creeper", "ghast", "phantom"] {
            let key = format!("minecraft:{species}").parse().expect("valid key");
            let id = sim.spawn_species(key, above_floor()).id();
            let metadata = sim.get(id).expect("spawned").snapshot().metadata;
            if baby_field(&metadata).is_some() {
                wrong.push(format!(
                    "{species}: must emit no Baby field at all, metadata was {metadata:?}"
                ));
            }
        }

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The grown-up transition.** A baby that matures must produce a
    /// snapshot whose `Baby` is `Some(false)`, not absent — an absent field
    /// leaves the client holding whatever `Baby(true)` it was sent on
    /// arrival, so the mob would stay a baby on screen forever. See
    /// `SimMob::snapshot`'s own doc comment for why this variant is pushed
    /// unconditionally rather than only while `is_baby()` is true.
    #[test]
    fn a_grown_up_baby_reports_baby_false_not_absent() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        let baby_metadata = sim.get(id).expect("spawned").snapshot().metadata;
        assert_eq!(
            baby_field(&baby_metadata),
            Some(true),
            "a freshly spawned baby zombie must report Baby(true), got {baby_metadata:?}"
        );

        sim.get_mut(id).expect("spawned").set_age(0);
        let adult_metadata = sim.get(id).expect("still spawned").snapshot().metadata;
        assert!(
            adult_metadata.iter().any(|f| matches!(f, MetadataField::Baby(_))),
            "the grown-up snapshot must still carry a Baby field, not omit it: {adult_metadata:?}"
        );
        assert_eq!(
            baby_field(&adult_metadata),
            Some(false),
            "after growing up the field must flip to Baby(false), got {adult_metadata:?}"
        );
    }
}

/// Lead attach/detach, the fence-knot re-parent, and the
/// distance-based pull/snap physics.
#[cfg(test)]
mod leash_tests {
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
}

/// Entity-spawn slice: the trader plus its leashed llama
/// escort. The spawn-cycle timing/POI search is out of scope here — see
/// `spawn_wandering_trader`'s own doc comment.
#[cfg(test)]
mod wandering_trader_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=16 {
            for z in -8..=16 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    #[test]
    fn spawning_a_wandering_trader_leashes_two_llamas_to_it() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);

        let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(10.0, 5.0, 10.0));

        assert_eq!(llamas.len(), 2, "vanilla attempts exactly two escorts");
        let trader = sim.get(trader_id).expect("trader spawned");
        assert_eq!(trader.entity_type().path(), "wandering_trader");

        for llama_id in llamas {
            let llama = sim.get(llama_id).expect("llama spawned");
            assert_eq!(llama.entity_type().path(), "trader_llama");
            assert_eq!(
                llama.leash_holder(),
                Some(LeashHolder::Mob(trader_id)),
                "each llama must be leashed to the trader, not merely placed near it"
            );
        }
    }

    /// **The leash is real, not cosmetic** — moving the trader and ticking
    /// leashes must pull an escort that has drifted past the elastic
    /// distance, exactly as it would for a player-held leash. This is the
    /// control that separates "the llama has a `leash_holder` field set" from
    /// "the llama is actually tethered".
    #[test]
    fn the_escort_leash_actually_pulls_when_the_trader_moves_away() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(0.0, 0.0, 0.0));
        let llama_id = llamas[0];

        // Drag the trader far enough that the escort (2 blocks from spawn,
        // at x=2) is past LEASH_ELASTIC_DIST (6) from it but still short of
        // LEASH_TOO_FAR_DIST (12), so this exercises the *pull* branch —
        // distance 8, not the snap branch a farther drag would hit instead.
        // There is no teleport API, so drive it through a knockback impulse
        // large enough to land at the target position deterministically.
        let trader_pos = sim.get(trader_id).expect("spawned").position();
        let target = Vec3::new(10.0, 0.0, 0.0);
        sim.get_mut(trader_id).expect("spawned").apply_knockback(Vec3::new(
            target.x - trader_pos.x,
            target.y - trader_pos.y,
            target.z - trader_pos.z,
        ));
        assert_eq!(sim.get(trader_id).expect("spawned").position(), target);

        let llama_before = sim.get(llama_id).expect("spawned").position();
        sim.tick_leashes();
        let llama_after = sim.get(llama_id).expect("still spawned").position();
        assert_ne!(
            llama_before, llama_after,
            "the llama must move toward its holder once the trader is far enough away"
        );
    }
}

/// `MobSim`'s periodic idle-vocalisation producer (`roll_ambient_sound`),
/// wired into [`MobSim::tick`], emits ambient sounds during ordinary
/// exploration in addition to hurt and death sounds.
#[cfg(test)]
mod ambient_sound_tests {
    use super::*;
    use crate::effects::WorldEffect;
    use lodestone_model::SoundCategory;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// **The wire is real, not merely the derivation.** A hermetic call to
    /// `effects::mob_ambient_sound` proves the string derivation; it proves
    /// nothing about whether anything in `MobSim::tick` ever calls it — the
    /// exact island shape this repo's own evidence standards warn about
    /// repeatedly. Ticking a real, freshly spawned cow through the real
    /// production `tick()` loop and draining `take_ambient_sounds()` is what
    /// proves the roll actually fires and actually reaches the queue
    /// `crate::tick::run_tick_loop` drains.
    ///
    /// `ambient_sound_time` starts at `0` and this cow's RNG stream is
    /// seeded deterministically from its id, so how many ticks the first
    /// firing takes is a fixed, measured number for this exact scenario, not
    /// a guess: measured at **tick 36** for this world/spawn order. The
    /// 400-tick loop bound is over 10x that measured value, not a round
    /// number reached for on its own — it exists only so a regression that
    /// pushed the firing tick out shows up as a clean failure rather than an
    /// infinite loop.
    #[test]
    fn a_real_mob_ticked_through_the_production_loop_eventually_vocalises() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
            if !fired.is_empty() {
                break;
            }
        }

        assert_eq!(
            fired.len(),
            1,
            "exactly one ambient sound must have fired within the margin, got {fired:?}"
        );
        match &fired[0] {
            WorldEffect::Sound { sound, category, .. } => {
                assert_eq!(sound, "minecraft:entity.cow.ambient");
                assert_eq!(*category, SoundCategory::Neutral);
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }

    /// Owner batching must not turn a globally serial entity pass into
    /// owner-major publication. These effects deliberately interleave origin,
    /// negative, then origin chunks; the negative source is the control for
    /// truncating `-0.5 / 16` to the origin instead of using floor and Euclidean
    /// division.
    #[test]
    fn ambient_effect_batches_keep_negative_owners_and_restore_serial_order() {
        let effect = |data| crate::effects::WorldEffect::LevelEvent {
            event: crate::effects::SOUND_ZOMBIE_CONVERTED,
            pos: BlockPos::new(data, 64, 0),
            data,
            global: false,
        };
        let pending = vec![
            PendingEntityTickEffect {
                owner: entity_tick_owner(Vec3::new(0.5, 64.0, 0.5)),
                source: Vec3::new(0.5, 64.0, 0.5),
                effect: effect(0),
            },
            PendingEntityTickEffect {
                owner: entity_tick_owner(Vec3::new(-0.5, 64.0, -0.5)),
                source: Vec3::new(-0.5, 64.0, -0.5),
                effect: effect(1),
            },
            PendingEntityTickEffect {
                owner: entity_tick_owner(Vec3::new(16.5, 64.0, 0.5)),
                source: Vec3::new(16.5, 64.0, 0.5),
                effect: effect(2),
            },
            PendingEntityTickEffect {
                owner: entity_tick_owner(Vec3::new(0.25, 64.0, 0.25)),
                source: Vec3::new(0.25, 64.0, 0.25),
                effect: effect(3),
            },
        ];

        let batches = batch_entity_tick_effects(pending);
        assert_eq!(
            batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
            vec![
                EntityTickOwner::Chunk { cx: 0, cz: 0 },
                EntityTickOwner::Chunk { cx: -1, cz: -1 },
                EntityTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "owners are deterministic by first serial appearance, including negative chunks"
        );
        assert_eq!(
            batches[0]
                .effects()
                .iter()
                .map(|effect| effect.sequence)
                .collect::<Vec<_>>(),
            vec![0, 3],
            "one owner receives its effects in the serial order it produced them"
        );

        let mut serial: Vec<_> = batches
            .into_iter()
            .flat_map(|batch| batch.effects)
            .collect();
        serial.sort_unstable_by_key(|effect| effect.sequence);
        assert_eq!(
            serial.iter().map(|effect| effect.sequence).collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
            "the central hand-off restores the old cross-owner visit order"
        );
    }

    /// **Control: the roll is load-bearing, not vacuous.** A dead mob
    /// (`health <= 0.0`) must never roll — vanilla's own guard is
    /// `isAlive() && …` — so ticking a mob whose health is forced to zero
    /// for the same window above must produce nothing at all. Without this,
    /// the subject test's "it fired" is not distinguishable from "everything
    /// always fires".
    #[test]
    fn a_dead_mob_never_rolls_an_ambient_sound() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id).expect("spawned").health = 0.0;

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
        }
        assert!(
            fired.is_empty(),
            "a mob at zero health must never roll an ambient sound, got {fired:?}"
        );
    }

    /// A hostile species reports the `Hostile` sound category, matching
    /// `mob_vocalisation`'s own hurt/death split — checked with the same
    /// production-loop wiring as the subject test above, not just the
    /// hermetic derivation.
    #[test]
    fn a_hostile_mob_vocalises_on_the_hostile_category() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
            if !fired.is_empty() {
                break;
            }
        }
        assert_eq!(fired.len(), 1, "expected exactly one ambient sound, got {fired:?}");
        match &fired[0] {
            WorldEffect::Sound { sound, category, .. } => {
                assert_eq!(sound, "minecraft:entity.zombie.ambient");
                assert_eq!(*category, SoundCategory::Hostile);
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }
}

/// Gossip, reputation and zombie-villager curing,
/// driven through real production entry points
/// (`MobSim::interact`/`MobSim::tick`/`MobSim::attack_from_player`) rather
/// than calling `villager::gossip`/`villager::reputation`/`villager::conversion`
/// directly — those modules' own test suites already cover the pure
/// arithmetic; what these gates prove is that the wiring actually reaches a
/// live [`SimMob`], the same "reaches pixels, not just a closed loop"
/// standard `ambient_sound_tests` above applies.
#[cfg(test)]
mod villager_gossip_reputation_and_curing_tests {
    use super::*;

    /// A real floor near the origin — see `leash_tests::flat_world`'s own
    /// doc comment for why a bare void `ChunkWorld` stopped being safe once
    /// idle mobs fall. This module's "distant villager" control spawns at
    /// `(500, 0, 500)`, deliberately left ungrounded: nothing in this module
    /// asserts that villager's own position, only that gossip/curing never
    /// reaches it, and x/z are untouched by falling regardless.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=20 {
            for z in -8..=20 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn alice() -> PlayerIdentity {
        PlayerIdentity {
            uuid: Uuid::from_u128(0xA11CE),
            entity_id: 4242,
        }
    }

    /// A golden apple on a zombie villager with no Weakness must do nothing
    /// at all — no conversion state, `Pass`, matching vanilla's own
    /// plain-success-no-reduction arm (disclosed as `Pass`,
    /// see `InteractOutcome::ZombieVillagerConversionStarted`'s own doc).
    #[test]
    fn a_golden_apple_on_an_unweakened_zombie_villager_does_nothing() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:zombie_villager".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();

        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:golden_apple".parse().expect("valid key")),
        );
        assert_eq!(outcome, InteractOutcome::Pass);
        assert!(
            sim.get(id).expect("still alive").conversion.is_none(),
            "no conversion state must be started without Weakness"
        );
    }

    /// **The wire is real, not merely the derivation.** A golden apple used
    /// on a weakened zombie villager must: report
    /// `ZombieVillagerConversionStarted` (which consumes the item), start a
    /// real [`villager::conversion::ConversionState`] with the actor's uuid
    /// recorded, remove Weakness, add Strength, and publish the cure sound
    /// through the same [`MobSim::take_vocalisations`] queue
    /// `crate::tick::run_tick_loop` drains in production — not a hermetic
    /// call to `effects::zombie_villager_cure_sound` in isolation.
    #[test]
    fn a_golden_apple_on_a_weakened_zombie_villager_starts_a_real_conversion() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:zombie_villager".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .apply_effect("minecraft:weakness", 1000, 0);

        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:golden_apple".parse().expect("valid key")),
        );
        assert_eq!(outcome, InteractOutcome::ZombieVillagerConversionStarted);
        assert!(
            outcome.consumes_item(),
            "the golden apple must be consumed, matching itemStack.consume(1, player)"
        );

        let mob = sim.get(id).expect("still alive");
        let state = mob.conversion.expect("a conversion must have started");
        assert_eq!(state.starter, Some(alice().uuid));
        assert!(
            (villager::conversion::CONVERSION_WAIT_MIN..=villager::conversion::CONVERSION_WAIT_MAX)
                .contains(&state.remaining_ticks),
            "remaining_ticks must land in the real vanilla 3600-6000 range, got {}",
            state.remaining_ticks
        );
        assert!(
            mob.effects().amplifier_of("minecraft:weakness").is_none(),
            "Weakness must be removed"
        );
        assert!(
            mob.effects().amplifier_of("minecraft:strength").is_some(),
            "Strength must be applied"
        );

        let vocalisations = sim.take_vocalisations();
        assert_eq!(
            vocalisations.len(),
            1,
            "exactly one cure sound must have been queued, got {vocalisations:?}"
        );
        match &vocalisations[0] {
            crate::effects::WorldEffect::Sound { sound, .. } => {
                assert_eq!(sound, "minecraft:entity.zombie_villager.cure");
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }

    /// **The whole timer, driven through the production `tick()` loop** rather
    /// than a direct call to `villager::conversion::conversion_progress`.
    /// The countdown uses a handful of ticks (private-field access, same crate)
    /// so this test does not need 3600+ iterations; the *mechanism* ticked is
    /// the same one production drives. A completed conversion must flip
    /// `entity_type` to `minecraft:villager`, seed gossip with the curer's
    /// `ZombieVillagerCured` entries, apply nausea (the "confusion" state), and publish
    /// a conversion-sound level event
    /// through the same queue production drains.
    #[test]
    fn a_completed_conversion_becomes_a_real_villager_with_seeded_gossip() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:zombie_villager".parse().expect("valid key"),
                Vec3::new(10.0, 0.0, 10.0),
            )
            .id();
        let curer = alice().uuid;
        sim.get_mut(id).expect("spawned").conversion = Some(villager::conversion::ConversionState {
            starter: Some(curer),
            remaining_ticks: 3,
        });

        let mut level_events = Vec::new();
        for _ in 0..10 {
            sim.tick();
            level_events.extend(sim.take_ambient_sounds());
            if sim
                .get(id)
                .is_some_and(|m| m.entity_type().path() == "villager")
            {
                break;
            }
        }

        let mob = sim.get(id).expect("still alive");
        assert_eq!(mob.entity_type().path(), "villager", "must have become a real villager");
        assert!(mob.conversion.is_none(), "conversion state must be cleared");
        assert_eq!(
            mob.gossip.reputation(curer),
            125,
            "ZombieVillagerCured's own predicted value (20*5 + 25*1), seeded onto the \
             new villager's own ledger"
        );
        assert!(
            mob.effects().amplifier_of("minecraft:nausea").is_some(),
            "the post-cure confusion state (Nausea) must be applied"
        );

        assert!(
            level_events.iter().any(|effect| matches!(
                effect,
                crate::effects::WorldEffect::LevelEvent { event, .. }
                    if *event == crate::effects::SOUND_ZOMBIE_CONVERTED
            )),
            "the SOUND_ZOMBIE_CONVERTED level event must reach the production queue, got {level_events:?}"
        );
    }

    /// `MobSim::record_reputation_event`/`villager_reputation` reach a real
    /// spawned villager's own ledger, not a hermetic `GossipContainer`.
    #[test]
    fn record_reputation_event_reaches_a_real_villagers_own_ledger() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let player = alice().uuid;

        assert_eq!(sim.villager_reputation(id, player), 0);
        sim.record_reputation_event(id, villager::reputation::ReputationEventType::Trade, player);
        assert_eq!(sim.villager_reputation(id, player), 2, "trading grants 2 * weight(1) = 2");
    }

    /// `MobSim::attack_from_player`: hurting a real villager
    /// writes negative gossip onto **that villager's own** ledger about the
    /// attacker — driven through the real hit pipeline
    /// (`apply_damage`/`note_hurt`), not a direct `apply_reputation_event`
    /// call.
    #[test]
    fn hurting_a_real_villager_lowers_its_reputation_of_the_attacker() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let attacker = alice();

        assert_eq!(sim.villager_reputation(id, attacker.uuid), 0);
        let outcome = sim.attack_from_player(
            id,
            Some(attacker),
            Vec3::new(1.0, 0.0, 0.0),
            1.0,
            DamageFlags::default(),
            0.0,
        );
        assert!(outcome.is_some(), "the villager must have been hit");
        assert_eq!(
            sim.villager_reputation(id, attacker.uuid),
            -25,
            "VillagerHurt's predicted value: 25 * minor_negative.weight()(-1)"
        );
    }

    /// **Control: a `None` attacker must write no gossip at all** — the
    /// disclosed "unidentified actor" skip, proven by actually driving it
    /// rather than merely asserting the branch exists.
    #[test]
    fn an_unidentified_attacker_writes_no_reputation_gossip() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        sim.attack_from_player(id, None, Vec3::new(1.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.0);
        assert!(
            sim.get(id).expect("still alive").gossip.is_empty(),
            "no attacker identity means no gossip write at all"
        );
    }

    /// Two villagers close enough to gossip exchange ledger entries through the
    /// real `tick()` loop's `spread_villager_gossip` pass, exercising the
    /// integrated producer and consumer path.
    #[test]
    fn two_nearby_villagers_spread_gossip_through_the_real_tick_loop() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let b = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();
        let stranger = Uuid::from_u128(0xDEAD_BEEF);
        sim.get_mut(a)
            .expect("spawned")
            .gossip
            // `Trading`, not `MajorPositive`: `MajorPositive`'s own
            // `decay_per_transfer` (20) equals its own `max` (20), so it
            // can *never* survive a transfer (always decays to exactly 0,
            // below `DISCARD_THRESHOLD`) — a real vanilla quirk `gossip.rs`'s
            // own `a_transferred_entry_that_decays_below_threshold_is_dropped`
            // test already predicts. `Trading` at its own max (25) decays to
            // 5, which does survive.
            .add(stranger, villager::gossip::GossipType::Trading, 25);

        for _ in 0..(MobSim::GOSSIP_SPREAD_INTERVAL_TICKS + 1) {
            sim.tick();
        }

        assert!(
            sim.get(b)
                .expect("still alive")
                .gossip
                .entries_for(stranger)
                .is_some(),
            "villager b must have picked up some gossip about the stranger from villager a"
        );
    }

    /// Control: two villagers far apart never spread, even across many
    /// gossip-spread passes — otherwise the subject test above could pass
    /// under an implementation with no distance gate at all.
    #[test]
    fn distant_villagers_never_spread_gossip() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let b = sim
            .spawn_species(
                "minecraft:villager".parse().expect("valid key"),
                Vec3::new(500.0, 0.0, 500.0),
            )
            .id();
        let stranger = Uuid::from_u128(0xDEAD_BEEF);
        sim.get_mut(a)
            .expect("spawned")
            .gossip
            // `Trading`, not `MajorPositive`: `MajorPositive`'s own
            // `decay_per_transfer` (20) equals its own `max` (20), so it
            // can *never* survive a transfer (always decays to exactly 0,
            // below `DISCARD_THRESHOLD`) — a real vanilla quirk `gossip.rs`'s
            // own `a_transferred_entry_that_decays_below_threshold_is_dropped`
            // test already predicts. `Trading` at its own max (25) decays to
            // 5, which does survive.
            .add(stranger, villager::gossip::GossipType::Trading, 25);

        for _ in 0..(MobSim::GOSSIP_SPREAD_INTERVAL_TICKS * 3) {
            sim.tick();
        }

        assert!(
            sim.get(b)
                .expect("still alive")
                .gossip
                .entries_for(stranger)
                .is_none(),
            "villagers 500 blocks apart must never spread gossip to each other"
        );
    }
}

#[cfg(test)]
mod allay_carrying_tests {
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

    fn alice() -> PlayerIdentity {
        PlayerIdentity {
            uuid: Uuid::from_u128(0xA11CE),
            entity_id: 4242,
        }
    }

    /// The empty-handed allay "carrying" interaction path: an allay given an
    /// item must take it
    /// into its main hand, consume the item, and report `ItemGiven`.
    #[test]
    fn giving_an_empty_handed_allay_an_item_makes_it_hold_that_item() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert!(
            sim.get(id).expect("spawned").mob.main_hand_item().is_none(),
            "a freshly spawned allay must start empty-handed"
        );

        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:emerald".parse().expect("valid key")),
        );

        assert_eq!(outcome, InteractOutcome::ItemGiven);
        assert!(
            outcome.consumes_item(),
            "the given item must be consumed, matching itemStack.consume(1, player)"
        );
        assert_eq!(
            sim.get(id).expect("still alive").mob.main_hand_item(),
            Some("emerald"),
            "the allay must now be carrying exactly the item it was given"
        );
    }

    /// The negative control: an allay **already** carrying an item must
    /// refuse a second one — vanilla's own allay interaction override's gate is
    /// specifically "empty main hand", not "any interaction with an item".
    /// Without this, the positive gate above could be passing because every
    /// interaction overwrites the held item unconditionally rather than
    /// because the empty-hand gate is real.
    #[test]
    fn an_already_carrying_allay_refuses_a_second_item() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        let first = sim.interact(
            id,
            alice(),
            Some(&"minecraft:emerald".parse().expect("valid key")),
        );
        assert_eq!(first, InteractOutcome::ItemGiven);

        let second = sim.interact(
            id,
            alice(),
            Some(&"minecraft:diamond".parse().expect("valid key")),
        );
        assert_eq!(
            second,
            InteractOutcome::Pass,
            "an allay already carrying an item must refuse a second one"
        );
        assert_eq!(
            sim.get(id).expect("still alive").mob.main_hand_item(),
            Some("emerald"),
            "the original item must still be held after the refused second gift"
        );
    }

    /// A second negative control: an empty-handed interaction (no item held
    /// by the actor) must never clear or otherwise touch an allay's hands.
    #[test]
    fn an_empty_hand_interaction_does_nothing_to_an_allay() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();

        let outcome = sim.interact(id, alice(), None);
        assert_eq!(outcome, InteractOutcome::Pass);
        assert!(sim.get(id).expect("still alive").mob.main_hand_item().is_none());
    }

    /// An allay-specific pickup check: a carrying allay
    /// with a matching item dropped right next to it absorbs the whole
    /// stack into [`SimMob::allay_inventory_count`] and the ground item is
    /// gone, driven through the real production path (`MobSim::tick` →
    /// `allay_pick_up_items`).
    #[test]
    fn an_allay_picks_up_a_matching_dropped_item_nearby() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        let stick_id = sim.spawn_item(
            "minecraft:stick".parse().expect("valid key"),
            Vec3::new(0.5, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(3, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
        );

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            3,
            "the whole 3-stack must be absorbed"
        );
        assert!(
            sim.item_lifecycle(stick_id).is_none(),
            "the fully-absorbed ground stack must be removed, not left at count 0"
        );
    }

    /// **Control**: an emerald dropped next to a stick-carrying allay must
    /// never be picked up — `allayConsidersItemEqual`'s own item-identity
    /// gate, without which the positive test above could be passing because
    /// every nearby item is absorbed regardless of type.
    #[test]
    fn an_allay_ignores_a_dropped_item_of_a_different_type() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        sim.spawn_item(
            "minecraft:emerald".parse().expect("valid key"),
            Vec3::new(0.5, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
        );

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            0,
            "a mismatched item type must never be picked up"
        );
        assert_eq!(sim.item_count(), 1, "the mismatched item must still be on the ground");
    }

    /// `GoAndGiveItemsToTarget`: a carrying allay standing at its own liked
    /// note block's `.above()` cell throws exactly one item there per tick
    /// — a real dropped [`crate::item_entity::ItemEntity`] a player could
    /// walk over, not a state flag. Drives `MobSim::tick` →
    /// `allay_deliver_items` directly against host state set the way
    /// `resolve_vibrations`' own `hearNoteblock` arm would have left it,
    /// isolating the *delivery* half from the *hearing* half already proven
    /// end-to-end in `crate::tick`'s own note-block gates.
    #[test]
    fn a_carrying_allay_at_its_liked_noteblock_delivers_one_item_per_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 1.0, 0.0),
            )
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        {
            let mob = sim.get_mut(id).expect("alive");
            mob.allay_inventory_count = 2;
            mob.allay_liked_noteblock = Some((Vec3::new(0.0, 0.0, 0.0), 100));
        }

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            1,
            "exactly one item must be thrown this tick"
        );
        assert_eq!(sim.item_count(), 1, "the thrown item must be a real ground entity");
    }

    /// **Control**: the identical fixture but far from any liked note block
    /// (`allay_liked_noteblock` left `None`) must never deliver — proving
    /// the arrival check above is a real gate, not unconditional draining.
    #[test]
    fn a_carrying_allay_with_no_liked_noteblock_never_delivers() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 1.0, 0.0),
            )
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        sim.get_mut(id).expect("alive").allay_inventory_count = 2;

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            2,
            "with nothing liked, nothing must be thrown"
        );
        assert_eq!(sim.item_count(), 0);
    }

    /// **The allay duplication arm, through the production path** — driven by
    /// `allay_liked_noteblock` as the dance signal. An amethyst shard on such an allay
    /// must spawn a second, real allay and consume the shard.
    #[test]
    fn an_amethyst_shard_duplicates_an_allay_that_recently_heard_a_noteblock() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id).expect("alive").allay_liked_noteblock = Some((Vec3::new(3.0, 0.0, 0.0), 100));

        let before = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:amethyst_shard".parse().expect("valid key")),
        );

        assert_eq!(outcome, InteractOutcome::AllayDuplicated);
        assert!(outcome.consumes_item(), "the shard must be consumed");
        let after = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        assert_eq!(after, before + 1, "duplication must spawn exactly one real new allay");
        assert!(
            sim.get(id).expect("alive").allay_duplication_cooldown > 0,
            "the original allay must be put on cooldown too"
        );
        assert!(
            sim.take_vocalisations().iter().any(|effect| matches!(
                effect,
                crate::effects::WorldEffect::Particles { particle, .. } if particle == "minecraft:heart"
            )),
            "vanilla's own allay entity-event handler's status-18 heart burst \
             must reach the production queue too, not just the outcome's own \
             particle() classification"
        );
    }

    /// **Control**: the identical shard interaction against an allay that
    /// has never heard a note block must do nothing — proving the
    /// `isDancing()` substitute is a real gate, not one that always fires
    /// on an amethyst shard.
    #[test]
    fn an_amethyst_shard_does_nothing_to_an_allay_that_never_heard_a_noteblock() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        let before = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:amethyst_shard".parse().expect("valid key")),
        );

        assert_ne!(
            outcome,
            InteractOutcome::AllayDuplicated,
            "an allay that never heard a note block must never duplicate"
        );
        let after = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        assert_eq!(after, before, "no new allay must have spawned");
    }
}

/// Villager hurt or nearby-hostile conditions can summon an iron golem through
/// the integrated mob-simulation path.
#[cfg(test)]
mod golem_summon_tests {
    use super::*;

    /// A real floor near the origin — see `leash_tests::flat_world`'s own
    /// doc comment for why a bare void `ChunkWorld` stopped being safe once
    /// idle mobs fall. This module's `hurt_villager` calls at `x = ±50` are
    /// deliberately left ungrounded: no assertion here reads either
    /// villager's own position, only the resulting golem-summon count.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn hurt_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), pos)
            .id();
        sim.get_mut(id)
            .expect("just spawned")
            .mob
            .note_hurt(Some(Vec3::new(pos.x + 1.0, pos.y, pos.z)));
        id
    }

    fn iron_golem_count(sim: &MobSim<'_>) -> usize {
        sim.mobs
            .iter()
            .filter(|m| m.entity_type.path() == "iron_golem")
            .count()
    }

    /// The headline case: three hurt villagers within the 10-block agreement
    /// box must produce exactly one iron golem, and every villager in the box
    /// (not only the triggering one) must be marked `golem_detected_until` so
    /// a second pass this same 100-tick window does not summon a second one.
    #[test]
    fn three_hurt_villagers_close_together_summon_exactly_one_golem() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(2.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(-2.0, 0.0, 0.0));

        sim.tick();

        assert_eq!(
            iron_golem_count(&sim),
            1,
            "three hurt villagers within the agreement box must summon exactly one golem"
        );
        assert!(
            sim.mobs
                .iter()
                .filter(|m| m.entity_type.path() == "villager")
                .all(|m| m.golem_detected_until.is_some()),
            "every villager in the agreement box must be marked golem-detected after a spawn"
        );
    }

    /// Below `GOLEM_VILLAGERS_NEEDED` (3): two hurt villagers must summon
    /// nothing — the discriminating floor, not merely "some villagers hurt
    /// summons a golem eventually".
    #[test]
    fn two_hurt_villagers_do_not_summon_a_golem() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(2.0, 0.0, 0.0));

        for _ in 0..150 {
            sim.tick();
        }

        assert_eq!(iron_golem_count(&sim), 0, "two hurt villagers must never reach the agreement floor");
    }

    /// Three villagers far enough apart that they never share an agreement
    /// box (each pairwise distance exceeds `GOLEM_AGREEMENT_RADIUS`) must not
    /// summon a golem even though each is individually hurt.
    #[test]
    fn three_hurt_villagers_too_far_apart_do_not_summon_a_golem() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(50.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(-50.0, 0.0, 0.0));

        sim.tick();

        assert_eq!(iron_golem_count(&sim), 0, "villagers outside one shared agreement box must not summon a golem");
    }

    /// A villager that is neither hurt nor near a hostile is not a candidate
    /// at all — three *unhurt* villagers standing together must never summon
    /// a golem.
    #[test]
    fn unhurt_villagers_with_no_hostile_nearby_never_summon() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(-2.0, 0.0, 0.0));

        for _ in 0..150 {
            sim.tick();
        }

        assert_eq!(iron_golem_count(&sim), 0, "villagers with nothing wrong must never summon a golem");
    }

    /// A hostile mob nearby is exactly as good as being hurt — three
    /// *unhurt* villagers next to a zombie must still summon, proving the
    /// `hurt || hostile_near` disjunction rather than only the hurt half.
    #[test]
    fn a_nearby_hostile_alone_is_enough_to_summon() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(-2.0, 0.0, 0.0));
        sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(1.0, 0.0, 1.0));

        sim.tick();

        assert_eq!(iron_golem_count(&sim), 1, "a nearby hostile alone must be enough to summon, with no villager hurt");
    }
}

/// Cat gift and parrot shoulder-ride requests are drained and resolved by the
/// real [`MobSim::tick`] loop, covering the production connection between
/// request producers and host consumers.
#[cfg(test)]
mod cat_gift_and_shoulder_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn cat_key() -> ResourceKey {
        "minecraft:cat".parse().expect("valid key")
    }

    fn parrot_key() -> ResourceKey {
        "minecraft:parrot".parse().expect("valid key")
    }

    /// `cat_gift_chance`'s two-keyframe step function, pinned against both
    /// hypotheses a rounding-off reading could produce: inside the
    /// pre-dawn window (`[23667, 24000)` and `[0, 362)`, wrapping) the
    /// chance is vanilla's own `0.7F`; everywhere else it is a hard `0.0`,
    /// not merely "lower".
    #[test]
    fn cat_gift_chance_matches_the_timeline_step_function() {
        assert_eq!(MobSim::cat_gift_chance(0), 0.7, "the very start of a day is still inside the wrapped window");
        assert_eq!(MobSim::cat_gift_chance(361), 0.7, "one tick before the low keyframe");
        assert_eq!(MobSim::cat_gift_chance(362), 0.0, "the low keyframe itself");
        assert_eq!(MobSim::cat_gift_chance(12_000), 0.0, "the middle of the day");
        assert_eq!(MobSim::cat_gift_chance(23_666), 0.0, "one tick before the high keyframe");
        assert_eq!(MobSim::cat_gift_chance(23_667), 0.7, "the high keyframe itself");
        assert_eq!(MobSim::cat_gift_chance(23_999), 0.7, "the last tick before wrapping");
        assert_eq!(
            MobSim::cat_gift_chance(24_000 + 12_000),
            0.0,
            "a second day's middle must read the same as the first's"
        );
    }

    /// A cat's [`MobController::request_gift`] call, drained through one real
    /// `MobSim::tick`, must reach an actual item entity when the gift chance
    /// is favourable — proving the production drain (`gift_requests` →
    /// `resolve_cat_gifts`), not merely that the resolver function works when
    /// called directly. Forty cats rather than one: at chance `0.7` the
    /// probability every single one fails is `0.3^40`, indistinguishable
    /// from zero, so this is not a flaky roll of the dice — it is the
    /// deterministic-in-practice discriminator against a wiring break that
    /// would make it fail **every** time instead.
    #[test]
    fn a_cats_gift_request_reaches_a_real_item_through_one_production_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.day_time = 23_800; // inside the 0.7 window
        for i in 0..40 {
            let id = sim.spawn_species(cat_key(), Vec3::new(f64::from(i) * 8.0, 64.0, 0.0)).id();
            sim.get_mut(id).expect("just spawned").mob.request_gift();
        }
        assert_eq!(sim.item_count(), 0, "no item exists before the tick that drains the request");
        sim.tick();
        assert!(
            sim.item_count() > 0,
            "at a 0.7 gift chance across 40 requests, at least one real item entity must exist \
             after one production tick"
        );
    }

    /// The deterministic negative control for the same chain: outside the
    /// gift window the chance is a **hard** `0.0` (not merely lower), so no
    /// request — however many — can ever produce an item. This is what
    /// separates "the wiring reaches the resolver" from "the resolver always
    /// spawns something regardless of the roll".
    #[test]
    fn a_cats_gift_request_never_lands_outside_the_gift_window() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.day_time = 12_000; // dead centre of the 0.0 window
        for i in 0..40 {
            let id = sim.spawn_species(cat_key(), Vec3::new(f64::from(i) * 8.0, 64.0, 0.0)).id();
            sim.get_mut(id).expect("just spawned").mob.request_gift();
        }
        sim.tick();
        assert_eq!(sim.item_count(), 0, "a 0.0 gift chance must never spawn an item, however many cats request one");
    }

    /// A parrot's [`MobController::request_shoulder_ride`] call, drained
    /// through one real tick, removes the mob from the world and records a
    /// shoulder rider for its owner — the mount half. Then, once the owner
    /// is reported asleep and the 20-tick minimum ride has elapsed, the next
    /// several ticks must dismount it: the parrot reappears as a real mob
    /// again and the shoulder-rider slot clears. Both halves driven through
    /// production `MobSim::tick`, not through `resolve_shoulder_mounts`/
    /// `tick_shoulder_dismounts` called directly.
    #[test]
    fn a_parrots_shoulder_request_despawns_it_and_a_sleeping_owner_dismounts_it_back() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let owner_uuid = Uuid::new_v4();
        let owner_entity_id = 500;
        sim.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity {
                uuid: owner_uuid,
                entity_id: owner_entity_id,
            }),
            perception: PlayerPerception {
                position: Vec3::new(0.0, 64.0, 0.0),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        let id = sim.spawn_species(parrot_key(), Vec3::new(0.0, 64.0, 0.0)).id();
        sim.get_mut(id).expect("just spawned").tame(MobOwner::Player(owner_uuid));
        sim.get_mut(id).expect("just spawned").mob.request_shoulder_ride();

        sim.tick();

        assert_eq!(
            sim.mobs.iter().filter(|m| m.entity_type.path() == "parrot").count(),
            0,
            "a mounted parrot must be removed from the world, matching vanilla's own \
             shoulder-mount setter's discard"
        );
        assert_eq!(sim.shoulder_riders.len(), 1, "the mount must be recorded for its owner");

        // The owner falls asleep. The dismount is gated on the 20-tick
        // minimum ride (vanilla's own per-player shoulder-mount timer plus 20), so this
        // must run well past that before asserting.
        let since = sim.tick_count;
        sim.set_sleeping_players(vec![(owner_entity_id, since)]);
        for _ in 0..30 {
            sim.tick();
        }

        assert_eq!(
            sim.mobs.iter().filter(|m| m.entity_type.path() == "parrot").count(),
            1,
            "a sleeping owner past the ride-cooldown grace period must dismount and respawn the parrot"
        );
        assert!(sim.shoulder_riders.is_empty(), "the shoulder slot must clear on dismount");
    }

    /// The 20-tick minimum-ride grace period, isolated: an owner reported
    /// asleep on the *same* tick the parrot mounts must not dismount it
    /// immediately — the `mounted_tick + 20 < game_tick` grace-period guard.
    #[test]
    fn a_freshly_mounted_parrot_does_not_dismount_before_the_grace_period() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let owner_uuid = Uuid::new_v4();
        let owner_entity_id = 501;
        sim.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity {
                uuid: owner_uuid,
                entity_id: owner_entity_id,
            }),
            perception: PlayerPerception {
                position: Vec3::new(0.0, 64.0, 0.0),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        let id = sim.spawn_species(parrot_key(), Vec3::new(0.0, 64.0, 0.0)).id();
        sim.get_mut(id).expect("just spawned").tame(MobOwner::Player(owner_uuid));
        sim.get_mut(id).expect("just spawned").mob.request_shoulder_ride();
        sim.tick();
        assert_eq!(sim.shoulder_riders.len(), 1);

        let since = sim.tick_count;
        sim.set_sleeping_players(vec![(owner_entity_id, since)]);
        // Well under the 20-tick grace period.
        for _ in 0..5 {
            sim.tick();
        }
        assert_eq!(
            sim.shoulder_riders.len(),
            1,
            "a parrot inside its 20-tick minimum ride must not dismount just because the owner sleeps"
        );
    }
}

/// The cat block search (`MobSim::tick_cat_block_search`) uses a host-computed
/// candidate position.
#[cfg(test)]
mod cat_block_search_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    /// Set at `y = -1` so it never collides with a search target block a
    /// test places at `y = 0`.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn spawn_cat(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        sim.spawn_species("minecraft:cat".parse().expect("valid key"), pos).id()
    }

    /// The headline case: a chest three blocks away, with clear headroom,
    /// must be found and fed to the sit goal's seam — and the bed seam must
    /// stay empty, since nothing bed-shaped exists in this world.
    #[test]
    fn a_cat_finds_a_nearby_chest_as_its_sit_target() {
        let mut world = flat_world();
        world.set_block(3, 0, 0, "minecraft:chest");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        let target = cat.mob.cat_sit_target();
        assert_eq!(
            target,
            Some(Vec3::new(3.5, 1.0, 0.5)),
            "the sit target must be the chest's stand-on point, got {target:?}"
        );
        assert_eq!(cat.mob.cat_bed_target(), None, "no bed exists in this world");
    }

    /// A bed's *foot* part must feed both seams: the sit goal accepts a bed
    /// foot (vanilla's own valid-target check's own third clause) and the
    /// lie goal accepts any bed part.
    #[test]
    fn a_cat_finds_a_nearby_bed_foot_for_both_seams() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:red_bed[facing=north,part=foot]");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        assert!(cat.mob.cat_sit_target().is_some(), "a bed foot is a valid sit target too");
        assert!(cat.mob.cat_bed_target().is_some(), "a bed foot is a valid lie target");
    }

    /// A bed's *head* part must be excluded from the sit seam
    /// (vanilla's own valid-target check's "not the head part" clause) but
    /// still accepted by the lie seam, which makes no part distinction at
    /// all (vanilla's own lie-goal valid-target check).
    #[test]
    fn a_beds_head_part_is_excluded_from_sitting_but_not_from_lying() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:red_bed[facing=north,part=head]");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        assert_eq!(cat.mob.cat_sit_target(), None, "a bed head must not be a sit target");
        assert!(cat.mob.cat_bed_target().is_some(), "a bed head is still a valid lie target");
    }

    /// An unlit furnace is not a valid sit target — only the lit furnace
    /// state qualifies (vanilla's own valid-target check's second clause).
    #[test]
    fn an_unlit_furnace_is_not_a_sit_target_but_a_lit_one_is() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:furnace[facing=north,lit=false]");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        sim.tick();
        assert_eq!(
            sim.get(id).expect("spawned").mob.cat_sit_target(),
            None,
            "an unlit furnace must not be a sit target"
        );

        let mut world2 = flat_world();
        world2.set_block(0, 0, 2, "minecraft:furnace[facing=north,lit=true]");
        let mut sim2 = MobSim::new(&world2);
        let id2 = spawn_cat(&mut sim2, Vec3::new(0.0, 0.0, 0.0));
        sim2.tick();
        assert!(
            sim2.get(id2).expect("spawned").mob.cat_sit_target().is_some(),
            "a lit furnace must be a sit target"
        );
    }

    /// A chest with no headroom (a solid block directly above it) must be
    /// rejected — vanilla's own valid-target check's
    /// "is empty block one above" clause.
    #[test]
    fn a_chest_with_no_headroom_is_rejected() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:chest");
        world.set_block(0, 1, 2, "minecraft:stone");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        assert_eq!(
            sim.get(id).expect("spawned").mob.cat_sit_target(),
            None,
            "a chest with a solid block on top must not be a sit target"
        );
    }

    /// An empty world (no chest, furnace or bed anywhere in range) must
    /// leave both seams empty — the negative control.
    #[test]
    fn an_empty_world_finds_neither_target() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        assert_eq!(cat.mob.cat_sit_target(), None);
        assert_eq!(cat.mob.cat_bed_target(), None);
    }

    /// A non-cat species (a villager, which also walks around chests every
    /// day) must never receive a cat block search feed — the species filter
    /// is load-bearing, not merely a cost optimisation.
    #[test]
    fn a_non_cat_species_never_receives_a_cat_block_search_feed() {
        let mut world = flat_world();
        world.set_block(1, 0, 0, "minecraft:chest");
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        sim.tick();

        assert_eq!(sim.get(id).expect("spawned").mob.cat_sit_target(), None);
    }
}

/// Villager bed claiming runs through `tick_villager_beds` and
/// `occupied_homes_in_range` in the real per-tick [`MobSim`] loop, exercising
/// the integrated path rather than standalone helpers.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod villager_bed_claim_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn spawn_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), pos).id()
    }

    /// The headline case: a villager standing near an unclaimed bed claims it
    /// over a real tick, and the claim is visible through the same live query
    /// the raid trigger needs (`occupied_homes_in_range`).
    #[test]
    fn a_villager_claims_a_nearby_bed_through_a_real_tick_and_it_becomes_findable() {
        let mut world = flat_world();
        world.set_block(3, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let claimed = sim.get(id).expect("just spawned").bed();
        assert_eq!(
            claimed,
            Some(BlockPos::new(3, 0, 0)),
            "the villager must claim the only nearby bed"
        );
        let found = sim.occupied_homes_in_range(BlockPos::new(0, 0, 0), 64);
        assert_eq!(
            found,
            vec![BlockPos::new(3, 0, 0)],
            "the raid trigger's live query must see the claim the same tick it happens"
        );
    }

    /// Two villagers, one bed — the discriminating shape every claim gate in
    /// this file uses: a single-villager test would pass under an
    /// implementation with no occupancy at all.
    #[test]
    fn a_second_villager_cannot_claim_an_already_claimed_bed_through_the_sim() {
        let mut world = flat_world();
        world.set_block(3, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        let first = spawn_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        let second = spawn_villager(&mut sim, Vec3::new(1.0, 0.0, 0.0));

        sim.tick();

        let first_bed = sim.get(first).expect("spawned").bed();
        let second_bed = sim.get(second).expect("spawned").bed();
        assert_eq!(first_bed, Some(BlockPos::new(3, 0, 0)));
        assert_eq!(
            second_bed, None,
            "the bed has one ticket and the closer villager already holds it"
        );
    }

    /// A non-villager species standing right next to a bed must never claim
    /// it — the species filter is load-bearing, the same control
    /// `cat_block_search_tests` already runs for its own search.
    #[test]
    fn a_non_villager_species_never_claims_a_bed() {
        let mut world = flat_world();
        world.set_block(1, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        sim.tick();

        assert_eq!(sim.get(id).expect("spawned").bed(), None);
        assert!(sim.occupied_homes_in_range(BlockPos::new(0, 0, 0), 64).is_empty());
    }
}

/// WORK/MEET/REST schedule: proves the chain claimed-POI ->
/// `MobSim::set_day_time` -> `crate::brain::roster::villager_brain`'s
/// schedule -> `WalkToPoi`/`MoveToTargetSink` -> a real position change
/// reaches a real, spawned villager through `MobSim::tick`, the same
/// not-an-island bar `villager_bed_claim_tests` already sets for bed
/// claiming and `vibration_substrate_tests` sets for the warden.
#[cfg(test)]
mod villager_schedule_tests {
    use super::*;

    /// A flat, walkable floor wide enough that pathfinding across it never
    /// runs off the edge — the same shape `a_grazing_mob_hands_its_eat_to_the_driver`
    /// already establishes for a real multi-tick walk.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -32..=32 {
            for z in -32..=32 {
                world.set_block(x, -1, z, "minecraft:grass_block");
            }
        }
        world
    }

    fn spawn_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), pos).id()
    }

    fn horizontal_distance(a: Vec3, b: Vec3) -> f64 {
        (a.x - b.x).hypot(a.z - b.z)
    }

    /// **The headline case.** A villager spawns 20 blocks from a composter
    /// (a real `farmer` job site), stays idle at a day time before `WORK`
    /// starts (`2000`, `VILLAGER_SCHEDULE`'s own keyframe), then the clock
    /// enters the `WORK` window and the villager visibly closes most of the
    /// distance to its claimed workstation over real ticks — not merely
    /// "the position changed", a magnitude check against the starting gap,
    /// so a villager that only ever random-strolls (and might coincidentally
    /// drift a block or two toward the composter) cannot pass this by luck.
    #[test]
    fn a_villager_walks_to_its_claimed_workstation_once_work_begins() {
        let mut world = flat_world();
        // Inside `villager::SEARCH_RADIUS` (16 blocks) so the villager's
        // bounded job search can actually find it, and far enough past
        // `WalkToPoi`'s own 9-block close-enough radius that "arrived" and
        // "started here" are unambiguously different distances.
        let composter = BlockPos::new(15, 0, 0);
        world.set_block(composter.x, composter.y, composter.z, "minecraft:composter");

        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

        // Before `WORK` (schedule keyframe `2000`): let the villager claim
        // the workstation (job search is unthrottled on its first tick) but
        // never let the clock enter `WORK`, so any position drift here is
        // attributable only to `IDLE`'s own random stroll, not to this
        // schedule.
        for _ in 0..5 {
            sim.set_day_time(500);
            sim.tick();
        }
        assert_eq!(
            sim.get(id).expect("just spawned").workstation(),
            Some(composter),
            "the villager must have claimed the only nearby composter before WORK ever starts"
        );

        let workstation_center = Vec3::new(
            f64::from(composter.x) + 0.5,
            f64::from(composter.y) + 0.5,
            f64::from(composter.z) + 0.5,
        );
        let initial_distance = horizontal_distance(sim.get(id).expect("spawned").position(), workstation_center);

        // Now enter the WORK window and let the schedule + WalkToPoi close
        // the gap over real ticks.
        for _ in 0..400 {
            sim.set_day_time(3000);
            sim.tick();
        }
        let final_distance = horizontal_distance(sim.get(id).expect("spawned").position(), workstation_center);

        // Two predictions, not just "it got closer": `WalkToPoi::new(JOB_SITE,
        // …, 9)` stops issuing a fresh walk target once within 9 blocks
        // (`MoveToTargetSink::reached`'s own `+ 0.5` tolerance), so a working
        // villager should end up **near that exact radius**, not merely
        // "somewhat closer" — which a lucky IDLE stroll could also produce.
        assert!(
            final_distance <= 10.5,
            "a villager working its claimed job site should stop within WalkToPoi's own \
             9-block close-enough radius (plus MoveToTargetSink's 0.5 tolerance): \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
        assert!(
            initial_distance - final_distance > 4.0,
            "WORK must walk the villager measurably closer to its claimed workstation: \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
    }

    /// [`a_villager_walks_to_its_claimed_workstation_once_work_begins`]'s own
    /// sibling for `MEET`/bells rather than `WORK`/workstations — proving the
    /// third POI (the one this session's own `BellClaims` adds) reaches the
    /// identical real chain: claim -> schedule -> `WalkToPoi` -> a real
    /// position change.
    #[test]
    fn a_villager_walks_to_its_claimed_bell_once_meet_begins() {
        let mut world = flat_world();
        let bell = BlockPos::new(15, 0, 0);
        world.set_block(bell.x, bell.y, bell.z, "minecraft:bell[attachment=floor,facing=south]");

        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

        // Before `MEET` (schedule keyframe `9000`): let the villager claim
        // the bell but keep the clock in `IDLE`'s own window.
        for _ in 0..5 {
            sim.set_day_time(500);
            sim.tick();
        }
        assert_eq!(
            sim.get(id).expect("just spawned").meeting_point(),
            Some(bell),
            "the villager must have claimed the only nearby bell before MEET ever starts"
        );

        let bell_center = Vec3::new(f64::from(bell.x) + 0.5, f64::from(bell.y) + 0.5, f64::from(bell.z) + 0.5);
        let initial_distance = horizontal_distance(sim.get(id).expect("spawned").position(), bell_center);

        for _ in 0..400 {
            sim.set_day_time(9500);
            sim.tick();
        }
        let final_distance = horizontal_distance(sim.get(id).expect("spawned").position(), bell_center);

        // `WalkToPoi::new(MEETING_POINT, …, 6)` — a tighter close-enough
        // radius than the job site's `9`, which is the meeting-point walk
        // target's range.
        assert!(
            final_distance <= 7.5,
            "a villager meeting at its claimed bell should stop within WalkToPoi's own \
             6-block close-enough radius (plus MoveToTargetSink's 0.5 tolerance): \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
        assert!(
            initial_distance - final_distance > 4.0,
            "MEET must walk the villager measurably closer to its claimed bell: \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
    }

    /// The schedule's own negative control: a villager with **no** claimed
    /// job site (nothing nearby to claim) never becomes `WORK`-eligible —
    /// the generic villager activity requirement that a job-site memory be
    /// present — so it stays wherever `IDLE`'s random stroll leaves
    /// it: never *reliably* walking toward a fixed faraway point regardless
    /// of the clock. Asserted as "never claims a workstation", the
    /// discriminating fact this control actually has available deterministically
    /// (a stroll's own endpoint is randomised and not itself a safe assertion).
    #[test]
    fn a_villager_with_no_nearby_job_site_never_claims_one_regardless_of_the_clock() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

        for _ in 0..200 {
            sim.set_day_time(3000);
            sim.tick();
        }

        assert_eq!(
            sim.get(id).expect("spawned").workstation(),
            None,
            "with no workstation block anywhere nearby there is nothing to claim, \
             so WORK can never become eligible no matter how long the clock sits in its window"
        );
    }
}

/// Vibration events produced by `reap_dead` reach `resolve_vibrations` through
/// the real per-tick [`MobSim`] loop.
#[cfg(test)]
mod vibration_substrate_tests {
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
}

/// The elder guardian's mining-fatigue aura,
/// vanilla's own elder-guardian AI step calling
/// its own "add effect to players around" helper.
#[cfg(test)]
mod elder_guardian_mining_fatigue_tests {
    use super::*;

    /// A real floor near the origin — see `leash_tests::flat_world`'s own
    /// doc comment for why a bare void `ChunkWorld` stopped being safe once
    /// idle mobs fall. This module's `x = 60` position is a *player*
    /// (`set_players`), not a spawned mob, so it is unaffected either way
    /// and is deliberately left outside the floor.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
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

    /// `(tickCount + getId()) % 1200 == 0`, with `tick_count` standing in for
    /// vanilla's own generic tick-count field (see [`ELDER_GUARDIAN_EFFECT_INTERVAL`]'s own doc).
    /// A freshly spawned elder guardian gets id `1`, so the trigger tick is
    /// `1200 - 1 = 1199`; [`MobSim::tick`] reads `self.tick_count` *before*
    /// incrementing it, so seeding `set_tick_count(1199)` and ticking once is
    /// the tick this pulse fires on.
    #[test]
    fn a_player_within_fifty_blocks_is_pulsed_on_the_interval_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let guardian_id = sim
            .spawn_species(
                "minecraft:elder_guardian".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(40.0, 0.0, 0.0))]);
        sim.set_tick_count(1199);
        sim.tick();

        let pulses = sim.take_mining_fatigue_auras();
        assert_eq!(
            pulses,
            vec![MiningFatigueAura {
                target: PlayerIdentity {
                    uuid: alice,
                    entity_id: 99
                }
            }],
            "a player 40 blocks away (within the 50-block radius) must be pulsed on tick 1199, got {pulses:?}"
        );
    }

    /// The same setup, moved just past `EFFECT_RADIUS` — the spherical
    /// distance cut, not a box.
    #[test]
    fn a_player_beyond_fifty_blocks_is_not_pulsed() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let guardian_id = sim
            .spawn_species(
                "minecraft:elder_guardian".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(60.0, 0.0, 0.0))]);
        sim.set_tick_count(1199);
        sim.tick();

        assert!(
            sim.take_mining_fatigue_auras().is_empty(),
            "a player 60 blocks away is outside EFFECT_RADIUS and must not be pulsed"
        );
    }

    /// A tick that is not a multiple of the 1200-tick interval must pulse
    /// nobody, even with a player standing on top of the guardian.
    #[test]
    fn no_pulse_off_the_interval_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species(
            "minecraft:elder_guardian".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        );

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(0.0, 0.0, 0.0))]);
        sim.set_tick_count(1198);
        sim.tick();

        assert!(
            sim.take_mining_fatigue_auras().is_empty(),
            "one tick before the interval must not fire"
        );
    }

    /// An ordinary guardian (not elder) must never pulse — the aura is
    /// elder-guardian-only in vanilla; the ordinary guardian's own AI step has no
    /// such call.
    #[test]
    fn an_ordinary_guardian_never_pulses() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let guardian_id = sim
            .spawn_species(
                "minecraft:guardian".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(0.0, 0.0, 0.0))]);
        sim.set_tick_count(1199);
        sim.tick();

        assert!(
            sim.take_mining_fatigue_auras().is_empty(),
            "an ordinary guardian must never emit a mining-fatigue pulse"
        );
    }

    /// The magnitude gate: the constants a driver applies must match
    /// vanilla's own elder-guardian effect-duration/effect-amplifier fields, not
    /// a plausible-looking round number.
    #[test]
    fn effect_constants_match_the_jar() {
        assert_eq!(ELDER_GUARDIAN_EFFECT_DURATION, 6000);
        assert_eq!(ELDER_GUARDIAN_EFFECT_AMPLIFIER, 2, "Mining Fatigue III is amplifier 2, zero-indexed");
        assert_eq!(ELDER_GUARDIAN_EFFECT_RADIUS, 50.0);
    }
}

/// Goat spawn-finalization's pre-broken-horn roll and the metadata field
/// ([`crate::protocol::MetadataField::GoatHorns`]) that reaches the client.
/// The test wires both through a real [`MobSim::spawn_species`] call.
#[cfg(test)]
mod goat_horn_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// **Real arithmetic, not merely "sometimes true"**: over a large sample,
    /// the fraction of goats spawned missing a horn must land near
    /// vanilla's own goat spawn-finalization's own `0.1` roll — bounded generously (5%–15%
    /// over 2,000 trials) since this crate's `SpawnRng` is not a
    /// bit-identical port of `java.util.Random` (a disclosed approximation
    /// already established elsewhere in this crate, e.g. `raid::bonus_spawns`).
    #[test]
    fn about_one_in_ten_goats_spawn_missing_a_horn() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let trials = 2000;
        let mut missing = 0;
        for _ in 0..trials {
            let id = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
            let m = sim.get(id).expect("just spawned");
            if !(m.has_left_horn() && m.has_right_horn()) {
                missing += 1;
            }
        }
        let fraction = f64::from(missing) / f64::from(trials);
        assert!(
            (0.05..=0.15).contains(&fraction),
            "expected roughly 10% of {trials} goats missing a horn, got {missing} ({:.1}%)",
            fraction * 100.0
        );
    }

    /// The discriminating control: a goat that *does* lose a horn loses
    /// exactly one, never both — proves the roll picks a single horn rather
    /// than clearing the pair.
    #[test]
    fn a_goat_that_loses_a_horn_loses_exactly_one() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let mut saw_a_miss = false;
        for _ in 0..500 {
            let id = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
            let m = sim.get(id).expect("just spawned");
            let horn_count = i32::from(m.has_left_horn()) + i32::from(m.has_right_horn());
            assert!((1..=2).contains(&horn_count), "a goat must never lose both horns at spawn");
            if horn_count == 1 {
                saw_a_miss = true;
            }
        }
        assert!(saw_a_miss, "500 trials at a 10% roll must produce at least one miss");
    }

    /// A non-goat species is never touched by the roll — both accessors stay
    /// their default `true`, matching every other species' "meaningless"
    /// reading.
    #[test]
    fn a_non_goat_species_always_reports_both_horns() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        for _ in 0..20 {
            let id = sim.spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
            let m = sim.get(id).expect("just spawned");
            assert!(m.has_left_horn() && m.has_right_horn());
        }
    }

    /// The wiring proof: `SimMob::snapshot` pushes `MetadataField::GoatHorns`
    /// for a goat, carrying whatever [`SimMob::has_left_horn`]/
    /// [`SimMob::has_right_horn`] actually are — and pushes nothing for a
    /// species this field does not apply to, which is the control that rules
    /// out "always pushed regardless of species".
    #[test]
    fn snapshot_carries_goat_horns_for_a_goat_and_nothing_for_a_pig() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let goat = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
        let pig = sim.spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.0, 0.0, 0.0)).id();

        let goat_mob = sim.get_mut(goat).expect("spawned");
        goat_mob.has_left_horn = false;
        goat_mob.has_right_horn = true;
        let goat_snapshot = sim.get(goat).expect("spawned").snapshot();
        assert_eq!(
            goat_snapshot.metadata,
            vec![crate::protocol::MetadataField::GoatHorns { has_left: false, has_right: true }],
            "the snapshot must carry the mob's own current horn state, not the spawn default"
        );

        let pig_snapshot = sim.get(pig).expect("spawned").snapshot();
        assert!(
            !pig_snapshot.metadata.iter().any(|f| matches!(f, crate::protocol::MetadataField::GoatHorns { .. })),
            "a pig must never carry a GoatHorns field"
        );
    }
}
