//! The cushion entity resolves to a model, draws its dye colour's sheet, and
//! stands on its feet: a 1 x 0.25 slab resting on the entity's own position at
//! every quarter-turn yaw.
//!
//! Expected values are the reference's: the model literals of the cushion
//! layer (a 16 x 4 x 16 box at pivot (23, 4, -7) with origin (-31, -4, -1)), the
//! renderer's pose order (turn by `180 - yaw`, half turn about X, drop 0.25),
//! the sixteen dye names in ordinal order, and the texture files themselves.

use glam::{Mat4, Vec3};
use lodestone_data::entity_type::EntityType;
use lodestone_model::{EntityVariant, MobAppearance};
use lodestone_render::entity::model_for_type;
use lodestone_render::{AnimInput, EntityModelSet};

const DYES: [&str; 16] = [
    "white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray",
    "cyan", "purple", "blue", "brown", "green", "red", "black",
];

#[test]
fn the_cushion_type_resolves_to_its_own_rig() {
    let entry = model_for_type(EntityType::Cushion).expect("a cushion has a model");
    assert_eq!(entry.name, "cushion");
    let model = (entry.build)();
    assert_eq!((model.texture_width, model.texture_height), (64, 64));
    let slab = &model.root.children.iter().find(|(name, _)| name == "cushion").expect("the cushion part").1;
    assert_eq!([slab.pose.x, slab.pose.y, slab.pose.z], [23.0, 4.0, -7.0]);
    assert_eq!(slab.cubes.len(), 1);
    assert_eq!(slab.cubes[0].origin, [-31.0, -4.0, -1.0]);
    assert_eq!(slab.cubes[0].size, [16.0, 4.0, 16.0]);
}

#[test]
fn each_dye_ordinal_draws_its_colours_sheet() {
    let textures = lodestone_mc_cache::version_root(&lodestone_mc_cache::current_version())
        .join("client-src/assets/minecraft/textures");
    let mut wrong = Vec::new();
    for (ordinal, dye) in DYES.iter().enumerate() {
        let sheet = format!("entity/cushion/{dye}_cushion");
        let got = lodestone_render::entity_appearance_sheet(
            "cushion",
            Some(&EntityVariant::Dyed { color: ordinal as u8, sheared: false }),
            &MobAppearance::default(),
            false,
        );
        if got != Some(sheet.as_str()) {
            wrong.push(format!("{ordinal}: got {got:?}, want {sheet}"));
        }
        if textures.is_dir() && !textures.join(format!("{sheet}.png")).is_file() {
            wrong.push(format!("{sheet}.png is not in the reference assets"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert_eq!(
        lodestone_render::entity_appearance_sheet("cushion", None, &MobAppearance::default(), false),
        None,
        "an unreported colour keeps the model's own white sheet"
    );
    assert!(
        lodestone_render::entity_extra_sheet_dirs().iter().any(|dir| dir.ends_with("entity/cushion/")),
        "the sheet directory is loaded"
    );
}

/// The drawn slab matches the reference pose order at every quarter turn: its
/// corners, transformed by turn `180 - yaw`, half turn about X, drop 0.25, land
/// where the placement transform puts them.
#[test]
fn the_slab_stands_on_the_entity_position_at_every_quarter_turn() {
    let models = EntityModelSet::load();
    let feet = Vec3::new(10.5, 65.0, -3.5);
    for yaw in [0.0_f32, 90.0, 180.0, 270.0] {
        let instance = models
            .resolve_animated("cushion", feet, yaw, 0.0, 1.0, &AnimInput::default(), 0.0, 0.0)
            .expect("the cushion draws");
        // The reference order, applied to the model's own box in blocks.
        let reference = Mat4::from_translation(feet)
            * Mat4::from_rotation_y((180.0 - yaw).to_radians())
            * Mat4::from_rotation_x(std::f32::consts::PI)
            * Mat4::from_translation(Vec3::new(0.0, -0.25, 0.0));
        let (lo, hi) = (Vec3::new(-0.5, 0.0, -0.5), Vec3::new(0.5, 0.25, 0.5));
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            );
            let world = reference.transform_point3(corner);
            min = min.min(world);
            max = max.max(world);
        }
        // The slab spans one block across and a quarter up from the feet.
        assert!((min - Vec3::new(10.0, 65.0, -4.0)).abs().max_element() < 1e-5, "yaw {yaw}: {min:?}");
        assert!((max - Vec3::new(11.0, 65.25, -3.0)).abs().max_element() < 1e-5, "yaw {yaw}: {max:?}");
        // The drawn instance's box agrees, to the 0.005 the model shrinks by.
        let tolerance = 0.005 / 16.0 + 1e-4;
        assert!((instance.aabb_min - min).abs().max_element() < tolerance, "yaw {yaw}: {:?} vs {min:?}", instance.aabb_min);
        assert!((instance.aabb_max - max).abs().max_element() < tolerance, "yaw {yaw}: {:?} vs {max:?}", instance.aabb_max);
    }
}
