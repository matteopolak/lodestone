//! Where a held `minecraft:special` item actually lands in camera space, as a
//! **magnitude** assertion whose expected value is derived from the 26.2 jar and
//! the decompiled 26.2 source — never from this pipeline's own output.
//!
//! # Why this gate exists
//!
//! `first_person_head_hand_pixels.rs` proves a held player head *draws*. It
//! asserts nothing about **where**, so the whole family of "right rig, right
//! sheet, wrong pose" defects — a dropped node transformation, a missing or
//! doubled `-0.5` centring, the third-person slot read in place of the
//! first-person one — is invisible to it. Each of those moves the head by
//! exactly half a block or more, which at the hand's fixed 70° FOV is the
//! difference between a head you can see and one entirely below the viewport.
//!
//! # The chain, and where each term comes from
//!
//! ```text
//! placement = T(arm offset) · display_matrix(slot) · node_transformation
//! ```
//!
//! * **arm offset** `(0.56, -0.52, -0.72)` — vanilla's item-arm transform function,
//!   with `inverseArmHeight = 0` (a fully-equipped hand) and `attackAnim = 0`
//!   (at rest the whole attack chain cancels to the identity).
//! * **display slot** — vanilla's display-transforms accessor for `FIRST_PERSON_RIGHT_HAND` on the
//!   `base` model's merged transforms. `assets/minecraft/models/item/template_skull.json`
//!   **declares no `firstperson_righthand`**, so vanilla answers with the
//!   no-transform singleton, whose `apply` is the bare
//!   `translate(-0.5, -0.5, -0.5)` centring and nothing else.
//! * **node transformation** — `assets/minecraft/items/player_head.json`'s own
//!   `"transformation"` on the `minecraft:special` node:
//!   `translation [0.5, 0, 0.5]`, `left_rotation [1, 0, 0, -0]`
//!   (a JOML `(x, y, z, w)` quaternion, i.e. 180° about X), unit scale, identity
//!   right rotation. Vanilla's unbaked special-model-wrapper bake function composes it *under* the
//!   display transform via its transformation-record compose function (`Transformation.compose(parent, this.transformation)`),
//!   and its model-bakery class seeds the root with the identity.
//! * **the mesh** — vanilla's skull model's head-model-creation function's `addBox(-4, -8, -4, 8, 8, 8)`,
//!   divided by 16 like every `ModelPart`, so `y ∈ [-0.5, 0]`: authored **Y-down**,
//!   which is the whole reason the node transformation flips it.
//!
//! Neither vanilla's skull special-renderer's submit function nor its player-head
//! special-renderer's submit function
//! adds a placement of its own — both call vanilla's skull block-renderer's submit function
//! directly. The ground/wall `scale(-1, -1, 1)` belongs to the *block-entity*
//! path (vanilla's skull block-renderer's submit function) and must not appear here.
//!
//! # What the numbers say, and what they do not
//!
//! A held skull lands **low**: `y ∈ [-1.02, -0.52]` against a held chest's
//! `[-0.72, -0.37]`. That is vanilla's own answer, not a defect here —
//! `item/template_skull` is one of only **two** of the 35 `minecraft:special`
//! base models in the 26.2 jar with no `firstperson_righthand` (the other,
//! `item/dragon_head`, inherits from it), and it compensates the skull's
//! bottom-half offset in every slot it *does* declare: `+3` texels for `gui`,
//! `ground` and `thirdperson_righthand`, `+4` for `fixed`, `+8` with a `2×`
//! scale for `on_shelf`. First person simply has no such entry. Anyone tempted
//! to raise the head here should change the number in this gate first, with an
//! outside source for the new one.

use glam::{Mat4, Vec3};
use lodestone_assets::item_model::ItemNodeTransform;
use lodestone_assets::{DisplaySlot, DisplayTransform, DisplayTransforms};
use lodestone_render::block_entity::{BlockEntityModelSet, CHEST_SINGLE, SKULL_HUMANOID, SKULL_MOB};
use lodestone_render::compose_special_node_transform;
use lodestone_render::entity::{Arm, first_person_item_matrix, hand_transform};

/// `assets/minecraft/items/player_head.json`'s `"transformation"`, transcribed
/// from the jar. Every skull-family item carries the identical object.
fn player_head_node_transform() -> ItemNodeTransform {
    ItemNodeTransform {
        translation: [0.5, 0.0, 0.5],
        left_rotation: [1.0, 0.0, 0.0, -0.0],
        scale: [1.0, 1.0, 1.0],
        right_rotation: [0.0, 0.0, 0.0, 1.0],
    }
}

/// `assets/minecraft/models/item/template_skull.json`'s `display` map,
/// transcribed from the jar — **all five slots it declares**, so the absence of
/// `firstperson_righthand` is a property of the fixture rather than of what the
/// fixture's author remembered to type.
fn template_skull_display() -> DisplayTransforms {
    DisplayTransforms::NONE
        .with(
            DisplaySlot::Gui,
            DisplayTransform {
                rotation: [30.0, 45.0, 0.0],
                translation: [0.0, 3.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .with(
            DisplaySlot::Fixed,
            DisplayTransform {
                rotation: [0.0, 180.0, 0.0],
                translation: [0.0, 4.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            },
        )
        .with(
            DisplaySlot::OnShelf,
            DisplayTransform {
                rotation: [0.0, 0.0, 0.0],
                translation: [0.0, 8.0, 0.0],
                scale: [2.0, 2.0, 2.0],
            },
        )
        .with(
            DisplaySlot::Ground,
            DisplayTransform {
                rotation: [0.0, 0.0, 0.0],
                translation: [0.0, 3.0, 0.0],
                scale: [0.5, 0.5, 0.5],
            },
        )
        .with(
            DisplaySlot::ThirdPersonRightHand,
            DisplayTransform {
                rotation: [45.0, 45.0, 0.0],
                translation: [0.0, 3.0, 0.0],
                scale: [0.5, 0.5, 0.5],
            },
        )
}

/// `assets/minecraft/models/item/template_chest.json`'s `firstperson_righthand`,
/// transcribed from the jar. `item/chest` — the `base` a chest item's special
/// node names — declares no `display` of its own and inherits this through
/// vanilla's resolved-model top-transform function's per-slot walk.
fn template_chest_first_person() -> DisplayTransforms {
    DisplayTransforms::NONE.with(
        DisplaySlot::FirstPersonRightHand,
        DisplayTransform {
            rotation: [0.0, 315.0, 0.0],
            translation: [0.0, 0.0, 0.0],
            scale: [0.4, 0.4, 0.4],
        },
    )
}

/// The camera-space AABB of `min..max` under `m`, over all eight corners.
fn aabb(m: Mat4, min: Vec3, max: Vec3) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8u8 {
        let p = Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        );
        let q = m.transform_point3(p);
        lo = lo.min(q);
        hi = hi.max(q);
    }
    (lo, hi)
}

/// The production placement for one held special item at rest.
fn held_placement(display: &DisplayTransforms, node: &[ItemNodeTransform]) -> Mat4 {
    let transform = hand_transform(display, Arm::Right, true);
    let outer = first_person_item_matrix(Arm::Right, 0.0, 0.0, &transform);
    compose_special_node_transform(outer, node)
}

/// The baked mesh's own local AABB, so this gate is testing the *shipped* rig
/// rather than a second, hand-typed copy of vanilla's skull model's box that could agree
/// with vanilla while the rig did not.
fn mesh_aabb(name: &str) -> (Vec3, Vec3) {
    let set = BlockEntityModelSet::load();
    let mesh = set
        .get(name)
        .unwrap_or_else(|| panic!("{name} is not in BLOCK_ENTITY_MODELS"));
    (mesh.local_min, mesh.local_max)
}

/// Both hypotheses must be evaluated: it is the *distance* to the wrong ones
/// that makes this a magnitude assertion rather than a direction check.
const TOL: f32 = 1e-4;

/// The base head box, vanilla's skull model's head-model-creation function's
/// `addBox(-4, -8, -4, 8, 8, 8)` at `PartPose.ZERO`, in blocks.
const SKULL_HEAD_LO: Vec3 = Vec3::new(-0.25, -0.5, -0.25);
/// The other corner of [`SKULL_HEAD_LO`]'s box.
const SKULL_HEAD_HI: Vec3 = Vec3::new(0.25, 0.0, 0.25);

/// vanilla's skull model's humanoid-head-layer function's `"hat"` overlay is the same box
/// under `new CubeDeformation(0.25F)`, which inflates symmetrically — so it
/// widens the model's extent by a quarter texel on each of the six faces.
///
/// `0.25 / 16.0`, spelled out because it is the difference between the two
/// hypotheses in every assertion below.
const SKULL_HAT_INFLATION_BLOCKS: f32 = 0.25 / 16.0;

fn hat_lo() -> Vec3 {
    SKULL_HEAD_LO - Vec3::splat(SKULL_HAT_INFLATION_BLOCKS)
}

fn hat_hi() -> Vec3 {
    SKULL_HEAD_HI + Vec3::splat(SKULL_HAT_INFLATION_BLOCKS)
}

#[test]
fn the_skull_rig_is_authored_y_down_exactly_as_vanilla_authors_it() {
    // vanilla's skull model's head-model-creation function: `addBox(-4, -8, -4, 8, 8, 8)`, `PartPose.ZERO`,
    // `/16` at compile time. Every number below depends on this, and a rig baked
    // block-space-up instead would make the node transformation's 180°-about-X
    // flip push the head *down* rather than up — the same half-block error, from
    // the other end of the chain.
    //
    // # The two canvases differ by more than their canvas
    //
    // This gate once asserted the bare head box for **both** models, which was
    // right for `skull_mob` and stale for `skull_humanoid`:
    // `createHumanoidHeadLayer` adds a `"hat"` child inflated `0.25`, and the
    // item path bakes the very same layer the block-entity path does
    // (vanilla's skull block-renderer's create-model function → `ModelLayers.PLAYER_HEAD` →
    // `humanoidHeadLayer`), with vanilla's own `getExtentsForGui` walking the
    // whole root. So the hatted extent is the one vanilla measures for a held
    // head too, and asserting the bare box here was asserting the absence of a
    // layer rather than the authoring convention this gate is named for.
    //
    // Both models are checked, against *different* expectations, which is what
    // makes the pair discriminating: a hat wrongly added to the mob rig, or
    // wrongly dropped from the humanoid one, fails exactly one of them.
    let mut failures = Vec::new();
    for (name, want_lo, want_hi) in [
        (SKULL_MOB, SKULL_HEAD_LO, SKULL_HEAD_HI),
        (SKULL_HUMANOID, hat_lo(), hat_hi()),
    ] {
        let (lo, hi) = mesh_aabb(name);
        if (lo - want_lo).abs().max_element() >= TOL || (hi - want_hi).abs().max_element() >= TOL {
            failures.push(format!(
                "{name} is baked at {lo:?}..{hi:?}, not vanilla's Y-down {want_lo:?}..{want_hi:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn a_held_player_head_lands_where_the_jar_says_and_nowhere_near_the_alternatives() {
    let node = [player_head_node_transform()];
    let display = template_skull_display();

    // The premise this whole gate rests on, asserted rather than assumed: the
    // jar declares no first-person slot for a skull, so `hand_transform` must
    // answer with the identity — vanilla's `ItemTransform.NO_TRANSFORM`.
    assert!(
        display.declared(DisplaySlot::FirstPersonRightHand).is_none(),
        "the fixture gained a firstperson_righthand slot; re-read \
         item/template_skull.json before trusting anything below"
    );
    let resolved = hand_transform(&display, Arm::Right, true);
    assert_eq!(
        resolved,
        DisplayTransform::default(),
        "an undeclared firstperson_righthand must resolve to the identity \
         (vanilla's NO_TRANSFORM), not to a neighbouring slot"
    );

    let (lo, hi) = mesh_aabb(SKULL_HUMANOID);
    let (min, max) = aabb(held_placement(&display, &node), lo, hi);

    // Hand-derived from the jar, not read off this pipeline:
    //   T(0.56, -0.52, -0.72) · T(-0.5, -0.5, -0.5) · T(0.5, 0, 0.5) · Rx(180°)
    // folds to T(0.56, -1.02, -0.72) · Rx(180°); Rx(180°) sends the mesh's
    // y ∈ [-0.515625, 0.015625] to [-0.015625, 0.515625] and leaves
    // |x|, |z| ≤ 0.265625.
    //
    // The half-extent is `4 + 0.25` texels, not `4`: `ModelLayers.PLAYER_HEAD`
    // is `createHumanoidHeadLayer`, whose `"hat"` child is inflated `0.25`,
    // and the held-item path bakes that same layer
    // (vanilla's skull block-renderer's create-model function, shared with the block-entity path)
    // while vanilla's skull special-renderer's extents function measures the whole root through
    // `getExtentsForGui`. The bare-head numbers this gate carried before are
    // the *un-hatted* hypothesis, and they are computed below so the
    // assertion says which one it landed on.
    let fold = Vec3::new(0.56, -1.02, -0.72);
    let half = 0.25 + SKULL_HAT_INFLATION_BLOCKS;
    let want_min = Vec3::new(fold.x - half, fold.y - SKULL_HAT_INFLATION_BLOCKS, fold.z - half);
    let want_max = Vec3::new(
        fold.x + half,
        fold.y + 0.5 + SKULL_HAT_INFLATION_BLOCKS,
        fold.z + half,
    );
    let unhatted_min = Vec3::new(fold.x - 0.25, fold.y, fold.z - 0.25);
    let unhatted_max = Vec3::new(fold.x + 0.25, fold.y + 0.5, fold.z + 0.25);
    assert!(
        (want_min - unhatted_min).abs().max_element() > 1e-3,
        "the hatted and un-hatted hypotheses coincide, so this gate cannot tell them apart"
    );
    assert!(
        (min - unhatted_min).abs().max_element() > 1e-3
            || (max - unhatted_max).abs().max_element() > 1e-3,
        "held player head landed on the un-hatted hypothesis {unhatted_min:?}..{unhatted_max:?}, \
         which means the skull model lost its hat overlay"
    );
    assert!(
        (min - want_min).abs().max_element() < 1e-3 && (max - want_max).abs().max_element() < 1e-3,
        "held player head lands at {min:?}..{max:?}, jar-derived expectation is \
         {want_min:?}..{want_max:?}"
    );

    // The three wrong hypotheses, each a *different* composition of the same
    // terms, each computed here so the assertion is "it landed on this one and
    // not those" rather than "it moved in the right direction".
    let dropped_node = aabb(held_placement(&display, &[]), lo, hi).1.y;
    let third_person = aabb(
        held_placement(
            &DisplayTransforms::NONE.with(
                DisplaySlot::FirstPersonRightHand,
                display
                    .declared(DisplaySlot::ThirdPersonRightHand)
                    .expect("the fixture declares thirdperson_righthand"),
            ),
            &node,
        ),
        lo,
        hi,
    )
    .1
    .y;
    // The centring omitted: vanilla's item-transform apply function's trailing translate(-0.5³),
    // which vanilla applies on *both* sides of its `NO_TRANSFORM` branch.
    let uncentred = max.y + 0.5;

    for (label, other) in [
        ("the node transformation dropped", dropped_node),
        ("the thirdperson_righthand slot read instead", third_person),
        ("the -0.5 centring omitted", uncentred),
    ] {
        assert!(
            (max.y - other).abs() > 0.2,
            "the correct pose and \"{label}\" agree to within {:.3} on the head's \
             top edge ({} vs {other}) — this gate cannot tell them apart",
            (max.y - other).abs(),
            max.y
        );
    }
}

#[test]
fn a_held_chest_takes_its_declared_first_person_slot_and_sits_above_the_head() {
    // The control arm, and a real one: a chest's `base` *does* declare
    // `firstperson_righthand` and carries **no** node transformation at all, so
    // it exercises the opposite branch of both terms this gate is about.
    let display = template_chest_first_person();
    let (lo, hi) = mesh_aabb(CHEST_SINGLE);
    let (min, max) = aabb(held_placement(&display, &[]), lo, hi);

    // `y` is untouched by the slot's 315° rotation about Y, so it is derivable
    // in one line from the jar: -0.52 + 0.4 · (mesh_y - 0.5).
    let want_y_min = -0.52 + 0.4 * (lo.y - 0.5);
    let want_y_max = -0.52 + 0.4 * (hi.y - 0.5);
    assert!(
        (min.y - want_y_min).abs() < 1e-3 && (max.y - want_y_max).abs() < 1e-3,
        "held chest spans y {}..{}, jar-derived expectation is {want_y_min}..{want_y_max}",
        min.y,
        max.y
    );

    // And the ordering the head gate's doc claims, measured rather than
    // asserted from memory: an un-compensated skull really does hang lower than
    // a chest posed by its own declared slot.
    let head_max_y = aabb(
        held_placement(&template_skull_display(), &[player_head_node_transform()]),
        Vec3::new(-0.25, -0.5, -0.25),
        Vec3::new(0.25, 0.0, 0.25),
    )
    .1
    .y;
    assert!(
        head_max_y < max.y,
        "a held skull's top edge ({head_max_y}) is no longer below a held chest's \
         ({}) — one of the two poses changed",
        max.y
    );
}
