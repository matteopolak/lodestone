use super::*;
use crate::blocks::{DemoClassifier, id};

struct WorkerRelease(Option<crossbeam_channel::Sender<()>>);

impl WorkerRelease {
    fn release(mut self) {
        self.0.take().unwrap().send(()).unwrap();
    }
}

impl Drop for WorkerRelease {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.try_send(());
        }
    }
}

fn isolated_cubes(count: usize) -> SectionSnapshot {
    let mut column = ChunkColumn::new(
        0,
        1,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        id::AIR,
        0,
    );
    for x in [2, 6, 10].into_iter().take(count) {
        column.set_block(x, 5, 8, id::STONE);
    }
    let mut world = World::new();
    world.load(
        ChunkPos::new(0, 0),
        lodestone_world::LoadedChunk::new(
            column,
            lodestone_world::ColumnLight::new(1),
            lodestone_world::Heightmaps::new(),
            Vec::new(),
        ),
    );
    snapshot_section(&world, SectionKey { cx: 0, cz: 0, si: 0, min_y: 0 }, Default::default()).unwrap()
}

#[test]
fn held_native_worker_skips_superseded_jobs_before_meshing() {
    for ignore_cancellation in [true, false] {
        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let mut scheduler = MeshScheduler::new_inner(
            1,
            ShellClassifier::Demo(DemoClassifier),
            Some(NativeWorkerGate {
                entered: entered_tx,
                release: release_rx,
                ignore_cancellation,
            }),
        );
        let release = WorkerRelease(Some(release_tx));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        for count in 1..=3 {
            scheduler.submit_current(isolated_cubes(count));
        }
        assert_eq!(scheduler.pending(), 3);
        assert_eq!(scheduler.native_work_counters(), NativeMeshWorkCounters {
            submitted: 3,
            ..NativeMeshWorkCounters::default()
        });

        release.release();
        let results = scheduler.drain_blocking(3);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].mesh.quad_count(), 3 * 6);
        assert_eq!(scheduler.pending(), 0);
        assert_eq!(scheduler.native_work_counters(), NativeMeshWorkCounters {
            submitted: 3,
            started: if ignore_cancellation { 3 } else { 1 },
            skipped_before_mesh: if ignore_cancellation { 0 } else { 2 },
            stale_results_discarded: if ignore_cancellation { 2 } else { 0 },
        });
    }
}
