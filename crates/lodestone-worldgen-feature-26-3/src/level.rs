//! The decoration medium: a 3x3 chunk window with the reference region's read and write rules
//! (heightmap upkeep, void-air outside the build height) and a write journal.

use std::sync::Arc;

use lodestone_worldgen_core::engine::release26_3::biome::{BiomeId, BiomeInfo, BiomeTable, zoomed_biome};

use crate::blocks::State;
use crate::env::{Env, Heightmap};

/// One chunk's blocks (`y + (x + z * 16) * height`, `y` relative to the minimum) and its six
/// heightmaps (each the first free Y above the highest counted block, as the reference stores it).
#[derive(Clone, Debug)]
pub struct ChunkData {
    pub states: Vec<State>,
    hm: [[i32; 256]; 6],
}

impl ChunkData {
    /// Primes every heightmap from the blocks, the way terrain generation leaves them.
    #[must_use]
    pub fn new(env: &Env, min_y: i32, height: i32, states: Vec<State>) -> Self {
        assert_eq!(states.len(), (256 * height) as usize);
        let hm = prime(env, min_y, height, &states, 0x3f);
        Self { states, hm }
    }

    /// The same chunk holding `states`: live heightmaps are recomputed from them and the frozen
    /// world-generation maps (final once carving is done) are kept.
    #[must_use]
    pub fn with_states(&self, env: &Env, min_y: i32, height: i32, states: Vec<State>) -> Self {
        assert_eq!(states.len(), (256 * height) as usize);
        let mut hm = prime(env, min_y, height, &states, 0b11_1010);
        for h in Heightmap::ALL {
            if !h.live() {
                hm[h as usize] = self.hm[h as usize];
            }
        }
        Self { states, hm }
    }
}

/// The heightmaps selected by the bit set `wanted` (bit `i` for heightmap `i`), the first free Y
/// above the highest counted block of each column; unselected maps stay at `min_y`.
fn prime(env: &Env, min_y: i32, height: i32, states: &[State], wanted: u8) -> [[i32; 256]; 6] {
    let mut hm = [[min_y; 256]; 6];
    for col in 0..256usize {
        let base = col * height as usize;
        let mut remaining = wanted;
        for dy in (0..height as usize).rev() {
            let s = states[base + dy];
            if s == env.known.air {
                continue;
            }
            for h in Heightmap::ALL {
                if remaining & (1 << h as u8) != 0 && env.counts_for(h, s) {
                    hm[h as usize][col] = min_y + dy as i32 + 1;
                    remaining &= !(1 << h as u8);
                }
            }
            if remaining == 0 {
                break;
            }
        }
    }
    hm
}

/// A read/write window over 3x3 chunks.
pub struct Level<'a> {
    pub env: &'a Env,
    pub min_y: i32,
    pub height: i32,
    /// The generator's own vertical range (anchors resolve against it).
    pub gen_min_y: i32,
    pub gen_depth: i32,
    pub sea_level: i32,
    pub seed: i64,
    /// The region's own random source (a few blocks draw from it instead of the feature's).
    pub region_rng: lodestone_worldgen_core::rng::LegacyRandomSource,
    zoom_seed: i64,
    cx0: i32,
    cz0: i32,
    chunks: Vec<ChunkData>,
    biome_source: Box<dyn Fn(i32, i32, i32) -> BiomeId + 'a>,
    /// The climate table biome queries resolve against; the decorator installs it.
    pub biomes: Option<Arc<BiomeTable>>,
    journal: Vec<(i32, i32, i32, State)>,
    journaling: bool,
    /// Reads or writes that fell outside the window (the reference would fault).
    pub out_of_window: u32,
}

impl std::fmt::Debug for Level<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Level").field("cx0", &self.cx0).field("cz0", &self.cz0).finish_non_exhaustive()
    }
}

impl<'a> Level<'a> {
    /// `chunks` are the nine chunks, row-major by `z` then `x`, starting at `(center_x - 1,
    /// center_z - 1)`. `biome_source` resolves quart coordinates; it is zoomed with the
    /// obfuscated seed `zoom_seed`.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        env: &'a Env,
        seed: i64,
        zoom_seed: i64,
        center_x: i32,
        center_z: i32,
        min_y: i32,
        height: i32,
        sea_level: i32,
        chunks: Vec<ChunkData>,
        biome_source: Box<dyn Fn(i32, i32, i32) -> BiomeId + 'a>,
    ) -> Self {
        assert_eq!(chunks.len(), 9);
        Self {
            env,
            min_y,
            height,
            gen_min_y: min_y,
            gen_depth: height,
            sea_level,
            seed,
            region_rng: lodestone_worldgen_core::rng::LegacyRandomSource::new(seed),
            zoom_seed,
            cx0: center_x - 1,
            cz0: center_z - 1,
            chunks,
            biome_source,
            biomes: None,
            journal: Vec::new(),
            journaling: false,
            out_of_window: 0,
        }
    }

    #[must_use]
    pub fn max_y(&self) -> i32 {
        self.min_y + self.height - 1
    }

    #[must_use]
    pub fn is_outside_build_height(&self, y: i32) -> bool {
        y < self.min_y || y >= self.min_y + self.height
    }

    fn chunk_index(&self, x: i32, z: i32) -> Option<usize> {
        let (dx, dz) = ((x >> 4) - self.cx0, (z >> 4) - self.cz0);
        ((0..3).contains(&dx) && (0..3).contains(&dz)).then(|| (dz * 3 + dx) as usize)
    }

    fn cell(&self, x: i32, y: i32, z: i32) -> usize {
        ((y - self.min_y) + ((x & 15) + (z & 15) * 16) * self.height) as usize
    }

    /// The block at a position; void air outside the build height or the window.
    pub fn get(&self, x: i32, y: i32, z: i32) -> State {
        if self.is_outside_build_height(y) {
            return self.env.known.void_air;
        }
        match self.chunk_index(x, z) {
            Some(i) => self.chunks[i].states[self.cell(x, y, z)],
            None => self.env.known.void_air,
        }
    }

    /// Like [`get`](Self::get) but counts reads outside the window.
    pub fn get_checked(&mut self, x: i32, y: i32, z: i32) -> State {
        if self.chunk_index(x, z).is_none() {
            self.out_of_window += 1;
        }
        self.get(x, y, z)
    }

    /// The first free Y of a heightmap column (the reference region's own height query).
    pub fn height(&self, h: Heightmap, x: i32, z: i32) -> i32 {
        match self.chunk_index(x, z) {
            Some(i) => self.chunks[i].hm[h as usize][((x & 15) + (z & 15) * 16) as usize],
            None => self.min_y,
        }
    }

    /// Whether a column is inside the writable window.
    #[must_use]
    pub fn can_write(&self, x: i32, z: i32) -> bool {
        self.chunk_index(x, z).is_some()
    }

    /// The region write: writes and keeps the live heightmaps current. Returns whether the
    /// position is writable (inside the window); positions above or below the build height
    /// are accepted and dropped.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: State) -> bool {
        let Some(i) = self.chunk_index(x, z) else {
            self.out_of_window += 1;
            return false;
        };
        if self.is_outside_build_height(y) {
            return true;
        }
        let cell = self.cell(x, y, z);
        let old = self.chunks[i].states[cell];
        if self.journaling {
            self.journal.push((x, y, z, old));
        }
        self.chunks[i].states[cell] = state;
        let col = ((x & 15) + (z & 15) * 16) as usize;
        for h in Heightmap::ALL {
            if h.live() {
                self.update_heightmap(i, h, col, y, state);
            }
        }
        true
    }

    /// A write straight into a chunk section: no heightmap upkeep (what bulk-writing features do).
    pub fn set_raw(&mut self, x: i32, y: i32, z: i32, state: State) {
        let Some(i) = self.chunk_index(x, z) else {
            self.out_of_window += 1;
            return;
        };
        if self.is_outside_build_height(y) {
            return;
        }
        let cell = self.cell(x, y, z);
        if self.journaling {
            self.journal.push((x, y, z, self.chunks[i].states[cell]));
        }
        self.chunks[i].states[cell] = state;
    }

    fn update_heightmap(&mut self, chunk: usize, h: Heightmap, col: usize, y: i32, state: State) {
        let first_available = self.chunks[chunk].hm[h as usize][col];
        if y <= first_available - 2 {
            return;
        }
        if self.env.counts_for(h, state) {
            if y >= first_available {
                self.chunks[chunk].hm[h as usize][col] = y + 1;
            }
        } else if first_available - 1 == y {
            let base = col * self.height as usize;
            let mut yy = y - 1;
            while yy >= self.min_y {
                let s = self.chunks[chunk].states[base + (yy - self.min_y) as usize];
                if self.env.counts_for(h, s) {
                    self.chunks[chunk].hm[h as usize][col] = yy + 1;
                    return;
                }
                yy -= 1;
            }
            self.chunks[chunk].hm[h as usize][col] = self.min_y;
        }
    }

    /// The zoomed biome at a block position. The stored biomes are clamped to the world's
    /// vertical quart range, so queries above or below it read the nearest stored cell.
    #[must_use]
    pub fn biome(&self, x: i32, y: i32, z: i32) -> BiomeId {
        let min_quart = self.min_y >> 2;
        let max_quart = min_quart + (self.height >> 2) - 1;
        let mut src = |qx: i32, qy: i32, qz: i32| (self.biome_source)(qx, qy.clamp(min_quart, max_quart), qz);
        zoomed_biome(self.zoom_seed, x, y, z, &mut src)
    }

    /// The climate of the zoomed biome at a block position.
    ///
    /// # Panics
    /// If no biome table is installed (the decorator installs one before features run).
    #[must_use]
    pub fn biome_info(&self, x: i32, y: i32, z: i32) -> &BiomeInfo {
        let id = self.biome(x, y, z);
        self.biomes.as_ref().expect("a biome table is installed").info(id)
    }

    /// Starts recording writes (for per-feature diffs).
    pub fn begin_journal(&mut self) {
        self.journaling = true;
        self.journal.clear();
    }

    /// The positions changed since the journal was last drained, with their current states,
    /// sorted by position; a write that restored the original state is not a change.
    pub fn drain_changes(&mut self) -> Vec<(i32, i32, i32, State)> {
        // A stable sort by position keeps each position's first recorded state first.
        self.journal.sort_by_key(|&(x, y, z, _)| (x, y, z));
        let mut out: Vec<(i32, i32, i32, State)> = Vec::new();
        let mut i = 0;
        while i < self.journal.len() {
            let (x, y, z, old) = self.journal[i];
            while i < self.journal.len() && self.journal[i].0 == x && self.journal[i].1 == y && self.journal[i].2 == z {
                i += 1;
            }
            let now = self.get(x, y, z);
            if now != old {
                out.push((x, y, z, now));
            }
        }
        self.journal.clear();
        out
    }

    /// The nine chunks' blocks, for hashing.
    #[must_use]
    pub fn chunk_states(&self, dx: i32, dz: i32) -> &[State] {
        &self.chunks[(dz * 3 + dx) as usize].states
    }
}
