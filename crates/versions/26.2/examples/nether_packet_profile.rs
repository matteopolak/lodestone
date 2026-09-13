//! One-target packet comparator profile for the Nether source.
//!
//! This is intentionally a read-only probe: it reports the source stages that
//! run before the packet hash, then compares the nine-column neighbour-aware
//! encoding sequence with and without replay cache preparation.

use std::time::Instant;

use lodestone_server::{ChunkSource, ServerDirective, ServerProtocol, nether_chunk_source};
use lodestone_server::dimension::Dimension;
use lodestone_v26_2::V770ServerProtocol;

fn report(label: &str, started: Instant) {
    println!("{label}: {:.3}s", started.elapsed().as_secs_f64());
}

fn packet_for(source: &impl ChunkSource) -> Vec<u8> {
    let center = source.column(0, 0);
    let mut neighbours = Vec::with_capacity(8);
    for dz in -1..=1 {
        for dx in -1..=1 {
            if (dx, dz) == (0, 0) {
                continue;
            }
            neighbours.push((dx, dz, source.column(dx, dz)));
        }
    }
    match V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(0, 0, &center, &neighbours, Dimension::Nether)
        .expect("neighbour-aware chunk encoding")
    {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("unexpected chunk directive: {other:?}"),
    }
}

fn main() {
    let started = Instant::now();
    let source = nether_chunk_source(42);
    report("construct Nether source", started);

    let started = Instant::now();
    let _shaped = source.generator().column_shaped(0, 0);
    report("direct shaped (prefix) (0,0)", started);

    let source = nether_chunk_source(42);
    let started = Instant::now();
    let _full = source.generator().column(0, 0);
    report("direct full (0,0)", started);

    let source = nether_chunk_source(42);
    let started = Instant::now();
    let _served = source.column(0, 0);
    report("served source full plus attachment (0,0)", started);

    let source = nether_chunk_source(42);
    let started = Instant::now();
    let baseline = packet_for(&source);
    report("unprepared direct packet (9 columns)", started);
    println!(
        "unprepared pre-decoration computations={}, evictions={}",
        source.generator().pre_decoration_computations(),
        source.generator().pre_decoration_evictions(),
    );

    let prepared = nether_chunk_source(42);
    let capacity = prepared.generator().prepare_packet_replay(&[(0, 0)]);
    println!("prepared pre-decoration capacity={capacity}");
    let started = Instant::now();
    let optimized = packet_for(&prepared);
    report("prepared direct packet (9 columns)", started);
    println!(
        "prepared pre-decoration computations={}, evictions={}",
        prepared.generator().pre_decoration_computations(),
        prepared.generator().pre_decoration_evictions(),
    );
    println!("packet bytes identical={}", baseline == optimized);
}
