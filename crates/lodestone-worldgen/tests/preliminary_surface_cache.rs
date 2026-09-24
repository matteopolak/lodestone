//! Diagnostic control for preliminary-surface work shared by adjacent chunks.
//!
//! The pre-cache discriminator recorded 28,675 requests for the seed-42 ring.
//! The final gate keeps the same ring under one request lease and asserts that
//! shared work is reused while preserving chunk fingerprints.

#![cfg(feature = "gen-counters")]

use lodestone_worldgen::counters;
use lodestone_worldgen::overworld::GeneratedColumn;
use sha2::{Digest, Sha256};

const SEED: i64 = 42;

#[test]
fn adjacent_ring_deduplicates_preliminary_surface_work() {
    let arm = std::env::var("PRELIM_CACHE_ARM").ok();
    let coords: Vec<_> = (-1..=1)
        .flat_map(|cz| (-1..=1).map(move |cx| (cx, cz)))
        .collect();

    let (scalar_columns, scalar_snapshot, scalar_region_stats) = if arm.as_deref() == Some("batch") {
        (None, None, None)
    } else {
        let scalar_generator = lodestone_server::overworld_generator(SEED);
        counters::reset();
        let columns = coords
            .iter()
            .map(|&(cx, cz)| {
                let column = scalar_generator.column_shaped(cx, cz);
                std::hint::black_box(column.non_air_count());
                column
            })
            .collect::<Vec<_>>();
        (
            Some(columns),
            Some(counters::snapshot()),
            Some(scalar_generator.preliminary_region_cache_stats()),
        )
    };
    if arm.as_deref() == Some("scalar") {
        let snapshot = scalar_snapshot.expect("scalar arm snapshot");
        let region = scalar_region_stats.expect("scalar arm region stats");
        assert_eq!(
            region.capacity,
            8_192,
        );
        assert!(
            region.retained_entries > 0,
            "scalar columns must consume the generator region cache"
        );
        println!(
            "preliminary scalar ring: requests={} computations={} region={region:?}",
            snapshot.preliminary_surface_requests,
            snapshot.preliminary_surface_computations,
        );
        return;
    }

    let generator = lodestone_server::overworld_generator(SEED);
    counters::reset();
    let lease = generator.lease_batch(&coords);
    let columns = {
        let lease = &lease;
        coords
            .iter()
            .map(|&(cx, cz)| {
                let column = lease.column_shaped(cx, cz);
                std::hint::black_box(column.non_air_count());
                column
            })
            .collect::<Vec<_>>()
    };
    let cache_stats = lease.preliminary_cache_stats();
    let region_stats = generator.preliminary_region_cache_stats();
    drop(lease);
    let snapshot = counters::snapshot();
    assert_eq!(snapshot.preliminary_surface_requests, 28_675);
    assert!(
        snapshot.preliminary_surface_requests > snapshot.preliminary_surface_unique,
        "the adjacent ring must reuse snapped coordinates: {snapshot:?}"
    );
    assert_eq!(
        snapshot.preliminary_surface_computations,
        snapshot.preliminary_surface_unique,
        "the unique-work diagnostic must track computed values: {snapshot:?}"
    );
    if arm.as_deref() == Some("batch") {
        println!(
            "preliminary batch ring: requests={} computations={} region={region_stats:?} cache={cache_stats:?}",
            snapshot.preliminary_surface_requests,
            snapshot.preliminary_surface_computations,
        );
        return;
    }
    let scalar_fingerprints = scalar_columns.map(|columns| {
        coords
            .iter()
            .zip(columns.iter())
            .map(|(&(cx, cz), column)| (cx, cz, fingerprint(column)))
            .collect::<Vec<_>>()
    });
    let fingerprints = coords
        .iter()
        .zip(columns.iter())
        .map(|(&(cx, cz), column)| (cx, cz, fingerprint(column)))
        .collect::<Vec<_>>();
    if let Some(scalar_fingerprints) = scalar_fingerprints {
        assert_eq!(fingerprints, scalar_fingerprints, "scalar and batch fingerprints differ");
    }

    let translated = lodestone_server::overworld_generator(SEED);
    for &(cx, cz, expected) in &fingerprints {
        let actual_column = translated.column_shaped(cx, cz);
        let actual = fingerprint(&actual_column);
        assert_eq!(actual, expected, "chunk fingerprint changed at ({cx},{cz})");
        if cx == 0 && cz == 0 {
            assert_columns_equal(
                &actual_column,
                &generator.column_shaped(cx, cz),
                (cx, cz),
            );
        }
    }

    counters::reset();
    std::hint::black_box(generator.column_shaped(1_024, -1_024).non_air_count());
    let translated_snapshot = counters::snapshot();
    assert!(translated_snapshot.preliminary_surface_requests > 0);
    assert!(translated_snapshot.preliminary_surface_unique > 0);
    assert_eq!(
        translated_snapshot.preliminary_surface_computations,
        translated_snapshot.preliminary_surface_unique,
        "translated negative coordinates must not alias existing cache keys: {translated_snapshot:?}"
    );
    println!(
        "preliminary ring: scalar requests={} computations={} region={scalar_region_stats:?}; batch requests={} computations={} region={region_stats:?} cache={cache_stats:?}",
        scalar_snapshot.as_ref().map_or(0, |snapshot| snapshot.preliminary_surface_requests),
        scalar_snapshot.as_ref().map_or(0, |snapshot| snapshot.preliminary_surface_computations),
        snapshot.preliminary_surface_requests,
        snapshot.preliminary_surface_computations,
    );
}

fn fingerprint(column: &GeneratedColumn) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(column.min_y().to_le_bytes());
    hash.update(column.height().to_le_bytes());
    for y in column.min_y()..column.min_y() + column.height() {
        for lz in 0..16 {
            for lx in 0..16 {
                hash.update(column.block_state_id(lx, y, lz).raw().to_le_bytes());
            }
        }
    }
    hash.finalize().into()
}

fn assert_columns_equal(a: &GeneratedColumn, b: &GeneratedColumn, chunk: (i32, i32)) {
    assert_eq!(a.min_y(), b.min_y(), "min y changed at {chunk:?}");
    assert_eq!(a.height(), b.height(), "height changed at {chunk:?}");
    for y in a.min_y()..a.min_y() + a.height() {
        for lz in 0..16 {
            for lx in 0..16 {
                assert_eq!(
                    a.block_state_id(lx, y, lz),
                    b.block_state_id(lx, y, lz),
                    "block changed at chunk {chunk:?}, local ({lx},{y},{lz})"
                );
            }
        }
    }
}
