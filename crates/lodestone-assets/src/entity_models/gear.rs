//! Worn-gear layer rigs: the second mesh an equipped saddle, horse armour,
//! llama carpet or wolf armour draws over the animal's own body.
//!
//! A gear layer is the animal's own part tree (same part names, so the same
//! skeleton poses it) inflated or extended, drawn with an equipment sheet from
//! `textures/entity/equipment/<layer>/`. Parts the real client only shows while
//! the animal is ridden (the rein lines) are present but collapsed by the renderer
//! unless the entity has a passenger; see `lodestone_render::hidden_parts`.

use super::equines_felines::{donkey_from_equine, equine_base_root};
use super::monsters::scaled;
use super::*;

/// Adds `g` texels of grow to every cube under `part` that carries none yet.
fn inflate(part: &mut PartDef, g: f32) {
    for c in &mut part.cubes {
        if c.grow == crate::entity::Deformation::default() {
            c.grow = crate::entity::Deformation::uniform(g);
        }
    }
    for (_, child) in &mut part.children {
        inflate(child, g);
    }
}

fn model(w: u32, h: u32, root: PartDef) -> EntityModelDef {
    EntityModelDef { texture_width: w, texture_height: h, root }
}

/// The saddle mesh over the shared equine body: a saddle block on the back, a
/// head saddle, the bit rings, the mouth wrap and the two rein lines (ridden-only).
fn equine_saddle_root() -> PartDef {
    let mut root = equine_base_root();
    if let Some(body) = root.child_mut("body") {
        body.children.push((
            "saddle".to_string(),
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-5.0, -8.0, -9.0], [10.0, 9.0, 9.0], [26.0, 0.0]).grown(0.5)),
        ));
    }
    if let Some(head_parts) = root.child_mut("head_parts") {
        let mut add = |name: &str, c: CubeDef| {
            head_parts
                .children
                .push((name.to_string(), PartDef::new(PartPose::ZERO).with_cube(c)));
        };
        add("left_saddle_mouth", cube([2.0, -9.0, -6.0], [1.0, 2.0, 2.0], [29.0, 5.0]));
        add("right_saddle_mouth", cube([-3.0, -9.0, -6.0], [1.0, 2.0, 2.0], [29.0, 5.0]));
        let line = |x: f32| cube([x, -6.0, -8.0], [0.0, 3.0, 16.0], [32.0, 2.0]);
        for (name, x) in [("left_saddle_line", 3.1), ("right_saddle_line", -3.1)] {
            head_parts.children.push((
                name.to_string(),
                PartDef::new(PartPose::rotation(-PI / 6.0, 0.0, 0.0)).with_cube(line(x)),
            ));
        }
        let mut add = |name: &str, c: CubeDef| {
            head_parts
                .children
                .push((name.to_string(), PartDef::new(PartPose::ZERO).with_cube(c)));
        };
        add("head_saddle", cube([-3.0, -11.0, -1.9], [6.0, 5.0, 6.0], [1.0, 1.0]).grown(0.22));
        add("mouth_saddle_wrap", cube([-2.0, -11.0, -4.0], [4.0, 5.0, 2.0], [19.0, 0.0]).grown(0.2));
    }
    root
}

/// The armour mesh: the equine body with every grown-by-parameter box inflated
/// by 0.1. The body box keeps its own fixed 0.05, the ears their -0.001, and the
/// bare head-parts box none.
fn equine_armor_root() -> PartDef {
    let mut root = equine_base_root();
    for name in ["body", "head_parts", "left_hind_leg", "right_hind_leg", "left_front_leg", "right_front_leg"] {
        if let Some(part) = root.child_mut(name) {
            let keep_own = name == "head_parts";
            let own = part.cubes.clone();
            inflate(part, 0.1);
            if keep_own {
                part.cubes = own;
            }
        }
    }
    root
}

pub fn horse_saddle_model() -> EntityModelDef {
    scaled(model(64, 64, equine_saddle_root()), 1.1)
}
pub fn undead_horse_saddle_model() -> EntityModelDef {
    model(64, 64, equine_saddle_root())
}
pub fn donkey_saddle_model() -> EntityModelDef {
    donkey_from_equine(equine_saddle_root(), 0.87, false)
}
pub fn mule_saddle_model() -> EntityModelDef {
    donkey_from_equine(equine_saddle_root(), 0.92, false)
}
pub fn horse_armor_model() -> EntityModelDef {
    scaled(model(64, 64, equine_armor_root()), 1.1)
}
pub fn undead_horse_armor_model() -> EntityModelDef {
    model(64, 64, equine_armor_root())
}

/// The pig body with every box inflated by 0.5: the saddle layer.
pub fn pig_saddle_model() -> EntityModelDef {
    let mut m = pig_model();
    inflate(&mut m.root, 0.5);
    m
}

/// The wolf body inflated by 0.2: the armour layer.
pub fn wolf_armor_model() -> EntityModelDef {
    let mut m = wolf_model();
    inflate(&mut m.root, 0.2);
    m
}

/// The llama body inflated by 0.5 and without chest boxes: the carpet layer.
pub fn llama_decor_model() -> EntityModelDef {
    let mut m = llama_model();
    m.root.children.retain(|(n, _)| n != "left_chest" && n != "right_chest");
    inflate(&mut m.root, 0.5);
    m
}

/// The camel body plus its saddle block, bridle and reins (ridden-only).
pub fn camel_saddle_model() -> EntityModelDef {
    let mut m = camel_model();
    let g = 0.05;
    if let Some(body) = m.root.child_mut("body") {
        body.children.push((
            "saddle".to_string(),
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.5, -17.0, -15.5], [9.0, 5.0, 11.0], [74.0, 64.0]).grown(g))
                .with_cube(cube([-3.5, -20.0, -15.5], [7.0, 3.0, 11.0], [92.0, 114.0]).grown(g))
                .with_cube(cube([-7.5, -12.0, -23.5], [15.0, 12.0, 27.0], [0.0, 89.0]).grown(g)),
        ));
        if let Some(head) = body.child_mut("head") {
            head.children.push((
                "reins".to_string(),
                PartDef::new(PartPose::ZERO)
                    .with_cube(cube([3.51, -18.0, -17.0], [0.0, 7.0, 15.0], [98.0, 42.0]))
                    .with_cube(cube([-3.5, -18.0, -2.0], [7.0, 7.0, 0.0], [84.0, 57.0]))
                    .with_cube(cube([-3.51, -18.0, -17.0], [0.0, 7.0, 15.0], [98.0, 42.0])),
            ));
            head.children.push((
                "bridle".to_string(),
                PartDef::new(PartPose::ZERO)
                    .with_cube(cube([-3.5, -7.0, -15.0], [7.0, 8.0, 19.0], [60.0, 87.0]).grown(g))
                    .with_cube(cube([-3.5, -21.0, -15.0], [7.0, 14.0, 7.0], [21.0, 64.0]).grown(g))
                    .with_cube(cube([-2.5, -21.0, -21.0], [5.0, 5.0, 6.0], [50.0, 64.0]).grown(g))
                    .with_cube(cube([2.5, -19.0, -18.0], [1.0, 2.0, 2.0], [74.0, 70.0]))
                    .with_cube(cube([-3.5, -19.0, -18.0], [1.0, 2.0, 2.0], [74.0, 70.0]).mirrored()),
            ));
        }
    }
    m
}

/// Every gear rig as a corpus entry. The default sheet is only a placeholder
/// (the layer always names its own equipment sheet).
pub fn gear_entries() -> Vec<EntityModelEntry> {
    let rows: [(&'static str, &'static str, fn() -> EntityModelDef); 10] = [
        ("pig_saddle", "entity/equipment/pig_saddle/saddle", pig_saddle_model),
        ("horse_saddle", "entity/equipment/horse_saddle/saddle", horse_saddle_model),
        ("undead_horse_saddle", "entity/equipment/skeleton_horse_saddle/saddle", undead_horse_saddle_model),
        ("donkey_saddle", "entity/equipment/donkey_saddle/saddle", donkey_saddle_model),
        ("mule_saddle", "entity/equipment/mule_saddle/saddle", mule_saddle_model),
        ("horse_armor", "entity/equipment/horse_body/iron", horse_armor_model),
        ("undead_horse_armor", "entity/equipment/horse_body/iron", undead_horse_armor_model),
        ("wolf_armor", "entity/equipment/wolf_body/armadillo_scute", wolf_armor_model),
        ("llama_decor", "entity/equipment/llama_body/white", llama_decor_model),
        ("camel_saddle", "entity/equipment/camel_saddle/saddle", camel_saddle_model),
    ];
    rows.into_iter()
        .map(|(name, sheet, build)| EntityModelEntry { name, texture: EntityTexture::Fixed(sheet), build })
        .collect()
}
