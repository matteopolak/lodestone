//! External controls for the Nether target-completion boundary.

use lodestone_server::nether_chunk_source;

const TARGET: (i32, i32) = (0, 1);
const WORLD: (i32, i32, i32) = (2, 53, 28);

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
fn target_features_match_external_task_body() {
    const TARGET: (i32, i32) = (381, 380);
    const EXTERNAL_ROOTS: [(i32, i32, i32); 24] = [
        (3, 56, 6),
        (2, 56, 7),
        (0, 56, 8),
        (1, 56, 8),
        (2, 56, 8),
        (0, 56, 9),
        (4, 56, 9),
        (1, 56, 10),
        (3, 56, 11),
        (2, 56, 13),
        (1, 67, 3),
        (2, 68, 3),
        (3, 69, 1),
        (4, 70, 1),
        (4, 70, 2),
        (5, 71, 1),
        (5, 71, 2),
        (13, 77, 11),
        (15, 77, 12),
        (11, 77, 13),
        (10, 77, 14),
        (8, 77, 15),
        (11, 77, 15),
        (13, 80, 3),
    ];
    let source = nether_chunk_source(42);
    let spills = source
        .generator()
        .parity_target_spills_with_resident(TARGET.0, TARGET.1, &[], |_, _| None);
    let roots = spills
        .iter()
        .filter(|spill| {
            spill.state == "minecraft:crimson_roots"
                && (TARGET.0 * 16..TARGET.0 * 16 + 16).contains(&spill.position.0)
                && (TARGET.1 * 16..TARGET.1 * 16 + 16).contains(&spill.position.2)
        })
        .map(|spill| {
            (
                spill.position.0 - TARGET.0 * 16,
                spill.position.1,
                spill.position.2 - TARGET.1 * 16,
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let expected = EXTERNAL_ROOTS.into_iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(roots, expected, "target FEATURES writes differ from the captured task body");
    assert_eq!(roots.len(), 24);
    assert!(!roots.contains(&(15, 77, 5)), "the later east-neighbour spill is not target-owned");

    let target_replay = source
        .generator()
        .parity_source_spills_with_overrides(TARGET.0, TARGET.1, TARGET.0, TARGET.1, &[]);
    let replay_roots = target_replay
        .iter()
        .filter(|spill| {
            spill.state == "minecraft:crimson_roots"
                && (TARGET.0 * 16..TARGET.0 * 16 + 16).contains(&spill.position.0)
                && (TARGET.1 * 16..TARGET.1 * 16 + 16).contains(&spill.position.2)
        })
        .map(|spill| {
            (
                spill.position.0 - TARGET.0 * 16,
                spill.position.1,
                spill.position.2 - TARGET.1 * 16,
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(replay_roots, expected, "source replay must share the authenticated target body");

    for (label, source_x, source_z) in [("dependency", 381, 379), ("future", 382, 380)] {
        let filtered = source.generator().parity_source_spills_with_overrides(
            TARGET.0,
            TARGET.1,
            source_x,
            source_z,
            &[],
        );
        let roots = filtered
            .iter()
            .filter(|spill| {
                spill.state == "minecraft:crimson_roots"
                    && (TARGET.0 * 16..TARGET.0 * 16 + 16).contains(&spill.position.0)
                    && (TARGET.1 * 16..TARGET.1 * 16 + 16).contains(&spill.position.2)
            })
            .map(|spill| {
                (
                    spill.position.0 - TARGET.0 * 16,
                    spill.position.1,
                    spill.position.2 - TARGET.1 * 16,
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert!(roots.is_disjoint(&expected), "{label} source replay claimed target-owned roots: {roots:?}");
    }
}
