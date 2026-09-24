use super::*;

// ============================================================================
// Second batch of overworld-priority mobs, ordered by how often a player
// actually encounters them: polar_bear (common in snowy biomes), the illager
// raid roster (pillager/vindicator/evoker/illusioner, all one shared mesh),
// ravager, allay, shulker.
// ============================================================================

/// Vanilla's own polar-bear-model body-layer construction (a quadruped model): head (main box, mouth,
/// 2 ears — all direct siblings on one part, not nested), body, and 4
/// unparented legs, baked at `scaling(1.2)`. Sheet 128×64. Fixed texture; the
/// baby variant is a separate model-layer/renderer scale, out of scope like
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

/// Vanilla's own illager-model body-layer construction: the shared mesh for pillager, vindicator,
/// evoker and illusioner (vanilla's own layer-definitions table puts the identical
/// illager body layer under all four model-layer entries, unscaled). `head`
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

/// Evoker: the illager mesh, unscaled. Fixed texture. (vanilla's own evoker-fangs model is a
/// separate summon-effect entity, out of scope here.)
pub fn evoker_model() -> EntityModelDef {
    illager_base_model()
}

/// Illusioner: the illager mesh, unscaled. Fixed texture.
pub fn illusioner_model() -> EntityModelDef {
    illager_base_model()
}

/// vanilla's own ravager model's body-layer construction: `neck` (1 box) holds `head` (2 boxes,
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

/// vanilla's own allay model's body-layer construction: the mesh's *own* root is empty and holds one
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

/// vanilla's own shulker model's body-layer construction: `lid`, `base` and `head`, all direct root
/// children with no nesting. Sheet 64×64. Fixed to the default (purple)
/// skin: vanilla's actual texture is a genuine `DyeColor` (16-way + a
/// colourless default) variant (vanilla's own shulker-renderer texture-location query), which
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

/// Vanilla's own glow-squid renderer reuses its own squid model verbatim; only the texture path
/// changes (`entity/squid/glow_squid`, not e.g. a tinted overlay — the glow
/// itself is an emissive-texture/render-layer effect, not geometry).
pub fn glow_squid_model() -> EntityModelDef {
    squid_model()
}

/// Vanilla's own wandering-trader renderer reuses its own villager model verbatim (bakes
/// its own wandering-trader model layer, the same mesh shape as the plain
/// villager), swapping only the base skin
/// (`entity/wandering_trader/wandering_trader`); profession-specific
/// clothing layers are a separate custom-head-layer/item-layer concern this
/// port doesn't model, same as for villager professions.
pub fn wandering_trader_model() -> EntityModelDef {
    villager_model()
}

/// Mooshroom: the plain cow model, unscaled — vanilla's mushroom growth is a
/// render layer (its own mushroom-cow mushroom layer), not extra mesh geometry.
pub fn mooshroom_model() -> EntityModelDef {
    cow_model()
}

// ============================================================================
// Projectiles: rigs whose renderer is **not** vanilla's own living-entity renderer
// ============================================================================
//
// Every other entry in this file is placed by vanilla's own living-entity renderer's pose
// stack, which flips Y (`scale(-1, -1, 1)`) and lifts by `1.501` blocks. The
// two entries below are not: vanilla's own arrow renderer and thrown-trident renderer both
// extend its own base entity-renderer directly, which applies **neither** — the base
// entity renderer
// itself contains no `scale(` call at all, against the living-entity renderer's
// own render step,
// which has both. So these meshes are
// authored in the *world* orientation (+Y up), not the Y-down mob orientation,
// and `lodestone_render::entity::projectile_model_matrix` places them.
//
// The consequence for the geometry here: **the long axis is not Y.** An arrow's
// shaft runs along `+X` (the `cross` box spans `x ∈ [-12, +4]` texels, tip at
// high X), which is why vanilla rotates pitch about `Axis.ZP` and not `XP`. A
// trident's pole runs along `−Y` (spikes at negative Y below a pole spanning
// `y ∈ [+2, +27]`), which is why vanilla's own thrown-trident renderer adds `+90°` to the
// pitch: that offset is what turns the pole's axis into the arrow's. Both end
// up pointing along the entity's velocity; see `docs/projectile-renderers.md`.

/// Vanilla's own arrow-model body-layer construction — the rig vanilla's own arrow renderer bakes, shared by
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
/// vanilla (its own model-part cube type does the same); the degenerate ones rasterise no
/// fragments, and the corpus tests that walk UVs skip them explicitly via
/// `quad_is_degenerate`.
///
/// Two scale factors, both real and both easy to lose:
///
/// * The **whole mesh** is `0.9×`. Vanilla's own mesh-definition construction (`mesh.transformed(pose
///   -> pose.scaled(0.9F)), 32, 32)` looks like it scales every part, but
///   vanilla's own part-definition transformed step applies the function to *its own* pose and
///   copies its children untouched — so it is the
///   **root** pose that carries the 0.9, and children inherit it through the
///   transform chain. Modelled here as a root [`PartPose::scale`], which is
///   exactly that.
/// * `back` is a further `0.8×` (vanilla's own with-scale step at `0.8F`), so the fletching
///   ends up at `0.72×`.
///
/// Vanilla's own arrow-model per-frame pose step also adds a `zRot` wobble from `state.shake` for the
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

/// Vanilla's own trident-model layer construction — the rig vanilla's own thrown-trident renderer bakes. Sheet
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
/// Vanilla's own thrown-trident is-foil query. Not modelled: enchantment glint needs its own render
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

/// Wind charge / breeze wind charge: two boxes under one `bone` pivot, sheet
/// 64×32 — transcribed from vanilla's own wind-charge-model body-layer construction.
///
/// `bone`'s two children are named `wind` (a flattened ring, two boxes tilted
/// 45° about Y) and `wind_charge` (the plain 4×4×4 inner cube). Both entity
/// types share this rig: vanilla's own entity-renderers registration table registers both
/// both the `wind_charge` and `breeze_wind_charge` entity types against the
/// same wind-charge renderer, so `breeze_wind_charge` is an alias in
/// [`canonical_model_name_for_type`](crate::entity::canonical_model_name_for_type)
/// rather than a second corpus entry — the same shape as `Bogged` → `skeleton`.
///
/// # What this rig does not carry
///
/// Vanilla's own wind-charge-model per-frame pose step spins `wind_charge` and `wind` in opposite
/// directions at 16°/tick, driven by `ageInTicks`; there is no per-tick clock
/// plumbed to a corpus rig's parts here (the same gap [`shulker_bullet_model`]
/// documents for its own three-axis tumble), so both parts are baked at their
/// rest pose. And unlike every other entry in this table,
/// Vanilla's own wind-charge-renderer submit step applies **neither** `scale(-1, -1, 1)` nor any
/// `mulPose` rotation — it submits the model at the dispatcher's bare
/// translate, so the vanilla box union does not turn to face the entity's
/// direction of travel at all, only the (unported) internal spin moves it.
/// [`non_living_vehicle_placement`](crate::entity::non_living_vehicle_placement)
/// has no "translate only" placement to route this through, so it takes the
/// same fixed-orientation-under-flip treatment as `shulker_bullet`/
/// `wither_skull` — a whole-body yaw rotation and a scale flip vanilla does not
/// apply. Both boxes are close to rotationally symmetric (a 45°-tilted square
/// ring plus a cube), which is what makes the flip and the extra yaw
/// non-obvious on screen rather than a visible mirroring defect.
pub fn wind_charge_model() -> EntityModelDef {
    let wind = PartDef::new(PartPose::offset_and_rotation(0.0, 0.0, 0.0, 0.0, -0.785_4, 0.0))
        .with_cube(cube([-4.0, -1.0, -4.0], [8.0, 2.0, 8.0], [15.0, 20.0]))
        .with_cube(cube([-3.0, -2.0, -3.0], [6.0, 4.0, 6.0], [0.0, 9.0]));
    let wind_charge =
        PartDef::new(PartPose::ZERO).with_cube(cube([-2.0, -2.0, -2.0], [4.0, 4.0, 4.0], [0.0, 0.0]));
    let bone = PartDef::new(PartPose::ZERO)
        .with_child("wind", wind)
        .with_child("wind_charge", wind_charge);
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root: PartDef::new(PartPose::ZERO).with_child("bone", bone),
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

pub(super) fn mooshroom_color_texture(v: EntityVariant) -> &'static str {
    match v {
        EntityVariant::Mooshroom(MooshroomColor::Red) => "entity/cow/mooshroom_red",
        EntityVariant::Mooshroom(MooshroomColor::Brown) => "entity/cow/mooshroom_brown",
        _ => "entity/cow/mooshroom_red",
    }
}
