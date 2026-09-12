use super::*;

/// Vanilla's own pig model: the quadruped base (leg 6, left legs mirrored) with a two-box head
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
/// Vanilla's own cow model (its own base-cow-model construction): four-box head (head, snout, two horns),
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

/// Vanilla's own sheep model base body (no wool layer): quadruped base (leg 12, right legs
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

/// Vanilla's own sheep-fur-model fur-layer construction: the wool overlay vanilla's own sheep-wool layer draws over
/// [`sheep_model`] whenever the sheep is not sheared. Not a second skeleton —
/// see `docs/entity-rendering.md`'s wool section — its six parts share
/// `sheep_model`'s part *names* and pivots exactly (`head`, `body`,
/// `right_hind_leg`, `left_hind_leg`, `right_front_leg`, `left_front_leg`), the
/// same discipline `equipment.rs`'s armour meshes follow against
/// [`humanoid_root`]: a caller poses each wool part off the sheep body's own
/// already-animated `part_transforms`, reading and never mutating.
///
/// Three real deviations from the base body mesh, all read from
/// vanilla's own sheep-fur-model source rather
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
///   (`quadruped_root`'s `mirror_right`, matching vanilla's own quadruped-model
///   leg construction),
///   but vanilla's own sheep-fur-model fur-layer construction builds one `CubeListBuilder` and reuses
///   it for all four legs with no `.mirror()` call, so the wool sheet's leg
///   region is not flipped for the right side the way the body's is.
///
/// Sheet 64×32 (vanilla's own mesh definition declared at 64×32), same as the body.
/// Adult only: vanilla's own baby-sheep model/`textures/entity/sheep/sheep_wool_baby.png` is
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
    // vanilla's own texture-diffuse-color table, in dye-color id order, with vanilla's fixed
    // 0.75 brightness already applied (`floor(channel * 0.75)`), transcribed
    // from the literal constants in the decompiled 26.2 client source.
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

/// Vanilla's own adult-chicken model (its own base-chicken-model construction): head with beak and wattle
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
