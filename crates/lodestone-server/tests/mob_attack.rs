//! Acceptance gate for issue #12: [`MobSim::attack`] — the hop that resolves
//! a player's `Attack` packet into real damage and knockback against a live
//! mob, through the crate's **public** API (the same "no `#[cfg(test)]`
//! fake, a real consumer" discipline `tests/mob_sim.rs` already established
//! for AI-driven movement).
//!
//! Every expected number here is either hand-derived from the exact formula
//! [`lodestone_entity::apply_reductions`]/[`lodestone_physics::knockback::knockback_impulse`]
//! already implement (both independently jar-verified — see those crates'
//! own tests) or taken from a value already live-verified against a real
//! vanilla 26.2 server (the diamond-armour case, cited below) — never
//! invented for this file.

use lodestone_entity::{DamageFlags, Defenses};
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkWorld, MobSim};
use lodestone_physics::geometry::Vec3d;
use lodestone_physics::knockback::knockback_impulse;

fn empty_world() -> ChunkWorld {
    ChunkWorld::new(-4, 24)
}

/// Spawns a single generic-default mob (20 max health, no armour) at `pos`
/// and returns its id.
fn spawn_plain_mob(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
    let entity_type = ResourceKey::new("minecraft", "zombie").expect("valid key");
    sim.spawn_species(entity_type, pos).id()
}

/// A hit against an id nobody holds must be a clean `None`, never a panic —
/// the control that proves [`MobSim::attack`] actually looks the target up
/// rather than assuming it exists.
#[test]
fn attack_against_an_unknown_target_id_returns_none() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    spawn_plain_mob(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    let outcome = sim.attack(9999, Vec3::new(0.0, 0.0, 0.0), 5.0, DamageFlags::default(), 0.0);
    assert_eq!(outcome, None, "no mob holds id 9999");
}

/// The full reduction pipeline actually runs, with the exact number this
/// project already **live-verified against a real vanilla 26.2 server**: a
/// diamond-armoured target (armor 20.0, toughness 8.0) takes exactly 3.0
/// from a raw 10.0 `minecraft:mob_attack`-shaped hit — see
/// `lodestone-entity/src/damage.rs`'s
/// `armor_formula_lands_on_the_toughness_hypothesis_not_the_flat_one` for the
/// RCON transcript this number comes from. This is the **magnitude** check
/// (CLAUDE.md's vacuous-test species): a wrong flat-percentage formula would
/// also show "damage reduced" but land on 2.0, not 3.0.
#[test]
fn attack_runs_the_full_armour_reduction_pipeline_with_the_live_verified_number() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    sim.get_mut(id).expect("just spawned").set_defenses(Defenses {
        armor: 20.0,
        armor_toughness: 8.0,
        ..Defenses::default()
    });
    let max_health = sim.get(id).expect("present").health();

    let outcome = sim
        .attack(id, Vec3::new(-1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
        .expect("id is live");

    assert!((outcome.damage_dealt - 3.0).abs() < 1e-4, "got {}", outcome.damage_dealt);
    assert!((outcome.health - (max_health - 3.0)).abs() < 1e-4, "got {}", outcome.health);
    assert!(!outcome.killed);
}

/// **Control**: no damage landing (the i-frame case — a weaker follow-up hit
/// inside the invulnerability window) must leave velocity completely
/// untouched. Proves the knockback branch is gated on the hit actually
/// dealing damage, not firing unconditionally.
///
/// A bare `knockback_power <= 0.0` is deliberately **not** this control any
/// more: vanilla's `LivingEntity.dealDefaultKnockback` applies a flat `0.4`
/// knockback to every damaging hit regardless of the attacker's own
/// `attack_knockback` attribute, so a non-sprinting punch —
/// `knockback_power == 0.0` in
/// `crate::server::apply_attack`'s own vocabulary — still knocks the target
/// back. See `MobSim::attack`'s own doc comment for the two-call model this
/// pins.
#[test]
fn no_damage_dealt_leaves_velocity_exactly_unchanged() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    sim.get_mut(id).expect("just spawned").set_defenses(Defenses::default());

    // First hit lands and knocks back, establishing a non-zero velocity so
    // the control below is "unchanged from something", not "unchanged from
    // zero" (which a knockback bug that merely no-ops could satisfy by
    // accident).
    sim.attack(id, Vec3::new(-1.0, 0.0, 0.0), 8.0, DamageFlags::default(), 0.0)
        .expect("id is live");
    let before = sim.get(id).expect("present").velocity();
    assert_ne!(before, Vec3::new(0.0, 0.0, 0.0), "precondition: the first hit must have knocked back");

    // Second, weaker hit inside the 20-tick i-frame window: `damage_dealt`
    // must be `0.0` (pinned separately by
    // `a_weaker_followup_hit_inside_the_iframe_window_is_ignored`), so no
    // knockback of either kind should apply.
    let outcome = sim
        .attack(id, Vec3::new(-1.0, 0.0, 0.0), 5.0, DamageFlags::default(), 0.0)
        .expect("id is live");
    assert_eq!(outcome.damage_dealt, 0.0, "precondition: the follow-up must be swallowed by i-frames");

    assert_eq!(outcome.velocity, before);
    assert_eq!(sim.get(id).expect("present").velocity(), before);
}

/// A landed hit produces the **exact** predicted velocity — hand-derived from
/// `knockback_impulse`'s own formula (`LivingEntity.knockback`), applied
/// **twice** and chained (vanilla's own `dealDefaultKnockback` then
/// `causeExtraKnockback`, two independent `LivingEntity.knockback` calls —
/// see `MobSim::attack`'s own doc comment), not re-derived intuition:
///
/// attacker at (0,0,0), target at (1,0,0), so the target-to-attacker vector
/// is `(dx, dz) = (-1, 0)`; velocity starts at zero; `knockback_resistance =
/// 0.0` (generic default); grounded; `knockback_power = 0.5` (the
/// sprint-attack bonus).
///
/// Stage 1 (`MELEE_DEFAULT_KNOCKBACK_POWER = 0.4`, vanilla's mandatory
/// per-hit knockback): `dir = normalize(-1,0) = (-1,0)`, `deltaVector = dir *
/// 0.4 = (-0.4,0,0)`. `x' = 0/2 - (-0.4) = 0.4`. `y' = min(0/2 + 0.4, 0.4) =
/// 0.4` (the grounded cap). `z' = 0/2 - 0 = 0.0`. `v1 = (0.4, 0.4, 0.0)`.
///
/// Stage 2 (the `0.5` sprint bonus, chained onto `v1`): `deltaVector = dir *
/// 0.5 = (-0.5,0,0)`. `x' = 0.4/2 - (-0.5) = 0.7`. `y' = min(0.4/2 + 0.5,
/// 0.4) = 0.4` (capped again). `z' = 0.0/2 - 0 = 0.0`. `v2 = (0.7, 0.4,
/// 0.0)`.
///
/// The target (at `+x`, relative to an attacker at the origin) is pushed
/// further in `+x` — away from the attacker, not toward it.
#[test]
fn positive_knockback_power_produces_the_exact_predicted_velocity() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(1.0, 0.0, 0.0));

    let outcome = sim
        .attack(id, Vec3::new(0.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.5)
        .expect("id is live");

    let expected = Vec3::new(0.7, 0.4, 0.0);
    assert!(
        (outcome.velocity.x - expected.x).abs() < 1e-9
            && (outcome.velocity.y - expected.y).abs() < 1e-9
            && (outcome.velocity.z - expected.z).abs() < 1e-9,
        "expected {expected:?}, got {:?}",
        outcome.velocity
    );

    // Cross-check against calling `knockback_impulse` directly, twice and
    // chained, with the same inputs — proves `MobSim::attack` is not
    // silently diverging from the primitive it claims to call.
    let after_default = knockback_impulse(
        Vec3d { x: 0.0, y: 0.0, z: 0.0 },
        true,
        0.4,
        -1.0,
        0.0,
        0.0,
        || (1.0, 0.0),
    );
    let direct = knockback_impulse(after_default, true, 0.5, -1.0, 0.0, 0.0, || (1.0, 0.0));
    assert!((outcome.velocity.x - direct.x).abs() < 1e-9);
    assert!((outcome.velocity.y - direct.y).abs() < 1e-9);
    assert!((outcome.velocity.z - direct.z).abs() < 1e-9);
}

/// A non-sprinting (`knockback_power == 0.0`) melee hit still knocks the
/// target back — the exact regression #607 reported ("mobs dont take
/// knockback if i punch them"). Only the mandatory `0.4` default fires;
/// hand-derived the same way as the sprint case above, minus stage 2.
///
/// attacker at (0,0,0), target at (1,0,0): `(dx, dz) = (-1, 0)`. `dir =
/// (-1,0)`. `deltaVector = dir * 0.4 = (-0.4,0,0)`. `x' = 0/2 - (-0.4) =
/// 0.4`. `y' = min(0/2 + 0.4, 0.4) = 0.4`. `z' = 0.0`.
#[test]
fn a_non_sprinting_hit_still_applies_the_default_knockback() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(1.0, 0.0, 0.0));

    let outcome = sim
        .attack(id, Vec3::new(0.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.0)
        .expect("id is live");

    let expected = Vec3::new(0.4, 0.4, 0.0);
    assert!(
        (outcome.velocity.x - expected.x).abs() < 1e-9
            && (outcome.velocity.y - expected.y).abs() < 1e-9
            && (outcome.velocity.z - expected.z).abs() < 1e-9,
        "expected {expected:?}, got {:?}",
        outcome.velocity
    );
}

/// A target's own `knockback_resistance` attribute fully cancels the
/// impulse — the identical "full resistance is a no-op" control
/// `lodestone-physics`' own test suite already runs for the primitive,
/// re-proven here through `MobSim::attack`'s real attribute plumbing (the
/// resistance value SimMob actually carries, not a value handed to the
/// primitive directly by the test).
#[test]
fn full_knockback_resistance_cancels_the_impulse_through_the_real_attribute() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(1.0, 0.0, 0.0));
    sim.get_mut(id).expect("just spawned").set_knockback_resistance(1.0);
    let before = sim.get(id).expect("present").velocity();

    let outcome = sim
        .attack(id, Vec3::new(0.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.5)
        .expect("id is live");

    assert_eq!(outcome.velocity, before, "full resistance must leave velocity untouched");
}

/// The invulnerability-frame gate is real: a second, weaker hit inside the
/// 20-tick window lands as `damage_dealt == 0.0` (ignored), not a second
/// full hit — the same `HurtCooldown` behaviour `lodestone-entity`'s own
/// tests pin, exercised here through the actual attack entry point.
#[test]
fn a_weaker_followup_hit_inside_the_iframe_window_is_ignored() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    // A real zombie carries nonzero base armour (`default_attributes`'
    // census) — zeroed here so the reduction pipeline is a plain
    // pass-through and the i-frame behaviour this test targets is not
    // entangled with the armour formula (that is a separate, already-pinned
    // test above).
    sim.get_mut(id).expect("just spawned").set_defenses(Defenses::default());
    let max_health = sim.get(id).expect("present").health();

    let first = sim
        .attack(id, Vec3::new(-1.0, 0.0, 0.0), 8.0, DamageFlags::default(), 0.0)
        .expect("id is live");
    assert!((first.damage_dealt - 8.0).abs() < 1e-4);

    let second = sim
        .attack(id, Vec3::new(-1.0, 0.0, 0.0), 5.0, DamageFlags::default(), 0.0)
        .expect("id is live");
    assert_eq!(second.damage_dealt, 0.0, "a weaker follow-up must be ignored inside i-frames");
    assert!((second.health - (max_health - 8.0)).abs() < 1e-4, "health must not drop again");
}

/// A killing blow removes the mob **immediately**, not deferred to the next
/// [`MobSim::tick`] — matches vanilla's own immediate death removal, and is
/// the control that proves a second attack against the same id afterward
/// finds nothing (rather than a lingering zero-health corpse still
/// attackable).
#[test]
fn a_killing_blow_removes_the_mob_immediately() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    sim.get_mut(id).expect("just spawned").set_health(2.0);
    assert_eq!(sim.len(), 1);

    let outcome = sim
        .attack(id, Vec3::new(-1.0, 0.0, 0.0), 50.0, DamageFlags::default(), 0.0)
        .expect("id is live");

    assert!(outcome.killed);
    assert_eq!(outcome.health, 0.0);
    assert_eq!(sim.len(), 0, "the killed mob must be gone from the sim immediately");
    assert!(sim.get(id).is_none());

    let followup = sim.attack(id, Vec3::new(-1.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.0);
    assert_eq!(followup, None, "attacking an already-dead id must find nothing");
}

/// Damage and knockback resolve **together** in one call — a sprinting
/// attacker's hit both hurts and knocks back in the same `attack()`, not two
/// separate steps a caller could apply out of order.
#[test]
fn one_attack_call_applies_both_damage_and_knockback() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_plain_mob(&mut sim, Vec3::new(1.0, 0.0, 0.0));
    // Zeroed for the same reason as the i-frame test above: a real zombie's
    // base armour would entangle this with the (separately pinned) armour
    // formula.
    sim.get_mut(id).expect("just spawned").set_defenses(Defenses::default());
    let max_health = sim.get(id).expect("present").health();

    let outcome = sim
        .attack(id, Vec3::new(0.0, 0.0, 0.0), 4.0, DamageFlags::default(), 0.5)
        .expect("id is live");

    assert!((outcome.damage_dealt - 4.0).abs() < 1e-4);
    assert!((outcome.health - (max_health - 4.0)).abs() < 1e-4);
    assert_ne!(outcome.velocity, Vec3::new(0.0, 0.0, 0.0), "knockback must also have landed");
}
