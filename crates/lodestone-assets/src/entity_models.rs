//! The hand-ported entity-model corpus for the 1.21.5–26.2 family.
//!
//! Entity geometry is **code, not data** in vanilla (see [`crate::entity`]), so
//! each mesh here is transcribed by hand from the decompiled client under
//! `net/minecraft/client/model/...`. This module holds only the *data* (the
//! per-mob [`EntityModelDef`]s and their default texture paths); the version-free
//! baking primitive lives in [`crate::entity`]. In the project's ideal shape this
//! data would live in a version crate the way `AssetProfile` is supplied per
//! version; it lives here for now alongside `player_model`, clearly scoped to the
//! modern family, and the loader/baker never branches on version.
//!
//! Meshes are largely stable across versions, so this is authored once and
//! tweaked per version rather than reimplemented. Texel offsets, box extents,
//! poses and sheet sizes are the exact vanilla values; the sheet size of every
//! entry is checked against the real texture PNG in `client.jar` by the
//! `real_jar` coverage test, so a mistranscribed sheet size cannot pass silently.

use crate::entity::{
    CatCoat, CubeDef, EntityModelDef, EntityTexture, EntityVariant, HorseColor, HorseMarkings,
    LlamaColor, MooshroomColor, ParrotColor, PartDef, PartPose, Temperature, WolfCoat, WolfState,
    player_model,
};
use std::f32::consts::PI;

/// One ported entity model: a stable `name`, its texture (fixed or
/// variant-driven, see [`EntityTexture`]), and a builder that produces the
/// bake-ready [`EntityModelDef`].
///
/// 26.2 split pig/cow/chicken into `_temperate`/`_cold`/`_warm` variants and
/// removed the bare `pig.png`, so those entries carry an [`EntityTexture::ByVariant`]
/// selector with the temperate skin as the canonical default; invariant mobs are
/// [`EntityTexture::Fixed`].
#[derive(Clone, Debug)]
pub struct EntityModelEntry {
    /// Stable identifier for the model (not necessarily the registry id).
    pub name: &'static str,
    /// The texture sheet(s) for this model, resolved via [`EntityTexture`].
    pub texture: EntityTexture,
    /// Builds the bake-ready model.
    pub build: fn() -> EntityModelDef,
}

/// `_temperate`/`_cold`/`_warm` selector for a mob whose only variant axis is
/// climate (pig, cow, chicken in 26.2). Each mob gets a named selector below
/// because a `fn` pointer cannot capture per-mob path literals.
fn pig_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Temperature(Temperature::Cold) => "entity/pig/pig_cold",
        EntityVariant::Temperature(Temperature::Warm) => "entity/pig/pig_warm",
        EntityVariant::Temperature(Temperature::Temperate) => "entity/pig/pig_temperate",
        // `EntityVariant` grew axes for other mobs (horse colour, llama, cat,
        // wolf, parrot); pig only cares about `Temperature`, so it falls
        // through to its own canonical default for all of them.
        _ => "entity/pig/pig_temperate",
    }
}

fn cow_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Temperature(Temperature::Cold) => "entity/cow/cow_cold",
        EntityVariant::Temperature(Temperature::Warm) => "entity/cow/cow_warm",
        EntityVariant::Temperature(Temperature::Temperate) => "entity/cow/cow_temperate",
        _ => "entity/cow/cow_temperate",
    }
}

fn chicken_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Temperature(Temperature::Cold) => "entity/chicken/chicken_cold",
        EntityVariant::Temperature(Temperature::Warm) => "entity/chicken/chicken_warm",
        EntityVariant::Temperature(Temperature::Temperate) => "entity/chicken/chicken_temperate",
        _ => "entity/chicken/chicken_temperate",
    }
}

fn cube(origin: [f32; 3], size: [f32; 3], tex: [f32; 2]) -> CubeDef {
    CubeDef::new(origin, size, tex)
}

/// The shared humanoid mesh (`HumanoidModel.createMesh(g, yOffset=0)`): head with
/// a hat overlay, body, two arms and two legs, on the standard box layout. `g` is
/// the uniform cube deformation (`0.0` for the base layer).
///
/// Visible to the crate because [`crate::equipment`] builds the armour layers
/// from the *same* function at `g = 0.5` / `g = 1.0`. Vanilla does exactly that
/// — `HumanoidModel.createBaseArmorMesh` calls `createMesh(g, 0.0F)` — and
/// sharing it is what keeps an armour piece's pivots identical to the wearer's,
/// which is the precondition for posing a piece off the wearer's own part
/// matrix.
pub(crate) fn humanoid_root(g: f32) -> PartDef {
    let head = PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]).grown(g))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0]).grown(g + 0.5)),
        );
    PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 0.0, 0.0))
                .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0]).grown(g)),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0))
                .with_cube(cube([-3.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0]).grown(g)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0)).with_cube(
                cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 16.0])
                    .grown(g)
                    .mirrored(),
            ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-1.9, 12.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]).grown(g)),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.9, 12.0, 0.0)).with_cube(
                cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0])
                    .grown(g)
                    .mirrored(),
            ),
        )
}

/// `ZombieModel` / standard humanoid body layer, sheet 64×64.
pub fn zombie_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: humanoid_root(0.0),
    }
}

/// `SkeletonModel`: the humanoid mesh with thin (2×12×2) arms and legs, sheet
/// 64×32. Arms keep the humanoid pose; legs move to `±2.0`.
pub fn skeleton_model() -> EntityModelDef {
    let mut root = humanoid_root(0.0);
    if let Some(arm) = root.child_mut("right_arm") {
        arm.pose = PartPose::offset(-5.0, 2.0, 0.0);
        arm.cubes = vec![cube([-1.0, -2.0, -1.0], [2.0, 12.0, 2.0], [40.0, 16.0])];
    }
    if let Some(arm) = root.child_mut("left_arm") {
        arm.pose = PartPose::offset(5.0, 2.0, 0.0);
        arm.cubes = vec![cube([-1.0, -2.0, -1.0], [2.0, 12.0, 2.0], [40.0, 16.0]).mirrored()];
    }
    if let Some(leg) = root.child_mut("right_leg") {
        leg.pose = PartPose::offset(-2.0, 12.0, 0.0);
        leg.cubes = vec![cube([-1.0, 0.0, -1.0], [2.0, 12.0, 2.0], [0.0, 16.0])];
    }
    if let Some(leg) = root.child_mut("left_leg") {
        leg.pose = PartPose::offset(2.0, 12.0, 0.0);
        leg.cubes = vec![cube([-1.0, 0.0, -1.0], [2.0, 12.0, 2.0], [0.0, 16.0]).mirrored()];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `CreeperModel`, sheet 64×32. Head and body at `y=6`; four short legs.
pub fn creeper_model() -> EntityModelDef {
    let leg = || cube([-2.0, 0.0, -2.0], [4.0, 6.0, 4.0], [0.0, 16.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 6.0, 0.0)).with_cube(cube(
                [-4.0, -8.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 6.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, -2.0],
                [8.0, 12.0, 4.0],
                [16.0, 16.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-2.0, 18.0, 4.0)).with_cube(leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(2.0, 18.0, 4.0)).with_cube(leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-2.0, 18.0, -4.0)).with_cube(leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(2.0, 18.0, -4.0)).with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `SpiderModel`, sheet 64×32. Head, two body segments, eight 16-long legs posed
/// by `offsetAndRotation` (the rotations are the vanilla rest pose).
pub fn spider_model() -> EntityModelDef {
    let right_leg = || cube([-15.0, -1.0, -1.0], [16.0, 2.0, 2.0], [18.0, 0.0]);
    let left_leg = || cube([-1.0, -1.0, -1.0], [16.0, 2.0, 2.0], [18.0, 0.0]).mirrored();
    let leg = |name: &str, x: f32, z: f32, y_rot: f32, z_rot: f32, mirror: bool| {
        let c = if mirror { left_leg() } else { right_leg() };
        (
            name.to_string(),
            PartDef::new(PartPose::offset_and_rotation(x, 15.0, z, 0.0, y_rot, z_rot)).with_cube(c),
        )
    };
    let mut root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 15.0, -3.0)).with_cube(cube(
                [-4.0, -4.0, -8.0],
                [8.0, 8.0, 8.0],
                [32.0, 4.0],
            )),
        )
        .with_child(
            "body0",
            PartDef::new(PartPose::offset(0.0, 15.0, 0.0)).with_cube(cube(
                [-3.0, -3.0, -3.0],
                [6.0, 6.0, 6.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body1",
            PartDef::new(PartPose::offset(0.0, 15.0, 9.0)).with_cube(cube(
                [-5.0, -4.0, -6.0],
                [10.0, 8.0, 12.0],
                [0.0, 12.0],
            )),
        );
    let s = 0.58119464_f32;
    let legs = [
        leg("right_hind_leg", -4.0, 2.0, PI / 4.0, -PI / 4.0, false),
        leg("left_hind_leg", 4.0, 2.0, -PI / 4.0, PI / 4.0, true),
        leg("right_middle_hind_leg", -4.0, 1.0, PI / 8.0, -s, false),
        leg("left_middle_hind_leg", 4.0, 1.0, -PI / 8.0, s, true),
        leg("right_middle_front_leg", -4.0, 0.0, -PI / 8.0, -s, false),
        leg("left_middle_front_leg", 4.0, 0.0, PI / 8.0, s, true),
        leg("right_front_leg", -4.0, -1.0, -PI / 4.0, -PI / 4.0, false),
        leg("left_front_leg", 4.0, -1.0, PI / 4.0, PI / 4.0, true),
    ];
    for (name, part) in legs {
        root.children.push((name, part));
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// The shared quadruped mesh (`QuadrupedModel.createBodyMesh`): head, rotated
/// body, and four legs. `mirror_left`/`mirror_right` mirror the respective legs.
fn quadruped_root(leg_size: f32, mirror_left: bool, mirror_right: bool) -> PartDef {
    let right = || {
        let c = cube([-2.0, 0.0, -2.0], [4.0, leg_size, 4.0], [0.0, 16.0]);
        if mirror_right { c.mirrored() } else { c }
    };
    let left = || {
        let c = cube([-2.0, 0.0, -2.0], [4.0, leg_size, 4.0], [0.0, 16.0]);
        if mirror_left { c.mirrored() } else { c }
    };
    PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 18.0 - leg_size, -6.0)).with_cube(cube(
                [-4.0, -4.0, -8.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                17.0 - leg_size,
                2.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-5.0, -10.0, -7.0], [10.0, 16.0, 8.0], [28.0, 8.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 24.0 - leg_size, 7.0)).with_cube(right()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.0, 24.0 - leg_size, 7.0)).with_cube(left()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 24.0 - leg_size, -5.0)).with_cube(right()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.0, 24.0 - leg_size, -5.0)).with_cube(left()),
        )
}

/// `PigModel`: the quadruped base (leg 6, left legs mirrored) with a two-box head
/// (head + snout), sheet 64×64.
pub fn pig_model() -> EntityModelDef {
    let mut root = quadruped_root(6.0, true, false);
    if let Some(head) = root.child_mut("head") {
        head.pose = PartPose::offset(0.0, 12.0, -6.0);
        head.cubes = vec![
            cube([-4.0, -4.0, -8.0], [8.0, 8.0, 8.0], [0.0, 0.0]),
            cube([-2.0, 0.0, -9.0], [4.0, 3.0, 1.0], [16.0, 16.0]),
        ];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `CowModel` (`createBaseCowModel`): four-box head (head, snout, two horns),
/// two-box body (body + udder), four full-length legs, sheet 64×64.
pub fn cow_model() -> EntityModelDef {
    let right = || cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]);
    let left = || cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 16.0]).mirrored();
    let head = PartDef::new(PartPose::offset(0.0, 4.0, -8.0))
        .with_cube(cube([-4.0, -4.0, -6.0], [8.0, 8.0, 6.0], [0.0, 0.0]))
        .with_cube(cube([-3.0, 1.0, -7.0], [6.0, 3.0, 1.0], [1.0, 33.0]))
        .with_cube(cube([-5.0, -5.0, -5.0], [1.0, 3.0, 1.0], [22.0, 0.0]))
        .with_cube(cube([4.0, -5.0, -5.0], [1.0, 3.0, 1.0], [22.0, 0.0]));
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        5.0,
        2.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-6.0, -10.0, -7.0], [12.0, 18.0, 10.0], [18.0, 4.0]))
    .with_cube(cube([-2.0, 2.0, -8.0], [4.0, 6.0, 1.0], [52.0, 0.0]));
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.0, 12.0, 7.0)).with_cube(right()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.0, 12.0, 7.0)).with_cube(left()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.0, 12.0, -5.0)).with_cube(right()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.0, 12.0, -5.0)).with_cube(left()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `SheepModel` base body (no wool layer): quadruped base (leg 12, right legs
/// mirrored) with an overridden head and body, sheet 64×32.
pub fn sheep_model() -> EntityModelDef {
    let mut root = quadruped_root(12.0, false, true);
    if let Some(head) = root.child_mut("head") {
        head.pose = PartPose::offset(0.0, 6.0, -8.0);
        head.cubes = vec![cube([-3.0, -4.0, -6.0], [6.0, 6.0, 8.0], [0.0, 0.0])];
    }
    if let Some(body) = root.child_mut("body") {
        body.pose = PartPose::offset_and_rotation(0.0, 5.0, 2.0, PI / 2.0, 0.0, 0.0);
        body.cubes = vec![cube([-4.0, -10.0, -7.0], [8.0, 16.0, 6.0], [28.0, 8.0])];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `SheepFurModel.createFurLayer`: the wool overlay `SheepWoolLayer` draws over
/// [`sheep_model`] whenever the sheep is not sheared. Not a second skeleton —
/// see `docs/entity-rendering.md`'s wool section — its six parts share
/// `sheep_model`'s part *names* and pivots exactly (`head`, `body`,
/// `right_hind_leg`, `left_hind_leg`, `right_front_leg`, `left_front_leg`), the
/// same discipline `equipment.rs`'s armour meshes follow against
/// [`humanoid_root`]: a caller poses each wool part off the sheep body's own
/// already-animated `part_transforms`, reading and never mutating.
///
/// Three real deviations from the base body mesh, all read from
/// `SheepFurModel.java` (`net/minecraft/client/model/animal/sheep/`) rather
/// than guessed:
///
/// * **A different inflation per part**, not one uniform grow: `head` at
///   `+0.6`, `body` at `+1.75`, all four legs at `+0.5`
///   (`CubeDeformation`s baked into each `addBox` call). The head and body
///   boxes are vanilla's own literal origin/size, not `sheep_model`'s — in
///   particular the head's wool box is one texel *shallower* in Z
///   (`(-3,-4,-4)`, size `(6,6,6)`) than the body mesh's head box
///   (`(-3,-4,-6)`, size `(6,6,8)`), so even grown by 0.6 the wool never
///   reaches as far forward as the snout. That gap is vanilla, not a bug: a
///   sheep's face stays bare.
/// * **The legs are a shorter box, not a scaled copy of the body's.**
///   `addBox(-2,0,-2,4,6,4,0.5)` is 6 texels tall against the base leg's 12,
///   sharing the same pivot, so wool covers only the upper half of each leg —
///   vanilla's "socks" look, not a wrong deformation.
/// * **No mirroring anywhere.** `sheep_model` mirrors its right legs' UV
///   (`quadruped_root`'s `mirror_right`, matching `QuadrupedModel.createLegs`),
///   but `SheepFurModel.createFurLayer` builds one `CubeListBuilder` and reuses
///   it for all four legs with no `.mirror()` call, so the wool sheet's leg
///   region is not flipped for the right side the way the body's is.
///
/// Sheet 64×32 (`LayerDefinition.create(mesh, 64, 32)`), same as the body.
/// Adult only: `BabySheepModel`/`textures/entity/sheep/sheep_wool_baby.png` is
/// a separate, smaller mesh this port does not build yet — see the gap note in
/// `docs/entity-rendering.md`.
#[must_use]
pub fn sheep_wool_model() -> EntityModelDef {
    let leg = || cube([-2.0, 0.0, -2.0], [4.0, 6.0, 4.0], [0.0, 16.0]).grown(0.5);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 6.0, -8.0))
                .with_cube(cube([-3.0, -4.0, -4.0], [6.0, 6.0, 6.0], [0.0, 0.0]).grown(0.6)),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(0.0, 5.0, 2.0, PI / 2.0, 0.0, 0.0))
                .with_cube(cube([-4.0, -10.0, -7.0], [8.0, 16.0, 6.0], [28.0, 8.0]).grown(1.75)),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 12.0, 7.0)).with_cube(leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.0, 12.0, 7.0)).with_cube(leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 12.0, -5.0)).with_cube(leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.0, 12.0, -5.0)).with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// Sheep wool tint: `ColorLerper.Type.SHEEP`'s per-`DyeColor` modified colour at
/// vanilla's fixed `brightness = 0.75` (`SheepRenderState.getWoolColor`'s
/// non-`jeb_` branch — the rainbow name easter egg is not modelled here, see
/// the gap note in `docs/entity-rendering.md`).
///
/// `ordinal` is the wire's `0..=15` value in `DyeColor.id` order (white=0 …
/// black=15), matching `lodestone_model::event::EntityVariant::Dyed::color` —
/// the same ordinal the protocol layer already decodes out of the sheep wool
/// metadata byte's low nibble. Anything outside that range draws as undyed
/// white, the same fail-open rule `armour_layer_tint` uses for a colour it
/// doesn't recognise.
///
/// White is vanilla's own hardcoded special case, not `0.75 * (255,255,255)`:
/// `ColorLerper.getModifiedColor` returns the literal constant `-1644826`
/// (`0xE6E6E6`, `(230,230,230)`) for `DyeColor.WHITE` rather than running the
/// brightness multiply, which is why this table is hand-transcribed per entry
/// rather than computed here from `DyeColor.textureDiffuseColor` — the two
/// happen to be the *same* formula for every other colour
/// (`floor(channel * 0.75)`), but encoding white as a 17th special case in this
/// function would silently invite someone to "simplify" it back into the
/// formula and regress the one entry that is not one.
#[must_use]
pub fn sheep_wool_tint(ordinal: u8) -> [u8; 3] {
    const WHITE: [u8; 3] = [230, 230, 230];
    // `DyeColor.textureDiffuseColor`, `DyeColor.id` order, with vanilla's fixed
    // 0.75 brightness already applied (`floor(channel * 0.75)`), transcribed
    // from the literal constants in `DyeColor.java`.
    const TINTS: [[u8; 3]; 16] = [
        WHITE,           // 0  white (special-cased, see above)
        [186, 96, 21],   // 1  orange
        [149, 58, 141],  // 2  magenta
        [43, 134, 163],  // 3  light_blue
        [190, 162, 45],  // 4  yellow
        [96, 149, 23],   // 5  lime
        [182, 104, 127], // 6  pink
        [53, 59, 61],    // 7  gray
        [117, 117, 113], // 8  light_gray
        [16, 117, 117],  // 9  cyan
        [102, 37, 138],  // 10 purple
        [45, 51, 127],   // 11 blue
        [98, 63, 37],    // 12 brown
        [70, 93, 16],    // 13 green
        [132, 34, 28],   // 14 red
        [21, 21, 24],    // 15 black
    ];
    TINTS.get(ordinal as usize).copied().unwrap_or(WHITE)
}

/// `AdultChickenModel` (`createBaseChickenModel`): head with beak and wattle
/// children, rotated body, two legs, two wings, sheet 64×32.
pub fn chicken_model() -> EntityModelDef {
    let leg = || cube([-1.0, 0.0, -3.0], [3.0, 5.0, 3.0], [26.0, 0.0]);
    let head = PartDef::new(PartPose::offset(0.0, 15.0, -4.0))
        .with_cube(cube([-2.0, -6.0, -2.0], [4.0, 6.0, 3.0], [0.0, 0.0]))
        .with_child(
            "beak",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-2.0, -4.0, -4.0],
                [4.0, 2.0, 2.0],
                [14.0, 0.0],
            )),
        )
        .with_child(
            "red_thing",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-1.0, -2.0, -3.0],
                [2.0, 2.0, 2.0],
                [14.0, 4.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                16.0,
                0.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-3.0, -4.0, -3.0], [6.0, 8.0, 6.0], [0.0, 9.0])),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-2.0, 19.0, 1.0)).with_cube(leg()),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.0, 19.0, 1.0)).with_cube(leg()),
        )
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-4.0, 13.0, 0.0)).with_cube(cube(
                [0.0, 0.0, -3.0],
                [1.0, 4.0, 6.0],
                [24.0, 13.0],
            )),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(4.0, 13.0, 0.0)).with_cube(cube(
                [-1.0, 0.0, -3.0],
                [1.0, 4.0, 6.0],
                [24.0, 13.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `SlimeModel.createOuterBodyLayer`: the translucent outer shell, a single
/// 8×8×8 cube at `(-4,16,-4)` texOffs `(0,0)`, sheet 64×32. The inner core with
/// eyes/mouth is a second render layer (translucent-over-opaque) and is left to
/// the render pipeline; the outer cube is the recognisable silhouette.
pub fn slime_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child(
            "cube",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, 16.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        ),
    }
}

/// `MagmaCubeModel.createBodyLayer`: eight stacked 8×1×8 segments (`y = 16+i`)
/// whose texel offset steps per the vanilla `u,v` schedule, plus a 4×4×4 inside
/// cube. Sheet 64×64. The loop is mirrored structurally, not unrolled.
pub fn magma_cube_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO);
    for i in 0..8i32 {
        let (mut u, mut v) = (0i32, 0i32);
        if i > 0 && i < 4 {
            v += 9 * i;
        } else if i > 3 {
            u = 32;
            v += 9 * i - 36;
        }
        root = root.with_child(
            &format!("cube{i}"),
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, (16 + i) as f32, -4.0],
                [8.0, 1.0, 8.0],
                [u as f32, v as f32],
            )),
        );
    }
    root = root.with_child(
        "inside_cube",
        PartDef::new(PartPose::ZERO).with_cube(cube(
            [-2.0, 18.0, -2.0],
            [4.0, 4.0, 4.0],
            [24.0, 40.0],
        )),
    );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `BlazeModel.createBodyLayer`: a head plus twelve rods placed on three rings
/// (radii 9/7/5, offset angles `0`/`π/4`/`0.47123894`), sheet 64×32. Positions
/// come from the exact vanilla trig loop; `Mth.cos`/`sin` are approximated by
/// `f32` trig (sub-pixel identical for placement).
pub fn blaze_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "head",
        PartDef::new(PartPose::ZERO).with_cube(cube(
            [-4.0, -4.0, -4.0],
            [8.0, 8.0, 8.0],
            [0.0, 0.0],
        )),
    );
    let rod = |x: f32, y: f32, z: f32| {
        PartDef::new(PartPose::offset(x, y, z)).with_cube(cube(
            [0.0, 0.0, 0.0],
            [2.0, 8.0, 2.0],
            [0.0, 16.0],
        ))
    };
    let mut idx = 0usize;
    // Ring 0: i in 0..4, y = -2 + cos(i*2*0.25); Ring 1: i in 4..8, y = 2 + cos(i*2*0.25);
    // Ring 2: i in 8..12, y = 11 + cos(i*1.5*0.5). The cos-arg uses vanilla's
    // absolute `i`, so the three loops keep distinct `i` ranges.
    {
        let mut angle = 0.0f32;
        for i in 0..4 {
            let x = angle.cos() * 9.0;
            let y = -2.0 + ((i as f32) * 2.0 * 0.25).cos();
            let z = angle.sin() * 9.0;
            root.children.push((format!("part{idx}"), rod(x, y, z)));
            idx += 1;
            angle += PI / 2.0;
        }
        angle = PI / 4.0;
        for i in 4..8 {
            let x = angle.cos() * 7.0;
            let y = 2.0 + ((i as f32) * 2.0 * 0.25).cos();
            let z = angle.sin() * 7.0;
            root.children.push((format!("part{idx}"), rod(x, y, z)));
            idx += 1;
            angle += PI / 2.0;
        }
        angle = 0.47123894;
        for i in 8..12 {
            let x = angle.cos() * 5.0;
            let y = 11.0 + ((i as f32) * 1.5 * 0.5).cos();
            let z = angle.sin() * 5.0;
            root.children.push((format!("part{idx}"), rod(x, y, z)));
            idx += 1;
            angle += PI / 2.0;
        }
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `SquidModel.createBodyLayer`: a 12×16×12 body plus eight 2×18×2 tentacles
/// evenly placed on a radius-5 ring, each yaw-rotated to face outward. Sheet
/// 64×32. Positions/rotations come from the exact vanilla loop.
pub fn squid_model() -> EntityModelDef {
    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "body",
        PartDef::new(PartPose::offset(0.0, 8.0, 0.0)).with_cube(cube(
            [-6.0, -8.0, -6.0],
            [12.0, 16.0, 12.0],
            [0.0, 0.0],
        )),
    );
    for i in 0..8i32 {
        let a = (i as f32) * std::f32::consts::TAU / 8.0;
        let x = a.cos() * 5.0;
        let z = a.sin() * 5.0;
        let y_rot = (i as f32) * -std::f32::consts::TAU / 8.0 + PI / 2.0;
        root.children.push((
            format!("tentacle{i}"),
            PartDef::new(PartPose::offset_and_rotation(x, 15.0, z, 0.0, y_rot, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 18.0, 2.0], [48.0, 0.0])),
        ));
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `BatModel.createBodyLayer`: body, head with two flat ears, folded wings
/// (each a wing + wing-tip child on the body) and flat feet. Sheet **32×32** —
/// the only small-sheet entry in the batch, which the real-PNG coverage test
/// pins. Zero-thickness parts are intentional (vanilla flat quads).
pub fn bat_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset(0.0, 17.0, 0.0))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 5.0, 2.0], [0.0, 0.0]))
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-1.5, 0.0, 0.0))
                .with_cube(cube([-2.0, -2.0, 0.0], [2.0, 7.0, 0.0], [12.0, 0.0]))
                .with_child(
                    "right_wing_tip",
                    PartDef::new(PartPose::offset(-2.0, 0.0, 0.0)).with_cube(cube(
                        [-6.0, -2.0, 0.0],
                        [6.0, 8.0, 0.0],
                        [16.0, 0.0],
                    )),
                ),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(1.5, 0.0, 0.0))
                .with_cube(cube([0.0, -2.0, 0.0], [2.0, 7.0, 0.0], [12.0, 7.0]))
                .with_child(
                    "left_wing_tip",
                    PartDef::new(PartPose::offset(2.0, 0.0, 0.0)).with_cube(cube(
                        [0.0, -2.0, 0.0],
                        [6.0, 8.0, 0.0],
                        [16.0, 8.0],
                    )),
                ),
        )
        .with_child(
            "feet",
            PartDef::new(PartPose::offset(0.0, 5.0, 0.0)).with_cube(cube(
                [-1.5, 0.0, 0.0],
                [3.0, 2.0, 0.0],
                [16.0, 16.0],
            )),
        );
    let head = PartDef::new(PartPose::offset(0.0, 17.0, 0.0))
        .with_cube(cube([-2.0, -3.0, -1.0], [4.0, 3.0, 2.0], [0.0, 7.0]))
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.5, -2.0, 0.0)).with_cube(cube(
                [-2.5, -4.0, 0.0],
                [3.0, 5.0, 0.0],
                [1.0, 15.0],
            )),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.1, -3.0, 0.0)).with_cube(cube(
                [-0.1, -3.0, 0.0],
                [3.0, 5.0, 0.0],
                [8.0, 15.0],
            )),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO)
            .with_child("body", body)
            .with_child("head", head),
    }
}

/// `EndermanModel.createBodyLayer`: starts from the humanoid mesh but replaces
/// every part, so it is authored directly — small head with a `-0.5` hat, a
/// slim body, and the characteristic 2×30×2 arms and legs. Sheet 64×32.
pub fn enderman_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -13.0, 0.0))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 16.0]).grown(-0.5)),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, -14.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, -2.0],
                [8.0, 12.0, 4.0],
                [32.0, 16.0],
            )),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, -12.0, 0.0)).with_cube(cube(
                [-1.0, -2.0, -1.0],
                [2.0, 30.0, 2.0],
                [56.0, 0.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, -12.0, 0.0))
                .with_cube(cube([-1.0, -2.0, -1.0], [2.0, 30.0, 2.0], [56.0, 0.0]).mirrored()),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-2.0, -5.0, 0.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 30.0, 2.0],
                [56.0, 0.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(2.0, -5.0, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 30.0, 2.0], [56.0, 0.0]).mirrored()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

// ===========================================================================
// monster/* remainder (impl-assets lane). Everything below is transcribed from
// `net/minecraft/client/model/...` for the 26.2 family. The sibling `animal/*`,
// `npc/*` and `object/*` models are appended to `entity_models()` in their own
// delimited block.
// ===========================================================================

/// `DrownedModel` (extends `ZombieModel`): the zombie humanoid mesh with the
/// left arm and left leg re-textured (`texOffs (32,48)` / `(16,48)`) and no
/// longer mirrored. Sheet 64×64.
pub fn drowned_model() -> EntityModelDef {
    let mut root = humanoid_root(0.0);
    if let Some(arm) = root.child_mut("left_arm") {
        arm.pose = PartPose::offset(5.0, 2.0, 0.0);
        arm.cubes = vec![cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [32.0, 48.0])];
    }
    if let Some(leg) = root.child_mut("left_leg") {
        leg.pose = PartPose::offset(1.9, 12.0, 0.0);
        leg.cubes = vec![cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [16.0, 48.0])];
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `IronGolemModel`: head (+nose), a broad body (+belt), long arms and stocky
/// legs. Sheet **128×128** — the largest in the corpus, pinned by the real-PNG
/// coverage test.
pub fn iron_golem_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -7.0, -2.0))
        .with_cube(cube([-4.0, -12.0, -5.5], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_cube(cube([-1.0, -5.0, -7.5], [2.0, 4.0, 2.0], [24.0, 0.0]));
    let body = PartDef::new(PartPose::offset(0.0, -7.0, 0.0))
        .with_cube(cube([-9.0, -2.0, -6.0], [18.0, 12.0, 11.0], [0.0, 40.0]))
        .with_cube(cube([-4.5, 10.0, -3.0], [9.0, 5.0, 6.0], [0.0, 70.0]).grown(0.5));
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(0.0, -7.0, 0.0)).with_cube(cube(
                [-13.0, -2.5, -3.0],
                [4.0, 30.0, 6.0],
                [60.0, 21.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(0.0, -7.0, 0.0)).with_cube(cube(
                [9.0, -2.5, -3.0],
                [4.0, 30.0, 6.0],
                [60.0, 58.0],
            )),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-4.0, 11.0, 0.0)).with_cube(cube(
                [-3.5, -3.0, -3.0],
                [6.0, 16.0, 5.0],
                [37.0, 0.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(5.0, 11.0, 0.0))
                .with_cube(cube([-3.5, -3.0, -3.0], [6.0, 16.0, 5.0], [60.0, 0.0]).mirrored()),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `SnowGolemModel`: two stacked snow spheres, a head and two stick arms posed
/// with a `zRot` of ±1 rad (the right arm additionally yawed by π). All cubes
/// carry a `-0.5` deformation. Sheet 64×64.
pub fn snow_golem_model() -> EntityModelDef {
    let g = -0.5;
    let arm = || cube([-1.0, 0.0, -1.0], [12.0, 2.0, 2.0], [32.0, 0.0]).grown(g);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 4.0, 0.0))
                .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]).grown(g)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset_and_rotation(5.0, 6.0, 1.0, 0.0, 0.0, 1.0))
                .with_cube(arm()),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset_and_rotation(
                -5.0, 6.0, -1.0, 0.0, PI, -1.0,
            ))
            .with_cube(arm()),
        )
        .with_child(
            "upper_body",
            PartDef::new(PartPose::offset(0.0, 13.0, 0.0))
                .with_cube(cube([-5.0, -10.0, -5.0], [10.0, 10.0, 10.0], [0.0, 16.0]).grown(g)),
        )
        .with_child(
            "lower_body",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-6.0, -12.0, -6.0], [12.0, 12.0, 12.0], [0.0, 36.0]).grown(g)),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `VexModel`: a small floating humanoid — head, two-box body, thin arms and two
/// flat wings, all under a `root` pivot offset by `-2.5`. Sheet 32×32.
pub fn vex_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset(0.0, 20.0, 0.0))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 4.0, 2.0], [0.0, 10.0]))
        .with_cube(cube([-1.5, 1.0, -1.0], [3.0, 5.0, 2.0], [0.0, 16.0]).grown(-0.2))
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-1.75, 0.25, 0.0))
                .with_cube(cube([-1.25, -0.5, -1.0], [2.0, 4.0, 2.0], [23.0, 0.0]).grown(-0.1)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(1.75, 0.25, 0.0))
                .with_cube(cube([-0.75, -0.5, -1.0], [2.0, 4.0, 2.0], [23.0, 6.0]).grown(-0.1)),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(0.5, 1.0, 1.0))
                .with_cube(cube([0.0, 0.0, 0.0], [0.0, 5.0, 8.0], [16.0, 14.0]).mirrored()),
        )
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-0.5, 1.0, 1.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 5.0, 8.0],
                [16.0, 14.0],
            )),
        );
    let root = PartDef::new(PartPose::offset(0.0, -2.5, 0.0))
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 20.0, 0.0)).with_cube(cube(
                [-2.5, -5.0, -2.5],
                [5.0, 5.0, 5.0],
                [0.0, 0.0],
            )),
        )
        .with_child("body", body);
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child("root", root),
    }
}

/// The shared segment-worm builder for `SilverfishModel`/`EndermiteModel`: each
/// segment is a box sized from `sizes[i]` at `texOffs texs[i]`, dropped to the
/// floor (`y = 24 - height`) and chained along `z` by half the sum of adjacent
/// depths starting at `-3.5`. Returns the root plus the per-segment `z` offsets
/// (the silverfish reuses them to place its raised plates).
fn segment_worm(sizes: &[[i32; 3]], texs: &[[i32; 2]]) -> (PartDef, Vec<f32>) {
    let mut root = PartDef::new(PartPose::ZERO);
    let mut z_place = Vec::with_capacity(sizes.len());
    let mut placement = -3.5f32;
    for i in 0..sizes.len() {
        let (w, h, d) = (sizes[i][0], sizes[i][1], sizes[i][2]);
        root = root.with_child(
            &format!("segment{i}"),
            PartDef::new(PartPose::offset(0.0, (24 - h) as f32, placement)).with_cube(cube(
                [w as f32 * -0.5, 0.0, d as f32 * -0.5],
                [w as f32, h as f32, d as f32],
                [texs[i][0] as f32, texs[i][1] as f32],
            )),
        );
        z_place.push(placement);
        if i + 1 < sizes.len() {
            placement += (d + sizes[i + 1][2]) as f32 * 0.5;
        }
    }
    (root, z_place)
}

/// `SilverfishModel`: a seven-segment worm plus three raised texture plates
/// keyed off the segment `z` offsets. Sheet 64×32.
pub fn silverfish_model() -> EntityModelDef {
    let sizes = [
        [3, 2, 2],
        [4, 3, 2],
        [6, 4, 3],
        [3, 3, 3],
        [2, 2, 3],
        [2, 1, 2],
        [1, 1, 2],
    ];
    let texs = [[0, 0], [0, 4], [0, 9], [0, 16], [0, 22], [11, 0], [13, 4]];
    let (mut root, zp) = segment_worm(&sizes, &texs);
    root = root
        .with_child(
            "plate0",
            PartDef::new(PartPose::offset(0.0, 16.0, zp[2])).with_cube(cube(
                [-5.0, 0.0, sizes[2][2] as f32 * -0.5],
                [10.0, 8.0, sizes[2][2] as f32],
                [20.0, 0.0],
            )),
        )
        .with_child(
            "plate1",
            PartDef::new(PartPose::offset(0.0, 20.0, zp[4])).with_cube(cube(
                [-3.0, 0.0, sizes[4][2] as f32 * -0.5],
                [6.0, 4.0, sizes[4][2] as f32],
                [20.0, 11.0],
            )),
        )
        .with_child(
            "plate2",
            PartDef::new(PartPose::offset(0.0, 19.0, zp[1])).with_cube(cube(
                [-3.0, 0.0, sizes[4][2] as f32 * -0.5],
                [6.0, 5.0, sizes[1][2] as f32],
                [20.0, 18.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `EndermiteModel`: a four-segment worm on the same chaining rule. Sheet 64×32.
pub fn endermite_model() -> EntityModelDef {
    let sizes = [[4, 3, 2], [6, 4, 5], [3, 3, 1], [1, 2, 1]];
    let texs = [[0, 0], [0, 5], [0, 14], [0, 18]];
    let (root, _) = segment_worm(&sizes, &texs);
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// Applies a vanilla `MeshTransformer.scaling(factor)` to a whole model, exactly
/// as `LayerDefinition.apply` bakes it into the mesh: the root pose is
/// `scaled(factor).translated(0, 24.016*(1-factor), 0)`, i.e. scale about the
/// origin then shift so the model's feet stay planted. Used for the size-variant
/// mobs whose geometry is otherwise identical to a base model.
fn scaled(mut model: EntityModelDef, factor: f32) -> EntityModelDef {
    let y_offset = 24.016 * (1.0 - factor);
    let p = &mut model.root.pose;
    // PartPose::scaled multiplies both the offset and the scale by the factor;
    // PartPose::translated then adds the y offset.
    p.x *= factor;
    p.y = p.y * factor + y_offset;
    p.z *= factor;
    p.scale = [
        p.scale[0] * factor,
        p.scale[1] * factor,
        p.scale[2] * factor,
    ];
    model
}

/// Husk: the zombie/humanoid mesh baked at `scaling(1.0625)`
/// (`LayerDefinitions.java`).
pub fn husk_model() -> EntityModelDef {
    scaled(zombie_model(), 1.0625)
}

/// Wither skeleton: the skeleton mesh baked at `scaling(1.2)`.
pub fn wither_skeleton_model() -> EntityModelDef {
    scaled(skeleton_model(), 1.2)
}

/// Cave spider: the spider mesh baked at `scaling(0.7)`.
pub fn cave_spider_model() -> EntityModelDef {
    scaled(spider_model(), 0.7)
}

/// Ghast (`GhastModel.createBodyLayer`): a 16³ body plus nine hanging tentacles
/// whose lengths come from a seeded `SingleThreadedRandomSource(1660)` (vanilla's
/// java.util.Random LCG). The whole mesh is baked at `scaling(4.5)`. The model's
/// UV sheet is 64x32 even though the shipped texture is 128x64 (a 2x texture).
pub fn ghast_model() -> EntityModelDef {
    // Vanilla java.util.Random, so the tentacle lengths are byte-identical to the
    // game rather than eyeballed.
    struct JavaRng(i64);
    impl JavaRng {
        fn new(seed: i64) -> Self {
            JavaRng((seed ^ 0x5DEECE66D) & ((1i64 << 48) - 1))
        }
        fn next(&mut self, bits: u32) -> i32 {
            self.0 = self.0.wrapping_mul(0x5DEECE66D).wrapping_add(0xB) & ((1i64 << 48) - 1);
            (self.0 >> (48 - bits)) as i32
        }
        fn next_int(&mut self, bound: i32) -> i32 {
            if bound & -bound == bound {
                return ((bound as i64).wrapping_mul(self.next(31) as i64) >> 31) as i32;
            }
            loop {
                let bits = self.next(31);
                let val = bits % bound;
                if bits - val + (bound - 1) >= 0 {
                    return val;
                }
            }
        }
    }

    let mut root = PartDef::new(PartPose::ZERO).with_child(
        "body",
        PartDef::new(PartPose::offset(0.0, 17.6, 0.0)).with_cube(cube(
            [-8.0, -8.0, -8.0],
            [16.0, 16.0, 16.0],
            [0.0, 0.0],
        )),
    );

    let mut rng = JavaRng::new(1660);
    for i in 0..9i32 {
        // Transcribed verbatim from GhastModel: xo uses (i % 3) and (i / 3 % 2)
        // with integer division/modulo; yo uses (i / 3).
        let xo = (((i % 3) as f32 - (i / 3 % 2) as f32 * 0.5 + 0.25) / 2.0 * 2.0 - 1.0) * 5.0;
        let yo = ((i / 3) as f32 / 2.0 * 2.0 - 1.0) * 5.0;
        let len = (rng.next_int(7) + 8) as f32;
        root = root.with_child(
            &format!("tentacle{i}"),
            PartDef::new(PartPose::offset(xo, 24.6, yo)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, len, 2.0],
                [0.0, 0.0],
            )),
        );
    }

    scaled(
        EntityModelDef {
            texture_width: 64,
            texture_height: 32,
            root,
        },
        4.5,
    )
}

/// Hoglin (`HoglinModel.createBodyLayer`, also used for zoglin): a boxy body
/// with a flat mane plane, a tilted head with ears and horns, and four legs.
/// Sheet 128x64.
pub fn hoglin_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset_and_rotation(
        0.0, 2.0, -12.0, 0.87266463, 0.0, 0.0,
    ))
    .with_cube(cube([-7.0, -3.0, -19.0], [14.0, 6.0, 19.0], [61.0, 1.0]))
    .with_child(
        "right_ear",
        PartDef::new(PartPose::offset_and_rotation(
            -6.0,
            -2.0,
            -3.0,
            0.0,
            0.0,
            -PI * 2.0 / 9.0,
        ))
        .with_cube(cube([-6.0, -1.0, -2.0], [6.0, 1.0, 4.0], [1.0, 1.0])),
    )
    .with_child(
        "left_ear",
        PartDef::new(PartPose::offset_and_rotation(
            6.0,
            -2.0,
            -3.0,
            0.0,
            0.0,
            PI * 2.0 / 9.0,
        ))
        .with_cube(cube([0.0, -1.0, -2.0], [6.0, 1.0, 4.0], [1.0, 6.0])),
    )
    .with_child(
        "right_horn",
        PartDef::new(PartPose::offset(-7.0, 2.0, -12.0)).with_cube(cube(
            [-1.0, -11.0, -1.0],
            [2.0, 11.0, 2.0],
            [10.0, 13.0],
        )),
    )
    .with_child(
        "left_horn",
        PartDef::new(PartPose::offset(7.0, 2.0, -12.0)).with_cube(cube(
            [-1.0, -11.0, -1.0],
            [2.0, 11.0, 2.0],
            [1.0, 13.0],
        )),
    );

    let body = PartDef::new(PartPose::offset(0.0, 7.0, 0.0))
        .with_cube(cube([-8.0, -7.0, -13.0], [16.0, 14.0, 26.0], [1.0, 1.0]))
        .with_child(
            "mane",
            PartDef::new(PartPose::offset(0.0, -14.0, -7.0))
                .with_cube(cube([0.0, 0.0, -9.0], [0.0, 10.0, 19.0], [90.0, 33.0]).grown(0.001)),
        );

    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("head", head)
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.0, 10.0, -8.5)).with_cube(cube(
                [-3.0, 0.0, -3.0],
                [6.0, 14.0, 6.0],
                [66.0, 42.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.0, 10.0, -8.5)).with_cube(cube(
                [-3.0, 0.0, -3.0],
                [6.0, 14.0, 6.0],
                [41.0, 42.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.0, 13.0, 10.0)).with_cube(cube(
                [-2.5, 0.0, -2.5],
                [5.0, 11.0, 5.0],
                [21.0, 45.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(5.0, 13.0, 10.0)).with_cube(cube(
                [-2.5, 0.0, -2.5],
                [5.0, 11.0, 5.0],
                [0.0, 45.0],
            )),
        );

    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// Adult strider (`AdultStriderModel.createBodyLayer`): two tall legs, a cubic
/// body, and six flat mirrored bristle planes. Sheet 64x128.
pub fn strider_model() -> EntityModelDef {
    // Each bristle is a flat (zero-height) plane; the three on the right are
    // mirrored. (origin, tex, mirrored, pose offset, zRot)
    type Bristle = ([f32; 3], [f32; 2], bool, [f32; 3], f32);
    let bristles: [Bristle; 6] = [
        (
            [-12.0, 0.0, 0.0],
            [16.0, 65.0],
            true,
            [-8.0, 4.0, -8.0],
            -1.2217305,
        ),
        (
            [-12.0, 0.0, 0.0],
            [16.0, 49.0],
            true,
            [-8.0, -1.0, -8.0],
            -1.134464,
        ),
        (
            [-12.0, 0.0, 0.0],
            [16.0, 33.0],
            true,
            [-8.0, -5.0, -8.0],
            -0.87266463,
        ),
        (
            [0.0, 0.0, 0.0],
            [16.0, 33.0],
            false,
            [8.0, -6.0, -8.0],
            0.87266463,
        ),
        (
            [0.0, 0.0, 0.0],
            [16.0, 49.0],
            false,
            [8.0, -2.0, -8.0],
            1.134464,
        ),
        (
            [0.0, 0.0, 0.0],
            [16.0, 65.0],
            false,
            [8.0, 3.0, -8.0],
            1.2217305,
        ),
    ];
    let names = [
        "right_bottom_bristle",
        "right_middle_bristle",
        "right_top_bristle",
        "left_top_bristle",
        "left_middle_bristle",
        "left_bottom_bristle",
    ];
    let mut body = PartDef::new(PartPose::offset(0.0, 1.0, 0.0)).with_cube(cube(
        [-8.0, -6.0, -8.0],
        [16.0, 14.0, 16.0],
        [0.0, 0.0],
    ));
    for (i, (origin, tex, mirror, off, z_rot)) in bristles.iter().enumerate() {
        let mut c = cube(*origin, [12.0, 0.0, 16.0], *tex);
        if *mirror {
            c = c.mirrored();
        }
        body = body.with_child(
            names[i],
            PartDef::new(PartPose::offset_and_rotation(
                off[0], off[1], off[2], 0.0, 0.0, *z_rot,
            ))
            .with_cube(c),
        );
    }
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-4.0, 8.0, 0.0)).with_cube(cube(
                [-2.0, 0.0, -2.0],
                [4.0, 16.0, 4.0],
                [0.0, 32.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(4.0, 8.0, 0.0)).with_cube(cube(
                [-2.0, 0.0, -2.0],
                [4.0, 16.0, 4.0],
                [0.0, 55.0],
            )),
        )
        .with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 128,
        root,
    }
}

/// Guardian (`GuardianModel.createBodyLayer`): a five-box head, twelve spikes
/// placed on a cube's faces at their resting offsets, an eye, and a three-part
/// tail. Sheet 64x64.
pub fn guardian_model() -> EntityModelDef {
    // Per-spike face position (SPIKE_X/Y/Z) and rotation multiples of PI
    // (SPIKE_*_ROT), transcribed from GuardianModel.
    const SPIKE_X: [f32; 12] = [
        0.0, 0.0, 8.0, -8.0, -8.0, 8.0, 8.0, -8.0, 0.0, 0.0, 8.0, -8.0,
    ];
    const SPIKE_Y: [f32; 12] = [
        -8.0, -8.0, -8.0, -8.0, 0.0, 0.0, 0.0, 0.0, 8.0, 8.0, 8.0, 8.0,
    ];
    const SPIKE_Z: [f32; 12] = [
        8.0, -8.0, 0.0, 0.0, -8.0, -8.0, 8.0, 8.0, 8.0, -8.0, 0.0, 0.0,
    ];
    const SPIKE_X_ROT: [f32; 12] = [
        1.75, 0.25, 0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 1.25, 0.75, 0.0, 0.0,
    ];
    const SPIKE_Y_ROT: [f32; 12] = [
        0.0, 0.0, 0.0, 0.0, 0.25, 1.75, 1.25, 0.75, 0.0, 0.0, 0.0, 0.0,
    ];
    const SPIKE_Z_ROT: [f32; 12] = [
        0.0, 0.0, 0.25, 1.75, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.75, 1.25,
    ];

    let mut head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-6.0, 10.0, -8.0], [12.0, 12.0, 16.0], [0.0, 0.0]))
        .with_cube(cube([-8.0, 10.0, -6.0], [2.0, 12.0, 12.0], [0.0, 28.0]))
        .with_cube(cube([6.0, 10.0, -6.0], [2.0, 12.0, 12.0], [0.0, 28.0]).mirrored())
        .with_cube(cube([-6.0, 8.0, -6.0], [12.0, 2.0, 12.0], [16.0, 40.0]))
        .with_cube(cube([-6.0, 22.0, -6.0], [12.0, 2.0, 12.0], [16.0, 40.0]));

    // The resting spike offset baked by vanilla at ageInTicks=0, withdrawal=0:
    // offset = 1 + cos(i) * 0.01.
    for i in 0..12usize {
        let off = 1.0 + (i as f32).cos() * 0.01;
        let x = SPIKE_X[i] * off;
        let y = 16.0 + SPIKE_Y[i] * off;
        let z = SPIKE_Z[i] * off;
        head = head.with_child(
            &format!("spike{i}"),
            PartDef::new(PartPose::offset_and_rotation(
                x,
                y,
                z,
                PI * SPIKE_X_ROT[i],
                PI * SPIKE_Y_ROT[i],
                PI * SPIKE_Z_ROT[i],
            ))
            .with_cube(cube([-1.0, -4.5, -1.0], [2.0, 9.0, 2.0], [0.0, 0.0])),
        );
    }

    head = head.with_child(
        "eye",
        PartDef::new(PartPose::offset(0.0, 0.0, -8.25)).with_cube(cube(
            [-1.0, 15.0, 0.0],
            [2.0, 2.0, 1.0],
            [8.0, 0.0],
        )),
    );

    let tail2 = PartDef::new(PartPose::offset(0.5, 0.5, 6.0))
        .with_cube(cube([0.0, 14.0, 0.0], [2.0, 2.0, 6.0], [41.0, 32.0]))
        .with_cube(cube([1.0, 10.5, 3.0], [1.0, 9.0, 9.0], [25.0, 19.0]));
    let tail1 = PartDef::new(PartPose::offset(-1.5, 0.5, 14.0))
        .with_cube(cube([0.0, 14.0, 0.0], [3.0, 3.0, 7.0], [0.0, 54.0]))
        .with_child("tail2", tail2);
    let tail0 = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-2.0, 14.0, 7.0], [4.0, 4.0, 8.0], [40.0, 0.0]))
        .with_child("tail1", tail1);
    head = head.with_child("tail0", tail0);

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child("head", head),
    }
}

/// Adult piglin mesh (`AdultPiglinModel.createBodyLayer`): the vanilla player
/// mesh with the body reduced to its base layer and the head replaced by the
/// piglin head (snout, tusks, floppy ears). Shared by piglin, zombified_piglin
/// and piglin_brute — only the texture differs.
pub fn piglin_model() -> EntityModelDef {
    let mut model = player_model(false);
    let root = &mut model.root;

    // body: base cube only, jacket overlay removed.
    if let Some(body) = root.child_mut("body") {
        body.pose = PartPose::ZERO;
        body.cubes = vec![cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0])];
        body.children.clear();
    }

    // head: piglin snout/tusks, hat overlay removed, floppy ears added.
    if let Some(head) = root.child_mut("head") {
        head.pose = PartPose::ZERO;
        head.cubes = vec![
            cube([-5.0, -8.0, -4.0], [10.0, 8.0, 8.0], [0.0, 0.0]),
            cube([-2.0, -4.0, -5.0], [4.0, 4.0, 1.0], [31.0, 1.0]),
            cube([2.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 4.0]),
            cube([-3.0, -2.0, -5.0], [1.0, 2.0, 1.0], [2.0, 0.0]),
        ];
        head.children = vec![
            (
                "left_ear".to_string(),
                PartDef::new(PartPose::offset_and_rotation(
                    4.5,
                    -6.0,
                    0.0,
                    0.0,
                    0.0,
                    -PI / 6.0,
                ))
                .with_cube(cube([0.0, 0.0, -2.0], [1.0, 5.0, 4.0], [51.0, 6.0])),
            ),
            (
                "right_ear".to_string(),
                PartDef::new(PartPose::offset_and_rotation(
                    -4.5,
                    -6.0,
                    0.0,
                    0.0,
                    0.0,
                    PI / 6.0,
                ))
                .with_cube(cube([-1.0, 0.0, -2.0], [1.0, 5.0, 4.0], [39.0, 6.0])),
            ),
        ];
    }

    model
}

/// `PhantomModel.createBodyLayer`: a body carrying a two-segment tail, two
/// two-segment wings (right mirrored) and a head. Eight boxes, sheet 64x64.
pub fn phantom_model() -> EntityModelDef {
    let tail_tip = PartDef::new(PartPose::offset(0.0, 0.5, 6.0)).with_cube(cube(
        [-1.0, 0.0, 0.0],
        [1.0, 1.0, 6.0],
        [4.0, 29.0],
    ));
    let tail_base = PartDef::new(PartPose::offset(0.0, -2.0, 1.0))
        .with_cube(cube([-2.0, 0.0, 0.0], [3.0, 2.0, 6.0], [3.0, 20.0]))
        .with_child("tail_tip", tail_tip);

    let left_wing_tip = PartDef::new(PartPose::offset_and_rotation(6.0, 0.0, 0.0, 0.0, 0.0, 0.1))
        .with_cube(cube([0.0, 0.0, 0.0], [13.0, 1.0, 9.0], [16.0, 24.0]));
    let left_wing_base = PartDef::new(PartPose::offset_and_rotation(
        2.0, -2.0, -8.0, 0.0, 0.0, 0.1,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [6.0, 2.0, 9.0], [23.0, 12.0]))
    .with_child("left_wing_tip", left_wing_tip);

    let right_wing_tip = PartDef::new(PartPose::offset_and_rotation(
        -6.0, 0.0, 0.0, 0.0, 0.0, -0.1,
    ))
    .with_cube(cube([-13.0, 0.0, 0.0], [13.0, 1.0, 9.0], [16.0, 24.0]).mirrored());
    let right_wing_base = PartDef::new(PartPose::offset_and_rotation(
        -3.0, -2.0, -8.0, 0.0, 0.0, -0.1,
    ))
    .with_cube(cube([-6.0, 0.0, 0.0], [6.0, 2.0, 9.0], [23.0, 12.0]).mirrored())
    .with_child("right_wing_tip", right_wing_tip);

    let head = PartDef::new(PartPose::offset_and_rotation(0.0, 1.0, -7.0, 0.2, 0.0, 0.0))
        .with_cube(cube([-4.0, -2.0, -5.0], [7.0, 3.0, 5.0], [0.0, 0.0]));

    let body = PartDef::new(PartPose::rotation(-0.1, 0.0, 0.0))
        .with_cube(cube([-3.0, -2.0, -8.0], [5.0, 3.0, 9.0], [0.0, 8.0]))
        .with_child("tail_base", tail_base)
        .with_child("left_wing_base", left_wing_base)
        .with_child("right_wing_base", right_wing_base)
        .with_child("head", head);

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child("body", body),
    }
}

/// `WardenModel.createBodyLayer`: a rooted `bone` carrying the body (with two
/// flat ribcage planes, a head bearing two flat tendrils, and two long arms)
/// plus two legs. Ten boxes, sheet 128x128.
pub fn warden_model() -> EntityModelDef {
    let right_tendril = PartDef::new(PartPose::offset(-8.0, -12.0, 0.0)).with_cube(cube(
        [-16.0, -13.0, 0.0],
        [16.0, 16.0, 0.0],
        [52.0, 32.0],
    ));
    let left_tendril = PartDef::new(PartPose::offset(8.0, -12.0, 0.0)).with_cube(cube(
        [0.0, -13.0, 0.0],
        [16.0, 16.0, 0.0],
        [58.0, 0.0],
    ));
    let head = PartDef::new(PartPose::offset(0.0, -13.0, 0.0))
        .with_cube(cube([-8.0, -16.0, -5.0], [16.0, 16.0, 10.0], [0.0, 32.0]))
        .with_child("right_tendril", right_tendril)
        .with_child("left_tendril", left_tendril);

    let right_ribcage = PartDef::new(PartPose::offset(-7.0, -2.0, -4.0)).with_cube(cube(
        [-2.0, -11.0, -0.1],
        [9.0, 21.0, 0.0],
        [90.0, 11.0],
    ));
    let left_ribcage = PartDef::new(PartPose::offset(7.0, -2.0, -4.0))
        .with_cube(cube([-7.0, -11.0, -0.1], [9.0, 21.0, 0.0], [90.0, 11.0]).mirrored());
    let right_arm = PartDef::new(PartPose::offset(-13.0, -13.0, 1.0)).with_cube(cube(
        [-4.0, 0.0, -4.0],
        [8.0, 28.0, 8.0],
        [44.0, 50.0],
    ));
    let left_arm = PartDef::new(PartPose::offset(13.0, -13.0, 1.0)).with_cube(cube(
        [-4.0, 0.0, -4.0],
        [8.0, 28.0, 8.0],
        [0.0, 58.0],
    ));

    let body = PartDef::new(PartPose::offset(0.0, -21.0, 0.0))
        .with_cube(cube([-9.0, -13.0, -4.0], [18.0, 21.0, 11.0], [0.0, 0.0]))
        .with_child("right_ribcage", right_ribcage)
        .with_child("left_ribcage", left_ribcage)
        .with_child("head", head)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);

    let right_leg = PartDef::new(PartPose::offset(-5.9, -13.0, 0.0)).with_cube(cube(
        [-3.1, 0.0, -3.0],
        [6.0, 13.0, 6.0],
        [76.0, 48.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(5.9, -13.0, 0.0)).with_cube(cube(
        [-2.9, 0.0, -3.0],
        [6.0, 13.0, 6.0],
        [76.0, 76.0],
    ));

    let bone = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);

    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root: PartDef::new(PartPose::ZERO).with_child("bone", bone),
    }
}

/// `WitherBossModel.createBodyLayer` at the base deformation: shoulders, a
/// four-cube ribcage, a tail, and three heads. Nine boxes, sheet 64x64. The
/// tail's rest pose is placed from `cos/sin(0.20420352)*10`, transcribed rather
/// than pre-computed so it matches vanilla to the last bit.
pub fn wither_model() -> EntityModelDef {
    const RIBCAGE_X_ROT: f32 = 0.20420352;

    let shoulders = PartDef::new(PartPose::ZERO).with_cube(cube(
        [-10.0, 3.9, -0.5],
        [20.0, 3.0, 3.0],
        [0.0, 16.0],
    ));

    let ribcage = PartDef::new(PartPose::offset_and_rotation(
        -2.0,
        6.9,
        -0.5,
        RIBCAGE_X_ROT,
        0.0,
        0.0,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [3.0, 10.0, 3.0], [0.0, 22.0]))
    .with_cube(cube([-4.0, 1.5, 0.5], [11.0, 2.0, 2.0], [24.0, 22.0]))
    .with_cube(cube([-4.0, 4.0, 0.5], [11.0, 2.0, 2.0], [24.0, 22.0]))
    .with_cube(cube([-4.0, 6.5, 0.5], [11.0, 2.0, 2.0], [24.0, 22.0]));

    let tail = PartDef::new(PartPose::offset_and_rotation(
        -2.0,
        6.9 + RIBCAGE_X_ROT.cos() * 10.0,
        -0.5 + RIBCAGE_X_ROT.sin() * 10.0,
        0.83252203,
        0.0,
        0.0,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [3.0, 6.0, 3.0], [12.0, 22.0]));

    let center_head = PartDef::new(PartPose::ZERO).with_cube(cube(
        [-4.0, -4.0, -4.0],
        [8.0, 8.0, 8.0],
        [0.0, 0.0],
    ));
    let side_head = |x: f32| {
        PartDef::new(PartPose::offset(x, 4.0, 0.0)).with_cube(cube(
            [-4.0, -4.0, -4.0],
            [6.0, 6.0, 6.0],
            [32.0, 0.0],
        ))
    };

    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("shoulders", shoulders)
            .with_child("ribcage", ribcage)
            .with_child("tail", tail)
            .with_child("center_head", center_head)
            .with_child("right_head", side_head(-8.0))
            .with_child("left_head", side_head(10.0)),
    }
}

/// `EnderDragonModel.createBodyLayer`: head+jaw, five neck segments and twelve
/// tail segments (all sharing one two-cube "spine" mesh), a body, two wings
/// (each a bone + a flat skin plane) and four three-segment legs. 65 boxes on a
/// 256x256 sheet. The wings' `skin` planes use vanilla's negative `texOffs(-56,
/// ..)` — legitimately off the left edge, which is why the UV gate is a gross
/// sanity envelope, not a strict `[0, 1]` bound.
pub fn ender_dragon_model() -> EntityModelDef {
    // The shared neck/tail segment: a 10-cube plus a small dorsal scale.
    let spine = |x: f32, y: f32, z: f32| {
        PartDef::new(PartPose::offset(x, y, z))
            .with_cube(cube([-5.0, -5.0, -5.0], [10.0, 10.0, 10.0], [192.0, 104.0]))
            .with_cube(cube([-1.0, -9.0, -3.0], [2.0, 4.0, 6.0], [48.0, 0.0]))
    };

    let jaw = PartDef::new(PartPose::offset(0.0, 4.0, -8.0)).with_cube(cube(
        [-6.0, 0.0, -16.0],
        [12.0, 4.0, 16.0],
        [176.0, 65.0],
    ));
    let head = PartDef::new(PartPose::offset(0.0, 20.0, -62.0))
        .with_cube(cube([-6.0, -1.0, -24.0], [12.0, 5.0, 16.0], [176.0, 44.0]))
        .with_cube(cube([-8.0, -8.0, -10.0], [16.0, 16.0, 16.0], [112.0, 30.0]))
        .with_cube(cube([-5.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]).mirrored())
        .with_cube(cube([-5.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]).mirrored())
        .with_cube(cube([3.0, -12.0, -4.0], [2.0, 4.0, 6.0], [0.0, 0.0]).mirrored())
        .with_cube(cube([3.0, -3.0, -22.0], [2.0, 2.0, 4.0], [112.0, 0.0]).mirrored())
        .with_child("jaw", jaw);

    // One side's leg chain (hip -> tip -> foot). `s` is the x-sign: +1 left,
    // -1 right. Vanilla mirrors by sign of the hip offset, not by tex mirror.
    let leg = |s: f32| {
        let front_foot = PartDef::new(PartPose::offset_and_rotation(
            0.0, 23.0, 0.0, 0.75, 0.0, 0.0,
        ))
        .with_cube(cube([-4.0, 0.0, -12.0], [8.0, 4.0, 16.0], [144.0, 104.0]));
        let front_tip = PartDef::new(PartPose::offset_and_rotation(
            0.0, 20.0, -1.0, -0.5, 0.0, 0.0,
        ))
        .with_cube(cube([-3.0, -1.0, -3.0], [6.0, 24.0, 6.0], [226.0, 138.0]))
        .with_child("front_foot", front_foot);
        let front_leg = PartDef::new(PartPose::offset_and_rotation(
            12.0 * s,
            17.0,
            -6.0,
            1.3,
            0.0,
            0.0,
        ))
        .with_cube(cube([-4.0, -4.0, -4.0], [8.0, 24.0, 8.0], [112.0, 104.0]))
        .with_child("front_tip", front_tip);

        let hind_foot = PartDef::new(PartPose::offset_and_rotation(
            0.0, 31.0, 4.0, 0.75, 0.0, 0.0,
        ))
        .with_cube(cube([-9.0, 0.0, -20.0], [18.0, 6.0, 24.0], [112.0, 0.0]));
        let hind_tip = PartDef::new(PartPose::offset_and_rotation(
            0.0, 32.0, -4.0, 0.5, 0.0, 0.0,
        ))
        .with_cube(cube([-6.0, -2.0, 0.0], [12.0, 32.0, 12.0], [196.0, 0.0]))
        .with_child("hind_foot", hind_foot);
        let hind_leg = PartDef::new(PartPose::offset_and_rotation(
            16.0 * s,
            13.0,
            34.0,
            1.0,
            0.0,
            0.0,
        ))
        .with_cube(cube([-8.0, -4.0, -8.0], [16.0, 32.0, 16.0], [0.0, 0.0]))
        .with_child("hind_tip", hind_tip);
        (front_leg, hind_leg)
    };

    // Left wing bones are tex-mirrored and grow toward +x; right wing is not
    // mirrored and grows toward -x (box origins already at -56).
    let left_wing_tip = PartDef::new(PartPose::offset(56.0, 0.0, 0.0))
        .with_cube(cube([0.0, -2.0, -2.0], [56.0, 4.0, 4.0], [112.0, 136.0]).mirrored())
        .with_cube(cube([0.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 144.0]).mirrored());
    let left_wing = PartDef::new(PartPose::offset(12.0, 2.0, -6.0))
        .with_cube(cube([0.0, -4.0, -4.0], [56.0, 8.0, 8.0], [112.0, 88.0]).mirrored())
        .with_cube(cube([0.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 88.0]).mirrored())
        .with_child("left_wing_tip", left_wing_tip);

    let right_wing_tip = PartDef::new(PartPose::offset(-56.0, 0.0, 0.0))
        .with_cube(cube([-56.0, -2.0, -2.0], [56.0, 4.0, 4.0], [112.0, 136.0]))
        .with_cube(cube([-56.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 144.0]));
    let right_wing = PartDef::new(PartPose::offset(-12.0, 2.0, -6.0))
        .with_cube(cube([-56.0, -4.0, -4.0], [56.0, 8.0, 8.0], [112.0, 88.0]))
        .with_cube(cube([-56.0, 0.0, 2.0], [56.0, 0.0, 56.0], [-56.0, 88.0]))
        .with_child("right_wing_tip", right_wing_tip);

    let (left_front, left_hind) = leg(1.0);
    let (right_front, right_hind) = leg(-1.0);
    let body = PartDef::new(PartPose::offset(0.0, 3.0, 8.0))
        .with_cube(cube([-12.0, 1.0, -16.0], [24.0, 24.0, 64.0], [0.0, 0.0]))
        .with_cube(cube([-1.0, -5.0, -10.0], [2.0, 6.0, 12.0], [220.0, 53.0]))
        .with_cube(cube([-1.0, -5.0, 10.0], [2.0, 6.0, 12.0], [220.0, 53.0]))
        .with_cube(cube([-1.0, -5.0, 30.0], [2.0, 6.0, 12.0], [220.0, 53.0]))
        .with_child("left_wing", left_wing)
        .with_child("left_front_leg", left_front)
        .with_child("left_hind_leg", left_hind)
        .with_child("right_wing", right_wing)
        .with_child("right_front_leg", right_front)
        .with_child("right_hind_leg", right_hind);

    let mut root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body);
    for i in 0..5 {
        root = root.with_child(
            &format!("neck{i}"),
            spine(0.0, 20.0, -12.0 - i as f32 * 10.0),
        );
    }
    for i in 0..12 {
        root = root.with_child(
            &format!("tail{i}"),
            spine(0.0, 10.0, 60.0 + i as f32 * 10.0),
        );
    }

    EntityModelDef {
        texture_width: 256,
        texture_height: 256,
        root,
    }
}

/// `WitchModel.createBodyLayer`: the villager body (body+jacket, three-cube
/// arms, two legs) and a head bearing the pointy witch hat (four stacked,
/// progressively-rotated segments over the inherited villager hat brim) plus a
/// nose with its mole. Fifteen boxes on a 64x128 sheet, baked at the villager
/// `scaling(0.9375)`. Witch has a single texture (`witch.png`), so unlike the
/// villager itself it needs no profession/type variant seam.
pub fn witch_model() -> EntityModelDef {
    // Villager body/arms/legs (VillagerModel.createBodyModel), unchanged.
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_child(
            "jacket",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.5)),
        );
    let arms = PartDef::new(PartPose::offset_and_rotation(
        0.0, 3.0, -1.0, -0.75, 0.0, 0.0,
    ))
    .with_cube(cube([-8.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]))
    .with_cube(cube([4.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]).mirrored())
    .with_cube(cube([-4.0, 2.0, -2.0], [8.0, 4.0, 4.0], [40.0, 38.0]));
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0)).with_cube(cube(
        [-2.0, 0.0, -2.0],
        [4.0, 12.0, 4.0],
        [0.0, 22.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored());

    // Witch head: the villager head cube, the witch hat (with the villager brim
    // inherited beneath it via addOrReplaceChild's child merge), and the nose+mole.
    let hat4 = PartDef::new(PartPose::offset_and_rotation(
        1.75,
        -2.0,
        2.0,
        -PI / 15.0,
        0.0,
        0.10471976,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [1.0, 2.0, 1.0], [0.0, 95.0]).grown(0.25));
    let hat3 = PartDef::new(PartPose::offset_and_rotation(
        1.75,
        -4.0,
        2.0,
        -0.10471976,
        0.0,
        0.05235988,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [4.0, 4.0, 4.0], [0.0, 87.0]))
    .with_child("hat4", hat4);
    let hat2 = PartDef::new(PartPose::offset_and_rotation(
        1.75,
        -4.0,
        2.0,
        -0.05235988,
        0.0,
        0.02617994,
    ))
    .with_cube(cube([0.0, 0.0, 0.0], [7.0, 4.0, 7.0], [0.0, 76.0]))
    .with_child("hat3", hat3);
    let hat_rim = PartDef::new(PartPose::rotation(-PI / 2.0, 0.0, 0.0)).with_cube(cube(
        [-8.0, -8.0, -6.0],
        [16.0, 16.0, 1.0],
        [30.0, 47.0],
    ));
    let hat = PartDef::new(PartPose::offset(-5.0, -10.03125, -5.0))
        .with_cube(cube([0.0, 0.0, 0.0], [10.0, 2.0, 10.0], [0.0, 64.0]))
        .with_child("hat_rim", hat_rim)
        .with_child("hat2", hat2);
    let nose = PartDef::new(PartPose::offset(0.0, -2.0, 0.0))
        .with_cube(cube([-1.0, -1.0, -6.0], [2.0, 4.0, 2.0], [24.0, 0.0]))
        .with_child(
            "mole",
            PartDef::new(PartPose::offset(0.0, -2.0, 0.0))
                .with_cube(cube([0.0, 3.0, -6.75], [1.0, 1.0, 1.0], [0.0, 0.0]).grown(-0.25)),
        );
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_child("hat", hat)
        .with_child("nose", nose);

    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 128,
        root: PartDef::new(PartPose::ZERO)
            .with_child("head", head)
            .with_child("body", body)
            .with_child("arms", arms)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    };
    scaled(model, 0.9375)
}

/// `VillagerModel.createBodyModel` (`npc/VillagerModel.java`), sheet 64×64, wrapped
/// in `villagerLikeScale` = `MeshTransformer.scaling(0.9375)`. Head carries a hat
/// (deform 0.51) with a flat brim rotated `-π/2`, plus the trademark nose; body
/// carries a jacket overlay (deform 0.5); the arms are one part posed forward
/// (`offsetAndRotation(0,3,-1, -0.75,0,0)`) holding both limb cubes and a
/// connecting cube. 11 boxes.
///
/// Profession/type/biome skins are *overlay layers* composited over this base
/// sheet in vanilla, not a single-sheet swap, so this ships `Fixed` on the plain
/// base texture; overlay compositing is a separate seam from the temperature
/// `ByVariant` swap and is deferred until the shell needs it.
pub fn villager_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [32.0, 0.0]).grown(0.51))
                .with_child(
                    "hat_rim",
                    PartDef::new(PartPose::rotation(-PI / 2.0, 0.0, 0.0)).with_cube(cube(
                        [-8.0, -8.0, -6.0],
                        [16.0, 16.0, 1.0],
                        [30.0, 47.0],
                    )),
                ),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, -2.0, 0.0)).with_cube(cube(
                [-1.0, -1.0, -6.0],
                [2.0, 4.0, 2.0],
                [24.0, 0.0],
            )),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_child(
            "jacket",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.5)),
        );
    let arms = PartDef::new(PartPose::offset_and_rotation(
        0.0, 3.0, -1.0, -0.75, 0.0, 0.0,
    ))
    .with_cube(cube([-8.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]))
    .with_cube(cube([4.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]).mirrored())
    .with_cube(cube([-4.0, 2.0, -2.0], [8.0, 4.0, 4.0], [40.0, 38.0]));
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0)).with_cube(cube(
        [-2.0, 0.0, -2.0],
        [4.0, 12.0, 4.0],
        [0.0, 22.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored());
    let model = EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("head", head)
            .with_child("body", body)
            .with_child("arms", arms)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    };
    scaled(model, 0.9375)
}

/// `ZombieVillagerModel.createBodyLayer` (`monster/zombie/ZombieVillagerModel.java`),
/// sheet 64×64, **no** villager scale (unlike the live villager). Built on the
/// humanoid layout but every part is re-specified: the head holds its own nose
/// cube (tex 24,0) as a second box, a hat (deform 0.5) with a flat brim, a body
/// with a jacket overlay (deform 0.05), straight zombie arms and standard legs.
/// 10 boxes.
///
/// Profession/type overlays composite over this base like the live villager, so
/// it also ships `Fixed` on the plain base sheet.
pub fn zombie_villager_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_cube(cube([-1.0, -3.0, -6.0], [2.0, 4.0, 2.0], [24.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [32.0, 0.0]).grown(0.5))
                .with_child(
                    "hat_rim",
                    PartDef::new(PartPose::rotation(-PI / 2.0, 0.0, 0.0)).with_cube(cube(
                        [-8.0, -8.0, -6.0],
                        [16.0, 16.0, 1.0],
                        [30.0, 47.0],
                    )),
                ),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.05));
    let right_arm = PartDef::new(PartPose::offset(-5.0, 2.0, 0.0)).with_cube(cube(
        [-3.0, -2.0, -2.0],
        [4.0, 12.0, 4.0],
        [44.0, 22.0],
    ));
    let left_arm = PartDef::new(PartPose::offset(5.0, 2.0, 0.0))
        .with_cube(cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [44.0, 22.0]).mirrored());
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0)).with_cube(cube(
        [-2.0, 0.0, -2.0],
        [4.0, 12.0, 4.0],
        [0.0, 22.0],
    ));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored());
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("head", head)
            .with_child("body", body)
            .with_child("right_arm", right_arm)
            .with_child("left_arm", left_arm)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    }
}

// ============================================================================
// animal/*, npc/*, object/* half (owned by this agent; impl-assets owns the
// piglin/guardian/witch/villager/golem/vex/phantom/ghast/silverfish/endermite/
// drowned/strider/warden/wither/ender_dragon lane above). The non-variant
// models (armor_stand, boats, minecart, end_crystal, rabbit..armadillo below)
// landed first; horse family, cat/wolf/ocelot and parrot followed once the
// texture-variant seam (`EntityTexture::ByVariant`, `EntityVariant`) was
// settled — see the second banner further down, right before
// `equine_base_root`.
// ============================================================================

/// `EndCrystalModel.createBodyLayer`: `outer_glass` (8³) with a nested
/// `inner_glass` (same box, `withScale(0.875)`) and a further-nested `cube`
/// (`withScale(0.765625)` = `0.875²`, a literal in vanilla, not computed) plus
/// a separate `base` box. Sheet 64×32.
pub fn end_crystal_model() -> EntityModelDef {
    let glass_cube = || cube([-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]);
    let mut inner_glass = PartDef::new(PartPose {
        scale: [0.875, 0.875, 0.875],
        ..PartPose::ZERO
    })
    .with_cube(glass_cube());
    inner_glass = inner_glass.with_child(
        "cube",
        PartDef::new(PartPose {
            scale: [0.765625, 0.765625, 0.765625],
            ..PartPose::ZERO
        })
        .with_cube(cube([-4.0, -4.0, -4.0], [8.0, 8.0, 8.0], [32.0, 0.0])),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "outer_glass",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(glass_cube())
                .with_child("inner_glass", inner_glass),
        )
        .with_child(
            "base",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-6.0, 0.0, -6.0],
                [12.0, 4.0, 12.0],
                [0.0, 16.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `ArmorStandModel.createBodyLayer`: starts from `HumanoidModel.createMesh`
/// but overrides head/body/arms/legs entirely with armor-stand-specific boxes
/// and adds `right_body_stick`/`left_body_stick`/`shoulder_stick`/`base_plate`.
/// The inherited `hat` child from the base humanoid mesh survives the
/// `addOrReplaceChild("head", ...)` merge (vanilla's `PartDefinition` keeps a
/// replaced node's *children*), but the constructor unconditionally sets
/// `this.hat.visible = false` and nothing ever re-enables it — so `hat` is
/// excluded here rather than baked as a permanently-invisible box. Sheet
/// 64×64.
pub fn armor_stand_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 1.0, 0.0)).with_cube(cube(
                [-1.0, -7.0, -1.0],
                [2.0, 7.0, 2.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-6.0, 0.0, -1.5],
                [12.0, 3.0, 3.0],
                [0.0, 26.0],
            )),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0)).with_cube(cube(
                [-2.0, -2.0, -1.0],
                [2.0, 12.0, 2.0],
                [24.0, 0.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0))
                .with_cube(cube([0.0, -2.0, -1.0], [2.0, 12.0, 2.0], [32.0, 16.0]).mirrored()),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-1.9, 12.0, 0.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 11.0, 2.0],
                [8.0, 0.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(1.9, 12.0, 0.0))
                .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 11.0, 2.0], [40.0, 16.0]).mirrored()),
        )
        .with_child(
            "right_body_stick",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-3.0, 3.0, -1.0],
                [2.0, 7.0, 2.0],
                [16.0, 0.0],
            )),
        )
        .with_child(
            "left_body_stick",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [1.0, 3.0, -1.0],
                [2.0, 7.0, 2.0],
                [48.0, 16.0],
            )),
        )
        .with_child(
            "shoulder_stick",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, 10.0, -1.0],
                [8.0, 2.0, 2.0],
                [0.0, 48.0],
            )),
        )
        .with_child(
            "base_plate",
            PartDef::new(PartPose::offset(0.0, 12.0, 0.0)).with_cube(cube(
                [-6.0, 11.0, -6.0],
                [12.0, 1.0, 12.0],
                [0.0, 32.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// The hull + paddles shared by `BoatModel.addCommonParts` (also reused,
/// structurally, by the chest-boat variant which just appends chest boxes).
/// Sheet 128×64 for the plain boat.
fn boat_hull() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                3.0,
                1.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-14.0, -9.0, -3.0], [28.0, 16.0, 3.0], [0.0, 0.0])),
        )
        .with_child(
            "back",
            PartDef::new(PartPose::offset_and_rotation(
                -15.0,
                4.0,
                4.0,
                0.0,
                PI * 3.0 / 2.0,
                0.0,
            ))
            .with_cube(cube([-13.0, -7.0, -1.0], [18.0, 6.0, 2.0], [0.0, 19.0])),
        )
        .with_child(
            "front",
            PartDef::new(PartPose::offset_and_rotation(
                15.0,
                4.0,
                0.0,
                0.0,
                PI / 2.0,
                0.0,
            ))
            .with_cube(cube([-8.0, -7.0, -1.0], [16.0, 6.0, 2.0], [0.0, 27.0])),
        )
        .with_child(
            "right",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, -9.0, 0.0, PI, 0.0))
                .with_cube(cube([-14.0, -7.0, -1.0], [28.0, 6.0, 2.0], [0.0, 35.0])),
        )
        .with_child(
            "left",
            PartDef::new(PartPose::offset(0.0, 4.0, 9.0)).with_cube(cube(
                [-14.0, -7.0, -1.0],
                [28.0, 6.0, 2.0],
                [0.0, 43.0],
            )),
        )
        .with_child(
            "left_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -5.0,
                9.0,
                0.0,
                0.0,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [62.0, 0.0]))
            .with_cube(cube([-1.001, -3.0, 8.0], [1.0, 6.0, 7.0], [62.0, 0.0])),
        )
        .with_child(
            "right_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -5.0,
                -9.0,
                0.0,
                PI,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [62.0, 20.0]))
            .with_cube(cube([0.001, -3.0, 8.0], [1.0, 6.0, 7.0], [62.0, 20.0])),
        )
}

/// `BoatModel.createBoatModel`: hull + both paddles, no chest. Sheet 128×64.
/// (`createWaterPatch`'s own invisible clip quad is [`boat_water_patch_model`],
/// a separate corpus entry rather than a child of this part tree — see its
/// own doc for why.)
pub fn boat_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: boat_hull(),
    }
}

/// `BoatModel.createWaterPatch`: an invisible depth-only mask shaped like a
/// **mirror** of the hull's own `bottom` plank — same box, offset the other
/// way (`y = -3.0` here against `bottom`'s `y = 3.0`) — that fills the boat's
/// hollow interior. Owner report: "placing down a boat still shows water
/// through the bottom". The hull is five thin planks around an open top, so
/// looking down (or across, at a grazing angle) into an occupied or empty
/// boat has a real gap between them; without this patch, the translucent
/// water surface underneath draws straight through it. Vanilla closes the
/// gap not with a visible floor but with **depth only**: this model is
/// submitted through `EntityPipeline::water_mask_pipeline` (colour writes
/// disabled, depth writes on, texture bound but never sampled into the
/// framebuffer) rather than the normal textured entity pipeline, so it
/// occludes the water pass's depth test while remaining itself invisible —
/// whatever would have been visible with no boat there at all (sky, terrain,
/// nothing) still shows through the hollow, exactly as vanilla's is.
///
/// A **separate corpus entry**, not a child part of [`boat_model`]/
/// [`chest_boat_model`]'s own tree: every part of one `PartDef` draws through
/// the *same* pipeline in one batch (`prepare_entities`), and this needs a
/// different one. `BoatRenderer`'s own constructor bakes one shared
/// `ModelLayers.BOAT_WATER_PATCH` regardless of chest-or-not
/// (`.cache/mc/26.2/client-src`'s `BoatRenderer.java`), so one entry serves
/// both here too.
///
/// **Rafts get none of this.** `RaftRenderer` does not override
/// `AbstractBoatRenderer.submitTypeAdditions`, whose default body is empty —
/// so `"raft"`/`"chest_raft"` never resolve to this model, matching vanilla's
/// real (if inconsistent) behaviour: a raft's water is not masked either.
pub fn boat_water_patch_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child(
            "water_patch",
            PartDef::new(PartPose::offset_and_rotation(0.0, -3.0, 1.0, PI / 2.0, 0.0, 0.0))
                .with_cube(cube([-14.0, -9.0, -3.0], [28.0, 16.0, 3.0], [0.0, 0.0])),
        ),
    }
}

/// `BoatModel.createChestBoatModel`: the same hull plus `chest_bottom`/
/// `chest_lid`/`chest_lock`. Sheet promotes to 128×128 to fit the chest.
pub fn chest_boat_model() -> EntityModelDef {
    let root = boat_hull()
        .with_child(
            "chest_bottom",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -5.0,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 8.0, 12.0], [0.0, 76.0])),
        )
        .with_child(
            "chest_lid",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -9.0,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 4.0, 12.0], [0.0, 59.0])),
        )
        .with_child(
            "chest_lock",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                -6.0,
                -1.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 4.0, 1.0], [0.0, 59.0])),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// The hull + paddles shared by `RaftModel.addCommonParts`. Same part names as
/// `BoatModel` but a different hull shape (2 boxes, bamboo raft's flatter
/// profile) and paddle texOffs. The `1.5708F` bottom x-rotation is transcribed
/// verbatim (vanilla writes the literal, not `Math.PI / 2`).
#[allow(
    clippy::approx_constant,
    reason = "1.5708 is vanilla's own literal in RaftModel.java, not Math.PI/2 — transcribed verbatim, not the nearby true constant"
)]
fn raft_hull() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, -2.1, 1.0, 1.5708, 0.0, 0.0,
            ))
            .with_cube(cube([-14.0, -11.0, -4.0], [28.0, 20.0, 4.0], [0.0, 0.0]))
            .with_cube(cube([-14.0, -9.0, -8.0], [28.0, 16.0, 4.0], [0.0, 0.0])),
        )
        .with_child(
            "left_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -4.0,
                9.0,
                0.0,
                0.0,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [0.0, 24.0]))
            .with_cube(cube([-1.001, -3.0, 8.0], [1.0, 6.0, 7.0], [0.0, 24.0])),
        )
        .with_child(
            "right_paddle",
            PartDef::new(PartPose::offset_and_rotation(
                3.0,
                -4.0,
                -9.0,
                0.0,
                PI,
                PI / 16.0,
            ))
            .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 2.0, 18.0], [40.0, 24.0]))
            .with_cube(cube([0.001, -3.0, 8.0], [1.0, 6.0, 7.0], [40.0, 24.0])),
        )
}

/// `RaftModel.createRaftModel`: sheet 128×64.
pub fn raft_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root: raft_hull(),
    }
}

/// `RaftModel.createChestRaftModel`: same hull plus chest boxes at raft-specific
/// heights. Sheet 128×128.
pub fn chest_raft_model() -> EntityModelDef {
    let root = raft_hull()
        .with_child(
            "chest_bottom",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -10.1,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 8.0, 12.0], [0.0, 76.0])),
        )
        .with_child(
            "chest_lid",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -14.1,
                -6.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [12.0, 4.0, 12.0], [0.0, 59.0])),
        )
        .with_child(
            "chest_lock",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                -11.1,
                -1.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 4.0, 1.0], [0.0, 59.0])),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `MinecartModel.createBodyLayer`: bottom + 4 walls, flat root. Vanilla's
/// chest/hopper/tnt/furnace/command-block/spawner minecarts all reuse this
/// exact geometry class (they differ only by a separate block-overlay render
/// layer), so a single `"minecart"` entry covers them —
/// `lodestone_render::entity::canonical_model_name` aliases every
/// server-simulated subtype onto this rig. The overlay itself is not part of
/// this corpus entry: it is a block model, not a second cuboid rig, and draws
/// through `crates/lodestone-shell/src/gpu/moving_blocks.rs`'s
/// `merge_minecart_contents` instead. Sheet 64×32.
pub fn minecart_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                4.0,
                0.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-10.0, -8.0, -1.0], [20.0, 16.0, 2.0], [0.0, 10.0])),
        )
        .with_child(
            "front",
            PartDef::new(PartPose::offset_and_rotation(
                -9.0,
                4.0,
                0.0,
                0.0,
                PI * 3.0 / 2.0,
                0.0,
            ))
            .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "back",
            PartDef::new(PartPose::offset_and_rotation(
                9.0,
                4.0,
                0.0,
                0.0,
                PI / 2.0,
                0.0,
            ))
            .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "left",
            PartDef::new(PartPose::offset_and_rotation(0.0, 4.0, -7.0, 0.0, PI, 0.0))
                .with_cube(cube([-8.0, -9.0, -1.0], [16.0, 8.0, 2.0], [0.0, 0.0])),
        )
        .with_child(
            "right",
            PartDef::new(PartPose::offset(0.0, 4.0, 7.0)).with_cube(cube(
                [-8.0, -9.0, -1.0],
                [16.0, 8.0, 2.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// `AdultRabbitModel.createBodyLayer`: body(+tail), head(+ears), and two
/// pivot-only leg-group nodes (`frontlegs`/`backlegs`/`right_hind_leg`/
/// `left_hind_leg`) whose only cubes live on the leaf `*_leg`/`*_haunch`
/// parts. Sheet 64×64.
#[allow(
    clippy::approx_constant,
    reason = "the 0.3927/-0.3927 rotations are vanilla's own literals in AdultRabbitModel.java, not Math.PI/8 — transcribed verbatim"
)]
pub fn rabbit_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0, 23.0, 4.0, -0.3927, 0.0, 0.0,
    ))
    .with_cube(cube([-4.0, -6.0, -9.0], [8.0, 6.0, 10.0], [0.0, 0.0]))
    .with_child(
        "tail",
        PartDef::new(PartPose::offset(0.0, -4.9916, 0.0125)).with_cube(cube(
            [-2.0, -3.0084, -1.0125],
            [4.0, 4.0, 4.0],
            [20.0, 16.0],
        )),
    )
    .with_child(
        "head",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, -5.2929, -8.1213, 0.3927, 0.0, 0.0,
        ))
        .with_cube(cube([-2.5, -3.0, -4.0], [5.0, 5.0, 5.0], [0.0, 16.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.5, -3.7071, -0.8787)).with_cube(cube(
                [-1.0, -4.2929, -0.1213],
                [2.0, 5.0, 1.0],
                [32.0, 0.0],
            )),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.5, -3.7071, -0.8787)).with_cube(cube(
                [-1.0, -4.2929, -0.1213],
                [2.0, 5.0, 1.0],
                [26.0, 0.0],
            )),
        ),
    )
    .with_child(
        "frontlegs",
        PartDef::new(PartPose::offset(0.0, -1.5349, -6.3108))
            .with_child(
                "right_front_leg",
                PartDef::new(PartPose::offset_and_rotation(
                    -2.0, 1.9239, 0.3827, 0.3927, 0.0, 0.0,
                ))
                .with_cube(cube(
                    [-0.9, -1.0, -0.9],
                    [2.0, 4.0, 2.0],
                    [36.0, 18.0],
                )),
            )
            .with_child(
                "left_front_leg",
                PartDef::new(PartPose::offset_and_rotation(
                    2.0, 1.9239, 0.4827, 0.3927, 0.0, 0.0,
                ))
                .with_cube(cube(
                    [-1.0, -1.0, -1.0],
                    [2.0, 4.0, 2.0],
                    [44.0, 18.0],
                )),
            ),
    );
    let backlegs = PartDef::new(PartPose::offset(0.0, 23.0, 4.0))
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 0.5, 0.0)).with_child(
                "right_haunch",
                PartDef::new(PartPose::offset_and_rotation(
                    0.0, -0.5, 0.0, 0.0, 0.3927, 0.0,
                ))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 1.0, 6.0], [20.0, 24.0])),
            ),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.0, 0.5, 0.0)).with_child(
                "left_haunch",
                PartDef::new(PartPose::offset_and_rotation(
                    0.0, -0.5, 0.0, 0.0, -0.3927, 0.0,
                ))
                .with_cube(cube([-1.0, 0.0, -5.0], [2.0, 1.0, 6.0], [36.0, 24.0])),
            ),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("backlegs", backlegs);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultFoxModel.createBodyLayer`: head(+ears+nose), body(+tail), and 4 legs.
/// `leftLeg`/`rightLeg` are each a single vanilla `CubeListBuilder` reused
/// across the hind and front leg *on the same side* (identical box, distinct
/// per-side texOffs); the sides are not related by `mirror()` at all. Sheet
/// 48×32.
pub fn fox_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(-1.0, 16.5, -3.0))
        .with_cube(cube([-3.0, -2.0, -5.0], [8.0, 6.0, 6.0], [1.0, 5.0]))
        .with_child(
            "right_ear",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-3.0, -4.0, -4.0],
                [2.0, 2.0, 1.0],
                [8.0, 1.0],
            )),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [3.0, -4.0, -4.0],
                [2.0, 2.0, 1.0],
                [15.0, 1.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-1.0, 2.01, -8.0],
                [4.0, 2.0, 3.0],
                [6.0, 18.0],
            )),
        );
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        16.0,
        -6.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-3.0, 3.999, -3.5], [6.0, 11.0, 6.0], [24.0, 15.0]))
    .with_child(
        "tail",
        PartDef::new(PartPose::offset_and_rotation(
            -4.0,
            15.0,
            -1.0,
            -0.05235988,
            0.0,
            0.0,
        ))
        .with_cube(cube([2.0, 0.0, -1.0], [4.0, 9.0, 5.0], [30.0, 0.0])),
    );
    let leg_fudge = 0.001;
    let left_leg = || cube([2.0, 0.5, -1.0], [2.0, 6.0, 2.0], [4.0, 24.0]).grown(leg_fudge);
    let right_leg = || cube([2.0, 0.5, -1.0], [2.0, 6.0, 2.0], [13.0, 24.0]).grown(leg_fudge);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.0, 17.5, 7.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(-1.0, 17.5, 7.0)).with_cube(left_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.0, 17.5, 0.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(-1.0, 17.5, 0.0)).with_cube(left_leg()),
        );
    EntityModelDef {
        texture_width: 48,
        texture_height: 32,
        root,
    }
}

/// `PandaModel.createBodyLayer` (`QuadrupedModel`): head(+nose+2 ears), body,
/// and 4 legs sharing one vanilla `CubeListBuilder` (no mirroring, identical
/// box on all four). Sheet 64×64.
pub fn panda_model() -> EntityModelDef {
    let leg = || cube([-3.0, 0.0, -3.0], [6.0, 9.0, 6.0], [40.0, 0.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 11.5, -17.0))
                .with_cube(cube([-6.5, -5.0, -4.0], [13.0, 10.0, 9.0], [0.0, 6.0]))
                .with_cube(cube([-3.5, 0.0, -6.0], [7.0, 5.0, 2.0], [45.0, 16.0]))
                .with_cube(cube([3.5, -8.0, -1.0], [5.0, 4.0, 1.0], [52.0, 25.0]))
                .with_cube(cube([-8.5, -8.0, -1.0], [5.0, 4.0, 1.0], [52.0, 25.0])),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                10.0,
                0.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-9.5, -13.0, -6.5], [19.0, 26.0, 13.0], [0.0, 25.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-5.5, 15.0, 9.0)).with_cube(leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(5.5, 15.0, 9.0)).with_cube(leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.5, 15.0, -9.0)).with_cube(leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(5.5, 15.0, -9.0)).with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `GoatModel.createBodyLayer` (`QuadrupedModel`): head builds 3 boxes on one
/// `CubeListBuilder` — `right_ear` (no mirror), `left_ear` (`.mirror()`), then
/// `goatee` — and vanilla's `mirror()` flag is sticky per-builder with no
/// reset, so `goatee` inherits `mirror=true` too. It's a zero-width box so
/// this is visually inert, but transcribed faithfully rather than "corrected"
/// away. Head also carries left_horn/right_horn/nose children (each
/// independently toggleable via `hasLeftHorn`/`hasRightHorn` at runtime, both
/// baked here since this registry has no per-part visibility toggle). Sheet
/// 64×64.
pub fn goat_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(1.0, 14.0, 0.0))
        .with_cube(cube([-6.0, -11.0, -10.0], [3.0, 2.0, 1.0], [2.0, 61.0]))
        .with_cube(cube([2.0, -11.0, -10.0], [3.0, 2.0, 1.0], [2.0, 61.0]).mirrored())
        .with_cube(cube([-0.5, -3.0, -14.0], [0.0, 7.0, 5.0], [23.0, 52.0]).mirrored())
        .with_child(
            "left_horn",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-0.01, -16.0, -10.0],
                [2.0, 7.0, 2.0],
                [12.0, 55.0],
            )),
        )
        .with_child(
            "right_horn",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-2.99, -16.0, -10.0],
                [2.0, 7.0, 2.0],
                [12.0, 55.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, -8.0, -8.0, 0.9599, 0.0, 0.0,
            ))
            .with_cube(cube([-3.0, -4.0, -8.0], [5.0, 7.0, 10.0], [34.0, 46.0])),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-4.0, -17.0, -7.0], [9.0, 11.0, 16.0], [1.0, 1.0]))
                .with_cube(cube([-5.0, -18.0, -8.0], [11.0, 14.0, 11.0], [0.0, 28.0])),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(1.0, 14.0, 4.0)).with_cube(cube(
                [0.0, 4.0, 0.0],
                [3.0, 6.0, 3.0],
                [36.0, 29.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.0, 14.0, 4.0)).with_cube(cube(
                [0.0, 4.0, 0.0],
                [3.0, 6.0, 3.0],
                [49.0, 29.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(1.0, 14.0, -6.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [3.0, 10.0, 3.0],
                [49.0, 2.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.0, 14.0, -6.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [3.0, 10.0, 3.0],
                [35.0, 2.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultBeeModel.createBodyLayer`: `bone` > `body`(stinger, left/right
/// antenna) plus `bone` > wings/legs. Sheet 64×64.
pub fn bee_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-3.5, -4.0, -5.0], [7.0, 7.0, 10.0], [0.0, 0.0]))
        .with_child(
            "stinger",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [0.0, -1.0, 5.0],
                [0.0, 1.0, 2.0],
                [26.0, 7.0],
            )),
        )
        .with_child(
            "left_antenna",
            PartDef::new(PartPose::offset(0.0, -2.0, -5.0)).with_cube(cube(
                [1.5, -2.0, -3.0],
                [1.0, 2.0, 3.0],
                [2.0, 0.0],
            )),
        )
        .with_child(
            "right_antenna",
            PartDef::new(PartPose::offset(0.0, -2.0, -5.0)).with_cube(cube(
                [-2.5, -2.0, -3.0],
                [1.0, 2.0, 3.0],
                [2.0, 3.0],
            )),
        );
    let wing_fudge = 0.001;
    let bone = PartDef::new(PartPose::offset(0.0, 19.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset_and_rotation(
                -1.5, -4.0, -3.0, 0.0, -0.2618, 0.0,
            ))
            .with_cube(cube([-9.0, 0.0, 0.0], [9.0, 0.0, 6.0], [0.0, 18.0]).grown(wing_fudge)),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset_and_rotation(
                1.5, -4.0, -3.0, 0.0, 0.2618, 0.0,
            ))
            .with_cube(
                cube([0.0, 0.0, 0.0], [9.0, 0.0, 6.0], [0.0, 18.0])
                    .grown(wing_fudge)
                    .mirrored(),
            ),
        )
        .with_child(
            "front_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, -2.0)).with_cube(cube(
                [-5.0, 0.0, 0.0],
                [7.0, 2.0, 0.0],
                [26.0, 1.0],
            )),
        )
        .with_child(
            "middle_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, 0.0)).with_cube(cube(
                [-5.0, 0.0, 0.0],
                [7.0, 2.0, 0.0],
                [26.0, 3.0],
            )),
        )
        .with_child(
            "back_legs",
            PartDef::new(PartPose::offset(1.5, 3.0, 2.0)).with_cube(cube(
                [-5.0, 0.0, 0.0],
                [7.0, 2.0, 0.0],
                [26.0, 5.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("bone", bone);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultTurtleModel.createBodyLayer`: head, body(shell+belly), egg_belly
/// (visibility-toggled at runtime by `hasEgg`, baked unconditionally here) and
/// 4 legs. Sheet 128×64.
pub fn turtle_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 19.0, -10.0)).with_cube(cube(
                [-3.0, -1.0, -3.0],
                [6.0, 5.0, 6.0],
                [3.0, 0.0],
            )),
        )
        .with_child(
            "body",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                11.0,
                -10.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-9.5, 3.0, -10.0], [19.0, 20.0, 6.0], [7.0, 37.0]))
            .with_cube(cube([-5.5, 3.0, -13.0], [11.0, 18.0, 3.0], [31.0, 1.0])),
        )
        .with_child(
            "egg_belly",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                11.0,
                -10.0,
                PI / 2.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.5, 3.0, -14.0], [9.0, 18.0, 1.0], [70.0, 33.0])),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.5, 22.0, 11.0)).with_cube(cube(
                [-2.0, 0.0, 0.0],
                [4.0, 1.0, 10.0],
                [1.0, 23.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.5, 22.0, 11.0)).with_cube(cube(
                [-2.0, 0.0, 0.0],
                [4.0, 1.0, 10.0],
                [1.0, 12.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-5.0, 21.0, -4.0)).with_cube(cube(
                [-13.0, 0.0, -2.0],
                [13.0, 1.0, 5.0],
                [27.0, 30.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(5.0, 21.0, -4.0)).with_cube(cube(
                [0.0, 0.0, -2.0],
                [13.0, 1.0, 5.0],
                [27.0, 24.0],
            )),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// `AdultCamelModel.createBodyMesh`: body(+hump+tail), head (3 stacked boxes:
/// muzzle/skull/snout + ears), 4 legs. Sheet 128×128.
pub fn camel_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -3.0, -19.5))
        .with_cube(cube([-3.5, -7.0, -15.0], [7.0, 8.0, 19.0], [60.0, 24.0]))
        .with_cube(cube([-3.5, -21.0, -15.0], [7.0, 14.0, 7.0], [21.0, 0.0]))
        .with_cube(cube([-2.5, -21.0, -21.0], [5.0, 5.0, 6.0], [50.0, 0.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(2.5, -21.0, -9.5)).with_cube(cube(
                [-0.5, 0.5, -1.0],
                [3.0, 1.0, 2.0],
                [45.0, 0.0],
            )),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-2.5, -21.0, -9.5)).with_cube(cube(
                [-2.5, 0.5, -1.0],
                [3.0, 1.0, 2.0],
                [67.0, 0.0],
            )),
        );
    let body = PartDef::new(PartPose::offset(0.0, 4.0, 9.5))
        .with_cube(cube([-7.5, -12.0, -23.5], [15.0, 12.0, 27.0], [0.0, 25.0]))
        .with_child(
            "hump",
            PartDef::new(PartPose::offset(0.0, -12.0, -10.0)).with_cube(cube(
                [-4.5, -5.0, -5.5],
                [9.0, 5.0, 11.0],
                [74.0, 0.0],
            )),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, -9.0, 3.5)).with_cube(cube(
                [-1.5, 0.0, 0.0],
                [3.0, 14.0, 0.0],
                [122.0, 0.0],
            )),
        )
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.9, 1.0, 9.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [58.0, 16.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.9, 1.0, 9.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [94.0, 16.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.9, 1.0, -10.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.9, 1.0, -10.5)).with_cube(cube(
                [-2.5, 2.0, -2.5],
                [5.0, 21.0, 5.0],
                [0.0, 26.0],
            )),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `CodModel.createBodyLayer`: body, head, nose, 2 side fins, tail_fin, and a
/// `top_fin` at `texOffs(20, -6)` on a `0×1×6` box. The **negative Y offset is
/// vanilla's own value**, not a transcription slip: `top_fin`'s real
/// (non-degenerate) faces are the ones spanning the depth×height texel rect
/// that this offset addresses, and there is no non-wrapping `texOffs` vanilla
/// could have chosen instead that keeps that rect on-sheet — see the same
/// wraparound note on `salmon`'s `right_fin`. `every_uv_is_within_the_sheet`
/// allows a small texel margin that covers exactly this. Sheet 32×32.
pub fn cod_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-1.0, -2.0, 0.0],
                [2.0, 4.0, 7.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-1.0, -2.0, -3.0],
                [2.0, 4.0, 3.0],
                [11.0, 0.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0)).with_cube(cube(
                [-1.0, -2.0, -1.0],
                [2.0, 3.0, 1.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                23.0,
                0.0,
                0.0,
                0.0,
                -PI / 4.0,
            ))
            .with_cube(cube([-2.0, 0.0, -1.0], [2.0, 0.0, 2.0], [22.0, 1.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                1.0,
                23.0,
                0.0,
                0.0,
                0.0,
                PI / 4.0,
            ))
            .with_cube(cube([0.0, 0.0, -1.0], [2.0, 0.0, 2.0], [22.0, 4.0])),
        )
        .with_child(
            "tail_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, 7.0)).with_cube(cube(
                [0.0, -2.0, 0.0],
                [0.0, 4.0, 4.0],
                [22.0, 3.0],
            )),
        )
        .with_child(
            "top_fin",
            PartDef::new(PartPose::offset(0.0, 20.0, 0.0)).with_cube(cube(
                [0.0, -1.0, -1.0],
                [0.0, 1.0, 6.0],
                [20.0, -6.0],
            )),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `SalmonModel.createBodyLayer`: body_front/body_back (each with a fin
/// child), head, and 2 side fins. `right_fin` is `texOffs(-4, 0)` on a
/// `2×0×2` box — vanilla relying on `GL_REPEAT` texture wrap for this one
/// real (non-degenerate, since height is the zero dimension here, not width)
/// face rect; verified there is no equivalent non-wrapping offset that fits
/// the required texel span before the sheet boundary. Named exception for
/// `"salmon"` in `every_uv_is_within_the_sheet`. Sheet 32×32.
pub fn salmon_model() -> EntityModelDef {
    let body_front = PartDef::new(PartPose::offset(0.0, 20.0, -7.2))
        .with_cube(cube([-1.5, -2.5, 0.0], [3.0, 5.0, 8.0], [0.0, 0.0]))
        .with_child(
            "top_front_fin",
            PartDef::new(PartPose::offset(0.0, -4.5, 5.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 2.0, 3.0],
                [2.0, 1.0],
            )),
        );
    let body_back = PartDef::new(PartPose::offset(0.0, 20.0, 0.8000002))
        .with_cube(cube([-1.5, -2.5, 0.0], [3.0, 5.0, 8.0], [0.0, 13.0]))
        .with_child(
            "back_fin",
            PartDef::new(PartPose::offset(0.0, 0.0, 8.0)).with_cube(cube(
                [0.0, -2.5, 0.0],
                [0.0, 5.0, 6.0],
                [20.0, 10.0],
            )),
        )
        .with_child(
            "top_back_fin",
            PartDef::new(PartPose::offset(0.0, -4.5, -1.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 2.0, 4.0],
                [0.0, 2.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body_front", body_front)
        .with_child("body_back", body_back)
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 20.0, -7.2)).with_cube(cube(
                [-1.0, -2.0, -3.0],
                [2.0, 4.0, 3.0],
                [22.0, 0.0],
            )),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -1.5,
                21.5,
                -7.2,
                0.0,
                0.0,
                -PI / 4.0,
            ))
            .with_cube(cube([-2.0, 0.0, 0.0], [2.0, 0.0, 2.0], [-4.0, 0.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                1.5,
                21.5,
                -7.2,
                0.0,
                0.0,
                PI / 4.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 0.0, 2.0], [0.0, 0.0])),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `PufferfishBigModel.createBodyLayer`: body plus 3 mirrored fin pairs (blue,
/// front/back, top/bottom×3). Chosen as the registered pufferfish variant
/// specifically because — unlike `PufferfishSmallModel` (`back_fin` at
/// `texOffs(-3, 0)`) — none of its offsets are negative, so it needs no UV
/// exception. Sheet 32×32.
pub fn pufferfish_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-4.0, -8.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_blue_fin",
            PartDef::new(PartPose::offset(-4.0, 15.0, -2.0)).with_cube(cube(
                [-2.0, 0.0, -1.0],
                [2.0, 1.0, 2.0],
                [24.0, 0.0],
            )),
        )
        .with_child(
            "left_blue_fin",
            PartDef::new(PartPose::offset(4.0, 15.0, -2.0)).with_cube(cube(
                [0.0, 0.0, -1.0],
                [2.0, 1.0, 2.0],
                [24.0, 3.0],
            )),
        )
        .with_child(
            "top_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                14.0,
                -4.0,
                PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, -1.0, 0.0], [8.0, 1.0, 0.0], [15.0, 17.0])),
        )
        .with_child(
            "top_middle_fin",
            PartDef::new(PartPose::offset(0.0, 14.0, 0.0)).with_cube(cube(
                [-4.0, -1.0, 0.0],
                [8.0, 1.0, 1.0],
                [14.0, 16.0],
            )),
        )
        .with_child(
            "top_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                14.0,
                4.0,
                -PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, -1.0, 0.0], [8.0, 1.0, 0.0], [23.0, 18.0])),
        )
        .with_child(
            "right_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -4.0,
                22.0,
                -4.0,
                0.0,
                -PI / 4.0,
                0.0,
            ))
            .with_cube(cube([-1.0, -8.0, 0.0], [1.0, 8.0, 0.0], [5.0, 17.0])),
        )
        .with_child(
            "left_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                4.0,
                22.0,
                -4.0,
                0.0,
                PI / 4.0,
                0.0,
            ))
            .with_cube(cube([0.0, -8.0, 0.0], [1.0, 8.0, 0.0], [1.0, 17.0])),
        )
        .with_child(
            "bottom_front_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                22.0,
                -4.0,
                -PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, 0.0, 0.0], [8.0, 1.0, 0.0], [15.0, 20.0])),
        )
        .with_child(
            "bottom_middle_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, 0.0],
                [8.0, 1.0, 0.0],
                [15.0, 20.0],
            )),
        )
        .with_child(
            "bottom_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                22.0,
                4.0,
                PI / 4.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-4.0, 0.0, 0.0], [8.0, 1.0, 0.0], [15.0, 20.0])),
        )
        .with_child(
            "right_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -4.0,
                22.0,
                4.0,
                0.0,
                PI / 4.0,
                0.0,
            ))
            .with_cube(cube([-1.0, -8.0, 0.0], [1.0, 8.0, 0.0], [9.0, 17.0])),
        )
        .with_child(
            "left_back_fin",
            PartDef::new(PartPose::offset_and_rotation(
                4.0,
                22.0,
                4.0,
                0.0,
                -PI / 4.0,
                0.0,
            ))
            .with_cube(cube([0.0, -8.0, 0.0], [1.0, 8.0, 0.0], [9.0, 17.0])),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `TropicalFishLargeModel.createBodyLayer(CubeDeformation.NONE)` — the plain
/// (unpatterned) large tropical fish, chosen over `TropicalFishSmallModel`
/// (which has negative-Y `texOffs` on `tail`/`top_fin`) so this entry needs no
/// UV exception. Sheet 32×32.
pub fn tropical_fish_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 19.0, 0.0)).with_cube(cube(
                [-1.0, -3.0, -3.0],
                [2.0, 6.0, 6.0],
                [0.0, 20.0],
            )),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 19.0, 3.0)).with_cube(cube(
                [0.0, -3.0, 0.0],
                [0.0, 6.0, 5.0],
                [21.0, 16.0],
            )),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0,
                20.0,
                0.0,
                0.0,
                PI / 4.0,
                0.0,
            ))
            .with_cube(cube([-2.0, 0.0, 0.0], [2.0, 2.0, 0.0], [2.0, 16.0])),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                1.0,
                20.0,
                0.0,
                0.0,
                -PI / 4.0,
                0.0,
            ))
            .with_cube(cube([0.0, 0.0, 0.0], [2.0, 2.0, 0.0], [2.0, 12.0])),
        )
        .with_child(
            "top_fin",
            PartDef::new(PartPose::offset(0.0, 16.0, -3.0)).with_cube(cube(
                [0.0, -4.0, 0.0],
                [0.0, 4.0, 6.0],
                [20.0, 11.0],
            )),
        )
        .with_child(
            "bottom_fin",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0)).with_cube(cube(
                [0.0, 0.0, 0.0],
                [0.0, 4.0, 6.0],
                [20.0, 21.0],
            )),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `DolphinModel.createBodyLayer`: body(back_fin, mirrored left_fin,
/// right_fin, tail[tail_fin], head[nose]). Sheet 64×64.
pub fn dolphin_model() -> EntityModelDef {
    let tail = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        -2.5,
        11.0,
        -0.10471976,
        0.0,
        0.0,
    ))
    .with_cube(cube([-2.0, -2.5, 0.0], [4.0, 5.0, 11.0], [0.0, 19.0]))
    .with_child(
        "tail_fin",
        PartDef::new(PartPose::offset(0.0, 0.0, 9.0)).with_cube(cube(
            [-5.0, -0.5, 0.0],
            [10.0, 1.0, 6.0],
            [19.0, 20.0],
        )),
    );
    let head = PartDef::new(PartPose::offset(0.0, -4.0, -3.0))
        .with_cube(cube([-4.0, -3.0, -3.0], [8.0, 7.0, 6.0], [0.0, 0.0]))
        .with_child(
            "nose",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-1.0, 2.0, -7.0],
                [2.0, 2.0, 4.0],
                [0.0, 13.0],
            )),
        );
    let body = PartDef::new(PartPose::offset(0.0, 22.0, -5.0))
        .with_cube(cube([-4.0, -7.0, 0.0], [8.0, 7.0, 13.0], [22.0, 0.0]))
        .with_child(
            "back_fin",
            PartDef::new(PartPose::rotation(PI / 3.0, 0.0, 0.0)).with_cube(cube(
                [-0.5, 0.0, 8.0],
                [1.0, 4.0, 5.0],
                [51.0, 0.0],
            )),
        )
        .with_child(
            "left_fin",
            PartDef::new(PartPose::offset_and_rotation(
                2.0,
                -2.0,
                4.0,
                PI / 3.0,
                0.0,
                PI * 2.0 / 3.0,
            ))
            .with_cube(cube([-0.5, -4.0, 0.0], [1.0, 4.0, 7.0], [48.0, 20.0]).mirrored()),
        )
        .with_child(
            "right_fin",
            PartDef::new(PartPose::offset_and_rotation(
                -2.0,
                -2.0,
                4.0,
                PI / 3.0,
                0.0,
                -PI * 2.0 / 3.0,
            ))
            .with_cube(cube([-0.5, -4.0, 0.0], [1.0, 4.0, 7.0], [48.0, 20.0])),
        )
        .with_child("tail", tail)
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `AdultAxolotlModel.createBodyLayer`: body (main + a zero-width ridge box),
/// head (`grow=0.001` fudge, matching vanilla's flat-cube-z-fighting fix) with
/// 3 gill children, 4 legs (2 vanilla builders reused across front/hind on
/// each side, distinct origins — not `mirror()`-flagged), tail. Sheet 64×64.
pub fn axolotl_model() -> EntityModelDef {
    let fudge = 0.001;
    let head = PartDef::new(PartPose::offset(0.0, 0.0, -9.0))
        .with_cube(cube([-4.0, -3.0, -5.0], [8.0, 5.0, 5.0], [0.0, 1.0]).grown(fudge))
        .with_child(
            "top_gills",
            PartDef::new(PartPose::offset(0.0, -3.0, -1.0))
                .with_cube(cube([-4.0, -3.0, 0.0], [8.0, 3.0, 0.0], [3.0, 37.0]).grown(fudge)),
        )
        .with_child(
            "left_gills",
            PartDef::new(PartPose::offset(-4.0, 0.0, -1.0))
                .with_cube(cube([-3.0, -5.0, 0.0], [3.0, 7.0, 0.0], [0.0, 40.0]).grown(fudge)),
        )
        .with_child(
            "right_gills",
            PartDef::new(PartPose::offset(4.0, 0.0, -1.0))
                .with_cube(cube([0.0, -5.0, 0.0], [3.0, 7.0, 0.0], [11.0, 40.0]).grown(fudge)),
        );
    let left_leg = || cube([-1.0, 0.0, 0.0], [3.0, 5.0, 0.0], [2.0, 13.0]).grown(fudge);
    let right_leg = || cube([-2.0, 0.0, 0.0], [3.0, 5.0, 0.0], [2.0, 13.0]).grown(fudge);
    let body = PartDef::new(PartPose::offset(0.0, 19.5, 5.0))
        .with_cube(cube([-4.0, -2.0, -9.0], [8.0, 4.0, 10.0], [0.0, 11.0]))
        .with_cube(cube([0.0, -3.0, -8.0], [0.0, 5.0, 9.0], [2.0, 17.0]))
        .with_child("head", head)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.5, 1.0, -1.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.5, 1.0, -1.0)).with_cube(left_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.5, 1.0, -8.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.5, 1.0, -8.0)).with_cube(left_leg()),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 0.0, 1.0)).with_cube(cube(
                [0.0, -3.0, 0.0],
                [0.0, 5.0, 12.0],
                [2.0, 19.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("body", body);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// `FrogModel.createBodyLayer`: `root` (invisible pivot at y=24) > `body`(+
/// `head`[+`eyes`> left/right_eye], `croaking_body`, `tongue`, `left_arm`
/// [+`left_hand`], `right_arm`[+`right_hand`]), and `left_leg`/`right_leg`
/// (each +foot) as siblings of `body` under the same pivot node. `eyes` and
/// the `*_leg`/`*_arm` nodes are themselves pivot-only where noted. Sheet
/// 48×48 (**not** 64×64 — this is a small, easy-to-miss exception).
pub fn frog_model() -> EntityModelDef {
    let eyes = PartDef::new(PartPose::offset(-0.5, 0.0, 2.0))
        .with_child(
            "right_eye",
            PartDef::new(PartPose::offset(-1.5, -3.0, -6.5)).with_cube(cube(
                [-1.5, -1.0, -1.5],
                [3.0, 2.0, 3.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "left_eye",
            PartDef::new(PartPose::offset(2.5, -3.0, -6.5)).with_cube(cube(
                [-1.5, -1.0, -1.5],
                [3.0, 2.0, 3.0],
                [0.0, 5.0],
            )),
        );
    let head = PartDef::new(PartPose::offset(0.0, -2.0, -1.0))
        .with_cube(cube([-3.5, -1.0, -7.0], [7.0, 0.0, 9.0], [23.0, 13.0]))
        .with_cube(cube([-3.5, -2.0, -7.0], [7.0, 3.0, 9.0], [0.0, 13.0]))
        .with_child("eyes", eyes);
    let left_arm = PartDef::new(PartPose::offset(4.0, -1.0, -6.5))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 3.0], [0.0, 32.0]))
        .with_child(
            "left_hand",
            PartDef::new(PartPose::offset(0.0, 3.0, -1.0)).with_cube(cube(
                [-4.0, 0.01, -4.0],
                [8.0, 0.0, 8.0],
                [18.0, 40.0],
            )),
        );
    let right_arm = PartDef::new(PartPose::offset(-4.0, -1.0, -6.5))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 3.0, 3.0], [0.0, 38.0]))
        .with_child(
            "right_hand",
            PartDef::new(PartPose::offset(0.0, 3.0, 0.0)).with_cube(cube(
                [-4.0, 0.01, -5.0],
                [8.0, 0.0, 8.0],
                [2.0, 40.0],
            )),
        );
    let body = PartDef::new(PartPose::offset(0.0, -2.0, 4.0))
        .with_cube(cube([-3.5, -2.0, -8.0], [7.0, 3.0, 9.0], [3.0, 1.0]))
        .with_cube(cube([-3.5, -1.0, -8.0], [7.0, 0.0, 9.0], [23.0, 22.0]))
        .with_child("head", head)
        .with_child(
            "croaking_body",
            PartDef::new(PartPose::offset(0.0, -1.0, -5.0))
                .with_cube(cube([-3.5, -0.1, -2.9], [7.0, 2.0, 3.0], [26.0, 5.0]).grown(-0.1)),
        )
        .with_child(
            "tongue",
            PartDef::new(PartPose::offset(0.0, -1.01, 1.0)).with_cube(cube(
                [-2.0, 0.0, -7.1],
                [4.0, 0.0, 7.0],
                [17.0, 13.0],
            )),
        )
        .with_child("left_arm", left_arm)
        .with_child("right_arm", right_arm);
    let model_root = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(3.5, -3.0, 4.0))
                .with_cube(cube([-1.0, 0.0, -2.0], [3.0, 3.0, 4.0], [14.0, 25.0]))
                .with_child(
                    "left_foot",
                    PartDef::new(PartPose::offset(2.0, 3.0, 0.0)).with_cube(cube(
                        [-4.0, 0.01, -4.0],
                        [8.0, 0.0, 8.0],
                        [2.0, 32.0],
                    )),
                ),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-3.5, -3.0, 4.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [3.0, 3.0, 4.0], [0.0, 25.0]))
                .with_child(
                    "right_foot",
                    PartDef::new(PartPose::offset(-2.0, 3.0, 0.0)).with_cube(cube(
                        [-4.0, 0.01, -4.0],
                        [8.0, 0.0, 8.0],
                        [18.0, 32.0],
                    )),
                ),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("root", model_root);
    EntityModelDef {
        texture_width: 48,
        texture_height: 48,
        root,
    }
}

/// `TadpoleModel.createBodyLayer`: 2 flat boxes, no hierarchy. Sheet is
/// **16×16** — the smallest sheet in the whole corpus, easy to fat-finger as
/// 32×32.
pub fn tadpole_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "body",
            PartDef::new(PartPose::offset(0.0, 22.0, -3.0)).with_cube(cube(
                [-1.5, -1.0, 0.0],
                [3.0, 2.0, 3.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "tail",
            PartDef::new(PartPose::offset(0.0, 22.0, 0.0)).with_cube(cube(
                [0.0, -1.0, 0.0],
                [0.0, 2.0, 7.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: 16,
        texture_height: 16,
        root,
    }
}

/// `SnifferModel.createBodyLayer`: `bone` > `body`(3 boxes) + 6 legs, `body` >
/// `head`(2 boxes) > ears/nose/lower_beak. Sheet is **192×192** — by far the
/// largest sheet in the corpus.
pub fn sniffer_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 6.5, -19.48))
        .with_cube(cube([-6.5, -7.5, -11.5], [13.0, 18.0, 11.0], [8.0, 15.0]))
        .with_cube(cube([-6.5, 7.5, -11.5], [13.0, 0.0, 11.0], [8.0, 4.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(6.51, -7.5, -4.51)).with_cube(cube(
                [0.0, 0.0, -3.0],
                [1.0, 19.0, 7.0],
                [2.0, 0.0],
            )),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-6.51, -7.5, -4.51)).with_cube(cube(
                [-1.0, 0.0, -3.0],
                [1.0, 19.0, 7.0],
                [48.0, 0.0],
            )),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, -4.5, -11.5)).with_cube(cube(
                [-6.5, -2.0, -9.0],
                [13.0, 2.0, 9.0],
                [10.0, 45.0],
            )),
        )
        .with_child(
            "lower_beak",
            PartDef::new(PartPose::offset(0.0, 2.5, -12.5)).with_cube(cube(
                [-6.5, -7.0, -8.0],
                [13.0, 12.0, 9.0],
                [10.0, 57.0],
            )),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube(
            [-12.5, -14.0, -20.0],
            [25.0, 29.0, 40.0],
            [62.0, 68.0],
        ))
        .with_cube(cube([-12.5, -14.0, -20.0], [25.0, 24.0, 40.0], [62.0, 0.0]).grown(0.5))
        .with_cube(cube([-12.5, 12.0, -20.0], [25.0, 0.0, 40.0], [87.0, 68.0]))
        .with_child("head", head);
    let bone = PartDef::new(PartPose::offset(0.0, 5.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, -15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [32.0, 87.0],
            )),
        )
        .with_child(
            "right_mid_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, 0.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [32.0, 105.0],
            )),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-7.5, 10.0, 15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [32.0, 123.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, -15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [0.0, 87.0],
            )),
        )
        .with_child(
            "left_mid_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, 0.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [0.0, 105.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(7.5, 10.0, 15.0)).with_cube(cube(
                [-3.5, -1.0, -4.0],
                [7.0, 10.0, 8.0],
                [0.0, 123.0],
            )),
        );
    let root = PartDef::new(PartPose::ZERO).with_child("bone", bone);
    EntityModelDef {
        texture_width: 192,
        texture_height: 192,
        root,
    }
}

/// `AdultArmadilloModel.createBodyLayer`: body(+tail+head[+ears]), 4 legs, and
/// a separate root-level `cube` (the rolled-up ball form; vanilla toggles its
/// visibility with the roll animation state, baked unconditionally here since
/// this registry has no per-part runtime visibility). Sheet 64×64.
#[allow(
    clippy::approx_constant,
    reason = "-0.3927 is vanilla's own literal in AdultArmadilloModel.java, not Math.PI/8 — transcribed verbatim"
)]
pub fn armadillo_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -2.0, -11.0))
        .with_child(
            "head_cube",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, 0.0, 0.0, -0.3927, 0.0, 0.0,
            ))
            .with_cube(cube([-1.5, -1.0, -1.0], [3.0, 5.0, 2.0], [43.0, 15.0])),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::offset(-1.0, -1.0, 0.0)).with_child(
                "right_ear_cube",
                PartDef::new(PartPose::offset_and_rotation(
                    -0.5, 0.0, -0.6, 0.1886, -0.3864, -0.0718,
                ))
                .with_cube(cube([-2.0, -3.0, 0.0], [2.0, 5.0, 0.0], [43.0, 10.0])),
            ),
        )
        .with_child(
            "left_ear",
            PartDef::new(PartPose::offset(1.0, -2.0, 0.0)).with_child(
                "left_ear_cube",
                PartDef::new(PartPose::offset_and_rotation(
                    0.5, 1.0, -0.6, 0.1886, 0.3864, 0.0718,
                ))
                .with_cube(cube([0.0, -3.0, 0.0], [2.0, 5.0, 0.0], [47.0, 10.0])),
            ),
        );
    let body = PartDef::new(PartPose::offset(0.0, 21.0, 4.0))
        .with_cube(cube([-4.0, -7.0, -10.0], [8.0, 8.0, 12.0], [0.0, 20.0]).grown(0.3))
        .with_cube(cube([-4.0, -7.0, -10.0], [8.0, 8.0, 12.0], [0.0, 40.0]))
        .with_child(
            "tail",
            PartDef::new(PartPose::offset_and_rotation(
                0.0, -3.0, 1.0, 0.5061, 0.0, 0.0,
            ))
            .with_cube(cube([-0.5, -0.0865, 0.0933], [1.0, 6.0, 1.0], [44.0, 53.0])),
        )
        .with_child("head", head);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-2.0, 21.0, 4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [51.0, 31.0],
            )),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(2.0, 21.0, 4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [42.0, 31.0],
            )),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-2.0, 21.0, -4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [51.0, 43.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(2.0, 21.0, -4.0)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, 3.0, 2.0],
                [42.0, 43.0],
            )),
        )
        .with_child(
            "cube",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0)).with_cube(cube(
                [-5.0, -10.0, -6.0],
                [10.0, 10.0, 10.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

// ============================================================================
// The variant-heavy remainder of this agent's roster: horse family (horse,
// donkey, mule, skeleton_horse, zombie_horse, llama, trader_llama), cat, wolf,
// ocelot, parrot. Deferred until the `EntityTexture::ByVariant`/`EntityVariant`
// seam (in `entity.rs`) was settled; it has been, so these are ported here.
//
// Coordination note: `impl-assets` (who proposed the seam) was unreachable
// when this landed (agent no longer present in this session's roster), so the
// `HorseColor`/`LlamaColor`/`CatCoat`/`WolfCoat`/`WolfState`/`ParrotColor` enum
// shapes below were decided unilaterally from the decompiled source rather
// than confirmed with them first, as the task asked. Flagging this explicitly
// for review rather than presenting it as pre-agreed. One open design point:
// horse markings (`Markings.java`) genuinely need a *second*, independently
// selected texture layer composited over the base colour (`HorseMarkingLayer`
// submits a second translucent pass using the same model) — `ByVariant`
// resolves exactly one path per call, so it cannot express this. Rather than
// invent a new `EntityTexture` case unilaterally (a real seam decision that
// affects the shell/render consumer), `horse_markings_texture` below is a
// plain standalone function + `HorseMarkings` enum, deliberately *not* wired
// into `EntityTexture`/`EntityVariant`, ready for whoever implements the
// second render pass to call directly.
// ============================================================================

/// `AbstractEquineModel.createBodyMesh(CubeDeformation.NONE)`: the shared
/// horse/donkey/mule/skeleton_horse/zombie_horse body. Structural tree per
/// vanilla: `body` (with a `tail` child) and `head_parts` (rotated `PI/6` down,
/// with a `head` child that itself carries `left_ear`/`right_ear`, plus
/// `mane` and `upper_mouth` siblings of `head`) are both direct root children,
/// alongside four independent, unparented legs. The body cube's `(0.05)`
/// deformation is hardcoded in vanilla regardless of the mesh's own `g`
/// parameter (only used here with `g = NONE`, so it doesn't matter yet, but
/// transcribed as vanilla wrote it in case a future caller passes non-zero
/// `g`, e.g. for `HORSE_ARMOR`'s `CubeDeformation(0.1)`). Sheet 64×64.
fn equine_base_root() -> PartDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-3.0, -11.0, -2.0], [6.0, 5.0, 7.0], [0.0, 13.0]))
        .with_child(
            "left_ear",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([0.55, -13.0, 4.0], [2.0, 3.0, 1.0], [19.0, 16.0]).grown(-0.001)),
        )
        .with_child(
            "right_ear",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-2.55, -13.0, 4.0], [2.0, 3.0, 1.0], [19.0, 16.0]).grown(-0.001)),
        );
    let head_parts = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        4.0,
        -12.0,
        PI / 6.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-2.05, -6.0, -2.0], [4.0, 12.0, 7.0], [0.0, 35.0]))
    .with_child("head", head)
    .with_child(
        "mane",
        PartDef::new(PartPose::ZERO).with_cube(cube(
            [-1.0, -11.0, 5.01],
            [2.0, 16.0, 2.0],
            [56.0, 36.0],
        )),
    )
    .with_child(
        "upper_mouth",
        PartDef::new(PartPose::ZERO).with_cube(cube(
            [-2.0, -11.0, -7.0],
            [4.0, 5.0, 5.0],
            [0.0, 25.0],
        )),
    );
    let body = PartDef::new(PartPose::offset(0.0, 11.0, 5.0))
        .with_cube(cube([-5.0, -8.0, -17.0], [10.0, 10.0, 22.0], [0.0, 32.0]).grown(0.05))
        .with_child(
            "tail",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                -5.0,
                2.0,
                PI / 6.0,
                0.0,
                0.0,
            ))
            .with_cube(cube([-1.5, 0.0, 0.0], [3.0, 14.0, 4.0], [42.0, 36.0])),
        );
    PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("head_parts", head_parts)
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.0, 14.0, 7.0))
                .with_cube(cube([-3.0, -1.01, -1.0], [4.0, 11.0, 4.0], [48.0, 21.0]).mirrored()),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.0, 14.0, 7.0)).with_cube(cube(
                [-1.0, -1.01, -1.0],
                [4.0, 11.0, 4.0],
                [48.0, 21.0],
            )),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(4.0, 14.0, -10.0))
                .with_cube(cube([-3.0, -1.01, -1.9], [4.0, 11.0, 4.0], [48.0, 21.0]).mirrored()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-4.0, 14.0, -10.0)).with_cube(cube(
                [-1.0, -1.01, -1.9],
                [4.0, 11.0, 4.0],
                [48.0, 21.0],
            )),
        )
}

fn equine_base_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: equine_base_root(),
    }
}

/// Skeleton horse: the base equine mesh, unscaled (`UndeadHorseRenderer` bakes
/// `HorseModel` on `ModelLayers.SKELETON_HORSE`, which is
/// `AbstractEquineModel.createBodyMesh(NONE)` with no `MeshTransformer`
/// applied — `LayerDefinitions.java`'s `horseBodyLayer` reused as-is). Fixed
/// texture, no colour/markings variant.
pub fn skeleton_horse_model() -> EntityModelDef {
    equine_base_model()
}

/// Zombie horse: identical to skeleton horse — same unscaled base equine mesh,
/// same `horseBodyLayer` reuse in `LayerDefinitions.java`. Fixed texture.
pub fn zombie_horse_model() -> EntityModelDef {
    equine_base_model()
}

/// Horse: the base equine mesh baked at `scaling(1.1)`
/// (`LayerDefinitions.java`: `horseBodyLayer.apply(MeshTransformer.scaling(1.1F))`).
/// Colour is a real variant (`Horse.Variant`, 7 coats); markings
/// (`Markings`, 5 patterns incl. "none") are an independent second texture
/// layer — see the module-level note above `equine_base_root` and
/// `horse_markings_texture` below.
pub fn horse_model() -> EntityModelDef {
    scaled(equine_base_model(), 1.1)
}

fn horse_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::HorseColor(HorseColor::White) => "entity/horse/horse_white",
        EntityVariant::HorseColor(HorseColor::Creamy) => "entity/horse/horse_creamy",
        EntityVariant::HorseColor(HorseColor::Chestnut) => "entity/horse/horse_chestnut",
        EntityVariant::HorseColor(HorseColor::Brown) => "entity/horse/horse_brown",
        EntityVariant::HorseColor(HorseColor::Black) => "entity/horse/horse_black",
        EntityVariant::HorseColor(HorseColor::Gray) => "entity/horse/horse_gray",
        EntityVariant::HorseColor(HorseColor::DarkBrown) => "entity/horse/horse_darkbrown",
        _ => "entity/horse/horse_white",
    }
}

/// The horse markings overlay path, or `None` for no second pass
/// (`Markings.NONE` maps to vanilla's invisible-texture sentinel in
/// `HorseMarkingLayer.java`). Deliberately not an `EntityTexture`/
/// `EntityVariant` selector — see the module note above `equine_base_root`.
pub fn horse_markings_texture(markings: HorseMarkings) -> Option<&'static str> {
    match markings {
        HorseMarkings::None => None,
        HorseMarkings::White => Some("entity/horse/horse_markings_white"),
        HorseMarkings::WhiteField => Some("entity/horse/horse_markings_whitefield"),
        HorseMarkings::WhiteDots => Some("entity/horse/horse_markings_whitedots"),
        HorseMarkings::BlackDots => Some("entity/horse/horse_markings_blackdots"),
    }
}

/// `DonkeyModel.createBodyLayer`: the base equine mesh with vanilla's
/// `DONKEY_TRANSFORMER` applied — `left_ear`/`right_ear` under `head` replaced
/// with larger, rotated donkey ears, and `left_chest`/`right_chest` boxes
/// added under `body` (vanilla toggles their visibility per-instance via
/// `state.hasChest`; that's a runtime concern, so they're baked in
/// unconditionally here, matching this port's existing llama/pig-saddle
/// precedent of not modelling equipment-visibility toggles). Then baked at
/// `scaling(DONKEY_SCALE = 0.87)`. Fixed texture, no variant.
fn donkey_body_root(scale: f32) -> EntityModelDef {
    let mut root = equine_base_root();
    if let Some(head_parts) = root.child_mut("head_parts")
        && let Some(head) = head_parts.child_mut("head")
    {
        head.children
            .retain(|(n, _)| n != "left_ear" && n != "right_ear");
        head.children.push((
            "left_ear".to_string(),
            PartDef::new(PartPose::offset_and_rotation(
                1.25,
                -10.0,
                4.0,
                PI / 12.0,
                0.0,
                PI / 12.0,
            ))
            .with_cube(cube([-1.0, -7.0, 0.0], [2.0, 7.0, 1.0], [0.0, 12.0])),
        ));
        head.children.push((
            "right_ear".to_string(),
            PartDef::new(PartPose::offset_and_rotation(
                -1.25,
                -10.0,
                4.0,
                PI / 12.0,
                0.0,
                -PI / 12.0,
            ))
            .with_cube(cube([-1.0, -7.0, 0.0], [2.0, 7.0, 1.0], [0.0, 12.0])),
        ));
    }
    if let Some(body) = root.child_mut("body") {
        body.children.push((
            "left_chest".to_string(),
            PartDef::new(PartPose::offset_and_rotation(
                6.0,
                -8.0,
                0.0,
                0.0,
                -PI / 2.0,
                0.0,
            ))
            .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 8.0, 3.0], [26.0, 21.0])),
        ));
        body.children.push((
            "right_chest".to_string(),
            PartDef::new(PartPose::offset_and_rotation(
                -6.0,
                -8.0,
                0.0,
                0.0,
                PI / 2.0,
                0.0,
            ))
            .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 8.0, 3.0], [26.0, 21.0])),
        ));
    }
    scaled(
        EntityModelDef {
            texture_width: 64,
            texture_height: 64,
            root,
        },
        scale,
    )
}

/// Donkey: `DonkeyModel.createBodyLayer(DonkeyModel.DONKEY_SCALE = 0.87F)`.
pub fn donkey_model() -> EntityModelDef {
    donkey_body_root(0.87)
}

/// Mule: the same `DonkeyModel` mesh, baked at `DonkeyModel.MULE_SCALE = 0.92F`
/// instead (`LayerDefinitions.java`: `DonkeyModel.createBodyLayer(0.92F)`).
pub fn mule_model() -> EntityModelDef {
    donkey_body_root(0.92)
}

/// `LlamaModel.createBodyLayer`: head (with neck and two ears), body, two
/// chest boxes (vanilla toggles visibility via `state.hasChest`; baked in
/// unconditionally, see the donkey chest note above), and four legs — all
/// direct root children, no deeper nesting. Sheet 128×64 (llama is the only
/// model in this corpus wider than 64px). `trader_llama` reuses this exact
/// mesh (`LayerDefinitions.java` puts the same `llamaBodyLayer` under both
/// `ModelLayers.LLAMA` and `ModelLayers.TRADER_LLAMA`).
pub fn llama_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 7.0, -6.0))
        .with_cube(cube([-2.0, -14.0, -10.0], [4.0, 4.0, 9.0], [0.0, 0.0]))
        .with_cube(cube([-4.0, -16.0, -6.0], [8.0, 18.0, 6.0], [0.0, 14.0]))
        .with_cube(cube([-4.0, -19.0, -4.0], [3.0, 3.0, 2.0], [17.0, 0.0]))
        .with_cube(cube([1.0, -19.0, -4.0], [3.0, 3.0, 2.0], [17.0, 0.0]));
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        5.0,
        2.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-6.0, -10.0, -7.0], [12.0, 18.0, 10.0], [29.0, 0.0]));
    let right_chest = PartDef::new(PartPose::offset_and_rotation(
        -8.5,
        3.0,
        3.0,
        0.0,
        PI / 2.0,
        0.0,
    ))
    .with_cube(cube([-3.0, 0.0, 0.0], [8.0, 8.0, 3.0], [45.0, 28.0]));
    let left_chest = PartDef::new(PartPose::offset_and_rotation(
        5.5,
        3.0,
        3.0,
        0.0,
        PI / 2.0,
        0.0,
    ))
    .with_cube(cube([-3.0, 0.0, 0.0], [8.0, 8.0, 3.0], [45.0, 41.0]));
    let leg = || cube([-2.0, 0.0, -2.0], [4.0, 14.0, 4.0], [29.0, 29.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child("right_chest", right_chest)
        .with_child("left_chest", left_chest)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-3.5, 10.0, 6.0)).with_cube(leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(3.5, 10.0, 6.0)).with_cube(leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.5, 10.0, -5.0)).with_cube(leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.5, 10.0, -5.0)).with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 64,
        root,
    }
}

/// Trader llama: byte-identical geometry to `llama_model` — vanilla reuses the
/// same baked `LayerDefinition` for both (`LayerDefinitions.java`). Only the
/// renderer differs (decor/carpet layer), which is out of scope for geometry.
pub fn trader_llama_model() -> EntityModelDef {
    llama_model()
}

fn llama_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Llama(LlamaColor::Creamy) => "entity/llama/llama_creamy",
        EntityVariant::Llama(LlamaColor::White) => "entity/llama/llama_white",
        EntityVariant::Llama(LlamaColor::Brown) => "entity/llama/llama_brown",
        EntityVariant::Llama(LlamaColor::Gray) => "entity/llama/llama_gray",
        _ => "entity/llama/llama_creamy",
    }
}

/// `AdultFelineModel.createBodyMesh`: the body mesh shared by cat and ocelot.
/// `head` carries `main`/`nose`/`ear1`/`ear2` as *unnamed sibling boxes on one
/// part* in vanilla (`CubeListBuilder` with four `addBox` calls, no child
/// parts) — modelled here as four cubes on the same `head` part rather than
/// four separate named children, matching vanilla's actual structure (only
/// `body`/`tail1`/`tail2`/the four legs are independent `PartDefinition`s).
/// `tail2` uses its own `CubeDeformation(-0.02)`, distinct from the other
/// parts' shared `g`. Sheet 64×32. Ocelot uses this mesh unscaled
/// (`ModelLayers.OCELOT` = `felineBodyLayer` with no transformer).
fn feline_base_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 15.0, -9.0))
        .with_cube(cube([-2.5, -2.0, -3.0], [5.0, 4.0, 5.0], [0.0, 0.0]))
        .with_cube(cube([-1.5, -0.001, -4.0], [3.0, 2.0, 2.0], [0.0, 24.0]))
        .with_cube(cube([-2.0, -3.0, 0.0], [1.0, 1.0, 2.0], [0.0, 10.0]))
        .with_cube(cube([1.0, -3.0, 0.0], [1.0, 1.0, 2.0], [6.0, 10.0]));
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        12.0,
        -10.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-2.0, 3.0, -8.0], [4.0, 16.0, 6.0], [20.0, 0.0]));
    let tail1 = PartDef::new(PartPose::offset_and_rotation(0.0, 15.0, 8.0, 0.9, 0.0, 0.0))
        .with_cube(cube([-0.5, 0.0, 0.0], [1.0, 8.0, 1.0], [0.0, 15.0]));
    let tail2 = PartDef::new(PartPose::offset(0.0, 20.0, 14.0))
        .with_cube(cube([-0.5, 0.0, 0.0], [1.0, 8.0, 1.0], [4.0, 15.0]).grown(-0.02));
    let hind_leg = || cube([-1.0, 0.0, 1.0], [2.0, 6.0, 2.0], [8.0, 13.0]);
    let front_leg = || cube([-1.0, 0.0, 0.0], [2.0, 10.0, 2.0], [40.0, 0.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child("tail1", tail1)
        .with_child("tail2", tail2)
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(1.1, 18.0, 5.0)).with_cube(hind_leg()),
        )
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-1.1, 18.0, 5.0)).with_cube(hind_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(1.2, 14.1, -5.0)).with_cube(front_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-1.2, 14.1, -5.0)).with_cube(front_leg()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// Ocelot: the feline mesh unscaled. Fixed texture (`entity/cat/ocelot`) —
/// unlike cat, ocelot has **no colour variant** in this version (colour
/// variants moved to the separate `Cat` entity type in 1.14); confirmed via
/// `OcelotRenderer.getTextureLocation`, which returns one hardcoded path.
pub fn ocelot_model() -> EntityModelDef {
    feline_base_model()
}

/// Cat: the feline mesh baked at `AdultCatModel.CAT_TRANSFORMER =
/// scaling(0.8F)` (`LayerDefinitions.java`:
/// `felineBodyLayer.apply(AdultCatModel.CAT_TRANSFORMER)`). Breed is a real
/// variant (11 `CatVariant`s); the collar tint (`CatCollarLayer`) is a
/// runtime dye-colour overlay, not a texture-file variant, so it's out of
/// scope here.
pub fn cat_model() -> EntityModelDef {
    scaled(feline_base_model(), 0.8)
}

fn cat_coat_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Cat(CatCoat::Tabby) => "entity/cat/cat_tabby",
        EntityVariant::Cat(CatCoat::Black) => "entity/cat/cat_black",
        EntityVariant::Cat(CatCoat::Red) => "entity/cat/cat_red",
        EntityVariant::Cat(CatCoat::Siamese) => "entity/cat/cat_siamese",
        EntityVariant::Cat(CatCoat::BritishShorthair) => "entity/cat/cat_british_shorthair",
        EntityVariant::Cat(CatCoat::Calico) => "entity/cat/cat_calico",
        EntityVariant::Cat(CatCoat::Persian) => "entity/cat/cat_persian",
        EntityVariant::Cat(CatCoat::Ragdoll) => "entity/cat/cat_ragdoll",
        EntityVariant::Cat(CatCoat::White) => "entity/cat/cat_white",
        EntityVariant::Cat(CatCoat::Jellie) => "entity/cat/cat_jellie",
        EntityVariant::Cat(CatCoat::AllBlack) => "entity/cat/cat_all_black",
        _ => "entity/cat/cat_tabby",
    }
}

/// `AdultWolfModel.createBodyLayer`: `head` (empty, pivot-only) holding a
/// `real_head` child with four boxes (main head, two identically-textured
/// ear boxes placed by origin sign rather than mirroring, and a snout); `body`
/// and `upper_body` are independent, both rotated `PI/2`; four legs share two
/// `CubeListBuilder`s (`leftLeg`/`rightLeg`, the latter `.mirror()`ed) reused
/// across hind and front pairs, exactly like `BlazeModel`'s ring reuse; `tail`
/// (empty, pivot-only) holds a `real_tail` child. Sheet 64×32, unscaled
/// (`LayerDefinitions.java`'s `wolfBodyLayer` has no `MeshTransformer`).
pub fn wolf_model() -> EntityModelDef {
    let real_head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-2.0, -3.0, -2.0], [6.0, 6.0, 4.0], [0.0, 0.0]))
        .with_cube(cube([-2.0, -5.0, 0.0], [2.0, 2.0, 1.0], [16.0, 14.0]))
        .with_cube(cube([2.0, -5.0, 0.0], [2.0, 2.0, 1.0], [16.0, 14.0]))
        .with_cube(cube([-0.5, -0.001, -5.0], [3.0, 3.0, 4.0], [0.0, 10.0]));
    let head = PartDef::new(PartPose::offset(-1.0, 13.5, -7.0)).with_child("real_head", real_head);
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0,
        14.0,
        2.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-3.0, -2.0, -3.0], [6.0, 9.0, 6.0], [18.0, 14.0]));
    let upper_body = PartDef::new(PartPose::offset_and_rotation(
        -1.0,
        14.0,
        -3.0,
        PI / 2.0,
        0.0,
        0.0,
    ))
    .with_cube(cube([-3.0, -3.0, -3.0], [8.0, 6.0, 7.0], [21.0, 0.0]));
    let left_leg = || cube([0.0, 0.0, -1.0], [2.0, 8.0, 2.0], [0.0, 18.0]);
    let right_leg = || cube([0.0, 0.0, -1.0], [2.0, 8.0, 2.0], [0.0, 18.0]).mirrored();
    let real_tail = PartDef::new(PartPose::ZERO).with_cube(cube(
        [0.0, 0.0, -1.0],
        [2.0, 8.0, 2.0],
        [9.0, 18.0],
    ));
    let tail = PartDef::new(PartPose::offset_and_rotation(
        -1.0,
        12.0,
        8.0,
        PI / 5.0,
        0.0,
        0.0,
    ))
    .with_child("real_tail", real_tail);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child("upper_body", upper_body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-2.5, 16.0, 7.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(0.5, 16.0, 7.0)).with_cube(left_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-2.5, 16.0, -4.0)).with_cube(right_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(0.5, 16.0, -4.0)).with_cube(left_leg()),
        )
        .with_child("tail", tail);
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

fn wolf_coat_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Wolf { coat, state } => {
            let base = match coat {
                WolfCoat::Pale => "entity/wolf/wolf",
                WolfCoat::Spotted => "entity/wolf/wolf_spotted",
                WolfCoat::Snowy => "entity/wolf/wolf_snowy",
                WolfCoat::Black => "entity/wolf/wolf_black",
                WolfCoat::Ashen => "entity/wolf/wolf_ashen",
                WolfCoat::Rusty => "entity/wolf/wolf_rusty",
                WolfCoat::Woods => "entity/wolf/wolf_woods",
                WolfCoat::Chestnut => "entity/wolf/wolf_chestnut",
                WolfCoat::Striped => "entity/wolf/wolf_striped",
            };
            // `Wolf.getTexture()` appends `_tame`/`_angry` to the breed's file
            // stem for the other two states; `Pale`'s stem has no breed suffix
            // (`WolfVariants.register(context, PALE, "wolf", ...)`), so its
            // tame/angry files are `wolf_tame`/`wolf_angry`, not
            // `wolf_pale_tame` — a per-breed string-concat quirk, not a
            // lookup table, transcribed by matching the same concat pattern.
            match state {
                WolfState::Wild => base,
                WolfState::Tame => wolf_suffixed(base, "_tame"),
                WolfState::Angry => wolf_suffixed(base, "_angry"),
            }
        }
        _ => "entity/wolf/wolf",
    }
}

/// Vanilla's `Wolf.getTexture()` does `Identifier.withDefaultNamespace` string
/// concatenation at runtime; this corpus only has `&'static str`s to hand
/// back, so the small, fixed concatenated set is enumerated instead of built
/// with runtime string concatenation.
fn wolf_suffixed(base: &'static str, suffix: &'static str) -> &'static str {
    match (base, suffix) {
        ("entity/wolf/wolf", "_tame") => "entity/wolf/wolf_tame",
        ("entity/wolf/wolf", "_angry") => "entity/wolf/wolf_angry",
        ("entity/wolf/wolf_spotted", "_tame") => "entity/wolf/wolf_spotted_tame",
        ("entity/wolf/wolf_spotted", "_angry") => "entity/wolf/wolf_spotted_angry",
        ("entity/wolf/wolf_snowy", "_tame") => "entity/wolf/wolf_snowy_tame",
        ("entity/wolf/wolf_snowy", "_angry") => "entity/wolf/wolf_snowy_angry",
        ("entity/wolf/wolf_black", "_tame") => "entity/wolf/wolf_black_tame",
        ("entity/wolf/wolf_black", "_angry") => "entity/wolf/wolf_black_angry",
        ("entity/wolf/wolf_ashen", "_tame") => "entity/wolf/wolf_ashen_tame",
        ("entity/wolf/wolf_ashen", "_angry") => "entity/wolf/wolf_ashen_angry",
        ("entity/wolf/wolf_rusty", "_tame") => "entity/wolf/wolf_rusty_tame",
        ("entity/wolf/wolf_rusty", "_angry") => "entity/wolf/wolf_rusty_angry",
        ("entity/wolf/wolf_woods", "_tame") => "entity/wolf/wolf_woods_tame",
        ("entity/wolf/wolf_woods", "_angry") => "entity/wolf/wolf_woods_angry",
        ("entity/wolf/wolf_chestnut", "_tame") => "entity/wolf/wolf_chestnut_tame",
        ("entity/wolf/wolf_chestnut", "_angry") => "entity/wolf/wolf_chestnut_angry",
        ("entity/wolf/wolf_striped", "_tame") => "entity/wolf/wolf_striped_tame",
        ("entity/wolf/wolf_striped", "_angry") => "entity/wolf/wolf_striped_angry",
        _ => base,
    }
}

/// `ParrotModel.createBodyLayer`: body, tail, two wings (sharing one
/// texOffs), a head with four children (`head2`, `beak1`, `beak2`, and a
/// zero-*width* `feather` box — another vanilla degenerate-box UV quirk, kept
/// verbatim), and two legs. All parts are direct root children except the
/// head's four sub-boxes. Sheet 32×32.
pub fn parrot_model() -> EntityModelDef {
    let head2 = PartDef::new(PartPose::offset(0.0, -2.0, -1.0)).with_cube(cube(
        [-1.0, -0.5, -2.0],
        [2.0, 1.0, 4.0],
        [10.0, 0.0],
    ));
    let beak1 = PartDef::new(PartPose::offset(0.0, -0.5, -1.5)).with_cube(cube(
        [-0.5, -1.0, -0.5],
        [1.0, 2.0, 1.0],
        [11.0, 7.0],
    ));
    let beak2 = PartDef::new(PartPose::offset(0.0, -1.75, -2.45)).with_cube(cube(
        [-0.5, 0.0, -0.5],
        [1.0, 2.0, 1.0],
        [16.0, 7.0],
    ));
    let feather = PartDef::new(PartPose::offset_and_rotation(
        0.0, -2.15, 0.15, -0.2214, 0.0, 0.0,
    ))
    .with_cube(cube([0.0, -4.0, -2.0], [0.0, 5.0, 4.0], [2.0, 18.0]));
    let head = PartDef::new(PartPose::offset(0.0, 15.69, -2.76))
        .with_cube(cube([-1.0, -1.5, -1.0], [2.0, 3.0, 2.0], [2.0, 2.0]))
        .with_child("head2", head2)
        .with_child("beak1", beak1)
        .with_child("beak2", beak2)
        .with_child("feather", feather);
    let body = PartDef::new(PartPose::offset_and_rotation(
        0.0, 16.5, -3.0, 0.4937, 0.0, 0.0,
    ))
    .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 6.0, 3.0], [2.0, 8.0]));
    let tail = PartDef::new(PartPose::offset_and_rotation(
        0.0, 21.07, 1.16, 1.015, 0.0, 0.0,
    ))
    .with_cube(cube([-1.5, -1.0, -1.0], [3.0, 4.0, 1.0], [22.0, 1.0]));
    let wing = || cube([-0.5, 0.0, -1.5], [1.0, 5.0, 3.0], [19.0, 8.0]);
    let leg = || cube([-0.5, 0.0, -0.5], [1.0, 2.0, 1.0], [14.0, 18.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("tail", tail)
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset_and_rotation(
                1.5, 16.94, -2.76, -0.6981, -PI, 0.0,
            ))
            .with_cube(wing()),
        )
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset_and_rotation(
                -1.5, 16.94, -2.76, -0.6981, -PI, 0.0,
            ))
            .with_cube(wing()),
        )
        .with_child("head", head)
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset_and_rotation(
                1.0, 22.0, -1.05, -0.0299, 0.0, 0.0,
            ))
            .with_cube(leg()),
        )
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset_and_rotation(
                -1.0, 22.0, -1.05, -0.0299, 0.0, 0.0,
            ))
            .with_cube(leg()),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

fn parrot_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Parrot(ParrotColor::RedBlue) => "entity/parrot/parrot_red_blue",
        EntityVariant::Parrot(ParrotColor::Blue) => "entity/parrot/parrot_blue",
        EntityVariant::Parrot(ParrotColor::Green) => "entity/parrot/parrot_green",
        EntityVariant::Parrot(ParrotColor::YellowBlue) => "entity/parrot/parrot_yellow_blue",
        // Vanilla's own filename is spelled "grey", not "gray" — kept verbatim
        // even though the enum case (matching `Parrot.Variant.GRAY`) is not.
        EntityVariant::Parrot(ParrotColor::Gray) => "entity/parrot/parrot_grey",
        _ => "entity/parrot/parrot_red_blue",
    }
}

// ============================================================================
// Second batch of overworld-priority mobs, ordered by how often a player
// actually encounters them: polar_bear (common in snowy biomes), the illager
// raid roster (pillager/vindicator/evoker/illusioner, all one shared mesh),
// ravager, allay, shulker.
// ============================================================================

/// `PolarBearModel.createBodyLayer` (`QuadrupedModel`): head (main box, mouth,
/// 2 ears — all direct siblings on one part, not nested), body, and 4
/// unparented legs, baked at `scaling(1.2)`. Sheet 128×64. Fixed texture; the
/// baby variant is a separate `ModelLayer`/renderer scale, out of scope like
/// this port's other baby models.
pub fn polar_bear_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 10.0, -16.0))
        .with_cube(cube([-3.5, -3.0, -3.0], [7.0, 7.0, 7.0], [0.0, 0.0]))
        .with_cube(cube([-2.5, 1.0, -6.0], [5.0, 3.0, 3.0], [0.0, 44.0]))
        .with_cube(cube([-4.5, -4.0, -1.0], [2.0, 2.0, 1.0], [26.0, 0.0]))
        .with_cube(cube([2.5, -4.0, -1.0], [2.0, 2.0, 1.0], [26.0, 0.0]).mirrored());
    let body = PartDef::new(PartPose::offset_and_rotation(-2.0, 9.0, 12.0, PI / 2.0, 0.0, 0.0))
        .with_cube(cube([-5.0, -13.0, -7.0], [14.0, 14.0, 11.0], [0.0, 19.0]))
        .with_cube(cube([-4.0, -25.0, -7.0], [12.0, 12.0, 10.0], [39.0, 0.0]));
    let hind_leg = || cube([-2.0, 0.0, -2.0], [4.0, 10.0, 8.0], [50.0, 22.0]);
    let front_leg = || cube([-2.0, 0.0, -2.0], [4.0, 10.0, 6.0], [50.0, 40.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-4.5, 14.0, 6.0)).with_cube(hind_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(4.5, 14.0, 6.0)).with_cube(hind_leg()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-3.5, 14.0, -8.0)).with_cube(front_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(3.5, 14.0, -8.0)).with_cube(front_leg()),
        );
    scaled(
        EntityModelDef {
            texture_width: 128,
            texture_height: 64,
            root,
        },
        1.2,
    )
}

/// `IllagerModel.createBodyLayer`: the shared mesh for pillager, vindicator,
/// evoker and illusioner (`LayerDefinitions.java` puts the identical
/// `illagerBodyLayer` under all four `ModelLayers` entries, unscaled). `head`
/// carries a `hat` child (vanilla sets `hat.visible = false` permanently for
/// this model — a runtime visibility toggle this port doesn't model, same as
/// the always-shown player/zombie hat elsewhere in this corpus) and a `nose`.
/// `arms` itself carries two direct cubes *and* a `left_shoulder` child
/// (vanilla's own asymmetric shape: `arms` is the crossed-arms pose, only
/// shown when `IllagerArmPose.CROSSED`; `right_arm`/`left_arm` are the normal
/// separate arms). Sheet 64×64.
fn illager_base_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 10.0, 8.0], [0.0, 0.0]))
        .with_child(
            "hat",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -10.0, -4.0], [8.0, 12.0, 8.0], [32.0, 0.0]).grown(0.45)),
        )
        .with_child(
            "nose",
            PartDef::new(PartPose::offset(0.0, -2.0, 0.0))
                .with_cube(cube([-1.0, -1.0, -6.0], [2.0, 4.0, 2.0], [24.0, 0.0])),
        );
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 12.0, 6.0], [16.0, 20.0]))
        .with_cube(cube([-4.0, 0.0, -3.0], [8.0, 20.0, 6.0], [0.0, 38.0]).grown(0.5));
    let arms = PartDef::new(PartPose::offset_and_rotation(0.0, 3.0, -1.0, -0.75, 0.0, 0.0))
        .with_cube(cube([-8.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]))
        .with_cube(cube([-4.0, 2.0, -2.0], [8.0, 4.0, 4.0], [40.0, 38.0]))
        .with_child(
            "left_shoulder",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([4.0, -2.0, -2.0], [4.0, 8.0, 4.0], [44.0, 22.0]).mirrored()),
        );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("head", head)
        .with_child("body", body)
        .with_child("arms", arms)
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(-2.0, 12.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0])),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
                .with_cube(cube([-2.0, 0.0, -2.0], [4.0, 12.0, 4.0], [0.0, 22.0]).mirrored()),
        )
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-5.0, 2.0, 0.0))
                .with_cube(cube([-3.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 46.0])),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(5.0, 2.0, 0.0))
                .with_cube(cube([-1.0, -2.0, -2.0], [4.0, 12.0, 4.0], [40.0, 46.0]).mirrored()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// Pillager: the illager mesh, unscaled. Fixed texture.
pub fn pillager_model() -> EntityModelDef {
    illager_base_model()
}

/// Vindicator: the illager mesh, unscaled. Fixed texture.
pub fn vindicator_model() -> EntityModelDef {
    illager_base_model()
}

/// Evoker: the illager mesh, unscaled. Fixed texture. (`EvokerFangsModel` is a
/// separate summon-effect entity, out of scope here.)
pub fn evoker_model() -> EntityModelDef {
    illager_base_model()
}

/// Illusioner: the illager mesh, unscaled. Fixed texture.
pub fn illusioner_model() -> EntityModelDef {
    illager_base_model()
}

/// `RavagerModel.createBodyLayer`: `neck` (1 box) holds `head` (2 boxes,
/// skull plus a small nested box), which itself holds `right_horn`,
/// `left_horn` and `mouth`; `body` (2 boxes) and 4 unparented legs are
/// direct root children. Sheet 128×128, unscaled.
pub fn ravager_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, 16.0, -17.0))
        .with_cube(cube([-8.0, -20.0, -14.0], [16.0, 20.0, 16.0], [0.0, 0.0]))
        .with_cube(cube([-2.0, -6.0, -18.0], [4.0, 8.0, 4.0], [0.0, 0.0]))
        .with_child(
            "right_horn",
            PartDef::new(PartPose::offset_and_rotation(-10.0, -14.0, -8.0, 1.0995574, 0.0, 0.0))
                .with_cube(cube([0.0, -14.0, -2.0], [2.0, 14.0, 4.0], [74.0, 55.0])),
        )
        .with_child(
            "left_horn",
            PartDef::new(PartPose::offset_and_rotation(8.0, -14.0, -8.0, 1.0995574, 0.0, 0.0))
                .with_cube(cube([0.0, -14.0, -2.0], [2.0, 14.0, 4.0], [74.0, 55.0]).mirrored()),
        )
        .with_child(
            "mouth",
            PartDef::new(PartPose::offset(0.0, -2.0, 2.0))
                .with_cube(cube([-8.0, 0.0, -16.0], [16.0, 3.0, 16.0], [0.0, 36.0])),
        );
    let neck = PartDef::new(PartPose::offset(0.0, -7.0, 5.5))
        .with_cube(cube([-5.0, -1.0, -18.0], [10.0, 10.0, 18.0], [68.0, 73.0]))
        .with_child("head", head);
    let body = PartDef::new(PartPose::offset_and_rotation(0.0, 1.0, 2.0, PI / 2.0, 0.0, 0.0))
        .with_cube(cube([-7.0, -10.0, -7.0], [14.0, 16.0, 20.0], [0.0, 55.0]))
        .with_cube(cube([-6.0, 6.0, -7.0], [12.0, 13.0, 18.0], [0.0, 91.0]));
    let hind_leg = || cube([-4.0, 0.0, -4.0], [8.0, 37.0, 8.0], [96.0, 0.0]);
    let front_leg = || cube([-4.0, 0.0, -4.0], [8.0, 37.0, 8.0], [64.0, 0.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child("neck", neck)
        .with_child("body", body)
        .with_child(
            "right_hind_leg",
            PartDef::new(PartPose::offset(-8.0, -13.0, 18.0)).with_cube(hind_leg()),
        )
        .with_child(
            "left_hind_leg",
            PartDef::new(PartPose::offset(8.0, -13.0, 18.0)).with_cube(hind_leg().mirrored()),
        )
        .with_child(
            "right_front_leg",
            PartDef::new(PartPose::offset(-8.0, -13.0, -5.0)).with_cube(front_leg()),
        )
        .with_child(
            "left_front_leg",
            PartDef::new(PartPose::offset(8.0, -13.0, -5.0)).with_cube(front_leg().mirrored()),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root,
    }
}

/// `AllayModel.createBodyLayer`: the mesh's *own* root is empty and holds one
/// child, `"root"` (offset `(0, 23.5, 0)`), which vanilla's constructor then
/// re-roots onto (`super(root.getChild("root"))`) — i.e. the part vanilla
/// actually renders from is `"root"`, not the mesh's nominal top part. This
/// port bakes that offset directly into `EntityModelDef.root`'s own pose
/// (matching what vanilla actually renders) rather than reproducing the
/// unused outer wrapper part, which would just be a no-op pass-through here.
/// `right_wing`/`left_wing` are zero-*width* degenerate boxes, like parrot's
/// `feather` — kept verbatim. Sheet 32×32.
pub fn allay_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -3.99, 0.0))
        .with_cube(cube([-2.5, -5.0, -2.5], [5.0, 5.0, 5.0], [0.0, 0.0]));
    let body = PartDef::new(PartPose::offset(0.0, -4.0, 0.0))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 4.0, 2.0], [0.0, 10.0]))
        .with_cube(cube([-1.5, 0.0, -1.0], [3.0, 5.0, 2.0], [0.0, 16.0]).grown(-0.2))
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-1.75, 0.5, 0.0))
                .with_cube(cube([-0.75, -0.5, -1.0], [1.0, 4.0, 2.0], [23.0, 0.0]).grown(-0.01)),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(1.75, 0.5, 0.0))
                .with_cube(cube([-0.25, -0.5, -1.0], [1.0, 4.0, 2.0], [23.0, 6.0]).grown(-0.01)),
        )
        .with_child(
            "right_wing",
            PartDef::new(PartPose::offset(-0.5, 0.0, 0.6))
                .with_cube(cube([0.0, 1.0, 0.0], [0.0, 5.0, 8.0], [16.0, 14.0])),
        )
        .with_child(
            "left_wing",
            PartDef::new(PartPose::offset(0.5, 0.0, 0.6))
                .with_cube(cube([0.0, 1.0, 0.0], [0.0, 5.0, 8.0], [16.0, 14.0])),
        );
    let root = PartDef::new(PartPose::offset(0.0, 23.5, 0.0))
        .with_child("head", head)
        .with_child("body", body);
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `ShulkerModel.createBodyLayer`: `lid`, `base` and `head`, all direct root
/// children with no nesting. Sheet 64×64. Fixed to the default (purple)
/// skin: vanilla's actual texture is a genuine `DyeColor` (16-way + a
/// colourless default) variant (`ShulkerRenderer.getTextureLocation`), which
/// this port does not model — `DyeColor` doesn't exist as a shared type in
/// this crate yet, and adding a 17th ad-hoc variant enum for a single mob
/// felt like more mechanism than the "keep geometry moving" priority
/// warranted this pass. Flagging as an explicit gap, not a silent one.
pub fn shulker_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-8.0, -16.0, -8.0], [16.0, 12.0, 16.0], [0.0, 0.0])),
        )
        .with_child(
            "base",
            PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
                .with_cube(cube([-8.0, -8.0, -8.0], [16.0, 8.0, 16.0], [0.0, 28.0])),
        )
        .with_child(
            "head",
            PartDef::new(PartPose::offset(0.0, 12.0, 0.0))
                .with_cube(cube([-3.0, 0.0, -3.0], [6.0, 6.0, 6.0], [0.0, 52.0])),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

// ============================================================================
// Cheap-reuse mobs: same builder as an already-ported mesh, only the texture
// (or texture variant) differs. Connecting existing geometry to new registry
// entries rather than porting anything new.
// ============================================================================

/// `GlowSquidRenderer` reuses `SquidModel` verbatim; only the texture path
/// changes (`entity/squid/glow_squid`, not e.g. a tinted overlay — the glow
/// itself is an emissive-texture/render-layer effect, not geometry).
pub fn glow_squid_model() -> EntityModelDef {
    squid_model()
}

/// `WanderingTraderRenderer` reuses `VillagerModel` verbatim (bakes
/// `ModelLayers.WANDERING_TRADER`, the same mesh shape as the plain
/// villager), swapping only the base skin
/// (`entity/wandering_trader/wandering_trader`); profession-specific
/// clothing layers are a separate `CustomHeadLayer`/item-layer concern this
/// port doesn't model, same as for villager professions.
pub fn wandering_trader_model() -> EntityModelDef {
    villager_model()
}

/// Mooshroom: the plain `CowModel`, unscaled — vanilla's mushroom growth is a
/// render layer (`MushroomCowMushroomLayer`), not extra mesh geometry.
pub fn mooshroom_model() -> EntityModelDef {
    cow_model()
}

// ============================================================================
// Projectiles: rigs whose renderer is **not** a `LivingEntityRenderer`
// ============================================================================
//
// Every other entry in this file is placed by `LivingEntityRenderer`'s pose
// stack, which flips Y (`scale(-1, -1, 1)`) and lifts by `1.501` blocks. The
// two entries below are not: `ArrowRenderer` and `ThrownTridentRenderer` both
// `extend EntityRenderer` directly, which applies **neither** — `EntityRenderer`
// itself contains no `scale(` call at all, against `LivingEntityRenderer.render`,
// which has both. So these meshes are
// authored in the *world* orientation (+Y up), not the Y-down mob orientation,
// and `lodestone_render::entity::projectile_model_matrix` places them.
//
// The consequence for the geometry here: **the long axis is not Y.** An arrow's
// shaft runs along `+X` (the `cross` box spans `x ∈ [-12, +4]` texels, tip at
// high X), which is why vanilla rotates pitch about `Axis.ZP` and not `XP`. A
// trident's pole runs along `−Y` (spikes at negative Y below a pole spanning
// `y ∈ [+2, +27]`), which is why `ThrownTridentRenderer` adds `+90°` to the
// pitch: that offset is what turns the pole's axis into the arrow's. Both end
// up pointing along the entity's velocity; see `docs/projectile-renderers.md`.

/// `ArrowModel.createBodyLayer()` — the rig `ArrowRenderer` bakes, shared by
/// `arrow`, `spectral_arrow` and the tipped-arrow variant. Sheet 32×32.
///
/// Three boxes, **two of which are zero-extent planes**, which is the one thing
/// about this mesh that is unlike every mob above:
///
/// * `back`, the fletching: `addBox(0, -2.5, -2.5, 0, 5, 5)` — zero *width*, so
///   only the WEST and EAST faces have area and the other four collapse.
/// * `cross`, the shaft-and-head: `addBox(-12, -2, 0, 16, 4, 0, NONE, 1.0, 0.8)`
///   — zero *depth*, so only NORTH and SOUTH have area. It is instantiated twice,
///   at `xRot = π/4` and `3π/4`, forming the X-section a real arrow's fletching
///   makes when you look down the shaft.
///
/// [`crate::entity::bake_entity`] emits all six faces of every box regardless, so
/// this bakes 18 quads of which 6 are degenerate. That is deliberate and matches
/// vanilla (`ModelPart.Cube` does the same); the degenerate ones rasterise no
/// fragments, and the corpus tests that walk UVs skip them explicitly via
/// `quad_is_degenerate`.
///
/// Two scale factors, both real and both easy to lose:
///
/// * The **whole mesh** is `0.9×`. `LayerDefinition.create(mesh.transformed(pose
///   -> pose.scaled(0.9F)), 32, 32)` looks like it scales every part, but
///   `PartDefinition.transformed` applies the function to *its own* pose and
///   copies its children untouched — so it is the
///   **root** pose that carries the 0.9, and children inherit it through the
///   transform chain. Modelled here as a root [`PartPose::scale`], which is
///   exactly that.
/// * `back` is a further `0.8×` (`PartPose.withScale(0.8F)`), so the fletching
///   ends up at `0.72×`.
///
/// `ArrowModel.setupAnim` also adds a `zRot` wobble from `state.shake` for the
/// seven ticks after an arrow sticks in a block. Not modelled: `shakeTime` is not
/// on this side of the wire (it is neither entity metadata nor a packet field —
/// vanilla sets it client-side from the `IN_GROUND` metadata *transition*), so
/// there is no input to drive it. A stuck arrow therefore rests still instead of
/// quivering.
pub fn arrow_model() -> EntityModelDef {
    let cross = || {
        let mut c = cube([-12.0, -2.0, 0.0], [16.0, 4.0, 0.0], [0.0, 0.0]);
        // `addBox(..., CubeDeformation.NONE, xTexScale = 1.0, yTexScale = 0.8)`.
        // The V divisor becomes `32 * 0.8 = 25.6`, which stretches the box's
        // 4 texels of height across 5 rows of the sheet — the shaft strip is 5
        // pixels tall in `arrow.png`, not 4.
        c.tex_scale = [1.0, 0.8];
        c
    };
    let root = PartDef::new(PartPose {
        scale: [0.9, 0.9, 0.9],
        ..PartPose::ZERO
    })
    .with_child(
        "back",
        PartDef::new(PartPose {
            x: -11.0,
            y: 0.0,
            z: 0.0,
            x_rot: PI / 4.0,
            y_rot: 0.0,
            z_rot: 0.0,
            scale: [0.8, 0.8, 0.8],
        })
        .with_cube(cube([0.0, -2.5, -2.5], [0.0, 5.0, 5.0], [0.0, 0.0])),
    )
    .with_child(
        "cross_1",
        PartDef::new(PartPose::rotation(PI / 4.0, 0.0, 0.0)).with_cube(cross()),
    )
    .with_child(
        "cross_2",
        PartDef::new(PartPose::rotation(PI * 3.0 / 4.0, 0.0, 0.0)).with_cube(cross()),
    );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root,
    }
}

/// `TridentModel.createLayer()` — the rig `ThrownTridentRenderer` bakes. Sheet
/// 32×32, five solid boxes, no zero-extent planes and no mesh-wide scale.
///
/// A `pole` spanning `y ∈ [+2, +27]` with four children hanging off it: the
/// `base` crossguard at `y ∈ [0, 2]`, and three spikes at *negative* Y
/// (`middle_spike` `y ∈ [-4, 0]`, `left_spike`/`right_spike` `y ∈ [-3, +1]`).
/// The tip is therefore at **−Y**, the opposite end from where a mob model puts
/// its head, and `right_spike` is `left_spike` mirrored (vanilla calls
/// `.mirror()` on the same texel offset, which flips both the X extent and the
/// winding — [`crate::entity::CubeDef::mirrored`] is that).
///
/// Vanilla draws a second `entityGlint` pass over this mesh when
/// `ThrownTrident.isFoil()`. Not modelled: enchantment glint needs its own render
/// type (a scrolling additive layer), which nothing in this engine has, and
/// `isFoil` is not decoded on this side of the wire either.
pub fn trident_model() -> EntityModelDef {
    let pole = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-0.5, 2.0, -0.5], [1.0, 25.0, 1.0], [0.0, 6.0]))
        .with_child(
            "base",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-1.5, 0.0, -0.5], [3.0, 2.0, 1.0], [4.0, 0.0])),
        )
        .with_child(
            "left_spike",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-2.5, -3.0, -0.5], [1.0, 4.0, 1.0], [4.0, 3.0])),
        )
        .with_child(
            "middle_spike",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-0.5, -4.0, -0.5], [1.0, 4.0, 1.0], [0.0, 0.0])),
        )
        .with_child(
            "right_spike",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([1.5, -3.0, -0.5], [1.0, 4.0, 1.0], [4.0, 3.0]).mirrored()),
        );
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child("pole", pole),
    }
}

/// Evoker fangs: a buried 10×12×10 base with two 4×14×8 jaws leaning out of the
/// ground, sheet 64×32.
///
/// # The rest pose is the *open* pose, and that is why it is usable static
///
/// The two jaws are authored at `zRot` `2.042035` and `4.2411504`, which are
/// exactly `π ∓ 0.35π` — the values the bite animation reaches at full open. So
/// the layer as baked is the first frame of the bite rather than an arbitrary
/// resting shape, which is what makes a rig with no animation channel look right
/// for the twenty-odd ticks a fang exists.
///
/// Two things the animation does that this cannot: it closes the jaws over the
/// bite, and it scales the whole rig to nothing over the last tenth. Neither is
/// available without per-entity progress.
///
/// # The 90° in the root pose
///
/// This rig's renderer yaws by `90 - yRot` where a mob's yaws by `180 - yRot`.
/// Composed under the model-space flip a `y_rot` of `φ` on the root subtracts `φ`
/// from that total, so `φ = π/2` turns the mob placement into the fangs' own —
/// the `S · Ry(φ) = Ry(-φ) · S` identity, not a fudge factor. Fold it here rather
/// than at a draw site: the draw site has one placement for every mob and the
/// difference is a property of this rig.
///
/// The base box sits at `y ∈ [24, 36]` model texels, i.e. **below** the ground
/// plane, on purpose — a fang is a pair of jaws rising out of buried gums.
pub fn evoker_fangs_model() -> EntityModelDef {
    let jaw = || cube([0.0, 0.0, 0.0], [4.0, 14.0, 8.0], [40.0, 0.0]);
    let base = PartDef::new(PartPose::offset(-5.0, 24.0, -5.0))
        .with_cube(cube([0.0, 0.0, 0.0], [10.0, 12.0, 10.0], [0.0, 0.0]))
        .with_child(
            "upper_jaw",
            PartDef::new(PartPose::offset_and_rotation(6.5, 0.0, 1.0, 0.0, 0.0, 2.042_035))
                .with_cube(jaw()),
        )
        .with_child(
            "lower_jaw",
            PartDef::new(PartPose::offset_and_rotation(3.5, 0.0, 9.0, 0.0, PI, 4.241_150_4))
                .with_cube(jaw()),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose {
            y_rot: PI / 2.0,
            ..PartPose::ZERO
        })
        .with_child("base", base),
    }
}

/// Shulker bullet: three interpenetrating slabs forming a six-pointed star, sheet
/// 64×32, baked at half scale.
///
/// The three boxes are the same 8×8×2 slab on each axis in turn, so the shape is
/// symmetric under any 90° turn — which is the reason a static orientation is a
/// tolerable stand-in here and would not be on an asymmetric rig. Vanilla tumbles
/// it on all three axes at three different rates off `ageInTicks`, and draws a
/// second, 1.5× translucent copy over it; neither is available to a corpus entry,
/// which carries one mesh, one sheet and no clock.
///
/// The `0.5` is vanilla's own `scale(-0.5, -0.5, 0.5)`, whose flip half is already
/// supplied by the placement — only the magnitude belongs in the mesh.
pub fn shulker_bullet_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose {
            scale: [0.5, 0.5, 0.5],
            ..PartPose::ZERO
        })
        .with_child(
            "main",
            PartDef::new(PartPose::ZERO)
                .with_cube(cube([-4.0, -4.0, -1.0], [8.0, 8.0, 2.0], [0.0, 0.0]))
                .with_cube(cube([-1.0, -4.0, -4.0], [2.0, 8.0, 8.0], [0.0, 10.0]))
                .with_cube(cube([-4.0, -1.0, -4.0], [8.0, 2.0, 8.0], [20.0, 0.0])),
        ),
    }
}

/// Wither skull: one 8×8×8 head, sheet 64×64.
///
/// The layer is declared inside the renderer rather than in a model class, and it
/// is **not** the ordinary skull head: it reads its texels at `(0, 35)` on the
/// wither's own sheet, not at `(0, 0)`. Transcribing the generic skull layout here
/// would put the wither's body on the skull's face.
///
/// The sheet is the harmless one of the pair. A skull fired by a wither at low
/// health is drawn from `wither_invulnerable` instead, which is per-entity state
/// this rig has no channel for.
pub fn wither_skull_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child(
            "head",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-4.0, -8.0, -4.0],
                [8.0, 8.0, 8.0],
                [0.0, 35.0],
            )),
        ),
    }
}

/// Llama spit: seven 2×2×2 cubes in a plus-sign cluster, sheet 64×32.
///
/// All seven share texel offset `(0, 0)` — vanilla chains seven `addBox` calls
/// after a single `texOffs`, so every cube samples the same 2×2×2 patch. That is
/// not a transcription slip to be "fixed" by spreading them across the sheet.
///
/// The cluster is authored in the octant `x, y, z ∈ [-4, 6]` rather than centred
/// on the origin, so it hangs off its pivot by design.
pub fn llama_spit_model() -> EntityModelDef {
    // (x, y, z) origin of each 2-cube, in the order vanilla adds them.
    const CUBES: [[f32; 3]; 7] = [
        [-4.0, 0.0, 0.0],
        [0.0, -4.0, 0.0],
        [0.0, 0.0, -4.0],
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [0.0, 2.0, 0.0],
        [0.0, 0.0, 2.0],
    ];
    let mut main = PartDef::new(PartPose::ZERO);
    for origin in CUBES {
        main = main.with_cube(cube(origin, [2.0, 2.0, 2.0], [0.0, 0.0]));
    }
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child("main", main),
    }
}

/// Elder guardian: the guardian mesh baked at a 2.35× mesh scale, on its own sheet.
///
/// The scale is the only geometric difference — same twelve spikes, same eye, same
/// three-part tail — which is why this reuses `guardian_model` rather than
/// restating it. The sheet is not shared, though: an elder is a paler, larger
/// texture at the same 64×64 UV layout, so the alias is a *corpus entry* rather
/// than a name mapping onto the guardian's own entry.
pub fn elder_guardian_model() -> EntityModelDef {
    scaled(guardian_model(), 2.35)
}

/// Parched: a skeleton whose every limb carries a second, slightly larger overlay
/// box on the same part, sheet 64×64.
///
/// This is **not** the skeleton rig with a different sheet, and treating it as one
/// would lose the whole look. Each of the six parts holds two boxes: the ordinary
/// thin skeleton box, and an overlay a fraction larger at a different texel offset,
/// which is what gives the mob its ragged second silhouette. Three details in that
/// second box are load bearing rather than noise, and all three would look like
/// transcription slop to a tidying reader:
///
/// * The arms sit at `±5.5`, not the skeleton's `±5.0`, and their overlays are
///   offset by `-1.55` on the right and `-1.45` on the left — **not** mirrored
///   values of one number.
/// * The overlays start `0.025` texels above their base box (`-2.025` against
///   `-2.0`), which is what stops the two coplanar tops z-fighting.
/// * The head's overlay is a `0.2` grow and the body's a `0.025` one; the limbs use
///   a larger box rather than a grow at all.
///
/// The `hat` child is present and empty, matching the part tree the armour layers
/// pose against; it carries no box of its own.
pub fn parched_model() -> EntityModelDef {
    let body = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 16.0]))
        .with_cube(cube([-4.0, 10.0, -2.0], [8.0, 1.0, 4.0], [28.0, 0.0]))
        .with_cube(cube([-4.0, 0.0, -2.0], [8.0, 12.0, 4.0], [16.0, 48.0]).grown(0.025));
    let head = PartDef::new(PartPose::ZERO)
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 32.0]).grown(0.2))
        .with_child("hat", PartDef::new(PartPose::ZERO));
    let right_arm = PartDef::new(PartPose::offset(-5.5, 2.0, 0.0))
        .with_cube(cube([-1.0, -2.0, -1.0], [2.0, 12.0, 2.0], [40.0, 16.0]))
        .with_cube(cube([-1.55, -2.025, -1.5], [3.0, 12.0, 3.0], [42.0, 33.0]));
    let left_arm = PartDef::new(PartPose::offset(5.5, 2.0, 0.0))
        .with_cube(cube([-1.0, -2.0, -1.0], [2.0, 12.0, 2.0], [56.0, 16.0]))
        .with_cube(cube([-1.45, -2.025, -1.5], [3.0, 12.0, 3.0], [40.0, 48.0]));
    let right_leg = PartDef::new(PartPose::offset(-2.0, 12.0, 0.0))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 12.0, 2.0], [0.0, 16.0]))
        .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 12.0, 3.0], [0.0, 49.0]));
    let left_leg = PartDef::new(PartPose::offset(2.0, 12.0, 0.0))
        .with_cube(cube([-1.0, 0.0, -1.0], [2.0, 12.0, 2.0], [0.0, 16.0]))
        .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 12.0, 3.0], [4.0, 49.0]));
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO)
            .with_child("body", body)
            .with_child("head", head)
            .with_child("right_arm", right_arm)
            .with_child("left_arm", left_arm)
            .with_child("right_leg", right_leg)
            .with_child("left_leg", left_leg),
    }
}

/// Giant: the plain humanoid mesh baked at a 6× mesh scale, on the zombie sheet.
///
/// The 6× is the whole port. This type's registry hitbox is 3.6 × 12.28 and the
/// humanoid mesh it reuses is two blocks tall, so a 1× rig would stand ankle-deep
/// inside its own collision box — the "you walk into something you cannot see"
/// shape this corpus exists to close. There is no per-entity scale attribute
/// carrying it either: the factor lives in the baked layer, which is why it
/// belongs here rather than at a draw site. See `docs/entity-rendering.md` for the
/// vanilla layer and renderer this is transcribed from.
pub fn giant_model() -> EntityModelDef {
    scaled(zombie_model(), 6.0)
}

/// Leash knot: one 6×8×6 box hanging below its pivot, sheet 32×32.
///
/// The whole model. Its renderer is **not** a living-entity renderer — it flips
/// the model and submits, with no feet lift and no yaw — so this rig is routed
/// through `lodestone_render::entity::non_living_vehicle_placement` with a zero
/// bob rather than through the mob placement, or the knot would hang 1.501 blocks
/// under the fence post it is tied to.
pub fn leash_knot_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child(
            "knot",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-3.0, -8.0, -3.0],
                [6.0, 8.0, 6.0],
                [0.0, 0.0],
            )),
        ),
    }
}

/// Sulfur cube (adult): the outer 18³ translucent shell, sheet 128×128.
///
/// # The root pose is a renderer constant, not part of the baked layer
///
/// The layer this transcribes is a bare box centred on its own pivot, which would
/// draw a metre-and-a-bit cube floating with its centre 1.501 blocks above the
/// feet. Everything that puts it on the ground lives in the renderer's `scale`
/// hook, which this corpus has no equivalent of, so the constant part of that hook
/// is folded into the root pose here. Derived rather than eyeballed, in the order
/// vanilla composes it (all of it inside the flipped, Y-down model frame, *before*
/// the 1.501 feet lift):
///
/// ```text
///   S(0.999) · T(0, 0.001, 0)        z-fight mitigation
///   · S(2)                           the adult's size multiplier
///   · S(0.5) · T(0, 0.98 - 1/16, 0)  the adult's extra downscale and drop
/// ```
///
/// `S(2) · S(0.5)` is the identity, so the surviving scale is `0.999` and the
/// surviving translation is `0.9185` blocks. Composing that with the `-1.501` lift
/// and re-expressing as *translate outside scale* (which is what a `PartPose` is)
/// gives `T(0, 0.91909, 0) · S(0.999)` — `14.7054` texels of Y offset. The check
/// that it landed: the box then spans world `y ∈ [0.020, 1.144]`, i.e. sitting on
/// the ground and overhanging its own 0.98-block hitbox by the same 1.147 ratio at
/// either size, which is the ratio the size-1 arithmetic gives independently.
///
/// **Not modelled**: the inner core (a second, separate layer), the block a cube
/// may be carrying, the fuse swell, and the per-instance squish — all four are
/// per-entity state this rig has no channel for. What is here is the shell, which
/// is what makes the mob visible at all.
pub fn sulfur_cube_model() -> EntityModelDef {
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root: PartDef::new(PartPose {
            y: 14.7054,
            scale: [0.999, 0.999, 0.999],
            ..PartPose::ZERO
        })
        .with_child(
            "cube",
            PartDef::new(PartPose::ZERO).with_cube(cube(
                [-9.0, -9.0, -9.0],
                [18.0, 18.0, 18.0],
                [0.0, 0.0],
            )),
        ),
    }
}

/// Breeze: a two-box head over three rods on a hexagonal-ish tripod, sheet 32×32.
///
/// The rods are the reason the rotations here are not round numbers: each is the
/// same 2×8×2 box, placed at a ±30° yaw about a shared pivot 3 texels up, then
/// rolled a half-turn — `2.5981` is `3·√3/2`, the X leg of that placement, and it
/// must stay paired with its own `∓1.0472` (30°) yaw or the tripod splays.
///
/// **The wind is a separate rig and is deliberately absent.** It is a translucent
/// three-shell funnel on its own 128×128 sheet, drawn as a second pass over the
/// same entity; a corpus entry has one sheet, so folding it in here would draw the
/// funnel with the body's UVs. The `eyes` part is absent for a different reason: it
/// is a texel-for-texel duplicate of `head` that exists only so the emissive pass
/// has a part to retain, so including it would add coplanar duplicate quads and
/// change no pixel.
pub fn breeze_model() -> EntityModelDef {
    // One shared box; only the pose differs. Written as a whole `PartPose` per rod
    // rather than as five loose floats through a helper, because every argument
    // here is an `f32` and two of the three rods differ from each other in exactly
    // one sign — the shape a transposed pair survives unnoticed.
    let rod = |pose: PartPose| {
        PartDef::new(pose).with_cube(cube([-1.0, 0.0, -3.0], [2.0, 8.0, 2.0], [0.0, 17.0]))
    };
    let rods = PartDef::new(PartPose::offset(0.0, 8.0, 0.0))
        .with_child(
            "rod_1",
            rod(PartPose::offset_and_rotation(
                2.5981, -3.0, 1.5, -2.7489, -1.0472, 3.1416,
            )),
        )
        .with_child(
            "rod_2",
            rod(PartPose::offset_and_rotation(
                -2.5981, -3.0, 1.5, -2.7489, 1.0472, 3.1416,
            )),
        )
        .with_child(
            "rod_3",
            rod(PartPose::offset_and_rotation(
                0.0, -3.0, -3.0, 0.3927, 0.0, 0.0,
            )),
        );
    let head = PartDef::new(PartPose::offset(0.0, 4.0, 0.0))
        .with_cube(cube([-5.0, -5.0, -4.2], [10.0, 3.0, 4.0], [4.0, 24.0]))
        .with_cube(cube([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]));
    EntityModelDef {
        texture_width: 32,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child(
            "body",
            PartDef::new(PartPose::ZERO)
                .with_child("rods", rods)
                .with_child("head", head),
        ),
    }
}

/// Creaking: a lopsided wooden biped, sheet 64×64.
///
/// Two things about it are unlike every other humanoid-shaped rig here, and both
/// are load bearing rather than transcription noise:
///
/// * **It is deliberately asymmetric.** The left arm is 16 texels long and the
///   right 21; the legs differ in length, pivot and thickness. A "tidied" mirror
///   of either side is wrong.
/// * **Four of its boxes are zero-extent planes** — two 9×14 flags on the head and
///   a 5×9 sole under each foot. A zero-size axis bakes two coincident faces, which
///   is how this corpus already draws flat parts elsewhere; do not round them up to
///   a thin box.
///
/// The part names (`head`, `right_arm`, `left_arm`, `right_leg`, `left_leg`) put
/// this rig in the humanoid animation family, which is an approximation: vanilla
/// drives it from keyframe clips, not from the humanoid limb swing. The rest pose
/// is faithful and the swing is not, which is the right way round for a mob whose
/// defect was being invisible.
pub fn creaking_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(-3.0, -11.0, 0.0))
        .with_cube(cube([-3.0, -10.0, -3.0], [6.0, 10.0, 6.0], [0.0, 0.0]))
        .with_cube(cube([-3.0, -13.0, -3.0], [6.0, 3.0, 6.0], [28.0, 31.0]))
        .with_cube(cube([3.0, -13.0, 0.0], [9.0, 14.0, 0.0], [12.0, 40.0]))
        .with_cube(cube([-12.0, -14.0, 0.0], [9.0, 14.0, 0.0], [34.0, 12.0]));
    let body = PartDef::new(PartPose::offset(0.0, -7.0, 1.0))
        .with_cube(cube([0.0, -3.0, -3.0], [6.0, 13.0, 5.0], [0.0, 16.0]))
        .with_cube(cube([-6.0, -4.0, -3.0], [6.0, 7.0, 5.0], [24.0, 0.0]));
    let right_arm = PartDef::new(PartPose::offset(-7.0, -9.5, 1.5))
        .with_cube(cube([-2.0, -1.5, -1.5], [3.0, 21.0, 3.0], [22.0, 13.0]))
        .with_cube(cube([-2.0, 19.5, -1.5], [3.0, 4.0, 3.0], [46.0, 0.0]));
    let left_arm = PartDef::new(PartPose::offset(6.0, -9.0, 0.5))
        .with_cube(cube([0.0, -1.0, -1.5], [3.0, 16.0, 3.0], [30.0, 40.0]))
        .with_cube(cube([0.0, -5.0, -1.5], [3.0, 4.0, 3.0], [52.0, 12.0]))
        .with_cube(cube([0.0, 15.0, -1.5], [3.0, 4.0, 3.0], [52.0, 19.0]));
    let upper_body = PartDef::new(PartPose::offset(-1.0, -19.0, 0.0))
        .with_child("head", head)
        .with_child("body", body)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);
    let left_leg = PartDef::new(PartPose::offset(1.5, -16.0, 0.5))
        .with_cube(cube([-1.5, 0.0, -1.5], [3.0, 16.0, 3.0], [42.0, 40.0]))
        .with_cube(cube([-1.5, 15.7, -4.5], [5.0, 0.0, 9.0], [45.0, 55.0]));
    let right_leg = PartDef::new(PartPose::offset(-1.0, -17.5, 0.5))
        .with_cube(cube([-3.0, -1.5, -1.5], [3.0, 19.0, 3.0], [0.0, 34.0]))
        .with_cube(cube([-5.0, 17.2, -4.5], [5.0, 0.0, 9.0], [45.0, 46.0]))
        .with_cube(cube([-3.0, -4.5, -1.5], [3.0, 3.0, 3.0], [12.0, 34.0]));
    let root = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("upper_body", upper_body)
        .with_child("left_leg", left_leg)
        .with_child("right_leg", right_leg);
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root: PartDef::new(PartPose::ZERO).with_child("root", root),
    }
}

/// Copper golem: a squat biped with a lightning-rod antenna, sheet 64×64.
///
/// The standing pose. Vanilla bakes four more layers off the same texel budget (a
/// running, sitting and star pose, plus the emissive eyes); those are separate
/// baked meshes selected per animation state, not parts of this one, and this
/// corpus resolves one mesh per model name.
///
/// Three of the head's four boxes carry a **negative** grow (`-0.015`) and one a
/// positive one: the antenna stack is shrunk into the skull and the skull itself is
/// pushed out, which is what stops the four coplanar seams z-fighting. Dropping the
/// signs — or applying one sign to all four — puts the flicker back.
///
/// The oxidation stage is a texture axis (four sheets) driven by per-entity state
/// this rig has no channel for, so it draws the unoxidised sheet.
pub fn copper_golem_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, 0.0))
        .with_cube(cube([-4.0, -5.0, -5.0], [8.0, 5.0, 10.0], [0.0, 0.0]).grown(0.015))
        .with_cube(cube([-1.0, -2.0, -6.0], [2.0, 3.0, 2.0], [56.0, 0.0]))
        .with_cube(cube([-1.0, -9.0, -1.0], [2.0, 4.0, 2.0], [37.0, 8.0]).grown(-0.015))
        .with_cube(cube([-2.0, -13.0, -2.0], [4.0, 4.0, 4.0], [37.0, 0.0]).grown(-0.015));
    let body = PartDef::new(PartPose::offset(0.0, -5.0, 0.0))
        .with_cube(cube([-4.0, -6.0, -3.0], [8.0, 6.0, 6.0], [0.0, 15.0]))
        .with_child("head", head)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_cube(cube(
                [-3.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [36.0, 16.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_cube(cube(
                [0.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [50.0, 16.0],
            )),
        );
    let root = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(0.0, -5.0, 0.0)).with_cube(cube(
                [-4.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [0.0, 27.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(0.0, -5.0, 0.0)).with_cube(cube(
                [0.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [16.0, 27.0],
            )),
        );
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// Happy ghast (adult): a 16³ body with nine hanging tentacles, sheet 64×64, the
/// whole mesh baked at a 4× scale.
///
/// Unlike its hostile cousin the tentacle lengths are **authored, not seeded** —
/// 5, 7, 4, 5, 5, 7, 8, 8, 5 in part order — and the X/Z offsets are on a
/// hand-placed 3×3 grid rather than a computed one. Do not reach for the seeded
/// generator that builds the other ghast's fringe; it produces different lengths.
///
/// The 4× is a mesh-level scale, the same mechanism the hostile ghast uses, and it
/// is what fills the 4×4 hitbox. The baby is a separate baked layer (a second body
/// shell, then 0.2375× on top), and the harness and ropes are separate equipment
/// layers; none of the three is a part of this mesh.
pub fn happy_ghast_model() -> EntityModelDef {
    // (x, y, z, length) per tentacle, in part order.
    const TENTACLES: [(f32, f32, f32, f32); 9] = [
        (-3.75, 7.0, -5.0, 5.0),
        (1.25, 7.0, -5.0, 7.0),
        (6.25, 7.0, -5.0, 4.0),
        (-6.25, 7.0, 0.0, 5.0),
        (-1.25, 7.0, 0.0, 5.0),
        (3.75, 7.0, 0.0, 7.0),
        (-3.75, 7.0, 5.0, 8.0),
        (1.25, 7.0, 5.0, 8.0),
        (6.25, 7.0, 5.0, 5.0),
    ];
    let mut body = PartDef::new(PartPose::offset(0.0, 16.0, 0.0)).with_cube(cube(
        [-8.0, -8.0, -8.0],
        [16.0, 16.0, 16.0],
        [0.0, 0.0],
    ));
    for (i, (x, y, z, len)) in TENTACLES.iter().enumerate() {
        body = body.with_child(
            &format!("tentacle{i}"),
            PartDef::new(PartPose::offset(*x, *y, *z)).with_cube(cube(
                [-1.0, 0.0, -1.0],
                [2.0, *len, 2.0],
                [0.0, 0.0],
            )),
        );
    }
    scaled(
        EntityModelDef {
            texture_width: 64,
            texture_height: 64,
            root: PartDef::new(PartPose::ZERO).with_child("body", body),
        },
        4.0,
    )
}

/// Nautilus: a spiral shell over a body with a three-part beak, sheet 128×128.
///
/// Shared, unmodified, by the zombie variant — that type differs only in its sheet
/// and in a coral overlay that is its own baked layer, so both corpus entries build
/// this same mesh.
///
/// Two transcription traps live in the fractional offsets, and both are there to
/// stop coplanar faces flickering rather than to move anything visibly: the body's
/// two boxes start at `y = -4.51` (not `-4.5`) so they clear the shell, and the
/// upper and lower beak carry a **negative** grow of `-0.001` while the inner mouth
/// between them carries none. Rounding any of the three loses the separation.
///
/// The shell's third box and the body's second are zero-extent planes — the shell's
/// rear rim and the body's tail fin — and bake as coincident double faces.
pub fn nautilus_model() -> EntityModelDef {
    let shell = PartDef::new(PartPose::offset(0.0, -13.0, 5.0))
        .with_cube(cube([-7.0, -10.0, -7.0], [14.0, 10.0, 16.0], [0.0, 0.0]))
        .with_cube(cube([-7.0, 0.0, -7.0], [14.0, 8.0, 20.0], [0.0, 26.0]))
        .with_cube(cube([-7.0, 0.0, 6.0], [14.0, 8.0, 0.0], [48.0, 26.0]));
    let body = PartDef::new(PartPose::offset(0.0, -8.5, 12.3))
        .with_cube(cube([-5.0, -4.51, -3.0], [10.0, 8.0, 14.0], [0.0, 54.0]))
        .with_cube(cube([-5.0, -4.51, 7.0], [10.0, 8.0, 0.0], [0.0, 76.0]))
        .with_child(
            "upper_mouth",
            PartDef::new(PartPose::offset(0.0, -2.51, 7.0)).with_cube(
                cube([-5.0, -2.0, 0.0], [10.0, 4.0, 4.0], [54.0, 54.0]).grown(-0.001),
            ),
        )
        .with_child(
            "inner_mouth",
            PartDef::new(PartPose::offset(0.0, -0.51, 7.5)).with_cube(cube(
                [-3.0, -2.0, -0.5],
                [6.0, 4.0, 4.0],
                [54.0, 70.0],
            )),
        )
        .with_child(
            "lower_mouth",
            PartDef::new(PartPose::offset(0.0, 1.49, 7.0)).with_cube(
                cube([-5.0, -1.98, 0.0], [10.0, 4.0, 4.0], [54.0, 62.0]).grown(-0.001),
            ),
        );
    EntityModelDef {
        texture_width: 128,
        texture_height: 128,
        root: PartDef::new(PartPose::ZERO).with_child(
            "root",
            PartDef::new(PartPose::offset(0.0, 29.0, -6.0))
                .with_child("shell", shell)
                .with_child("body", body),
        ),
    }
}

fn mooshroom_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Mooshroom(MooshroomColor::Red) => "entity/cow/mooshroom_red",
        EntityVariant::Mooshroom(MooshroomColor::Brown) => "entity/cow/mooshroom_brown",
        _ => "entity/cow/mooshroom_red",
    }
}

fn player_wide() -> EntityModelDef {
    player_model(false)
}

fn player_slim() -> EntityModelDef {
    player_model(true)
}

/// The ported entity-model corpus, in a fixed order (priority: player first,
/// then the common overworld set). Growing this list is how coverage climbs.
pub fn entity_models() -> Vec<EntityModelEntry> {
    vec![
        EntityModelEntry {
            name: "player_wide",
            texture: EntityTexture::Fixed("entity/player/wide/steve"),
            build: player_wide,
        },
        EntityModelEntry {
            name: "player_slim",
            texture: EntityTexture::Fixed("entity/player/slim/alex"),
            build: player_slim,
        },
        EntityModelEntry {
            name: "zombie",
            texture: EntityTexture::Fixed("entity/zombie/zombie"),
            build: zombie_model,
        },
        EntityModelEntry {
            name: "skeleton",
            texture: EntityTexture::Fixed("entity/skeleton/skeleton"),
            build: skeleton_model,
        },
        EntityModelEntry {
            name: "creeper",
            texture: EntityTexture::Fixed("entity/creeper/creeper"),
            build: creeper_model,
        },
        EntityModelEntry {
            name: "spider",
            texture: EntityTexture::Fixed("entity/spider/spider"),
            build: spider_model,
        },
        EntityModelEntry {
            name: "pig",
            texture: EntityTexture::ByVariant {
                default: "entity/pig/pig_temperate",
                select: pig_texture,
            },
            build: pig_model,
        },
        EntityModelEntry {
            name: "cow",
            texture: EntityTexture::ByVariant {
                default: "entity/cow/cow_temperate",
                select: cow_texture,
            },
            build: cow_model,
        },
        EntityModelEntry {
            name: "sheep",
            texture: EntityTexture::Fixed("entity/sheep/sheep"),
            build: sheep_model,
        },
        EntityModelEntry {
            name: "chicken",
            texture: EntityTexture::ByVariant {
                default: "entity/chicken/chicken_temperate",
                select: chicken_texture,
            },
            build: chicken_model,
        },
        // Tier 2: common overworld/hostile expansion. Texture-only variants reuse
        // an existing builder because vanilla renders them with the same model
        // class. Where vanilla applies a `MeshTransformer.scaling(..)` to the
        // layer (baked into the mesh, not the renderer), we wrap the base builder
        // in `scaled(..)` so the geometry carries the same size.
        EntityModelEntry {
            name: "husk",
            texture: EntityTexture::Fixed("entity/zombie/husk"),
            build: husk_model,
        },
        EntityModelEntry {
            name: "stray",
            texture: EntityTexture::Fixed("entity/skeleton/stray"),
            build: skeleton_model,
        },
        EntityModelEntry {
            name: "wither_skeleton",
            texture: EntityTexture::Fixed("entity/skeleton/wither_skeleton"),
            build: wither_skeleton_model,
        },
        EntityModelEntry {
            name: "cave_spider",
            texture: EntityTexture::Fixed("entity/spider/cave_spider"),
            build: cave_spider_model,
        },
        EntityModelEntry {
            name: "slime",
            texture: EntityTexture::Fixed("entity/slime/slime"),
            build: slime_model,
        },
        EntityModelEntry {
            name: "magma_cube",
            texture: EntityTexture::Fixed("entity/slime/magmacube"),
            build: magma_cube_model,
        },
        EntityModelEntry {
            name: "blaze",
            texture: EntityTexture::Fixed("entity/blaze/blaze"),
            build: blaze_model,
        },
        EntityModelEntry {
            name: "squid",
            texture: EntityTexture::Fixed("entity/squid/squid"),
            build: squid_model,
        },
        EntityModelEntry {
            name: "bat",
            texture: EntityTexture::Fixed("entity/bat/bat"),
            build: bat_model,
        },
        EntityModelEntry {
            name: "enderman",
            texture: EntityTexture::Fixed("entity/enderman/enderman"),
            build: enderman_model,
        },
        // Tier 3: monster/* remainder (impl-assets lane).
        EntityModelEntry {
            name: "drowned",
            texture: EntityTexture::Fixed("entity/zombie/drowned"),
            build: drowned_model,
        },
        EntityModelEntry {
            name: "iron_golem",
            texture: EntityTexture::Fixed("entity/iron_golem/iron_golem"),
            build: iron_golem_model,
        },
        EntityModelEntry {
            name: "snow_golem",
            texture: EntityTexture::Fixed("entity/snow_golem/snow_golem"),
            build: snow_golem_model,
        },
        EntityModelEntry {
            name: "vex",
            texture: EntityTexture::Fixed("entity/illager/vex"),
            build: vex_model,
        },
        EntityModelEntry {
            name: "silverfish",
            texture: EntityTexture::Fixed("entity/silverfish/silverfish"),
            build: silverfish_model,
        },
        EntityModelEntry {
            name: "endermite",
            texture: EntityTexture::Fixed("entity/endermite/endermite"),
            build: endermite_model,
        },
        EntityModelEntry {
            name: "piglin",
            texture: EntityTexture::Fixed("entity/piglin/piglin"),
            build: piglin_model,
        },
        EntityModelEntry {
            name: "zombified_piglin",
            texture: EntityTexture::Fixed("entity/piglin/zombified_piglin"),
            build: piglin_model,
        },
        EntityModelEntry {
            name: "piglin_brute",
            texture: EntityTexture::Fixed("entity/piglin/piglin_brute"),
            build: piglin_model,
        },
        EntityModelEntry {
            name: "ghast",
            texture: EntityTexture::Fixed("entity/ghast/ghast"),
            build: ghast_model,
        },
        EntityModelEntry {
            name: "hoglin",
            texture: EntityTexture::Fixed("entity/hoglin/hoglin"),
            build: hoglin_model,
        },
        EntityModelEntry {
            name: "zoglin",
            texture: EntityTexture::Fixed("entity/hoglin/zoglin"),
            build: hoglin_model,
        },
        EntityModelEntry {
            name: "strider",
            texture: EntityTexture::Fixed("entity/strider/strider"),
            build: strider_model,
        },
        EntityModelEntry {
            name: "guardian",
            texture: EntityTexture::Fixed("entity/guardian/guardian"),
            build: guardian_model,
        },
        EntityModelEntry {
            name: "phantom",
            texture: EntityTexture::Fixed("entity/phantom/phantom"),
            build: phantom_model,
        },
        EntityModelEntry {
            name: "warden",
            texture: EntityTexture::Fixed("entity/warden/warden"),
            build: warden_model,
        },
        EntityModelEntry {
            name: "wither",
            texture: EntityTexture::Fixed("entity/wither/wither"),
            build: wither_model,
        },
        EntityModelEntry {
            name: "ender_dragon",
            texture: EntityTexture::Fixed("entity/enderdragon/dragon"),
            build: ender_dragon_model,
        },
        EntityModelEntry {
            name: "witch",
            texture: EntityTexture::Fixed("entity/witch/witch"),
            build: witch_model,
        },
        EntityModelEntry {
            name: "villager",
            texture: EntityTexture::Fixed("entity/villager/villager"),
            build: villager_model,
        },
        EntityModelEntry {
            name: "zombie_villager",
            texture: EntityTexture::Fixed("entity/zombie_villager/zombie_villager"),
            build: zombie_villager_model,
        },
        // --- animal/npc/object half (owned by this agent). Horse family,
        // cat/wolf/ocelot and parrot are deferred pending the texture-variant
        // seam (see the module banner above `end_crystal_model`). item_frame
        // is intentionally absent: vanilla resolves it via a block-model JSON
        // (`ItemFrameRenderer`/`BlockModelResolver`), not a `ModelPart`
        // `LayerDefinition`, so it does not fit `CubeDef`'s single-tex_offset
        // box-unwrap primitive without extending it or routing through the
        // block-model pipeline instead. ---
        EntityModelEntry {
            name: "armor_stand",
            texture: EntityTexture::Fixed("entity/armorstand/armorstand"),
            build: armor_stand_model,
        },
        EntityModelEntry {
            name: "boat",
            texture: EntityTexture::Fixed("entity/boat/oak"),
            build: boat_model,
        },
        EntityModelEntry {
            name: "chest_boat",
            texture: EntityTexture::Fixed("entity/chest_boat/oak"),
            build: chest_boat_model,
        },
        // The invisible water-clip mask both `boat` and `chest_boat` draw a
        // second, separately-pipelined instance of — see
        // `boat_water_patch_model`'s own doc. The texture is real (so this
        // loads exactly like every other corpus entry) but never sampled into
        // the framebuffer: `EntityPipeline::water_mask_pipeline` disables
        // colour writes.
        EntityModelEntry {
            name: "boat_water_patch",
            texture: EntityTexture::Fixed("entity/boat/oak"),
            build: boat_water_patch_model,
        },
        EntityModelEntry {
            name: "raft",
            texture: EntityTexture::Fixed("entity/boat/bamboo"),
            build: raft_model,
        },
        EntityModelEntry {
            name: "chest_raft",
            texture: EntityTexture::Fixed("entity/chest_boat/bamboo"),
            build: chest_raft_model,
        },
        EntityModelEntry {
            name: "minecart",
            texture: EntityTexture::Fixed("entity/minecart/minecart"),
            build: minecart_model,
        },
        EntityModelEntry {
            name: "end_crystal",
            texture: EntityTexture::Fixed("entity/end_crystal/end_crystal"),
            build: end_crystal_model,
        },
        EntityModelEntry {
            name: "rabbit",
            texture: EntityTexture::Fixed("entity/rabbit/rabbit_brown"),
            build: rabbit_model,
        },
        EntityModelEntry {
            name: "fox",
            texture: EntityTexture::Fixed("entity/fox/fox"),
            build: fox_model,
        },
        EntityModelEntry {
            name: "panda",
            texture: EntityTexture::Fixed("entity/panda/panda"),
            build: panda_model,
        },
        EntityModelEntry {
            name: "goat",
            texture: EntityTexture::Fixed("entity/goat/goat"),
            build: goat_model,
        },
        EntityModelEntry {
            name: "bee",
            texture: EntityTexture::Fixed("entity/bee/bee"),
            build: bee_model,
        },
        EntityModelEntry {
            name: "turtle",
            texture: EntityTexture::Fixed("entity/turtle/turtle"),
            build: turtle_model,
        },
        EntityModelEntry {
            name: "camel",
            texture: EntityTexture::Fixed("entity/camel/camel"),
            build: camel_model,
        },
        EntityModelEntry {
            name: "cod",
            texture: EntityTexture::Fixed("entity/fish/cod"),
            build: cod_model,
        },
        EntityModelEntry {
            name: "salmon",
            texture: EntityTexture::Fixed("entity/fish/salmon"),
            build: salmon_model,
        },
        EntityModelEntry {
            name: "pufferfish",
            texture: EntityTexture::Fixed("entity/fish/pufferfish"),
            build: pufferfish_model,
        },
        EntityModelEntry {
            name: "tropical_fish",
            // `TropicalFishRenderer` pairs `TropicalFishLargeModel` (this
            // entry's geometry, chosen to avoid `TropicalFishSmallModel`'s
            // negative texOffs) with `tropical_b.png`, not `tropical_a.png`
            // (that's the small model's texture) — confirmed directly against
            // `TropicalFishRenderer.getTextureLocation`.
            texture: EntityTexture::Fixed("entity/fish/tropical_b"),
            build: tropical_fish_model,
        },
        EntityModelEntry {
            name: "dolphin",
            texture: EntityTexture::Fixed("entity/dolphin/dolphin"),
            build: dolphin_model,
        },
        EntityModelEntry {
            name: "axolotl",
            texture: EntityTexture::Fixed("entity/axolotl/axolotl_lucy"),
            build: axolotl_model,
        },
        EntityModelEntry {
            name: "frog",
            texture: EntityTexture::Fixed("entity/frog/frog_temperate"),
            build: frog_model,
        },
        EntityModelEntry {
            name: "tadpole",
            texture: EntityTexture::Fixed("entity/tadpole/tadpole"),
            build: tadpole_model,
        },
        EntityModelEntry {
            name: "sniffer",
            texture: EntityTexture::Fixed("entity/sniffer/sniffer"),
            build: sniffer_model,
        },
        EntityModelEntry {
            name: "armadillo",
            texture: EntityTexture::Fixed("entity/armadillo/armadillo"),
            build: armadillo_model,
        },
        // ---- horse family, cat/wolf/ocelot, parrot: variant-driven, see the
        // module banner above `equine_base_root` for the horse-markings caveat ----
        EntityModelEntry {
            name: "horse",
            texture: EntityTexture::ByVariant {
                default: "entity/horse/horse_white",
                select: horse_color_texture,
            },
            build: horse_model,
        },
        EntityModelEntry {
            name: "donkey",
            texture: EntityTexture::Fixed("entity/horse/donkey"),
            build: donkey_model,
        },
        EntityModelEntry {
            name: "mule",
            texture: EntityTexture::Fixed("entity/horse/mule"),
            build: mule_model,
        },
        EntityModelEntry {
            name: "skeleton_horse",
            texture: EntityTexture::Fixed("entity/horse/horse_skeleton"),
            build: skeleton_horse_model,
        },
        EntityModelEntry {
            name: "zombie_horse",
            texture: EntityTexture::Fixed("entity/horse/horse_zombie"),
            build: zombie_horse_model,
        },
        EntityModelEntry {
            name: "llama",
            texture: EntityTexture::ByVariant {
                default: "entity/llama/llama_creamy",
                select: llama_color_texture,
            },
            build: llama_model,
        },
        EntityModelEntry {
            name: "trader_llama",
            texture: EntityTexture::ByVariant {
                default: "entity/llama/llama_creamy",
                select: llama_color_texture,
            },
            build: trader_llama_model,
        },
        EntityModelEntry {
            name: "cat",
            texture: EntityTexture::ByVariant {
                default: "entity/cat/cat_tabby",
                select: cat_coat_texture,
            },
            build: cat_model,
        },
        EntityModelEntry {
            name: "ocelot",
            texture: EntityTexture::Fixed("entity/cat/ocelot"),
            build: ocelot_model,
        },
        EntityModelEntry {
            name: "wolf",
            texture: EntityTexture::ByVariant {
                default: "entity/wolf/wolf",
                select: wolf_coat_texture,
            },
            build: wolf_model,
        },
        EntityModelEntry {
            name: "parrot",
            texture: EntityTexture::ByVariant {
                default: "entity/parrot/parrot_red_blue",
                select: parrot_color_texture,
            },
            build: parrot_model,
        },
        // ---- second overworld-priority batch: polar_bear, illager raid
        // roster, ravager, allay, shulker ----
        EntityModelEntry {
            name: "polar_bear",
            texture: EntityTexture::Fixed("entity/bear/polarbear"),
            build: polar_bear_model,
        },
        EntityModelEntry {
            name: "pillager",
            texture: EntityTexture::Fixed("entity/illager/pillager"),
            build: pillager_model,
        },
        EntityModelEntry {
            name: "vindicator",
            texture: EntityTexture::Fixed("entity/illager/vindicator"),
            build: vindicator_model,
        },
        EntityModelEntry {
            name: "evoker",
            texture: EntityTexture::Fixed("entity/illager/evoker"),
            build: evoker_model,
        },
        EntityModelEntry {
            name: "illusioner",
            texture: EntityTexture::Fixed("entity/illager/illusioner"),
            build: illusioner_model,
        },
        EntityModelEntry {
            name: "ravager",
            texture: EntityTexture::Fixed("entity/illager/ravager"),
            build: ravager_model,
        },
        EntityModelEntry {
            name: "allay",
            texture: EntityTexture::Fixed("entity/allay/allay"),
            build: allay_model,
        },
        EntityModelEntry {
            name: "shulker",
            texture: EntityTexture::Fixed("entity/shulker/shulker"),
            build: shulker_model,
        },
        // ---- cheap-reuse mobs: existing builder, new registry entry only ----
        EntityModelEntry {
            name: "glow_squid",
            texture: EntityTexture::Fixed("entity/squid/glow_squid"),
            build: glow_squid_model,
        },
        EntityModelEntry {
            name: "wandering_trader",
            texture: EntityTexture::Fixed("entity/wandering_trader/wandering_trader"),
            build: wandering_trader_model,
        },
        EntityModelEntry {
            name: "mooshroom",
            texture: EntityTexture::ByVariant {
                default: "entity/cow/mooshroom_red",
                select: mooshroom_color_texture,
            },
            build: mooshroom_model,
        },
        // ---- the "invisible but solid" set: types the hitbox table already
        // knew about while the rig corpus did not, so a player collided with
        // something that drew nothing ----
        // ---- projectiles and effects with a cuboid rig of their own: placed by
        // `non_living_vehicle_matrix` or `projectile_model_matrix`, never by the
        // mob placement, because none of their renderers is a living-entity one ----
        EntityModelEntry {
            name: "evoker_fangs",
            texture: EntityTexture::Fixed("entity/illager/evoker_fangs"),
            build: evoker_fangs_model,
        },
        EntityModelEntry {
            name: "shulker_bullet",
            texture: EntityTexture::Fixed("entity/shulker/spark"),
            build: shulker_bullet_model,
        },
        EntityModelEntry {
            name: "wither_skull",
            // The harmless sheet; `wither_invulnerable` is chosen per entity by
            // a bit this rig has no channel for.
            texture: EntityTexture::Fixed("entity/wither/wither"),
            build: wither_skull_model,
        },
        EntityModelEntry {
            name: "llama_spit",
            texture: EntityTexture::Fixed("entity/llama/llama_spit"),
            build: llama_spit_model,
        },
        EntityModelEntry {
            name: "elder_guardian",
            texture: EntityTexture::Fixed("entity/guardian/guardian_elder"),
            build: elder_guardian_model,
        },
        EntityModelEntry {
            name: "parched",
            texture: EntityTexture::Fixed("entity/skeleton/parched"),
            build: parched_model,
        },
        EntityModelEntry {
            name: "giant",
            // The giant reuses the zombie's sheet outright; only the mesh scale
            // differs, so there is no `giant.png` to point at.
            texture: EntityTexture::Fixed("entity/zombie/zombie"),
            build: giant_model,
        },
        EntityModelEntry {
            name: "leash_knot",
            texture: EntityTexture::Fixed("entity/lead_knot/lead_knot"),
            build: leash_knot_model,
        },
        EntityModelEntry {
            name: "sulfur_cube",
            // The adult shell. The `_small` sheet belongs to the size-1 rig,
            // which is a separate baked layer this corpus does not carry.
            texture: EntityTexture::Fixed("entity/sulfur_cube/sulfur_cube_outer"),
            build: sulfur_cube_model,
        },
        EntityModelEntry {
            name: "breeze",
            texture: EntityTexture::Fixed("entity/breeze/breeze"),
            build: breeze_model,
        },
        EntityModelEntry {
            name: "creaking",
            texture: EntityTexture::Fixed("entity/creaking/creaking"),
            build: creaking_model,
        },
        EntityModelEntry {
            name: "copper_golem",
            // The unoxidised sheet; the other three stages are a per-entity
            // state axis nothing on this side carries yet.
            texture: EntityTexture::Fixed("entity/copper_golem/copper_golem"),
            build: copper_golem_model,
        },
        EntityModelEntry {
            name: "happy_ghast",
            texture: EntityTexture::Fixed("entity/ghast/happy_ghast"),
            build: happy_ghast_model,
        },
        EntityModelEntry {
            name: "nautilus",
            texture: EntityTexture::Fixed("entity/nautilus/nautilus"),
            build: nautilus_model,
        },
        EntityModelEntry {
            name: "zombie_nautilus",
            // Same rig, own sheet — a sibling of `nautilus`, not a variant of
            // it, because vanilla picks it by renderer class rather than by
            // entity state. The coral crust is a second baked layer and is not
            // part of this mesh.
            texture: EntityTexture::Fixed("entity/nautilus/zombie_nautilus"),
            build: nautilus_model,
        },
        EntityModelEntry {
            name: "camel_husk",
            // The camel rig, unmodified, on its own sheet — the same
            // one-rig-two-sheets shape as `nautilus`/`zombie_nautilus`.
            texture: EntityTexture::Fixed("entity/camel/camel_husk"),
            build: camel_model,
        },
        // ---- projectiles: placed by `projectile_model_matrix`, not by
        // `entity_model_matrix` (see the "Projectiles" section above) ----
        EntityModelEntry {
            name: "arrow",
            // `TippableArrowRenderer.NORMAL_ARROW_LOCATION`. The tipped sheet
            // (`arrow_tipped`) is a second, potion-colour-driven texture chosen
            // by `state.isTipped`; that bit is not decoded here, so an
            // arrow-of-harming draws as a plain arrow rather than as a wrongly
            // *tinted* one.
            texture: EntityTexture::Fixed("entity/projectiles/arrow"),
            build: arrow_model,
        },
        EntityModelEntry {
            name: "spectral_arrow",
            // `SpectralArrowRenderer.SPECTRAL_ARROW_LOCATION`. Same rig, own
            // sheet — a sibling of `arrow`, not a variant of it, because vanilla
            // picks it by *renderer class* rather than by entity state.
            texture: EntityTexture::Fixed("entity/projectiles/arrow_spectral"),
            build: arrow_model,
        },
        EntityModelEntry {
            name: "trident",
            texture: EntityTexture::Fixed("entity/trident/trident"),
            build: trident_model,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The corpus really carries the water-clip mask, not just the standalone
    /// builder function — the island shape this repo's own defects keep
    /// taking: a function can be correct and unit-testable while nothing
    /// wires it into the table `EntityModelSet::load()` actually walks.
    #[test]
    fn the_water_patch_is_a_real_corpus_entry() {
        let entry = entity_models()
            .into_iter()
            .find(|e| e.name == "boat_water_patch")
            .expect("boat_water_patch must be registered in entity_models()");
        assert_eq!((entry.build)(), boat_water_patch_model());
    }

    /// **Both hypotheses, from the real vanilla source.** `BoatModel.createWaterPatch`
    /// (`.cache/mc/26.2/client-src`) builds the *same* box `boat_hull`'s own
    /// `"bottom"` child does — `addBox(-14, -9, -3, 28, 16, 3)` at `texOffs(0, 0)`
    /// — but at pose `offsetAndRotation(0, -3, 1, PI/2, 0, 0)`, where `"bottom"`
    /// sits at `offsetAndRotation(0, 3, 1, PI/2, 0, 0)`. Only the pivot's `y`
    /// differs, and only in sign: everything else — the box, the rotation, `x`,
    /// `z` — must be bit-identical, or the patch sits somewhere vanilla's own
    /// hollow-interior fix does not, and the gap it exists to close reopens on
    /// one side.
    #[test]
    fn the_water_patch_mirrors_the_hulls_own_bottom_plank() {
        let hull = boat_hull();
        let bottom = hull
            .children
            .iter()
            .find(|(name, _)| name == "bottom")
            .map(|(_, part)| part)
            .expect("boat_hull must carry a \"bottom\" child");

        let patch_model = boat_water_patch_model();
        assert_eq!(patch_model.texture_width, 128, "same sheet width as the boat itself");
        assert_eq!(patch_model.texture_height, 64, "same sheet height as the boat itself");
        let (name, patch) = &patch_model.root.children[0];
        assert_eq!(name, "water_patch");

        // The one axis that must differ, and only in sign: this is the whole
        // mechanism by which the patch sits *inside* the hollow rather than on
        // the visible outer hull.
        assert!(
            (patch.pose.y - (-bottom.pose.y)).abs() < 1e-6,
            "the patch's y pivot ({}) must be the *negation* of bottom's ({}), not a copy",
            patch.pose.y,
            bottom.pose.y
        );
        // The wrong-but-plausible neighbour: a straight copy of "bottom",
        // which would draw the mask exactly where the visible hull already is
        // and leave the actual gap (further up, inside the hollow) unmasked.
        assert!(
            (patch.pose.y - bottom.pose.y).abs() > 1.0,
            "the patch must not merely copy bottom's pose, or it masks the wrong plane"
        );

        // Every other pose axis: identical.
        assert!((patch.pose.x - bottom.pose.x).abs() < 1e-6, "x pivot must match");
        assert!((patch.pose.z - bottom.pose.z).abs() < 1e-6, "z pivot must match");
        assert!((patch.pose.x_rot - bottom.pose.x_rot).abs() < 1e-6, "x rotation must match");
        assert!((patch.pose.y_rot - bottom.pose.y_rot).abs() < 1e-6, "y rotation must match");
        assert!((patch.pose.z_rot - bottom.pose.z_rot).abs() < 1e-6, "z rotation must match");

        // The box itself: bit-identical origin/size/tex_offset to `"bottom"`'s,
        // per `BoatModel.createWaterPatch`'s own `texOffs(0, 0).addBox(-14, -9,
        // -3, 28, 16, 3)` — the same literal `addBox` call `addCommonParts`
        // makes for `"bottom"`.
        assert_eq!(patch.cubes.len(), 1, "the patch is one box, not the whole hull");
        assert_eq!(bottom.cubes.len(), 1);
        assert_eq!(patch.cubes[0].origin, bottom.cubes[0].origin, "box origin must match");
        assert_eq!(patch.cubes[0].size, bottom.cubes[0].size, "box size must match");
        assert_eq!(
            patch.cubes[0].tex_offset,
            bottom.cubes[0].tex_offset,
            "tex offset must match (irrelevant for colour, but a real transcription \
             checks the whole literal, not just the parts that render differently)"
        );
    }

    /// The negative control this pair needs: `chest_boat_model` must **not**
    /// carry its own water-patch child — vanilla's `BoatRenderer` submits the
    /// mask as a second model entirely, not folded into either boat variant's
    /// own part tree (see `boat_water_patch_model`'s doc for why it is a
    /// separate corpus entry).
    #[test]
    fn neither_boat_variant_carries_the_patch_as_its_own_child() {
        for (label, model) in [("boat", boat_model()), ("chest_boat", chest_boat_model())] {
            assert!(
                !model.root.children.iter().any(|(name, _)| name == "water_patch"),
                "{label} must not carry \"water_patch\" as a child part"
            );
        }
    }
}
