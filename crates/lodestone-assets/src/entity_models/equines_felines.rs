use super::*;

/// Vanilla's own abstract-equine-model body-mesh construction, no deformation: the shared
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

/// Skeleton horse: the base equine mesh, unscaled (vanilla's own undead-horse
/// renderer bakes
/// its own horse model on its own skeleton-horse model layer, which is
/// the abstract-equine-model body-mesh construction with no deformation and no
/// mesh transformer
/// applied — vanilla's own layer-definitions table reuses its horse body layer
/// as-is). Fixed
/// texture, no colour/markings variant.
pub fn skeleton_horse_model() -> EntityModelDef {
    equine_base_model()
}

/// Zombie horse: identical to skeleton horse — same unscaled base equine mesh,
/// same horse-body-layer reuse in vanilla's own layer-definitions table. Fixed texture.
pub fn zombie_horse_model() -> EntityModelDef {
    equine_base_model()
}

/// Horse: the base equine mesh baked at `scaling(1.1)`
/// (vanilla's own layer-definitions table applies a `1.1F` mesh-transformer
/// scaling to the horse body layer).
/// Colour is a real variant (vanilla's own horse variant field, 7 coats); markings
/// (`Markings`, 5 patterns incl. "none") are an independent second texture
/// layer — see the module-level note above `equine_base_root` and
/// `horse_markings_texture` below.
pub fn horse_model() -> EntityModelDef {
    scaled(equine_base_model(), 1.1)
}

pub(super) fn horse_color_texture(v: EntityVariant) -> &'static str {
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
/// (vanilla's own no-markings variant maps to vanilla's invisible-texture sentinel in
/// its own horse-marking layer). Deliberately not an `EntityTexture`/
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

/// vanilla's own donkey model's body-layer construction: the base equine mesh with vanilla's
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

/// Donkey: vanilla's own donkey-model body-layer construction at its own
/// donkey-scale constant of `0.87F`.
pub fn donkey_model() -> EntityModelDef {
    donkey_body_root(0.87)
}

/// Mule: the same donkey-model mesh, baked at vanilla's own mule-scale constant
/// of `0.92F`
/// instead (vanilla's own layer-definitions table calls the donkey-model
/// body-layer construction with `0.92F`).
pub fn mule_model() -> EntityModelDef {
    donkey_body_root(0.92)
}

/// Vanilla's own llama-model body-layer construction: head (with neck and two ears), body, two
/// chest boxes (vanilla toggles visibility via `state.hasChest`; baked in
/// unconditionally, see the donkey chest note above), and four legs — all
/// direct root children, no deeper nesting. Sheet 128×64 (llama is the only
/// model in this corpus wider than 64px). `trader_llama` reuses this exact
/// mesh (vanilla's own layer-definitions table puts the same llama body layer
/// under both
/// its own llama and trader-llama model-layer keys).
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
/// same baked mesh definition for both (vanilla's own layer-definitions table). Only the
/// renderer differs (decor/carpet layer), which is out of scope for geometry.
pub fn trader_llama_model() -> EntityModelDef {
    llama_model()
}

pub(super) fn llama_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Llama(LlamaColor::Creamy) => "entity/llama/llama_creamy",
        EntityVariant::Llama(LlamaColor::White) => "entity/llama/llama_white",
        EntityVariant::Llama(LlamaColor::Brown) => "entity/llama/llama_brown",
        EntityVariant::Llama(LlamaColor::Gray) => "entity/llama/llama_gray",
        _ => "entity/llama/llama_creamy",
    }
}

/// vanilla's own adult feline model's body-mesh construction: the body mesh shared by cat and ocelot.
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
/// vanilla's own ocelot-renderer texture-location query, which returns one hardcoded path.
pub fn ocelot_model() -> EntityModelDef {
    feline_base_model()
}

/// Cat: the feline mesh baked at vanilla's own adult-cat-model transformer constant of
/// `scaling(0.8F)` (vanilla's own layer-definitions table applies that
/// transformer to the feline body layer). Breed is a real
/// variant (11 breeds); the collar tint (vanilla's own cat-collar layer) is a
/// runtime dye-colour overlay, not a texture-file variant, so it's out of
/// scope here.
pub fn cat_model() -> EntityModelDef {
    scaled(feline_base_model(), 0.8)
}

pub(super) fn cat_coat_texture(v: EntityVariant) -> &'static str {
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

/// Vanilla's own adult-wolf-model body-layer construction: `head` (empty, pivot-only) holding a
/// `real_head` child with four boxes (main head, two identically-textured
/// ear boxes placed by origin sign rather than mirroring, and a snout); `body`
/// and `upper_body` are independent, both rotated `PI/2`; four legs share two
/// `CubeListBuilder`s (`leftLeg`/`rightLeg`, the latter `.mirror()`ed) reused
/// across hind and front pairs, exactly like vanilla's own blaze model's ring reuse; `tail`
/// (empty, pivot-only) holds a `real_tail` child. Sheet 64×32, unscaled
/// (vanilla's own layer-definitions table's wolf body layer has no mesh transformer).
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

pub(super) fn wolf_coat_texture(v: EntityVariant) -> &'static str {
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
            // vanilla's own wolf texture-resolution query appends `_tame`/`_angry` to the breed's file
            // stem for the other two states; `Pale`'s stem has no breed suffix
            // (vanilla's own wolf-variant registration for the pale/"wolf" entry), so its
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

/// Vanilla's own wolf texture-resolution query does its own with-default-namespace string
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

/// vanilla's own parrot model's body-layer construction: body, tail, two wings (sharing one
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

pub(super) fn parrot_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Parrot(ParrotColor::RedBlue) => "entity/parrot/parrot_red_blue",
        EntityVariant::Parrot(ParrotColor::Blue) => "entity/parrot/parrot_blue",
        EntityVariant::Parrot(ParrotColor::Green) => "entity/parrot/parrot_green",
        EntityVariant::Parrot(ParrotColor::YellowBlue) => "entity/parrot/parrot_yellow_blue",
        // Vanilla's own filename is spelled "grey", not "gray" — kept verbatim
        // even though the enum case (matching vanilla's own parrot-variant gray entry) is not.
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
