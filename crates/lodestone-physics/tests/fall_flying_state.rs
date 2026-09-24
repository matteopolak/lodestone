//! The glide **state machine** — vanilla's own "try to start fall flying" step
//! and the half of its own "update fall flying" step that ends a glide.
//!
//! The glide *maths* (`tick_elytra`/`update_fall_flying_movement`) has its own
//! golden traces; this file is about the flag those traces take as given.
//! Record definitions, described rather than transcribed:
//!
//! ```text
//! start:  if not already fall-flying, and can glide, and not in water: start fall-flying
//! can-glide (player): not flying, and the base can-glide check
//! can-glide (base):   not on ground, not a passenger, no Levitation effect,
//!                      and any slot holds a glider
//! end:    every tick, if not can-glide: clear the fall-flying flag
//! ```

use lodestone_physics::{
    PlayerState, StatusEffects, Vec3d, can_glide, try_start_fall_flying, update_fall_flying,
};

/// Airborne, not flying, no levitation — the state every start case varies from.
fn airborne() -> PlayerState {
    let mut state = PlayerState::at(Vec3d::new(0.5, 80.0, 0.5), 0.0);
    state.on_ground = false;
    state
}

#[test]
fn a_glider_in_the_air_starts_gliding_and_reports_it() {
    let mut state = airborne();
    assert!(
        try_start_fall_flying(&mut state, true, false),
        "the return value is the bit vanilla's own client-side per-tick \
         update turns into one START_FALL_FLYING command; a silent start \
         would leave the server simulating a falling player"
    );
    assert!(state.fall_flying);
}

#[test]
fn every_missing_conjunct_refuses_the_start() {
    // One table, one assertion each, because the failure mode this guards is a
    // conjunct being dropped in the port — and a test that only exercised the
    // happy path would pass with any subset of them.
    let cases: [(&str, PlayerState, bool, bool); 5] = [
        ("on the ground", {
            let mut s = airborne();
            s.on_ground = true;
            s
        }, true, false),
        ("no glider equipped", airborne(), false, false),
        ("in water", airborne(), true, true),
        ("creative flight on", {
            let mut s = airborne();
            s = s.with_flight(true, 0.05);
            s
        }, true, false),
        ("levitating", {
            let mut s = airborne();
            s.effects = StatusEffects {
                levitation: Some(0),
                ..StatusEffects::default()
            };
            s
        }, true, false),
    ];
    for (label, mut state, glider, in_water) in cases {
        assert!(
            !try_start_fall_flying(&mut state, glider, in_water),
            "{label}: must not start a glide"
        );
        assert!(!state.fall_flying, "{label}: must not set the flag either");
    }
}

#[test]
fn an_already_gliding_player_does_not_restart() {
    // The `!isFallFlying()` guard is what stops a held jump key sending one
    // START_FALL_FLYING per tick for the whole descent.
    let mut state = airborne();
    state.fall_flying = true;
    assert!(!try_start_fall_flying(&mut state, true, false));
    assert!(state.fall_flying, "and it must stay gliding");
}

#[test]
fn landing_ends_the_glide() {
    // Vanilla clears the shared flag server-side; we predict it, because
    // otherwise `lodestone_physics::tick` routes every subsequent tick to
    // `tick_elytra` and the player can never walk again.
    let mut state = airborne();
    state.fall_flying = true;
    state.on_ground = true;
    update_fall_flying(&mut state, true);
    assert!(!state.fall_flying);
}

#[test]
fn a_glide_in_progress_survives_the_air() {
    let mut state = airborne();
    state.fall_flying = true;
    update_fall_flying(&mut state, true);
    assert!(
        state.fall_flying,
        "the control for `landing_ends_the_glide`: a stop condition that fired \
         unconditionally would pass that test and break every glide"
    );
    // Losing the elytra mid-air ends it, though — vanilla's own can-glide
    // check re-reads equipment every tick.
    update_fall_flying(&mut state, false);
    assert!(!state.fall_flying);
}

#[test]
fn can_glide_is_the_shared_predicate() {
    // Both entry points read one predicate, so they cannot drift apart.
    let mut airborne_with_elytra = airborne();
    assert!(can_glide(&airborne_with_elytra, true));
    assert!(!can_glide(&airborne_with_elytra, false));
    airborne_with_elytra.on_ground = true;
    assert!(!can_glide(&airborne_with_elytra, true));
}
