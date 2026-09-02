//! A humanoid skull's second skin layer — the model's `"hat"` overlay child
//! part, built alongside the base head.
//!
//! # Scope
//!
//! Hermetic CPU gates over the baked mesh: the part exists, its texels come
//! from the right half of the sheet, and its box sits strictly outside the
//! base head. They rasterise nothing, and they do not touch the path that
//! decides *which* texture a placed head samples.
//!
//! The overlay's whole purpose is to be the layer that reads the sheet's
//! right-hand half, so "which half do these UVs come from" is the property
//! worth pinning — a `hat` part accidentally authored at `texOffs(0, 0)`
//! would exist, bake, draw, and duplicate the base head, which looks like a
//! z-fighting bug rather than a wrong unwrap.

use lodestone_render::block_entity::{BlockEntityMesh, SKULL_HUMANOID, SKULL_MOB};

fn mesh_for(name: &str) -> BlockEntityMesh {
    let def = match name {
        SKULL_HUMANOID => lodestone_assets::block_entity_models::skull_humanoid_model(),
        SKULL_MOB => lodestone_assets::block_entity_models::skull_mob_model(),
        other => panic!("no skull model named {other}"),
    };
    BlockEntityMesh::from_model(&def)
}

/// The humanoid skull carries a `hat` and the mob skull does not.
///
/// Both halves matter. `createMobHeadLayer` adds no overlay and a skeleton
/// sheet is 64x32 with nothing at `(32, 0)`, so a `hat` on the mob model
/// would sample the *body* half of a 64x32 sheet and paint a skull with
/// ribcage texels.
#[test]
fn only_the_humanoid_skull_has_a_hat() {
    let humanoid = mesh_for(SKULL_HUMANOID);
    let mob = mesh_for(SKULL_MOB);
    assert!(
        humanoid.index_of("hat").is_some(),
        "the humanoid skull has no hat part, so a player head can draw no second layer; \
         parts are {:?}",
        humanoid.part_names
    );
    assert!(
        mob.index_of("hat").is_none(),
        "the mob skull grew a hat part; createMobHeadLayer adds none and a 64x32 sheet has \
         nothing at (32, 0) to draw. parts are {:?}",
        mob.part_names
    );
}

/// The hat's texels come from the **right** half of the 64x64 sheet.
///
/// `texOffs(32, 0)` on a 64-wide sheet puts every hat U at or above `0.5`,
/// while the base head's `texOffs(0, 0)` box is 32 texels wide and so stays
/// at or below `0.5`. Both hypotheses are computed from the sheet size
/// rather than asserted as a bare inequality: a hat wrongly left at
/// `texOffs(0, 0)` would land on the head's own range, and the numbers say
/// which happened.
#[test]
fn the_hat_unwraps_onto_the_right_half_of_the_sheet() {
    let mesh = mesh_for(SKULL_HUMANOID);
    let hat = mesh.index_of("hat").expect("the hat part");
    let head = mesh.index_of("head").expect("the head part");

    let u_range = |part: usize| {
        let range = &mesh.parts[part];
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        mesh.vertices[start..end]
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.uv[0]), hi.max(v.uv[0]))
            })
    };

    let (hat_lo, hat_hi) = u_range(hat);
    let (head_lo, head_hi) = u_range(head);

    // A `texOffs(0, 0)` / `texOffs(32, 0)` box of size 8x8x8 spans
    // `2 * (8 + 8) = 32` texels of U, so on a 64-wide sheet: head [0, 0.5],
    // hat [0.5, 1.0].
    let mut failures = Vec::new();
    if (head_lo - 0.0).abs() > 1.0e-5 || (head_hi - 0.5).abs() > 1.0e-5 {
        failures.push(format!("head U is [{head_lo}, {head_hi}], expected [0, 0.5]"));
    }
    if (hat_lo - 0.5).abs() > 1.0e-5 || (hat_hi - 1.0).abs() > 1.0e-5 {
        failures.push(format!(
            "hat U is [{hat_lo}, {hat_hi}], expected [0.5, 1.0]; [0, 0.5] would mean the hat \
             was left at texOffs(0, 0) and is duplicating the base head"
        ));
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// The hat box is inflated `0.25` texels, so it sits strictly outside the
/// base head on every axis.
///
/// The measured quantity is the mesh's own local AABB, which the base head
/// alone would leave at exactly 8 texels (0.5 blocks) on each axis. With the
/// overlay it is `8 + 2 * 0.25 = 8.5` texels, i.e. `0.53125` blocks. The two
/// hypotheses are 6% apart, so this is a prediction rather than a direction:
/// an un-inflated hat coincides with the head exactly and z-fights, which is
/// worse than having no overlay at all.
#[test]
fn the_hat_is_inflated_a_quarter_texel_clear_of_the_head() {
    let mesh = mesh_for(SKULL_HUMANOID);
    let span = mesh.local_max - mesh.local_min;
    let with_hat = 8.5 / 16.0;
    let head_only = 8.0 / 16.0;
    let mut failures = Vec::new();
    for (axis, got) in [("x", span.x), ("y", span.y), ("z", span.z)] {
        if (got - with_hat).abs() > 1.0e-5 {
            failures.push(format!(
                "{axis} span is {got}; an inflated hat predicts {with_hat} and a head with no \
                 overlay (or an un-inflated one) predicts {head_only}"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

/// The hat is a child *of* the head, not a sibling.
///
/// The model builds the hat as a child added onto the head part, so the
/// overlay inherits the head's pose: the pose function rotates `head` by the
/// block's yaw and pitch, and a sibling hat would keep its own orientation and
/// slide off a rotated skull. Nothing about the rest pose distinguishes the
/// two — both start at the identity pose — so this is only observable in the
/// parent link.
#[test]
fn the_hat_is_parented_to_the_head() {
    let mesh = mesh_for(SKULL_HUMANOID);
    let hat = mesh.index_of("hat").expect("the hat part");
    let head = mesh.index_of("head").expect("the head part");
    assert_eq!(
        mesh.part_parents[hat],
        Some(head),
        "the hat's parent is {:?}, not the head; a sibling overlay does not follow the skull's \
         own rotation",
        mesh.part_parents[hat].map(|i| mesh.part_names[i].clone())
    );
}
