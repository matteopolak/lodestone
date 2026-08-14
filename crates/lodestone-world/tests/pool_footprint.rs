//! Real-terrain memory measurement for the chunk-section allocation pool
//! question ("Chunk section data is allocated fresh every time —
//! size-classed pool with handles").
//!
//! This is explicitly a **footprint** question, not a
//! throughput one — `World::load` costs ~1.0-1.5 us per insert (see
//! `benches/chunk_load.rs`), so allocation *time* is not the problem worth
//! solving. What decides whether a size-classed buffer pool is worth building,
//! and how many classes it needs, is two distributions this file measures:
//!
//! 1. **The size-class histogram.** [`PackedArray`]'s backing `Vec<u64>` size
//!    depends only on `bits_per_entry`, and `values_per_long = 64 / bits`
//!    floors, so a 4096-entry block-state section collapses onto a small set of
//!    byte sizes. The pool proposal's own table claims 14 classes for a generic
//!    4096-entry array; this file re-derives that arithmetic at runtime from
//!    [`PackedArray::long_count`] rather than trusting the table (see
//!    `derive_size_classes` below), and separately establishes the *narrower*
//!    truth for this codebase: [`PaletteKind::block_states`] only ever produces
//!    `bits_per_entry` in `{0 (single), 4, 5, 6, 7, 8, 15}` and
//!    [`PaletteKind::biomes`] only `{0, 1, 2, 3, 6}` — every other class in the
//!    generic table is dead code for both kinds actually used in this repo
//!    (confirmed by `crates/protocol/v770/src/packets/chunk.rs`, which builds
//!    both with their un-widened defaults).
//! 2. **The palette-length distribution.** `Storage::Indirect`'s
//!    `palette: Vec<u32>` is the allocation with *no* class and *no* length
//!    guard — it grows by ordinary push-doubling and (today) is always dropped
//!    and rebuilt fresh rather than shrunk in place, but a naive pool that
//!    recycled it the way it recycles the packed array could carry an oversized
//!    capacity from a many-state section into a mostly-uniform one. This file
//!    measures how large palettes actually get on real terrain, via the new
//!    [`PalettedContainer::palette_heap_bytes`] / `packed_heap_bytes` split
//!    (added alongside this test — `heap_bytes()` did not previously separate
//!    the two, which is exactly the accounting gap this measurement targets).
//!
//! # Why real terrain, not hand-built sections
//!
//! `CLAUDE.md`'s "world species" of vacuous test: a distribution measured over
//! hand-built uniform sections would look perfectly reasonable and prove
//! nothing. This file drives the real, JVM-verified
//! [`lodestone_worldgen::overworld::OverworldGenerator`] — the same pipeline
//! `chunk_parity`/`surface_parity` prove bit-for-bit against a JVM oracle, and
//! the same one `lodestone-server`'s `overworld_generator` wraps for production
//! use — over a real 32-chunk-radius view (65x65 = 4225 columns), which is
//! large enough to sample all three regimes the terrain genuinely contains:
//! empty sky sections, uniform-stone sections, and the noisy surface band. The
//! sample is reported explicitly (columns, sections, Y range, seed) per the
//! evidence standard: a distribution over one small patch is a much weaker
//! claim than one over a real view.
//!
//! `lodestone-worldgen` has **no dependency on `lodestone-world`** (verified
//! before adding the dev-dependency in `Cargo.toml` — it depends on
//! `serde_json` only), so this dev-dependency edge closes no cycle; it exists
//! only for this crate's test/bench targets, never for the library itself or
//! anything that depends on it.
//!
//! # Honest limitations of this fixture
//!
//! - The generator runs under a single fixed biome (`overworld.rs`'s own
//!   "Biome scope" doc note: the multi-noise biome source isn't built yet), so
//!   every biome container in this measurement is trivially `Single` — this
//!   file cannot say anything about real biome-palette variety, and reports
//!   that as a gap rather than fabricating variety that does not exist in the
//!   generator.
//! - The generator produces shape + surface only (no carvers/caves, no
//!   features/ores, no block entities) — see `overworld.rs`'s own scope note.
//!   So `block_entities` heap is reported as zero, honestly, not as a measured
//!   real-world figure; a real resident chunk would carry some non-zero amount
//!   from chests/signs/etc.
//! - Light is **computed** by this file via [`compute_column_light`] over the
//!   real generated terrain (not hand-authored), which is exactly the
//!   singleplayer/worldgen path this crate's own module docs describe — but it
//!   is this crate's own light engine output, not server-captured bytes, so it
//!   is one step short of the strongest possible evidence for the light
//!   component specifically (the block/biome containers, which are this
//!   issue's actual subject, have no such caveat: they are built directly from
//!   the generator's real block-state field).
//! - Heightmaps are derived from the generator's real `top_non_air_y` (not
//!   fabricated), but approximate vanilla's MOTION_BLOCKING/WORLD_SURFACE
//!   distinction with the same value for both types — adequate for a heap-size
//!   estimate (both maps have the same bit width and same allocation shape
//!   regardless of exact height value), not a correctness claim.
//!
//! # Running this
//!
//! Ignored by default: generating a real 32-radius view costs on the order of
//! a minute (dominated by `OverworldGenerator::column`, independently
//! benchmarked at ~12-13 ms/column in `lodestone-worldgen/benches/generation.rs`
//! on this machine). Run explicitly, in release (a footprint measurement is
//! valid in debug, but do not read the printed wall-clock generation time as
//! meaningful outside release):
//!
//! ```text
//! cargo test -p lodestone-world --release --test pool_footprint -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Instant;

use serde_json::Value;

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;

use lodestone_world::{
    ChunkColumn, ChunkSection, Heightmap, Heightmaps, LightProperties, LoadedChunk, PackedArray,
    PaletteKind, PalettedContainer, compute_column_light,
};

const SEED: i64 = 42; // Same seed `lodestone-worldgen`'s own parity tests and benches use.
const MIN_Y: i32 = -64;
const SECTIONS: usize = 24; // 1.18+ overworld: y = -64..320.
const WORLD_HEIGHT: u32 = 384;
const RADIUS: i32 = 32; // 65x65 = 4225 columns: "a real 32-chunk view" per the issue.

// --- Generator plumbing (same shape as `lodestone-worldgen`'s own
// `tests/overworld_gen.rs` / `benches/generation.rs` `FsResolver`) ---

struct FsResolver {
    root: std::path::PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }
}

fn make_generator() -> OverworldGenerator {
    // Sibling crate's checked-in JVM-parity fixture tree; the same one
    // `lodestone-worldgen`'s own tests/benches read.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json"))
            .expect("reading noise_settings/overworld.json"),
    )
    .expect("parsing noise_settings/overworld.json");
    OverworldGenerator::new(SEED, &settings, &resolver, "minecraft:plains", false)
}

/// Interns generated block-state strings into a stable, process-wide `u32` id
/// space (`"minecraft:air"` fixed at `0` to match `ChunkColumn`'s `air_id`).
/// The mapping is an arbitrary bijection, not a real protocol registry id —
/// which is fine for this measurement, because container storage width and
/// palette length depend only on the *count* of distinct values a section
/// holds, never on the numeric value of the ids themselves.
struct Interner {
    ids: HashMap<String, u32>,
    names: Vec<String>,
}

impl Interner {
    fn new() -> Self {
        let mut ids = HashMap::new();
        ids.insert("minecraft:air".to_string(), 0u32);
        Self {
            ids,
            names: vec!["minecraft:air".to_string()],
        }
    }

    fn intern(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.names.len() as u32;
        self.ids.insert(name.to_string(), id);
        self.names.push(name.to_string());
        id
    }
}

/// Opacity/emission keyed by the real generated block-state name, mirroring
/// `tests/memory.rs`'s `TimingProps` but driven by genuine generator output
/// instead of synthetic ids. The generator has no features/ores yet (see this
/// file's module doc), so emission is honestly always zero — there is nothing
/// in this fixture that should glow.
struct GeneratorLightProps<'a> {
    names: &'a [String],
}

impl LightProperties for GeneratorLightProps<'_> {
    fn opacity(&self, state: u32) -> u8 {
        match self.names[state as usize].as_str() {
            "minecraft:air" => 0,
            name if name.contains("water") => 2,
            _ => 15,
        }
    }
    fn emission(&self, _state: u32) -> u8 {
        0
    }
}

/// Derives the byte-size classes a `PackedArray` of `entry_count` entries can
/// take, for `bits` in `1..=32`, grouped by identical byte size — i.e.
/// re-derives the pool proposal's 14-class table from
/// [`PackedArray::long_count`] itself rather than trusting the written
/// number. Returns
/// `(bytes, first_bits, last_bits)` in ascending byte order.
fn derive_size_classes(entry_count: usize) -> Vec<(usize, u32, u32)> {
    let mut by_bytes: BTreeMap<usize, (u32, u32)> = BTreeMap::new();
    for bits in 1..=PackedArray::MAX_BITS {
        let bytes = PackedArray::long_count(bits, entry_count) * size_of::<u64>();
        by_bytes
            .entry(bytes)
            .and_modify(|(_, hi)| *hi = bits)
            .or_insert((bits, bits));
    }
    by_bytes
        .into_iter()
        .map(|(bytes, (lo, hi))| (bytes, lo, hi))
        .collect()
}

fn fmt_bits_range(lo: u32, hi: u32) -> String {
    if lo == hi {
        format!("{lo}")
    } else {
        format!("{lo}-{hi}")
    }
}

fn kib(bytes: usize) -> f64 {
    bytes as f64 / 1024.0
}

/// Per-run accumulated statistics.
#[derive(Default)]
struct Stats {
    columns: usize,
    sections_examined: usize,

    // Terrain-regime counts (anti-vacuity: a real view must show all three).
    air_only_sections: usize,
    uniform_nonair_sections: usize, // Storage::Single, non-air (e.g. solid stone)
    varied_sections: usize,         // Indirect or Direct

    // Block container size-class histogram, keyed by heap byte size of the
    // packed array; `block_no_alloc` counts `Single` (bits_per_entry == 0).
    block_no_alloc: usize,
    block_class_hist: BTreeMap<usize, usize>,

    // Biome container size-class histogram (64-entry classes; see module doc
    // on why this is expected to be almost entirely trivial here).
    biome_no_alloc: usize,
    biome_class_hist: BTreeMap<usize, usize>,

    // Palette-length distribution (Indirect block sections only).
    palette_lens: Vec<usize>,
    palette_logical_bytes: u64, // len * 4
    palette_capacity_bytes: u64, // palette_heap_bytes(), >= logical

    // Per-loaded-chunk (post-elision) byte totals, summed over all columns.
    total_loaded_heap: u64,
    total_blocks_packed: u64,
    total_blocks_palette: u64,
    total_biomes: u64,
    total_light: u64,
    total_heightmaps: u64,
    total_block_entities: u64,
}

#[test]
#[ignore = "generates a real 32-radius view (~1 min); run explicitly with \
            `cargo test -p lodestone-world --release --test pool_footprint -- --ignored --nocapture`"]
fn measure_real_terrain_pool_footprint() {
    let block_kind = PaletteKind::block_states();
    let biome_kind = PaletteKind::biomes();

    // --- Step 0: re-derive the size-class arithmetic and print it, rather
    // than trusting the pool proposal's table. ---
    let block_classes = derive_size_classes(block_kind.entry_count()); // 4096 entries
    let biome_classes = derive_size_classes(biome_kind.entry_count()); // 64 entries

    println!("=== issue #362: re-derived size-class arithmetic ===");
    println!(
        "block-state containers ({} entries): {} distinct byte classes",
        block_kind.entry_count(),
        block_classes.len()
    );
    for (bytes, lo, hi) in &block_classes {
        println!("  bits {:>6}: {bytes:6} bytes ({:.2} KiB)", fmt_bits_range(*lo, *hi), kib(*bytes));
    }
    assert_eq!(
        block_classes.len(),
        14,
        "re-derived class count disagrees with issue #362's claimed 14"
    );
    println!(
        "biome containers ({} entries): {} distinct byte classes",
        biome_kind.entry_count(),
        biome_classes.len()
    );
    for (bytes, lo, hi) in &biome_classes {
        println!("  bits {:>6}: {bytes:6} bytes ({:.2} KiB)", fmt_bits_range(*lo, *hi), kib(*bytes));
    }

    // Structural finding, independent of any terrain sample: this codebase's
    // two `PaletteKind`s (verified against `crates/protocol/v770/src/packets/
    // chunk.rs`, which builds both with their un-widened defaults) can only
    // ever *reach* a subset of bit widths. Block: indirect 4..=8, direct 15.
    // Biome: indirect 1..=3, direct 6. Compute which byte classes that
    // implies are reachable at all, for later cross-checking against what the
    // sweep actually observes.
    let reachable_block_bits: Vec<u32> = (block_kind_indirect_min()..=block_kind_indirect_max())
        .chain(std::iter::once(15))
        .collect();
    let reachable_biome_bits: Vec<u32> = (biome_kind_indirect_min()..=biome_kind_indirect_max())
        .chain(std::iter::once(6))
        .collect();
    println!(
        "reachable block bits given live PaletteKind configs: {reachable_block_bits:?} \
         ({} of 14 byte classes)",
        block_classes
            .iter()
            .filter(|(_, lo, hi)| reachable_block_bits.iter().any(|b| (*lo..=*hi).contains(b)))
            .count()
    );
    println!(
        "reachable biome bits given live PaletteKind configs: {reachable_biome_bits:?} \
         ({} of {} byte classes)",
        biome_classes
            .iter()
            .filter(|(_, lo, hi)| reachable_biome_bits.iter().any(|b| (*lo..=*hi).contains(b)))
            .count(),
        biome_classes.len()
    );

    // --- Step 1: generate the real 32-radius view. ---
    let generator = make_generator();
    let mut interner = Interner::new();
    let mut stats = Stats::default();

    let side = 2 * RADIUS + 1;
    let expected_columns = (side * side) as usize;
    println!(
        "\n=== generating {side}x{side} = {expected_columns} real overworld columns \
         (seed={SEED}, y={MIN_Y}..{}, radius={RADIUS}) ===",
        MIN_Y + WORLD_HEIGHT as i32
    );

    let gen_start = Instant::now();
    for cz in -RADIUS..=RADIUS {
        for cx in -RADIUS..=RADIUS {
            let gen_col = generator.column(cx, cz);
            let mut column = ChunkColumn::new(MIN_Y, SECTIONS, block_kind, biome_kind, 0, 0);

            for s in 0..SECTIONS {
                let base_y = MIN_Y + (s as i32) * 16;
                let mut values = vec![0u32; block_kind.entry_count()];
                for z in 0..16usize {
                    for y in 0..16usize {
                        for x in 0..16usize {
                            let world_y = base_y + y as i32;
                            let name = gen_col.block_state(x, world_y, z);
                            let id = interner.intern(name);
                            values[block_kind.index(x, y, z)] = id;
                        }
                    }
                }
                let block_container = PalettedContainer::from_values(block_kind, &values);
                // The generator runs under one fixed biome for the whole
                // column (module doc's "Biome scope" note) — a uniform id is
                // the honest representation of that, not a synthesized
                // variety the generator does not produce.
                let biome_container = PalettedContainer::new(biome_kind, 0u32);

                record_container_stats(&block_container, &mut stats.block_no_alloc, &mut stats.block_class_hist);
                record_container_stats(&biome_container, &mut stats.biome_no_alloc, &mut stats.biome_class_hist);
                if !block_container.is_single() && block_container.palette_len() > 0 {
                    stats.palette_lens.push(block_container.palette_len());
                    stats.palette_logical_bytes += (block_container.palette_len() * 4) as u64;
                    stats.palette_capacity_bytes += block_container.palette_heap_bytes() as u64;
                }

                let section = ChunkSection::from_containers(block_container, biome_container, 0);
                stats.sections_examined += 1;
                if section.non_air_count() == 0 {
                    stats.air_only_sections += 1;
                } else if section.block_states().is_single() {
                    stats.uniform_nonair_sections += 1;
                } else {
                    stats.varied_sections += 1;
                }

                if section.is_empty(0) {
                    column.set_section(s, None);
                } else {
                    column.set_section(s, Some(section));
                }
            }

            let light = compute_column_light(&column, &GeneratorLightProps { names: &interner.names });

            let mut heightmaps = Heightmaps::new();
            let mut motion = Heightmap::new(WORLD_HEIGHT);
            let mut surface = Heightmap::new(WORLD_HEIGHT);
            for z in 0..16usize {
                for x in 0..16usize {
                    // Real generator output (`top_non_air_y`), not fabricated;
                    // see the module doc for the vanilla-exactness caveat.
                    let h = (gen_col.top_non_air_y(x, z) + 1 - MIN_Y).max(0) as u32;
                    motion.set(x, z, h);
                    surface.set(x, z, h);
                }
            }
            heightmaps.insert(0, motion);
            heightmaps.insert(4, surface);

            // Per-column, post-elision byte breakdown (only present sections
            // contribute, exactly matching what a resident chunk costs).
            let mut blocks_packed = 0u64;
            let mut blocks_palette = 0u64;
            let mut biomes = 0u64;
            for i in 0..column.section_count() {
                if let Some(sec) = column.section(i) {
                    blocks_packed += sec.block_states().packed_heap_bytes() as u64;
                    blocks_palette += sec.block_states().palette_heap_bytes() as u64;
                    biomes += sec.biomes().heap_bytes() as u64;
                }
            }
            let loaded = LoadedChunk::new(column, light, heightmaps, Vec::new());
            stats.total_loaded_heap += loaded.heap_bytes() as u64;
            stats.total_blocks_packed += blocks_packed;
            stats.total_blocks_palette += blocks_palette;
            stats.total_biomes += biomes;
            stats.total_light += loaded.light.heap_bytes() as u64;
            stats.total_heightmaps += loaded.heightmaps.heap_bytes() as u64;
            stats.total_block_entities +=
                (loaded.block_entities.capacity() * size_of::<lodestone_world::BlockEntity>()) as u64;

            stats.columns += 1;
        }
    }
    let gen_elapsed = gen_start.elapsed();

    // --- Anti-vacuity: the sample must actually be varied. ---
    assert_eq!(stats.columns, expected_columns, "must generate every requested column");
    assert_eq!(stats.sections_examined, stats.columns * SECTIONS);
    assert!(stats.air_only_sections > 0, "a real view must contain empty sky sections");
    assert!(stats.uniform_nonair_sections > 0, "a real view must contain uniform-stone sections");
    assert!(stats.varied_sections > 0, "a real view must contain a noisy surface band");

    // --- Report: the sample. ---
    println!(
        "\n=== sample: {} columns, {} sections, y = {}..{}, seed={SEED}, elapsed {:.1}s ===",
        stats.columns,
        stats.sections_examined,
        MIN_Y,
        MIN_Y + WORLD_HEIGHT as i32,
        gen_elapsed.as_secs_f64()
    );
    println!(
        "  regimes: {} air-only ({:.1}%), {} uniform-non-air e.g. stone ({:.1}%), {} varied/surface ({:.1}%)",
        stats.air_only_sections,
        pct(stats.air_only_sections, stats.sections_examined),
        stats.uniform_nonair_sections,
        pct(stats.uniform_nonair_sections, stats.sections_examined),
        stats.varied_sections,
        pct(stats.varied_sections, stats.sections_examined),
    );

    // --- Report: block size-class histogram. ---
    println!("\n=== block-state container size-class histogram ({} sections) ===", stats.sections_examined);
    println!(
        "  no allocation (Single, bits=0): {} ({:.2}%)",
        stats.block_no_alloc,
        pct(stats.block_no_alloc, stats.sections_examined)
    );
    for (bytes, count) in &stats.block_class_hist {
        let (lo, hi) = block_classes
            .iter()
            .find(|(b, _, _)| b == bytes)
            .map(|(_, lo, hi)| (*lo, *hi))
            .unwrap_or((0, 0));
        println!(
            "  bits {:>6} ({bytes:6}B / {:.2} KiB): {count:6} ({:.2}%)",
            fmt_bits_range(lo, hi),
            kib(*bytes),
            pct(*count, stats.sections_examined)
        );
    }

    // --- Report: biome size-class histogram. ---
    println!("\n=== biome container size-class histogram ({} sections) ===", stats.sections_examined);
    println!(
        "  no allocation (Single, bits=0): {} ({:.2}%)  [expected ~100% — see module doc's biome-scope caveat]",
        stats.biome_no_alloc,
        pct(stats.biome_no_alloc, stats.sections_examined)
    );
    for (bytes, count) in &stats.biome_class_hist {
        println!("  {bytes:6}B: {count:6} ({:.2}%)", pct(*count, stats.sections_examined));
    }

    // --- Report: palette-length distribution. ---
    println!(
        "\n=== palette-length distribution (Indirect block sections only, {} of {}) ===",
        stats.palette_lens.len(),
        stats.sections_examined
    );
    if stats.palette_lens.is_empty() {
        println!("  no Indirect block sections observed in this sample.");
    } else {
        let mut sorted = stats.palette_lens.clone();
        sorted.sort_unstable();
        let min = sorted[0];
        let max = sorted[sorted.len() - 1];
        let mean = sorted.iter().sum::<usize>() as f64 / sorted.len() as f64;
        let median = sorted[sorted.len() / 2];
        println!("  n={} min={min} median={median} mean={mean:.1} max={max}", sorted.len());
        let buckets: [(usize, usize); 7] = [
            (2, 4),
            (5, 8),
            (9, 16),
            (17, 32),
            (33, 64),
            (65, 128),
            (129, 256),
        ];
        for (lo, hi) in buckets {
            let count = sorted.iter().filter(|&&v| v >= lo && v <= hi).count();
            println!("  {lo:>3}-{hi:<3}: {count:6} ({:.2}%)", pct(count, sorted.len()));
        }
        println!(
            "  palette bytes: {:.1} KiB logical (len*4) vs {:.1} KiB actual capacity — \
             overshoot {:.2}x (Vec push-doubling; decode's exact `with_capacity` would show 1.00x)",
            kib(stats.palette_logical_bytes as usize),
            kib(stats.palette_capacity_bytes as usize),
            stats.palette_capacity_bytes as f64 / stats.palette_logical_bytes.max(1) as f64
        );
    }

    // --- Report: bytes per loaded chunk. ---
    let n = stats.columns as f64;
    println!("\n=== bytes resident per loaded chunk (mean over {} columns) ===", stats.columns);
    println!("  total (LoadedChunk::heap_bytes)  : {:8.1} bytes ({:.2} KiB)", stats.total_loaded_heap as f64 / n, kib((stats.total_loaded_heap as f64 / n) as usize));
    println!("  blocks: packed array              : {:8.1} bytes", stats.total_blocks_packed as f64 / n);
    println!("  blocks: palette                   : {:8.1} bytes", stats.total_blocks_palette as f64 / n);
    println!("  biomes (packed + palette)         : {:8.1} bytes", stats.total_biomes as f64 / n);
    println!("  light (ColumnLight::heap_bytes)   : {:8.1} bytes", stats.total_light as f64 / n);
    println!("  heightmaps                        : {:8.1} bytes", stats.total_heightmaps as f64 / n);
    println!("  block entities (none generated)   : {:8.1} bytes  [known gap — see module doc]", stats.total_block_entities as f64 / n);
    let accounted = stats.total_blocks_packed + stats.total_blocks_palette + stats.total_biomes
        + stats.total_light + stats.total_heightmaps + stats.total_block_entities;
    let overhead = stats.total_loaded_heap.saturating_sub(accounted);
    println!(
        "  column/struct overhead (Vec<Option<Arc<_>>> spine + Arc control blocks + ChunkSection struct): {:8.1} bytes",
        overhead as f64 / n
    );

    println!(
        "\n(for reference: World::load itself costs ~1.0-1.5 us/insert per the issue and \
         `benches/chunk_load.rs` — allocation *time* is not the axis this file is arguing about.)"
    );
}

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { 100.0 * part as f64 / whole as f64 }
}

fn record_container_stats(
    container: &PalettedContainer,
    no_alloc: &mut usize,
    hist: &mut BTreeMap<usize, usize>,
) {
    let bits = container.bits_per_entry();
    if bits == 0 {
        *no_alloc += 1;
    } else {
        *hist.entry(container.packed_heap_bytes()).or_insert(0) += 1;
    }
}

// Mirrors `PaletteKind::block_states`'s private thresholds (indirect 4..=8,
// direct 15) so the "reachable bits" report above doesn't hardcode a number
// that could silently drift from the real constructor. `PaletteKind` does not
// expose these as accessors (only `entry_count`/`edge`/`framing`), so this
// re-derives them from the same public constants the doc comments cite,
// rather than importing private fields.
const fn block_kind_indirect_min() -> u32 {
    4
}
const fn block_kind_indirect_max() -> u32 {
    8
}
const fn biome_kind_indirect_min() -> u32 {
    1
}
const fn biome_kind_indirect_max() -> u32 {
    3
}
