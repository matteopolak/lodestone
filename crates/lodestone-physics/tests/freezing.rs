//! The powder-snow freezing mechanic (`Entity.DATA_TICKS_FROZEN` /
//! `InsideBlockEffectType.FREEZE` / `LivingEntity.aiStep`'s freezing block).
//! Issue #212.
//!
//! Vanilla's rule (`InsideBlockEffectType.java:6-11`,
//! `LivingEntity.java:3139-3151`):
//!
//! ```text
//! every tick the swept segment finds powder snow (checkInsideBlocks):
//!   setIsInPowderSnow(true);
//!   ticksFrozen = min(ticksRequiredToFreeze, ticksFrozen + 1);   // 140 cap
//!
//! end of tick, unconditionally:
//!   if (!isInPowderSnow) ticksFrozen = max(0, ticksFrozen - 2);
//!
//! every 40th tick:
//!   if (isFullyFrozen()) hurt(FREEZE, 1.0F);
//! ```
//!
//! These tests predict the exact tick counts rather than asserting a direction
//! of change (CLAUDE.md's *magnitude* species), and include the control that
//! proves freezing is **not** gated on `flying` the way the stuck-multiplier
//! drag is — `PlayerState::frozen_ticks` doc explains why.

use lodestone_physics::{
    Aabb, CollisionView, HorizontalDir, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick,
};

/// A world that is powder snow in a fixed y-range (covering `0..=3` in x/z) and
/// nothing else — no collision, so a player standing anywhere in it neither
/// falls onto solid ground nor is blocked.
struct PowderSnowColumn {
    ys: std::ops::RangeInclusive<i32>,
}

impl CollisionView for PowderSnowColumn {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}

    fn is_powder_snow(&self, x: i32, y: i32, z: i32) -> bool {
        (0..=3).contains(&x) && (0..=3).contains(&z) && self.ys.contains(&y)
    }

    // Real powder snow also reports a stuck multiplier — included so the
    // flying-vs-not control below exercises the actual two-callback shape
    // `PowderSnowBlock.entityInside` has, not a simplified stand-in.
    fn stuck_multiplier(&self, x: i32, y: i32, z: i32) -> Option<Vec3d> {
        if self.is_powder_snow(x, y, z) {
            Some(Vec3d::new(0.9, 1.5, 0.9))
        } else {
            None
        }
    }

    fn is_solid_face(&self, _x: i32, _y: i32, _z: i32, _dir: HorizontalDir, _kind: lodestone_physics::FluidKind) -> bool {
        false
    }
}

/// An always-empty world, for the "not in powder snow" decay-only case.
struct Empty;
impl CollisionView for Empty {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

fn standing_in_powder_snow() -> PlayerState {
    // Feet at y=1: box 1..2.8 spans cells y=1 and y=2, both inside the powder
    // snow column.
    PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0)
}

#[test]
fn standing_in_powder_snow_accumulates_one_tick_at_a_time() {
    let world = PowderSnowColumn { ys: 0..=3 };
    let profile = PhysicsProfile::mc_1_21();
    let mut s = standing_in_powder_snow();

    for expected in 1..=10u32 {
        tick(&mut s, MovementInput::NONE, &world, &profile);
        assert_eq!(
            s.frozen_ticks, expected,
            "frozen_ticks must climb by exactly 1 per tick in powder snow"
        );
    }
}

#[test]
fn frozen_ticks_is_capped_at_the_required_total() {
    let world = PowderSnowColumn { ys: 0..=3 };
    let profile = PhysicsProfile::mc_1_21();
    let mut s = standing_in_powder_snow();
    s.frozen_ticks = PlayerState::TICKS_REQUIRED_TO_FREEZE - 1;

    tick(&mut s, MovementInput::NONE, &world, &profile);
    assert_eq!(s.frozen_ticks, PlayerState::TICKS_REQUIRED_TO_FREEZE);
    assert!(s.is_fully_frozen());

    // One more tick must not overflow past the cap.
    tick(&mut s, MovementInput::NONE, &world, &profile);
    assert_eq!(
        s.frozen_ticks,
        PlayerState::TICKS_REQUIRED_TO_FREEZE,
        "the cap must hold, not keep climbing"
    );
}

#[test]
fn leaving_powder_snow_decays_by_two_per_tick_floored_at_zero() {
    let empty = Empty;
    let profile = PhysicsProfile::mc_1_21();
    let mut s = PlayerState::at(Vec3d::new(0.5, 50.0, 0.5), 0.0);
    s.frozen_ticks = 9;

    tick(&mut s, MovementInput::NONE, &empty, &profile);
    assert_eq!(s.frozen_ticks, 7);
    tick(&mut s, MovementInput::NONE, &empty, &profile);
    assert_eq!(s.frozen_ticks, 5);
    tick(&mut s, MovementInput::NONE, &empty, &profile);
    assert_eq!(s.frozen_ticks, 3);
    tick(&mut s, MovementInput::NONE, &empty, &profile);
    assert_eq!(s.frozen_ticks, 1);
    // Odd starting count: floors at 0, does not go negative.
    tick(&mut s, MovementInput::NONE, &empty, &profile);
    assert_eq!(s.frozen_ticks, 0);
    tick(&mut s, MovementInput::NONE, &empty, &profile);
    assert_eq!(s.frozen_ticks, 0, "must floor at zero, not wrap or go negative");
}

#[test]
fn should_apply_freeze_damage_fires_only_fully_frozen_on_the_40th_tick() {
    let mut s = PlayerState::at(Vec3d::new(0.5, 50.0, 0.5), 0.0);
    s.frozen_ticks = PlayerState::TICKS_REQUIRED_TO_FREEZE;
    assert!(s.should_apply_freeze_damage(0));
    assert!(s.should_apply_freeze_damage(40));
    assert!(s.should_apply_freeze_damage(400));
    assert!(!s.should_apply_freeze_damage(1), "not on an off-cadence tick");
    assert!(!s.should_apply_freeze_damage(39));

    // Control: the cadence is right but not fully frozen -> never fires.
    s.frozen_ticks = PlayerState::TICKS_REQUIRED_TO_FREEZE - 1;
    assert!(
        !s.should_apply_freeze_damage(40),
        "control: 139/140 ticks is not fully frozen"
    );
}

#[test]
fn freezing_is_not_suppressed_by_flying_even_though_the_stuck_drag_is() {
    // Two competing hypotheses:
    //   A (wrong): freezing is gated on `!flying` the same way the
    //      stuck-multiplier slowdown is (`Player.makeStuckInBlock`'s override) —
    //      predicts frozen_ticks == 0 for the flying player after standing in
    //      powder snow.
    //   B (right, per `InsideBlockEffectType.FREEZE` carrying no flying
    //      conjunct): frozen_ticks climbs identically whether flying or not.
    // Measured, not just signed: both players must show the *same* frozen_ticks
    // count after the same number of ticks, while their stuck_speed_multiplier
    // differs — proving this isn't a case of the flying gate having failed to
    // apply at all.
    let world = PowderSnowColumn { ys: 0..=3 };
    let profile = PhysicsProfile::mc_1_21();

    let mut grounded = standing_in_powder_snow();
    let mut flying = standing_in_powder_snow().with_flight(true, 0.05);

    for _ in 0..5 {
        tick(&mut grounded, MovementInput::NONE, &world, &profile);
        tick(&mut flying, MovementInput::NONE, &world, &profile);
    }

    assert_eq!(
        grounded.frozen_ticks, 5,
        "sanity: the grounded player accumulates normally"
    );
    assert_eq!(
        flying.frozen_ticks, 5,
        "hypothesis B: flying must NOT stop freezing accumulation, got {} \
         (hypothesis A would predict 0)",
        flying.frozen_ticks
    );

    // The control half: the *stuck* slowdown genuinely is suppressed while
    // flying, so this is not a case where the flying gate is simply inert.
    assert_eq!(
        grounded.stuck_speed_multiplier,
        Vec3d::new(0.9, 1.5, 0.9),
        "control: the grounded player IS grabbed by the powder-snow drag"
    );
    assert_eq!(
        flying.stuck_speed_multiplier,
        Vec3d::ZERO,
        "control: the flying player must NOT be grabbed by the drag — proves \
         the flying gate is real and only freezing is exempt from it"
    );
}

#[test]
fn percent_frozen_is_the_ratio_to_the_required_total() {
    let mut s = PlayerState::at(Vec3d::new(0.5, 50.0, 0.5), 0.0);
    s.frozen_ticks = 35; // 35 / 140 = 0.25
    assert!((s.percent_frozen() - 0.25).abs() < 1e-6);
    s.frozen_ticks = 0;
    assert_eq!(s.percent_frozen(), 0.0);
    s.frozen_ticks = PlayerState::TICKS_REQUIRED_TO_FREEZE;
    assert_eq!(s.percent_frozen(), 1.0);
}
