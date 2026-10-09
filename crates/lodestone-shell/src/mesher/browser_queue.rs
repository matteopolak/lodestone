use std::collections::HashMap;
use std::time::Duration;

use super::{
    BiomeNames, BrowserMeshQueueStats, ColumnSource, Meshed, SectionKey, SectionSnapshot,
    SkyDefault, SnapshotOutcome, snapshot_section_in,
};
use crate::platform::Instant;
use super::priority::{FairMeshOrder, MeshPriority};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureSource {
    Column,
    Section,
    Light,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SectionIntent {
    pub(super) key: SectionKey,
    pub(super) section_count: usize,
    pub(super) force: bool,
    pub(super) source: CaptureSource,
    pub(super) priority: MeshPriority,
}

#[derive(Debug)]
pub(super) enum BrowserMeshRequest {
    Snapshot(SectionSnapshot),
    Capture(SectionIntent),
}

impl BrowserMeshRequest {
    pub(super) fn key(&self) -> SectionKey {
        match self {
            Self::Snapshot(snapshot) => snapshot.key,
            Self::Capture(intent) => intent.key,
        }
    }

    pub(super) fn capture(
        self,
        world: &lodestone_world::World,
        sky_default: SkyDefault,
        columns: ColumnSource,
        biome_names: BiomeNames,
    ) -> (SnapshotOutcome, bool, Option<CaptureSource>) {
        match self {
            Self::Snapshot(snapshot) => (SnapshotOutcome::Ready(snapshot), false, None),
            Self::Capture(intent) => (
                snapshot_section_in(
                    world,
                    intent.key,
                    Some(intent.section_count),
                    sky_default,
                    columns,
                    biome_names,
                ),
                intent.force,
                Some(intent.source),
            ),
        }
    }
}

#[derive(Debug)]
struct Entry<T> {
    key: SectionKey,
    value: T,
    submitted_at: Instant,
    priority: MeshPriority,
    previous: Option<usize>,
    next: Option<usize>,
}

#[derive(Debug)]
enum Slot<T> {
    Occupied(Entry<T>),
    Free(Option<usize>),
}

/// Indexed priority bands with one payload per key and reusable vacant slots.
#[derive(Debug)]
pub(super) struct SectionQueue<T> {
    slots: Vec<Slot<T>>,
    positions: HashMap<SectionKey, usize>,
    first: [Option<usize>; 2],
    last: [Option<usize>; 2],
    order: FairMeshOrder,
    free: Option<usize>,
    totals: BrowserMeshQueueStats,
}

impl<T> Default for SectionQueue<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            positions: HashMap::new(),
            first: [None; 2],
            last: [None; 2],
            order: FairMeshOrder::default(),
            free: None,
            totals: BrowserMeshQueueStats::default(),
        }
    }
}

impl<T> SectionQueue<T> {
    fn front(&self) -> Option<&T> {
        Some(&self.entry(self.front_index()?).value)
    }

    fn front_index(&self) -> Option<usize> {
        let preferred = self.order.preferred().index();
        self.first[preferred].or(self.first[1 - preferred])
    }

    fn len(&self) -> usize {
        self.positions.len()
    }

    fn push_back(&mut self, key: SectionKey, value: T, clock: impl FnOnce() -> Instant) {
        self.push_with_priority(key, value, MeshPriority::Background, clock);
    }

    fn push_with_priority(
        &mut self,
        key: SectionKey,
        value: T,
        priority: MeshPriority,
        clock: impl FnOnce() -> Instant,
    ) {
        if let Some(&index) = self.positions.get(&key) {
            self.entry_mut(index).value = value;
            if priority.index() > self.entry(index).priority.index() {
                self.promote(index, priority);
            }
            self.totals.replacements = self.totals.replacements.saturating_add(1);
            return;
        }

        let entry = Entry {
            key,
            value,
            submitted_at: clock(),
            priority,
            previous: self.last[priority.index()],
            next: None,
        };
        let index = if let Some(index) = self.free {
            let Slot::Free(next) = &self.slots[index] else {
                unreachable!("free queue slot is occupied");
            };
            self.free = *next;
            self.slots[index] = Slot::Occupied(entry);
            index
        } else {
            let index = self.slots.len();
            self.slots.push(Slot::Occupied(entry));
            index
        };
        if let Some(last) = self.last[priority.index()] {
            self.entry_mut(last).next = Some(index);
        } else {
            self.first[priority.index()] = Some(index);
        }
        self.last[priority.index()] = Some(index);
        self.positions.insert(key, index);
        self.totals.insertions = self.totals.insertions.saturating_add(1);
        self.totals.high_water_keys = self.totals.high_water_keys.max(self.len());
    }

    pub(super) fn pop_front(&mut self, now: Instant) -> Option<T> {
        let index = self.front_index()?;
        let entry = self.remove_at(index);
        self.order.served(entry.priority);
        self.totals.pops = self.totals.pops.saturating_add(1);
        let wait = now.saturating_duration_since(entry.submitted_at);
        self.totals.max_pop_wait = self.totals.max_pop_wait.max(wait);
        Some(entry.value)
    }

    fn remove(&mut self, key: &SectionKey) -> Option<T> {
        let index = *self.positions.get(key)?;
        self.totals.cancellations = self.totals.cancellations.saturating_add(1);
        Some(self.remove_at(index).value)
    }

    fn remove_column(&mut self, cx: i32, cz: i32) {
        let keys: Vec<_> = self
            .positions
            .keys()
            .filter(|key| key.cx == cx && key.cz == cz)
            .copied()
            .collect();
        for key in keys {
            self.remove(&key);
        }
    }

    fn clear(&mut self) {
        self.totals.cancellations = self.totals.cancellations.saturating_add(self.len() as u64);
        self.slots.clear();
        self.positions.clear();
        self.first = [None; 2];
        self.last = [None; 2];
        self.order = FairMeshOrder::default();
        self.free = None;
    }

    fn stats(&self, clock: impl FnOnce() -> Instant) -> BrowserMeshQueueStats {
        let oldest = self.first.iter().flatten()
            .map(|&index| self.entry(index).submitted_at).min();
        let oldest_wait = oldest.map_or(Duration::ZERO, |submitted_at| {
            clock().saturating_duration_since(submitted_at)
        });
        BrowserMeshQueueStats {
            queued_keys: self.len(),
            oldest_wait,
            ..self.totals
        }
    }

    fn entry_mut(&mut self, index: usize) -> &mut Entry<T> {
        match &mut self.slots[index] {
            Slot::Occupied(entry) => entry,
            Slot::Free(_) => unreachable!("queued section slot is vacant"),
        }
    }

    fn entry(&self, index: usize) -> &Entry<T> {
        match &self.slots[index] {
            Slot::Occupied(entry) => entry,
            Slot::Free(_) => unreachable!("queued section slot is vacant"),
        }
    }

    fn unlink(&mut self, index: usize) {
        let entry = self.entry(index);
        let (previous, next, band) = (entry.previous, entry.next, entry.priority.index());
        if let Some(previous) = previous {
            self.entry_mut(previous).next = next;
        } else {
            self.first[band] = next;
        }
        if let Some(next) = next {
            self.entry_mut(next).previous = previous;
        } else {
            self.last[band] = previous;
        }
    }

    fn promote(&mut self, index: usize, priority: MeshPriority) {
        let submitted_at = self.entry(index).submitted_at;
        self.unlink(index);
        let band = priority.index();
        let mut previous = self.last[band];
        // Preserve first-submission age order within the edit band.
        while let Some(candidate) = previous {
            let entry = self.entry(candidate);
            if entry.submitted_at <= submitted_at {
                break;
            }
            previous = entry.previous;
        }
        let next = previous.map_or(self.first[band], |previous| self.entry(previous).next);
        let entry = self.entry_mut(index);
        entry.priority = priority;
        entry.previous = previous;
        entry.next = next;
        if let Some(previous) = previous {
            self.entry_mut(previous).next = Some(index);
        } else {
            self.first[band] = Some(index);
        }
        if let Some(next) = next {
            self.entry_mut(next).previous = Some(index);
        } else {
            self.last[band] = Some(index);
        }
    }

    fn remove_at(&mut self, index: usize) -> Entry<T> {
        self.unlink(index);
        let Slot::Occupied(entry) =
            std::mem::replace(&mut self.slots[index], Slot::Free(self.free))
        else {
            unreachable!("queued section slot is vacant");
        };
        self.free = Some(index);
        self.positions.remove(&entry.key);
        entry
    }
}

#[derive(Debug, Default)]
pub(super) struct BrowserMeshBacklog {
    pub(super) queue: SectionQueue<BrowserMeshRequest>,
    pub(super) ready: Vec<Meshed>,
}

impl BrowserMeshBacklog {
    pub(super) fn submit(&mut self, snapshot: SectionSnapshot) {
        self.ready.retain(|meshed| meshed.key != snapshot.key);
        self.queue.push_back(
            snapshot.key, BrowserMeshRequest::Snapshot(snapshot), Instant::now,
        );
    }

    pub(super) fn submit_intent(&mut self, mut intent: SectionIntent) {
        if let Some(&index) = self.queue.positions.get(&intent.key)
            && let BrowserMeshRequest::Capture(previous) = &self.queue.entry_mut(index).value
        {
            intent.force |= previous.force;
            if previous.priority.index() > intent.priority.index() {
                intent.priority = previous.priority;
            }
            if previous.source == CaptureSource::Column {
                intent.source = CaptureSource::Column;
            }
        }
        self.ready.retain(|meshed| meshed.key != intent.key);
        self.queue.push_with_priority(
            intent.key, BrowserMeshRequest::Capture(intent), intent.priority, Instant::now,
        );
    }

    pub(super) fn pop_snapshot(&mut self, now: Instant) -> Option<SectionSnapshot> {
        if !matches!(self.queue.front(), Some(BrowserMeshRequest::Snapshot(_))) {
            return None;
        }
        let BrowserMeshRequest::Snapshot(snapshot) = self.queue.pop_front(now)? else {
            unreachable!("front request changed while popping");
        };
        Some(snapshot)
    }

    pub(super) fn pending(&self) -> usize {
        self.queue.len() + self.ready.len()
    }

    pub(super) fn stats(&self) -> BrowserMeshQueueStats {
        self.queue.stats(Instant::now)
    }

    pub(super) fn forget_generation(&mut self, key: &SectionKey) {
        self.queue.remove(key);
        self.ready.retain(|meshed| meshed.key != *key);
    }

    pub(super) fn forget_column(&mut self, cx: i32, cz: i32) {
        self.queue.remove_column(cx, cz);
        self.ready.retain(|meshed| meshed.key.cx != cx || meshed.key.cz != cz);
    }

    pub(super) fn discard_pending(&mut self) {
        self.queue.clear();
        self.ready.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::{DemoClassifier, ShellClassifier, id};
    use crate::mesher::{MeshScheduler, Neighbour, SectionGeometry, SkyDefault, TerrainMesh};
    use lodestone_render::Mesh;
    use lodestone_world::{
        ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
    };
    use std::sync::Arc;

    fn key(cx: i32, si: usize) -> SectionKey {
        SectionKey {
            cx,
            cz: -3,
            si,
            min_y: -64,
        }
    }

    fn snapshot(key: SectionKey) -> SectionSnapshot {
        SectionSnapshot {
            key,
            sections: vec![Neighbour::Air; 27],
            lights: vec![None; 27],
            sky_default: SkyDefault::Full,
            biome_names: Arc::from([]),
            light_revision: None,
        }
    }

    fn meshed(key: SectionKey) -> Meshed {
        Meshed::new(key, SectionGeometry::Packed(Mesh::default()))
    }

    #[test]
    fn replacement_bursts_keep_fifo_and_one_slot_per_section() {
        let now = Instant::now();
        let mut queue = SectionQueue::default();
        for cx in 0..257 {
            queue.push_back(key(cx, 1), cx * 11 + 5, || now);
        }
        for value in 0..4_097 {
            queue.push_back(key(37, 1), value, || now);
        }
        assert_eq!(queue.len(), 257);
        assert_eq!(queue.slots.len(), 257);
        for cx in 0..257 {
            let expected = if cx == 37 { 4_096 } else { cx * 11 + 5 };
            assert_eq!(queue.pop_front(now), Some(expected), "section {cx}");
        }
        assert_eq!(queue.pop_front(now), None);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn edits_bypass_background_with_a_failing_fifo_control() {
        let now = Instant::now();
        let build = |edit_priority| {
            let mut queue = SectionQueue::default();
            queue.push_back(key(0, 1), 17, || now);
            queue.push_back(key(1, 1), 29, || now);
            queue.push_with_priority(key(2, 1), 43, edit_priority, || now);
            std::iter::from_fn(|| queue.pop_front(now)).collect::<Vec<_>>()
        };
        let expected = [43, 17, 29];
        assert!(std::panic::catch_unwind(|| {
            assert_eq!(build(MeshPriority::Background), expected);
        }).is_err());
        assert_eq!(build(MeshPriority::Edit), expected);
    }

    #[test]
    fn edit_bursts_leave_background_progress_and_cancellation_reuses_slots() {
        let now = Instant::now();
        let mut queue = SectionQueue::default();
        for cx in 0..3 {
            queue.push_back(key(cx, 1), cx, || now);
        }
        for cx in 10..18 {
            queue.push_with_priority(key(cx, 1), cx, MeshPriority::Edit, || now);
        }
        let actual: Vec<_> = std::iter::from_fn(|| queue.pop_front(now)).collect();
        assert_eq!(actual, [10, 11, 12, 13, 0, 14, 15, 16, 17, 1, 2]);
        queue.push_with_priority(key(20, 1), 20, MeshPriority::Edit, || now);
        assert_eq!(queue.remove(&key(20, 1)), Some(20));
        queue.push_back(key(21, 1), 21, || now);
        assert_eq!(queue.slots.len(), 11);
        assert_eq!(queue.pop_front(now), Some(21));
        queue.clear();
        assert!(queue.front().is_none());
    }

    #[test]
    fn promotion_retains_first_age_and_later_light_cannot_downgrade_edit() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut queue = SectionQueue::default();
        queue.push_back(key(0, 1), 17, || at(2));
        queue.push_back(key(1, 1), 29, || at(3));
        queue.push_with_priority(key(2, 1), 43, MeshPriority::Edit, || at(5));
        queue.push_with_priority(key(1, 1), 53, MeshPriority::Edit, || {
            panic!("promotion reset first submission age")
        });
        queue.push_back(key(1, 1), 61, || panic!("replacement read the clock"));
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.slots.len(), 3);
        assert_eq!(queue.stats(|| at(10)).oldest_wait, Duration::from_millis(8));
        assert_eq!(queue.pop_front(at(10)), Some(61));
        assert_eq!(queue.pop_front(at(10)), Some(43));
        assert_eq!(queue.pop_front(at(10)), Some(17));
        queue.push_back(key(0, 1), 71, || at(12));
        queue.push_back(key(1, 1), 83, || at(13));
        assert_eq!(queue.pop_front(at(14)), Some(71));
        assert_eq!(queue.pop_front(at(14)), Some(83));
    }

    #[test]
    fn cancellation_reuses_slots_without_moving_survivors() {
        let now = Instant::now();
        let mut queue = SectionQueue::default();
        for cx in 0..5 {
            queue.push_back(key(cx, 1), cx * 13 + 7, || now);
        }
        assert_eq!(queue.remove(&key(2, 1)), Some(33));
        assert_eq!(queue.remove(&key(0, 1)), Some(7));
        assert_eq!(queue.remove(&key(4, 1)), Some(59));
        assert_eq!(queue.remove(&key(4, 1)), None);
        queue.push_back(key(5, 1), 72, || now);
        queue.push_back(key(6, 1), 85, || now);
        queue.push_back(key(7, 1), 98, || now);
        assert_eq!(queue.slots.len(), 5);
        let actual: Vec<_> = std::iter::from_fn(|| queue.pop_front(now)).collect();
        assert_eq!(actual, [20, 46, 72, 85, 98]);
        queue.push_back(key(8, 1), 111, || now);
        assert_eq!(queue.slots.len(), 5);
        assert_eq!(queue.pop_front(now), Some(111));
    }

    #[test]
    fn queue_counters_include_pre_drain_peaks_and_only_real_operations() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut queue = SectionQueue::default();
        assert_eq!(
            queue.stats(|| panic!("empty queue read the clock")),
            BrowserMeshQueueStats::default(),
        );
        queue.push_back(key(1, 0), 17, || at(3));
        queue.push_back(key(2, 0), 23, || at(5));
        queue.push_back(key(3, 0), 31, || at(7));
        queue.push_back(key(2, 0), 41, || at(11));
        assert_eq!(queue.stats(|| at(13)).queued_keys, 3);
        assert_eq!(queue.stats(|| at(13)).high_water_keys, 3);
        assert_eq!(queue.remove(&key(4, 0)), None);
        assert_eq!(queue.remove(&key(2, 0)), Some(41));
        assert_eq!(queue.pop_front(at(19)), Some(17));
        queue.clear();
        assert_eq!(queue.pop_front(at(23)), None);
        queue.clear();

        assert_eq!(
            queue.stats(|| at(29)),
            BrowserMeshQueueStats {
                insertions: 3,
                replacements: 1,
                cancellations: 2,
                pops: 1,
                queued_keys: 0,
                high_water_keys: 3,
                oldest_wait: Duration::ZERO,
                max_pop_wait: Duration::from_millis(16),
            },
        );
    }

    #[test]
    fn wait_age_tracks_first_submission_and_resets_after_cancellation() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut queue = SectionQueue::default();
        let mut clock_reads = 0;
        queue.push_back(key(1, 0), 17, || {
            clock_reads += 1;
            at(4)
        });
        assert_eq!(clock_reads, 1);
        queue.push_back(key(2, 0), 23, || at(9));
        queue.push_back(key(1, 0), 31, || panic!("replacement read the clock"));
        assert_eq!(queue.stats(|| at(13)).oldest_wait, Duration::from_millis(9));
        assert_eq!(queue.remove(&key(1, 0)), Some(31));
        assert_eq!(queue.stats(|| at(13)).oldest_wait, Duration::from_millis(4));
        queue.push_back(key(1, 0), 41, || at(14));
        assert_eq!(queue.pop_front(at(17)), Some(23));
        assert_eq!(queue.stats(|| at(17)).oldest_wait, Duration::from_millis(3));
        assert_eq!(queue.pop_front(at(21)), Some(41));
        assert_eq!(queue.stats(|| at(21)).oldest_wait, Duration::ZERO);
        assert_eq!(queue.stats(|| at(21)).max_pop_wait, Duration::from_millis(8));

        queue.push_back(key(1, 0), 47, || at(30));
        assert_eq!(queue.stats(|| at(29)).oldest_wait, Duration::ZERO);
        assert_eq!(queue.stats(|| at(35)).oldest_wait, Duration::from_millis(5));
        assert_eq!(queue.pop_front(at(35)), Some(47));
        assert_eq!(queue.stats(|| at(35)).max_pop_wait, Duration::from_millis(8));
    }

    #[test]
    fn browser_invalidations_remove_queued_completed_and_never_uploaded_work() {
        let replaced = key(2, 0);
        let never_uploaded = key(2, 1);
        let survivor = key(3, 0);
        let retained = Arc::new(crate::mesher::air_section());
        let mut backlog = BrowserMeshBacklog::default();
        backlog.ready.extend([meshed(replaced), meshed(survivor)]);
        let mut waiting = snapshot(never_uploaded);
        waiting.sections[13] = Neighbour::Present(Arc::clone(&retained));
        backlog.submit(waiting);
        let mut previous = snapshot(replaced);
        previous.sections[13] = Neighbour::Present(Arc::clone(&retained));
        backlog.submit(previous);
        assert_eq!(Arc::strong_count(&retained), 3);
        backlog.submit(snapshot(replaced));
        assert_eq!(Arc::strong_count(&retained), 2);
        assert_eq!(backlog.pending(), 3);
        assert_eq!(backlog.stats().queued_keys, 2);
        assert_eq!(
            backlog.ready.iter().map(|mesh| mesh.key).collect::<Vec<_>>(),
            [survivor],
        );

        backlog.ready.push(meshed(replaced));
        assert_eq!(backlog.pending(), 4);
        backlog.forget_generation(&replaced);
        assert_eq!(backlog.pending(), 2);
        backlog.submit(snapshot(replaced));
        backlog.ready.push(meshed(replaced));
        assert_eq!(backlog.pending(), 4);
        backlog.forget_column(2, -3);
        assert_eq!(backlog.pending(), 1);
        assert_eq!(Arc::strong_count(&retained), 1);
        assert!(backlog.queue.pop_front(Instant::now()).is_none());
        assert_eq!(backlog.ready[0].key, survivor);

        backlog.submit(snapshot(replaced));
        assert_eq!(backlog.pending(), 2);
        backlog.discard_pending();
        assert_eq!(backlog.pending(), 0);
        assert!(backlog.queue.pop_front(Instant::now()).is_none());
        backlog.submit(snapshot(survivor));
        assert_eq!(backlog.pop_snapshot(Instant::now()).unwrap().key, survivor);
    }

    fn intent_fixture() -> (World, SectionIntent) {
        let mut world = World::new();
        let mut column = ChunkColumn::new(
            0, 1, PaletteKind::block_states(), PaletteKind::biomes(), id::AIR, 0,
        );
        column.set_block(2, 7, 8, id::STONE);
        world.load(
            ChunkPos::new(0, 0),
            LoadedChunk::new(column, ColumnLight::new(1), Heightmaps::new(), Vec::new()),
        );
        let intent = SectionIntent {
            key: SectionKey { cx: 0, cz: 0, si: 0, min_y: 0 },
            section_count: 1,
            force: false,
            source: CaptureSource::Column,
            priority: MeshPriority::Background,
        };
        (world, intent)
    }

    #[test]
    fn coalesced_intents_capture_latest_world_once_instead_of_retaining_old_sections() {
        let (mut world, intent) = intent_fixture();
        let old = snapshot_section_in(
            &world, intent.key, Some(1), SkyDefault::Full, ColumnSource::Complete,
            Default::default(),
        ).ready().unwrap();
        let mut backlog = BrowserMeshBacklog::default();
        backlog.submit_intent(intent);
        world.set_block(5, 7, 8, id::STONE);
        backlog.submit_intent(SectionIntent {
            source: CaptureSource::Section, priority: MeshPriority::Edit, ..intent
        });
        world.set_block(8, 7, 8, id::STONE);
        backlog.submit_intent(SectionIntent { source: CaptureSource::Light, ..intent });

        assert_eq!(old.quad_count(&DemoClassifier), 6);
        assert_eq!(backlog.pending(), 1);
        assert_eq!(backlog.queue.slots.len(), 1);
        assert!(backlog.pop_snapshot(Instant::now()).is_none());
        assert_eq!(backlog.stats().pops, 0);
        assert_eq!(backlog.queue.entry(0).priority, MeshPriority::Edit);
        let request = backlog.queue.pop_front(Instant::now()).unwrap();
        assert_eq!(request.key(), intent.key);
        let (outcome, force, source) = request.capture(
            &world, SkyDefault::Full, ColumnSource::Complete, Arc::from([]),
        );
        assert!(!force);
        assert_eq!(source, Some(CaptureSource::Column));
        assert_eq!(outcome.ready().unwrap().quad_count(&DemoClassifier), 18);
        assert_eq!(backlog.stats().insertions, 1);
        assert_eq!(backlog.stats().replacements, 2);
        assert_eq!(backlog.stats().pops, 1);
        assert_eq!(backlog.pending(), 0);
    }

    #[test]
    fn failed_upload_retry_uses_gpu_residency_not_reset_readiness() {
        use lodestone_ecs::ChunkWorldWrite;

        for had_resident in [false, true] {
            let (world, intent) = intent_fixture();
            let write = ChunkWorldWrite::new(world);
            let store = write.read_handle();
            let mut terrain = TerrainMesh::new(MeshScheduler::new(
                1, ShellClassifier::Demo(DemoClassifier),
            ));
            terrain.column_source = ColumnSource::Streaming;
            terrain.mark_mesh_uploaded(intent.key);
            terrain.uploaded_sections.insert(intent.key);
            terrain.reset_column_readiness(0, 0);
            assert!(!terrain.presented_sections.contains(&intent.key));

            terrain.retry_mesh_upload(&store, intent.key, had_resident);
            assert_eq!(terrain.uploaded_sections.contains(&intent.key), had_resident);
            assert!(!terrain.column_mesh_settled(&store, 0, 0));
            let meshes = terrain.drain_all_meshes_with_world(&store);
            assert_eq!(meshes.len(), usize::from(had_resident));
            if had_resident {
                assert_eq!(meshes[0].mesh.quad_count(), 6);
                assert!(!terrain.column_mesh_settled(&store, 0, 0));
                terrain.mark_mesh_uploaded(meshes[0].key);
                assert!(terrain.column_mesh_settled(&store, 0, 0));
            }
        }
    }

    #[test]
    fn late_capture_preserves_deferred_admission_and_cancels_unloaded_or_rearriving_work() {
        let (mut world, intent) = intent_fixture();
        let mut terrain = TerrainMesh::new(MeshScheduler::new(
            1, ShellClassifier::Demo(DemoClassifier),
        ));
        let mut backlog = BrowserMeshBacklog::default();
        let capture = |request: BrowserMeshRequest, world: &World| {
            request.capture(world, SkyDefault::Full, ColumnSource::Streaming, Arc::from([]))
        };
        backlog.submit_intent(intent);
        let (outcome, force, _) = capture(backlog.queue.pop_front(Instant::now()).unwrap(), &world);
        assert!(matches!(
            &outcome,
            SnapshotOutcome::Deferred(snapshot) if snapshot.unloaded_neighbours() == 24,
        ));
        assert!(terrain.accept_snapshot(intent.key, outcome, force).is_none());
        assert_eq!(terrain.deferred, 1);

        backlog.submit_intent(SectionIntent { force: true, ..intent });
        backlog.submit_intent(intent);
        let (outcome, force, _) = capture(backlog.queue.pop_front(Instant::now()).unwrap(), &world);
        assert!(force);
        assert_eq!(
            terrain.accept_snapshot(intent.key, outcome, force).unwrap().quad_count(&DemoClassifier),
            6,
        );

        terrain.uploaded_sections.insert(intent.key);
        backlog.submit_intent(intent);
        let (outcome, force, _) = capture(backlog.queue.pop_front(Instant::now()).unwrap(), &world);
        assert!(!force);
        assert!(terrain.accept_snapshot(intent.key, outcome, force).is_some());

        backlog.submit_intent(intent);
        backlog.ready.push(meshed(intent.key));
        let survivor = SectionIntent { key: SectionKey { cx: 3, ..intent.key }, ..intent };
        backlog.submit_intent(survivor);
        world.unload(ChunkPos::new(0, 0));
        backlog.forget_column(0, 0);
        assert_eq!(backlog.pending(), 1);
        assert!(backlog.ready.is_empty());
        assert_eq!(backlog.queue.pop_front(Instant::now()).unwrap().key(), survivor.key);

        backlog.submit_intent(intent);
        let (outcome, force, _) = capture(backlog.queue.pop_front(Instant::now()).unwrap(), &world);
        assert!(matches!(&outcome, SnapshotOutcome::Empty));
        assert!(terrain.accept_snapshot(intent.key, outcome, force).is_none());
        assert_eq!(terrain.pending_removals, [intent.key]);
        backlog.submit_intent(intent);
        backlog.ready.push(meshed(intent.key));
        backlog.forget_column(0, 0);
        let (new_world, _) = intent_fixture();
        world = new_world;
        assert_eq!(backlog.pending(), 0);
        backlog.submit_intent(intent);
        let (outcome, _, _) = capture(backlog.queue.pop_front(Instant::now()).unwrap(), &world);
        assert!(matches!(outcome, SnapshotOutcome::Deferred(_)));
    }
}
