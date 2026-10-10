//! A neutral mob's grudge is saved the way vanilla saves it (`anger_end_time`
//! plus `angry_at`) and, after a load, points at the offender again.

use super::*;
use crate::entity_record::SavedEntity;
use lodestone_core::Nbt;

fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_block(x, -1, z, "minecraft:grass_block");
        }
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("valid key")
}

fn alice() -> PlayerIdentity {
    PlayerIdentity { uuid: Uuid::from_u128(0xA11CE), entity_id: 4242 }
}

fn player_at(identity: PlayerIdentity, position: Vec3) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: Some(identity),
        perception: PlayerPerception {
            position,
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

fn reload<'w>(sim: &MobSim<'_>, world: &'w ChunkWorld) -> MobSim<'w> {
    let records: Vec<SavedEntity> = sim
        .saved_entities()
        .iter()
        .map(|saved| SavedEntity::from_nbt(&saved.to_nbt()).expect("record decodes"))
        .collect();
    let mut restored = MobSim::new(world);
    restored.restore_saved(&records);
    restored
}

fn hit(sim: &mut MobSim<'_>, id: i32, from: PlayerIdentity, at: Vec3) {
    sim.attack_from_player(id, Some(from), at, 1.0, DamageFlags::default(), 0.0)
        .expect("the mob exists");
}

fn saved_field(sim: &MobSim<'_>, species: &str, name: &str) -> Option<Nbt> {
    sim.saved_entities()
        .iter()
        .find(|s| s.id == key(species))
        .and_then(|s| s.extra.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
}

/// A wolf hit by a player writes a positive deadline and that player's uuid,
/// and reloads angry at the same player, chasing their current position.
#[test]
fn a_grudge_survives_a_reload_and_follows_its_offender() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![player_at(alice(), Vec3::new(3.0, 0.0, 3.0))]);
    let wolf = sim.spawn_species(key("wolf"), Vec3::new(0.5, 0.0, 0.5)).id();
    hit(&mut sim, wolf, alice(), Vec3::new(3.0, 0.0, 3.0));

    let Some(Nbt::Long(end)) = saved_field(&sim, "wolf", "anger_end_time") else {
        panic!("a hit wolf saves its deadline");
    };
    assert!(end > 0, "a live grudge has a positive deadline: {end}");
    let Some(Nbt::IntArray(uuid)) = saved_field(&sim, "wolf", "angry_at") else {
        panic!("the offender is saved as a uuid");
    };
    assert_eq!(uuid, crate::entity_record::uuid_to_ints(alice().uuid));

    // The offender has moved by the time the world is loaded and rejoined.
    let mut restored = reload(&sim, &world);
    let anger = restored.mobs[0].anger.expect("the grudge is restored");
    assert_eq!(anger.attacker, Some(alice().uuid));
    assert_eq!(anger.target, None, "no player is in the world yet");
    restored.set_players(vec![player_at(alice(), Vec3::new(-2.0, 0.0, 5.0))]);
    restored.tick();
    let target = restored.mobs[0].anger.and_then(|a| a.target);
    assert_eq!(target, Some(Vec3::new(-2.0, 0.0, 5.0)), "re-resolved to the offender's position");
}

/// Controls: an unhurt wolf saves "no grudge" and loads calm, a mob that is not
/// a neutral species saves no grudge fields even after a hit, and a deadline
/// that has already passed is not revived.
#[test]
fn grudge_controls() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let calm = sim.spawn_species(key("wolf"), Vec3::new(0.5, 0.0, 0.5)).id();
    assert_eq!(saved_field(&sim, "wolf", "anger_end_time"), Some(Nbt::Long(-1)));
    assert_eq!(saved_field(&sim, "wolf", "angry_at"), None);
    assert!(reload(&sim, &world).mobs[0].anger.is_none());
    let _ = calm;

    let cow = sim.spawn_species(key("cow"), Vec3::new(2.5, 0.0, 0.5)).id();
    hit(&mut sim, cow, alice(), Vec3::new(3.0, 0.0, 3.0));
    assert_eq!(saved_field(&sim, "cow", "anger_end_time"), None);

    // A record whose deadline is not in the future does not restore a grudge.
    let mut records = sim.saved_entities();
    for record in &mut records {
        if record.id == key("wolf") {
            record.extra.retain(|(k, _)| k != "anger_end_time");
            record.extra.push(("anger_end_time".to_owned(), Nbt::Long(0)));
            record.extra.push((
                "angry_at".to_owned(),
                Nbt::IntArray(crate::entity_record::uuid_to_ints(alice().uuid)),
            ));
        }
    }
    let mut restored = MobSim::new(&world);
    restored.restore_saved(&records);
    let wolf = restored.mobs.iter().find(|m| m.entity_type == key("wolf")).unwrap();
    assert!(wolf.anger.is_none());
}

/// The pack a hit wolf alerts records the same offender, so it reloads angry at
/// that player too.
#[test]
fn an_alerted_packmate_is_angry_at_the_same_player() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    sim.set_players(vec![player_at(alice(), Vec3::new(3.0, 0.0, 3.0))]);
    let hurt = sim.spawn_species(key("wolf"), Vec3::new(0.5, 0.0, 0.5)).id();
    let mate = sim.spawn_species(key("wolf"), Vec3::new(1.5, 0.0, 0.5)).id();
    for id in [hurt, mate] {
        sim.get_mut(id).unwrap().set_owner(Some(MobOwner::Player(Uuid::from_u128(7))));
    }
    hit(&mut sim, hurt, alice(), Vec3::new(3.0, 0.0, 3.0));
    assert_eq!(sim.get(mate).unwrap().anger.and_then(|a| a.attacker), Some(alice().uuid));
    let restored = reload(&sim, &world);
    assert!(restored.mobs.iter().all(|m| m.anger.and_then(|a| a.attacker) == Some(alice().uuid)));
}
