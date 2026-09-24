use super::*;

/// A real floor near the origin — see `leash_tests::flat_world`'s own
/// doc comment for why a bare void `ChunkWorld` stopped being safe once
/// idle mobs fall. This module's "distant villager" control spawns at
/// `(500, 0, 500)`, deliberately left ungrounded: nothing in this module
/// asserts that villager's own position, only that gossip/curing never
/// reaches it, and x/z are untouched by falling regardless.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=20 {
        for z in -8..=20 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn alice() -> PlayerIdentity {
    PlayerIdentity {
        uuid: Uuid::from_u128(0xA11CE),
        entity_id: 4242,
    }
}

/// A golden apple on a zombie villager with no Weakness must do nothing
/// at all — no conversion state, `Pass`, matching vanilla's own
/// plain-success-no-reduction arm (disclosed as `Pass`,
/// see `InteractOutcome::ZombieVillagerConversionStarted`'s own doc).
#[test]
fn a_golden_apple_on_an_unweakened_zombie_villager_does_nothing() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:zombie_villager".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();

    let outcome = sim.interact(
        id,
        alice(),
        Some(&"minecraft:golden_apple".parse().expect("valid key")),
    );
    assert_eq!(outcome, InteractOutcome::Pass);
    assert!(
        sim.get(id).expect("still alive").conversion.is_none(),
        "no conversion state must be started without Weakness"
    );
}

/// **The wire is real, not merely the derivation.** A golden apple used
/// on a weakened zombie villager must: report
/// `ZombieVillagerConversionStarted` (which consumes the item), start a
/// real [`villager::conversion::ConversionState`] with the actor's uuid
/// recorded, remove Weakness, add Strength, and publish the cure sound
/// through the same [`MobSim::take_vocalisations`] queue
/// `crate::tick::run_tick_loop` drains in production — not a hermetic
/// call to `effects::zombie_villager_cure_sound` in isolation.
#[test]
fn a_golden_apple_on_a_weakened_zombie_villager_starts_a_real_conversion() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:zombie_villager".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        )
        .id();
    sim.get_mut(id)
        .expect("spawned")
        .apply_effect("minecraft:weakness", 1000, 0);

    let outcome = sim.interact(
        id,
        alice(),
        Some(&"minecraft:golden_apple".parse().expect("valid key")),
    );
    assert_eq!(outcome, InteractOutcome::ZombieVillagerConversionStarted);
    assert!(
        outcome.consumes_item(),
        "the golden apple must be consumed, matching itemStack.consume(1, player)"
    );

    let mob = sim.get(id).expect("still alive");
    let state = mob.conversion.expect("a conversion must have started");
    assert_eq!(state.starter, Some(alice().uuid));
    assert!(
        (villager::conversion::CONVERSION_WAIT_MIN..=villager::conversion::CONVERSION_WAIT_MAX)
            .contains(&state.remaining_ticks),
        "remaining_ticks must land in the real vanilla 3600-6000 range, got {}",
        state.remaining_ticks
    );
    assert!(
        mob.effects().amplifier_of("minecraft:weakness").is_none(),
        "Weakness must be removed"
    );
    assert!(
        mob.effects().amplifier_of("minecraft:strength").is_some(),
        "Strength must be applied"
    );

    let vocalisations = sim.take_vocalisations();
    assert_eq!(
        vocalisations.len(),
        1,
        "exactly one cure sound must have been queued, got {vocalisations:?}"
    );
    match &vocalisations[0] {
        crate::effects::WorldEffect::Sound { sound, .. } => {
            assert_eq!(sound, "minecraft:entity.zombie_villager.cure");
        }
        other => panic!("expected a Sound effect, got {other:?}"),
    }
}

/// **The whole timer, driven through the production `tick()` loop** rather
/// than a direct call to `villager::conversion::conversion_progress`.
/// The countdown uses a handful of ticks (private-field access, same crate)
/// so this test does not need 3600+ iterations; the *mechanism* ticked is
/// the same one production drives. A completed conversion must flip
/// `entity_type` to `minecraft:villager`, seed gossip with the curer's
/// `ZombieVillagerCured` entries, apply nausea (the "confusion" state), and publish
/// a conversion-sound level event
/// through the same queue production drains.
#[test]
fn a_completed_conversion_becomes_a_real_villager_with_seeded_gossip() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species(
            "minecraft:zombie_villager".parse().expect("valid key"),
            Vec3::new(10.0, 0.0, 10.0),
        )
        .id();
    let curer = alice().uuid;
    sim.get_mut(id).expect("spawned").conversion = Some(villager::conversion::ConversionState {
        starter: Some(curer),
        remaining_ticks: 3,
    });

    let mut level_events = Vec::new();
    for _ in 0..10 {
        sim.tick();
        level_events.extend(sim.take_ambient_sounds());
        if sim
            .get(id)
            .is_some_and(|m| m.entity_type().path() == "villager")
        {
            break;
        }
    }

    let mob = sim.get(id).expect("still alive");
    assert_eq!(mob.entity_type().path(), "villager", "must have become a real villager");
    assert!(mob.conversion.is_none(), "conversion state must be cleared");
    assert_eq!(
        mob.gossip.reputation(curer),
        125,
        "ZombieVillagerCured's own predicted value (20*5 + 25*1), seeded onto the \
         new villager's own ledger"
    );
    assert!(
        mob.effects().amplifier_of("minecraft:nausea").is_some(),
        "the post-cure confusion state (Nausea) must be applied"
    );

    assert!(
        level_events.iter().any(|effect| matches!(
            effect,
            crate::effects::WorldEffect::LevelEvent { event, .. }
                if *event == crate::effects::SOUND_ZOMBIE_CONVERTED
        )),
        "the SOUND_ZOMBIE_CONVERTED level event must reach the production queue, got {level_events:?}"
    );
}

/// `MobSim::record_reputation_event`/`villager_reputation` reach a real
/// spawned villager's own ledger, not a hermetic `GossipContainer`.
#[test]
fn record_reputation_event_reaches_a_real_villagers_own_ledger() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let player = alice().uuid;

    assert_eq!(sim.villager_reputation(id, player), 0);
    sim.record_reputation_event(id, villager::reputation::ReputationEventType::Trade, player);
    assert_eq!(sim.villager_reputation(id, player), 2, "trading grants 2 * weight(1) = 2");
}

/// `MobSim::attack_from_player`: hurting a real villager
/// writes negative gossip onto **that villager's own** ledger about the
/// attacker — driven through the real hit pipeline
/// (`apply_damage`/`note_hurt`), not a direct `apply_reputation_event`
/// call.
#[test]
fn hurting_a_real_villager_lowers_its_reputation_of_the_attacker() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let attacker = alice();

    assert_eq!(sim.villager_reputation(id, attacker.uuid), 0);
    let outcome = sim.attack_from_player(
        id,
        Some(attacker),
        Vec3::new(1.0, 0.0, 0.0),
        1.0,
        DamageFlags::default(),
        0.0,
    );
    assert!(outcome.is_some(), "the villager must have been hit");
    assert_eq!(
        sim.villager_reputation(id, attacker.uuid),
        -25,
        "VillagerHurt's predicted value: 25 * minor_negative.weight()(-1)"
    );
}

/// **Control: a `None` attacker must write no gossip at all** — the
/// disclosed "unidentified actor" skip, proven by actually driving it
/// rather than merely asserting the branch exists.
#[test]
fn an_unidentified_attacker_writes_no_reputation_gossip() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();

    sim.attack_from_player(id, None, Vec3::new(1.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.0);
    assert!(
        sim.get(id).expect("still alive").gossip.is_empty(),
        "no attacker identity means no gossip write at all"
    );
}

/// Two villagers close enough to gossip exchange ledger entries through the
/// real `tick()` loop's `spread_villager_gossip` pass, exercising the
/// integrated producer and consumer path.
#[test]
fn two_nearby_villagers_spread_gossip_through_the_real_tick_loop() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let a = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let b = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
        .id();
    let stranger = Uuid::from_u128(0xDEAD_BEEF);
    sim.get_mut(a)
        .expect("spawned")
        .gossip
        // `Trading`, not `MajorPositive`: `MajorPositive`'s own
        // `decay_per_transfer` (20) equals its own `max` (20), so it
        // can *never* survive a transfer (always decays to exactly 0,
        // below `DISCARD_THRESHOLD`) — a real vanilla quirk `gossip.rs`'s
        // own `a_transferred_entry_that_decays_below_threshold_is_dropped`
        // test already predicts. `Trading` at its own max (25) decays to
        // 5, which does survive.
        .add(stranger, villager::gossip::GossipType::Trading, 25);

    for _ in 0..(MobSim::GOSSIP_SPREAD_INTERVAL_TICKS + 1) {
        sim.tick();
    }

    assert!(
        sim.get(b)
            .expect("still alive")
            .gossip
            .entries_for(stranger)
            .is_some(),
        "villager b must have picked up some gossip about the stranger from villager a"
    );
}

/// Control: two villagers far apart never spread, even across many
/// gossip-spread passes — otherwise the subject test above could pass
/// under an implementation with no distance gate at all.
#[test]
fn distant_villagers_never_spread_gossip() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let a = sim
        .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
        .id();
    let b = sim
        .spawn_species(
            "minecraft:villager".parse().expect("valid key"),
            Vec3::new(500.0, 0.0, 500.0),
        )
        .id();
    let stranger = Uuid::from_u128(0xDEAD_BEEF);
    sim.get_mut(a)
        .expect("spawned")
        .gossip
        // `Trading`, not `MajorPositive`: `MajorPositive`'s own
        // `decay_per_transfer` (20) equals its own `max` (20), so it
        // can *never* survive a transfer (always decays to exactly 0,
        // below `DISCARD_THRESHOLD`) — a real vanilla quirk `gossip.rs`'s
        // own `a_transferred_entry_that_decays_below_threshold_is_dropped`
        // test already predicts. `Trading` at its own max (25) decays to
        // 5, which does survive.
        .add(stranger, villager::gossip::GossipType::Trading, 25);

    for _ in 0..(MobSim::GOSSIP_SPREAD_INTERVAL_TICKS * 3) {
        sim.tick();
    }

    assert!(
        sim.get(b)
            .expect("still alive")
            .gossip
            .entries_for(stranger)
            .is_none(),
        "villagers 500 blocks apart must never spread gossip to each other"
    );
}
