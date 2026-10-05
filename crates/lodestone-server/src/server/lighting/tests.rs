//! Tests for relight batching, tick block updates and light delivery.

use super::*;
use crate::server::tests::{ColdColumnSource, RetainedLifecycleProtocol};
use crate::chunk::ChunkColumn;
use std::sync::atomic::{AtomicUsize, Ordering};
use crate::server::tests::{default_block_state, RefusingChunkProtocol};

#[test]
fn tick_relight_requires_a_resident_light_footprint_without_generating() {
    let cold = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };
    assert!(resident_light_neighbourhood(&cold, 0, 0, 1).is_none());
    assert_eq!(
        cold.column_reads.load(Ordering::Relaxed),
        0,
        "a missing tick-light neighbour must defer to the future chunk snapshot, not generate"
    );

    let warm = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let (_, neighbours) = resident_light_neighbourhood(&warm, 0, 0, 1)
        .expect("a resident 3x3 footprint must be available for a live relight");
    assert_eq!(neighbours.len(), 8, "the cross-column footprint has all eight neighbours");
    assert_eq!(
        warm.column_reads.load(Ordering::Relaxed),
        0,
        "a complete resident footprint must also avoid the generating accessor"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn detached_tick_light_is_invalidated_by_a_later_block_edit() {
    fn flat_light(
        centre: &ChunkColumn,
        neighbours: &[(i32, i32, &ChunkColumn)],
        _: crate::dimension::Dimension,
    ) -> lodestone_world::ColumnLight {
        assert_eq!(neighbours.len(), 8);
        let mut light = lodestone_world::ColumnLight::new(centre.section_count());
        *light.sky_mut(0) = lodestone_world::LightData::Uniform(7);
        light
    }

    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let store = crate::chunk_store::ChunkStore::with_capacity(source, 32);
    for dz in -1..=1 {
        for dx in -1..=1 {
            let _ = store.column(dx, dz);
        }
    }
    let light = compute_detached_relight(
        &store,
        (0, 0),
        crate::dimension::Dimension::Overworld,
        true,
        true,
        flat_light,
    )
    .expect("a resident footprint settles off the connection path");
    assert_eq!(
        store.resident_column(0, 0).unwrap().centre_settled_light(),
        Some(&light)
    );

    store.set_block(0, 0, 0, lodestone_data::block::Block::Stone.default_state());
    assert_ne!(
        store.resident_column(0, 0).unwrap().centre_settled_light(),
        Some(&light),
        "a completed worker result must not be sent after a newer edit"
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn direct_relight_can_complete_a_cold_neighbourhood_off_thread() {
    fn flat_light(
        centre: &ChunkColumn,
        neighbours: &[(i32, i32, &ChunkColumn)],
        _: crate::dimension::Dimension,
    ) -> lodestone_world::ColumnLight {
        assert_eq!(neighbours.len(), 8);
        lodestone_world::ColumnLight::new(centre.section_count())
    }

    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };
    let store = crate::chunk_store::ChunkStore::with_capacity(source, 32);
    let _ = store.column(0, 0);
    assert!(store.resident_column(1, 0).is_none());
    assert!(compute_detached_relight(
        &store,
        (0, 0),
        crate::dimension::Dimension::Overworld,
        true,
        true,
        flat_light,
    )
    .is_none());
    assert!(store.resident_column(1, 0).is_none());
    assert!(compute_detached_relight(
        &store,
        (0, 0),
        crate::dimension::Dimension::Overworld,
        true,
        false,
        flat_light,
    )
    .is_some());
    assert!(store.resident_column(1, 0).is_some());
}

#[tokio::test]
async fn retained_tick_relight_defers_missing_footprint_but_keeps_no_light_fallback() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: true,
    };
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;

    send_resident_column_light(&mut conn, &protocol, &source, &mut state, 0, 0)
        .await
        .expect("a missing resident footprint defers the tick relight");
    assert_eq!(
        protocol.fallback_encodes.load(Ordering::Acquire),
        0,
        "missing resident neighbours must not fall back to an isolated full-column packet"
    );
    assert_eq!(
        source.column_reads.load(Ordering::Acquire),
        0,
        "the resident-only path must not generate a missing neighbour"
    );
    assert_eq!(
        source.store_calls.load(Ordering::Acquire),
        0,
        "a deferred relight must not persist a partial footprint"
    );
    drop(conn);
    drop(client_end);

    // A complete footprint with a protocol that genuinely has no light
    // result remains on the existing compatible full-column fallback.
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: true,
        center_only: false,
    };
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let (_client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    send_resident_column_light(&mut conn, &protocol, &source, &mut state, 0, 0)
        .await
        .expect("a genuine protocol no-light result keeps the fallback");
    assert_eq!(
        protocol.fallback_encodes.load(Ordering::Acquire),
        1,
        "a genuine no-light result must remain distinguishable from a missing footprint"
    );
}

#[tokio::test]
async fn tick_block_updates_only_target_columns_already_delivered() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut pending_relights = PendingRelights::default();

    send_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &HashSet::from([(0, 0)]),
        &mut pending_relights,
        vec![
            crate::tick::TickBlockChange {
                x: 1,
                y: 64,
                z: 1,
                state: default_block_state(Block::GrassBlock),
                needs_relight: true,
            },
            crate::tick::TickBlockChange {
                x: 17,
                y: 64,
                z: 1,
                state: default_block_state(Block::Dirt),
                needs_relight: false,
            },
        ],
    )
    .await
    .expect("tick updates write without a complete join stream");

    let mut peer = Connection::new(client_end);
    assert_eq!(
        peer.read_packet().await.expect("first tick update frame decodes"),
        Some((43, vec![1, 64, 1]))
    );
    assert_eq!(pending_relights.pop_front(), Some((0, 0)));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), peer.read_packet())
            .await
            .is_err(),
        "the pending column's later snapshot supersedes its tick update"
    );
}

#[tokio::test]
async fn tick_block_updates_without_light_changes_skip_relight() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut pending_relights = PendingRelights::default();

    send_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &HashSet::from([(0, 0)]),
        &mut pending_relights,
        vec![crate::tick::TickBlockChange {
            x: 1,
            y: 64,
            z: 1,
            state: default_block_state(Block::GrassBlock),
            needs_relight: false,
        }],
    )
    .await
    .expect("block updates are independent of light changes");

    let mut peer = Connection::new(client_end);
    assert_eq!(
        peer.read_packet().await.expect("block update decodes"),
        Some((43, vec![1, 64, 1]))
    );
    assert!(pending_relights.is_empty());
}

#[tokio::test]
async fn tick_block_update_burst_yields_after_one_ordered_batch() {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut peer = Connection::new(client_end);
    let mut state = State::Play;
    let mut pending_relights = PendingRelights::default();
    let mut pending = VecDeque::new();
    let mut delivered = HashSet::from([(0, 0)]);
    let changes = (0..65)
        .map(|y| crate::tick::TickBlockChange {
            x: 1,
            y,
            z: 1,
            state: default_block_state(Block::Stone),
            needs_relight: false,
        })
        .chain(std::iter::once(crate::tick::TickBlockChange {
            x: 17,
            y: 0,
            z: 1,
            state: default_block_state(Block::Stone),
            needs_relight: false,
        }))
        .collect();
    queue_tick_block_updates(&mut pending, &delivered, changes);
    assert_eq!(pending.len(), 65);
    delivered.insert((1, 0));

    send_pending_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &delivered,
        &mut pending_relights,
        &mut pending,
    )
    .await
    .expect("first bounded update batch");
    assert_eq!(pending.len(), 1);
    for y in 0..64 {
        assert_eq!(
            peer.read_packet().await.expect("ordered block update"),
            Some((43, vec![1, y, 1]))
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(1), peer.read_packet())
            .await
            .is_err(),
        "the next update waits for a separate connection pass"
    );

    send_pending_tick_block_updates(
        &mut conn,
        &RefusingChunkProtocol,
        &mut state,
        &delivered,
        &mut pending_relights,
        &mut pending,
    )
    .await
    .expect("second bounded update batch");
    assert_eq!(pending.len(), 0);
    assert_eq!(
        peer.read_packet().await.expect("final block update"),
        Some((43, vec![1, 64, 1]))
    );
    assert!(pending_relights.is_empty());
}

#[test]
fn tick_relight_targets_keep_the_delivered_cross_column_footprint() {
    let delivered = HashSet::from([
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (0, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
        (2, 0),
    ]);
    assert_eq!(
        tick_relight_targets([(0, 0)], &delivered, 1),
        delivered
            .iter()
            .copied()
            .filter(|&(cx, cz)| {
                (-1..=1).contains(&cx) && (-1..=1).contains(&cz)
            })
            .collect(),
        "a seam or corner edit must queue every delivered column in the 3x3 footprint"
    );
    assert_eq!(
        tick_relight_targets([(0, 0)], &delivered, 0),
        HashSet::from([(0, 0)]),
        "single-column protocols retain the isolated relight footprint"
    );
}

#[test]
fn pending_relights_deduplicate_batches_and_keep_fifo_fairness() {
    let mut pending = PendingRelights::default();
    assert_eq!(
        pending.enqueue_batch([(2, 0), (0, 0), (2, 0)]),
        2,
        "each batch is canonicalized and duplicate targets collapse"
    );
    assert_eq!(pending.enqueue_batch([(1, 0), (0, 0)]), 1);
    assert_eq!(pending.pop_front(), Some((0, 0)));
    assert_eq!(pending.enqueue_batch([(0, 0)]), 1);
    assert_eq!(pending.pop_front(), Some((2, 0)));
    assert_eq!(pending.pop_front(), Some((1, 0)));
    assert_eq!(pending.pop_front(), Some((0, 0)));
    assert!(pending.is_empty());
}

#[test]
fn deferred_relight_batch_rotates_and_retries_individual_outputs() {
    let delivered = HashSet::from([(-1, -1), (0, 0)]);
    let mut pending = PendingRelights::default();
    pending.enqueue_batch([(-1, -1), (0, 0)]);
    let batch = pending.batch(&delivered, true);
    assert_eq!(batch.coordinates, [(-1, -1), (0, 0)]);
    pending.admit(&batch);
    pending.defer(&batch);
    assert!(!pending.ready());
    assert_eq!(pending.front(), Some((0, 0)));
    let retry = pending.batch(&delivered, true);
    assert_eq!(retry.coordinates, [(0, 0)]);
    pending.admit(&retry);
    assert_eq!(pending.front(), Some((-1, -1)));
}

#[test]
fn relight_admission_preserves_new_edits_and_only_requeues_owed_permissions() {
    let delivered = HashSet::from([(0, 0), (1, 0)]);
    let mut pending = PendingRelights::default();
    pending.enqueue_edit(0, 0, 0);
    pending.enqueue_batch([(1, 0)]);
    let batch = pending.batch(&delivered, true);
    pending.admit(&batch);
    pending.enqueue_edit(0, 0, 0);
    pending.defer(&RelightBatch {
        coordinates: vec![(1, 0)],
        generation_required: batch.generation_required,
    });
    assert_eq!(pending.pop_front(), Some((0, 0)));
    assert!(!pending.requires_generation((0, 0)));
    assert_eq!(pending.pop_front(), Some((1, 0)));
    assert!(pending.generation_required.is_empty());
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
async fn direct_block_edit_queues_a_generating_relight_without_reading_columns() {
    let source = ColdColumnSource {
        column_reads: AtomicUsize::new(0),
        store_calls: AtomicUsize::new(0),
        resident: false,
        center_only: false,
    };
    let protocol = RetainedLifecycleProtocol {
        computes: Arc::new(AtomicUsize::new(0)),
        retain_initial_light: true,
        fallback_encodes: Arc::new(AtomicUsize::new(0)),
        dependency_light: None,
    };
    let (_client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut pending = PendingRelights::default();
    resend_column_for_light(
        &mut conn,
        &protocol,
        &source,
        &mut state,
        Some(&mut pending),
        Block::Air.default_state(),
        Block::Air.default_state(),
        BlockPos::new(15, 64, 0),
    )
    .await
    .unwrap();
    assert!(pending.is_empty(), "a light-neutral edit must not queue a relight");

    resend_column_for_light(
        &mut conn,
        &protocol,
        &source,
        &mut state,
        Some(&mut pending),
        Block::Stone.default_state(),
        Block::Air.default_state(),
        BlockPos::new(15, 64, 0),
    )
    .await
    .unwrap();
    assert_eq!(pending.len(), 9);
    assert_eq!(source.column_reads.load(Ordering::Relaxed), 0);
    assert!(pending.requires_generation((0, 0)));
    assert!(pending.requires_generation((1, 1)));
    assert_eq!(pending.pop_front(), Some((-1, -1)));
    assert!(!pending.requires_generation((-1, -1)));
}
