//! Realistic chunk-churn workload built on the *actual* `lodestone-world`
//! storage types, so the allocator sees the same allocations the running game
//! makes: paletted block containers (size-classed `Vec<u64>` backings), biome
//! containers, `Arc<ChunkSection>` boxes, and 2 KiB `Arc`-backed light nibble
//! arrays — allocated on worker threads and freed either locally or on a
//! separate consumer thread (the mesh-upload / chunk-unload handoff).
//!
//! The per-column shape is calibrated to impl-world's measured figures for this
//! game: a realistic overworld column has ~7 of 24 sections carrying heap (the
//! rest are single-valued air/stone → 0 heap), ~16 KiB of block storage and
//! ~9 KiB of light. Single-valued sections are modelled as genuinely elided
//! (never allocated), which is the single biggest departure from a naive
//! "allocate every section" microbenchmark and the reason the system allocator
//! looks better here than under synthetic churn.

use lodestone_world::{
    ChunkColumn, ChunkSection, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, PalettedContainer,
};

use crate::Rng;

const SECTIONS: usize = 24;
const MIN_Y: i32 = -64;
const AIR_ID: u32 = 0;
const BIOME_ID: u32 = 0;

/// Distinct palette entries needed to force each target bits-per-entry class in
/// a 4096-entry block container (indirect min 4 / max 8 bits, then direct 15).
/// Calibrated to impl-world's measured realistic terrain (~16 KiB of block
/// storage across ~7 sections ⇒ ~2.3 KiB/section ⇒ mostly 4–5 bpe). Denser
/// worst-case sections (15 bpe) are deliberately rare here; the dense scenario
/// is a separate case, not "realistic terrain".
const BPE_MIX: [(usize, u32); 4] = [
    (12, 60), // -> 4 bpe  (2048 B backing)
    (24, 25), // -> 5 bpe  (2736 B)
    (48, 10), // -> 6 bpe  (3280 B)
    (200, 5), // -> 8 bpe  (4096 B)
];

fn pick_distinct(rng: &mut Rng) -> usize {
    let roll = (rng.next() % 100) as u32;
    let mut acc = 0;
    for &(k, w) in &BPE_MIX {
        acc += w;
        if roll < acc {
            return k;
        }
    }
    BPE_MIX[0].0
}

/// Build one paletted block section with `distinct` palette entries, which the
/// real palette-selection logic packs at the corresponding bits-per-entry.
fn build_section(rng: &mut Rng, distinct: usize) -> ChunkSection {
    // 4096 block entries drawn from `distinct` non-air ids (+ some air), so the
    // container really allocates its size-classed backing store.
    let mut values = vec![0u32; 4096];
    for (i, v) in values.iter_mut().enumerate() {
        // ~20% air so non_air_count is realistic; the rest cycle the palette.
        *v = if rng.next().is_multiple_of(5) {
            AIR_ID
        } else {
            1 + (i as u32 % distinct as u32)
        };
    }
    let block_states = PalettedContainer::from_values(PaletteKind::block_states(), &values);
    // Biomes are overwhelmingly single-valued in a column → 0 heap, matching
    // real worlds; keep them single here.
    let biomes = PalettedContainer::new(PaletteKind::biomes(), BIOME_ID);
    ChunkSection::from_containers(block_states, biomes, AIR_ID)
}

/// Assemble one realistic `LoadedChunk` (~7 heap sections + ~5 light arrays).
pub fn build_column(rng: &mut Rng) -> LoadedChunk {
    let mut column = ChunkColumn::new(
        MIN_Y,
        SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        AIR_ID,
        BIOME_ID,
    );

    // 6..9 of 24 sections carry heap; the rest stay elided (single-valued air).
    let allocated = 6 + (rng.next() % 3) as usize;
    // Cluster the allocated sections into a surface band (indices ~3..3+n),
    // which is where real terrain concentrates non-air blocks.
    let base = 3 + (rng.next() % 3) as usize;
    for s in 0..allocated {
        let idx = (base + s).min(SECTIONS - 1);
        let distinct = pick_distinct(rng);
        column.set_section(idx, Some(build_section(rng, distinct)));
    }

    // Light: ~4 sections carry a real 2 KiB block-light array, ~1 a sky array,
    // matching impl-world's ~9 KiB/column. A single divergent write
    // materialises the array (copy-on-write).
    let mut light = ColumnLight::new(SECTIONS);
    let lit = 3 + (rng.next() % 3) as usize;
    for s in 0..lit {
        let i = (base + s).min(light.light_section_count() - 1);
        light.set_block_light(i, (rng.next() % 4096) as usize, 1 + (rng.next() % 15) as u8);
        if s == 0 {
            light.set_sky_light(i, (rng.next() % 4096) as usize, 1 + (rng.next() % 15) as u8);
        }
    }

    LoadedChunk::new(column, light, Heightmaps::new(), Vec::new())
}
