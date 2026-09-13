//! Cross-owner handoff parity for scheduled ticks and block entities.
//!
//! Each fixture places work in two chunk owners, deliberately observes the
//! owner results in reverse completion order, and sends those results through
//! the same central merge functions used by the tick path. The duplicate and
//! missing controls prove that a partial completion cannot be published.

use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};

use lodestone_model::{BlockPos, ItemStack};
use lodestone_server::{
    BlockEntity, BlockEntityHandle, BlockEntityTickEffectBatch, BlockEntityTickOwner,
    ChunkScheduledTickQueue, Furnace, FurnaceKind, ScheduledTick, ScheduledTickKind,
    ScheduledTickOwner, ScheduledTickOwnerBatch, TickPriority, merge_due_owner_batches,
    merge_tick_effect_batches,
};

fn assert_rejects_completion<T>(work: impl FnOnce() -> T) {
    assert!(
        catch_unwind(AssertUnwindSafe(work)).is_err(),
        "a missing or duplicate owner completion must be rejected"
    );
}

fn scheduled_owner_batches() -> Vec<ScheduledTickOwnerBatch<ScheduledTickKind>> {
    let mut queue = ChunkScheduledTickQueue::new();
    assert!(queue.schedule(
        (-1, 64, 0),
        ScheduledTickKind::Torch,
        7,
        TickPriority::Normal,
    ));
    assert!(queue.schedule(
        (16, 64, 0),
        ScheduledTickKind::Repeater,
        7,
        TickPriority::Normal,
    ));
    assert!(queue.schedule(
        (-1, 65, 0),
        ScheduledTickKind::Comparator,
        7,
        TickPriority::Normal,
    ));
    queue.drain_due_owner_batches(7, usize::MAX)
}

fn scheduled_keys(
    ticks: &[ScheduledTick<ScheduledTickKind>],
) -> Vec<((i32, i32, i32), ScheduledTickKind)> {
    ticks
        .iter()
        .map(|tick| (tick.pos, tick.kind.clone()))
        .collect()
}

#[test]
fn scheduled_owner_handoff_restores_order_and_rejects_non_exact_completion() {
    let batches = scheduled_owner_batches();
    assert_eq!(
        batches.iter().map(|batch| batch.owner).collect::<Vec<_>>(),
        [
            ScheduledTickOwner::Chunk { cx: -1, cz: 0 },
            ScheduledTickOwner::Chunk { cx: 1, cz: 0 },
        ],
        "the plan must contain exactly the two disjoint chunk owners"
    );

    let expected = vec![
        ((-1, 64, 0), ScheduledTickKind::Torch),
        ((16, 64, 0), ScheduledTickKind::Repeater),
        ((-1, 65, 0), ScheduledTickKind::Comparator),
    ];
    let serial = merge_due_owner_batches(batches.clone());
    assert_eq!(scheduled_keys(&serial), expected);

    let mut reversed = batches.clone();
    reversed.reverse();
    let completion_order: Vec<_> = reversed
        .iter()
        .flat_map(|batch| batch.assignments())
        .map(|assignment| (assignment.tick().pos, assignment.tick().kind.clone()))
        .collect();
    assert_ne!(
        completion_order, expected,
        "the control must make reversed owner completion observable before merging"
    );
    assert_eq!(
        scheduled_keys(&merge_due_owner_batches(reversed)),
        expected,
        "central publication must restore the tick-start serial order"
    );

    let mut missing = batches.clone();
    missing.pop();
    assert_rejects_completion(|| merge_due_owner_batches(missing));

    let mut duplicate = batches.clone();
    duplicate.push(duplicate.first().expect("two owner batches").clone());
    assert_rejects_completion(|| merge_due_owner_batches(duplicate));
}

#[test]
fn scheduled_owner_batches_are_a_disjoint_cover_across_chunk_boundaries() {
    fn queue() -> ChunkScheduledTickQueue<&'static str> {
        let mut q = ChunkScheduledTickQueue::new();
        assert!(q.schedule((-17, 0, -1), "negative", 10, TickPriority::Normal));
        assert!(q.schedule((-16, 0, 0), "west", 10, TickPriority::High));
        assert!(q.schedule((15, 0, 0), "origin", 10, TickPriority::Low));
        assert!(q.schedule((16, 0, 0), "east", 10, TickPriority::Normal));
        assert!(q.schedule((31, 0, 16), "north-east", 10, TickPriority::Normal));
        q
    }

    let mut expected_queue = queue();
    let expected = expected_queue.drain_due(10, usize::MAX);
    let expected_keys: BTreeSet<_> = expected.iter().map(|tick| (tick.pos, tick.kind)).collect();
    let mut owner_queue = queue();
    let batches = owner_queue.drain_due_owner_batches(10, usize::MAX);

    let mut seen = BTreeSet::new();
    for batch in &batches {
        for assignment in batch.assignments() {
            let tick = assignment.tick();
            let owner = ScheduledTickOwner::Chunk {
                cx: tick.pos.0.div_euclid(16),
                cz: tick.pos.2.div_euclid(16),
            };
            assert_eq!(batch.owner, owner, "a tick must stay with its target chunk");
            assert!(
                seen.insert((tick.pos, tick.kind)),
                "one selected tick must not overlap two owner batches"
            );
        }
    }
    assert_eq!(seen, expected_keys, "owner batches must cover every selected tick exactly once");
    let merged = merge_due_owner_batches(batches);
    assert_eq!(
        merged.iter().map(|tick| (tick.pos, tick.kind)).collect::<Vec<_>>(),
        expected.iter().map(|tick| (tick.pos, tick.kind)).collect::<Vec<_>>(),
        "central merge must restore the world-wide drain order"
    );
}

fn stack(item: &str, count: u32) -> ItemStack {
    ItemStack::new(item.parse().expect("valid item key"), count)
}

fn furnace() -> Furnace {
    let mut furnace = Furnace::new(FurnaceKind::Furnace);
    furnace.set_fuel(Some(stack("minecraft:coal", 1)));
    furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
    furnace
}

fn effect_keys(
    batches: &[BlockEntityTickEffectBatch],
) -> Vec<(BlockEntityTickOwner, BlockPos, bool)> {
    batches
        .iter()
        .flat_map(|batch| batch.effects())
        .map(|effect| (effect.owner, effect.pos, effect.lit))
        .collect()
}

#[test]
fn block_entity_owner_handoff_restores_order_and_rejects_non_exact_completion() {
    let west = BlockPos::new(-1, 70, 0);
    let east = BlockPos::new(16, 70, 0);
    let handle = BlockEntityHandle::new();
    handle.with(|registry| {
        registry.insert(west, BlockEntity::Furnace(furnace()));
        registry.insert(east, BlockEntity::Furnace(furnace()));
        let owners: Vec<_> = registry
            .tick_plan()
            .owner_batches()
            .into_iter()
            .map(|batch| batch.owner)
            .collect();
        assert_eq!(
            owners,
            [
                BlockEntityTickOwner::Chunk { cx: -1, cz: 0 },
                BlockEntityTickOwner::Chunk { cx: 1, cz: 0 },
            ],
            "the block-entity plan must keep disjoint chunk owners separate"
        );
    });

    let batches = handle.tick_non_hoppers_by_owner(&|_| true);
    let expected = vec![
        (BlockEntityTickOwner::Chunk { cx: -1, cz: 0 }, west, true),
        (BlockEntityTickOwner::Chunk { cx: 1, cz: 0 }, east, true),
    ];
    assert_eq!(effect_keys(&batches), expected);
    handle.with(|registry| {
        for pos in [west, east] {
            let Some(BlockEntity::Furnace(furnace)) = registry.get(pos) else {
                panic!("owner application must retain the furnace at {pos:?}");
            };
            assert!(furnace.is_lit(), "owner application must advance {pos:?} exactly once");
        }
    });

    let mut reversed = batches.clone();
    reversed.reverse();
    assert_ne!(
        effect_keys(&reversed),
        expected,
        "the control must make reversed owner completion observable before merging"
    );
    assert_eq!(
        effect_keys_from_flattened(&merge_tick_effect_batches(reversed)),
        expected,
        "central publication must restore the tick-start serial order"
    );

    let mut missing = batches.clone();
    missing.pop();
    assert_rejects_completion(|| merge_tick_effect_batches(missing));

    let mut duplicate = batches.clone();
    duplicate.push(duplicate.first().expect("two owner batches").clone());
    assert_rejects_completion(|| merge_tick_effect_batches(duplicate));
}

fn effect_keys_from_flattened(
    effects: &[lodestone_server::BlockEntityTickEffect],
) -> Vec<(BlockEntityTickOwner, BlockPos, bool)> {
    effects
        .iter()
        .map(|effect| (effect.owner, effect.pos, effect.lit))
        .collect()
}
