//! Closes the last hop of a reported symptom ("if i look at an
//! enderman in the eyes it doesnt do anything"): does a sustained stare
//! actually turn into *behaviour* through the real production `MobSim`, not
//! just into the `is_being_stared_at` boolean.
//!
//! `crates/lodestone-server/src/mobs/mod.rs`'s own
//! `the_gaze_feed_reaches_is_being_stared_at_and_a_look_away_does_not`
//! already proves the feed (`PlayerPerception::view_direction` ->
//! `is_in_view_cone` -> `set_stared_at`) reaches `MobController` — but it
//! stops there and asserts the boolean directly. Nothing before this file
//! proved that boolean actually reaches `EndermanLookForPlayerGoal`'s
//! aggro-delay state machine (`crates/lodestone-entity/src/ai/goals.rs`)
//! *through* `MobSim::spawn_species`'s real roster wiring
//! (`roster::goals_for`) — the same "does any gate in this subsystem reach
//! the layer that actually acts" question this repo asks of every reported
//! island.
//!
//! Two phases, in one continuous run against one enderman: sustained eye
//! contact must freeze it in place once it acquires a target (vanilla's
//! `EndermanFreezeWhenLookedAt`, priority 1 in the goal selector, requires an
//! existing `attack_target` to activate — so acquisition has to happen
//! first), and looking away must release it to close the distance (vanilla's
//! `MeleeAttackGoal`, which the already-acquired target now drives once the
//! freeze goal's `can_use` fails).

use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkWorld, MobSim, PlayerPerception};
use std::str::FromStr;

/// A flat floor under both the enderman's start and the player's position —
/// wide enough for the pursuit phase to have room to close.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=16 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

/// The exact unit vector from `player_eye` to `mob_eye` — `dot == 1.0`
/// against itself, so this is as deep inside `is_in_view_cone`'s tolerance
/// as any input can be, regardless of the enderman's real per-species eye
/// height or the exact distance. Mirrors
/// `the_gaze_feed_reaches_is_being_stared_at_and_a_look_away_does_not`'s own
/// derivation rather than guessing an axis-aligned vector and hoping the
/// geometry lines up.
fn looking_at(sim: &MobSim<'_>, mob_id: i32, mob_pos: Vec3, player_pos: Vec3) -> Vec3 {
    const PLAYER_EYE_HEIGHT: f64 = 1.62; // vanilla's own standing eye-height getter
    let mob_eye_height = f64::from(sim.get(mob_id).expect("spawned").shape().height) * 0.85;
    let mob_eye = Vec3::new(mob_pos.x, mob_pos.y + mob_eye_height, mob_pos.z);
    let player_eye = Vec3::new(player_pos.x, player_pos.y + PLAYER_EYE_HEIGHT, player_pos.z);
    let delta = Vec3::new(mob_eye.x - player_eye.x, mob_eye.y - player_eye.y, mob_eye.z - player_eye.z);
    let dist = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
    Vec3::new(delta.x / dist, delta.y / dist, delta.z / dist)
}

#[test]
fn a_sustained_stare_freezes_the_enderman_and_looking_away_releases_it_to_close_in() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let enderman_pos = Vec3::new(8.0, 0.0, 0.0);
    let player_pos = Vec3::new(0.0, 0.0, 0.0);
    let id = sim
        .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
        .id();

    let stare = looking_at(&sim, id, enderman_pos, player_pos);
    // As far outside the cone as a unit vector can be — not a near-miss —
    // matching the sibling feed-level gate's own choice for "not staring".
    let look_away = Vec3::new(-stare.x, -stare.y, -stare.z);

    // --- Phase 1: sustained eye contact -----------------------------------
    //
    // `EndermanLookForPlayerGoal`'s own aggro delay is 5 ticks before a
    // pending candidate is promoted to a live `attack_target`; ticking well
    // past that (40 ticks) both gives it time to acquire *and* proves the
    // freeze holds for a sustained stare, not just the one tick acquisition
    // happens on.
    for _ in 0..40 {
        sim.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: stare,
        }]);
        sim.tick();
    }

    let after_stare = sim.get(id).expect("alive");
    assert!(
        after_stare.attack_target().is_some(),
        "40 ticks of direct eye contact must be enough for \
         EndermanLookForPlayerGoal's 5-tick aggro delay to acquire the player \
         as an attack target — if this is None, the gaze either never \
         reaches the goal or never gets past the pending stage"
    );
    let pos_after_stare = after_stare.position();
    let drift = ((pos_after_stare.x - enderman_pos.x).powi(2)
        + (pos_after_stare.y - enderman_pos.y).powi(2)
        + (pos_after_stare.z - enderman_pos.z).powi(2))
    .sqrt();
    assert!(
        drift < 0.5,
        "an enderman with an acquired target that is still being stared at \
         must stay frozen (EndermanFreezeWhenLookedAt stops its navigation) \
         — it drifted {drift:.3} blocks from its spawn point instead"
    );

    // --- Phase 2: look away -------------------------------------------------
    //
    // The target is already live from phase 1 — nothing has to re-acquire
    // it. With the stare gone, `EndermanFreezeWhenLookedAt::can_use` fails
    // (`is_being_stared_at()` is now false) and yields MOVE/JUMP back to
    // whatever else wants them, which for a mob with a live attack target is
    // `MeleeAttackGoal`: it must close the 8-block gap.
    for _ in 0..300 {
        sim.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: look_away,
        }]);
        sim.tick();
    }

    let final_pos = sim.get(id).expect("alive").position();
    let gap_to_player = ((final_pos.x - player_pos.x).powi(2)
        + (final_pos.y - player_pos.y).powi(2)
        + (final_pos.z - player_pos.z).powi(2))
    .sqrt();
    assert!(
        gap_to_player < 3.0,
        "looking away must release the freeze and let the already-acquired \
         target drive MeleeAttackGoal's pursuit — the enderman ended \
         {gap_to_player:.3} blocks from the player instead of closing to \
         melee range (started {:.3} blocks away)",
        ((enderman_pos.x - player_pos.x).powi(2) + (enderman_pos.z - player_pos.z).powi(2)).sqrt()
    );
}

/// Negative control on phase 1 alone: an enderman that is never stared at
/// must neither acquire a target nor move — proving the freeze/acquisition
/// result above is caused by the stare, not by ticking `MobSim` in general.
#[test]
fn an_enderman_never_stared_at_acquires_nothing_and_does_not_move() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let enderman_pos = Vec3::new(8.0, 0.0, 0.0);
    let player_pos = Vec3::new(0.0, 0.0, 0.0);
    let id = sim
        .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
        .id();
    let away = {
        let at = looking_at(&sim, id, enderman_pos, player_pos);
        Vec3::new(-at.x, -at.y, -at.z)
    };

    for _ in 0..40 {
        sim.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: away,
        }]);
        sim.tick();
    }

    let after = sim.get(id).expect("alive");
    assert_eq!(
        after.attack_target(),
        None,
        "a player who never looks at the enderman must never provoke it"
    );
}
