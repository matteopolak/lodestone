//! Tests for baking resolved models into renderer-ready quads ([`bake_model`]).
//!
//! The pure-geometry tests use a single 16x16 sprite atlas (so atlas UVs equal
//! the sprite-local UVs) and full-cube elements whose baked vertices are
//! computed by hand from vanilla's `FaceBakery` algorithm.

use std::collections::{BTreeMap, HashMap};

use lodestone_assets::{
    Atlas, AtlasBuilder, BlockBaker, Direction, Element, Face, FirstWeight, GuiLight, Image,
    ModelResolver, ModelTransform, ResolvedModel, ResourceLocation, ResourceManager, SeededWeight,
    TextureBinding, WeightSelector, bake_model,
};

/// A 16x16 single-sprite atlas: the sprite fills it, so atlas UV == local UV.
fn unit_atlas(texture: &str) -> Atlas {
    let mut b = AtlasBuilder::new().with_width(16);
    b.add_texture(
        ResourceLocation::parse(texture).unwrap(),
        Image {
            width: 16,
            height: 16,
            rgba: vec![255u8; 16 * 16 * 4],
        },
        None,
    );
    b.build().unwrap()
}

fn face(texture: &str) -> Face {
    Face {
        uv: None,
        texture: texture.to_string(),
        cullface: None,
        rotation: 0,
        tintindex: None,
    }
}

/// A full-cube model with the given faces, textured by variable `all`.
fn cube_model(faces: HashMap<Direction, Face>) -> ResolvedModel {
    let mut textures = HashMap::new();
    textures.insert(
        "all".to_string(),
        TextureBinding::Resolved(ResourceLocation::parse("minecraft:block/t").unwrap()),
    );
    ResolvedModel {
        textures,
        elements: vec![Element {
            from: [0.0, 0.0, 0.0],
            to: [16.0, 16.0, 16.0],
            rotation: None,
            faces,
            shade: None,
            light_emission: None,
            name: None,
        }],
        ambient_occlusion: true,
        gui_light: GuiLight::Side,
        display: HashMap::new(),
        texture_size: [16, 16],
        builtin: None,
    }
}

fn one_face_cube(dir: Direction, f: Face) -> ResolvedModel {
    let mut faces = HashMap::new();
    faces.insert(dir, f);
    cube_model(faces)
}

fn approx(a: [[f32; 3]; 4], b: [[f32; 3]; 4]) {
    for (va, vb) in a.iter().zip(b.iter()) {
        for k in 0..3 {
            assert!(
                (va[k] - vb[k]).abs() < 1e-4,
                "position mismatch: {a:?} vs {b:?}"
            );
        }
    }
}

fn approx_uv(a: [[f32; 2]; 4], b: [[f32; 2]; 4]) {
    for (va, vb) in a.iter().zip(b.iter()) {
        for k in 0..2 {
            assert!((va[k] - vb[k]).abs() < 1e-4, "uv mismatch: {a:?} vs {b:?}");
        }
    }
}

#[test]
fn unrotated_up_face_positions_and_default_uv() {
    let atlas = unit_atlas("minecraft:block/t");
    let model = one_face_cube(Direction::Up, face("#all"));
    let quads = bake_model(&model, &atlas, ModelTransform::default()).unwrap();
    assert_eq!(quads.len(), 1);
    let q = &quads[0];
    assert_eq!(q.direction, Direction::Up);
    approx(
        q.positions,
        [
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0],
        ],
    );
    // Default UV for a full up-face is the whole sprite, corners CCW.
    approx_uv(q.uvs, [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
    assert!(q.shade);
}

#[test]
fn unrotated_north_face_default_uv() {
    let atlas = unit_atlas("minecraft:block/t");
    let model = one_face_cube(Direction::North, face("#all"));
    let quads = bake_model(&model, &atlas, ModelTransform::default()).unwrap();
    let q = &quads[0];
    assert_eq!(q.direction, Direction::North);
    approx(
        q.positions,
        [
            [1.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
    );
    approx_uv(q.uvs, [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
}

#[test]
fn explicit_uv_with_face_rotation() {
    let atlas = unit_atlas("minecraft:block/t");
    let mut f = face("#all");
    f.uv = Some([0.0, 0.0, 8.0, 8.0]);
    f.rotation = 90;
    let model = one_face_cube(Direction::Up, f);
    let q = &bake_model(&model, &atlas, ModelTransform::default()).unwrap()[0];
    // 90-degree face rotation shifts the UV corners one step.
    approx_uv(q.uvs, [[0.0, 0.5], [0.5, 0.5], [0.5, 0.0], [0.0, 0.0]]);
}

#[test]
fn model_y_rotation_moves_positions_and_cullface() {
    let atlas = unit_atlas("minecraft:block/t");
    let mut f = face("#all");
    f.cullface = Some(Direction::North);
    let model = one_face_cube(Direction::North, f);
    let t = ModelTransform {
        x: 0,
        y: 90,
        uvlock: false,
    };
    let q = &bake_model(&model, &atlas, t).unwrap()[0];
    // North rotated 90 deg about Y (clockwise from above) becomes the east face.
    assert_eq!(q.direction, Direction::East);
    assert_eq!(q.cullface, Some(Direction::East));
    approx(
        q.positions,
        [
            [1.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
    );
}

#[test]
fn uvlock_keeps_uvs_world_aligned() {
    let atlas = unit_atlas("minecraft:block/t");
    let unrotated = &bake_model(
        &one_face_cube(Direction::Up, face("#all")),
        &atlas,
        ModelTransform::default(),
    )
    .unwrap()[0];

    let rotated_no_lock = &bake_model(
        &one_face_cube(Direction::Up, face("#all")),
        &atlas,
        ModelTransform {
            x: 0,
            y: 90,
            uvlock: false,
        },
    )
    .unwrap()[0];

    let rotated_lock = &bake_model(
        &one_face_cube(Direction::Up, face("#all")),
        &atlas,
        ModelTransform {
            x: 0,
            y: 90,
            uvlock: true,
        },
    )
    .unwrap()[0];

    // Without uvlock the top texture spins with the model; with uvlock it stays
    // aligned to the world, matching the unrotated UVs.
    approx_uv(rotated_lock.uvs, unrotated.uvs);
    let differs = rotated_no_lock
        .uvs
        .iter()
        .zip(unrotated.uvs.iter())
        .any(|(a, b)| (a[0] - b[0]).abs() > 1e-3 || (a[1] - b[1]).abs() > 1e-3);
    assert!(differs, "expected non-uvlock UVs to rotate with the model");
}

#[test]
fn tint_index_is_carried_through() {
    let atlas = unit_atlas("minecraft:block/t");
    let mut f = face("#all");
    f.tintindex = Some(0);
    let model = one_face_cube(Direction::Up, f);
    let q = &bake_model(&model, &atlas, ModelTransform::default()).unwrap()[0];
    assert_eq!(q.tint_index, Some(0));
}

#[test]
fn unresolved_texture_is_an_error() {
    let atlas = unit_atlas("minecraft:block/t");
    let mut textures = HashMap::new();
    textures.insert(
        "all".to_string(),
        TextureBinding::Unresolved("#missing".to_string()),
    );
    let model = ResolvedModel {
        textures,
        elements: vec![Element {
            from: [0.0; 3],
            to: [16.0; 3],
            rotation: None,
            faces: {
                let mut m = HashMap::new();
                m.insert(Direction::Up, face("#all"));
                m
            },
            shade: None,
            light_emission: None,
            name: None,
        }],
        ambient_occlusion: true,
        gui_light: GuiLight::Side,
        display: HashMap::new(),
        texture_size: [16, 16],
        builtin: None,
    };
    assert!(bake_model(&model, &atlas, ModelTransform::default()).is_err());
}

#[test]
fn faces_emitted_in_deterministic_order() {
    let atlas = unit_atlas("minecraft:block/t");
    let mut faces = HashMap::new();
    for d in [
        Direction::West,
        Direction::East,
        Direction::Up,
        Direction::Down,
        Direction::South,
        Direction::North,
    ] {
        faces.insert(d, face("#all"));
    }
    let model = cube_model(faces);
    let a = bake_model(&model, &atlas, ModelTransform::default()).unwrap();
    let b = bake_model(&model, &atlas, ModelTransform::default()).unwrap();
    let dirs_a: Vec<Direction> = a.iter().map(|q| q.direction).collect();
    let dirs_b: Vec<Direction> = b.iter().map(|q| q.direction).collect();
    assert_eq!(dirs_a, dirs_b);
    // Fixed order: down, up, north, south, east, west.
    assert_eq!(
        dirs_a,
        vec![
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::East,
            Direction::West,
        ]
    );
}

// --- WeightSelector ---

#[test]
fn first_weight_selects_index_zero() {
    assert_eq!(FirstWeight.select(&[5, 1, 1]), 0);
}

#[test]
fn seeded_weight_is_deterministic_and_in_range() {
    let s = SeededWeight(12345);
    let a = s.select(&[1, 1, 1, 1]);
    let b = s.select(&[1, 1, 1, 1]);
    assert_eq!(a, b);
    assert!(a < 4);
    // Different seeds can land on different candidates.
    let seeds: Vec<usize> = (0..64).map(|n| SeededWeight(n).select(&[1, 1])).collect();
    assert!(seeds.contains(&0));
    assert!(seeds.contains(&1));
}

// --- BlockBaker integration (in-memory pack) ---

fn memory_manager(files: &[(&str, &[u8])]) -> ResourceManager {
    use lodestone_assets::MemorySource;
    let mut src = MemorySource::new("test");
    for (path, bytes) in files {
        src.insert((*path).to_string(), bytes.to_vec());
    }
    ResourceManager::new(vec![Box::new(src)])
}

const UP_ONLY_MODEL: &[u8] =
    br##"{"textures":{"all":"block/t"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"up":{"texture":"#all"}}}]}"##;

#[test]
fn block_baker_unions_multipart_cases() {
    let blockstate = br#"{"multipart":[
        {"apply":{"model":"minecraft:block/a"}},
        {"apply":{"model":"minecraft:block/b"}}
    ]}"#;
    let manager = memory_manager(&[
        ("assets/minecraft/blockstates/testblock.json", blockstate),
        ("assets/minecraft/models/block/a.json", UP_ONLY_MODEL),
        ("assets/minecraft/models/block/b.json", UP_ONLY_MODEL),
    ]);
    let resolver = ModelResolver::new(&manager);
    let atlas = unit_atlas("minecraft:block/t");
    let baker = BlockBaker::new(&manager, &resolver, &atlas);

    let block = ResourceLocation::parse("minecraft:testblock").unwrap();
    let baked = baker
        .bake_block(&block, &BTreeMap::new(), &FirstWeight)
        .unwrap();
    // Two always-on cases, each a single up-face -> two quads.
    assert_eq!(baked.quads.len(), 2);
}

#[test]
fn block_baker_weighted_variant_is_deterministic() {
    let blockstate = br#"{"variants":{"":[
        {"model":"minecraft:block/a"},
        {"model":"minecraft:block/b"}
    ]}}"#;
    let manager = memory_manager(&[
        ("assets/minecraft/blockstates/testblock.json", blockstate),
        ("assets/minecraft/models/block/a.json", UP_ONLY_MODEL),
        ("assets/minecraft/models/block/b.json", UP_ONLY_MODEL),
    ]);
    let resolver = ModelResolver::new(&manager);
    let atlas = unit_atlas("minecraft:block/t");
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let block = ResourceLocation::parse("minecraft:testblock").unwrap();

    // A weighted list collapses to exactly one model -> one quad, same each run.
    let first = baker
        .bake_block(&block, &BTreeMap::new(), &FirstWeight)
        .unwrap();
    assert_eq!(first.quads.len(), 1);
    let again = baker
        .bake_block(&block, &BTreeMap::new(), &SeededWeight(7))
        .unwrap();
    let again2 = baker
        .bake_block(&block, &BTreeMap::new(), &SeededWeight(7))
        .unwrap();
    assert_eq!(again.quads.len(), 1);
    assert_eq!(again.quads, again2.quads);
}

#[test]
fn block_baker_missing_blockstate_errors() {
    let manager = memory_manager(&[]);
    let resolver = ModelResolver::new(&manager);
    let atlas = unit_atlas("minecraft:block/t");
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let block = ResourceLocation::parse("minecraft:nope").unwrap();
    assert!(
        baker
            .bake_block(&block, &BTreeMap::new(), &FirstWeight)
            .is_err()
    );
}

#[test]
fn uv_inset_shrinks_uvs_toward_sprite_centre() {
    // A 16px sprite occupies the whole 16px atlas, so one texel = 1/16 in UV.
    let atlas = unit_atlas("minecraft:block/t");
    let model = one_face_cube(Direction::Up, face("#all"));
    let base = &bake_model(&model, &atlas, ModelTransform::default()).unwrap()[0];
    let inset = &lodestone_assets::bake_model_with(
        &model,
        &atlas,
        ModelTransform::default(),
        &lodestone_assets::BakeOptions {
            uv_inset_texels: 0.5,
        },
    )
    .unwrap()[0];

    // Centre is unchanged; every corner moves toward it by half a texel (1/32).
    let cu = base.uvs.iter().map(|uv| uv[0]).sum::<f32>() / 4.0;
    let cv = base.uvs.iter().map(|uv| uv[1]).sum::<f32>() / 4.0;
    let icu = inset.uvs.iter().map(|uv| uv[0]).sum::<f32>() / 4.0;
    let icv = inset.uvs.iter().map(|uv| uv[1]).sum::<f32>() / 4.0;
    assert!(
        (cu - icu).abs() < 1e-6 && (cv - icv).abs() < 1e-6,
        "centre moved"
    );

    let texel = 1.0 / 16.0;
    for (b, i) in base.uvs.iter().zip(inset.uvs.iter()) {
        assert!(
            (b[0] - i[0]).abs() > 1e-6 || (b[1] - i[1]).abs() > 1e-6,
            "corner should move inward"
        );
        // Each corner shifts toward the centre by exactly half a texel per axis.
        assert!(((b[0] - i[0]).abs() - 0.5 * texel).abs() < 1e-5);
        assert!(((b[1] - i[1]).abs() - 0.5 * texel).abs() < 1e-5);
    }
}

#[test]
fn uv_inset_zero_is_identity() {
    let atlas = unit_atlas("minecraft:block/t");
    let model = one_face_cube(Direction::Up, face("#all"));
    let base = bake_model(&model, &atlas, ModelTransform::default()).unwrap();
    let opt = lodestone_assets::bake_model_with(
        &model,
        &atlas,
        ModelTransform::default(),
        &lodestone_assets::BakeOptions::default(),
    )
    .unwrap();
    assert_eq!(base, opt);
}
