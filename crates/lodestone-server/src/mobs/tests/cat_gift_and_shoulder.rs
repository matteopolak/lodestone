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

/// Cats that were relocated by the gift: standing more than a block from where
/// they began (one tick of idle movement is far below that; the relocation
/// lands on whole-number coordinates up to five blocks away).
fn relocated_cats(release: lodestone_data::GameDataVersion, floor: &str) -> usize {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_block(x, -1, z, floor);
        }
    }
    let mut sim = MobSim::for_release(&world, release);
    sim.day_time = 23_800;
    let mut cats = Vec::new();
    for i in 0..40 {
        let start = Vec3::new(f64::from(i % 5) * 3.0 - 6.0 + 0.5, 0.0, f64::from(i / 5) * 1.5 - 5.0 + 0.5);
        let id = sim.spawn_species(cat_key(), start).id();
        sim.get_mut(id).expect("just spawned").mob.request_gift();
        cats.push((id, start));
    }
    sim.tick();
    cats.iter()
        .filter(|(id, start)| {
            let now = sim.get(*id).expect("cat is still in the world").position();
            (now.x - start.x).hypot(now.z - start.z) > 1.0
        })
        .count()
}

/// The gift moves the cat to a random spot first. 26.3 refuses a spot on or in
/// a block that is dangerous for teleportation, so over a magma floor no cat
/// moves; 26.2 has no such filter. The stone floor is the control: the same
/// cats do move there in 26.3, so the zero is the filter and not a detector
/// that cannot see movement.
#[test]
fn a_gift_cat_moves_in_26_2_over_magma_but_26_3_leaves_it_where_it_stands() {
    use lodestone_data::GameDataVersion::{V26_2, V26_3};
    assert!(relocated_cats(V26_3, "minecraft:stone") >= 15, "control: cats relocate over stone");
    assert!(relocated_cats(V26_2, "minecraft:magma_block") >= 15, "26.2 relocates over magma");
    assert_eq!(relocated_cats(V26_3, "minecraft:magma_block"), 0, "26.3 refuses the dangerous landing block");
}

fn wide_world(floor: &str) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -48..=24 {
        for z in -8..=24 {
            world.set_block(x, -1, z, floor);
        }
    }
    world
}

/// The gift lands on the corner of the cat's block, one block behind its body
/// yaw: `(x - sin(yaw), z + cos(yaw))`. The cats stand on magma in 26.3, where a
/// gift cat never relocates, so each stays on its block and the spot depends only
/// on the yaw. The expected spots are worked out by hand: yaw 0 puts the item one
/// block south (+z), 90 one block west (-x), 180 one north, 270 one east.
#[test]
fn the_gift_lands_one_block_behind_the_cats_body_yaw() {
    let world = wide_world("minecraft:magma_block");
    let mut sim = MobSim::for_release(&world, lodestone_data::GameDataVersion::V26_3);
    sim.day_time = 23_800;
    let mut expected = Vec::new();
    for i in 0..40 {
        let yaw = [0.0f32, 90.0, 180.0, 270.0][i % 4];
        let (x, z) = (-40.0 + i as f64 * 1.5 + 0.5, 3.5);
        let id = sim.spawn_species(cat_key(), Vec3::new(x, 0.0, z)).id();
        let cat = sim.get_mut(id).expect("just spawned");
        cat.mob.set_body_yaw(yaw);
        cat.mob.request_gift();
        let (dx, dz) = match yaw as u32 {
            0 => (0.0, 1.0),
            90 => (-1.0, 0.0),
            180 => (0.0, -1.0),
            _ => (1.0, 0.0),
        };
        expected.push(Vec3::new(x.floor() + dx, 0.0, z.floor() + dz));
    }
    sim.tick();
    let items: Vec<Vec3> = (0..2000).filter_map(|id| sim.item_position(id)).collect();
    assert!(items.len() >= 15, "at a 0.7 chance over 40 cats most gift: {}", items.len());
    let strays: Vec<&Vec3> = items
        .iter()
        .filter(|item| !expected.iter().any(|spot| (item.x - spot.x).abs() < 0.25 && (item.z - spot.z).abs() < 0.25))
        .collect();
    assert!(strays.is_empty(), "items off every expected spot: {strays:?}");
}

/// Where cats that moved from `start` ended up, when each is tied to `holder`
/// (or to nothing) and gifts once over a stone floor in 26.3.
fn gift_landings(holder: Option<LeashHolder>) -> Vec<Vec3> {
    let world = wide_world("minecraft:stone");
    let mut sim = MobSim::for_release(&world, lodestone_data::GameDataVersion::V26_3);
    sim.day_time = 23_800;
    let start = Vec3::new(0.5, 0.0, 0.5);
    let mut cats = Vec::new();
    for _ in 0..40 {
        let id = sim.spawn_species(cat_key(), start).id();
        let cat = sim.get_mut(id).expect("just spawned");
        cat.set_leash_holder(holder);
        cat.mob.request_gift();
        cats.push(id);
    }
    sim.tick();
    cats.iter()
        .map(|id| sim.get(*id).expect("cat is still in the world").position())
        .filter(|now| (now.x - start.x).hypot(now.z - start.z) > 1.0)
        .collect()
}

/// A leashed cat moves to a random spot within five blocks of its holder's
/// block, not its own. The fence at (11, 0, 11) puts that box at 6..=16 on each
/// axis, disjoint from the box around the cat's own block (-5..=5), which is what
/// the untethered control lands in.
#[test]
fn a_leashed_cat_relocates_around_its_holder() {
    let control = gift_landings(None);
    assert!(control.len() >= 10, "control: cats relocate: {}", control.len());
    assert!(control.iter().all(|p| (-5.0..6.0).contains(&p.x) && (-5.0..6.0).contains(&p.z)), "{control:?}");

    let tied = gift_landings(Some(LeashHolder::Fence(BlockPos::new(11, 0, 11))));
    assert!(tied.len() >= 10, "tied cats relocate: {}", tied.len());
    assert!(tied.iter().all(|p| (6.0..17.0).contains(&p.x) && (6.0..17.0).contains(&p.z)), "{tied:?}");
}
