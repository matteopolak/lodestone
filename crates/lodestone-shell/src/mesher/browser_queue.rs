use std::collections::HashMap;
use std::time::Duration;

use super::{BrowserMeshQueueStats, Meshed, SectionKey, SectionSnapshot};
use crate::platform::Instant;

#[derive(Debug)]
struct Entry<T> {
    key: SectionKey,
    value: T,
    submitted_at: Instant,
    previous: Option<usize>,
    next: Option<usize>,
}

#[derive(Debug)]
enum Slot<T> {
    Occupied(Entry<T>),
    Free(Option<usize>),
}

/// An indexed FIFO with one payload per key and reusable vacant slots.
#[derive(Debug)]
pub(super) struct SectionQueue<T> {
    slots: Vec<Slot<T>>,
    positions: HashMap<SectionKey, usize>,
    first: Option<usize>,
    last: Option<usize>,
    free: Option<usize>,
    totals: BrowserMeshQueueStats,
}

impl<T> Default for SectionQueue<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            positions: HashMap::new(),
            first: None,
            last: None,
            free: None,
            totals: BrowserMeshQueueStats::default(),
        }
    }
}

impl<T> SectionQueue<T> {
    fn len(&self) -> usize {
        self.positions.len()
    }

    fn push_back(&mut self, key: SectionKey, value: T, clock: impl FnOnce() -> Instant) {
        if let Some(&index) = self.positions.get(&key) {
            self.entry_mut(index).value = value;
            self.totals.replacements = self.totals.replacements.saturating_add(1);
            return;
        }

        let entry = Entry {
            key,
            value,
            submitted_at: clock(),
            previous: self.last,
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
        if let Some(last) = self.last {
            self.entry_mut(last).next = Some(index);
        } else {
            self.first = Some(index);
        }
        self.last = Some(index);
        self.positions.insert(key, index);
        self.totals.insertions = self.totals.insertions.saturating_add(1);
        self.totals.high_water_keys = self.totals.high_water_keys.max(self.len());
    }

    pub(super) fn pop_front(&mut self, now: Instant) -> Option<T> {
        let index = self.first?;
        let entry = self.remove_at(index);
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
        self.first = None;
        self.last = None;
        self.free = None;
    }

    fn stats(&self, clock: impl FnOnce() -> Instant) -> BrowserMeshQueueStats {
        let oldest_wait = self.first.map_or(Duration::ZERO, |index| {
            let Slot::Occupied(entry) = &self.slots[index] else {
                unreachable!("queued section slot is vacant");
            };
            clock().saturating_duration_since(entry.submitted_at)
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

    fn remove_at(&mut self, index: usize) -> Entry<T> {
        let Slot::Occupied(entry) =
            std::mem::replace(&mut self.slots[index], Slot::Free(self.free))
        else {
            unreachable!("queued section slot is vacant");
        };
        self.free = Some(index);
        if let Some(previous) = entry.previous {
            self.entry_mut(previous).next = entry.next;
        } else {
            self.first = entry.next;
        }
        if let Some(next) = entry.next {
            self.entry_mut(next).previous = entry.previous;
        } else {
            self.last = entry.previous;
        }
        self.positions.remove(&entry.key);
        entry
    }
}

#[derive(Debug, Default)]
pub(super) struct BrowserMeshBacklog {
    pub(super) queue: SectionQueue<SectionSnapshot>,
    pub(super) ready: Vec<Meshed>,
}

impl BrowserMeshBacklog {
    pub(super) fn submit(&mut self, snapshot: SectionSnapshot) {
        self.ready.retain(|meshed| meshed.key != snapshot.key);
        self.queue.push_back(snapshot.key, snapshot, Instant::now);
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
    use crate::mesher::{Neighbour, SectionGeometry, SkyDefault};
    use lodestone_render::Mesh;
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
        assert_eq!(backlog.queue.pop_front(Instant::now()).unwrap().key, survivor);
    }
}
