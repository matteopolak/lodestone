//! A dense, palette-indexed block field over a fixed axis-aligned box — O(1)
//! array access instead of a `HashMap<(i32,i32,i32), String>` keyed by world
//! coordinates (issue #295's Job 2).
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
//! ~98,304 cells, and issue #295's ore composition (which runs the
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

/// A dense block field over `[min_x, min_x+size_x) × [min_y, min_y+size_y) ×
/// [min_z, min_z+size_z)`, palette-indexed the same way
/// [`crate::overworld::GeneratedColumn`] is. A read outside the box returns
/// `"minecraft:air"`; a write outside the box is a no-op — matching the
/// convention every `HashMap<(i32,i32,i32), String>`-keyed grid this replaces
/// already had (a caller only ever reads/writes within the box it built the
/// grid for).
#[derive(Debug, Clone)]
pub struct DenseBlockGrid {
    min_x: i32,
    min_y: i32,
    min_z: i32,
    size_x: i32,
    size_y: i32,
    size_z: i32,
    palette: Vec<String>,
    index_of: HashMap<String, u16>,
    blocks: Vec<u16>,
}

impl DenseBlockGrid {
    /// A grid over the given box, every cell initialised to `default`
    /// (palette index 0).
    #[must_use]
    pub fn new(min_x: i32, min_y: i32, min_z: i32, size_x: i32, size_y: i32, size_z: i32, default: &str) -> Self {
        let mut index_of = HashMap::new();
        index_of.insert(default.to_string(), 0u16);
        let cells = (size_x.max(0) as usize) * (size_y.max(0) as usize) * (size_z.max(0) as usize);
        Self {
            min_x,
            min_y,
            min_z,
            size_x,
            size_y,
            size_z,
            palette: vec![default.to_string()],
            index_of,
            blocks: vec![0u16; cells],
        }
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

    /// Canonical block-state string at `(x, y, z)`. `"minecraft:air"` outside
    /// the box.
    #[must_use]
    pub fn get(&self, x: i32, y: i32, z: i32) -> &str {
        match self.index(x, y, z) {
            Some(i) => self.palette[self.blocks[i] as usize].as_str(),
            None => "minecraft:air",
        }
    }

    /// Writes `state` at `(x, y, z)`. A no-op outside the box (matching the
    /// prior `HashMap`-keyed grids' implicit contract: nothing in this
    /// engine writes outside the box it built a working grid for).
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: &str) {
        let Some(i) = self.index(x, y, z) else {
            return;
        };
        let id = if let Some(&id) = self.index_of.get(state) {
            // The common case, and diagnostic D2: a `HashMap<String, u16>` probe
            // on every block write. Counted separately from the new-entry branch
            // because U3's acceptance criterion is about the *allocations* (the
            // branch below), while this branch's count is the hash-probe volume.
            crate::counters::bump_palette_intern_hit();
            id
        } else {
            crate::counters::bump_palette_intern_new();
            let id = self.palette.len() as u16;
            self.palette.push(state.to_string());
            self.index_of.insert(state.to_string(), id);
            id
        };
        self.blocks[i] = id;
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

    /// Test-adapter destructor (see module doc): every cell in the box,
    /// including untouched-default ones — matching what
    /// `crate::overworld::OverworldGenerator::materialize_world` used to
    /// build directly as a `HashMap`.
    #[must_use]
    pub fn into_hashmap(self) -> HashMap<(i32, i32, i32), String> {
        let mut out = HashMap::with_capacity(self.blocks.len());
        for ly in 0..self.size_y {
            for lz in 0..self.size_z {
                for lx in 0..self.size_x {
                    let i = ((ly * self.size_z + lz) * self.size_x + lx) as usize;
                    let state = self.palette[self.blocks[i] as usize].clone();
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
    #[must_use]
    pub fn into_palette_and_blocks(self) -> (Vec<String>, Vec<u16>) {
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
    fn repeated_writes_of_the_same_state_reuse_one_palette_entry() {
        let mut g = DenseBlockGrid::new(0, 0, 0, 4, 4, 4, "minecraft:air");
        for i in 0..10 {
            g.set(i % 4, 0, 0, "minecraft:granite");
        }
        let (palette, _blocks) = g.into_palette_and_blocks();
        assert_eq!(palette, vec!["minecraft:air".to_string(), "minecraft:granite".to_string()]);
    }
}
