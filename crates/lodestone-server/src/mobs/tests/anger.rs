use super::*;

/// The grudge window in ticks, stated **independently of [`ANGER_TICKS`]**.
/// Twenty-to-thirty-nine seconds at 20 ticks per second yields the
/// inclusive range `[400, 780]`.
///
/// These literals are load-bearing: reading the seconds as ticks would
/// produce `[20, 39]`, and deriving them from `ANGER_TICKS` would allow a
/// bad duration to move both the implementation and its expectation.
const JAR_LO: u64 = 400;
const JAR_HI: u64 = 780;

fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

/// Spawns one mob through [`MobSim::spawn_species`], hits it once, and
/// reports the tick offset at which `angry_target` first reads `None`.
///
/// Drives `MobSim` through its normal perception path, keeping the
/// AI-goal and spawn-category behavior under test.
///
/// The attacker position is placed well outside `flat_world`'s solid `±8`
/// platform. The attacker remains outside the walkable platform, so the bee
/// cannot path to it or clear `anger` through an attack. This function
/// measures **grudge duration**, not "does the
/// mob's own combat ever run" — an attacker outside the walkable platform
/// (so no path exists to it, for any of the four species' plausible
/// speeds) decouples the two.
fn ticks_until_anger_clears(species: &str, limit: u64) -> Option<u64> {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    let attacker = Vec3::new(128.0, 0.0, 0.0);
    sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
        .expect("the mob must still be alive to hold a grudge");

    // One tick to run the feed, then poll.
    for elapsed in 0..limit {
        sim.tick();
        if sim.get(id).expect("alive").mob.angry_target().is_none() {
            return Some(elapsed);
        }
    }
    None
}

/// **The gate.** A grudge must expire inside the measured `[400, 780]`
/// tick window. Twenty-to-thirty-nine seconds at 20 ticks per second
/// yields this range; treating the seconds as ticks would yield `[20, 39]`.
///
/// Predicting only "it eventually expires" is satisfied by both hypotheses
/// and by an off-by-one on the inclusive upper bound. Both bounds are
/// asserted so the inclusive interval is checked directly.
#[test]
fn anger_expires_inside_the_jars_tick_window() {
    let (lo, hi) = (JAR_LO, JAR_HI);
    // Generous headroom over `hi`, so "never expired" is distinguishable
    // from "expired late" rather than both timing out.
    let limit = hi * 2;

    for species in ["wolf", "bee", "enderman", "zombified_piglin"] {
        let elapsed = ticks_until_anger_clears(species, limit).unwrap_or_else(|| {
            panic!("{species}'s grudge never expired within {limit} ticks")
        });
        assert!(
            elapsed >= lo,
            "{species}'s grudge expired after {elapsed} ticks, before the jar's \
             minimum of {lo}. A value in [20, 39] means rangeOfSeconds(20, 39) \
             was read as seconds; it already returns ticks"
        );
        assert!(
            elapsed <= hi,
            "{species}'s grudge lasted {elapsed} ticks, past the jar's maximum \
             of {hi}"
        );
    }
}

/// The grudge must be **live** immediately after the hit, and must name the
/// attacker's position — not merely be non-`None` at some later point.
///
/// Control for the test above: without this, a mob whose anger was never
/// set at all would "expire" at tick 0 and only the lower-bound assertion
/// would catch it, for the wrong reason.
#[test]
fn a_hit_starts_a_grudge_naming_the_attacker() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    assert_eq!(
        sim.get(id).expect("alive").mob.angry_target(),
        None,
        "an unprovoked neutral mob must hold no grudge — if this is Some, \
         every neutral species is hostile on sight"
    );

    let attacker = Vec3::new(3.0, 0.0, 4.0);
    sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
        .expect("alive");
    sim.tick();

    assert_eq!(
        sim.get(id).expect("alive").mob.angry_target(),
        Some(attacker),
        "the grudge must name where the attacker was"
    );
}

/// The deadline is **absolute**, so a grudge refreshed by a second hit
/// extends from the *new* tick rather than from the first.
///
/// This is the assertion a decrementing counter passes only by accident:
/// it pins that the stored value is compared against `tick_count` rather
/// than decremented, by advancing the clock a long way between two hits and
/// requiring the grudge to outlive the first deadline's worst case.
#[test]
fn a_second_hit_extends_the_deadline_from_the_new_tick() {
    let (lo, hi) = (JAR_LO, JAR_HI);
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    let attacker = Vec3::new(1.0, 0.0, 0.0);
    sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
        .expect("alive");
    // Advance well past the first grudge's *minimum* but not its maximum,
    // then hit again.
    for _ in 0..lo {
        sim.tick();
    }
    sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
        .expect("alive");

    // The refreshed grudge must still be live `lo` ticks later, which the
    // first grudge could not guarantee: its worst case was `hi`, and we are
    // now at `lo + lo = 800 > hi`.
    for _ in 0..lo {
        sim.tick();
    }
    assert!(
        lo + lo > hi,
        "this test's arithmetic assumes 2*{lo} exceeds {hi}; if the window \
         changed, the schedule below no longer proves anything"
    );
    assert_eq!(
        sim.get(id).expect("alive").mob.angry_target(),
        Some(attacker),
        "the second hit must extend the deadline from the tick it landed on; \
         a grudge that has already expired here means the deadline was not \
         recomputed against the current clock"
    );
}

/// Vanilla's own zombified-piglin alert-interval's own `[80, 120]` window, stated
/// independently of [`PIGLIN_ALERT_INTERVAL_TICKS`] for the same reason
/// [`JAR_LO`]/[`JAR_HI`] are stated independently of [`ANGER_TICKS`]
/// above: a magnitude check that read the expectation off the constant
/// under test would pass even if the constant itself were wrong. Drawn
/// from a real spawned mob's own RNG stream (never a hand-rolled double),
/// the same [`MobController`] seam production reads.
#[test]
fn piglin_alert_interval_rolls_inside_the_jars_80_to_120_window() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:zombified_piglin").expect("valid key");
    let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

    for _ in 0..1000 {
        let draw = piglin_alert_interval(&mut sim.get_mut(id).expect("alive").mob);
        assert!(
            (80..=120).contains(&draw),
            "piglin_alert_interval drew {draw}, outside the jar's [80, 120] \
             ALERT_INTERVAL window"
        );
    }
}

/// The ongoing piglin group-alert mechanism is isolated from the one-shot
/// owner-group propagation it accompanies.
///
/// The one-shot arm fires when [`MobSim::attack`] creates a new grudge.
/// The neighbour is spawned after that event, so it cannot receive the
/// one-shot notification. An ensuing grudge therefore demonstrates the
/// periodic alert timer rather than the immediate group notification.
#[test]
fn a_piglin_holding_a_target_alerts_a_neighbour_that_did_not_exist_for_the_one_shot_alert() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let key = ResourceKey::from_str("minecraft:zombified_piglin").expect("valid key");

    let alerting = sim.spawn_species(key.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
    let attacker = Vec3::new(3.0, 0.0, 4.0);
    sim.attack(alerting, attacker, 1.0, DamageFlags::default(), 0.0)
        .expect("alive");

    // The neighbour did not exist for the hit above, so the immediate
    // group census could not have reached it.
    let neighbour = sim.spawn_species(key, Vec3::new(5.0, 0.0, 0.0)).id();
    assert_eq!(
        sim.get(neighbour).expect("alive").mob.angry_target(),
        None,
        "precondition: a freshly spawned neighbour must start with no grudge"
    );

    // 120 ticks covers the interval's own worst case
    // (`PIGLIN_ALERT_INTERVAL_TICKS`'s upper bound), plus headroom for the
    // alerting piglin's anger-gated target row to turn its grudge into a
    // real `attack_target` (what `piglin_alert_ticks` reads to decide it
    // has something to alert about).
    for _ in 0..200 {
        sim.tick();
        if sim
            .get(neighbour)
            .expect("alive")
            .mob
            .angry_target()
            .is_some()
        {
            return;
        }
    }
    panic!(
        "the neighbour was never alerted within 200 ticks — the ongoing \
         maybeAlertOthers mechanism has no live producer"
    );
}
