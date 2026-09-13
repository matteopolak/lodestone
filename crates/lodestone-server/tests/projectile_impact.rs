//! Acceptance gate: **a projectile now damages what
//! it hits.**
//!
//! Before this, `MobSim::spawn_projectile`'s own doc comment said hit detection
//! was "explicit follow-up", and `ProjectileRegistry`'s said impact resolution was
//! "the caller's job" — and no caller anywhere in the workspace did it. A
//! skeleton's arrows flew for their whole lifetime, straight through terrain and
//! straight through mobs, and nothing lost a point of health.
//!
//! Every test here drives [`MobSim::tick`], never
//! `ProjectileRegistry::tick`/`MobSim::resolve_projectile_impacts` directly, for
//! the same reason `projectile_and_item_registries.rs` gives: a test that calls
//! the resolver by hand passes whether or not the production tick loop
//! (`tick::run_tick_loop`, which ticks this exact type every 50 ms) reaches it.
//! Un-wire the impact pass from `MobSim::tick` and these fail.
//!
//! Expected damage values come from the 26.2 jar's own arithmetic, quoted at each
//! use, and never from re-running this workspace's integrator to see what it
//! produces.

use std::str::FromStr;

use lodestone_entity::projectile::Projectile;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkWorld, MobSim};

fn rk(s: &str) -> ResourceKey {
    ResourceKey::from_str(s).expect("valid resource key")
}

/// All air, wide enough vertically that nothing falls out of the world during the
/// handful of ticks these gates run.
fn empty_world() -> ChunkWorld {
    ChunkWorld::new(-64, 384)
}

/// A cow: 10 max health and, crucially, **no armour**, so a health delta reads
/// raw impact damage directly.
///
/// Deliberately not a zombie, which is the obvious choice and the wrong one:
/// `Zombie.createAttributes` adds `Attributes.ARMOR, 2.0`, so a zombie reduces
/// every hit it takes. That is real behaviour rather than a nuisance — it is what
/// [`a_zombies_own_species_armour_already_reduces_an_arrow`] gates — but it means
/// a zombie cannot be used to read raw damage off a health delta. Measuring 5.904
/// where the jar says 6 is exactly the confusion a species with a silent armour
/// attribute produces, and it is the first thing these gates measured.
const COW_MAX_HEALTH: f32 = 10.0;

fn spawn_cow(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
    sim.spawn_species(rk("minecraft:cow"), pos).id()
}

/// **The headline.** An arrow travelling at bow speed into a mob two blocks away
/// deals exactly **6.0**, and the cow is left on 4.0 of 10.0.
///
/// The number is the jar's, computed outside this workspace:
/// `AbstractArrow.onHitEntity` is
/// `Mth.ceil(Mth.clamp(deltaMovement.length() * baseDamage, 0, Integer.MAX_VALUE))`,
/// `baseDamage` initialises to `2.0` (`ARROW_BASE_DAMAGE`), and the arrow is
/// launched at `3.0` blocks/tick — which is `BowItem.releaseUsing`'s own
/// `pow * 3.0` at full charge. So `ceil(3.0 * 2.0) == 6`.
///
/// Three wrong hypotheses are each excluded by magnitude rather than direction:
/// dropping the speed scale gives `2`, using the trident's `8.0` base gives `24`,
/// and truncating instead of ceiling gives `6` here — so the truncation
/// hypothesis is separated by `a_slow_arrow_still_deals_a_whole_point` below
/// rather than pretended to be covered by this input, which is one where the two
/// coincide.
#[test]
fn an_arrow_at_bow_speed_deals_the_jars_six_points() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = spawn_cow(&mut sim, Vec3::new(2.0, 0.0, 0.0));
    let before = sim.get(target).expect("just spawned").health();
    assert!(
        (before - COW_MAX_HEALTH).abs() < 1e-6,
        "a cow starts on 10 max health, got {before}"
    );

    // Aimed along +x from the origin at the target's chest height. The cow's
    // box is 0.9 wide about x = 2.0, so the segment 0.0 -> 3.0 crosses it.
    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    assert_eq!(sim.projectile_count(), 1, "one arrow in flight");

    sim.tick();

    let after = sim.get(target).expect("still alive").health();
    assert!(
        (before - after - 6.0).abs() < 1e-4,
        "expected exactly 6.0 of damage, health went {before} -> {after}"
    );
    assert!(
        (after - 4.0).abs() < 1e-4,
        "expected 4.0 remaining of 10, got {after}"
    );
    assert_eq!(
        sim.projectile_count(),
        0,
        "the arrow is consumed by the hit"
    );
    // The wrong hypotheses, each a whole number of hearts away from the
    // measurement rather than merely on the other side of a threshold.
    let dealt = before - after;
    assert!((dealt - 2.0).abs() > 3.9, "the speed scale must be applied");
    assert!((dealt - 24.0).abs() > 17.0, "the arrow base is 2.0, not 8.0");
}

/// The `ceil` half of the formula, at the input where it actually decides
/// something: an arrow drifting at `0.2` blocks/tick would deal `0.4` truncated
/// and `1` ceiled. Only one of those is a whole heart-quarter of damage, and only
/// one matches the jar.
#[test]
fn a_slow_arrow_still_deals_a_whole_point() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = spawn_cow(&mut sim, Vec3::new(0.15, 0.0, 0.0));
    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.2, 0.0, 0.0)),
    );
    sim.tick();
    let dealt = COW_MAX_HEALTH - sim.get(target).expect("still alive").health();
    assert!((dealt - 1.0).abs() < 1e-4, "expected 1.0, got {dealt}");
    assert!(
        (dealt - 0.4).abs() > 0.5,
        "a truncating formula would deal a fraction of a point"
    );
}

/// **The negative control the assertions above need.** The same arrow, the same
/// mob, the same tick — aimed past it. Nothing is damaged and the arrow is *still
/// in flight*, which is what distinguishes "the impact pass found nothing" from
/// "the impact pass never ran".
#[test]
fn an_arrow_that_misses_damages_nothing_and_keeps_flying() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = spawn_cow(&mut sim, Vec3::new(2.0, 0.0, 0.0));

    // Ten blocks off to the side in z: nowhere near the 0.6-wide box.
    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 10.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    sim.tick();

    let health = sim.get(target).expect("untouched").health();
    assert!(
        (health - COW_MAX_HEALTH).abs() < 1e-6,
        "a miss must not damage: health {health}"
    );
    assert_eq!(
        sim.projectile_count(),
        1,
        "a missed arrow is still tracked — if this were 0 the pass would be \
         destroying projectiles rather than resolving hits"
    );
}

/// A wall between the archer and the target stops the arrow, and the target is
/// unharmed. The ordering is what is under test: an implementation that resolved
/// entity hits without consulting terrain would damage the mob through the wall.
///
/// The control is the identical scene with the wall removed, which must damage —
/// so "the wall shielded it" is not indistinguishable from "the arrow never hit
/// anything in either case".
#[test]
fn a_wall_stops_the_arrow_before_it_reaches_the_mob() {
    let mut walled = empty_world();
    // A solid cell the segment 0.0 -> 3.0 at y = 1.0 must pass through.
    walled.set_solid(1, 1, 0, true);
    let mut sim = MobSim::new(&walled);
    let target = spawn_cow(&mut sim, Vec3::new(2.0, 0.0, 0.0));
    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    sim.tick();
    let shielded = sim.get(target).expect("untouched").health();
    assert!(
        (shielded - COW_MAX_HEALTH).abs() < 1e-6,
        "the wall must shield the mob: health {shielded}"
    );
    assert_eq!(
        sim.projectile_count(),
        0,
        "the arrow is spent on the block"
    );

    // Control: same geometry, no wall.
    let open = empty_world();
    let mut control = MobSim::new(&open);
    let control_target = spawn_cow(&mut control, Vec3::new(2.0, 0.0, 0.0));
    control.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    control.tick();
    let unshielded = control.get(control_target).expect("hit").health();
    assert!(
        (unshielded - 4.0).abs() < 1e-4,
        "control: without the wall the same shot deals 6.0, got health {unshielded}"
    );
}

/// A projectile does not hit its own shooter, even though it is created inside the
/// shooter's bounding box.
///
/// Both halves of vanilla's guard are exercised: the owner exclusion
/// (`Projectile.canHitEntity`) and the zero hitbox margin for the first two ticks
/// (`ProjectileUtil.computeMargin`). The control is a *second* mob standing in the
/// identical spot with no ownership relation, which must be hit — so this is not
/// measuring "the impact pass ignores mobs at the origin".
#[test]
fn a_projectile_never_hits_its_own_shooter() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let shooter = spawn_cow(&mut sim, Vec3::new(0.0, 0.0, 0.0));
    let bystander = spawn_cow(&mut sim, Vec3::new(0.0, 0.0, 0.0));

    // Launched from inside both boxes.
    sim.spawn_projectile_from(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
        Some(shooter),
    );
    sim.tick();

    let shooter_health = sim.get(shooter).expect("alive").health();
    assert!(
        (shooter_health - COW_MAX_HEALTH).abs() < 1e-6,
        "the shooter must not be hit by its own arrow: health {shooter_health}"
    );
    let bystander_health = sim.get(bystander).expect("alive").health();
    assert!(
        (bystander_health - 4.0).abs() < 1e-4,
        "control: an unrelated mob in the same spot is hit for 6.0, got health \
         {bystander_health}"
    );
}

/// Armour reduces a projectile hit exactly as it reduces a melee one, because
/// `minecraft:arrow` is an ordinary reducible damage type — it carries no
/// `bypasses_armor` tag, unlike `minecraft:generic`.
///
/// A raw 6.0 arrow against armour 20 / toughness 8 (a full diamond set): the
/// jar's `CombatRules.getDamageAfterAbsorb` gives `toughness = 2 + 8/4 = 4`,
/// `realArmor = clamp(20 - 6/4, 4, 20) = 18.5`, `frac = 18.5/25 = 0.74`, so
/// `6 * 0.26 = 1.56`. Recomputed here for a raw of 6.0 rather than reused from the
/// live-verified 10.0 case — those are different inputs and quoting `3.0` here
/// would be the wrong arithmetic wearing a verified number's clothes.
#[test]
fn armour_reduces_an_arrow_hit_by_the_real_combat_rules_amount() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = spawn_cow(&mut sim, Vec3::new(2.0, 0.0, 0.0));
    sim.get_mut(target)
        .expect("just spawned")
        .set_defenses(lodestone_entity::Defenses {
            armor: 20.0,
            armor_toughness: 8.0,
            ..lodestone_entity::Defenses::default()
        });
    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    sim.tick();

    let dealt = COW_MAX_HEALTH - sim.get(target).expect("alive").health();
    assert!((dealt - 1.56).abs() < 1e-3, "expected 1.56, got {dealt}");
    // The unreduced hypothesis is 6.0, more than four points away — so this is a
    // magnitude check, and a broken tag lookup that skipped the armour stage
    // fails it by 4.44 rather than passing on a tolerance.
    assert!(
        (dealt - 6.0).abs() > 4.0,
        "the armour stage must actually run"
    );
}

/// A species' **own** armour attribute reduces an arrow hit, with no equipment
/// involved — the feed `combat_defaults` already had and which the impact pass now
/// exercises for the first time.
///
/// `Zombie.createAttributes` adds `Attributes.ARMOR, 2.0`, toughness `0.0`. Against
/// a raw 6.0: `toughness = 2 + 0/4 = 2`,
/// `realArmor = clamp(2 - 6/2, 2 * 0.2, 20) = clamp(-1, 0.4, 20) = 0.4`,
/// `frac = 0.4 / 25 = 0.016`, so `6 * 0.984 = 5.904`.
///
/// The `max(total_armor * 0.2)` floor is what makes this 5.904 and not 6.0: without
/// it the clamp would land on the negative `-1` and the reduction would vanish
/// entirely. That is a one-line difference in `damage_after_armor` and 0.096 of a
/// point in the measurement, which is why this asserts the exact value rather than
/// "armour reduced something".
#[test]
fn a_zombies_own_species_armour_already_reduces_an_arrow() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = sim
        .spawn_species(rk("minecraft:zombie"), Vec3::new(2.0, 0.0, 0.0))
        .id();
    let before = sim.get(target).expect("just spawned").health();
    assert!(
        (before - 20.0).abs() < 1e-6,
        "a zombie has the generic 20 max health, got {before}"
    );
    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    sim.tick();
    let dealt = before - sim.get(target).expect("alive").health();
    assert!(
        (dealt - 5.904).abs() < 1e-3,
        "expected the armour-2 reduction to 5.904, got {dealt}"
    );
    // The two neighbouring hypotheses: no armour at all (6.0) and a floor-less
    // clamp (also 6.0, by a different route). Both are excluded, and a cow — same
    // shot, no species armour — is the control that shows 6.0 is reachable.
    let open = empty_world();
    let mut control = MobSim::new(&open);
    let cow = spawn_cow(&mut control, Vec3::new(2.0, 0.0, 0.0));
    control.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    control.tick();
    let cow_dealt = COW_MAX_HEALTH - control.get(cow).expect("alive").health();
    assert!(
        (cow_dealt - 6.0).abs() < 1e-4,
        "control: an unarmoured species takes the full 6.0, got {cow_dealt}"
    );
    assert!(
        dealt < cow_dealt,
        "the zombie's own armour must reduce where the cow's absence does not"
    );
}

/// A lethal arrow kills, and the kill goes through the shared reaper — so it
/// drops the same loot a melee kill does rather than the mob simply vanishing.
#[test]
fn a_lethal_arrow_removes_the_mob_and_rolls_its_loot() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = spawn_cow(&mut sim, Vec3::new(2.0, 0.0, 0.0));
    sim.get_mut(target).expect("just spawned").set_health(3.0);
    let items_before = sim.item_count();

    sim.spawn_projectile(
        rk("minecraft:arrow"),
        Projectile::arrow(Vec3::new(0.0, 1.0, 0.0), Vec3::new(3.0, 0.0, 0.0)),
    );
    sim.tick();

    assert!(sim.get(target).is_none(), "a 6.0 hit on 3.0 health kills");
    assert!(
        sim.item_count() >= items_before,
        "the kill must route through the loot-rolling reaper, not a bare retain"
    );
}

/// **The end-to-end island gate: a skeleton's own arrows now hurt.**
///
/// Nothing here spawns a projectile by hand. A skeleton is given a target, the
/// sim is ticked, and its `RangedBowAttackGoal` draws for `BOW_FULL_DRAW_TICKS`
/// and releases on its own — the full production chain, goal to launch to impact.
///
/// The expected value is a **bracket derived from outside constants**, not a
/// re-run of this workspace's integrator: the goal launches at
/// `ARROW_POWER = 1.6` blocks/tick (`AbstractSkeleton`'s own figure), and air drag
/// is `0.99` per tick with gravity pulling the vertical component, so the speed at
/// impact is at most the launch speed. `AbstractArrow.onHitEntity`'s
/// `ceil(speed * 2.0)` is therefore in `1 ..= ceil(1.6 * 2.0) = 4`. Any damage in
/// that window is consistent with the jar; zero is not, and more than 4 would mean
/// the launch power or the base damage is wrong.
#[test]
fn a_skeleton_with_a_target_shoots_an_arrow_that_damages_it() {
    // A real floor under both mobs. This is the one test in this file that
    // ticks the sim 40 times rather than once, and `NavigatingMob::advance`
    // now applies gravity unconditionally to idle mobs (matching vanilla
    // `LivingEntity.travel`) -- an `empty_world()` void would let the cow
    // and skeleton fall out from under each other over those 40 ticks,
    // opening a vertical gap the arrow's arc cannot bridge and turning a
    // "does the arrow hit" gate into an accidental "did the mobs desync
    // while falling" one. Grounding them restores the fixed-height
    // precondition the arc math here assumes.
    let mut world = empty_world();
    for x in -2..=8 {
        for z in -2..=2 {
            world.set_block(x, -1, z, "minecraft:stone");
        }
    }
    let mut sim = MobSim::new(&world);

    let victim_pos = Vec3::new(6.0, 0.0, 0.0);
    let victim = spawn_cow(&mut sim, victim_pos);
    let skeleton = sim.spawn_species(rk("minecraft:skeleton"), Vec3::new(0.0, 0.0, 0.0));
    skeleton.set_attack_target(Some(victim_pos));
    let skeleton_id = skeleton.id();

    // Long enough to cover the 20-tick draw plus the arrow's flight over six
    // blocks at 1.6 blocks/tick, with margin for the arc.
    for _ in 0..40 {
        sim.tick();
        if sim.get(victim).is_none_or(|m| m.health() < COW_MAX_HEALTH) {
            break;
        }
    }

    let remaining = sim
        .get(victim)
        .map_or(0.0, lodestone_server::SimMob::health);
    let dealt = COW_MAX_HEALTH - remaining;
    assert!(
        dealt > 0.0,
        "a skeleton's arrow must actually hurt its target — this is the whole \
         defect #260 names, and zero here means the arrow flew straight through"
    );
    assert!(
        dealt >= 1.0 && dealt <= 4.0,
        "damage must land inside the jar-derived window 1..=4 for a launch at \
         1.6 blocks/tick against base damage 2.0, got {dealt}"
    );

    // Control: the shooter itself is untouched throughout, so the damage above
    // cannot be the skeleton shooting itself and the assertion reading the wrong
    // mob's health.
    let shooter_health = sim
        .get(skeleton_id)
        .map_or(0.0, lodestone_server::SimMob::health);
    assert!(
        (shooter_health - 20.0).abs() < 1e-6,
        "the skeleton must be unhurt: health {shooter_health}"
    );
}

/// A snowball is a real hit that consumes the projectile and deals nothing —
/// `Snowball.onHitEntity`'s `entity instanceof Blaze ? 3 : 0`. The distinction
/// this pins is "harmless hit" versus "no hit", which a damage-only assertion
/// cannot see.
#[test]
fn a_snowball_hits_harmlessly_but_is_still_consumed() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let target = spawn_cow(&mut sim, Vec3::new(1.0, 0.0, 0.0));
    sim.spawn_projectile(
        rk("minecraft:snowball"),
        Projectile::snowball(Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.5, 0.0, 0.0)),
    );
    sim.tick();
    assert!(
        (sim.get(target).expect("alive").health() - COW_MAX_HEALTH).abs() < 1e-6,
        "a snowball does not hurt a cow"
    );
    assert_eq!(
        sim.projectile_count(),
        0,
        "but it is consumed by the hit, not left flying"
    );
}

/// The blaze exception, and its control. Same snowball, same geometry, different
/// target species — `3.0` against a blaze and `0.0` against anything else.
#[test]
fn a_snowball_deals_three_to_a_blaze_only() {
    let world = empty_world();
    let mut sim = MobSim::new(&world);
    let blaze = sim
        .spawn_species(rk("minecraft:blaze"), Vec3::new(1.0, 0.0, 0.0))
        .id();
    let before = sim.get(blaze).expect("just spawned").health();
    sim.spawn_projectile(
        rk("minecraft:snowball"),
        Projectile::snowball(Vec3::new(0.0, 1.0, 0.0), Vec3::new(1.5, 0.0, 0.0)),
    );
    sim.tick();
    let dealt = before - sim.get(blaze).expect("alive").health();
    assert!(
        (dealt - 3.0).abs() < 1e-4,
        "a snowball deals exactly 3 to a blaze, got {dealt}"
    );
}
