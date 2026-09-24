//! Acceptance gate for **3-D item geometry in a GUI slot**: the geometry half of
//! the hotbar's mini-blocks, end to end through [`BlockModels::build`].
//!
//! [`tests/model_census.rs`] measures item-model *coverage* against a
//! hand-stitched atlas. This gate proves the thing the census cannot: that the
//! real [`BlockModels::build`] wiring holds together — the atlas it stitches
//! really does contain the item textures, the tint palette really is shared with
//! the block states, and the pose/mesh helpers really do turn a stored
//! [`ItemGeometry`] into GUI-space vertices with the right winding.
//!
//! The three assertions that are only true of genuine per-item baking:
//!
//! * `minecraft:stone`'s item geometry is a **full cube** whose quads sample the
//!   same atlas as the block state, so no second atlas is needed;
//! * `minecraft:grass_block`'s item quads carry the **same palette index** as
//!   its block state's tinted top — the property that keeps the hotbar icon and
//!   the world block the same green;
//! * `minecraft:structure_block` bakes, which is only possible because
//!   `build_complete_atlas` seeds item textures: its blockstate names four
//!   mode-specific models, so `block/structure_block` is reachable from no
//!   blockstate at all.
//!
//! `#[ignore]`d and fail-closed, like the sibling gates. Run with:
//! `cargo test -p lodestone-render --test item_geometry_gate -- --ignored --nocapture`

use std::collections::BTreeMap;

use lodestone_assets::{GuiLight, ResourceLocation, ResourceManager, ZipSource};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::{
    BlockModels, Camera, blocks_json_registry, gui_item_pose, gui_ortho, is_full_cube,
    mesh_item_quads,
};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

fn build_models() -> (BlockModels, Box<dyn BlockStateRegistry>) {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    (models, Box::new(registry))
}

fn loc(s: &str) -> ResourceLocation {
    s.parse().expect("valid resource location")
}

fn state_id(raw: u32) -> StateId {
    StateId::new(raw).expect("state id from the canonical blocks report")
}

/// The signed screen area of a triangle after `m` — the quantity whose **sign**
/// `FrontFace::Ccw` acts on.
fn signed_area(m: glam::Mat4, p: [glam::Vec3; 3]) -> f32 {
    let q: [glam::Vec3; 3] = std::array::from_fn(|i| m.project_point3(p[i]));
    let a = q[1] - q[0];
    let b = q[2] - q[0];
    a.x * b.y - a.y * b.x
}

/// The first state id whose block matches `block` and whose properties are a
/// superset of `want`.
fn find_state(reg: &dyn BlockStateRegistry, block: &str, want: &[(&str, &str)]) -> Option<u32> {
    let ident: Identifier = block.parse().ok()?;
    let wanted: BTreeMap<&str, &str> = want.iter().copied().collect();
    (0..reg.state_count()).find(|&id| {
        let Some(state) = reg.resolve(id) else {
            return false;
        };
        if *state.block != ident {
            return false;
        }
        wanted
            .iter()
            .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
    })
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn item_geometry_covers_every_model_item() {
    let (models, _reg) = build_models();
    eprintln!("=== BlockModels item geometry ===");
    eprintln!("items with baked geometry: {}", models.item_count());
    eprintln!(
        "bake misses / notes:       {}",
        models.item_bake_misses().len()
    );
    for m in models.item_bake_misses() {
        eprintln!("  - {m}");
    }

    // Two populations now: ~752 items whose icon is a real 3-D model, plus the
    // flat `builtin/generated` majority, extruded into vanilla's thin slab by
    // `ItemModelGenerator`. See `tests/sprite_drop_pixels.rs` for why the second
    // group has to exist at all — without it every tool, ingot, gem and food drew
    // zero pixels when dropped.
    assert!(
        models.item_count() > 1400,
        "expected the model items (~752) plus the extruded sprite items to cover most of 26.2's \
         1,537 items, got {}",
        models.item_count()
    );
    // Every recorded entry must be a known, named note rather than a bake
    // failure: with item textures seeded, nothing should fail to bake.
    for m in models.item_bake_misses() {
        assert!(
            m.contains("composite icon") || m.contains("none of which stitched"),
            "unexpected item bake failure: {m}"
        );
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn stone_item_is_a_cube_sharing_the_block_atlas() {
    let (models, reg) = build_models();
    let item = models.item(&loc("minecraft:stone")).expect("stone item");
    assert!(
        is_full_cube(&item.quads),
        "the stone item bakes to a full cube"
    );
    assert_eq!(item.gui_light, GuiLight::Side, "a block item is side-lit");

    // Same atlas as the block state: the item's UVs must land on the same sprite
    // rect the block state's do. This is the "no second atlas" claim, measured.
    let state = find_state(reg.as_ref(), "minecraft:stone", &[]).expect("stone state");
    let block_uv = models.quads(state_id(state))[0].uvs[0];
    let matches = item
        .quads
        .iter()
        .any(|q| q.uvs.iter().any(|uv| (uv[0] - block_uv[0]).abs() < 1e-6));
    assert!(
        matches,
        "the item's quads must sample the same atlas rect as the block state's"
    );
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn grass_block_item_shares_the_block_tint_palette_slot() {
    let (models, reg) = build_models();
    let item = models
        .item(&loc("minecraft:grass_block"))
        .expect("grass_block item");
    let item_tint = item
        .quads
        .iter()
        .find_map(|q| q.tint_index)
        .expect("the grass_block item has a tinted face");

    let state = state_id(
        find_state(reg.as_ref(), "minecraft:grass_block", &[("snowy", "false")])
            .expect("grass_block state"),
    );
    let block_tint = models
        .quads(state)
        .iter()
        .find_map(|q| q.tint_index)
        .expect("the grass_block state has a tinted face");

    assert_eq!(
        item_tint, block_tint,
        "hotbar icon and world block must resolve to the same palette slot"
    );
    // And that slot must be a real colour, not the white untinted sentinel.
    let colour = models.tint_palette()[item_tint as usize];
    assert!(
        colour[1] > colour[0] && colour[1] > colour[2],
        "the grass slot must hold a green, got {colour:?}"
    );
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn structure_block_bakes_only_because_item_textures_are_seeded() {
    // `blockstates/structure_block.json` names four mode-specific models
    // (corner/data/load/save), so `block/structure_block` — the texture its
    // *item* model uses — is reachable from no blockstate. It is the one texture
    // the item seeding exists for.
    let (models, _reg) = build_models();
    let item = models
        .item(&loc("minecraft:structure_block"))
        .expect("structure_block must bake once its texture is seeded");
    assert!(!item.quads.is_empty());
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn a_posed_item_lands_inside_its_slot_and_keeps_its_winding() {
    let (models, reg) = build_models();
    let item = models.item(&loc("minecraft:stone")).expect("stone item");

    let rect = [104.0, 220.0, 16.0, 16.0];
    let pose = gui_item_pose(rect, &item.transform);
    let mesh = mesh_item_quads(&item.quads, pose, item.gui_light);
    assert_eq!(mesh.quad_count(), item.quads.len(), "no quad is culled");

    // Every vertex lands inside the 16 px slot (the vanilla 0.625 pose fits).
    for v in &mesh.vertices {
        assert!(
            v.position[0] >= rect[0] - 0.01 && v.position[0] <= rect[0] + rect[2] + 0.01,
            "x {} outside the slot",
            v.position[0]
        );
        assert!(
            v.position[1] >= rect[1] - 0.01 && v.position[1] <= rect[1] + rect[3] + 0.01,
            "y {} outside the slot",
            v.position[1]
        );
        assert_eq!(v.light, lodestone_render::GUI_ITEM_LIGHT);
    }

    // Exactly three of a cube's six faces may survive back-face culling, and
    // they must be the three nearest — the inside-out check, on real geometry.
    //
    // The front-facing *sign* is derived from the world camera rather than
    // assumed: the terrain path renders correctly today through this same
    // pipeline (`FrontFace::Ccw`, cull `Back`) with these same outward-wound
    // quads, so whatever sign a face turned towards that camera produces is the
    // sign `cull_mode: Back` keeps.
    let camera = Camera {
        position: glam::Vec3::new(0.5, 0.5, 4.0),
        yaw: 180.0, // forward = (0, 0, -1)
        pitch: 0.0,
        ..Camera::default()
    };
    let south = models
        .quads(state_id(
            find_state(reg.as_ref(), "minecraft:stone", &[]).expect("stone state"),
        ))
        .iter()
        .find(|q| q.direction == lodestone_assets::Direction::South)
        .expect("stone has a south face")
        .positions;
    let front_sign = signed_area(
        camera.view_projection(),
        [south[0], south[1], south[2]].map(glam::Vec3::from),
    )
    .signum();

    let clip = gui_ortho(854, 480);
    let mut front = Vec::new();
    let mut back = Vec::new();
    for q in 0..mesh.quad_count() {
        let p: [glam::Vec3; 3] =
            std::array::from_fn(|i| glam::Vec3::from(mesh.vertices[q * 4 + i].position));
        let depth = p.iter().map(|v| clip.project_point3(*v).z).sum::<f32>() / 3.0;
        if signed_area(clip, p).signum() == front_sign {
            front.push(depth);
        } else {
            back.push(depth);
        }
    }
    assert_eq!(front.len(), 3, "exactly three faces face the viewer");
    assert_eq!(back.len(), 3);
    let nearest_back = back.iter().copied().fold(f32::MAX, f32::min);
    let farthest_front = front.iter().copied().fold(f32::MIN, f32::max);
    assert!(
        farthest_front < nearest_back,
        "the visible faces must be the nearest ones ({farthest_front} vs {nearest_back})"
    );
}
