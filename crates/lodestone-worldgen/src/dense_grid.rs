//! A dense, palette-indexed block field over a fixed axis-aligned box — O(1)
//! array access instead of a `HashMap<(i32,i32,i32), String>` keyed by world
//! coordinates.
//!
//! # Why this exists
//!
//! Composing carvers over `CarveGrid` (`crate::carver::CarveGrid`), itself
//! built from `HashMap<(i32,i32,i32), String>` shapes designed for parity
//! *harnesses* (a fixture is naturally sparse/keyed data), turned into a
//! measured regression once the same shape carried the *production*
//! per-chunk composition path: a 144-chunk sweep went from sub-second to
//! ~68s in debug. Every carve read/write and every `materialize_world`/
//! `intern_from_world` cell pays a hash of a 3-tuple key plus, for a write,
//! a fresh heap-allocated `String` clone — for a `16×384×16` chunk that is
//! ~98,304 cells, and ore composition (which runs the
//! pre-ore pipeline, carve included, for all 9 chunks in its 3×3
//! neighbourhood) multiplies that by 9 again.
//!
//! [`DenseBlockGrid`] is the fix: a flat `Vec<u16>` addressed by simple
//! arithmetic, palette-interned exactly like
//! [`crate::overworld::GeneratedColumn`] already is — this is *the* dense
//! representation the engine converges on at the end of every chunk's
//! pipeline anyway, so building the working grid this way from the start
//! means [`crate::overworld::OverworldGenerator::intern_from_dense`] can
//! adopt a centre-chunk-sized grid's palette/blocks directly instead of
//! re-hashing every cell a second time.
//!
//! # Test-adapter boundary
//!
//! [`DenseBlockGrid::from_hashmap`]/[`DenseBlockGrid::into_hashmap`] are the
//! seam `CLAUDE.md`'s Job 2 asks for: existing parity-harness tests
//! (`carver_parity.rs`, `feature_parity.rs`) keep building/reading
//! `HashMap<(i32,i32,i32), String>` fixtures unchanged — hand-writing a
//! sparse fixture as a literal map is clearer than constructing a dense grid
//! by hand, and those tests run once each, not per-chunk-per-neighbour, so
//! their conversion cost is irrelevant. Only the **production** composition
//! path in `crate::overworld` talks to `DenseBlockGrid` directly, with no
//! `HashMap<(i32,i32,i32), String>` in the hot loop at all.

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_worldgen_core::hash::FastMap;

use crate::interner::{StateId, StateInterner};

/// A dense block field over `[min_x, min_x+size_x) × [min_y, min_y+size_y) ×
/// [min_z, min_z+size_z)`, palette-indexed the same way
/// [`crate::overworld::GeneratedColumn`] is. A read outside the box returns
/// `"minecraft:air"`; a write outside the box is a no-op — matching the
/// convention every `HashMap<(i32,i32,i32), String>`-keyed grid this replaces
/// already had (a caller only ever reads/writes within the box it built the
/// grid for).
///
/// # The local palette holds ids, and its *order* is unchanged
///
/// Unit 3 (`docs/plans/worldgen-rewrite.md`) replaced this type's
/// `palette: Vec<String>` / `index_of: HashMap<String, u16>` with the
/// [`StateId`] equivalents. What that changed is the *cost* of a write — a
/// `u16` hash instead of a string hash, and **no `String` allocation on a new
/// palette entry**. What it deliberately did **not** change is the palette's
/// order: entries are still appended in first-write order, so
/// [`Self::into_palette_and_blocks`] emits a byte-identical `Vec<String>` and
/// `blocks` is untouched. See [`crate::interner`]'s module doc for why that
/// property is what keeps interner id-assignment order off the wire.
#[derive(Debug, Clone)]
pub struct DenseBlockGrid {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    size_x: i32,
    size_y: i32,
    size_z: i32,
    /// Shared with every other grid in the same generator, so a [`StateId`]
    /// read out of one grid can be written straight into another with no string
    /// round-trip — the hop that
    /// [`crate::overworld::OverworldGenerator::stitch_veg_region`] performed
    /// 884,736 times per warm column.
    interner: Arc<StateInterner>,
    /// Local palette, in first-write order (see the type doc).
    palette: Vec<StateId>,
    /// `palette` resolved to strings, appended in lock-step with it, so
    /// [`Self::get`] is a plain array read.
    ///
    /// **This exists to keep a shared lock off a per-cell path.** Resolving
    /// through the interner instead would take an `RwLock` read guard *per cell
    /// read*, and one `OverworldGenerator` is shared by every concurrent
    /// generation call (`OverworldChunkSource` holds it by value), so the carve
    /// path's per-cell `CarveGrid::get` would put ~289 threads on one cache
    /// line. `4307b59` is a revert in this repo caused by exactly that shape.
    /// Resolving once per *new palette entry* instead costs one `name_of` per
    /// entry — measured at **76 per chunk** (`palette_intern_new` 10,958 over a
    /// 144-chunk sweep) against ~98,304 cell reads.
    ///
    /// `&'static str` because [`StateInterner`] leaks its names, which is what
    /// makes this a plain `Copy` shadow rather than a second set of owned
    /// `String`s.
    palette_names: Vec<&'static str>,
    /// Reverse lookup for [`Self::palette`] — **not** an ordered structure, and
    /// never iterated (see U17's note on [`FastMap`]). `palette` is the thing
    /// whose order reaches the wire, and it is a `Vec` appended in first-write
    /// order; `index_of` only answers "is this state already in it".
    ///
    /// [`FastMap`] rather than the default hasher because this is probed on
    /// **every block write** — ~98,304 per chunk fill, ×25 for a cold column's
    /// pre-ore closure — on a `u16` key. U17's profile measured that probe at
    /// 11.8% of all SipHash time in the pipeline.
    index_of: FastMap<StateId, u16>,
    blocks: Vec<u16>,
}

impl DenseBlockGrid {
    /// A grid over the given box, every cell initialised to `default`
    /// (palette index 0), against a **fresh private interner**.
    ///
    /// Convenience for parity harnesses and unit tests, which build one grid in
    /// isolation. Production code paths must use [`Self::with_interner`] so
    /// every grid in a generator shares one table — ids from two different
    /// interners are not comparable (see [`StateId`]).
    #[must_use]
    pub fn new(min_x: i32, min_y: i32, min_z: i32, size_x: i32, size_y: i32, size_z: i32, default: &str) -> Self {
        let interner = Arc::new(StateInterner::new());
        let default = interner.id_of(default);
        Self::with_interner(interner, min_x, min_y, min_z, size_x, size_y, size_z, default)
    }

    /// A grid over the given box against a **shared** interner, every cell
    /// initialised to `default` (palette index 0).
    // Eight: a six-field bounding box, the interner, and the default state.
    // `new`/`from_hashmap` were already at the seven-argument limit before the
    // interner was threaded through. Folding the box into a `GridBounds` struct
    // is the real fix and would drop every one of these to three, but it changes
    // the two string-taking constructors that the parity harnesses call, so it
    // belongs in the decomposition unit rather than in a commit whose whole
    // claim is that it changed representation and nothing else.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn with_interner(
        interner: Arc<StateInterner>,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        default: StateId,
    ) -> Self {
        let mut index_of = FastMap::default();
        index_of.insert(default, 0u16);
        let cells = (size_x.max(0) as usize) * (size_y.max(0) as usize) * (size_z.max(0) as usize);
        let default_name = interner.name_of(default);
        Self {
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            interner,
            palette: vec![default],
            palette_names: vec![default_name],
            index_of,
            blocks: vec![0u16; cells],
        }
    }

    /// This grid's interner, for a caller building a second grid that must
    /// share it (or resolving an id this grid returned).
    #[must_use]
    pub fn interner(&self) -> &Arc<StateInterner> {
        &self.interner
    }

    #[inline]
    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let lx = x - self.min_x;
        let ly = y - self.min_y;
        let lz = z - self.min_z;
        if (0..self.size_x).contains(&lx) && (0..self.size_y).contains(&ly) && (0..self.size_z).contains(&lz) {
            Some(((ly * self.size_z + lz) * self.size_x + lx) as usize)
        } else {
            None
        }
    }

    /// Interned state id at `(x, y, z)`. [`StateId::AIR`] outside the box.
    ///
    /// The zero-allocation read path — prefer it over [`Self::get`] everywhere
    /// inside the engine. Valid only against [`Self::interner`].
    #[must_use]
    pub fn get_id(&self, x: i32, y: i32, z: i32) -> StateId {
        match self.index(x, y, z) {
            Some(i) => self.palette[self.blocks[i] as usize],
            None => StateId::AIR,
        }
    }

    /// Canonical block-state string at `(x, y, z)`. `"minecraft:air"` outside
    /// the box.
    ///
    /// Served from [`Self::palette_names`], so this is a plain array read — no
    /// interner lock and no allocation. Still prefer [`Self::get_id`] inside the
    /// engine, but this shim is cheap enough for a per-cell caller (which
    /// `CarveGrid::get` is).
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> &str {
        match self.index(x, y, z) {
            Some(i) => self.palette_names[self.blocks[i] as usize],
            None => "minecraft:air",
        }
    }

    /// Writes the interned `state` at `(x, y, z)`. A no-op outside the box
    /// (matching the prior `HashMap`-keyed grids' implicit contract: nothing in
    /// this engine writes outside the box it built a working grid for).
    ///
    /// The zero-allocation write path: a new palette entry costs a `Vec` push
    /// and a `u16`-keyed map insert, and **allocates nothing** — this is where
    /// U3's acceptance criterion is paid.
    pub fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId) {
        let Some(i) = self.index(x, y, z) else {
            return;
        };
        let id = if let Some(&id) = self.index_of.get(&state) {
            // Diagnostic D2's other half: a palette probe on every block write.
            // Still counted, because its *volume* is what U6/U7 reduce; what
            // changed in U3 is that it is now a `u16` hash, not a string hash.
            crate::counters::bump_palette_intern_hit();
            id
        } else {
            crate::counters::bump_palette_intern_new();
            let id = u16::try_from(self.palette.len()).expect("more than 65,536 palette entries in one grid");
            self.palette.push(state);
            // The one interner touch on the write path, and it happens only for a
            // state this grid has not seen before (~76 per chunk). Kept in
            // lock-step with `palette` so the two are always the same length.
            self.palette_names.push(self.interner.name_of(state));
            self.index_of.insert(state, id);
            id
        };
        self.blocks[i] = id;
    }

    /// Writes `state` at `(x, y, z)`, interning it first.
    ///
    /// A shim over [`Self::set_id`] for callers not yet ported to ids. Interning
    /// allocates the first time this generator sees `state` and never again, so
    /// this is safe for warm paths but still costs an interner lock — prefer
    /// [`Self::set_id`] in a loop.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: &str) {
        let id = self.interner.id_of(state);
        self.set_id(x, y, z, id);
    }

    /// Test-adapter constructor (see module doc): builds a dense grid over
    /// the given box from a sparse/fully-populated
    /// `HashMap<(i32,i32,i32), String>` fixture, defaulting any cell the map
    /// doesn't mention to `"minecraft:air"`.
    #[must_use]
    pub fn from_hashmap(
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        map: &HashMap<(i32, i32, i32), String>,
    ) -> Self {
        let mut grid = Self::new(min_x, min_y, min_z, size_x, size_y, size_z, "minecraft:air");
        for (&(x, y, z), state) in map {
            grid.set(x, y, z, state);
        }
        grid
    }

    /// [`Self::from_hashmap`] against a **shared** interner — the form
    /// production must use, so the resulting grid's ids are comparable with
    /// every other grid in the same generator.
    // See `with_interner` for why this is allowed rather than restructured.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn from_hashmap_with_interner(
        interner: Arc<StateInterner>,
        min_x: i32,
        min_y: i32,
        min_z: i32,
        size_x: i32,
        size_y: i32,
        size_z: i32,
        map: &HashMap<(i32, i32, i32), String>,
    ) -> Self {
        let air = interner.id_of("minecraft:air");
        let mut grid = Self::with_interner(interner, min_x, min_y, min_z, size_x, size_y, size_z, air);
        for (&(x, y, z), state) in map {
            grid.set(x, y, z, state);
        }
        grid
    }

    /// Test-adapter destructor (see module doc): every cell in the box,
    /// including untouched-default ones — matching what
    /// `crate::overworld::OverworldGenerator::materialize_world` used to
    /// build directly as a `HashMap`.
    #[must_use]
    pub fn into_hashmap(self) -> HashMap<(i32, i32, i32), String> {
        // `palette_names` is already the resolved palette, so this costs one
        // `to_owned()` per cell (the `HashMap` owns its values) and zero interner
        // work.
        let names = &self.palette_names;
        let mut out = HashMap::with_capacity(self.blocks.len());
        for ly in 0..self.size_y {
            for lz in 0..self.size_z {
                for lx in 0..self.size_x {
                    let i = ((ly * self.size_z + lz) * self.size_x + lx) as usize;
                    let state = names[self.blocks[i] as usize].to_owned();
                    out.insert((self.min_x + lx, self.min_y + ly, self.min_z + lz), state);
                }
            }
        }
        out
    }

    /// The box's origin and size, for a caller that needs to re-derive
    /// `(lx, ly, lz)` bounds (e.g. [`crate::overworld::GeneratedColumn`]
    /// adoption).
    #[must_use]
    pub fn bounds(&self) -> (i32, i32, i32, i32, i32, i32) {
        (self.min_x, self.min_y, self.min_z, self.size_x, self.size_y, self.size_z)
    }

    /// Consumes the grid into its raw `(palette, blocks)` parts, laid out
    /// `blocks[(ly * size_z + lz) * size_x + lx]` — identical to
    /// [`crate::overworld::GeneratedColumn`]'s own layout when `size_x ==
    /// size_z == 16`, so a centre-chunk-sized grid can be adopted directly
    /// with no re-intern pass.
    /// The palette resolved back to strings, in unchanged first-write order —
    /// O(palette) allocations (~50), which is the
    /// `docs/plans/worldgen-rewrite.md` allocation budget's explicit output
    /// allowance ("O(1) allocations for the returned `GeneratedColumn`'s own
    /// `palette`/`blocks` buffers, because they leave the function"), not hot-path
    /// traffic.
    #[must_use]
    pub fn into_palette_and_blocks(self) -> (Vec<String>, Vec<u16>) {
        let palette = self.palette_names.iter().map(|&name| name.to_owned()).collect();
        (palette, self.blocks)
    }

    /// The palette as interned ids, in first-write order — the allocation-free
    /// counterpart of [`Self::into_palette_and_blocks`], for a caller that can
    /// carry ids instead of strings.
    #[must_use]
    pub fn into_id_palette_and_blocks(self) -> (Vec<StateId>, Vec<u16>) {
        (self.palette, self.blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_set_round_trips_within_bounds() {
        let mut g = DenseBlockGrid::new(5, -64, 5, 4, 8, 4, "minecraft:air");
        assert_eq!(g.get(5, -64, 5), "minecraft:air");
        g.set(6, -60, 7, "minecraft:stone");
        assert_eq!(g.get(6, -60, 7), "minecraft:stone");
        // Neighbouring cells are untouched.
        assert_eq!(g.get(6, -60, 6), "minecraft:air");
    }

    #[test]
    fn out_of_bounds_read_is_air_and_write_is_noop() {
        let mut g = DenseBlockGrid::new(0, 0, 0, 2, 2, 2, "minecraft:air");
        assert_eq!(g.get(100, 100, 100), "minecraft:air");
        g.set(100, 100, 100, "minecraft:stone"); // must not panic
        assert_eq!(g.get(100, 100, 100), "minecraft:air");
    }

    #[test]
    fn hashmap_round_trip_preserves_every_cell() {
        let mut map = HashMap::new();
        for x in 0..3 {
            for y in 0..3 {
                for z in 0..3 {
                    map.insert((x, y, z), format!("minecraft:cell_{x}_{y}_{z}"));
                }
            }
        }
        let grid = DenseBlockGrid::from_hashmap(0, 0, 0, 3, 3, 3, &map);
        let back = grid.into_hashmap();
        assert_eq!(back, map);
    }

    #[test]
    fn palette_order_is_first_write_order() {
        // The invariant that keeps interner id-assignment order off the wire
        // (see `crate::interner`'s module doc). Written deliberately in an order
        // that does *not* match either alphabetical order or the order the
        // interner would assign ids in if it had seen these states earlier.
        let mut g = DenseBlockGrid::new(0, 0, 0, 4, 4, 4, "minecraft:air");
        g.set(0, 0, 0, "minecraft:zircon");
        g.set(1, 0, 0, "minecraft:andesite");
        g.set(2, 0, 0, "minecraft:zircon");
        g.set(3, 0, 0, "minecraft:marble");
        let (palette, _) = g.into_palette_and_blocks();
        assert_eq!(
            palette,
            vec![
                "minecraft:air".to_string(),
                "minecraft:zircon".to_string(),
                "minecraft:andesite".to_string(),
                "minecraft:marble".to_string(),
            ],
        );
    }

    #[test]
    fn an_id_read_from_one_grid_writes_into_another_sharing_the_interner() {
        // Exactly what `stitch_veg_region` does, and the property that makes the
        // 884,736 `to_string()` calls deletable.
        let interner = Arc::new(StateInterner::new());
        let air = interner.id_of("minecraft:air");
        let mut src = DenseBlockGrid::with_interner(Arc::clone(&interner), 0, 0, 0, 2, 2, 2, air);
        let mut dst = DenseBlockGrid::with_interner(Arc::clone(&interner), 0, 0, 0, 2, 2, 2, air);
        src.set(1, 1, 1, "minecraft:deepslate");

        dst.set_id(0, 0, 0, src.get_id(1, 1, 1));

        assert_eq!(dst.get(0, 0, 0), "minecraft:deepslate");
    }

    #[test]
    fn palette_names_stays_in_lock_step_with_the_id_palette() {
        // `get` reads `palette_names[blocks[i]]` while `set_id` indexes
        // `palette`, so a desynchronised pair would silently return the wrong
        // block — a fully-connected wire carrying the wrong value, which no
        // type check can see. Asserted through the public API: every cell's
        // `get` must agree with `name_of(get_id)`.
        let mut g = DenseBlockGrid::new(0, 0, 0, 3, 3, 3, "minecraft:air");
        let states = [
            "minecraft:stone",
            "minecraft:deepslate",
            "minecraft:oak_log[axis=y]",
            "minecraft:stone",
            "minecraft:gravel",
        ];
        for (i, state) in states.iter().enumerate() {
            let i = i as i32;
            g.set(i % 3, i / 3, 0, state);
        }
        let interner = Arc::clone(g.interner());
        for x in 0..3 {
            for y in 0..3 {
                for z in 0..3 {
                    assert_eq!(
                        g.get(x, y, z),
                        interner.name_of(g.get_id(x, y, z)),
                        "palette_names disagreed with the id palette at ({x}, {y}, {z})",
                    );
                }
            }
        }
    }

    #[test]
    fn get_id_is_air_outside_the_box() {
        let g = DenseBlockGrid::new(0, 0, 0, 2, 2, 2, "minecraft:air");
        assert_eq!(g.get_id(100, 100, 100), StateId::AIR);
    }

    #[test]
    fn id_palette_and_string_palette_agree_entry_for_entry() {
        let mut g = DenseBlockGrid::new(0, 0, 0, 2, 2, 2, "minecraft:air");
        g.set(0, 0, 0, "minecraft:tuff");
        g.set(1, 0, 0, "minecraft:calcite");
        let interner = Arc::clone(g.interner());
        let (ids, id_blocks) = g.clone().into_id_palette_and_blocks();
        let (names, name_blocks) = g.into_palette_and_blocks();
        assert_eq!(id_blocks, name_blocks, "blocks must not depend on palette form");
        let resolved: Vec<String> = ids.iter().map(|&id| interner.name_of(id).to_owned()).collect();
        assert_eq!(resolved, names);
    }

    #[test]
    fn repeated_writes_of_the_same_state_reuse_one_palette_entry() {
        let mut g = DenseBlockGrid::new(0, 0, 0, 4, 4, 4, "minecraft:air");
        for i in 0..10 {
            g.set(i % 4, 0, 0, "minecraft:granite");
        }
        let (palette, _blocks) = g.into_palette_and_blocks();
        assert_eq!(palette, vec!["minecraft:air".to_string(), "minecraft:granite".to_string()]);
    }
}
