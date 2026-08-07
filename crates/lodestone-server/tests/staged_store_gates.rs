//! Unit 6's determinism gates for the staged sharded store: a whole 12×12 sweep
//! byte-identical across independently constructed generators, and the **289-column
//! join burst** that forced `lodestone-server`'s per-ring barrier back in
//! (`4307b59`) producing byte-identical terrain concurrently.
//!
//! # Why these are separate from `staged_store_counters.rs`
//!
//! Because the counters are process-global atomics and these tests generate
//! terrain freely. Sharing a binary with the counter gate made it read
//! `pre_ore_computed = 502` against a true 256 under `--test-threads=2`. Nothing
//! here reads a counter, and nothing in that file may generate. Keep it that way.
//!
//! # Running them
//!
//! `#[ignore]`d: together these generate ~1,000 real embedded-data columns, minutes
//! in release and hours in debug. The always-on protection for the store's own
//! invariants is `overworld::store`'s unit-test module — including a negative
//! control that observes the *old* FIFO-cache shape recomputing under the same
//! race — plus the 13 worldgen parity binaries and
//! `column_is_byte_identical_across_two_independently_constructed_generators`,
//! which all run in the default suite.
//!
//! ```text
//! cargo test --release -p lodestone-server --test staged_store_gates -- --ignored --nocapture
//! ```

use lodestone_server::{GeneratedColumn, overworld_generator};

/// Sweep extent, matching the counter gate's.
const SWEEP: i32 = 12;

type ColumnBytes = (i32, i32, Vec<String>, Vec<u16>, Vec<String>);

fn raw(col: GeneratedColumn) -> ColumnBytes {
    let (min_y, height, palette, blocks, biomes) = col.into_raw();
    (min_y, height, palette, blocks, biomes.to_vec())
}

/// Determinism across the store: a second, independently constructed generator
/// reproduces a whole 12×12 sweep byte for byte.
///
/// This is `column_is_byte_identical_across_two_independently_constructed_generators`
/// widened from 6 chunks to 144, and it is the gate aimed at the hazards a
/// *sharded* store introduces specifically: a value published into the wrong
/// entry, or a shard hash that let two positions collide as keys rather than
/// merely as shards. Byte-level, so a palette **order** difference fails it too —
/// the failure mode `overworld/mod.rs`'s own `RandomState` post-mortem records, and
/// the one interning made it possible to reintroduce.
#[test]
#[ignore = "144 columns x2 of real embedded-data generation; minutes in release"]
fn the_whole_12x12_sweep_is_byte_identical_on_a_second_generator() {
    let first = overworld_generator(42);
    let second = overworld_generator(42);
    for cx in 0..SWEEP {
        for cz in 0..SWEEP {
            let a = raw(first.column(cx, cz));
            let b = raw(second.column(cx, cz));
            assert_eq!(a.0, b.0, "chunk ({cx},{cz}) min_y differs");
            assert_eq!(a.1, b.1, "chunk ({cx},{cz}) height differs");
            assert_eq!(
                a.2, b.2,
                "chunk ({cx},{cz}) palette differs between two independently constructed \
                 generators — palette assignment order reached the output"
            );
            assert_eq!(a.3, b.3, "chunk ({cx},{cz}) block indices differ");
            assert_eq!(a.4, b.4, "chunk ({cx},{cz}) biome quarts differ");
        }
    }
    assert_eq!(
        first.store_evictions(),
        0,
        "a 12x12 sweep closes over 16x16 = 256 chunks, well inside STORE_RETENTION"
    );
}

/// The D4 scenario itself: 289 columns generated concurrently through **one**
/// shared generator must match the serial answer byte for byte, with nothing
/// evicted.
///
/// 289 = 17×17, the burst named in `4307b59` — *"cache contention with 289
/// concurrent generator calls"*. It is the exact shape that produced ~5,000
/// attempts on a single `Arc<Mutex>`, and it is where a sharded store's own
/// failure modes would surface: a lost publication, a deadlock on the per-entry
/// once-guard (the store's layering rule — a worker that never joins fails this
/// test rather than hanging silently), or eviction firing mid-burst.
///
/// The eviction assertion is derived, not hopeful: 289 columns close over
/// 21×21 = 441 chunks and `STORE_RETENTION` is set from exactly that number.
#[test]
#[ignore = "289 columns x2 of real embedded-data generation; minutes in release"]
fn a_289_column_concurrent_burst_matches_serial_bytes_with_no_eviction() {
    const R: i32 = 8; // 17x17 = 289
    // Offset away from the origin so this shares no chunk with the sweep above,
    // keeping the two tests independent even when run in the same process.
    let coords: Vec<(i32, i32)> = (-R..=R)
        .flat_map(|cx| (-R..=R).map(move |cz| (cx + 500, cz + 500)))
        .collect();
    assert_eq!(coords.len(), 289);

    let serial = overworld_generator(42);
    let expected: Vec<ColumnBytes> = coords
        .iter()
        .map(|&(cx, cz)| raw(serial.column(cx, cz)))
        .collect();

    let parallel = std::sync::Arc::new(overworld_generator(42));
    let chunk_size = coords.len().div_ceil(8);
    let mut handles = Vec::new();
    for slice in coords.chunks(chunk_size) {
        let slice: Vec<(i32, i32)> = slice.to_vec();
        let generator = std::sync::Arc::clone(&parallel);
        handles.push(std::thread::spawn(move || {
            slice
                .into_iter()
                .map(|(cx, cz)| ((cx, cz), raw(generator.column(cx, cz))))
                .collect::<Vec<_>>()
        }));
    }
    let mut got: Vec<((i32, i32), ColumnBytes)> = Vec::new();
    for h in handles {
        got.extend(
            h.join()
                .expect("a burst worker panicked — a deadlock on the once-guard, or a poisoned lock"),
        );
    }
    assert_eq!(got.len(), 289);

    let index: std::collections::HashMap<(i32, i32), &ColumnBytes> =
        got.iter().map(|(pos, bytes)| (*pos, bytes)).collect();
    for (&(cx, cz), want) in coords.iter().zip(expected.iter()) {
        let have = index
            .get(&(cx, cz))
            .unwrap_or_else(|| panic!("chunk ({cx},{cz}) missing from the parallel burst"));
        assert_eq!(
            *have, want,
            "chunk ({cx},{cz}) differs between a 289-column concurrent burst and the serial \
             answer — the store published a wrong or partial value"
        );
    }
    assert_eq!(
        parallel.store_evictions(),
        0,
        "the burst closes over 21x21 = 441 chunks and STORE_RETENTION is derived from that, \
         so nothing may be evicted mid-burst"
    );
}
