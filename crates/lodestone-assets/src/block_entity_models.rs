//! Hand-ported *block-entity* model geometry for the 26.2 family — the cuboid
//! rigs that the real client's per-block-entity draw code renders and that no
//! block model covers.
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
//! — **zero elements**. Every visible triangle of a chest comes from the
//! dedicated chest draw code and its own cuboid rig, so a client with no
//! block-entity renderer draws a chest as a *hole in the world*, not as a
//! slightly-wrong box. That is the single highest-value thing in this module
//! and the reason chest is first.
//!
//! The converse is just as important and much easier to get wrong from memory:
//! **a 26.2 sign is a real block model.** `assets/minecraft/blockstates/oak_sign.json`
//! maps every one of the 16 `rotation` values to a `block/oak_sign_rot_N` model
//! with genuine geometry, and the real sign's block-entity draw code declares
//! **no model whatsoever** — only text transformations. So there is deliberately
//! no `sign_model()` here: porting one would draw a second board inside the one
//! the block model already meshes. Sign *text* is a text pass, not geometry, and
//! lives in `lodestone-shell`'s `gpu/block_entity_text.rs`.
//!
//! # The geometry is vanilla's, exactly
//!
//! Transcribed from the real client's chest cuboid-rig builder. Texel offsets,
//! box extents, pivots and the 64×64 sheet size are the exact vanilla values;
//! nothing here is rounded or "close enough". A double chest's two halves are
//! genuinely separate builders rather than one shared body posed twice — see
//! [`BLOCK_ENTITY_MODELS`]'s own doc for why that matters.
//!
//! # Not entity models, despite sharing every primitive
//!
//! These reuse [`CubeDef`]/[`PartDef`]/[`EntityModelDef`] because the *bake* is
//! identical — vanilla's own cuboid-part primitive does not know or care
//! whether its owner is a mob or a chest. What differs is **placement**, and
//! that difference is load-bearing enough to keep the two corpora apart:
//!
//! * An entity is placed by `entity_model_matrix`, which flips Y
//!   (`scale(-1, -1, 1)`) and lifts by `MODEL_FEET_OFFSET = 1.501` because mob
//!   model space is Y-**down**.
//! * A block entity is **not flipped and not lifted**. The real chest draw
//!   code's whole prologue is one rotation about the vertical axis, by the
//!   negated yaw the block's facing implies, pivoting about the block's own
//!   horizontal centre — and the model's own texels then land directly in
//!   block space: the chest `bottom` box spans y `0..10` texels, i.e.
//!   `0..0.625` blocks off the floor, and the `lid` pivot at y `9` puts the
//!   closed lid's top at `14/16` — the real chest height. Reusing the entity
//!   placement matrix would bury a chest 1.5 blocks into the floor, upside
//!   down.
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
//!   omits `East` (index 4) and the left half omits `West` (index 2), matching
//!   vanilla's own "every direction except this one" face set for each half.
//!   Getting the index wrong deletes the wrong face, and the result is a chest
//!   with a hole in its *front* that still passes any "does a chest draw" gate.

use crate::entity::{CubeDef, EntityModelDef, PartDef, PartPose};

mod copper_golem;
mod banners;
mod bells;
mod books;
mod chests;
mod conduit;
mod decorated_pot;
mod shields;
mod shulkers;
mod skulls;

pub use banners::*;
pub use bells::*;
pub use books::*;
pub use chests::*;
pub use conduit::*;
pub use copper_golem::{
    copper_golem_statue_running_model, copper_golem_statue_sitting_model,
    copper_golem_statue_standing_model, copper_golem_statue_star_model,
};
pub use decorated_pot::*;
pub use shields::*;
pub use shulkers::*;
pub use skulls::*;
pub(super) use skulls::skull_head_part;

/// The chest sheet is 64×64 (the declared canvas size in all three real
/// chest body-layer builders). Asserted against the real `client.jar` PNGs by
/// `lodestone-assets/tests/real_jar.rs`.
const CHEST_SHEET: (u32, u32) = (64, 64);

/// The bell sheet is 32×32 (the real bell body-layer builder's declared
/// canvas) — smaller than every chest/skull canvas so far, which is why
/// [`bell_model`]'s own test does not reuse [`CHEST_SHEET`].
const BELL_SHEET: (u32, u32) = (32, 32);

/// The banner sheet is 64×64 (the real banner body and flag builders both
/// declare this one canvas size, same as [`CHEST_SHEET`]).
const BANNER_SHEET: (u32, u32) = (64, 64);

/// The shulker sheet is 64×64 (the real box-layer builder's declared
/// canvas). Same size as [`CHEST_SHEET`] and [`BANNER_SHEET`], named
/// separately so a jar change is one edit per family.
const SHULKER_SHEET: (u32, u32) = (64, 64);

/// The shield sheet is 64×64 (the real shield layer builder's declared
/// canvas). Same canvas size as
/// [`CHEST_SHEET`]/[`BANNER_SHEET`]/[`SHULKER_SHEET`], named separately for
/// the same reason those are: a jar change to any one family is one constant
/// to edit.
const SHIELD_SHEET: (u32, u32) = (64, 64);

/// The book sheet is 64×**32** (the real book body-layer builder's declared
/// canvas) — the only non-square canvas in this module, so a builder that
/// reused [`CHEST_SHEET`] would halve every `v` coordinate and draw the page
/// texture at the wrong scale rather than not at all.
const BOOK_SHEET: (u32, u32) = (64, 32);

/// The conduit eye sheet, 16×16 — the real eye-layer builder's declared
/// canvas.
const CONDUIT_EYE_SHEET: (u32, u32) = (16, 16);

/// The conduit wind sheet, 64×32 — the real wind-layer builder's declared
/// canvas. Same canvas size as [`BOOK_SHEET`], named separately so a jar
/// change to either is one edit.
const CONDUIT_WIND_SHEET: (u32, u32) = (64, 32);

/// The conduit's inactive-shell sheet, 32×16 — the real shell-layer
/// builder's declared canvas.
const CONDUIT_SHELL_SHEET: (u32, u32) = (32, 16);

/// The conduit's active-shell ("cage") sheet, 32×16 — the real cage-layer
/// builder's declared canvas. Same dimensions as [`CONDUIT_SHELL_SHEET`] but
/// a genuinely different sheet (`entity/conduit/cage`, not
/// `entity/conduit/base`) and a genuinely different box (8×8×8, not 6×6×6) —
/// named separately rather than reused.
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
/// skull/head canvases — see [`skull_mob_model`]'s doc for why there are two —
/// plus the dragon and piglin heads, which are the two skull types that share
/// no geometry with that box at all.
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
        name: "skull_dragon",
        texture: "entity/enderdragon/dragon",
        build: dragon_head_model,
    },
    BlockEntityModelEntry {
        name: "skull_piglin",
        texture: "entity/piglin/piglin",
        build: piglin_head_model,
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
        // The default/no-pattern sheet — the real shield draw code picks
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











/// `[true; 6]` with one `entity::FACE_ORDER` index cleared.
fn hide_face(index: usize) -> [bool; 6] {
    let mut faces = [true; 6];
    faces[index] = false;
    faces
}

/// `[false; 6]` with one `entity::FACE_ORDER` index set — the inverse of
/// [`hide_face`], for a box only one face of which is ever visible (the
/// decorated pot's side quads, restricted to just the north face in the jar).
fn only_face(index: usize) -> [bool; 6] {
    let mut faces = [false; 6];
    faces[index] = true;
    faces
}

/// `entity::FACE_ORDER` index of the `North` face — see the module doc on why
/// this is not `Direction as usize`.
const FACE_NORTH: usize = 3;

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

    /// The dragon head is the corpus's only `PartPose.scaled` rig and its only
    /// use of `mirror`, and both are silent when wrong: dropping the `0.75`
    /// gives a head a third too large that still looks like a dragon, and
    /// dropping the mirror flags gives two boxes whose texels run backwards.
    #[test]
    fn dragon_head_is_a_scaled_six_cube_head_with_a_jaw_child() {
        let def = dragon_head_model();
        assert_eq!(def.texture_width, 256);
        assert_eq!(def.texture_height, 256);
        assert_eq!(part_names(&def), vec![String::new(), "head".to_string(), "jaw".to_string()]);

        let head = &def.root.children[0].1;
        assert_eq!(head.pose.scale, [0.75, 0.75, 0.75]);
        assert!((head.pose.y - -7.986_666).abs() < 1e-5, "{}", head.pose.y);
        assert_eq!(head.cubes.len(), 6);
        // Exactly the third and fourth are mirrored — the left-hand scale and
        // nostril. Asserted as the whole pattern, not "some cube is mirrored".
        let mirrored: Vec<bool> = head.cubes.iter().map(|c| c.mirror).collect();
        assert_eq!(mirrored, vec![false, false, true, true, false, false]);
        // The mirrored pair and its unmirrored partner share texels and differ
        // only in sign of X — a transposition of the two `scale` boxes would
        // otherwise round-trip unnoticed.
        assert_eq!(head.cubes[2].tex_offset, head.cubes[4].tex_offset);
        assert!((head.cubes[2].origin[0] + 5.0).abs() < 1e-6);
        assert!((head.cubes[4].origin[0] - 3.0).abs() < 1e-6);

        let jaw = &head.children[0].1;
        assert_eq!(jaw.cubes.len(), 1);
        assert_eq!(jaw.cubes[0].tex_offset, [176.0, 65.0]);
        assert_eq!(jaw.pose.y, 4.0);
        assert_eq!(jaw.pose.z, -8.0);
    }

    /// A piglin head is **ten** texels wide, so nothing about it could have
    /// been recovered by pointing the shared 8×8×8 skull box at the piglin
    /// sheet — which is the shortcut this model exists to rule out.
    #[test]
    fn piglin_head_is_ten_texels_wide_with_two_asymmetric_ears() {
        let def = piglin_head_model();
        assert_eq!((def.texture_width, def.texture_height), (64, 64));
        assert_eq!(
            part_names(&def),
            vec![
                String::new(),
                "head".to_string(),
                "left_ear".to_string(),
                "right_ear".to_string()
            ]
        );
        let quads = crate::entity::bake_entity(&def);
        let (mut min, mut max) = (f32::MAX, f32::MIN);
        for q in &quads {
            for p in &q.positions {
                min = min.min(p[0]);
                max = max.max(p[0]);
            }
        }
        // The skull box spans 8 texels; the piglin's own snouted skull spans
        // 10, and the ears push the silhouette wider still.
        assert!(
            (max - min) * 16.0 > 10.0,
            "piglin head spans {} texels, want more than the 10-wide skull box",
            (max - min) * 16.0
        );

        let head = &def.root.children[0].1;
        let (left, right) = (&head.children[0].1, &head.children[1].1);
        // Pivots and sheets are pairwise distinct, so a left/right
        // transposition cannot survive: same `z_rot` magnitude, opposite sign,
        // opposite pivot, different texel offsets.
        assert!((left.pose.x - 4.5).abs() < 1e-6, "{}", left.pose.x);
        assert!((right.pose.x + 4.5).abs() < 1e-6, "{}", right.pose.x);
        assert!((left.pose.z_rot + std::f32::consts::FRAC_PI_6).abs() < 1e-6);
        assert!((right.pose.z_rot - std::f32::consts::FRAC_PI_6).abs() < 1e-6);
        assert_eq!(left.cubes[0].tex_offset, [51.0, 6.0]);
        assert_eq!(right.cubes[0].tex_offset, [39.0, 6.0]);
    }

    #[test]
    fn every_entry_builds_and_resolves_by_name() {
        for entry in BLOCK_ENTITY_MODELS {
            let def = (entry.build)();
            // The canvas used to be checked against a literal 16/32/64
            // allow-list, which is a transcription of the corpus rather than a
            // rule about it — the dragon head's real 256×256 sheet failed it
            // on arrival. A power of two in this range is the rule that
            // actually holds, and it is all this gate can honestly claim: the
            // stronger "every cube's unwrap lands inside the canvas" check is
            // false for `decorated_pot_base`, which deliberately unwraps from
            // a negative texel offset.
            for (axis, size) in [("width", def.texture_width), ("height", def.texture_height)] {
                assert!(
                    size.is_power_of_two() && (16..=256).contains(&size),
                    "{}: canvas {axis} {size} is not a power of two in 16..=256",
                    entry.name
                );
            }
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
    /// vanilla's own banner-flag-model standing flag-layer construction's own
    /// offset pose. This is
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
    /// vanilla's own decorated-pot-renderer sides-layer construction's four
    /// offset-and-rotation
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
