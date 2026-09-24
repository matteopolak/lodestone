use super::*;

/// A real floor near the origin — see `leash_tests::flat_world`'s own
/// doc comment for why a bare void `ChunkWorld` stopped being safe once
/// idle mobs fall. This module's `x = 60` position is a *player*
/// (`set_players`), not a spawned mob, so it is unaffected either way
/// and is deliberately left outside the floor.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn player_at(uuid: Uuid, pos: Vec3) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid, entity_id: 99 }),
        perception: PlayerPerception {
            position: pos,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

/// `(tickCount + getId()) % 1200 == 0`, with `tick_count` standing in for
/// vanilla's own generic tick-count field (see [`ELDER_GUARDIAN_EFFECT_INTERVAL`]'s own doc).
/// A freshly spawned elder guardian gets id `1`, so the trigger tick is
/// `1200 - 1 = 1199`; [`MobSim::tick`] reads `self.tick_count` *before*
/// incrementing it, so seeding `set_tick_count(1199)` and ticking once is
/// the tick this pulse fires on.
#[test]
fn a_player_within_fifty_blocks_is_pulsed_on_the_interval_tick() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let guardian_id = sim
        .spawn_species(
            "minecraft:elder_guardian".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();
    assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

    let alice = Uuid::from_u128(0xA11CE);
    sim.set_players(vec![player_at(alice, Vec3::new(40.0, 0.0, 0.0))]);
    sim.set_tick_count(1199);
    sim.tick();

    let pulses = sim.take_mining_fatigue_auras();
    assert_eq!(
        pulses,
        vec![MiningFatigueAura {
            target: PlayerIdentity {
                uuid: alice,
                entity_id: 99
            }
        }],
        "a player 40 blocks away (within the 50-block radius) must be pulsed on tick 1199, got {pulses:?}"
    );
}

/// The same setup, moved just past `EFFECT_RADIUS` — the spherical
/// distance cut, not a box.
#[test]
fn a_player_beyond_fifty_blocks_is_not_pulsed() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let guardian_id = sim
        .spawn_species(
            "minecraft:elder_guardian".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();
    assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

    let alice = Uuid::from_u128(0xA11CE);
    sim.set_players(vec![player_at(alice, Vec3::new(60.0, 0.0, 0.0))]);
    sim.set_tick_count(1199);
    sim.tick();

    assert!(
        sim.take_mining_fatigue_auras().is_empty(),
        "a player 60 blocks away is outside EFFECT_RADIUS and must not be pulsed"
    );
}

/// A tick that is not a multiple of the 1200-tick interval must pulse
/// nobody, even with a player standing on top of the guardian.
#[test]
fn no_pulse_off_the_interval_tick() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.spawn_species(
        "minecraft:elder_guardian".parse().expect("valid key"),
        Vec3::new(0.0, 0.0, 0.0),
    );

    let alice = Uuid::from_u128(0xA11CE);
    sim.set_players(vec![player_at(alice, Vec3::new(0.0, 0.0, 0.0))]);
    sim.set_tick_count(1198);
    sim.tick();

    assert!(
        sim.take_mining_fatigue_auras().is_empty(),
        "one tick before the interval must not fire"
    );
}

/// An ordinary guardian (not elder) must never pulse — the aura is
/// elder-guardian-only in vanilla; the ordinary guardian's own AI step has no
/// such call.
#[test]
fn an_ordinary_guardian_never_pulses() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let guardian_id = sim
        .spawn_species(
            "minecraft:guardian".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();
    assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

    let alice = Uuid::from_u128(0xA11CE);
    sim.set_players(vec![player_at(alice, Vec3::new(0.0, 0.0, 0.0))]);
    sim.set_tick_count(1199);
    sim.tick();

    assert!(
        sim.take_mining_fatigue_auras().is_empty(),
        "an ordinary guardian must never emit a mining-fatigue pulse"
    );
}

/// The magnitude gate: the constants a driver applies must match
/// vanilla's own elder-guardian effect-duration/effect-amplifier fields, not
/// a plausible-looking round number.
#[test]
fn effect_constants_match_the_jar() {
    assert_eq!(ELDER_GUARDIAN_EFFECT_DURATION, 6000);
    assert_eq!(ELDER_GUARDIAN_EFFECT_AMPLIFIER, 2, "Mining Fatigue III is amplifier 2, zero-indexed");
    assert_eq!(ELDER_GUARDIAN_EFFECT_RADIUS, 50.0);
}
