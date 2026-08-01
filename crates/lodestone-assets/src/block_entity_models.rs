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
/// Three entries, all chest: vanilla genuinely has three separate chest
/// *layers*, not one layer posed three ways — a double chest's halves are
/// 15 texels wide instead of 14 and each omits the face that meets its partner,
/// so `left`/`right` cannot be derived from `single` by a transform.
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

/// `[true; 6]` with one `entity::FACE_ORDER` index cleared.
fn hide_face(index: usize) -> [bool; 6] {
    let mut faces = [true; 6];
    faces[index] = false;
    faces
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
            assert_eq!(def.texture_width, 64);
            assert_eq!(def.texture_height, 64);
            assert!(!crate::entity::bake_entity(&def).is_empty());
            assert_eq!(block_entity_model(entry.name).map(|e| e.name), Some(entry.name));
        }
        assert!(block_entity_model("no_such_model").is_none());
    }
}
