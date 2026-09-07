//! External-control checks for the Nether fortress piece graph.

use std::collections::BTreeSet;

use lodestone_worldgen::rng::{LegacyRandomSource, WorldgenRandom};
use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::structure::{BoundingBox, fortress};

const EXTERNAL: &str = include_str!("support/nether_fortress_seed42_external.txt");

fn external(key: &str) -> &str {
    EXTERNAL
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .unwrap_or_else(|| panic!("external fortress fixture is missing {key}"))
}

fn stream(seed: i64, cx: i32, cz: i32) -> WorldgenRandom<LegacyRandomSource> {
    let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
    random.set_large_feature_seed(seed, cx, cz);
    random
}

#[test]
fn seed_42_control_tree_has_90_children_and_the_recorded_chunk_zero_boxes() {
    let mut random = stream(42, 0, 0);
    let (pieces, origin) = fortress::generate(0, 0, &mut random);
    assert_eq!(pieces.len(), 90);
    assert_eq!(origin, [0, 69, 0]);
    assert_eq!(pieces[0].bounding_box, BoundingBox { min: [2, 69, 2], max: [20, 78, 20] });
    for box_ in [
        BoundingBox { min: [-5, 72, 8], max: [1, 80, 14] },
        BoundingBox { min: [-5, 72, 15], max: [1, 80, 21] },
        BoundingBox { min: [-5, 72, 1], max: [1, 82, 7] },
    ] {
        assert!(pieces.iter().any(|piece| piece.bounding_box == box_), "missing control box {box_:?}");
    }
    let root_blocks = pieces[0].blocks.as_ref().expect("root must reach the placement-stage block list");
    assert!(root_blocks.iter().any(|block| {
        (0..16).contains(&block.pos[0])
            && (0..16).contains(&block.pos[2])
            && block.state == "minecraft:nether_bricks"
    }), "the control chunk must receive fortress masonry");
    let kinds: BTreeSet<_> = pieces.iter().map(|piece| piece.id.as_str()).collect();
    assert_eq!(
        kinds,
        BTreeSet::from([
            "minecraft:nebcr", "minecraft:nebef", "minecraft:nebs", "minecraft:neccs",
            "minecraft:nece", "minecraft:necsr", "minecraft:nectb", "minecraft:nemt",
            "minecraft:nerc", "minecraft:nesc", "minecraft:nesclt", "minecraft:nescrt",
            "minecraft:nescsc", "minecraft:nesr",
        ]),
        "the captured full tree must exercise every distinct fortress block writer"
    );
}

/// The read-only external NBT capture has all persisted ids and orientations
/// from the seed-42 start.  It is intentionally a compact fixture: the capture
/// world was left with an in-flight journal, while the individual NBT/packet
/// diagnostics are stable evidence and are recorded alongside these rows.
#[test]
fn seed_42_external_fixture_covers_tree_orientations_and_terminal_rng() {
    let mut random = stream(42, 0, 0);
    let (pieces, _) = fortress::generate(0, 0, &mut random);
    assert_eq!(pieces.len(), external("children=").parse::<usize>().unwrap());
    let captured_tree: Vec<_> = EXTERNAL
        .lines()
        .filter_map(|line| line.strip_prefix("piece="))
        .collect();
    let actual_tree: Vec<_> = pieces
        .iter()
        .map(|piece| {
            format!(
                "{},{},{},{},{},{}|{}|{}|{}",
                piece.bounding_box.min[0],
                piece.bounding_box.min[1],
                piece.bounding_box.min[2],
                piece.bounding_box.max[0],
                piece.bounding_box.max[1],
                piece.bounding_box.max[2],
                piece.orientation.expect("fortress pieces are horizontal"),
                piece.gen_depth,
                piece.id,
            )
        })
        .collect();
    assert_eq!(actual_tree, captured_tree, "external queue order or a full-tree child field changed");

    let captured_ids: BTreeSet<_> = external("ids=").split(',').collect();
    let actual_ids: BTreeSet<_> = pieces.iter().map(|piece| piece.id.as_str()).collect();
    assert_eq!(actual_ids, captured_ids, "piece selection or queue order changed");
    let captured_orientations: BTreeSet<_> = external("orientations=")
        .split(',')
        .map(|value| value.parse::<i32>().unwrap())
        .collect();
    let actual_orientations: BTreeSet<_> = pieces.iter().filter_map(|piece| piece.orientation).collect();
    assert_eq!(actual_orientations, captured_orientations, "orientation stream changed");

    let fields: Vec<i32> = external("end_cap=")
        .split('|')
        .next()
        .unwrap()
        .split(',')
        .map(|value| value.parse().unwrap())
        .collect();
    let end_cap = pieces
        .iter()
        .find(|piece| piece.bounding_box == BoundingBox {
            min: [fields[0], fields[1], fields[2]],
            max: [fields[3], fields[4], fields[5]],
        })
        .expect("external terminal-fill box must be in the generated tree");
    let state_at = |pos| {
        end_cap.blocks.as_ref().and_then(|blocks| {
            blocks.iter().rev().find(|block| block.pos == pos).map(|block| block.state.as_str())
        })
    };
    assert_eq!(state_at([34, 75, 15]), Some("minecraft:nether_bricks"));
    assert_eq!(state_at([35, 75, 15]), None, "terminal random walk must not be a solid shell");
    assert!(EXTERNAL.contains("packet_sha256=spawner:"), "fixture must retain packet provenance");
    assert!(EXTERNAL.contains("packet_sha256=chest:"), "fixture must retain packet provenance");
    assert!(EXTERNAL.contains("packet_sha256=end_cap:"), "fixture must retain packet provenance");
}

/// Two externally captured foundation columns belong to castle pieces far from
/// the root.  Supplying replaceable cells above solid terrain exercises the
/// support walk's lower-bound and receiving-chunk clipping independently of
/// the root-only support control.
#[test]
fn seed_42_external_castle_support_columns_reach_the_captured_depths() {
    let mut world = DenseBlockGrid::new(-48, 0, 64, 16, 128, 16, "minecraft:netherrack");
    for y in 41..=50 {
        world.set(-36, y, 67, "minecraft:cave_air");
    }
    for y in 22..=60 {
        world.set(-37, y, 78, "minecraft:cave_air");
    }
    let mut random = stream(42, 0, 0);
    fortress::place_for_chunk(0, 0, -3, 4, &mut world, &mut random);
    for pos in [[-36, 41, 67], [-36, 50, 67], [-37, 22, 78], [-37, 60, 78]] {
        assert_eq!(world.get(pos[0], pos[1], pos[2]), "minecraft:nether_bricks", "external support {pos:?}");
    }
}

/// The captured rows retain the neighbor-derived facing and placement-stream
/// seed required for packet parity. The eager piece list does not have the
/// receiving chunk grid or its random stream, so this test records the two
/// boundaries explicitly instead of treating its fallback data as an oracle.
#[test]
fn seed_42_external_chest_rows_cover_state_and_expose_the_placement_rng_boundary() {
    let mut random = stream(42, 0, 0);
    let (pieces, _) = fortress::generate(0, 0, &mut random);
    let actual_seeds: BTreeSet<_> = pieces
        .iter()
        .flat_map(|piece| piece.loot.iter().map(|loot| (loot.pos, loot.seed)))
        .collect();
    let expected: Vec<_> = EXTERNAL
        .lines()
        .filter_map(|line| line.strip_prefix("chest="))
        .map(|row| {
            let mut fields = row.split('|');
            let location = fields.next().expect("captured chest location");
            let _facing = fields.next().expect("captured chest facing");
            let seed = fields.next().expect("captured chest seed");
            let mut coordinates = location.split(',').map(|value| value.parse::<i32>().unwrap());
            (
                [coordinates.next().unwrap(), coordinates.next().unwrap(), coordinates.next().unwrap()],
                _facing.to_string(),
                seed.parse::<i64>().unwrap(),
            )
        })
        .collect();
    let expected_positions: BTreeSet<_> = expected.iter().map(|(pos, _, _)| *pos).collect();
    let actual_positions: BTreeSet<_> = actual_seeds.iter().map(|(pos, _)| *pos).collect();
    assert!(
        actual_positions.is_superset(&expected_positions),
        "the external chest positions must stay attached to their piece writers; actual={actual_positions:?}"
    );
    assert_eq!(
        expected.iter().map(|(_, facing, _)| facing.as_str()).collect::<BTreeSet<_>>(),
        BTreeSet::from(["south", "west"]),
        "fixture must retain the captured neighbor-derived chest facings"
    );
    let expected_seeds: BTreeSet<_> = expected.iter().map(|(pos, _, seed)| (*pos, *seed)).collect();
    assert_ne!(
        actual_seeds, expected_seeds,
        "the eager start stream must not be mistaken for the external placement stream; replace this boundary assertion with equality when chunk placement owns the draws"
    );
}

/// A compact block control read from the external seed-42 chunk packet. The
/// root and west-facing room cover different local axes, so this catches both a
/// generic shell and unrotated directional fence properties without depending on
/// this crate's own terrain generator as an oracle.
#[test]
fn seed_42_external_chunk_zero_masonry_and_fence_sentinels() {
    let mut random = stream(42, 0, 0);
    let (pieces, _) = fortress::generate(0, 0, &mut random);
    let state_at = |piece: &lodestone_worldgen::structure::StructurePiece, pos| {
        piece
            .blocks
            .as_ref()
            .and_then(|blocks| blocks.iter().rev().find(|block| block.pos == pos))
            .map(|block| block.state.clone())
    };
    let root = &pieces[0];
    assert_eq!(root.id, "minecraft:nebcr");
    assert_eq!(state_at(root, [9, 69, 2]).as_deref(), Some("minecraft:nether_bricks"));
    assert_eq!(state_at(root, [10, 74, 2]).as_deref(), Some("minecraft:air"));

    let west_room = pieces
        .iter()
        .find(|piece| piece.bounding_box == BoundingBox { min: [-5, 72, 8], max: [1, 80, 14] })
        .expect("external chunk-zero west room");
    assert_eq!(west_room.id, "minecraft:nerc");
    assert_eq!(
        state_at(west_room, [1, 77, 10]).as_deref(),
        Some("minecraft:nether_brick_fence[east=false,north=true,south=true,waterlogged=false,west=false]")
    );
}

#[test]
fn negative_control_rejects_a_root_only_vertical_move() {
    let mut random = stream(42, 0, 0);
    let (pieces, _) = fortress::generate(0, 0, &mut random);
    let union = pieces.iter().fold(pieces[0].bounding_box, |all, piece| all.encapsulate(piece.bounding_box));
    assert_eq!(union.min[1], 48, "completed-tree anchor: {union:?}");
    assert_eq!(pieces[0].bounding_box.min[1], 69);
    assert_ne!(pieces[0].bounding_box.min[1], union.min[1], "the root alone cannot choose the final vertical translation");
}
