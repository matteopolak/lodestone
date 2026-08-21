//! Ten mob types had a row in the client's hitbox table and no rig, so a player
//! walked into something that drew nothing. This gate is the *placement* half of
//! closing that: it asserts each newly ported rig, resolved from its **registry
//! type path** through the same `EntityModelSet` the frame path uses, lands with
//! its geometry inside the volume the player collides with.
//!
//! # Why an AABB gate rather than a mesh assertion
//!
//! A mesh-level assertion cannot see this class. `lodestone-assets`' own jar
//! coverage already proves every one of these rigs bakes real quads against a real
//! sheet — and it would go on proving that with the mob drawn a metre and a half
//! underground, or a sixth of the size of its own collision box. Both of those are
//! exactly what the ten types needed fixing for, and both are properties of the
//! *placement*, which lives above the mesh.
//!
//! # Where the expected value comes from
//!
//! Not from this crate. The hitbox is
//! `lodestone_data::entity_dimensions::base_dimensions_for`, a generated table
//! keyed by registry id, and the drawn box is `EntityInstance::aabb_min/max` —
//! the same world AABB the frustum cull reads. The two are independent
//! constructions, so agreement between them is evidence rather than a round trip
//! through one belief.
//!
//! The measured criterion is **how much of the collision box the drawn geometry
//! covers vertically**, which is the question "is it invisible while solid?"
//! stated as a number. It is deliberately *not* "the drawn box equals the hitbox":
//! real rigs overhang on purpose — a copper golem's antenna leaves its 0.98-block
//! box, a happy ghast's tentacles hang below its feet — and a gate demanding
//! equality would have to be loosened until it stopped meaning anything.
//!
//! # The controls
//!
//! Three, each reproducing a specific way this could have gone wrong, and each
//! observed to fall far below the threshold rather than argued to:
//!
//! | control | what it stands for | coverage |
//! |---|---|---|
//! | the giant's hitbox against the *unscaled* humanoid rig | forgetting the mesh scale | ~0.16 |
//! | the leash knot through the **mob** placement | routing a non-living rig through the 1.501 feet lift | 0.00 |
//! | the sulfur cube with its root pose reset | dropping the renderer constant folded into the mesh | ~0.04 |
//!
//! Measured, the ten subjects run 0.619 to 1.000 and the three controls 0.000 to
//! 0.169, so the threshold sits in a 3.7× gap rather than between two adjacent
//! numbers. Every measured value is printed on failure, and the loop collects
//! mismatches rather than asserting inside itself, so a run reports *all* the
//! subjects that moved rather than the alphabetically first one.
//!
//! # The breeze's 0.619 is a real, known shortfall — do not read it as noise
//!
//! It is the only subject anywhere near the threshold, and the reason is a gap
//! rather than a mis-transcription: the breeze's solid rig is its head and the
//! three rods, and the bottom third of its collision box is filled by a
//! translucent wind funnel that is a **separate baked layer on its own sheet**.
//! A corpus entry carries one sheet, so that layer is absent, and the lower
//! 0.655 blocks of a breeze really do draw nothing. The number is here so that
//! whoever adds the funnel can watch it climb, and so that a *new* subject
//! landing at 0.62 is read as suspicious rather than as company.

use glam::Vec3;
use lodestone_assets::entity_models::entity_models;
use lodestone_data::entity_dimensions::base_dimensions_for;
use lodestone_data::entity_type::EntityType;
use lodestone_render::entity::{EntityInstance, EntityMesh, EntityModelSet};
use lodestone_render::entity_anim::AnimInput;

/// The ten registry types this pass gave a rig, by the path the wire carries.
const NEWLY_RIGGED: &[&str] = &[
    "breeze",
    "camel_husk",
    "copper_golem",
    "creaking",
    "giant",
    "happy_ghast",
    "leash_knot",
    "nautilus",
    "sulfur_cube",
    "zombie_nautilus",
];

/// The fraction of `[0, height]` that `[lo, hi]` covers.
fn vertical_coverage(lo: f32, hi: f32, height: f32) -> f32 {
    let overlap = hi.min(height) - lo.max(0.0);
    overlap.max(0.0) / height
}

/// The threshold. Chosen to sit in the gap between the three controls (0.00,
/// 0.04, 0.16) and the real subjects, not between two adjacent measurements.
const MIN_COVERAGE: f32 = 0.6;

fn instance_for(models: &EntityModelSet, type_path: &str) -> EntityInstance {
    models
        .resolve(
            type_path,
            Vec3::ZERO,
            0.0,
            1.0,
            &AnimInput::REST,
        )
        .unwrap_or_else(|| {
            panic!(
                "{type_path} resolves to no rig — the registry path does not reach the corpus, \
                 which is the island this pass exists to close"
            )
        })
}

/// Every newly rigged type resolves from its registry path *and* draws inside the
/// box the player collides with.
#[test]
fn every_newly_rigged_type_draws_inside_its_own_hitbox() {
    let models = EntityModelSet::load();
    let mut report: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for path in NEWLY_RIGGED {
        let entity_type = EntityType::from_name(path)
            .unwrap_or_else(|| panic!("{path} is not a registry entity type"));
        let dims = base_dimensions_for(entity_type);
        let instance = instance_for(&models, path);

        let coverage = vertical_coverage(instance.aabb_min.y, instance.aabb_max.y, dims.height);
        report.push(format!(
            "  {path:<16} drawn y [{:.3}, {:.3}]  x [{:.3}, {:.3}]  z [{:.3}, {:.3}]  \
             hitbox {:.3}w x {:.3}h  coverage {coverage:.3}",
            instance.aabb_min.y,
            instance.aabb_max.y,
            instance.aabb_min.x,
            instance.aabb_max.x,
            instance.aabb_min.z,
            instance.aabb_max.z,
            dims.width,
            dims.height,
        ));

        if coverage < MIN_COVERAGE {
            failures.push(format!(
                "{path}: drawn geometry covers only {coverage:.3} of its {:.3}-block collision \
                 box (drawn y [{:.3}, {:.3}]) — solid and all but invisible",
                dims.height, instance.aabb_min.y, instance.aabb_max.y,
            ));
        }
        // The entity's own axis must pass through the drawn footprint. A rig
        // placed sideways still covers its hitbox vertically, so the vertical
        // measure alone cannot see it.
        if instance.aabb_min.x > 0.0 || instance.aabb_max.x < 0.0 {
            failures.push(format!(
                "{path}: drawn x span [{:.3}, {:.3}] does not straddle the entity's own axis",
                instance.aabb_min.x, instance.aabb_max.x,
            ));
        }
        if instance.aabb_min.z > 0.0 || instance.aabb_max.z < 0.0 {
            failures.push(format!(
                "{path}: drawn z span [{:.3}, {:.3}] does not straddle the entity's own axis",
                instance.aabb_min.z, instance.aabb_max.z,
            ));
        }
    }

    eprintln!("newly rigged types, drawn box vs collision box:\n{}", report.join("\n"));
    assert!(
        failures.is_empty(),
        "{} of {} newly rigged types are mis-placed:\n{}",
        failures.len(),
        NEWLY_RIGGED.len(),
        failures.join("\n")
    );
}

/// The controls. Each is the *wrong* hypothesis for one subject, built the way it
/// would really have gone wrong, and each must land under the threshold — if one
/// of these ever passes, the gate above has stopped being able to fail.
#[test]
fn the_three_wrong_placements_all_fall_under_the_threshold() {
    let models = EntityModelSet::load();
    let mut passed_that_should_not: Vec<String> = Vec::new();
    let mut measured: Vec<String> = Vec::new();

    // 1. The giant with no mesh scale: the plain humanoid rig its 6× mesh is
    //    built from, measured against the giant's own twelve-block box.
    let giant_box = base_dimensions_for(EntityType::Giant);
    let unscaled = instance_for(&models, "zombie");
    let c1 = vertical_coverage(unscaled.aabb_min.y, unscaled.aabb_max.y, giant_box.height);
    measured.push(format!("  giant at 1x scale           coverage {c1:.3}"));
    if c1 >= MIN_COVERAGE {
        passed_that_should_not.push(format!("an unscaled humanoid covers {c1:.3} of a giant"));
    }

    // 2. The leash knot through the mob placement — the 1.501-block feet lift a
    //    non-living renderer does not apply.
    let knot_mesh = mesh_named(&models, "leash_knot");
    let mob_placed = EntityInstance::new(
        "leash_knot",
        knot_mesh,
        Vec3::ZERO,
        0.0,
        1.0,
        &AnimInput::REST,
    );
    let knot_box = base_dimensions_for(EntityType::LeashKnot);
    let c2 = vertical_coverage(mob_placed.aabb_min.y, mob_placed.aabb_max.y, knot_box.height);
    measured.push(format!(
        "  leash knot on the mob path  coverage {c2:.3}  (drawn y [{:.3}, {:.3}])",
        mob_placed.aabb_min.y, mob_placed.aabb_max.y
    ));
    if c2 >= MIN_COVERAGE {
        passed_that_should_not
            .push(format!("the leash knot still covers {c2:.3} through the mob placement"));
    }

    // 3. The sulfur cube with the renderer constant dropped from its root pose:
    //    the bare layer, centred on its own pivot.
    let mut bare = entity_models()
        .into_iter()
        .find(|e| e.name == "sulfur_cube")
        .map(|e| (e.build)())
        .expect("sulfur_cube is a corpus entry");
    bare.root.pose = lodestone_assets::entity::PartPose::ZERO;
    let bare_mesh = EntityMesh::from_named_model("sulfur_cube", &bare);
    let bare_instance = EntityInstance::new(
        "sulfur_cube",
        &bare_mesh,
        Vec3::ZERO,
        0.0,
        1.0,
        &AnimInput::REST,
    );
    let cube_box = base_dimensions_for(EntityType::SulfurCube);
    // The adult is size 2, so its live box is the base row doubled — the same
    // factor its rig already carries, read off the registry rather than assumed.
    let cube_height = cube_box.height * 2.0;
    let c3 = vertical_coverage(bare_instance.aabb_min.y, bare_instance.aabb_max.y, cube_height);
    measured.push(format!(
        "  sulfur cube, pose dropped   coverage {c3:.3}  (drawn y [{:.3}, {:.3}])",
        bare_instance.aabb_min.y, bare_instance.aabb_max.y
    ));
    if c3 >= MIN_COVERAGE {
        passed_that_should_not
            .push(format!("the sulfur cube still covers {c3:.3} with its root pose reset"));
    }

    eprintln!("controls (all must be under {MIN_COVERAGE}):\n{}", measured.join("\n"));
    assert!(
        passed_that_should_not.is_empty(),
        "{} control(s) passed a gate they exist to fail:\n{}",
        passed_that_should_not.len(),
        passed_that_should_not.join("\n")
    );
}

fn mesh_named<'a>(models: &'a EntityModelSet, name: &str) -> &'a EntityMesh {
    models
        .get(name)
        .unwrap_or_else(|| panic!("{name} is not a baked corpus entry"))
}

/// The sulfur cube's live hitbox is its base row scaled by its size, so the gate
/// above measures the adult rig against the *size-1* row and would read as a
/// generous overhang either way. This pins the ratio instead, which is the number
/// the rig's own root-pose derivation predicts and which is therefore able to be
/// wrong: the shell overhangs its collision box by the same factor at both sizes.
#[test]
fn the_sulfur_cube_shell_overhangs_its_box_by_the_ratio_its_root_pose_predicts() {
    let models = EntityModelSet::load();
    let instance = instance_for(&models, "sulfur_cube");
    let drawn = instance.aabb_max.y - instance.aabb_min.y;
    let adult_height = base_dimensions_for(EntityType::SulfurCube).height * 2.0;
    let ratio = drawn / adult_height;
    // 18 texels of box at the 0.999 the renderer's two scale steps leave behind,
    // over a 0.98-block adult box: 1.124 / 0.98.
    assert!(
        (ratio - 1.147).abs() < 0.01,
        "the shell/hitbox ratio is {ratio:.4}, not the 1.147 the folded renderer constant \
         predicts — drawn {drawn:.4} blocks over a {adult_height:.4}-block box"
    );
    assert!(
        instance.aabb_min.y.abs() < 0.05,
        "the shell's underside sits at y = {:.4} rather than on the ground",
        instance.aabb_min.y
    );
}
