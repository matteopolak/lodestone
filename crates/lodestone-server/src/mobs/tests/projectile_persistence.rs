//! Arrows, tridents and thrown items survive a save and a load with the
//! field names a vanilla server writes, and an arrow stuck in a block stays
//! stuck, keeps its pickup rule and can still be taken.

use super::*;
use crate::entity_storage::SavedEntity;
use lodestone_core::Nbt;
use lodestone_data::potion::PotionId;
use lodestone_entity::projectile::Projectile;

fn key(name: &str) -> ResourceKey {
    format!("minecraft:{name}").parse().expect("valid key")
}

fn alice() -> Uuid {
    Uuid::from_u128(0xA11CE)
}

/// Stone at (5, 1, 5) for an arrow to strike.
fn wall_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-4, 24);
    world.set_block(5, 1, 5, "minecraft:stone");
    world
}

/// The save, pushed through the same NBT encode/decode the region files use,
/// into a fresh sim over `world`.
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

fn saved<'a>(records: &'a [SavedEntity], name: &str) -> &'a SavedEntity {
    records.iter().find(|s| s.id == key(name)).expect("record present")
}

fn get<'a>(record: &'a SavedEntity, name: &str) -> Option<&'a Nbt> {
    record.extra.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// Fires a player-owned arrow into the stone block and lets it rattle out.
fn stuck_arrow(sim: &mut MobSim<'_>, pickup: ArrowPickup) -> i32 {
    let id = sim.spawn_projectile(
        key("arrow"),
        Projectile::arrow(Vec3::new(5.5, 1.5, 4.0), Vec3::new(0.0, 0.0, 2.0)),
    );
    sim.set_projectile_shooter(id, alice(), pickup);
    sim.resolve_projectile_impacts();
    for _ in 0..8 {
        sim.tick_stuck_arrows();
    }
    id
}

fn speed(sim: &MobSim<'_>, id: i32) -> f64 {
    sim.projectiles.get(id).expect("tracked").velocity.length()
}

/// The arrow stays in the world embedded after a block hit, and a save writes
/// vanilla's field names; the load puts it back embedded, owned and
/// takeable.
#[test]
fn a_stuck_arrow_reloads_embedded_with_its_pickup_rule() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    let id = stuck_arrow(&mut sim, ArrowPickup::Allowed);
    assert_eq!(sim.projectile_count(), 1, "a block hit leaves the arrow in the world");
    assert_eq!(speed(&sim, id), 0.0);
    let before = sim.projectile_position(id).expect("tracked");

    let records = sim.saved_entities();
    let record = saved(&records, "arrow");
    assert_eq!(get(record, "inGround"), Some(&Nbt::Byte(1)));
    assert_eq!(get(record, "pickup"), Some(&Nbt::Byte(1)));
    assert_eq!(get(record, "shake"), Some(&Nbt::Byte(0)));
    assert_eq!(get(record, "life"), Some(&Nbt::Short(8)));
    assert_eq!(get(record, "damage"), Some(&Nbt::Double(2.0)));
    let Some(Nbt::Compound(state)) = get(record, "inBlockState") else {
        panic!("inBlockState must be a palette entry");
    };
    assert!(state.contains(&("Name".to_owned(), Nbt::String("minecraft:stone".into()))));
    assert_eq!(
        get(record, "Owner"),
        Some(&Nbt::IntArray(crate::entity_storage::uuid_to_ints(alice())))
    );

    let mut restored = reload(&sim, &world);
    assert_eq!(restored.projectile_count(), 1);
    let new_id = restored.projectiles.iter().next().expect("one").id;
    assert_eq!(restored.projectile_position(new_id), Some(before));
    assert_eq!(speed(&restored, new_id), 0.0);
    // It stays put: nothing moves a frozen arrow while its block is there.
    restored.tick_stuck_arrows();
    let batches = restored.tick_projectile_owner_batches();
    restored.apply_projectile_tick_owner_batches(batches);
    assert_eq!(restored.projectile_position(new_id), Some(before));
    // And the rule survived: a survival player standing at it gets the arrow.
    let feet = Vec3::new(before.x, before.y - 0.5, before.z);
    assert_eq!(
        restored.arrows_within_pickup_range(feet, false),
        vec![(new_id, Some(key("arrow")))]
    );
}

/// Controls for the test above: the same save restored where the block is
/// gone does not stay frozen, and a disallowed arrow is saved as such and
/// cannot be taken after the load.
#[test]
fn stuck_arrow_controls() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    stuck_arrow(&mut sim, ArrowPickup::Disallowed);

    let mut empty = ChunkWorld::new(-4, 24);
    empty.set_block(0, 0, 0, "minecraft:stone"); // loads the column, not the cell
    let mut gone = reload(&sim, &empty);
    gone.tick_stuck_arrows();
    let id = gone.projectiles.iter().next().expect("one").id;
    assert!(
        !gone.projectiles.iter().next().expect("one").projectile.frozen,
        "with the block removed the arrow is released: {id}"
    );

    let kept = reload(&sim, &world);
    let at = kept.projectiles.iter().next().expect("one").projectile.position;
    assert_eq!(get(&kept.saved_entities()[0], "pickup"), Some(&Nbt::Byte(0)));
    assert!(
        kept.arrows_within_pickup_range(Vec3::new(at.x, at.y - 0.5, at.z), false).is_empty(),
        "a mob-style arrow is never takeable"
    );
}

/// An arrow still flying keeps its velocity and writes no embedded state.
#[test]
fn an_arrow_in_flight_reloads_in_flight() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_projectile(
        key("arrow"),
        Projectile::arrow(Vec3::new(1.0, 5.0, 1.0), Vec3::new(0.5, 0.25, 1.5)),
    );
    sim.set_projectile_shooter(id, alice(), ArrowPickup::CreativeOnly);
    let records = sim.saved_entities();
    let record = saved(&records, "arrow");
    assert_eq!(get(record, "inGround"), Some(&Nbt::Byte(0)));
    assert!(get(record, "inBlockState").is_none(), "no block when it is not in one");
    assert_eq!(get(record, "pickup"), Some(&Nbt::Byte(2)));

    let restored = reload(&sim, &world);
    let tracked = restored.projectiles.iter().next().expect("one");
    assert_eq!(tracked.projectile.velocity, Vec3::new(0.5, 0.25, 1.5));
    assert_eq!(tracked.projectile.position, Vec3::new(1.0, 5.0, 1.0));
    assert!(!tracked.projectile.frozen);
}

/// A stuck arrow despawns after its lifetime; a trident a player may reclaim
/// does not, and saves `DealtDamage`.
#[test]
fn lifetime_despawns_an_arrow_but_not_a_reclaimable_trident() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    let arrow = stuck_arrow(&mut sim, ArrowPickup::Allowed);
    let trident = sim.spawn_projectile(
        key("trident"),
        Projectile::arrow(Vec3::new(5.5, 1.5, 4.0), Vec3::new(0.0, 0.0, 2.0)),
    );
    sim.set_projectile_shooter(trident, alice(), ArrowPickup::Allowed);
    sim.resolve_projectile_impacts();
    for _ in 0..1300 {
        sim.tick_stuck_arrows();
    }
    assert!(sim.projectiles.get(arrow).is_none(), "the arrow expired");
    assert!(sim.projectiles.get(trident).is_some(), "the allowed trident did not");
    let records = sim.saved_entities();
    assert_eq!(get(saved(&records, "trident"), "DealtDamage"), Some(&Nbt::Byte(0)));
}

/// A thrown splash potion keeps its potion through a save, as vanilla's
/// `Item` stack with a `potion_contents` component; a snowball keeps just the
/// item. The control is a bare restore with the component stripped.
#[test]
fn a_thrown_potion_keeps_its_potion() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);
    let swiftness = PotionId::from_name("minecraft:swiftness").expect("potion");
    sim.spawn_potion_projectile_from(
        key("splash_potion"),
        Projectile::throwable(Vec3::new(1.0, 9.0, 1.0), Vec3::new(0.0, 0.3, 0.5)),
        None,
        Some(swiftness),
    );
    sim.spawn_projectile(
        key("snowball"),
        Projectile::throwable(Vec3::new(2.0, 9.0, 2.0), Vec3::new(0.0, 0.0, 0.5)),
    );
    let restored = reload(&sim, &world);
    let potion_of = |name: &str| {
        restored
            .projectile_meta
            .values()
            .find(|m| m.entity_type == key(name))
            .map(|m| m.potion)
    };
    assert_eq!(potion_of("splash_potion"), Some(Some(swiftness)));
    assert_eq!(potion_of("snowball"), Some(None));

    // Control: with the component removed from the record, the potion is lost.
    let mut records = sim.saved_entities();
    for record in &mut records {
        record.extra.retain(|(k, _)| k != "Item");
    }
    let mut stripped = MobSim::new(&world);
    stripped.restore_saved(&records);
    assert!(stripped.projectile_meta.values().all(|m| m.potion.is_none()));
}

/// A mob-fired arrow's owner is re-resolved from its uuid after the load.
#[test]
fn a_restored_arrow_resolves_its_mob_shooter() {
    let world = ChunkWorld::new(-4, 24);
    let mut sim = MobSim::new(&world);
    let skeleton = sim.spawn_species(key("skeleton"), Vec3::new(0.5, 0.0, 0.5)).id();
    sim.spawn_projectile_from(
        key("arrow"),
        Projectile::arrow(Vec3::new(1.0, 5.0, 1.0), Vec3::new(0.0, 0.0, 1.0)),
        Some(skeleton),
    );
    let restored = reload(&sim, &world);
    let new_skeleton = restored.mobs.iter().find(|m| m.entity_type == key("skeleton")).expect("mob").id;
    let meta = restored.projectile_meta.values().next().expect("arrow");
    assert_eq!(meta.owner, Some(new_skeleton));
}
