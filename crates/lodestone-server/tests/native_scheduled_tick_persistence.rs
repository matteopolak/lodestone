//! Native scheduled ticks retain their complete scheduler ordering key across
//! a close and reopen without changing the Anvil path.

use std::path::PathBuf;

use lodestone_server::world_storage::{Error, WorldStorage, WorldStorageBackend};
use lodestone_server::{ChunkColumn, ScheduledTickHandle, TickPriority};

fn tempdir(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "lodestone-native-scheduled-ticks-{name}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).expect("create native test segment");
    directory
}

#[test]
fn native_chunk_ticks_reopen_in_their_original_world_wide_order() {
    let directory = tempdir("reopen");
    let column = ChunkColumn::new(0, 16);
    let first = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("open first native store");
    let scheduled = ScheduledTickHandle::new();

    scheduled.with(|queues| {
        // Deliberately schedule east before west. On reopen the columns are
        // loaded in the opposite order; only the persisted insertion key can
        // retain this final scheduler tie-breaker.
        assert!(queues.block.schedule(
            (16, 4, 0),
            "redstone:repeater".to_owned(),
            50,
            TickPriority::Normal,
        ));
        assert!(queues.block.schedule(
            (0, 4, 0),
            "redstone:torch".to_owned(),
            50,
            TickPriority::High,
        ));
        assert!(queues.block.schedule(
            (-16, 4, 0),
            "redstone:observer".to_owned(),
            50,
            TickPriority::Normal,
        ));
        assert!(queues.fluid.schedule(
            (16, 5, 0),
            "lodestone:fluid".to_owned(),
            51,
            TickPriority::VeryLow,
        ));
    });

    for (x, z) in [(1, 0), (0, 0), (-1, 0)] {
        first
            .write_dirty_chunk_with_scheduled_ticks(x, z, &column, &scheduled)
            .expect("write typed column and its pending ticks");
    }
    drop(first);

    let reopened = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("reopen native store");
    let restored = ScheduledTickHandle::new();
    for (x, z) in [(-1, 0), (0, 0), (1, 0)] {
        assert!(reopened
            .load_chunk_with_scheduled_ticks(x, z, 0, 16, &restored)
            .expect("load typed column")
            .is_some());
    }

    let (block, fluid) = restored.with(|queues| {
        let block = queues
            .block
            .drain_due(50, usize::MAX)
            .into_iter()
            .map(|tick| (tick.pos, tick.kind, tick.trigger_tick, tick.priority))
            .collect::<Vec<_>>();
        let fluid = queues
            .fluid
            .drain_due(51, usize::MAX)
            .into_iter()
            .map(|tick| (tick.pos, tick.kind, tick.trigger_tick, tick.priority))
            .collect::<Vec<_>>();
        (block, fluid)
    });
    assert_eq!(
        block,
        vec![
            ((0, 4, 0), "redstone:torch".to_owned(), 50, TickPriority::High),
            ((16, 4, 0), "redstone:repeater".to_owned(), 50, TickPriority::Normal),
            ((-16, 4, 0), "redstone:observer".to_owned(), 50, TickPriority::Normal),
        ],
        "priority must beat insertion order, then the stored world-wide order must beat reload order"
    );
    assert_eq!(
        fluid,
        vec![(
            (16, 5, 0),
            "lodestone:fluid".to_owned(),
            51,
            TickPriority::VeryLow,
        )],
        "the fluid queue remains distinct and retains its trigger and priority"
    );
    drop(reopened);
    std::fs::remove_dir_all(directory).expect("remove native test segment");
}

#[test]
fn native_tick_save_refuses_a_custom_action_instead_of_losing_it() {
    let directory = tempdir("reject-custom");
    let storage = WorldStorage::open(WorldStorageBackend::LodestoneNative {
        directory: directory.clone(),
    })
    .expect("open native store");
    let scheduled = ScheduledTickHandle::new();
    scheduled.with(|queues| {
        assert!(queues.block.schedule(
            (0, 4, 0),
            "example:plugin_action".to_owned(),
            20,
            TickPriority::Normal,
        ));
    });
    assert!(matches!(
        storage.write_dirty_chunk_with_scheduled_ticks(0, 0, &ChunkColumn::new(0, 16), &scheduled),
        Err(Error::Chunk(lodestone_server::world_storage::ChunkRecordError::UnsupportedScheduledTickKind(kind))) if kind == "example:plugin_action"
    ));
    drop(storage);
    std::fs::remove_dir_all(directory).expect("remove native test segment");
}
