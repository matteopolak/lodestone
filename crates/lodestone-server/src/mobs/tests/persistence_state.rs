use super::*;
use crate::entity_storage::SavedEntity;
use lodestone_core::Nbt;

fn flat_world(composter: Option<BlockPos>) -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -20..=20 {
        for z in -20..=20 {
            world.set_block(x, -1, z, "minecraft:grass_block");
        }
    }
    if let Some(pos) = composter {
        world.set_block(pos.x, pos.y, pos.z, "minecraft:composter");
    }
    world
}

fn key(species: &str) -> ResourceKey {
    format!("minecraft:{species}").parse().expect("valid key")
}

fn owner_uuid() -> Uuid {
    Uuid::from_u128(0x0B5E_55ED)
}

/// Saves `sim`, pushes every record through the real NBT encoding and back (so
/// the field encoding, not just the in-memory struct, is what survives), and
/// restores into a fresh sim over `world`.
fn reload<'w>(sim: &MobSim<'_>, world: &'w ChunkWorld) -> MobSim<'w> {
    let records: Vec<SavedEntity> = sim
        .saved_entities()
        .iter()
        .map(|saved| SavedEntity::from_nbt(&saved.to_nbt()).expect("record decodes"))
        .collect();
    let mut restored = MobSim::new(world);
    assert_eq!(restored.restore_saved(&records), records.len());
    restored
}

fn only<'a, 'w>(sim: &'a MobSim<'w>, species: &str) -> &'a SimMob<'w> {
    sim.mobs.iter().find(|m| m.entity_type == key(species)).expect("species restored")
}

fn field<'a>(record: &'a SavedEntity, name: &str) -> Option<&'a Nbt> {
    record.extra.iter().find(|(key, _)| key == name).map(|(_, v)| v)
}

/// A villager's profession, level, xp, spent trades and workstation claim all
/// come back. The control restores the same record with its extra fields
/// stripped, which is what the persistence did before: an unemployed level-1
/// villager with fresh trades.
#[test]
fn a_villager_keeps_profession_level_xp_trades_and_claim() {
    let composter = BlockPos::new(10, 0, 0);
    let world = flat_world(Some(composter));
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_species(key("villager"), Vec3::new(0.5, 0.0, 0.5)).id();
    sim.tick();
    assert_eq!(sim.get(id).unwrap().profession(), villager::Profession::Farmer);
    {
        let mob = sim.get_mut(id).unwrap();
        mob.give_villager_xp(12);
        let trades = mob.ensure_trades().expect("employed villager has trades");
        trades.offers[0].uses = 3;
        trades.offers[0].demand = 7;
        trades.offers[1].special_price_diff = -2;
        trades.restock.number_of_restocks_today = 1;
    }
    let (level, xp) = {
        let mob = sim.get(id).unwrap();
        (mob.villager_level(), mob.villager_xp())
    };
    assert_eq!((level, xp), (2, 12), "the staged level and xp");

    let mut restored = reload(&sim, &world);
    let mob = only(&restored, "villager");
    assert_eq!(mob.profession(), villager::Profession::Farmer);
    assert_eq!((mob.villager_level(), mob.villager_xp()), (2, 12));
    assert_eq!(mob.workstation(), Some(composter), "the saved job site is re-claimed");
    let trades = &mob.trades.as_ref().expect("trades restored").2;
    assert_eq!((trades.offers[0].uses, trades.offers[0].demand), (3, 7));
    assert_eq!(trades.offers[1].special_price_diff, -2);
    assert_eq!(trades.restock.number_of_restocks_today, 1);

    // The claim is real: an unemployed villager cannot take the composter, and
    // the restored one is still a farmer after the profession pass runs.
    let rival = restored.spawn_species(key("villager"), Vec3::new(1.5, 0.0, 0.5)).id();
    for _ in 0..3 {
        restored.tick();
    }
    assert_eq!(restored.get(rival).unwrap().profession(), villager::Profession::None);
    assert_eq!(only(&restored, "villager").profession(), villager::Profession::Farmer);

    // Control: with the saved fields gone the villager comes back unemployed.
    let mut stripped = sim.saved_entities();
    for saved in &mut stripped {
        saved.extra.clear();
    }
    let mut control = MobSim::new(&world);
    control.restore_saved(&stripped);
    let mob = only(&control, "villager");
    assert_eq!(mob.profession(), villager::Profession::None);
    assert_eq!(mob.villager_level(), 1);
}

/// A restored villager whose job site is not (yet) in the world keeps its
/// profession and does not defect to a different station.
#[test]
fn a_restored_profession_survives_a_missing_station_and_reclaims_its_own() {
    let composter = BlockPos::new(10, 0, 0);
    let world = flat_world(Some(composter));
    let mut sim = MobSim::new(&world);
    sim.spawn_species(key("villager"), Vec3::new(0.5, 0.0, 0.5));
    sim.tick();

    // The saved world has no composter, only a barrel-less empty field plus a
    // lectern (a librarian station) the farmer must not take.
    let mut bare = flat_world(None);
    bare.set_block(-8, 0, 0, "minecraft:lectern");
    let mut restored = reload(&sim, &bare);
    for _ in 0..3 {
        restored.tick();
    }
    let mob = only(&restored, "villager");
    assert_eq!(mob.profession(), villager::Profession::Farmer, "profession is the durable fact");
    assert_eq!(mob.workstation(), None, "a lectern is not a farmer's station");
}

/// A tamed, sitting wolf keeps its owner; the control (a wild wolf) comes back
/// wild, so the restore is what tamed it.
#[test]
fn a_tamed_sitting_wolf_keeps_owner_and_sitting() {
    let world = flat_world(None);
    let mut sim = MobSim::new(&world);
    let tamed = sim.spawn_species(key("wolf"), Vec3::new(0.0, 0.0, 0.0)).id();
    sim.spawn_species(key("cat"), Vec3::new(3.0, 0.0, 0.0));
    let wolf = sim.get_mut(tamed).unwrap();
    wolf.tame(MobOwner::Player(owner_uuid()));
    wolf.set_ordered_to_sit(true);

    let saved = sim.saved_entities();
    let wolf_record = saved.iter().find(|s| s.id == key("wolf")).unwrap();
    assert_eq!(
        field(wolf_record, "Owner"),
        Some(&Nbt::IntArray(vec![0, 0, 0, 0x0B5E_55ED])),
        "vanilla's uuid int-array encoding"
    );
    assert_eq!(field(wolf_record, "Sitting"), Some(&Nbt::Byte(1)));
    assert!(field(saved.iter().find(|s| s.id == key("cat")).unwrap(), "Owner").is_none());

    let restored = reload(&sim, &world);
    let wolf = only(&restored, "wolf");
    assert!(wolf.is_tame());
    assert_eq!(wolf.owner(), Some(MobOwner::Player(owner_uuid())));
    assert!(wolf.is_ordered_to_sit());
    let cat = only(&restored, "cat");
    assert!(!cat.is_tame() && cat.owner().is_none() && !cat.is_ordered_to_sit());
}

/// A pet owned by another mob resolves back to a mob owner, not a player.
#[test]
fn a_mob_owner_resolves_to_the_restored_mob() {
    let world = flat_world(None);
    let mut sim = MobSim::new(&world);
    let boss = sim.spawn_species(key("cow"), Vec3::new(0.0, 0.0, 0.0)).id();
    let pet = sim.spawn_species(key("wolf"), Vec3::new(2.0, 0.0, 0.0)).id();
    sim.get_mut(pet).unwrap().tame(MobOwner::Mob(boss));
    let restored = reload(&sim, &world);
    let boss_id = only(&restored, "cow").id();
    assert_eq!(only(&restored, "wolf").owner(), Some(MobOwner::Mob(boss_id)));
}

/// A horse's `Tame` and `Temper`, and a leash to a fence post or a player.
#[test]
fn horse_taming_and_leashes_survive() {
    let world = flat_world(None);
    let mut sim = MobSim::new(&world);
    let horse = sim.spawn_species(key("horse"), Vec3::new(0.0, 0.0, 0.0)).id();
    sim.get_mut(horse).unwrap().tame(MobOwner::Player(owner_uuid())).set_temper(41);
    let fenced = sim.spawn_species(key("cow"), Vec3::new(4.0, 0.0, 0.0)).id();
    sim.get_mut(fenced).unwrap().set_leash_holder(Some(LeashHolder::Fence(BlockPos::new(3, 0, 3))));
    let walked = sim.spawn_species(key("pig"), Vec3::new(6.0, 0.0, 0.0)).id();
    sim.get_mut(walked).unwrap().set_leash_holder(Some(LeashHolder::Player(owner_uuid())));
    sim.spawn_species(key("sheep"), Vec3::new(8.0, 0.0, 0.0));

    let restored = reload(&sim, &world);
    let horse = only(&restored, "horse");
    assert!(horse.is_tame());
    assert_eq!(horse.temper(), 41);
    assert_eq!(only(&restored, "cow").leash_holder(), Some(LeashHolder::Fence(BlockPos::new(3, 0, 3))));
    assert_eq!(only(&restored, "pig").leash_holder(), Some(LeashHolder::Player(owner_uuid())));
    assert_eq!(only(&restored, "sheep").leash_holder(), None);
}

/// A name tag keeps a hostile mob from despawning, and the name plus every
/// unmodeled field (coat colour, shear state, variant) is carried through a
/// save/restore/save cycle verbatim. Controls: an unnamed zombie despawns
/// normally, and the second save of a mob restored without those fields has
/// none.
#[test]
fn names_persistence_and_unmodeled_fields_are_carried_through() {
    let world = flat_world(None);
    let record = |species: &str, extra: Vec<(String, Nbt)>| SavedEntity {
        id: key(species),
        uuid: Uuid::new_v4(),
        pos: Vec3::new(0.5, 0.0, 0.5),
        motion: Vec3::new(0.0, 0.0, 0.0),
        rotation: lodestone_model::Rotation::new(0.0, 0.0),
        health: Some(10.0),
        item: None,
        age: None,
        pickup_delay: None,
        extra,
    };
    let name = Nbt::String("{\"text\":\"Bessie\"}".to_owned());
    let named_zombie = record("zombie", vec![("CustomName".to_owned(), name.clone())]);
    let plain_zombie = record("zombie", Vec::new());
    let sheep = record(
        "sheep",
        vec![
            ("Color".to_owned(), Nbt::Byte(11)),
            ("Sheared".to_owned(), Nbt::Byte(1)),
            ("CustomName".to_owned(), name.clone()),
        ],
    );
    let cat = record(
        "cat",
        vec![
            ("variant".to_owned(), Nbt::String("minecraft:british_shorthair".to_owned())),
            ("CollarColor".to_owned(), Nbt::Byte(14)),
        ],
    );
    let mut sim = MobSim::new(&world);
    sim.restore_saved(&[named_zombie.clone(), plain_zombie.clone(), sheep, cat]);

    let named = sim.mobs.iter().find(|m| m.uuid == named_zombie.uuid).unwrap();
    assert!(named.is_persistent(), "a named hostile mob must not despawn");
    let plain = sim.mobs.iter().find(|m| m.uuid == plain_zombie.uuid).unwrap();
    assert!(!plain.is_persistent(), "control: an unnamed zombie still despawns");

    let saved = sim.saved_entities();
    let by = |species: &str| saved.iter().find(|s| s.id == key(species)).unwrap();
    assert_eq!(field(by("sheep"), "Color"), Some(&Nbt::Byte(11)));
    assert_eq!(field(by("sheep"), "Sheared"), Some(&Nbt::Byte(1)));
    assert_eq!(field(by("sheep"), "CustomName"), Some(&name));
    assert_eq!(field(by("cat"), "CollarColor"), Some(&Nbt::Byte(14)));
    assert_eq!(
        field(by("cat"), "variant"),
        Some(&Nbt::String("minecraft:british_shorthair".to_owned()))
    );
    let zombies: Vec<_> = saved.iter().filter(|s| s.id == key("zombie")).collect();
    let named_saved = zombies.iter().find(|s| s.uuid == named_zombie.uuid).unwrap();
    assert_eq!(field(named_saved, "PersistenceRequired"), Some(&Nbt::Byte(1)));
    let plain_saved = zombies.iter().find(|s| s.uuid == plain_zombie.uuid).unwrap();
    assert!(field(plain_saved, "PersistenceRequired").is_none());
}

/// A vanilla `PersistenceRequired: 0` on a passive animal must not make it
/// despawnable: the sim treats passive mobs as persistent by species.
#[test]
fn a_vanilla_unpersistent_passive_mob_stays_persistent() {
    let world = flat_world(None);
    let mut sim = MobSim::new(&world);
    sim.restore_saved(&[SavedEntity {
        id: key("cow"),
        uuid: Uuid::new_v4(),
        pos: Vec3::new(0.5, 0.0, 0.5),
        motion: Vec3::new(0.0, 0.0, 0.0),
        rotation: lodestone_model::Rotation::new(0.0, 0.0),
        health: Some(10.0),
        item: None,
        age: None,
        pickup_delay: None,
        extra: vec![("PersistenceRequired".to_owned(), Nbt::Byte(0))],
    }]);
    assert!(only(&sim, "cow").is_persistent());
}
