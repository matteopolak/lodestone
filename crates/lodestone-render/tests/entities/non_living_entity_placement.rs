//! Every non-projectile entity used to be placed by `dying_entity_model_matrix`,
//! the `LivingEntityRenderer` convention that lifts the model by
//! [`MODEL_FEET_OFFSET`] = 1.501 blocks. That is correct for a mob (a mob's
//! model origin sits *above* its feet in local space) and wrong for a boat,
//! chest boat, raft, chest raft or minecart, none of which extend
//! `LivingEntity` in vanilla — a placed boat floated 1.126 blocks above the
//! water and its interaction hitbox (built from the real, un-lifted position)
//! sat at the water, so right-clicking it did nothing.
//!
//! `boat_model_resolution.rs` gates only *which rig* an entity type resolves
//! to, never *where* that rig is placed — a defect entirely invisible to a
//! test that only checks the resolved model name. This file is that missing
//! placement gate.
//!
//! The expectation is not our own encoder read back: it is hand-derived from
//! the 26.2 decompile (`AbstractBoatRenderer.submit`,
//! `AbstractMinecartRenderer.submit`/`newRender`) via
//! `non_living_vehicle_matrix`'s own doc comment, which cites the exact
//! pose-stack ops and line numbers.

use glam::Vec3;
use lodestone_render::entity::{EntityModelSet, MODEL_FEET_OFFSET};
use lodestone_render::entity_anim::AnimInput;

/// Corpus model names that are non-living vehicles and must not get the
/// `LivingEntityRenderer` feet lift. `armor_stand` is deliberately absent:
/// `ArmorStand extends LivingEntity` in vanilla, so it keeps the mob
/// convention and is exactly the control a too-broad "non-projectile" rule
/// would get wrong.
const NON_LIVING_VEHICLES: &[&str] = &["boat", "chest_boat", "raft", "chest_raft", "minecart"];

/// A living-entity control the census above must *not* affect — if this
/// fails alongside the vehicle gate, the fixture harness is broken rather
/// than the fix; if it fails alone, the fix over-applied.
const LIVING_CONTROL: &str = "pig";

/// The model's own local origin `(0, 0, 0)` is a fixed point under every
/// placement transform in this crate: a yaw rotation about `Y` and the
/// `(-1, -1, 1)` flip both fix the origin (rotating or scaling the zero
/// vector yields the zero vector), so the origin's transformed `Y` is
/// *exactly* the vertical term the placement adds — nothing else can move it.
/// This is what makes the origin a clean probe instead of needing the whole
/// mesh's AABB.
fn placed_origin_y(models: &EntityModelSet, model_name: &str, feet_y: f32) -> f32 {
    let instance = models
        .resolve(
            model_name,
            Vec3::new(0.0, feet_y, 0.0),
            0.0,
            1.0,
            &AnimInput::REST,
        )
        .unwrap_or_else(|| panic!("{model_name} has no corpus rig — the table is stale"));
    instance.transform.transform_point3(Vec3::ZERO).y
}

/// The fix: every non-living vehicle's model origin sits `0.375` blocks above
/// its feet (vanilla's own bob), not `-1.501` (the living lift this bug
/// applied). Collected and asserted together, not one `assert!` per model in
/// a loop, so a regression names every failing rig instead of only whichever
/// sorts first.
#[test]
fn non_living_vehicles_get_the_vanilla_bob_not_the_living_lift() {
    let models = EntityModelSet::load();
    let mut wrong = Vec::new();
    for &name in NON_LIVING_VEHICLES {
        let y = placed_origin_y(&models, name, 10.0);
        let expected = 10.0 + 0.375;
        if (y - expected).abs() >= 1e-4 {
            wrong.push(format!(
                "{name}: model origin at y={y}, expected {expected} (feet + 0.375). \
                 A y near {} means it is still getting the LivingEntityRenderer's \
                 {MODEL_FEET_OFFSET}-block lift.",
                10.0 + MODEL_FEET_OFFSET
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} non-living vehicles are placed wrongly: {wrong:#?}",
        wrong.len(),
        NON_LIVING_VEHICLES.len()
    );
}

/// The negative control: a real `LivingEntity` must keep the 1.501 lift, so
/// the assertion above is discriminating rather than accidentally true for
/// every model regardless of placement.
#[test]
fn a_living_entity_still_gets_the_feet_lift() {
    let models = EntityModelSet::load();
    let y = placed_origin_y(&models, LIVING_CONTROL, 10.0);
    let expected = 10.0 + MODEL_FEET_OFFSET;
    assert!(
        (y - expected).abs() < 1e-4,
        "{LIVING_CONTROL}: model origin at y={y}, expected {expected} (feet + \
         {MODEL_FEET_OFFSET}) — if this fails, the vehicle gate above proves \
         nothing, because nothing in this fixture actually exercises the lift."
    );
}

/// `armor_stand` is the case a too-broad "everything non-projectile that
/// isn't obviously a mob" rule would get wrong: it is not in
/// [`NON_LIVING_VEHICLES`] and must keep the living lift exactly like a pig,
/// because `ArmorStandRenderer extends LivingEntityRenderer` in vanilla.
#[test]
fn armor_stand_keeps_the_living_lift_despite_its_name() {
    let models = EntityModelSet::load();
    let y = placed_origin_y(&models, "armor_stand", 10.0);
    let expected = 10.0 + MODEL_FEET_OFFSET;
    assert!(
        (y - expected).abs() < 1e-4,
        "armor_stand: model origin at y={y}, expected {expected} (feet + \
         {MODEL_FEET_OFFSET}) — ArmorStand extends LivingEntity in vanilla and \
         must not be swept into the non-living vehicle placement by a \
         name-shaped heuristic."
    );
}
