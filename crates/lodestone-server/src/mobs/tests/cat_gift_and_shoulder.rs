use super::*;

/// A real floor — see `leash_tests::flat_world`'s own doc comment for
/// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    world
}

fn cat_key() -> ResourceKey {
    "minecraft:cat".parse().expect("valid key")
}

fn parrot_key() -> ResourceKey {
    "minecraft:parrot".parse().expect("valid key")
}

/// `cat_gift_chance`'s two-keyframe step function, pinned against both
/// hypotheses a rounding-off reading could produce: inside the
/// pre-dawn window (`[23667, 24000)` and `[0, 362)`, wrapping) the
/// chance is vanilla's own `0.7F`; everywhere else it is a hard `0.0`,
/// not merely "lower".
#[test]
fn cat_gift_chance_matches_the_timeline_step_function() {
    assert_eq!(MobSim::cat_gift_chance(0), 0.7, "the very start of a day is still inside the wrapped window");
    assert_eq!(MobSim::cat_gift_chance(361), 0.7, "one tick before the low keyframe");
    assert_eq!(MobSim::cat_gift_chance(362), 0.0, "the low keyframe itself");
    assert_eq!(MobSim::cat_gift_chance(12_000), 0.0, "the middle of the day");
    assert_eq!(MobSim::cat_gift_chance(23_666), 0.0, "one tick before the high keyframe");
    assert_eq!(MobSim::cat_gift_chance(23_667), 0.7, "the high keyframe itself");
    assert_eq!(MobSim::cat_gift_chance(23_999), 0.7, "the last tick before wrapping");
    assert_eq!(
        MobSim::cat_gift_chance(24_000 + 12_000),
        0.0,
        "a second day's middle must read the same as the first's"
    );
}

/// A cat's [`MobController::request_gift`] call, drained through one real
/// `MobSim::tick`, must reach an actual item entity when the gift chance
/// is favourable — proving the production drain (`gift_requests` →
/// `resolve_cat_gifts`), not merely that the resolver function works when
/// called directly. Forty cats rather than one: at chance `0.7` the
/// probability every single one fails is `0.3^40`, indistinguishable
/// from zero, so this is not a flaky roll of the dice — it is the
/// deterministic-in-practice discriminator against a wiring break that
/// would make it fail **every** time instead.
#[test]
fn a_cats_gift_request_reaches_a_real_item_through_one_production_tick() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.day_time = 23_800; // inside the 0.7 window
    for i in 0..40 {
        let id = sim.spawn_species(cat_key(), Vec3::new(f64::from(i) * 8.0, 64.0, 0.0)).id();
        sim.get_mut(id).expect("just spawned").mob.request_gift();
    }
    assert_eq!(sim.item_count(), 0, "no item exists before the tick that drains the request");
    sim.tick();
    assert!(
        sim.item_count() > 0,
        "at a 0.7 gift chance across 40 requests, at least one real item entity must exist \
         after one production tick"
    );
}

/// The deterministic negative control for the same chain: outside the
/// gift window the chance is a **hard** `0.0` (not merely lower), so no
/// request — however many — can ever produce an item. This is what
/// separates "the wiring reaches the resolver" from "the resolver always
/// spawns something regardless of the roll".
#[test]
fn a_cats_gift_request_never_lands_outside_the_gift_window() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.day_time = 12_000; // dead centre of the 0.0 window
    for i in 0..40 {
        let id = sim.spawn_species(cat_key(), Vec3::new(f64::from(i) * 8.0, 64.0, 0.0)).id();
        sim.get_mut(id).expect("just spawned").mob.request_gift();
    }
    sim.tick();
    assert_eq!(sim.item_count(), 0, "a 0.0 gift chance must never spawn an item, however many cats request one");
}

/// A parrot's [`MobController::request_shoulder_ride`] call, drained
/// through one real tick, removes the mob from the world and records a
/// shoulder rider for its owner — the mount half. Then, once the owner
/// is reported asleep and the 20-tick minimum ride has elapsed, the next
/// several ticks must dismount it: the parrot reappears as a real mob
/// again and the shoulder-rider slot clears. Both halves driven through
/// production `MobSim::tick`, not through `resolve_shoulder_mounts`/
/// `tick_shoulder_dismounts` called directly.
#[test]
fn a_parrots_shoulder_request_despawns_it_and_a_sleeping_owner_dismounts_it_back() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let owner_uuid = Uuid::new_v4();
    let owner_entity_id = 500;
    sim.set_players(vec![PerceivedPlayer {
        identity: Some(PlayerIdentity {
            uuid: owner_uuid,
            entity_id: owner_entity_id,
        }),
        perception: PlayerPerception {
            position: Vec3::new(0.0, 64.0, 0.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }]);
    let id = sim.spawn_species(parrot_key(), Vec3::new(0.0, 64.0, 0.0)).id();
    sim.get_mut(id).expect("just spawned").tame(MobOwner::Player(owner_uuid));
    sim.get_mut(id).expect("just spawned").mob.request_shoulder_ride();

    sim.tick();

    assert_eq!(
        sim.mobs.iter().filter(|m| m.entity_type.path() == "parrot").count(),
        0,
        "a mounted parrot must be removed from the world, matching vanilla's own \
         shoulder-mount setter's discard"
    );
    assert_eq!(sim.shoulder_riders.len(), 1, "the mount must be recorded for its owner");

    // The owner falls asleep. The dismount is gated on the 20-tick
    // minimum ride (vanilla's own per-player shoulder-mount timer plus 20), so this
    // must run well past that before asserting.
    let since = sim.tick_count;
    sim.set_sleeping_players(vec![(owner_entity_id, since)]);
    for _ in 0..30 {
        sim.tick();
    }

    assert_eq!(
        sim.mobs.iter().filter(|m| m.entity_type.path() == "parrot").count(),
        1,
        "a sleeping owner past the ride-cooldown grace period must dismount and respawn the parrot"
    );
    assert!(sim.shoulder_riders.is_empty(), "the shoulder slot must clear on dismount");
}

/// The 20-tick minimum-ride grace period, isolated: an owner reported
/// asleep on the *same* tick the parrot mounts must not dismount it
/// immediately — the `mounted_tick + 20 < game_tick` grace-period guard.
#[test]
fn a_freshly_mounted_parrot_does_not_dismount_before_the_grace_period() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let owner_uuid = Uuid::new_v4();
    let owner_entity_id = 501;
    sim.set_players(vec![PerceivedPlayer {
        identity: Some(PlayerIdentity {
            uuid: owner_uuid,
            entity_id: owner_entity_id,
        }),
        perception: PlayerPerception {
            position: Vec3::new(0.0, 64.0, 0.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }]);
    let id = sim.spawn_species(parrot_key(), Vec3::new(0.0, 64.0, 0.0)).id();
    sim.get_mut(id).expect("just spawned").tame(MobOwner::Player(owner_uuid));
    sim.get_mut(id).expect("just spawned").mob.request_shoulder_ride();
    sim.tick();
    assert_eq!(sim.shoulder_riders.len(), 1);

    let since = sim.tick_count;
    sim.set_sleeping_players(vec![(owner_entity_id, since)]);
    // Well under the 20-tick grace period.
    for _ in 0..5 {
        sim.tick();
    }
    assert_eq!(
        sim.shoulder_riders.len(),
        1,
        "a parrot inside its 20-tick minimum ride must not dismount just because the owner sleeps"
    );
}
