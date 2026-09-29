use std::collections::HashMap;

use super::{Meshed, SectionKey, SectionSnapshot};

#[derive(Debug)]
struct Entry<T> {
    key: SectionKey,
    value: T,
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
}

impl<T> Default for SectionQueue<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            positions: HashMap::new(),
            first: None,
            last: None,
            free: None,
        }
    }
}

impl<T> SectionQueue<T> {
    fn len(&self) -> usize {
        self.positions.len()
    }

    fn push_back(&mut self, key: SectionKey, value: T) {
        if let Some(&index) = self.positions.get(&key) {
            self.entry_mut(index).value = value;
            return;
        }

        let entry = Entry {
            key,
            value,
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
    }

    pub(super) fn pop_front(&mut self) -> Option<T> {
        self.first.map(|index| self.remove_at(index))
    }

    fn remove(&mut self, key: &SectionKey) -> Option<T> {
        let index = *self.positions.get(key)?;
        Some(self.remove_at(index))
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
        self.slots.clear();
        self.positions.clear();
        self.first = None;
        self.last = None;
        self.free = None;
    }

    fn entry_mut(&mut self, index: usize) -> &mut Entry<T> {
        match &mut self.slots[index] {
            Slot::Occupied(entry) => entry,
            Slot::Free(_) => unreachable!("queued section slot is vacant"),
        }
    }

    fn remove_at(&mut self, index: usize) -> T {
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
        entry.value
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
        self.queue.push_back(snapshot.key, snapshot);
    }

    pub(super) fn pending(&self) -> usize {
        self.queue.len() + self.ready.len()
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
        let mut queue = SectionQueue::default();
        for cx in 0..257 {
            queue.push_back(key(cx, 1), cx * 11 + 5);
        }
        for value in 0..4_097 {
            queue.push_back(key(37, 1), value);
        }
        assert_eq!(queue.len(), 257);
        assert_eq!(queue.slots.len(), 257);
        for cx in 0..257 {
            let expected = if cx == 37 { 4_096 } else { cx * 11 + 5 };
            assert_eq!(queue.pop_front(), Some(expected), "section {cx}");
        }
        assert_eq!(queue.pop_front(), None);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn cancellation_reuses_slots_without_moving_survivors() {
        let mut queue = SectionQueue::default();
        for cx in 0..5 {
            queue.push_back(key(cx, 1), cx * 13 + 7);
        }
        assert_eq!(queue.remove(&key(2, 1)), Some(33));
        assert_eq!(queue.remove(&key(0, 1)), Some(7));
        assert_eq!(queue.remove(&key(4, 1)), Some(59));
        assert_eq!(queue.remove(&key(4, 1)), None);
        queue.push_back(key(5, 1), 72);
        queue.push_back(key(6, 1), 85);
        queue.push_back(key(7, 1), 98);
        assert_eq!(queue.slots.len(), 5);
        let actual: Vec<_> = std::iter::from_fn(|| queue.pop_front()).collect();
        assert_eq!(actual, [20, 46, 72, 85, 98]);
        queue.push_back(key(8, 1), 111);
        assert_eq!(queue.slots.len(), 5);
        assert_eq!(queue.pop_front(), Some(111));
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
        assert!(backlog.queue.pop_front().is_none());
        assert_eq!(backlog.ready[0].key, survivor);

        backlog.submit(snapshot(replaced));
        assert_eq!(backlog.pending(), 2);
        backlog.discard_pending();
        assert_eq!(backlog.pending(), 0);
        assert!(backlog.queue.pop_front().is_none());
        backlog.submit(snapshot(survivor));
        assert_eq!(backlog.queue.pop_front().unwrap().key, survivor);
    }
}
