//! Hand-ported *block-entity* model geometry for the 26.2 family — the cuboid
//! rigs that vanilla's `BlockEntityRenderer`s draw and that no block model
//! covers.
//!
//! # Why this module has to exist at all
//!
//! A block entity's appearance is not always in its block model. `chest.json`
//! in the real 26.2 `client.jar` is, verbatim:
//!
//! ```json
//! { "textures": { "particle": "minecraft:block/oak_planks" } }
//! ```
//!
//! — **zero elements**. Every visible triangle of a chest comes from
//! `ChestRenderer`/`ChestModel`, so a client with no block-entity renderer draws
//! a chest as a *hole in the world*, not as a slightly-wrong box. That is the
//! single highest-value thing in this module and the reason chest is first.
//!
//! The converse is just as important and much easier to get wrong from memory:
//! **a 26.2 sign is a real block model.** `assets/minecraft/blockstates/oak_sign.json`
//! maps every one of the 16 `rotation` values to a `block/oak_sign_rot_N` model
//! with genuine geometry, and `StandingSignRenderer` (checked in
//! `.cache/mc/26.2/client-src/.../blockentity/StandingSignRenderer.java`) declares
//! **no model whatsoever** — only text transformations. So there is deliberately
//! no `sign_model()` here: porting one would draw a second board inside the one
//! the block model already meshes. Sign *text* is a text pass, not geometry, and
//! lives in `lodestone-shell`'s `gpu/block_entity_text.rs`.
//!
//! # The geometry is vanilla's, exactly
//!
//! Transcribed from `net/minecraft/client/model/object/chest/ChestModel.java`
//! (26.2 ships de-obfuscated, so `createSingleBodyLayer` /
//! `createDoubleBodyLeftLayer` / `createDoubleBodyRightLayer` are the real
//! names). Texel offsets, box extents, pivots and the 64×64 sheet size are the
//! exact vanilla values; nothing here is rounded or "close enough".
//!
//! # Not entity models, despite sharing every primitive
//!
//! These reuse [`CubeDef`]/[`PartDef`]/[`EntityModelDef`] because the *bake* is
//! identical — vanilla's `ModelPart` does not know or care whether its owner is
//! a mob or a chest. What differs is **placement**, and that difference is
//! load-bearing enough to keep the two corpora apart:
//!
//! * An entity is placed by `entity_model_matrix`, which flips Y
//!   (`scale(-1, -1, 1)`) and lifts by `MODEL_FEET_OFFSET = 1.501` because mob
//!   model space is Y-**down**.
//! * A block entity is **not flipped and not lifted**. `ChestRenderer.submit`'s
//!   whole prologue is one `Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)`,
//!   and the model's own texels then land directly in block space: the chest
//!   `bottom` box spans y `0..10` texels, i.e. `0..0.625` blocks off the floor,
//!   and the `lid` pivot at y `9` puts the closed lid's top at `14/16` — the
//!   real chest height. Reusing the entity placement matrix would bury a chest
//!   1.5 blocks into the floor, upside down.
//!
//! # How to change it
//!
//! * Adding a type: add a `*_model()` builder plus a [`BlockEntityModelEntry`]
//!   to [`BLOCK_ENTITY_MODELS`]. The `name` is the key `lodestone-render`'s
//!   `BlockEntityModelSet` and the shell's texture map use, so it must be
//!   stable. `texture` is a jar path **without** the `assets/<ns>/textures/`
//!   prefix or the `.png` suffix, matching [`crate::entity::EntityTexture`]'s
//!   convention.
//! * Part **names are load-bearest**. `lodestone-render`'s chest renderer looks
//!   `lid` and `lock` up by name to apply the open angle; renaming either
//!   silently freezes the lid shut (the mesh still draws, so no test that only
//!   counts pixels notices). `crates/lodestone-render/src/block_entity.rs`'s
//!   `chest_part_roles` is the one place that mapping lives.
//! * `visible_faces` is indexed by `entity::FACE_ORDER`
//!   (`[Down, Up, West, North, East, South]`), **not** by `Direction`'s own
//!   discriminant order. The double-chest halves depend on this: the right half
//!   omits `East` (index 4) and the left half omits `West` (index 2), which is
//!   `Util.allOfEnumExcept(Direction.EAST)`/`WEST` in `ChestModel`. Getting the
//!   index wrong deletes the wrong face, and the result is a chest with a hole
//!   in its *front* that still passes any "does a chest draw" gate.

use crate::entity::{CubeDef, EntityModelDef, PartDef, PartPose};

/// The chest sheet is 64×64 (`LayerDefinition.create(mesh, 64, 64)` in all three
/// `ChestModel` layers). Asserted against the real `client.jar` PNGs by
/// `lodestone-assets/tests/real_jar.rs`.
const CHEST_SHEET: (u32, u32) = (64, 64);

/// The bell sheet is 32×32 (`BellModel.createBodyLayer`'s
/// `LayerDefinition.create(mesh, 32, 32)`) — smaller than every chest/skull
/// canvas so far, which is why [`bell_model`]'s own test does not reuse
/// [`CHEST_SHEET`].
const BELL_SHEET: (u32, u32) = (32, 32);

/// The banner sheet is 64×64 (`BannerModel`/`BannerFlagModel`'s
/// `LayerDefinition.create(mesh, 64, 64)` — both layers share one canvas
/// size, same as [`CHEST_SHEET`]).
const BANNER_SHEET: (u32, u32) = (64, 64);

/// The shulker sheet is 64×64 (`ShulkerModel.createBoxLayer`'s
/// `LayerDefinition.create(mesh, 64, 64)`). Same size as [`CHEST_SHEET`] and
/// [`BANNER_SHEET`], named separately so a jar change is one edit per family.
const SHULKER_SHEET: (u32, u32) = (64, 64);

/// The shield sheet is 64×64 (`ShieldModel.createLayer`'s
/// `LayerDefinition.create(mesh, 64, 64)`). Same canvas size as
/// [`CHEST_SHEET`]/[`BANNER_SHEET`]/[`SHULKER_SHEET`], named separately for
/// the same reason those are: a jar change to any one family is one constant
/// to edit.
const SHIELD_SHEET: (u32, u32) = (64, 64);

/// The book sheet is 64×**32** (`BookModel.createBodyLayer`'s
/// `LayerDefinition.create(mesh, 64, 32)`) — the only non-square canvas in this
/// module, so a builder that reused [`CHEST_SHEET`] would halve every `v`
/// coordinate and draw the page texture at the wrong scale rather than not at
/// all.
const BOOK_SHEET: (u32, u32) = (64, 32);

/// The conduit eye sheet, 16×16 — `ConduitRenderer.createEyeLayer()`'s
/// `LayerDefinition.create(mesh, 16, 16)`.
const CONDUIT_EYE_SHEET: (u32, u32) = (16, 16);

/// The conduit wind sheet, 64×32 — `ConduitRenderer.createWindLayer()`'s
/// `LayerDefinition.create(mesh, 64, 32)`. Same canvas size as [`BOOK_SHEET`],
/// named separately so a jar change to either is one edit.
const CONDUIT_WIND_SHEET: (u32, u32) = (64, 32);

/// The conduit's inactive-shell sheet, 32×16 — `ConduitRenderer.createShellLayer()`'s
/// `LayerDefinition.create(mesh, 32, 16)`.
const CONDUIT_SHELL_SHEET: (u32, u32) = (32, 16);

/// The conduit's active-shell ("cage") sheet, 32×16 — `ConduitRenderer.createCageLayer()`'s
/// `LayerDefinition.create(mesh, 32, 16)`. Same dimensions as
/// [`CONDUIT_SHELL_SHEET`] but a genuinely different sheet
/// (`entity/conduit/cage`, not `entity/conduit/base`) and a genuinely
/// different box (8×8×8, not 6×6×6) — named separately rather than reused.
const CONDUIT_CAGE_SHEET: (u32, u32) = (32, 16);

/// `entity::FACE_ORDER` index of the `West` face — see the module doc on why
/// this is not `Direction as usize`.
const FACE_WEST: usize = 2;
/// `entity::FACE_ORDER` index of the `East` face.
const FACE_EAST: usize = 4;

/// One ported block-entity model: a stable `name`, the jar texture path it
/// draws with, and a builder producing the bake-ready [`EntityModelDef`].
///
/// Unlike [`crate::entity_models::EntityModelEntry`] there is no
/// `EntityTexture` variant selector: a block entity's sheet is chosen by
/// *block state and NBT* (a trapped chest, an oxidised copper chest, a dyed
/// shulker), which the renderer resolves per-instance. `texture` here is only
/// the **default** sheet — the one a gate with no world state can still draw.
#[derive(Clone, Debug)]
pub struct BlockEntityModelEntry {
    /// Stable identifier for the model. Keys the renderer's mesh map and the
    /// shell's texture map; not a registry id.
    pub name: &'static str,
    /// Default jar texture path, without `assets/<ns>/textures/` or `.png`.
    pub texture: &'static str,
    /// Builds the bake-ready model.
    pub build: fn() -> EntityModelDef,
}

/// Every block-entity model ported so far.
///
/// Three chest layers (vanilla genuinely has three separate chest *layers*,
/// not one layer posed three ways — a double chest's halves are 15 texels wide
/// instead of 14 and each omits the face that meets its partner, so
/// `left`/`right` cannot be derived from `single` by a transform), plus the two
/// skull/head canvases — see [`skull_mob_model`]'s doc for why there are two.
pub const BLOCK_ENTITY_MODELS: &[BlockEntityModelEntry] = &[
    BlockEntityModelEntry {
        name: "chest",
        texture: "entity/chest/normal",
        build: chest_single_model,
    },
    BlockEntityModelEntry {
        name: "chest_left",
        texture: "entity/chest/normal_left",
        build: chest_double_left_model,
    },
    BlockEntityModelEntry {
        name: "chest_right",
        texture: "entity/chest/normal_right",
        build: chest_double_right_model,
    },
    BlockEntityModelEntry {
        name: "skull_mob",
        texture: "entity/skeleton/skeleton",
        build: skull_mob_model,
    },
    BlockEntityModelEntry {
        name: "skull_humanoid",
        texture: "entity/zombie/zombie",
        build: skull_humanoid_model,
    },
    BlockEntityModelEntry {
        name: "bell",
        texture: "entity/bell/bell_body",
        build: bell_model,
    },
    BlockEntityModelEntry {
        name: "banner_body",
        texture: "entity/banner/banner_base",
        build: banner_body_model,
    },
    BlockEntityModelEntry {
        name: "banner_flag",
        texture: "entity/banner/banner_base",
        build: banner_flag_model,
    },
    BlockEntityModelEntry {
        name: "shulker_box",
        texture: "entity/shulker/shulker",
        build: shulker_box_model,
    },
    BlockEntityModelEntry {
        name: "book",
        texture: "entity/enchantment/enchanting_table_book",
        build: book_model,
    },
    BlockEntityModelEntry {
        name: "banner_wall_body",
        texture: "entity/banner/banner_base",
        build: banner_wall_body_model,
    },
    BlockEntityModelEntry {
        name: "banner_wall_flag",
        texture: "entity/banner/banner_base",
        build: banner_wall_flag_model,
    },
    BlockEntityModelEntry {
        name: "decorated_pot_base",
        texture: "entity/decorated_pot/decorated_pot_base",
        build: decorated_pot_base_model,
    },
    BlockEntityModelEntry {
        name: "decorated_pot_side_front",
        texture: "entity/decorated_pot/decorated_pot_side",
        build: decorated_pot_side_front_model,
    },
    BlockEntityModelEntry {
        name: "decorated_pot_side_back",
        texture: "entity/decorated_pot/decorated_pot_side",
        build: decorated_pot_side_back_model,
    },
    BlockEntityModelEntry {
        name: "decorated_pot_side_left",
        texture: "entity/decorated_pot/decorated_pot_side",
        build: decorated_pot_side_left_model,
    },
    BlockEntityModelEntry {
        name: "decorated_pot_side_right",
        texture: "entity/decorated_pot/decorated_pot_side",
        build: decorated_pot_side_right_model,
    },
    BlockEntityModelEntry {
        name: "conduit_eye",
        texture: "entity/conduit/closed_eye",
        build: conduit_eye_model,
    },
    BlockEntityModelEntry {
        name: "conduit_wind",
        texture: "entity/conduit/wind",
        build: conduit_wind_model,
    },
    BlockEntityModelEntry {
        name: "conduit_shell",
        texture: "entity/conduit/base",
        build: conduit_shell_model,
    },
    BlockEntityModelEntry {
        name: "conduit_cage",
        texture: "entity/conduit/cage",
        build: conduit_cage_model,
    },
    BlockEntityModelEntry {
        name: "shield",
        // The default/no-pattern sheet — `ShieldSpecialRenderer.submit` picks
        // this one whenever the stack has neither a stored pattern layer nor
        // a `minecraft:base_color`, which is every shield straight off a
        // crafting table. `lodestone_render::block_entity::shield_texture_stem`
        // is the per-instance resolver a real draw uses; this is only the
        // default a gate with no item state can still draw.
        texture: "entity/shield/shield_base_nopattern",
        build: shield_model,
    },
    BlockEntityModelEntry {
        name: "copper_golem_statue_standing",
        texture: "entity/copper_golem/copper_golem",
        build: copper_golem_statue_standing_model,
    },
    BlockEntityModelEntry {
        name: "copper_golem_statue_running",
        texture: "entity/copper_golem/copper_golem",
        build: copper_golem_statue_running_model,
    },
    BlockEntityModelEntry {
        name: "copper_golem_statue_sitting",
        texture: "entity/copper_golem/copper_golem",
        build: copper_golem_statue_sitting_model,
    },
    BlockEntityModelEntry {
        name: "copper_golem_statue_star",
        texture: "entity/copper_golem/copper_golem",
        build: copper_golem_statue_star_model,
    },
];

/// Looks a model entry up by its stable name.
#[must_use]
pub fn block_entity_model(name: &str) -> Option<&'static BlockEntityModelEntry> {
    BLOCK_ENTITY_MODELS.iter().find(|e| e.name == name)
}

/// A single chest — `ChestModel.createSingleBodyLayer()`.
///
/// ```text
/// bottom  texOffs(0, 19)  box(1, 0, 1,  14, 10, 14)  pose ZERO
/// lid     texOffs(0,  0)  box(1, 0, 0,  14,  5, 14)  pose offset(0, 9, 1)
/// lock    texOffs(0,  0)  box(7,-2, 14,  2,  4,  1)  pose offset(0, 9, 1)
/// ```
///
/// `lid` and `lock` share a pivot on purpose: `ChestModel.setupAnim` assigns
/// `this.lock.xRot = this.lid.xRot`, so the latch swings with the lid rather
/// than staying put. They are **siblings**, not parent and child — a nested
/// `lock` would compose the pivot twice.
#[must_use]
pub fn chest_single_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [1.0, 0.0, 1.0],
                [14.0, 10.0, 14.0],
                [0.0, 19.0],
            )),
        )
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef::new(
                [1.0, 0.0, 0.0],
                [14.0, 5.0, 14.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "lock",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef::new(
                [7.0, -2.0, 14.0],
                [2.0, 4.0, 1.0],
                [0.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: CHEST_SHEET.0,
        texture_height: CHEST_SHEET.1,
        root,
    }
}

/// The left half of a double chest — `ChestModel.createDoubleBodyLeftLayer()`.
///
/// Boxes start at x `0` and are 15 wide (not 1/14 as in the single chest), and
/// every box omits its `West` face: that face is the seam against the right
/// half and is never visible. See the module doc for why the index is
/// [`FACE_WEST`] and not `Direction::West as usize`.
#[must_use]
pub fn chest_double_left_model() -> EntityModelDef {
    let hide = hide_face(FACE_WEST);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([0.0, 0.0, 1.0], [15.0, 10.0, 14.0], [0.0, 19.0])
            }),
        )
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([0.0, 0.0, 0.0], [15.0, 5.0, 14.0], [0.0, 0.0])
            }),
        )
        .with_child(
            "lock",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([0.0, -2.0, 14.0], [1.0, 4.0, 1.0], [0.0, 0.0])
            }),
        );
    EntityModelDef {
        texture_width: CHEST_SHEET.0,
        texture_height: CHEST_SHEET.1,
        root,
    }
}

/// The right half of a double chest — `ChestModel.createDoubleBodyRightLayer()`.
///
/// Boxes start at x `1` and are 15 wide, and every box omits its `East` face
/// (the seam against the left half).
#[must_use]
pub fn chest_double_right_model() -> EntityModelDef {
    let hide = hide_face(FACE_EAST);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "bottom",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([1.0, 0.0, 1.0], [15.0, 10.0, 14.0], [0.0, 19.0])
            }),
        )
        .with_child(
            "lid",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([1.0, 0.0, 0.0], [15.0, 5.0, 14.0], [0.0, 0.0])
            }),
        )
        .with_child(
            "lock",
            PartDef::new(PartPose::offset(0.0, 9.0, 1.0)).with_cube(CubeDef {
                visible_faces: hide,
                ..CubeDef::new([15.0, -2.0, 14.0], [1.0, 4.0, 1.0], [0.0, 0.0])
            }),
        );
    EntityModelDef {
        texture_width: CHEST_SHEET.0,
        texture_height: CHEST_SHEET.1,
        root,
    }
}

/// A skull/head's single box — `SkullModel.createHeadModel()`:
/// `addBox(-4, -8, -4, 8, 8, 8)` at `PartPose.ZERO`, texel offset `(0, 0)`.
///
/// Unlike the chest models, this is authored in the **same Y-down convention
/// as a mob's own head part** — vanilla never re-authored it block-space-up
/// the way `ChestModel` was. `SkullBlockRenderer`'s own placement transforms
/// (`createGroundTransformation`/`createWallTransformation`, ported as
/// `lodestone_render::block_entity::{skull_ground_placement_matrix,
/// skull_wall_placement_matrix}`) apply vanilla's `scale(-1, -1, 1)` flip to
/// compensate, exactly the sign [`crate::entity::entity_model_matrix`] uses.
/// Porting this box pre-flipped (to look "right" in isolation) would double
/// the flip once placement is applied.
fn skull_head_part() -> PartDef {
    PartDef::new(PartPose::ZERO)
        .with_cube(CubeDef::new([-4.0, -8.0, -4.0], [8.0, 8.0, 8.0], [0.0, 0.0]))
}

/// The 64×32-canvas skull head — `SkullModel.createMobHeadLayer()`. Used by
/// skeleton, wither skeleton and creeper, whose skin PNGs really are 64×32.
///
/// **Two models exist for one box, not one.** The head's `texOffs(0, 0)`
/// placement is identical on both canvases (the cube only occupies the
/// top-left 32×16 texels regardless of total sheet size — the extra height on
/// the 64×64 canvas is room for the "hat" overlay and body parts this
/// renderer does not draw), but UV normalisation divides by the *declared*
/// canvas size at bake time. Baking one model at 64×32 and sampling a 64×64
/// skin (or vice versa) would double or halve the head's `v` extent — a
/// texture-stretch bug invisible in a coverage-only gate, since the mesh
/// still draws a full box either way.
#[must_use]
pub fn skull_mob_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child("head", skull_head_part());
    EntityModelDef {
        texture_width: 64,
        texture_height: 32,
        root,
    }
}

/// The 64×64-canvas skull head — `SkullModel.createHumanoidHeadLayer()`
/// (base head only). Used by zombie (whose skin moved to 64×64) and player
/// (always 64×64). See [`skull_mob_model`] for why the canvas size is a
/// second model rather than a parameter.
///
/// The real `createHumanoidHeadLayer` also adds a `"hat"` overlay child
/// (`texOffs(32, 0)`, inflated `0.25`) — not ported. It is per-player
/// cosmetic geometry (usually empty/transparent) layered *outside* the base
/// head, and every ported skull type here draws with a fixed skin the hat
/// layer would just double-draw against; see the module's block-entity-renderers
/// doc for the tracked gap.
#[must_use]
pub fn skull_humanoid_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child("head", skull_head_part());
    EntityModelDef {
        texture_width: 64,
        texture_height: 64,
        root,
    }
}

/// A bell's swinging body and its flared bottom rim —
/// `BellModel.createBodyLayer()`:
///
/// ```text
/// bell_body  texOffs(0,  0)  box(-3, -6, -3,  6, 7, 6)  pose offset(8, 12, 8)
///   bell_base  texOffs(0, 13)  box(4, 4, 4,  8, 2, 8)  pose offset(-8, -12, -8)  (child of bell_body)
/// ```
///
/// `bell_base` is **nested inside** `bell_body` in the real jar
/// (`bellBody.addOrReplaceChild("bell_base", …)`), not a sibling under root.
/// Its own local pose `(-8, -12, -8)` exactly cancels `bell_body`'s pivot
/// `(8, 12, 8)`, so the flared rim's *world* pivot lands at the block's own
/// corner `(0, 0, 0)` — the rim (`4..12, 4..6, 4..12` texels there) then sits
/// directly below the tapered body (`5..11, 6..13, 5..11` texels once
/// `bell_body`'s own pivot is folded in), which is exactly what a bell's
/// flared bottom skirt should do. The nesting also matters for the
/// animation: `BellModel.setupAnim` only ever poses `bellBody.xRot`/`zRot`
/// (see [`crate::block_entity_models`]'s sibling doc in `lodestone-render`'s
/// `bell_shake_angle`) and the rim swings with it *because* it is a child,
/// the same "shared handle" reasoning [`chest_single_model`]'s doc gives for
/// why `lid`/`lock` share a pivot rather than nesting one inside the other —
/// here the correct shape is the opposite: nested, not siblings, because
/// vanilla itself nests them.
///
/// Authored **block-space-up**, the same convention [`chest_single_model`]
/// uses and unlike [`skull_head_part`]: `BellRenderer.submit` applies no
/// `scale(-1, -1, 1)` flip (unlike `SkullBlockRenderer`), so `CubeDef::origin`
/// and `PartPose` add directly with no sign flip.
#[must_use]
pub fn bell_model() -> EntityModelDef {
    let bell_base = PartDef::new(PartPose::offset(-8.0, -12.0, -8.0))
        .with_cube(CubeDef::new([4.0, 4.0, 4.0], [8.0, 2.0, 8.0], [0.0, 13.0]));
    let bell_body = PartDef::new(PartPose::offset(8.0, 12.0, 8.0))
        .with_cube(CubeDef::new([-3.0, -6.0, -3.0], [6.0, 7.0, 6.0], [0.0, 0.0]))
        .with_child("bell_base", bell_base);
    let root = PartDef::new(PartPose::ZERO).with_child("bell_body", bell_body);
    EntityModelDef {
        texture_width: BELL_SHEET.0,
        texture_height: BELL_SHEET.1,
        root,
    }
}

/// A standing banner's pole and cross-bar — `BannerModel.createBodyLayer(true)`:
///
/// ```text
/// pole  texOffs(44, 0)  addBox(-1, -42, -1,  2, 42, 2)  pose ZERO
/// bar   texOffs(0, 42)  addBox(-10, -44, -1,  20, 2, 2)  pose ZERO
/// ```
///
/// Only `standing = true` is ported (this issue's own scope — a wall banner
/// is `createBodyLayer(false)`, a second entry later with different box
/// origins for `bar` and no `pole` at all). Draws through the ordinary
/// opaque block-entity batcher with [`BlockEntityModelEntry::texture`]'s
/// sheet (`entity/banner/banner_base` — vanilla's `Sheets.BANNER_BASE`),
/// the *wood/cloth* texture, not a pattern mask; the
/// coloured pattern masks are a wholly separate translucent draw list (see
/// `lodestone_render::block_entity`'s banner doc) reusing the *flag* mesh,
/// never this one.
#[must_use]
pub fn banner_body_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "pole",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-1.0, -42.0, -1.0],
                [2.0, 42.0, 2.0],
                [44.0, 0.0],
            )),
        )
        .with_child(
            "bar",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-10.0, -44.0, -1.0],
                [20.0, 2.0, 2.0],
                [0.0, 42.0],
            )),
        );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A standing banner's cloth — `BannerFlagModel.createFlagLayer(true)`:
///
/// ```text
/// flag  texOffs(0, 0)  addBox(-10, 0, -2,  20, 40, 1)  pose offset(0, -44, 0)
/// ```
///
/// One part, one box — deliberately a single rigid box rather than the
/// per-vertex cloth-wave geometry an earlier pass through this doc
/// (mistakenly) assumed vanilla has. `BannerFlagModel.setupAnim` poses only
/// `flag.xRot`, a single per-part rotation
/// (`lodestone_render::block_entity::banner_flag_x_rot`) —
/// [`BlockEntityMesh::part_transforms`](crate::block_entity)'s override
/// mechanism (already used by the chest lid and the bell body) is the right
/// shape for it, not a new animation family.
///
/// Drawn **twice** by a real consumer: once opaque through the same
/// `entity/banner/banner_base` sheet [`banner_body_model`] uses (vanilla's
/// `submitBanner` passes `Sheets.BANNER_BASE` to both the body and the flag
/// model), and then again, translucent, once per pattern layer — see
/// `lodestone_render::block_entity`'s module doc for the draw-order and
/// pipeline split.
#[must_use]
pub fn banner_flag_model() -> EntityModelDef {
    let flag = PartPose::offset(0.0, -44.0, 0.0);
    let root = PartDef::new(PartPose::ZERO).with_child(
        "flag",
        PartDef::new(flag).with_cube(CubeDef::new([-10.0, 0.0, -2.0], [20.0, 40.0, 1.0], [0.0, 0.0])),
    );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A **wall** banner's cross-bar — `BannerModel.createBodyLayer(false)`:
///
/// ```text
/// bar  texOffs(0, 42)  addBox(-10, -20.5, 9.5,  20, 2, 2)  pose ZERO
/// ```
///
/// **No `pole`.** `createBodyLayer` adds the pole only under `if (standing)`, and
/// this is the branch that skips it — a wall banner hangs off a block face, so a
/// standing banner's 42-texel post would be a pole floating in mid-air. That is
/// exactly what happens if the two are conflated, and it is why the gather
/// declined wall banners outright until this mesh existed.
///
/// The `bar` box is not the standing one moved: **both of its `y` and `z` origins
/// differ** (`-20.5, 9.5` against `-44, -1`), from the same ternary pair in
/// `createBodyLayer`. Only the texel offsets and extents are shared, so this is a
/// second entry rather than a placement variant.
#[must_use]
pub fn banner_wall_body_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "bar",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-10.0, -20.5, 9.5],
            [20.0, 2.0, 2.0],
            [0.0, 42.0],
        )),
    );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A **wall** banner's cloth — `BannerFlagModel.createFlagLayer(false)`:
///
/// ```text
/// flag  texOffs(0, 0)  addBox(-10, 0, -2,  20, 40, 1)  pose offset(0, -20.5, 10.5)
/// ```
///
/// The **cube is byte-identical** to [`banner_flag_model`]'s; only the part's rest
/// pose differs (`(0, -20.5, 10.5)` against `(0, -44, 0)`). So this is one mesh
/// that could in principle have been one mesh with a pose override — and it is not,
/// for the reason [`banner_flag_model`]'s doc already gives: the flag's `x_rot`
/// sway is *itself* a pose override, and stacking a second, static override on the
/// same part is how the two silently start fighting over one field.
///
/// `BannerFlagModel.setupAnim` poses `flag.xRot` identically for both kinds — the
/// sway is not attachment-dependent.
#[must_use]
pub fn banner_wall_flag_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "flag",
        PartDef::new(PartPose::offset(0.0, -20.5, 10.5)).with_cube(CubeDef::new(
            [-10.0, 0.0, -2.0],
            [20.0, 40.0, 1.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: BANNER_SHEET.0,
        texture_height: BANNER_SHEET.1,
        root,
    }
}

/// A shield — `ShieldModel.createLayer()`:
///
/// ```text
/// plate   texOffs(0,  0)  addBox(-6, -11, -2,  12, 22, 1)  pose ZERO
/// handle  texOffs(26, 0)  addBox(-1,  -3, -1,   2,  6, 6)  pose ZERO
/// ```
///
/// Two parts, both `PartPose::ZERO` — unlike a banner's body+flag, vanilla
/// draws the whole shield (plate and handle together) as **one**
/// `submitModel` call, opaque, then re-submits this exact same mesh once per
/// pattern layer through the translucent pattern pipeline
/// (`BannerRenderer.submitPatterns(..., this.model, Unit.INSTANCE, banner =
/// false, ...)` in `ShieldSpecialRenderer.submit` — the `model` argument is
/// the whole `ShieldModel`, not a `flag`-only sub-part the way a banner's
/// masks are). So this crate's shield has no analogue of a banner's
/// `"banner_flag"`: a caller drawing a pattern layer reuses this same
/// `"shield"` mesh, at the
/// same placement, with a different sprite and tint — see
/// `lodestone_render::block_entity`'s shield section for the draw-order this
/// enables.
#[must_use]
pub fn shield_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "plate",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-6.0, -11.0, -2.0],
                [12.0, 22.0, 1.0],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "handle",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [-1.0, -3.0, -1.0],
                [2.0, 6.0, 6.0],
                [26.0, 0.0],
            )),
        );
    EntityModelDef {
        texture_width: SHIELD_SHEET.0,
        texture_height: SHIELD_SHEET.1,
        root,
    }
}

/// A shulker box's shell — `ShulkerModel.createBoxLayer()`, via the shared
/// `ShulkerModel.createShellMesh()`:
///
/// ```text
/// lid   texOffs(0,  0)  addBox(-8, -16, -8,  16, 12, 16)  pose offset(0, 24, 0)
/// base  texOffs(0, 28)  addBox(-8,  -8, -8,  16,  8, 16)  pose offset(0, 24, 0)
/// ```
///
/// **`createBoxLayer`, not `createBodyLayer`** — the two share `createShellMesh`
/// and the body layer adds a third `head` part for the *mob*. A block-entity
/// shulker box has no head, and baking the body layer here would draw a shulker's
/// face floating inside every box in the world.
///
/// `lid` and `base` are siblings with the **same** pivot `(0, 24, 0)`, which is
/// the sole reason this type "fits the existing `(model, texture)` batch key as
/// is": `ShulkerBoxRenderer.ShulkerBoxModel.setupAnim` only ever moves `lid`, and
/// a closed box (`progress == 0`) needs no pose override at all — so a shulker
/// box is one static mesh per dye colour and nothing per instance. The open
/// animation is `lid.y = 24 - progress * 8` and `lid.yRot = 270° * progress`, and
/// it needs a container-open signal this client does not have yet; see
/// `docs/block-entity-renderers.md`.
///
/// Authored **block-space-up** like [`chest_single_model`] and [`bell_model`]:
/// `ShulkerBoxRenderer.createModelTransform` folds its own
/// `scale(1, -1, -1) · translate(0, -1, 0)` flip into the *placement* matrix, so
/// the box origins here add to `PartPose` with no sign change.
#[must_use]
pub fn shulker_box_model() -> EntityModelDef {
    let pivot = PartPose::offset(0.0, 24.0, 0.0);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "lid",
            PartDef::new(pivot)
                .with_cube(CubeDef::new([-8.0, -16.0, -8.0], [16.0, 12.0, 16.0], [0.0, 0.0])),
        )
        .with_child(
            "base",
            PartDef::new(pivot)
                .with_cube(CubeDef::new([-8.0, -8.0, -8.0], [16.0, 8.0, 16.0], [0.0, 28.0])),
        );
    EntityModelDef {
        texture_width: SHULKER_SHEET.0,
        texture_height: SHULKER_SHEET.1,
        root,
    }
}

/// An open book — `BookModel.createBodyLayer()`
/// (`net/minecraft/client/model/object/book/BookModel.java`), sheet 64×32:
///
/// ```text
/// left_lid    texOffs( 0,  0)  addBox(-6, -5, -0.005,  6, 10, 0.005)  pose offset(0, 0, -1)
/// right_lid   texOffs(16,  0)  addBox( 0, -5, -0.005,  6, 10, 0.005)  pose offset(0, 0,  1)
/// seam        texOffs(12,  0)  addBox(-1, -5,  0,      2, 10, 0.005)  pose rotation(0, PI/2, 0)
/// left_pages  texOffs( 0, 10)  addBox( 0, -4, -0.99,   5,  8, 1)      pose ZERO
/// right_pages texOffs(12, 10)  addBox( 0, -4, -0.01,   5,  8, 1)      pose ZERO
/// flip_page1  texOffs(24, 10)  addBox( 0, -4,  0,      5,  8, 0.005)  pose ZERO
/// flip_page2  texOffs(24, 10)  addBox( 0, -4,  0,      5,  8, 0.005)  pose ZERO
/// ```
///
/// Three things in that table are deliberate and would each get "cleaned up"
/// by a reader who assumed a transcription error:
///
/// * **The lids and the flip pages are 0.005 texels thick.** They are paper-thin
///   *boxes*, not quads — `bake_cube` emits all six faces of each, two of which
///   are 0.005 texels wide. A mesher that culled near-degenerate cubes would eat
///   the covers and the turning pages and leave only the two page blocks, which
///   still reads as a book.
/// * **`flip_page1` and `flip_page2` share one `CubeListBuilder`** in the jar, so
///   their UVs are identical by construction. They differ only in the per-frame
///   `yRot` `setupAnim` gives them.
/// * **`seam` is the only part with a rest *rotation*** (`PartPose.rotation`, no
///   offset), and `BookModel.setupAnim` never poses it — so the spine's quarter
///   turn must survive as a rest pose rather than being folded into a caller's
///   override list.
///
/// Shared by two registrations, and the *work* is not shared: a lectern's
/// `BookModel.State` is a compile-time constant (see
/// `lodestone_render::block_entity`'s `LECTERN_BOOK_OPENNESS`), while
/// `EnchantTableRenderer`'s is a client-simulated animation state machine with
/// its own per-frame `open`/`flip`/`rot` counters. One mesh, two very different
/// consumers.
///
/// Authored **block-space-up** like [`chest_single_model`] and [`bell_model`]:
/// `LecternRenderer.submit` applies no `scale(-1, -1, 1)` (unlike
/// `SkullBlockRenderer`), so origins and poses add with no sign flip.
#[must_use]
pub fn book_model() -> EntityModelDef {
    // Vanilla builds both flip pages from one `CubeListBuilder`; one `CubeDef`
    // value cloned into both children is the same statement in Rust.
    let flip_page = CubeDef::new([0.0, -4.0, 0.0], [5.0, 8.0, 0.005], [24.0, 10.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "left_lid",
            PartDef::new(PartPose::offset(0.0, 0.0, -1.0)).with_cube(CubeDef::new(
                [-6.0, -5.0, -0.005],
                [6.0, 10.0, 0.005],
                [0.0, 0.0],
            )),
        )
        .with_child(
            "right_lid",
            PartDef::new(PartPose::offset(0.0, 0.0, 1.0)).with_cube(CubeDef::new(
                [0.0, -5.0, -0.005],
                [6.0, 10.0, 0.005],
                [16.0, 0.0],
            )),
        )
        .with_child(
            "seam",
            PartDef::new(PartPose::rotation(
                0.0,
                std::f32::consts::FRAC_PI_2,
                0.0,
            ))
            .with_cube(CubeDef::new(
                [-1.0, -5.0, 0.0],
                [2.0, 10.0, 0.005],
                [12.0, 0.0],
            )),
        )
        .with_child(
            "left_pages",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [0.0, -4.0, -0.99],
                [5.0, 8.0, 1.0],
                [0.0, 10.0],
            )),
        )
        .with_child(
            "right_pages",
            PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
                [0.0, -4.0, -0.01],
                [5.0, 8.0, 1.0],
                [12.0, 10.0],
            )),
        )
        .with_child(
            "flip_page1",
            PartDef::new(PartPose::ZERO).with_cube(flip_page.clone()),
        )
        .with_child(
            "flip_page2",
            PartDef::new(PartPose::ZERO).with_cube(flip_page),
        );
    EntityModelDef {
        texture_width: BOOK_SHEET.0,
        texture_height: BOOK_SHEET.1,
        root,
    }
}

/// `[true; 6]` with one `entity::FACE_ORDER` index cleared.
fn hide_face(index: usize) -> [bool; 6] {
    let mut faces = [true; 6];
    faces[index] = false;
    faces
}

/// `[false; 6]` with one `entity::FACE_ORDER` index set — the inverse of
/// [`hide_face`], for a box only one face of which is ever visible (the
/// decorated pot's side quads, `EnumSet.of(Direction.NORTH)` in the jar).
fn only_face(index: usize) -> [bool; 6] {
    let mut faces = [false; 6];
    faces[index] = true;
    faces
}

/// `entity::FACE_ORDER` index of the `North` face — see the module doc on why
/// this is not `Direction as usize`.
const FACE_NORTH: usize = 3;

/// The decorated pot's base sheet, 32×32 —
/// `DecoratedPotRenderer.createBaseLayer()`'s `LayerDefinition.create(mesh, 32, 32)`.
const DECORATED_POT_BASE_SHEET: (u32, u32) = (32, 32);

/// The decorated pot's side sheet, 16×16 —
/// `DecoratedPotRenderer.createSidesLayer()`'s `LayerDefinition.create(mesh, 16, 16)`.
/// One quad per model, not a full box — see [`decorated_pot_side_part`].
const DECORATED_POT_SIDE_SHEET: (u32, u32) = (16, 16);

/// The decorated pot's base — `DecoratedPotRenderer.createBaseLayer()`: the
/// neck (two nested boxes, deflated then inflated) plus flat top/bottom
/// planes sharing one cube. All three parts draw with the single
/// `decorated_pot_base` sheet regardless of which sherds (if any) are
/// stored, which is why this is one model rather than four.
///
/// `top`/`bottom` share `topBottomPlane`'s cube unchanged (`texOffs(-14, 13)`,
/// `addBox(0, 0, 0, 14, 0, 14)`, no `EnumSet` restriction — a real, if
/// degenerate, six-face box, exactly as the jar authors it) and differ only
/// in the pivot's `y`.
#[must_use]
pub fn decorated_pot_base_model() -> EntityModelDef {
    let top_bottom_cube = || CubeDef::new([0.0, 0.0, 0.0], [14.0, 0.0, 14.0], [-14.0, 13.0]);
    let root = PartDef::new(PartPose::ZERO)
        .with_child(
            "neck",
            PartDef::new(PartPose::offset_and_rotation(
                0.0,
                37.0,
                16.0,
                std::f32::consts::PI,
                0.0,
                0.0,
            ))
            .with_cube(CubeDef::new([4.0, 17.0, 4.0], [8.0, 3.0, 8.0], [0.0, 0.0]).grown(-0.1))
            .with_cube(CubeDef::new([5.0, 20.0, 5.0], [6.0, 1.0, 6.0], [0.0, 5.0]).grown(0.2)),
        )
        .with_child(
            "top",
            PartDef::new(PartPose::offset(1.0, 16.0, 1.0)).with_cube(top_bottom_cube()),
        )
        .with_child(
            "bottom",
            PartDef::new(PartPose::offset(1.0, 0.0, 1.0)).with_cube(top_bottom_cube()),
        );
    EntityModelDef {
        texture_width: DECORATED_POT_BASE_SHEET.0,
        texture_height: DECORATED_POT_BASE_SHEET.1,
        root,
    }
}

/// One decorated-pot side quad — `DecoratedPotRenderer.createSidesLayer()`'s
/// shared `sidePlane`: `texOffs(1, 0).addBox(0, 0, 0, 14, 16, 0,
/// EnumSet.of(Direction.NORTH))`. Only the `North` face is emitted (the box
/// has zero depth, so the other five are degenerate or coincident) — see
/// [`only_face`].
///
/// **Why four models and not one reused four times.** Each of vanilla's four
/// named children (`front`/`back`/`left`/`right`) has its own fixed
/// `PartPose` — not a yaw step around the block's own facing, which is
/// already applied once by [`crate::block_entity_models`]'s placement
/// matrix — so there is no single rest pose the other three could be
/// *derived* from at render time. And because
/// [`BlockEntityInstance`](../../lodestone_render/block_entity/struct.BlockEntityInstance.html)
/// carries exactly one texture for every part it draws, putting all four
/// quads in one model (as the jar's own `sidesRoot` does, for its four
/// separate `submitModelPart` calls) would force all four sherds to share
/// one sheet — the very limitation this type exists to route around. Four
/// single-quad models, one per side, each resolved as its own
/// [`BlockEntityInstance`](../../lodestone_render/block_entity/struct.BlockEntityInstance.html)
/// with its own texture, is the decomposition `docs/block-entity-renderers.md`
/// names.
fn decorated_pot_side_part(pose: PartPose) -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "side",
        PartDef::new(pose).with_cube(CubeDef {
            visible_faces: only_face(FACE_NORTH),
            ..CubeDef::new([0.0, 0.0, 0.0], [14.0, 16.0, 0.0], [1.0, 0.0])
        }),
    );
    EntityModelDef {
        texture_width: DECORATED_POT_SIDE_SHEET.0,
        texture_height: DECORATED_POT_SIDE_SHEET.1,
        root,
    }
}

/// The pot's front side — `offsetAndRotation(1, 16, 15, PI, 0, 0)`.
#[must_use]
pub fn decorated_pot_side_front_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        1.0,
        16.0,
        15.0,
        std::f32::consts::PI,
        0.0,
        0.0,
    ))
}

/// The pot's back side — `offsetAndRotation(15, 16, 1, 0, 0, PI)`.
#[must_use]
pub fn decorated_pot_side_back_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        15.0,
        16.0,
        1.0,
        0.0,
        0.0,
        std::f32::consts::PI,
    ))
}

/// The pot's left side — `offsetAndRotation(1, 16, 1, 0, -PI/2, PI)`.
#[must_use]
pub fn decorated_pot_side_left_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        1.0,
        16.0,
        1.0,
        0.0,
        -std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
    ))
}

/// The pot's right side — `offsetAndRotation(15, 16, 15, 0, PI/2, PI)`.
#[must_use]
pub fn decorated_pot_side_right_model() -> EntityModelDef {
    decorated_pot_side_part(PartPose::offset_and_rotation(
        15.0,
        16.0,
        15.0,
        0.0,
        std::f32::consts::FRAC_PI_2,
        std::f32::consts::PI,
    ))
}

/// The conduit's eye — `ConduitRenderer.createEyeLayer()`:
///
/// ```text
/// eye  texOffs(0, 0)  addBox(-4, -4, 0,  8, 8, 0, CubeDeformation(0.01F))  pose ZERO
/// ```
///
/// A near-planar box: zero depth grown by `0.01` texels on every axis by the
/// deformation, matching vanilla's own `CubeDeformation` rather than a hand
/// wave at "basically a quad". This is the part `ConduitRenderer.submit`
/// billboards toward the camera and re-skins between `open_eye`/`closed_eye`
/// per frame — see `lodestone_render::block_entity::resolve_conduit`'s doc for
/// the billboard and the hunting-state sprite swap, neither of which belongs
/// in the geometry.
#[must_use]
pub fn conduit_eye_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "eye",
        PartDef::new(PartPose::ZERO)
            .with_cube(CubeDef::new([-4.0, -4.0, 0.0], [8.0, 8.0, 0.0], [0.0, 0.0]).grown(0.01)),
    );
    EntityModelDef {
        texture_width: CONDUIT_EYE_SHEET.0,
        texture_height: CONDUIT_EYE_SHEET.1,
        root,
    }
}

/// The conduit's "wind" plume — `ConduitRenderer.createWindLayer()`:
///
/// ```text
/// wind  texOffs(0, 0)  addBox(-8, -8, -8,  16, 16, 16)  pose ZERO
/// ```
///
/// One full 16-texel cube, drawn **twice** per active frame at two different
/// poses sharing this one mesh (`resolve_conduit`'s two `CONDUIT_WIND`
/// instances) — vanilla reuses the same `ModelPart` for both
/// `submitModelPart` calls in `ConduitRenderer.submit`'s active branch.
#[must_use]
pub fn conduit_wind_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "wind",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-8.0, -8.0, -8.0],
            [16.0, 16.0, 16.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: CONDUIT_WIND_SHEET.0,
        texture_height: CONDUIT_WIND_SHEET.1,
        root,
    }
}

/// The conduit's **inactive** shell — `ConduitRenderer.createShellLayer()`:
///
/// ```text
/// shell  texOffs(0, 0)  addBox(-3, -3, -3,  6, 6, 6)  pose ZERO
/// ```
///
/// Drawn slowly spinning (`entity/conduit/base`) whenever the frame is not
/// complete — the `!state.isActive` branch of `ConduitRenderer.submit`. A
/// smaller cube than [`conduit_cage_model`]'s (6×6×6 against 8×8×8): the
/// conduit visibly grows when it activates, not just changes texture.
#[must_use]
pub fn conduit_shell_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "shell",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-3.0, -3.0, -3.0],
            [6.0, 6.0, 6.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: CONDUIT_SHELL_SHEET.0,
        texture_height: CONDUIT_SHELL_SHEET.1,
        root,
    }
}

/// The conduit's **active** shell ("cage") — `ConduitRenderer.createCageLayer()`:
///
/// ```text
/// shell  texOffs(0, 0)  addBox(-4, -4, -4,  8, 8, 8)  pose ZERO
/// ```
///
/// Named `"shell"` in the jar too (`createCageLayer` also calls
/// `addOrReplaceChild("shell", …)`) — the two are still separate *models*
/// here (`conduit_shell` vs `conduit_cage`), each its own
/// [`BlockEntityModelEntry`] with its own sheet, since a real client never
/// draws both in the same frame (`ConduitRenderer.submit` branches on
/// `state.isActive` and picks exactly one).
#[must_use]
pub fn conduit_cage_model() -> EntityModelDef {
    let root = PartDef::new(PartPose::ZERO).with_child(
        "shell",
        PartDef::new(PartPose::ZERO).with_cube(CubeDef::new(
            [-4.0, -4.0, -4.0],
            [8.0, 8.0, 8.0],
            [0.0, 0.0],
        )),
    );
    EntityModelDef {
        texture_width: CONDUIT_CAGE_SHEET.0,
        texture_height: CONDUIT_CAGE_SHEET.1,
        root,
    }
}

/// The copper golem statue's sheet is 64×64 (every one of `CopperGolemModel`'s
/// four `LayerDefinition.create(mesh, 64, 64)` layers — standing, running,
/// sitting, star — shares this canvas size).
const COPPER_GOLEM_SHEET: (u32, u32) = (64, 64);

/// The statue's head — identical geometry in all four poses
/// (`CopperGolemModel.createBodyLayer`/`createRunningPoseBodyLayer`/
/// `createSittingPoseBodyLayer`/`createStarPoseBodyLayer`, `"head"` child):
/// four boxes (a wide skull-ish base, a snout nub, an "antenna" stalk, and
/// its tip), all sharing one deform-inflate pattern.
fn copper_golem_head_cubes() -> Vec<CubeDef> {
    vec![
        CubeDef::new([-4.0, -5.0, -5.0], [8.0, 5.0, 10.0], [0.0, 0.0]).grown(0.015),
        CubeDef::new([-1.0, -2.0, -6.0], [2.0, 3.0, 2.0], [56.0, 0.0]),
        CubeDef::new([-1.0, -9.0, -1.0], [2.0, 4.0, 2.0], [37.0, 8.0]).grown(-0.015),
        CubeDef::new([-2.0, -13.0, -2.0], [4.0, 4.0, 4.0], [37.0, 0.0]).grown(-0.015),
    ]
}

/// `CopperGolemModel.createBodyLayer()` — the statue's `STANDING` pose
/// (`CopperGolemStatueBlock.Pose.STANDING`), and the plainest of the four:
/// every part is a bare `PartPose.offset` with no rotation, the ordinary
/// humanoid-ish rig every other block in this corpus that reuses a mob rig
/// (skull) already assumes.
#[must_use]
pub fn copper_golem_statue_standing_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, 0.0));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let body = PartDef::new(PartPose::offset(0.0, -5.0, 0.0))
        .with_cube(CubeDef::new([-4.0, -6.0, -3.0], [8.0, 6.0, 6.0], [0.0, 15.0]))
        .with_child("head", head)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_cube(CubeDef::new(
                [-3.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [36.0, 16.0],
            )),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_cube(CubeDef::new(
                [0.0, -1.0, -2.0],
                [3.0, 10.0, 4.0],
                [50.0, 16.0],
            )),
        );
    let root = PartDef::new(PartPose::offset(0.0, 24.0, 0.0))
        .with_child("body", body)
        .with_child(
            "right_leg",
            PartDef::new(PartPose::offset(0.0, -5.0, 0.0)).with_cube(CubeDef::new(
                [-4.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [0.0, 27.0],
            )),
        )
        .with_child(
            "left_leg",
            PartDef::new(PartPose::offset(0.0, -5.0, 0.0)).with_cube(CubeDef::new(
                [0.0, 0.0, -2.0],
                [4.0, 5.0, 4.0],
                [16.0, 27.0],
            )),
        );
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

/// `CopperGolemModel.createRunningPoseBodyLayer()` — the `RUNNING` pose.
/// Every limb is a *nested* `PartDefinition`: a bare pivot part (no cube of
/// its own) holding one `_r1` child that carries the real box at a further
/// `offsetAndRotation` — a 3D-pose-tool export shape distinct from every
/// other rig in this corpus, transcribed exactly rather than collapsed into
/// one part, since collapsing would double-apply neither pivot but *would*
/// silently drop the two-stage translate.
#[must_use]
pub fn copper_golem_statue_running_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.7, -5.6, -1.8));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let body_r1 = PartDef::new(PartPose::offset_and_rotation(
        1.1, 0.1, 0.7, 0.1204, -0.0064, -0.0779,
    ))
    .with_cube(CubeDef::new(
        [-4.02, -6.116, -3.5],
        [8.0, 6.0, 6.0],
        [0.0, 15.0],
    ));
    let body = PartDef::new(PartPose::offset(-1.064, -5.0, 0.0))
        .with_child("body_r1", body_r1)
        .with_child("head", head)
        .with_child(
            "right_arm",
            PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_child(
                "right_arm_r1",
                PartDef::new(PartPose::offset_and_rotation(
                    0.7, -0.248, -1.62, 1.0036, 0.0, 0.0,
                ))
                .with_cube(CubeDef::new(
                    [-3.052, -1.11, -2.036],
                    [3.0, 10.0, 4.0],
                    [36.0, 16.0],
                )),
            ),
        )
        .with_child(
            "left_arm",
            PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_child(
                "left_arm_r1",
                PartDef::new(PartPose::offset_and_rotation(
                    0.732, 0.0, 0.0, -0.8715, -0.0535, -0.0449,
                ))
                .with_cube(CubeDef::new(
                    [0.032, -1.1, -2.0],
                    [3.0, 10.0, 4.0],
                    [50.0, 16.0],
                )),
            ),
        );
    let right_leg = PartDef::new(PartPose::offset(-3.064, -5.0, 0.0)).with_child(
        "right_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.048, 0.0, -0.9, -0.8727, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-1.856, -0.1, -1.09],
            [4.0, 5.0, 4.0],
            [0.0, 27.0],
        )),
    );
    let left_leg = PartDef::new(PartPose::offset(0.936, -5.0, 0.0)).with_child(
        "left_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.0, 0.0, 0.0, 0.7854, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-2.088, -0.1, -2.0],
            [4.0, 5.0, 4.0],
            [16.0, 27.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

/// `CopperGolemModel.createSittingPoseBodyLayer()` — the `SITTING` pose.
/// The body itself carries **two** cubes plus a nested `body_r1` (the seat
/// cushion, rotated a full `180°` about `Z`) — the only pose whose top-level
/// part is not a bare pivot.
#[must_use]
pub fn copper_golem_statue_sitting_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, -0.2));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let body_r1 = PartDef::new(PartPose::offset_and_rotation(
        0.0, -1.0, -4.325, 0.0, 0.0, -3.1416,
    ))
    .with_cube(CubeDef::new(
        [-4.0, -3.0, -2.2],
        [8.0, 6.0, 3.0],
        [3.0, 18.0],
    ));
    let right_arm = PartDef::new(PartPose::offset_and_rotation(
        -4.0, -5.6, -1.8, 0.4363, 0.0, 0.0,
    ))
    .with_child(
        "right_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, 0.0893, 0.1198, -1.0472, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-3.075, -0.9733, -1.9966],
            [3.0, 10.0, 4.0],
            [36.0, 16.0],
        )),
    );
    let left_arm = PartDef::new(PartPose::offset_and_rotation(
        4.0, -5.6, -1.7, 0.4363, 0.0, 0.0,
    ))
    .with_child(
        "left_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.0, -0.0015, -0.0808, -1.0472, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [0.075, -1.0443, -1.8997],
            [3.0, 10.0, 4.0],
            [50.0, 16.0],
        )),
    );
    let body = PartDef::new(PartPose::offset(0.0, -3.0, 2.325))
        .with_cube(CubeDef::new(
            [-3.0, -4.0, -4.525],
            [6.0, 1.0, 6.0],
            [3.0, 19.0],
        ))
        .with_cube(CubeDef::new(
            [-4.0, -3.0, -3.525],
            [8.0, 6.0, 6.0],
            [0.0, 15.0],
        ))
        .with_child("body_r1", body_r1)
        .with_child("head", head)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);
    let right_leg = PartDef::new(PartPose::offset(-2.1, -2.1, -2.075)).with_child(
        "right_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.05, -1.9, 1.075, -1.5708, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-2.0, 0.975, 0.0],
            [4.0, 5.0, 4.0],
            [0.0, 27.0],
        )),
    );
    let left_leg = PartDef::new(PartPose::offset(2.0, -2.0, -2.075)).with_child(
        "left_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.05, -2.0, 1.075, -1.5708, 0.0, 0.0,
        ))
        .with_cube(CubeDef::new(
            [-2.0, 0.975, 0.0],
            [4.0, 5.0, 4.0],
            [16.0, 27.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

/// `CopperGolemModel.createStarPoseBodyLayer()` — the `STAR` pose (arms and
/// legs both flung outward). The arms additionally carry an empty
/// `"rightItem"` pivot child (`CopperGolemModel`'s held-item anchor for a
/// *live* golem) with no cube — not baked here, since a placed statue's
/// renderer submits only the model, never a held item.
#[must_use]
pub fn copper_golem_statue_star_model() -> EntityModelDef {
    let head = PartDef::new(PartPose::offset(0.0, -6.0, 0.0));
    let head = copper_golem_head_cubes()
        .into_iter()
        .fold(head, PartDef::with_cube);
    let right_arm = PartDef::new(PartPose::offset(-4.0, -6.0, 0.0)).with_child(
        "right_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.0, 1.0, 0.0, 0.0, 0.0, 1.9199,
        ))
        .with_cube(CubeDef::new(
            [-1.5, -5.0, -2.0],
            [3.0, 10.0, 4.0],
            [36.0, 16.0],
        )),
    );
    let left_arm = PartDef::new(PartPose::offset(4.0, -6.0, 0.0)).with_child(
        "left_arm_r1",
        PartDef::new(PartPose::offset_and_rotation(
            -1.0, 1.0, 0.0, 0.0, 0.0, -1.9199,
        ))
        .with_cube(CubeDef::new(
            [-1.5, -5.0, -2.0],
            [3.0, 10.0, 4.0],
            [50.0, 16.0],
        )),
    );
    let body = PartDef::new(PartPose::offset(0.0, -5.0, 0.0))
        .with_cube(CubeDef::new([-4.0, -6.0, -3.0], [8.0, 6.0, 6.0], [0.0, 15.0]))
        .with_child("head", head)
        .with_child("right_arm", right_arm)
        .with_child("left_arm", left_arm);
    let right_leg = PartDef::new(PartPose::offset(-3.0, -5.0, 0.0)).with_child(
        "right_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            0.35, 2.0, 0.01, 0.0, 0.0, 0.2618,
        ))
        .with_cube(CubeDef::new(
            [-2.0, -2.5, -2.0],
            [4.0, 5.0, 4.0],
            [0.0, 27.0],
        )),
    );
    let left_leg = PartDef::new(PartPose::offset(1.0, -5.0, 0.0)).with_child(
        "left_leg_r1",
        PartDef::new(PartPose::offset_and_rotation(
            1.65, 2.0, 0.0, 0.0, 0.0, -0.2618,
        ))
        .with_cube(CubeDef::new(
            [-2.0, -2.5, -2.0],
            [4.0, 5.0, 4.0],
            [16.0, 27.0],
        )),
    );
    let root = PartDef::new(PartPose::ZERO)
        .with_child("body", body)
        .with_child("right_leg", right_leg)
        .with_child("left_leg", left_leg);
    EntityModelDef {
        texture_width: COPPER_GOLEM_SHEET.0,
        texture_height: COPPER_GOLEM_SHEET.1,
        root,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::bake_entity_parts;
    use crate::model::Direction;

    /// The parts, in bake (pre-order) order, with the root first.
    fn part_names(def: &EntityModelDef) -> Vec<String> {
        bake_entity_parts(def)
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    #[test]
    fn single_chest_has_the_three_vanilla_parts_in_order() {
        assert_eq!(
            part_names(&chest_single_model()),
            vec![
                String::new(),
                "bottom".to_string(),
                "lid".to_string(),
                "lock".to_string()
            ]
        );
    }

    /// `STANDING` is the plain, unnested rig: `body` holding
    /// `head`/`right_arm`/`left_arm`, then the two legs as `root`'s
    /// remaining children — seven parts, pre-order.
    #[test]
    fn copper_golem_standing_pose_has_the_plain_seven_part_tree() {
        assert_eq!(
            part_names(&copper_golem_statue_standing_model()),
            vec![
                String::new(),
                "body".to_string(),
                "head".to_string(),
                "right_arm".to_string(),
                "left_arm".to_string(),
                "right_leg".to_string(),
                "left_leg".to_string(),
            ]
        );
    }

    /// `RUNNING`/`SITTING` both nest every limb one level deeper (a bare
    /// pivot part holding one `_r1` child carrying the real box) — twelve
    /// parts, pre-order, `body_r1` appearing right after `body` in both.
    #[test]
    fn copper_golem_running_and_sitting_poses_nest_every_limb_one_level() {
        let expected = vec![
            String::new(),
            "body".to_string(),
            "body_r1".to_string(),
            "head".to_string(),
            "right_arm".to_string(),
            "right_arm_r1".to_string(),
            "left_arm".to_string(),
            "left_arm_r1".to_string(),
            "right_leg".to_string(),
            "right_leg_r1".to_string(),
            "left_leg".to_string(),
            "left_leg_r1".to_string(),
        ];
        assert_eq!(part_names(&copper_golem_statue_running_model()), expected);
        assert_eq!(part_names(&copper_golem_statue_sitting_model()), expected);
    }

    /// `STAR` nests only the arms and legs (each an `_r1` child), **not**
    /// `head` — the one pose whose `head` sits as a direct child of `body`,
    /// like `STANDING`'s, distinct from running/sitting's shape.
    #[test]
    fn copper_golem_star_pose_nests_arms_and_legs_but_not_head() {
        assert_eq!(
            part_names(&copper_golem_statue_star_model()),
            vec![
                String::new(),
                "body".to_string(),
                "head".to_string(),
                "right_arm".to_string(),
                "right_arm_r1".to_string(),
                "left_arm".to_string(),
                "left_arm_r1".to_string(),
                "right_leg".to_string(),
                "right_leg_r1".to_string(),
                "left_leg".to_string(),
                "left_leg_r1".to_string(),
            ]
        );
    }

    /// The head carries four boxes in every pose (the base, the snout, the
    /// antenna stalk and its tip) — a part with the wrong cube count is the
    /// cheapest sign a transcription dropped or duplicated a box.
    #[test]
    fn copper_golem_head_has_four_boxes_in_every_pose() {
        for (label, def) in [
            ("standing", copper_golem_statue_standing_model()),
            ("running", copper_golem_statue_running_model()),
            ("sitting", copper_golem_statue_sitting_model()),
            ("star", copper_golem_statue_star_model()),
        ] {
            let parts = bake_entity_parts(&def);
            let head = parts
                .iter()
                .find(|p| p.name == "head")
                .unwrap_or_else(|| panic!("{label}: no head part"));
            assert_eq!(
                head.quads.len(),
                4 * 6,
                "{label}: expected 4 boxes (6 quads each)"
            );
        }
    }

    /// The whole lid animation keys on these two names; a rename must fail here
    /// rather than silently freeze the lid (see the module doc).
    #[test]
    fn lid_and_lock_share_the_pivot_the_animation_rotates_about() {
        for def in [
            chest_single_model(),
            chest_double_left_model(),
            chest_double_right_model(),
        ] {
            let parts = bake_entity_parts(&def);
            let lid = parts.iter().find(|p| p.name == "lid").expect("lid");
            let lock = parts.iter().find(|p| p.name == "lock").expect("lock");
            assert_eq!(lid.rest, PartPose::offset(0.0, 9.0, 1.0));
            assert_eq!(lock.rest, lid.rest);
            // Siblings under the root, not nested — a nested lock would compose
            // the `offset(0, 9, 1)` pivot twice and sit 9 texels too high.
            assert_eq!(lid.parent, Some(0));
            assert_eq!(lock.parent, Some(0));
        }
    }

    /// A single chest occupies `1..15` texels on X/Z and `0..14` on Y. This is
    /// the check that would catch an entity-style Y-down transcription: a
    /// flipped model's Y extent would come out negative or offset by 1.501.
    #[test]
    fn single_chest_occupies_the_real_chest_volume_in_block_space() {
        let quads = crate::entity::bake_entity(&chest_single_model());
        assert!(!quads.is_empty());
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for q in &quads {
            for p in &q.positions {
                for a in 0..3 {
                    min[a] = min[a].min(p[a]);
                    max[a] = max[a].max(p[a]);
                }
            }
        }
        // bottom box x 1..15 texels => 0.0625..0.9375 blocks; the lock pokes out
        // to z = 15 texels (14 origin + 1 depth, plus the lid's z pivot of 1).
        assert!((min[0] - 1.0 / 16.0).abs() < 1e-5, "min x {}", min[0]);
        assert!((max[0] - 15.0 / 16.0).abs() < 1e-5, "max x {}", max[0]);
        // Y starts on the floor and the closed lid tops out at 14/16.
        assert!(min[1].abs() < 1e-5, "min y {}", min[1]);
        assert!((max[1] - 14.0 / 16.0).abs() < 1e-5, "max y {}", max[1]);
        // Nothing dips below the floor or above one block: this is the
        // assertion an entity-space (Y-flipped, -1.501) placement fails.
        assert!(min[1] >= -1e-5 && max[1] <= 1.0 + 1e-5);
    }

    #[test]
    fn double_halves_omit_exactly_the_seam_face() {
        let left = crate::entity::bake_entity(&chest_double_left_model());
        assert!(
            !left.iter().any(|q| q.direction == Direction::West),
            "the left half must omit its West seam"
        );
        assert!(left.iter().any(|q| q.direction == Direction::East));

        let right = crate::entity::bake_entity(&chest_double_right_model());
        assert!(
            !right.iter().any(|q| q.direction == Direction::East),
            "the right half must omit its East seam"
        );
        assert!(right.iter().any(|q| q.direction == Direction::West));
    }

    /// Together the two halves span two blocks on X: left `0..15`, right
    /// `1..16` (each in its own block's local frame).
    #[test]
    fn double_halves_are_fifteen_texels_wide_at_opposite_ends() {
        let span = |def: &EntityModelDef| {
            let quads = crate::entity::bake_entity(def);
            let mut min = f32::MAX;
            let mut max = f32::MIN;
            for q in &quads {
                for p in &q.positions {
                    min = min.min(p[0]);
                    max = max.max(p[0]);
                }
            }
            (min * 16.0, max * 16.0)
        };
        let (lmin, lmax) = span(&chest_double_left_model());
        assert!((lmin - 0.0).abs() < 1e-4, "left min {lmin}");
        assert!((lmax - 15.0).abs() < 1e-4, "left max {lmax}");
        let (rmin, rmax) = span(&chest_double_right_model());
        assert!((rmin - 1.0).abs() < 1e-4, "right min {rmin}");
        assert!((rmax - 16.0).abs() < 1e-4, "right max {rmax}");
    }

    #[test]
    fn every_entry_builds_and_resolves_by_name() {
        for entry in BLOCK_ENTITY_MODELS {
            let def = (entry.build)();
            // 64 wide on every chest/skull canvas; the bell sheet is 32×32,
            // and the decorated pot's side quads are the first to be
            // narrower still (16×16 — see `decorated_pot_side_part`'s doc).
            assert!(
                def.texture_width == 16 || def.texture_width == 32 || def.texture_width == 64,
                "{}: unexpected canvas width {}",
                entry.name,
                def.texture_width
            );
            assert!(
                def.texture_height == 16 || def.texture_height == 32 || def.texture_height == 64,
                "{}: unexpected canvas height {}",
                entry.name,
                def.texture_height
            );
            assert!(!crate::entity::bake_entity(&def).is_empty());
            assert_eq!(block_entity_model(entry.name).map(|e| e.name), Some(entry.name));
        }
        assert!(block_entity_model("no_such_model").is_none());
    }

    /// The bell's own part hierarchy, in bake (pre-order) order:
    /// `bell_base` is a child of `bell_body`, not a sibling under root — see
    /// `bell_model`'s doc for why the nesting itself (not just the pivot
    /// arithmetic) is load-bearing.
    #[test]
    fn bell_base_is_nested_inside_bell_body_not_a_sibling() {
        let def = bell_model();
        let parts = bake_entity_parts(&def);
        assert_eq!(
            part_names(&def),
            vec![String::new(), "bell_body".to_string(), "bell_base".to_string()]
        );
        let body = parts.iter().position(|p| p.name == "bell_body").unwrap();
        let base = parts.iter().find(|p| p.name == "bell_base").unwrap();
        assert_eq!(
            base.parent,
            Some(body),
            "bell_base must be a child of bell_body, not the root"
        );
    }

    /// The rim (`bell_base`) sits just below the tapered body (`bell_body`) in
    /// world/block space, touching at the seam — the physical shape of a
    /// bell's flared bottom skirt. This is the assertion that would catch the
    /// child's local pose sign flipped (which would send the rim flying up
    /// through the body instead of hanging just under it) or the two parts
    /// accidentally swapped to siblings (which would double-apply
    /// `bell_body`'s own pivot and put the rim at the wrong height entirely).
    #[test]
    fn bell_base_sits_just_below_bell_body() {
        let def = bell_model();
        let quads = crate::entity::bake_entity(&def);
        assert!(!quads.is_empty());

        // `bake_entity_parts` deliberately leaves the pivot chain for the
        // caller to apply (see its own doc — that is what lets an animator
        // move a joint), so this rebuilds the same chain
        // `BlockEntityMesh::part_transforms` does: `Affine::of_pose` per
        // part, composed through the parent index.
        let baked = bake_entity_parts(&def);
        let mut chain: Vec<crate::entity::Affine> = Vec::with_capacity(baked.len());
        for part in &baked {
            let local = crate::entity::Affine::of_pose(&part.rest);
            let world = match part.parent {
                Some(p) => chain[p].compose(&local),
                None => local,
            };
            chain.push(world);
        }

        let mut body_min_y = f32::MAX;
        let mut base_max_y = f32::MIN;
        for (i, part) in baked.iter().enumerate() {
            for quad in &part.quads {
                for p in &quad.positions {
                    let world = chain[i].apply(*p);
                    if part.name == "bell_body" {
                        body_min_y = body_min_y.min(world[1]);
                    } else if part.name == "bell_base" {
                        base_max_y = base_max_y.max(world[1]);
                    }
                }
            }
        }
        // body spans 6..13 texels (5..11? see doc — y range specifically):
        // pivot 12 + local -6..1 = 6..13 texels = 0.375..0.8125 blocks.
        assert!(
            (body_min_y - 6.0 / 16.0).abs() < 1e-4,
            "bell_body min y {body_min_y}"
        );
        // base spans 4..6 texels = 0.25..0.375 blocks, touching the body's
        // bottom exactly (6/16).
        assert!(
            (base_max_y - 6.0 / 16.0).abs() < 1e-4,
            "bell_base max y {base_max_y} should touch bell_body's bottom"
        );
        assert!(
            base_max_y <= body_min_y + 1e-5,
            "the rim must not poke up into the body: base_max {base_max_y} body_min {body_min_y}"
        );
    }

    /// The skull box is identical on both canvases — only the declared sheet
    /// size differs, which is exactly the thing a copy-paste between the two
    /// builders could silently drop.
    #[test]
    fn skull_mob_and_humanoid_share_the_same_box_on_different_canvases() {
        let mob = skull_mob_model();
        let humanoid = skull_humanoid_model();
        assert_eq!(mob.texture_width, 64);
        assert_eq!(mob.texture_height, 32);
        assert_eq!(humanoid.texture_width, 64);
        assert_eq!(humanoid.texture_height, 64);
        assert_eq!(mob.root, PartDef::new(PartPose::ZERO).with_child("head", skull_head_part()));
        assert_eq!(
            mob.root.children[0].1.cubes,
            humanoid.root.children[0].1.cubes,
            "the head box itself must not differ between the two canvases"
        );
    }

    /// A skull is authored Y-down like a mob head (see `skull_head_part`'s
    /// doc): the box dips *below* its `PartPose::ZERO` pivot, which is the
    /// opposite sign from every chest box (which sits *above* the floor at
    /// its own zero pivot). This is the one assertion that would catch the
    /// box being accidentally re-authored block-space-up like a chest.
    #[test]
    fn skull_head_box_extends_below_its_pivot_like_a_mob_head() {
        let quads = crate::entity::bake_entity(&skull_mob_model());
        assert!(!quads.is_empty());
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        for q in &quads {
            for p in &q.positions {
                min_y = min_y.min(p[1]);
                max_y = max_y.max(p[1]);
            }
        }
        assert!(min_y < 0.0, "min y {min_y} should dip below the pivot");
        assert!(max_y <= 0.0 + 1e-5, "max y {max_y} should not rise above the pivot");
    }

    // --- banner ---------------------------------------------------------

    /// `pole` and `bar` are siblings under root, both at `PartPose::ZERO` —
    /// `BannerModel.createBodyLayer` never nests one under the other, and
    /// both boxes carry their own pivot-relative origin instead.
    #[test]
    fn banner_body_has_pole_and_bar_as_zero_pose_siblings() {
        let def = banner_body_model();
        assert_eq!(
            part_names(&def),
            vec![String::new(), "pole".to_string(), "bar".to_string()]
        );
        let parts = bake_entity_parts(&def);
        let pole = parts.iter().find(|p| p.name == "pole").unwrap();
        let bar = parts.iter().find(|p| p.name == "bar").unwrap();
        assert_eq!(pole.rest, PartPose::ZERO);
        assert_eq!(bar.rest, PartPose::ZERO);
        assert_eq!(pole.parent, Some(0));
        assert_eq!(bar.parent, Some(0));
    }

    /// The flag is one part, offset by `(0, -44, 0)` texels —
    /// `BannerFlagModel.createFlagLayer(true)`'s `PartPose.offset`. This is
    /// the pivot [`crate::entity::PartPose`]'s own `x_rot` overrides to swing
    /// the sway; a wrong offset here would put the sway pivot at the wrong
    /// height even with the angle formula exactly right.
    #[test]
    fn banner_flag_is_one_part_offset_by_the_bar_height() {
        let def = banner_flag_model();
        assert_eq!(part_names(&def), vec![String::new(), "flag".to_string()]);
        let parts = bake_entity_parts(&def);
        let flag = parts.iter().find(|p| p.name == "flag").unwrap();
        assert_eq!(flag.rest, PartPose::offset(0.0, -44.0, 0.0));
        assert_eq!(flag.parent, Some(0));
    }

    /// Both banner layers are declared on a 64×64 sheet — the same canvas
    /// size as the chest layers, and unlike the bell's narrower 32×32.
    #[test]
    fn banner_layers_use_the_sixty_four_sheet() {
        assert_eq!(banner_body_model().texture_width, BANNER_SHEET.0);
        assert_eq!(banner_body_model().texture_height, BANNER_SHEET.1);
        assert_eq!(banner_flag_model().texture_width, BANNER_SHEET.0);
        assert_eq!(banner_flag_model().texture_height, BANNER_SHEET.1);
    }

    /// The pole, bar and flag stack contiguously along Y in model-texel
    /// space, tallest (most negative — this is Y-down entity space, unlike
    /// the block-space-up chest/bell) at the crossbar, exactly the physical
    /// shape a banner has to have: a vertical staff, a crossbar at its top,
    /// and cloth hanging from the crossbar that stops short of the ground.
    /// Measured through the real baked quads, not restated from the literal
    /// `addBox` arguments a copy-paste error could also get wrong.
    #[test]
    fn banner_body_and_flag_stack_contiguously_along_the_staff() {
        let y_span = |def: &EntityModelDef, part: &str| -> (f32, f32) {
            let baked = bake_entity_parts(def);
            let index = baked.iter().position(|p| p.name == part).unwrap();
            let mut chain: Vec<crate::entity::Affine> = Vec::with_capacity(baked.len());
            for p in &baked {
                let local = crate::entity::Affine::of_pose(&p.rest);
                let world = match p.parent {
                    Some(parent) => chain[parent].compose(&local),
                    None => local,
                };
                chain.push(world);
            }
            let mut min = f32::MAX;
            let mut max = f32::MIN;
            for quad in &baked[index].quads {
                for pos in &quad.positions {
                    let world = chain[index].apply(*pos);
                    min = min.min(world[1]);
                    max = max.max(world[1]);
                }
            }
            (min * 16.0, max * 16.0)
        };

        let body = banner_body_model();
        let (pole_min, pole_max) = y_span(&body, "pole");
        let (bar_min, bar_max) = y_span(&body, "bar");
        let (flag_min, flag_max) = y_span(&banner_flag_model(), "flag");

        assert!((pole_min - -42.0).abs() < 1e-4, "pole min {pole_min}");
        assert!((pole_max - 0.0).abs() < 1e-4, "pole max {pole_max}");
        // The crossbar sits directly atop the pole (both in texels).
        assert!(
            (bar_max - pole_min).abs() < 1e-4,
            "bar max {bar_max} should touch pole min {pole_min}"
        );
        assert!((bar_min - -44.0).abs() < 1e-4, "bar min {bar_min}");
        // The flag hangs from the crossbar's own height and stops 4 texels
        // short of the ground (`BANNER_HEIGHT = 40`, `-44 + 40 = -4`).
        assert!(
            (flag_min - bar_min).abs() < 1e-4,
            "flag min {flag_min} should start at the crossbar {bar_min}"
        );
        assert!((flag_max - -4.0).abs() < 1e-4, "flag max {flag_max}");
    }

    /// The four side models' rest poses, transcribed straight from
    /// `DecoratedPotRenderer.createSidesLayer()`'s four `offsetAndRotation`
    /// calls. Checked pairwise-distinct too — a transposition between any two
    /// (say, swapping `left` and `right`) would still leave every individual
    /// pose "a real `PartPose`", which is why the pairwise check is separate
    /// from the four `assert_eq!`s above it.
    #[test]
    fn decorated_pot_side_poses_match_the_jars_four_offsets_and_rotations() {
        use std::f32::consts::{FRAC_PI_2, PI};
        let side_pose = |def: &EntityModelDef| -> PartPose {
            bake_entity_parts(def)
                .into_iter()
                .find(|p| p.name == "side")
                .expect("a `side` part")
                .rest
        };
        let front = side_pose(&decorated_pot_side_front_model());
        let back = side_pose(&decorated_pot_side_back_model());
        let left = side_pose(&decorated_pot_side_left_model());
        let right = side_pose(&decorated_pot_side_right_model());

        assert_eq!(
            front,
            PartPose::offset_and_rotation(1.0, 16.0, 15.0, PI, 0.0, 0.0)
        );
        assert_eq!(
            back,
            PartPose::offset_and_rotation(15.0, 16.0, 1.0, 0.0, 0.0, PI)
        );
        assert_eq!(
            left,
            PartPose::offset_and_rotation(1.0, 16.0, 1.0, 0.0, -FRAC_PI_2, PI)
        );
        assert_eq!(
            right,
            PartPose::offset_and_rotation(15.0, 16.0, 15.0, 0.0, FRAC_PI_2, PI)
        );

        let poses = [front, back, left, right];
        for i in 0..poses.len() {
            for j in (i + 1)..poses.len() {
                assert_ne!(poses[i], poses[j], "sides {i} and {j} share a pose");
            }
        }
    }

    /// Each side model must emit exactly one visible face — the `North` one,
    /// `EnumSet.of(Direction.NORTH)` in the jar — never the other five
    /// (coincident or degenerate on a zero-depth box). A mesher that dropped
    /// the `visible_faces` restriction would draw six overlapping/degenerate
    /// faces per side and still pass every geometry-existence check that only
    /// asks "did this bake any quads at all".
    #[test]
    fn decorated_pot_side_models_emit_exactly_one_quad_the_north_face_only() {
        for (name, def) in [
            ("front", decorated_pot_side_front_model()),
            ("back", decorated_pot_side_back_model()),
            ("left", decorated_pot_side_left_model()),
            ("right", decorated_pot_side_right_model()),
        ] {
            let quads = crate::entity::bake_entity(&def);
            assert_eq!(quads.len(), 1, "{name}: expected exactly one visible face");
            assert_eq!(
                quads[0].direction,
                Direction::North,
                "{name}: the one visible face must be North"
            );
        }
    }

    /// The base is one model with three named parts (`neck`/`top`/`bottom`,
    /// plus the empty-named geometry-free root `bake_entity_parts` always
    /// emits), each carrying real geometry — mirroring
    /// `single_chest_has_the_three_vanilla_parts_in_order`'s shape for a
    /// model this module has not tested before.
    #[test]
    fn decorated_pot_base_has_the_three_named_parts_and_real_geometry() {
        assert_eq!(
            part_names(&decorated_pot_base_model()),
            vec![
                String::new(),
                "neck".to_string(),
                "top".to_string(),
                "bottom".to_string(),
            ]
        );
        let quads = crate::entity::bake_entity(&decorated_pot_base_model());
        assert!(!quads.is_empty(), "the base baked no quads");
    }

    /// The four conduit sheets, by their own vanilla `LayerDefinition.create`
    /// calls (`ConduitRenderer.createEyeLayer/createWindLayer/createShellLayer/
    /// createCageLayer`) — a magnitude check, not a sign check: a builder that
    /// halved or doubled a dimension would still pass
    /// `every_entry_builds_and_resolves_by_name`'s coarse `{16,32,64}` gate.
    #[test]
    fn conduit_sheets_match_their_own_layer_definitions() {
        let eye = conduit_eye_model();
        assert_eq!((eye.texture_width, eye.texture_height), (16, 16));
        let wind = conduit_wind_model();
        assert_eq!((wind.texture_width, wind.texture_height), (64, 32));
        let shell = conduit_shell_model();
        assert_eq!((shell.texture_width, shell.texture_height), (32, 16));
        let cage = conduit_cage_model();
        assert_eq!((cage.texture_width, cage.texture_height), (32, 16));
    }

    /// The active shell ("cage") is a genuinely bigger box than the inactive
    /// one — 8×8×8 against 6×6×6 — not a re-skin of the same geometry. Measured
    /// by the baked span rather than restating the texel constants, the same
    /// discipline [`double_halves_are_fifteen_texels_wide_at_opposite_ends`]
    /// already uses for the chest halves.
    #[test]
    fn conduit_cage_is_a_larger_cube_than_the_inactive_shell() {
        let span_x = |def: &EntityModelDef| {
            let quads = crate::entity::bake_entity(def);
            let mut min = f32::MAX;
            let mut max = f32::MIN;
            for q in &quads {
                for p in &q.positions {
                    min = min.min(p[0]);
                    max = max.max(p[0]);
                }
            }
            (max - min) * 16.0
        };
        let shell_width = span_x(&conduit_shell_model());
        let cage_width = span_x(&conduit_cage_model());
        assert!(
            (shell_width - 6.0).abs() < 1e-4,
            "inactive shell width {shell_width}, expected 6"
        );
        assert!(
            (cage_width - 8.0).abs() < 1e-4,
            "cage width {cage_width}, expected 8"
        );
        assert!(
            cage_width > shell_width,
            "the active cage ({cage_width}) must be wider than the inactive shell ({shell_width})"
        );
    }

    /// The eye is a near-planar box (zero authored depth, grown `0.01` on every
    /// axis) — real, thin geometry, not a literal zero-volume box a mesher
    /// could legally cull to nothing. Measured against the deformation amount
    /// rather than merely asserting `> 0`, so a builder that dropped `.grown`
    /// (leaving a true zero-depth box, which some bakers special-case away)
    /// cannot pass by accident.
    #[test]
    fn conduit_eye_is_a_thin_grown_box_not_a_true_zero_depth_one() {
        let quads = crate::entity::bake_entity(&conduit_eye_model());
        assert!(!quads.is_empty(), "the eye baked no quads");
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for q in &quads {
            for p in &q.positions {
                min = min.min(p[2]);
                max = max.max(p[2]);
            }
        }
        let depth = (max - min) * 16.0;
        assert!(
            (depth - 0.02).abs() < 1e-4,
            "eye depth {depth} texels, expected 0.02 (2 × 0.01 grow)"
        );
    }
}
