//! External-control checks for the Nether fortress piece graph.

use lodestone_worldgen::rng::{LegacyRandomSource, WorldgenRandom};
use lodestone_worldgen::structure::{BoundingBox, fortress};

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
