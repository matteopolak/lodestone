//! The stronghold an eye of ender is thrown toward exists in the served
//! overworld: the locate query answers from the real placement, the start at the
//! answer carries a portal room, and its twelve frames reach the generated
//! chunk with the placement's own 10% eye rate.

use lodestone_model::BlockPos;
use lodestone_server::{overworld_chunk_source, ChunkSource};
use lodestone_worldgen::structure::PieceRefinement;

const SEED: i64 = -195_764_831;

fn portal_frames(start: &lodestone_worldgen::structure::StructureStart) -> Vec<([i32; 3], String)> {
    let mut frames = Vec::new();
    for piece in &start.pieces {
        if let Some(PieceRefinement::StrongholdBlocks { writes }) = piece.refine.as_ref() {
            for write in writes.iter() {
                let state = write.block.state.canonical_state();
                if state.starts_with("minecraft:end_portal_frame") {
                    frames.push((write.block.pos, state.to_string()));
                }
            }
        }
    }
    frames
}

/// 128 strongholds, in rings: the first three sit 4 * 32 chunks out with a
/// 40-chunk jitter, then up to 7 chunks of biome relocation. The bounds are the
/// placement rule's own, not read back from the generator.
#[test]
fn the_overworld_places_128_strongholds_with_the_first_ring_in_range() {
    let source = overworld_chunk_source(SEED);
    let origins = source.generator().ring_structure_origins("minecraft:strongholds");
    assert_eq!(origins.len(), 128);
    for &(cx, cz) in &origins[..3] {
        let chunks = f64::from(cx).hypot(f64::from(cz));
        assert!((80.0..=176.0).contains(&chunks), "first-ring stronghold at {chunks} chunks");
    }
    assert!(
        source.generator().ring_structure_origins("minecraft:villages").is_empty(),
        "a random-spread set has no ring list (control)"
    );
}

#[test]
fn the_located_start_is_a_stronghold_with_twelve_frames_in_one_portal_room() {
    let source = overworld_chunk_source(SEED);
    let from = BlockPos::new(0, 64, 0);
    let target = source.locate_stronghold(from).expect("the overworld has strongholds");
    assert_eq!((target.x.rem_euclid(16), target.y, target.z.rem_euclid(16)), (0, 0, 0));
    let (cx, cz) = (target.x.div_euclid(16), target.z.div_euclid(16));
    let origins = source.generator().ring_structure_origins("minecraft:strongholds");
    let nearest = origins
        .iter()
        .copied()
        .min_by_key(|&(x, z)| {
            let dx = i64::from(x) * 16 + 8;
            let dz = i64::from(z) * 16 + 8;
            dx * dx + 32 * 32 + dz * dz
        })
        .expect("origins");
    assert_eq!((cx, cz), nearest);
    let starts = source.generator().structure_starts(cx, cz);
    let start = starts
        .iter()
        .find(|start| start.structure == "minecraft:stronghold")
        .expect("a stronghold starts at the located chunk");
    let rooms = start.pieces.iter().filter(|piece| piece.id == "minecraft:shpr").count();
    assert_eq!(rooms, 1);
    assert_eq!(portal_frames(start).len(), 12);
}

/// Across all 128 starts, about one frame in ten holds an eye
/// (`next_float() > 0.9`): 1536 frames, so 154 expected, sigma about 12.
#[test]
fn frames_roll_an_eye_about_one_time_in_ten() {
    let source = overworld_chunk_source(SEED);
    let origins = source.generator().ring_structure_origins("minecraft:strongholds");
    let (mut frames, mut eyes) = (0usize, 0usize);
    for (cx, cz) in origins {
        let starts = source.generator().structure_starts(cx, cz);
        let start = starts
            .iter()
            .find(|start| start.structure == "minecraft:stronghold")
            .expect("every ring position starts a stronghold");
        for (_, state) in portal_frames(start) {
            frames += 1;
            eyes += usize::from(state.contains("eye=true"));
        }
    }
    assert_eq!(frames, 128 * 12);
    assert!((105..=205).contains(&eyes), "{eyes} of {frames} frames hold an eye");
}

/// The frames reach actual generated blocks, not just the start's write list.
#[test]
fn the_generated_chunk_contains_the_portal_frames() {
    let source = overworld_chunk_source(SEED);
    let target = source.locate_stronghold(BlockPos::new(0, 64, 0)).expect("stronghold");
    let (cx, cz) = (target.x.div_euclid(16), target.z.div_euclid(16));
    let starts = source.generator().structure_starts(cx, cz);
    let start = starts
        .iter()
        .find(|start| start.structure == "minecraft:stronghold")
        .expect("start");
    let frames = portal_frames(start);
    let (pos, expected) = frames.first().expect("frames").clone();
    let column = source.column(pos[0].div_euclid(16), pos[2].div_euclid(16));
    let actual = column.block_state_id(pos[0].rem_euclid(16), pos[1], pos[2].rem_euclid(16));
    let wanted = lodestone_data::block_states::StateId::from_state_str(&expected).expect("state");
    assert_eq!(actual, wanted, "frame at {pos:?}");
}
