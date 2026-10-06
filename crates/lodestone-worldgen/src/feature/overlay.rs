//! The write medium structure feature placement writes into: a sparse [`Overlay`] of
//! the cells a pass wrote, and the [`WriteLog`] of their positions in write
//! order.
//!
//! # How it works
//!
//! [`Overlay`] is a bounded direct-address page directory keyed by local
//! `(x, y, z)`. Each page cell carries a generation stamp, so a recycled
//! overlay ignores stale cells instead of clearing every page. [`WriteLog`] is
//! a plain `Vec`, so a caller replaying the writes gets a deterministic order.
//!
//! Both are taken from and returned to a per-thread free-list, bounded at a
//! few buffers per shape. A buffer that migrates threads (taken on one,
//! dropped on another) is still correct, merely relocated.
//!
//! # How to change it
//!
//! * The overlay is not iteration-ordered. Anything that must export writes
//!   in order reads the [`WriteLog`], never the overlay.
//! * Take-and-return, never borrow across a body: both types own their buffer
//!   and hand it back in `Drop`, so a nested construction gets a fresh buffer
//!   instead of a `RefCell` panic.
//! * Do not replace the free-list with a pool behind a lock. Structure placement
//!   runs on every Rayon worker at once, and a shared lock puts them all on one cache
//!   line.

pub(crate) use scratch::{Overlay, WriteLog};

mod scratch {
    use std::cell::RefCell;

    use lodestone_data::block_states::StateId;

    /// The overlay's key: `VegGrid`-local `(lx, y, lz)`, never absolute.
    type Key = (i32, i32, i32);

    /// How many buffers of each shape one thread keeps. Steady state needs one
    /// per shape per simultaneously-live medium (ore's view and vegetation's grid
    /// are sequential within a column, so one); the rest of the headroom exists so
    /// a re-entrant or nested construction does not permanently lose its buffer.
    ///
    /// A bound rather than an unbounded `Vec` because this is thread-local storage
    /// that lives as long as the thread: an unbounded free-list is a leak wearing
    /// a cache's clothes.
    const KEEP: usize = 4;

    const PAGE_X: usize = 4;
    const PAGE_Z: usize = 4;
    const PAGE_Y: usize = 16;
    const PAGE_X_SHIFT: u32 = PAGE_X.trailing_zeros();
    const PAGE_Z_SHIFT: u32 = PAGE_Z.trailing_zeros();
    const PAGE_Y_SHIFT: u32 = PAGE_Y.trailing_zeros();
    const PAGE_X_MASK: usize = PAGE_X - 1;
    const PAGE_Z_MASK: usize = PAGE_Z - 1;
    const PAGE_Y_MASK: usize = PAGE_Y - 1;
    const XZ_BITS: u32 = 10;
    const Y_BITS: u32 = 12;
    const XZ_MASK: u32 = (1 << XZ_BITS) - 1;
    const Y_MASK: u32 = (1 << Y_BITS) - 1;
    const DEFAULT_MIN_XZ: i32 = -32;
    const DEFAULT_MAX_XZ: i32 = 48;
    /// The default covers the widest vegetation footprint (`-32..48`) and all
    /// supported generator heights.
    const DEFAULT_MIN_Y: i32 = -64;
    const DEFAULT_HEIGHT: i32 = 448;

    thread_local! {
        static OVERLAYS: RefCell<Vec<Storage>> = const { RefCell::new(Vec::new()) };
        static LOGS: RefCell<Vec<Vec<Key>>> = const { RefCell::new(Vec::new()) };
    }

    #[derive(Clone, Copy, Debug)]
    struct EntryCell {
        generation: u32,
        state: StateId,
    }

    #[derive(Debug)]
    struct Page {
        cells: [EntryCell; PAGE_X * PAGE_Y * PAGE_Z],
    }

    impl Page {
        fn new() -> Self {
            Self {
                cells: [EntryCell { generation: 0, state: StateId::AIR };
                    PAGE_X * PAGE_Y * PAGE_Z],
            }
        }
    }

    /// A direct-address directory of lazily allocated pages. The directory is
    /// bounded by the view's coordinate window; a cell's packed local offset
    /// addresses one page and one slot without hashing. Pages and cells stay in
    /// the per-thread scratch free-list after a view drops. On reuse the
    /// generation advances and stale cells are ignored without clearing every
    /// page.
    #[derive(Debug)]
    struct Storage {
        min_x: i32,
        max_x: i32,
        min_y: i32,
        max_y: i32,
        min_z: i32,
        max_z: i32,
        x_pages: usize,
        z_pages: usize,
        y_pages: usize,
        generation: u32,
        pages: Vec<Option<Box<Page>>>,
        keys: Vec<u32>,
    }

    impl Storage {
        fn new(min_x: i32, max_x: i32, min_y: i32, height: i32, min_z: i32, max_z: i32) -> Self {
            let storage = Self {
                min_x,
                max_x,
                min_y,
                max_y: min_y + height,
                min_z,
                max_z,
                x_pages: 0,
                z_pages: 0,
                y_pages: 0,
                generation: 0,
                pages: Vec::new(),
                keys: Vec::new(),
            };
            storage
        }

        fn reconfigure(
            &mut self,
            min_x: i32,
            max_x: i32,
            min_y: i32,
            height: i32,
            min_z: i32,
            max_z: i32,
        ) {
            assert!(max_x > min_x && max_z > min_z && height > 0);
            assert!(max_x - min_x <= (1 << XZ_BITS));
            assert!(max_z - min_z <= (1 << XZ_BITS));
            assert!(height <= (1 << Y_BITS));
            let x_pages = ((max_x - min_x) as usize).div_ceil(PAGE_X);
            let z_pages = ((max_z - min_z) as usize).div_ceil(PAGE_Z);
            let y_pages = (height as usize).div_ceil(PAGE_Y);
            if self.x_pages != x_pages || self.z_pages != z_pages || self.y_pages != y_pages {
                self.pages.clear();
                self.pages.resize_with(x_pages * z_pages * y_pages, || None);
                self.x_pages = x_pages;
                self.z_pages = z_pages;
                self.y_pages = y_pages;
            }
            self.min_x = min_x;
            self.max_x = max_x;
            self.min_y = min_y;
            self.max_y = min_y + height;
            self.min_z = min_z;
            self.max_z = max_z;
            let next_generation = self.generation.wrapping_add(1);
            if next_generation == 0 {
                for page in self.pages.iter_mut().flatten() {
                    for cell in page.cells.iter_mut() {
                        cell.generation = 0;
                    }
                }
                self.generation = 1;
            } else {
                self.generation = next_generation;
            }
            self.keys.clear();
        }

        fn pack(&self, &(lx, y, lz): &Key) -> Option<u32> {
            if !(self.min_x..self.max_x).contains(&lx)
                || !(self.min_y..self.max_y).contains(&y)
                || !(self.min_z..self.max_z).contains(&lz)
            {
                return None;
            }
            Some(self.pack_unchecked(&(lx, y, lz)))
        }

        #[inline]
        fn pack_unchecked(&self, &(lx, y, lz): &Key) -> u32 {
            let x = (lx - self.min_x) as u32;
            let y = (y - self.min_y) as u32;
            let z = (lz - self.min_z) as u32;
            x | (z << XZ_BITS) | (y << (XZ_BITS * 2))
        }

        fn location(&self, packed: u32) -> (usize, usize) {
            let x = (packed & XZ_MASK) as usize;
            let z = ((packed >> XZ_BITS) & XZ_MASK) as usize;
            let y = ((packed >> (XZ_BITS * 2)) & Y_MASK) as usize;
            let page = ((x >> PAGE_X_SHIFT) * self.z_pages + (z >> PAGE_Z_SHIFT))
                * self.y_pages
                + (y >> PAGE_Y_SHIFT);
            let cell = (y & PAGE_Y_MASK) * PAGE_X * PAGE_Z
                + (z & PAGE_Z_MASK) * PAGE_X
                + (x & PAGE_X_MASK);
            (page, cell)
        }

        fn get_packed(&self, packed: u32) -> Option<StateId> {
            let (page, cell) = self.location(packed);
            let page = self.pages.get(page)?.as_ref()?;
            let cell = &page.cells[cell];
            (cell.generation == self.generation).then_some(cell.state)
        }

        #[inline]
        fn insert_in_bounds(&mut self, key: Key, state: StateId) {
            debug_assert!(
                self.pack(&key).is_some(),
                "overlay key is outside its configured bounds"
            );
            self.insert_packed(self.pack_unchecked(&key), state);
        }

        fn insert_packed(&mut self, packed: u32, state: StateId) {
            let (page_index, cell_index) = self.location(packed);
            let page = self.pages[page_index].get_or_insert_with(|| Box::new(Page::new()));
            let cell = &mut page.cells[cell_index];
            let is_new = cell.generation != self.generation;
            cell.generation = self.generation;
            cell.state = state;
            if is_new {
                self.keys.push(packed);
            }
        }
    }

    /// The overlay owns one storage instance. It is deliberately not `Sync` or
    /// shared: a view's direct pages belong to the thread running that view.
    #[derive(Debug)]
    pub(crate) struct Overlay {
        storage: Option<Storage>,
    }

    impl Default for Overlay {
        fn default() -> Self {
            Self::with_bounds(
                DEFAULT_MIN_XZ,
                DEFAULT_MAX_XZ,
                DEFAULT_MIN_Y,
                DEFAULT_HEIGHT,
            )
        }
    }

    impl Overlay {
        pub(crate) fn with_bounds(min_x: i32, max_x: i32, min_y: i32, height: i32) -> Self {
            Self::with_bounds_xyz(min_x, max_x, min_y, height, min_x, max_x)
        }

        pub(crate) fn with_bounds_xyz(
            min_x: i32,
            max_x: i32,
            min_y: i32,
            height: i32,
            min_z: i32,
            max_z: i32,
        ) -> Self {
            let recycled = OVERLAYS
                .try_with(|free| free.try_borrow_mut().ok().and_then(|mut f| f.pop()))
                .ok()
                .flatten();
            let mut storage = recycled.unwrap_or_else(|| {
                Storage::new(min_x, max_x, min_y, height, min_z, max_z)
            });
            storage.reconfigure(min_x, max_x, min_y, height, min_z, max_z);
            Self { storage: Some(storage) }
        }

        fn storage(&self) -> &Storage {
            self.storage.as_ref().expect("overlay storage is only taken in Drop")
        }

        fn storage_mut(&mut self) -> &mut Storage {
            self.storage.as_mut().expect("overlay storage is only taken in Drop")
        }

        #[inline]
        pub(crate) fn get_in_bounds(&self, key: &Key) -> Option<StateId> {
            let storage = self.storage();
            debug_assert!(
                storage.pack(key).is_some(),
                "overlay key is outside its configured bounds"
            );
            storage.get_packed(storage.pack_unchecked(key))
        }

        #[inline]
        pub(crate) fn insert_in_bounds(&mut self, key: Key, state: StateId) {
            self.storage_mut().insert_in_bounds(key, state);
        }
    }

    impl Drop for Overlay {
        fn drop(&mut self) {
            let Some(mut storage) = self.storage.take() else {
                return;
            };
            // Keep pages and their stamped cells. `reconfigure` advances the
            // generation on the next take, so retaining them avoids a full clear
            // while stale values remain unreadable.
            storage.keys.clear();
            let _ = OVERLAYS.try_with(|free| {
                if let Ok(mut f) = free.try_borrow_mut() {
                    if f.len() < KEEP {
                        f.push(storage);
                    }
                }
            });
        }
    }

    /// A write-order log of local keys whose backing `Vec` is recycled through
    /// this thread's free-list. `VegGrid::dirty`'s medium.
    ///
    /// Write **order** is world-visible here (the unified FEATURES dispatcher folds back in
    /// insertion order and a `DenseBlockGrid` appends to its palette in
    /// first-write order), so this stays a `Vec` and nothing about recycling
    /// touches that: a returned log is empty and a taken one is appended to from
    /// index 0, exactly as `Vec::new()` was.
    #[derive(Debug)]
    pub(crate) struct WriteLog {
        entries: Option<Vec<Key>>,
    }

    impl Default for WriteLog {
        fn default() -> Self {
            let recycled = LOGS
                .try_with(|free| free.try_borrow_mut().ok().and_then(|mut f| f.pop()))
                .ok()
                .flatten();
            Self { entries: Some(recycled.unwrap_or_default()) }
        }
    }

    impl WriteLog {
        fn entries(&self) -> &Vec<Key> {
            self.entries.as_ref().expect("write log is only taken in Drop")
        }

        pub(crate) fn push(&mut self, key: Key) {
            self.entries.as_mut().expect("write log is only taken in Drop").push(key);
        }

        pub(crate) fn len(&self) -> usize {
            self.entries().len()
        }

        pub(crate) fn iter(&self) -> impl Iterator<Item = &Key> {
            self.entries().iter()
        }
    }

    impl Drop for WriteLog {
        fn drop(&mut self) {
            let Some(mut entries) = self.entries.take() else {
                return;
            };
            entries.clear();
            let _ = LOGS.try_with(|free| {
                if let Ok(mut f) = free.try_borrow_mut() {
                    if f.len() < KEEP {
                        f.push(entries);
                    }
                }
            });
        }
    }
}
