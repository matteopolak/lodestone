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
