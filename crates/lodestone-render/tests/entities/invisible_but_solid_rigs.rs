//! Thirteen mob types were named across the client's draw surface — a hitbox row,
//! an arm-pose classifier, a bow-pose set — and resolved no rig, so a player
//! walked into something that drew nothing. This gate is the *placement* half of
//! closing that: it asserts each newly reachable rig, resolved from its
//! **registry type path** through the same `EntityModelSet` the frame path uses,
//! lands with its geometry inside the volume the player collides with.
//!
//! A second population lives at the bottom of this file: six types the census
//! called *absent* — nothing drew them and nothing named them — whose vanilla
//! renderer is a cuboid rig or the shared minecart frame. They get their own
//! gate rather than joining the list above, because the question is different:
//! a wither skull is not solid, so "how much of the collision box does it fill?"
//! is not the thing that can go wrong. What can is the *placement*, and each of
//! those four rigs is on a different matrix from the mob path.
//!
//! Ten needed a new mesh. The other three needed only routing — an elder guardian
//! is the guardian mesh at 2.35×, a parched is a skeleton with a second overlay
//! box on every part, and a mannequin is drawn by the *player's own* renderer —
//! and they are in the same list on purpose: "the rig exists and nothing reaches
//! it" and "no rig exists" produce the identical symptom on screen, so one gate
//! should be able to fail for either.
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
//! Measured, the thirteen subjects run 0.619 to 1.000 and the three controls 0.000 to
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

/// The registry types this pass made drawable, by the path the wire carries. The
/// first ten got a new mesh; the last three got a route to one that already
/// existed.
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
    "elder_guardian",
    "mannequin",
    "parched",
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

// ---------------------------------------------------------------------------
// The projectiles and effects: a different question, so a different gate
// ---------------------------------------------------------------------------

/// The census's *absent* bucket — nothing drew them and nothing named them —
/// restricted to the six whose vanilla renderer is a cuboid rig or the shared
/// minecart frame. The remaining absent types need a **draw path** rather than a
/// rig (a camera-facing quad built vertex by vertex, a procedural bolt, an item
/// model taken from entity metadata) and are out of scope for a corpus entry.
const NEWLY_DRAWN_PROJECTILES: &[&str] = &[
    "evoker_fangs",
    "shulker_bullet",
    "wither_skull",
    "llama_spit",
    "spawner_minecart",
    "command_block_minecart",
];

/// These are not "invisible while solid" — most of them are not solid at all —
/// so the hitbox-coverage question the gate above asks is the wrong one. What
/// they can still get wrong, and what this asserts, is landing somewhere other
/// than on the entity: every one of the four cuboid rigs is placed by a
/// *different* matrix from the mob path, and picking the wrong one moves the
/// draw by the 1.501-block feet lift, which is several times the size of a
/// wither skull.
#[test]
fn every_newly_drawn_projectile_lands_on_its_own_entity() {
    let models = EntityModelSet::load();
    let mut report: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for path in NEWLY_DRAWN_PROJECTILES {
        let entity_type = EntityType::from_name(path)
            .unwrap_or_else(|| panic!("{path} is not a registry entity type"));
        let dims = base_dimensions_for(entity_type);
        let instance = instance_for(&models, path);
        let mesh = mesh_named(&models, instance.model);

        report.push(format!(
            "  {path:<24} rig {:<10} y [{:.3}, {:.3}]  x [{:.3}, {:.3}]  z [{:.3}, {:.3}]  \
             hitbox {:.3}w x {:.3}h",
            instance.model,
            instance.aabb_min.y,
            instance.aabb_max.y,
            instance.aabb_min.x,
            instance.aabb_max.x,
            instance.aabb_min.z,
            instance.aabb_max.z,
            dims.width,
            dims.height,
        ));

        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            failures.push(format!("{path}: its rig bakes no geometry at all"));
        }
        // The drawn box must reach the entity's own cell. Stated as an overlap
        // with the collision box rather than as a distance, so it cannot be
        // satisfied by a rig that merely happens to be large.
        let overlaps = instance.aabb_min.y <= dims.height
            && instance.aabb_max.y >= 0.0
            && instance.aabb_min.x <= dims.width / 2.0
            && instance.aabb_max.x >= -dims.width / 2.0;
        if !overlaps {
            failures.push(format!(
                "{path}: drawn box y [{:.3}, {:.3}] x [{:.3}, {:.3}] misses its own \
                 {:.3}w x {:.3}h collision box entirely",
                instance.aabb_min.y,
                instance.aabb_max.y,
                instance.aabb_min.x,
                instance.aabb_max.x,
                dims.width,
                dims.height,
            ));
        }
    }

    eprintln!("newly drawn projectiles and effects:\n{}", report.join("\n"));
    assert!(
        failures.is_empty(),
        "{} of {} are mis-placed:\n{}",
        failures.len(),
        NEWLY_DRAWN_PROJECTILES.len(),
        failures.join("\n")
    );
}

/// The control for the gate above: each of the **three** rigs whose renderer is
/// not a living-entity one, put on the mob placement instead, must be pushed
/// clear of its entity by the 1.501-block feet lift.
///
/// `evoker_fangs` is not in this list, and the first version of it was — the
/// control ran and reported a shift of exactly `+0.000`, which is the finding
/// rather than a flaw: a fang's renderer **does** apply the flip and the 1.501
/// lift, so for that one rig the mob placement is the right placement and the
/// two hypotheses coincide. Its own control is the next test, on the axis that
/// actually differs. Left written down because "the control did not move" reads
/// as a broken control and was, here, a correct measurement.
///
/// Collected rather than asserted in the loop, so a run says which of the three
/// moved rather than only the first.
#[test]
fn the_mob_placement_pushes_every_projectile_rig_off_its_own_entity() {
    let models = EntityModelSet::load();
    let mut still_overlapping: Vec<String> = Vec::new();
    let mut measured: Vec<String> = Vec::new();

    for (path, rig) in [
        ("shulker_bullet", "shulker_bullet"),
        ("wither_skull", "wither_skull"),
        ("llama_spit", "llama_spit"),
    ] {
        let dims = base_dimensions_for(EntityType::from_name(path).expect("a registry type"));
        let real = instance_for(&models, path);
        let wrong = EntityInstance::new(
            rig,
            mesh_named(&models, rig),
            Vec3::ZERO,
            0.0,
            1.0,
            &AnimInput::REST,
        );
        let shift = wrong.aabb_min.y - real.aabb_min.y;
        measured.push(format!(
            "  {path:<16} own y [{:.3}, {:.3}]  mob-path y [{:.3}, {:.3}]  shift {shift:+.3}",
            real.aabb_min.y, real.aabb_max.y, wrong.aabb_min.y, wrong.aabb_max.y,
        ));
        if wrong.aabb_min.y <= dims.height && wrong.aabb_max.y >= 0.0 {
            still_overlapping.push(format!(
                "{path}: the mob placement still leaves the rig on its own entity \
                 (y [{:.3}, {:.3}] against a {:.3}-block box), so the gate above cannot \
                 tell the two placements apart",
                wrong.aabb_min.y, wrong.aabb_max.y, dims.height,
            ));
        }
    }

    eprintln!("wrong-placement control:\n{}", measured.join("\n"));
    assert!(
        still_overlapping.is_empty(),
        "{} control(s) failed to move:\n{}",
        still_overlapping.len(),
        still_overlapping.join("\n")
    );
}

/// The evoker fangs' own control, on the axis that separates its placement from
/// a mob's: the 90° its renderer yaws by and a mob's does not, folded into the
/// rig's root pose.
///
/// A pure rotation about Y cannot change a box's height, so the lift-shaped
/// control cannot see this one. What it *does* change is which horizontal axis
/// the fang's long side lies along — the jaws lean out along one axis and the
/// base is square, so dropping the fold swaps the drawn box's x and z spans. The
/// two hypotheses are therefore separated by a comparison of two numbers that
/// are both measured, with no threshold to fit.
#[test]
fn dropping_the_evoker_fangs_quarter_turn_swaps_the_drawn_box_axes() {
    let models = EntityModelSet::load();
    let folded = instance_for(&models, "evoker_fangs");

    let mut unfolded_def = entity_models()
        .into_iter()
        .find(|e| e.name == "evoker_fangs")
        .map(|e| (e.build)())
        .expect("evoker_fangs is a corpus entry");
    unfolded_def.root.pose.y_rot = 0.0;
    let unfolded_mesh = EntityMesh::from_named_model("evoker_fangs", &unfolded_def);
    let unfolded = EntityInstance::new(
        "evoker_fangs",
        &unfolded_mesh,
        Vec3::ZERO,
        0.0,
        1.0,
        &AnimInput::REST,
    );

    let span = |lo: f32, hi: f32| hi - lo;
    let fx = span(folded.aabb_min.x, folded.aabb_max.x);
    let fz = span(folded.aabb_min.z, folded.aabb_max.z);
    let ux = span(unfolded.aabb_min.x, unfolded.aabb_max.x);
    let uz = span(unfolded.aabb_min.z, unfolded.aabb_max.z);
    eprintln!(
        "evoker fangs: with the quarter turn x {fx:.3} z {fz:.3}; without it x {ux:.3} z {uz:.3}"
    );

    // The rig has to be asymmetric for this control to mean anything at all, so
    // that is asserted before the swap rather than assumed.
    assert!(
        (fx - fz).abs() > 0.5,
        "the fangs' drawn box is near-square (x {fx:.3}, z {fz:.3}), so a quarter turn \
         would be invisible and this control proves nothing"
    );
    assert!(
        (ux - fz).abs() < 1e-3 && (uz - fx).abs() < 1e-3,
        "dropping the root quarter turn should swap the drawn box's horizontal spans \
         (expected x {fz:.3}, z {fx:.3}); got x {ux:.3}, z {uz:.3}"
    );
    // And the height is untouched, which is why the lift-shaped control above is
    // blind to this and had to be separated from it.
    assert!(
        (unfolded.aabb_min.y - folded.aabb_min.y).abs() < 1e-4
            && (unfolded.aabb_max.y - folded.aabb_max.y).abs() < 1e-4,
        "a rotation about Y changed the drawn height, which means something other \
         than the fold moved"
    );
}
