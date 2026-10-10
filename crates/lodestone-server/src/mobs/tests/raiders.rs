//! Raider behaviour inside a raid: the leader banner, visiting homes, holding
//! ground, celebrating a lost village, recruiting strays and healing.

use lodestone_model::BlockPos;

use super::*;
use crate::mobs::raid::RaidStatus;

fn field() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -80..80 {
        for z in -80..80 {
            world.set_block(x, 0, z, "minecraft:grass_block");
        }
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("key")
}

fn step(sim: &mut MobSim<'_>, world: &ChunkWorld, ticks: u32) {
    for _ in 0..ticks {
        sim.tick_with_terrain(&|x, y, z| Some(world.block_state_id(x, y, z)));
    }
}

/// A sim whose village is a claimed bell at the origin, with an ongoing raid
/// centred there whose waves are all spent, so no raiders spawn on their own.
fn spent_raid(world: &ChunkWorld) -> (MobSim<'_>, i32) {
    let mut sim = MobSim::new(world);
    assert!(sim.bell_claims.try_claim(BlockPos::new(0, 1, 0)));
    let raid = sim
        .start_raid(Vec3::new(0.5, 1.0, 0.5), lodestone_model::Difficulty::Easy, 1)
        .expect("Easy is not Peaceful");
    let state = sim.raids.get_mut(&raid).expect("just started");
    state.groups_spawned = state.total_waves;
    (sim, raid)
}

fn enlist(sim: &mut MobSim<'_>, raid: i32, species: &str, at: Vec3) -> i32 {
    let id = sim.spawn_species(key(species), at).id();
    sim.raids.get_mut(&raid).expect("raid").raiders.push(id);
    id
}

/// Raiders celebrating after the bell that makes the origin a village is
/// released (or, for the control, kept) and whether the raid is still tracked
/// after a further 650 ticks.
fn loss_outcome(release_village: bool) -> (RaidStatus, usize, bool) {
    let world = field();
    let (mut sim, raid) = spent_raid(&world);
    let a = enlist(&mut sim, raid, "pillager", Vec3::new(10.5, 1.0, 10.5));
    let b = enlist(&mut sim, raid, "vindicator", Vec3::new(-10.5, 1.0, 10.5));
    step(&mut sim, &world, 5);
    if release_village {
        sim.bell_claims.release(BlockPos::new(0, 1, 0));
    }
    step(&mut sim, &world, 60);
    let status = sim.raids.get(&raid).expect("tracked").status;
    let celebrating = [a, b].iter().filter(|&&id| sim.get(id).is_some_and(|m| m.mob.is_celebrating())).count();
    step(&mut sim, &world, 650);
    (status, celebrating, sim.raids.contains_key(&raid))
}

#[test]
fn a_raid_that_loses_its_village_is_lost_and_its_raiders_celebrate_until_it_ends() {
    let (status, celebrating, tracked) = loss_outcome(true);
    assert_eq!(status, RaidStatus::Loss);
    assert_eq!(celebrating, 2, "both raiders should celebrate");
    assert!(!tracked, "a lost raid ends after 600 ticks of celebration");
    let (status, celebrating, tracked) = loss_outcome(false);
    assert_eq!(status, RaidStatus::Ongoing, "control: the village still stands");
    assert_eq!(celebrating, 0, "control: nobody celebrates");
    assert!(tracked, "control: the raid goes on");
}

#[test]
fn a_raid_with_no_village_and_no_wave_yet_simply_stops() {
    let world = field();
    let mut sim = MobSim::new(&world);
    let raid = sim
        .start_raid(Vec3::new(0.5, 1.0, 0.5), lodestone_model::Difficulty::Easy, 1)
        .expect("Easy is not Peaceful");
    step(&mut sim, &world, 3);
    assert!(!sim.raids.contains_key(&raid));
}

fn wears_banner(sim: &MobSim<'_>, id: i32) -> bool {
    sim.get(id).is_some_and(|m| {
        m.equipment_snapshot().iter().any(|e| {
            e.slot == lodestone_model::event::EquipmentSlot::Head
                && e.item.as_ref().is_some_and(|i| !i.components.banner_patterns.is_empty())
        })
    })
}

/// A wave spawned by a fresh raid: its leader wears the banner; when the
/// leader dies the banner drops and another raider of the wave picks it up.
/// Returns whether the leader wore the banner and dropped exactly it, who wears
/// it afterwards, and how many items remain on the ground.
fn banner_after_leader_dies() -> (bool, Option<i32>, usize) {
    let world = field();
    let mut sim = MobSim::new(&world);
    assert!(sim.bell_claims.try_claim(BlockPos::new(0, 1, 0)));
    let raid = sim
        .start_raid(Vec3::new(0.5, 1.0, 0.5), lodestone_model::Difficulty::Easy, 1)
        .expect("Easy is not Peaceful");
    step(&mut sim, &world, 2);
    let leader = sim.raids.get(&raid).expect("raid").captain.expect("a wave spawned with a leader");
    let led = wears_banner(&sim, leader);
    sim.get_mut(leader).expect("leader").apply_damage(1000.0, DamageFlags::default());
    step(&mut sim, &world, 2);
    let dropped = sim.item_count();
    step(&mut sim, &world, 900);
    let new_leader = sim.raids.get(&raid).and_then(|r| r.captain).filter(|&id| wears_banner(&sim, id));
    (led && dropped == 1, new_leader, sim.item_count())
}

#[test]
fn the_wave_leader_wears_the_banner_and_a_raider_picks_it_up_when_the_leader_dies() {
    let (dropped_on_death, new_leader, items_left) = banner_after_leader_dies();
    assert!(dropped_on_death, "the leader must wear the banner and drop exactly it");
    assert!(new_leader.is_some(), "another raider should end up wearing the banner");
    assert_eq!(items_left, 0, "the picked-up banner leaves the ground");
}

/// Drops the banner beside two pillagers, `enlisted` of them in an ongoing
/// raid that has (`led`) or lacks a living leader. Returns who ends up wearing it.
fn banner_fetch(enlisted: bool, led: bool) -> Vec<i32> {
    let world = field();
    let (mut sim, raid) = spent_raid(&world);
    let a = sim.spawn_species(key("pillager"), Vec3::new(8.5, 1.0, 0.5)).id();
    let b = sim.spawn_species(key("pillager"), Vec3::new(-8.5, 1.0, 0.5)).id();
    if enlisted {
        let state = sim.raids.get_mut(&raid).expect("raid");
        state.raiders.extend([a, b]);
        if led {
            state.captain = Some(a);
        }
    } else {
        // Move the raid out of recruiting range so the pillagers stay strays.
        assert!(sim.bell_claims.try_claim(BlockPos::new(300, 1, 0)));
        sim.raids.get_mut(&raid).expect("raid").center = Vec3::new(300.5, 1.0, 0.5);
    }
    if led {
        let m = sim.get_mut(a).expect("pillager");
        m.wears_banner = true;
    }
    let banner = super::super::raid::ominous_banner();
    sim.spawn_item(&banner, Vec3::new(0.5, 1.0, 0.5), Vec3::new(0.0, 0.0, 0.0), ItemLifecycle::newly_dropped(1, 64));
    step(&mut sim, &world, 300);
    [a, b].into_iter().filter(|&id| wears_banner(&sim, id)).collect()
}

#[test]
fn only_a_raider_of_a_raid_without_a_living_leader_fetches_the_banner() {
    assert_eq!(banner_fetch(true, false).len(), 1, "an unled raid's raider takes the banner");
    assert!(banner_fetch(false, false).is_empty(), "control: pillagers outside a raid leave it");
    assert_eq!(banner_fetch(true, true), vec![banner_fetch(true, true)[0]], "control: the leader keeps it");
    assert_eq!(banner_fetch(true, true).len(), 1, "control: with a living leader nobody else takes it");
}

/// The distance from a raider that starts at the origin to a claimed bed 20
/// blocks east after 500 ticks of an ongoing raid (the bed is claimed only
/// when `bed`).
fn distance_to_bed(bed: bool) -> f64 {
    let world = field();
    let (mut sim, raid) = spent_raid(&world);
    if bed {
        assert!(sim.bed_claims.try_claim(BlockPos::new(20, 1, 0)));
    }
    let id = enlist(&mut sim, raid, "pillager", Vec3::new(0.5, 1.0, 0.5));
    step(&mut sim, &world, 500);
    let at = sim.get(id).expect("alive").position();
    (at.x - 20.5).hypot(at.z - 0.5)
}

#[test]
fn a_raider_in_an_ongoing_raid_walks_to_a_claimed_bed() {
    let with_bed = distance_to_bed(true);
    let without = distance_to_bed(false);
    assert!(with_bed < 3.0, "the raider ended {with_bed} blocks from the bed");
    assert!(without > 8.0, "control: with no bed it ended {without} blocks from where the bed would be");
}

/// Whether a vindicator two blocks from a patrolling pillager acquires the
/// pillager's target as soon as the pillager spots a player 25 blocks away
/// (beyond the vindicator's own 12-block follow range), and whether the
/// pillager keeps still while the player is farther than 15 blocks.
fn hold_ground(patrolling: bool) -> (bool, f64) {
    let world = field();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![PerceivedPlayer {
        identity: None,
        perception: PlayerPerception {
            position: Vec3::new(25.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(-1.0, 0.0, 0.0),
        },
    }]);
    let pillager = sim.spawn_species(key("pillager"), Vec3::new(0.5, 1.0, 0.5)).id();
    let vindicator = sim.spawn_species(key("vindicator"), Vec3::new(-2.5, 1.0, 0.5)).id();
    sim.get_mut(pillager).expect("pillager").set_patrolling(patrolling);
    let mut shouted = false;
    for _ in 0..40 {
        step(&mut sim, &world, 1);
        shouted |= sim.get(vindicator).is_some_and(|m| m.mob.attack_target().is_some());
    }
    let at = sim.get(pillager).expect("alive").position();
    (shouted, (at.x - 0.5).hypot(at.z - 0.5))
}

#[test]
fn a_patrolling_raider_holds_ground_and_calls_the_raiders_near_it_to_its_target() {
    let (shouted, moved) = hold_ground(true);
    assert!(shouted, "the vindicator should have been given the pillager's target");
    assert!(moved < 1.0, "the pillager should stand still while the target is beyond 15 blocks, moved {moved}");
    let (shouted, moved) = hold_ground(false);
    assert!(!shouted, "control: a pillager that is not patrolling calls nobody");
    assert!(moved > 3.0, "control: it advances on the player, moved {moved}");
}

/// Whether a stray pillager standing `distance` blocks from an ongoing raid's
/// centre has joined it after 25 ticks.
fn recruited_at(distance: f64) -> bool {
    let world = field();
    let (mut sim, _raid) = spent_raid(&world);
    let id = sim.spawn_species(key("pillager"), Vec3::new(0.5 + distance, 1.0, 0.5)).id();
    step(&mut sim, &world, 25);
    sim.raid_containing_raider(id).is_some()
}

#[test]
fn a_stray_raider_within_96_blocks_of_an_ongoing_raid_joins_it() {
    assert!(recruited_at(60.0), "a stray 60 blocks out should be recruited");
    assert!(!recruited_at(110.0), "control: one 110 blocks out stays a stray");
}

/// A witch of an ongoing raid beside a wounded pillager of the same raid, or
/// (for the control) in a world with no raid at all. Returns the pillager's
/// health afterwards.
fn pillager_health_after_witch(raid_ongoing: bool) -> f32 {
    let world = field();
    let mut sim = MobSim::new(&world);
    let (witch, pillager);
    if raid_ongoing {
        assert!(sim.bell_claims.try_claim(BlockPos::new(0, 1, 0)));
        let raid = sim
            .start_raid(Vec3::new(0.5, 1.0, 0.5), lodestone_model::Difficulty::Easy, 1)
            .expect("Easy is not Peaceful");
        let state = sim.raids.get_mut(&raid).expect("just started");
        state.groups_spawned = state.total_waves;
        witch = enlist(&mut sim, raid, "witch", Vec3::new(0.5, 1.0, 0.5));
        pillager = enlist(&mut sim, raid, "pillager", Vec3::new(5.5, 1.0, 0.5));
    } else {
        witch = sim.spawn_species(key("witch"), Vec3::new(0.5, 1.0, 0.5)).id();
        pillager = sim.spawn_species(key("pillager"), Vec3::new(5.5, 1.0, 0.5)).id();
    }
    let _ = witch;
    sim.get_mut(pillager).expect("pillager").set_health(3.0);
    step(&mut sim, &world, 400);
    sim.get(pillager).expect("alive").health()
}

#[test]
fn a_witch_in_a_raid_heals_a_wounded_raider() {
    let healed = pillager_health_after_witch(true);
    let unhealed = pillager_health_after_witch(false);
    assert!(healed >= 6.0, "the pillager only reached {healed} health");
    assert!(unhealed < 4.0, "control: with no raid nobody heals it, health {unhealed}");
}

/// Damage a vindicator deals to a player standing 6 blocks away over 300 ticks,
/// with the player present or (for the control) absent.
fn vindicator_damage(player_present: bool) -> f32 {
    let world = field();
    let mut sim = MobSim::new(&world);
    if player_present {
        sim.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity { uuid: Uuid::from_u128(9), entity_id: 90 }),
            perception: PlayerPerception {
                position: Vec3::new(6.5, 1.0, 0.5),
                held_item: None,
                view_direction: Vec3::new(-1.0, 0.0, 0.0),
            },
        }]);
    }
    sim.spawn_species(key("vindicator"), Vec3::new(0.5, 1.0, 0.5));
    let mut damage = 0.0;
    for _ in 0..300 {
        step(&mut sim, &world, 1);
        damage += sim.take_player_hits().iter().map(|h| h.raw_damage).sum::<f32>();
    }
    damage
}

#[test]
fn a_vindicator_hunts_and_strikes_a_player() {
    assert!(vindicator_damage(true) >= 5.0, "the vindicator should land a 5-damage axe blow");
    assert_eq!(vindicator_damage(false), 0.0, "control: with no player nothing is struck");
}
