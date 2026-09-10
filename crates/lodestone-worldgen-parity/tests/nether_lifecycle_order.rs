//! External controls for the Nether source-completion boundary.
//!
//! The target coordinate is deliberately the small fortress/support seam
//! captured from the seed-42 lifecycle probe: a source in `(-1, 1)` can place a
//! brown mushroom at world `(2, 53, 28)`, while the target `(0, 1)` owns the
//! fortress foundation below it.

use lodestone_server::nether_chunk_source;
use lodestone_worldgen_parity::lifecycle::{LifecycleCompletion, LifecycleMaterializer};

const TARGET: (i32, i32) = (0, 1);
const SOURCE: (i32, i32) = (-1, 1);
const WORLD: (i32, i32, i32) = (2, 53, 28);

#[test]
fn neighbor_mushroom_spill_precedes_target_fortress_support() {
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    materializer.admit(SOURCE);
    materializer.admit(TARGET);

    let mut observed = Vec::new();
    materializer.complete_observing(SOURCE, LifecycleCompletion::Features, 0, |spill| {
        if spill.position == WORLD {
            observed.push((spill.source, spill.state.clone()));
        }
    });
    assert_eq!(
        observed,
        vec![(SOURCE, "minecraft:brown_mushroom".to_owned())],
        "the external source control must reach the target before its structure completes",
    );

    let mut target_observed = Vec::new();
    materializer.complete_observing(TARGET, LifecycleCompletion::Features, 1, |spill| {
        if spill.position.0 == WORLD.0 && spill.position.2 == WORLD.2 && (53..=54).contains(&spill.position.1) {
            target_observed.push((spill.position.1, spill.state.clone()));
        }
    });

    let target = materializer
        .resident_column(TARGET)
        .expect("target was admitted before completion");
    assert_eq!(target.block_state(2, 53, 12), "minecraft:brown_mushroom");
    assert_eq!(target.block_state(2, 54, 12), "minecraft:nether_bricks");
    assert!(
        target_observed.contains(&(54, "minecraft:nether_bricks".to_owned())),
        "target completion must emit the first support above the mushroom: {observed:?}",
    );
    assert!(
        !target_observed.contains(&(53, "minecraft:nether_bricks".to_owned())),
        "the support pass must not overwrite the neighbour's mushroom",
    );
}

#[test]
fn air_control_still_places_the_foundation_at_the_target() {
    let source = nether_chunk_source(42);
    let air = vec![(WORLD.0, WORLD.1, WORLD.2, "minecraft:cave_air".to_owned())];
    let spills = source
        .generator()
        .parity_source_spills_with_overrides(TARGET.0, TARGET.1, TARGET.0, TARGET.1, &air);

    assert!(
        spills.iter().any(|spill| spill.position == WORLD && spill.state == "minecraft:nether_bricks"),
        "air control must allow the cached fortress support to occupy y=53",
    );
}

#[test]
fn full_nether_dispatch_preserves_external_blackstone_witnesses() {
    let source = nether_chunk_source(42);
    for &(chunk_x, chunk_z, local_x, y, local_z) in &[
        (96, 96, 0, 12, 15),
        (96, 95, 13, 3, 2),
    ] {
        let column = source.generator().column(chunk_x, chunk_z);
        let state = column.block_state(local_x, y, local_z);
        assert_eq!(
            state,
            "minecraft:blackstone",
            "full x-major Nether dispatch changed witness at chunk ({chunk_x},{chunk_z}) local ({local_x},{y},{local_z})",
        );
    }
}

#[test]
fn resident_completion_order_is_observable_in_full_and_split_dispatchers() {
    const TARGET_CHUNK: (i32, i32) = (5, 3);
    const MAGMA_SOURCE: (i32, i32) = (6, 3);
    const GRAVEL_SOURCE: (i32, i32) = (5, 4);
    const WORLD: (i32, i32, i32) = (95, 32, 63);

    let cold = nether_chunk_source(42)
        .generator()
        .column(TARGET_CHUNK.0, TARGET_CHUNK.1);
    assert_eq!(
        cold.block_state(15, 32, 15),
        "minecraft:gravel",
        "the full mixed source dispatcher must use x-major replacement order",
    );

    let mut canonical = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 2..=4 {
        for x in 4..=6 {
            canonical.admit((x, z));
        }
    }
    let mut observed = Vec::new();
    canonical.complete_observing(
        MAGMA_SOURCE,
        LifecycleCompletion::Features,
        0,
        |spill| {
            if spill.position == WORLD {
                observed.push((spill.source, spill.state.clone()));
            }
        },
    );
    canonical.complete_observing(
        GRAVEL_SOURCE,
        LifecycleCompletion::Features,
        1,
        |spill| {
            if spill.position == WORLD {
                observed.push((spill.source, spill.state.clone()));
            }
        },
    );
    assert_eq!(
        observed,
        vec![(MAGMA_SOURCE, "minecraft:magma_block".to_owned())],
        "an explicit magma-first lifecycle completion must expose magma before gravel reads the resident target",
    );
    assert_eq!(
        canonical
            .resident_column(TARGET_CHUNK)
            .expect("target was admitted")
            .block_state(15, 32, 15),
        "minecraft:magma_block",
    );

    let mut reversed = LifecycleMaterializer::new(nether_chunk_source(42));
    for z in 2..=4 {
        for x in 4..=6 {
            reversed.admit((x, z));
        }
    }
    reversed.complete(GRAVEL_SOURCE, LifecycleCompletion::Features, 0);
    reversed.complete(MAGMA_SOURCE, LifecycleCompletion::Features, 1);
    assert_eq!(
        reversed
            .resident_column(TARGET_CHUNK)
            .expect("target was admitted")
            .block_state(15, 32, 15),
        "minecraft:gravel",
        "reversing the two source completions must retain the first accepted replacement",
    );
}

#[test]
fn later_source_reads_the_prior_sources_resident_replacement() {
    const TARGET: (i32, i32) = (93, 90);
    const PRIOR_SOURCE: (i32, i32) = (92, 90);
    const LATER_SOURCE: (i32, i32) = (94, 90);
    const WORLD: (i32, i32, i32) = (1503, 23, 1441);

    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    for x in 91..=95 {
        for z in 88..=92 {
            materializer.admit((x, z));
        }
    }
    materializer.complete(PRIOR_SOURCE, LifecycleCompletion::Features, 0);
    let before = materializer
        .resident_column(TARGET)
        .expect("target was admitted before prior completion")
        .block_state(WORLD.0.rem_euclid(16), WORLD.1, WORLD.2.rem_euclid(16))
        .to_owned();
    materializer.complete(LATER_SOURCE, LifecycleCompletion::Features, 1);
    let after = materializer
        .resident_column(TARGET)
        .expect("target remains resident after later completion")
        .block_state(WORLD.0.rem_euclid(16), WORLD.1, WORLD.2.rem_euclid(16));

    assert_eq!(before, "minecraft:basalt", "the control source must install basalt first");
    assert_eq!(
        after,
        "minecraft:basalt",
        "the later blackstone blob must read and preserve the resident basalt replacement",
    );
}
