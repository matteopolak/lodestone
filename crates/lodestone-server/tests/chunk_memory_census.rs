//! Resident-column memory census over real generated terrain.
//!
//! This is an ignored measurement rather than a normal test because generating
//! hundreds of columns is intentionally expensive. It retains the columns in a
//! vector while reporting direct logical capacities, then drops them before the
//! next dimension. The numbers therefore cover the actual Overworld, Nether,
//! and End source representations instead of a hand-authored two-state shape.
//!
//! Run in release mode with `--ignored --nocapture`. Set
//! `LODESTONE_MEMORY_CENSUS_COLUMNS` to a smaller value while iterating; the
//! default is 256 columns per dimension.

use std::env;

use lodestone_server::{
    ChunkColumn, ChunkColumnMemory, ChunkSource, end_chunk_source, nether_chunk_source,
    overworld_chunk_source,
};

const SEED: i64 = 0x5EED_1234;
const DEFAULT_COLUMNS: usize = 256;

fn sample_coordinates(count: usize) -> impl Iterator<Item = (i32, i32)> {
    let side = (count as f64).sqrt().ceil() as i32;
    (0..count).map(move |index| {
        let index = index as i32;
        (index % side - side / 2, index / side - side / 2)
    })
}

fn add(a: &mut ChunkColumnMemory, b: ChunkColumnMemory) {
    a.inline_bytes += b.inline_bytes;
    a.blocks_bytes += b.blocks_bytes;
    a.block_palette_slots_bytes += b.block_palette_slots_bytes;
    a.block_palette_text_bytes += b.block_palette_text_bytes;
    a.block_derived_bytes += b.block_derived_bytes;
    a.custom_arc_payload_bytes += b.custom_arc_payload_bytes;
    a.section_ticking_bytes += b.section_ticking_bytes;
    a.biome_surface_text_bytes += b.biome_surface_text_bytes;
    a.biome_palette_slots_bytes += b.biome_palette_slots_bytes;
    a.biome_palette_text_bytes += b.biome_palette_text_bytes;
    a.biome_cells_bytes += b.biome_cells_bytes;
    a.block_entities_slots_bytes += b.block_entities_slots_bytes;
    a.structure_slots_bytes += b.structure_slots_bytes;
    a.structure_refs_bytes += b.structure_refs_bytes;
    a.motion_blocking_bytes += b.motion_blocking_bytes;
    a.generation_spawns_bytes += b.generation_spawns_bytes;
    a.retained_light_bytes += b.retained_light_bytes;
}

fn print_census(label: &str, columns: &[ChunkColumn]) {
    let mut total = ChunkColumnMemory::default();
    let mut max = 0usize;
    for column in columns {
        let census = column.memory_census();
        max = max.max(census.logical_total());
        add(&mut total, census);
    }
    let count = columns.len();
    let mean = total.logical_total() / count.max(1);
    println!(
        "{label}: {count} retained columns, logical total {} MiB, mean {} bytes, max {} bytes",
        total.logical_total() as f64 / (1024.0 * 1024.0),
        mean,
        max
    );
    println!(
        "  blocks={} KiB, block palette slots/text/derived={} / {} / {} KiB, custom arcs={} KiB",
        total.blocks_bytes / 1024,
        total.block_palette_slots_bytes / 1024,
        total.block_palette_text_bytes / 1024,
        total.block_derived_bytes / 1024,
        total.custom_arc_payload_bytes / 1024
    );
    println!(
        "  biome surface/palette slots/text/cells={} / {} / {} / {} KiB, light={} KiB",
        total.biome_surface_text_bytes / 1024,
        total.biome_palette_slots_bytes / 1024,
        total.biome_palette_text_bytes / 1024,
        total.biome_cells_bytes / 1024,
        total.retained_light_bytes / 1024
    );
    assert!(
        total.blocks_bytes > 0,
        "{label} census is empty: the source did not produce resident block storage"
    );
}

#[test]
#[ignore = "real 256-column, three-dimension memory census; run explicitly in release"]
fn census_real_loaded_columns_in_all_dimensions() {
    let count = env::var("LODESTONE_MEMORY_CENSUS_COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_COLUMNS);
    assert!(count > 0, "census column count must be positive");
    let coords: Vec<_> = sample_coordinates(count).collect();

    let overworld_source = overworld_chunk_source(SEED);
    let overworld: Vec<_> = coords
        .iter()
        .map(|&(cx, cz)| overworld_source.column(cx, cz))
        .collect();
    print_census("overworld", &overworld);

    let nether_source = nether_chunk_source(SEED);
    let nether: Vec<_> = coords
        .iter()
        .map(|&(cx, cz)| nether_source.column(cx, cz))
        .collect();
    print_census("nether", &nether);

    let end_source = end_chunk_source(SEED);
    let end: Vec<_> = coords
        .iter()
        .map(|&(cx, cz)| end_source.column(cx, cz))
        .collect();
    print_census("end", &end);
}
