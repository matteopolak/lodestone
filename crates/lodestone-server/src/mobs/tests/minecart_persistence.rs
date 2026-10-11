//! Boats and minecarts save and reload their state. Expected field names,
//! tags and values are written out by hand from the reference save format
//! (container `Items` entries with `Slot`/`id`/`count`; furnace `PushX`,
//! `PushZ` doubles and a short `Fuel`; TNT int `fuse`), not read back from
//! the writer under test.

use super::*;
use super::super::minecart::MinecartKind;
use crate::entity_record::SavedEntity;
use lodestone_core::Nbt;
use lodestone_storage_schema::BuiltinDimension;

fn world() -> ChunkWorld {
    ChunkWorld::new(-64, 384)
}

fn key(name: &str) -> ResourceKey {
    format!("minecraft:{name}").parse().expect("valid key")
}

fn field<'a>(record: &'a SavedEntity, name: &str) -> Option<&'a Nbt> {
    record.extra.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

fn saved_cart(sim: &MobSim<'_>, entity: &str) -> SavedEntity {
    sim.saved_entities().into_iter().find(|s| s.id == key(entity)).expect("the cart is saved")
}

/// Reload through the on-disk record encoding.
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

/// Reload through the native store, the path a persistent world takes.
fn reload_native<'w>(sim: &MobSim<'_>, world: &'w ChunkWorld) -> MobSim<'w> {
    let records = sim.native_entities(BuiltinDimension::Overworld);
    let mut restored = MobSim::new(world);
    restored.restore_native(&records);
    restored
}

fn only_cart(sim: &MobSim<'_>) -> i32 {
    assert_eq!(sim.minecart_count(), 1, "exactly one cart came back");
    *sim.minecarts.keys().next().expect("a cart")
}

#[test]
fn a_chest_carts_items_are_saved_in_the_reference_shape_and_reload() {
    let w = world();
    let mut sim = MobSim::new(&w);
    let id = sim.spawn_minecart(MinecartKind::Chest, Vec3::new(0.5, 64.0, 0.5));
    sim.minecarts.get_mut(&id).expect("cart").slots[2] = Some(ItemStack::new(key("diamond"), 3));

    let saved = saved_cart(&sim, "chest_minecart");
    let Some(Nbt::List { elements, .. }) = field(&saved, "Items") else { panic!("Items is a list") };
    assert_eq!(
        elements,
        &vec![Nbt::Compound(vec![
            ("Slot".to_owned(), Nbt::Byte(2)),
            ("id".to_owned(), Nbt::String("minecraft:diamond".to_owned())),
            ("count".to_owned(), Nbt::Int(3)),
        ])]
    );

    for restored in [reload(&sim, &w), reload_native(&sim, &w)] {
        let id = only_cart(&restored);
        let slots = restored.minecart_slots(id).expect("slots");
        assert_eq!(slots.len(), 27);
        assert_eq!(slots[2], Some(ItemStack::new(key("diamond"), 3)));
        assert_eq!(slots.iter().flatten().count(), 1);
    }
}

#[test]
fn an_empty_container_cart_still_writes_an_empty_items_list() {
    let w = world();
    let mut sim = MobSim::new(&w);
    sim.spawn_minecart(MinecartKind::Hopper, Vec3::new(0.5, 64.0, 0.5));
    let saved = saved_cart(&sim, "hopper_minecart");
    assert!(matches!(field(&saved, "Items"), Some(Nbt::List { elements, .. }) if elements.is_empty()));
    let restored = reload_native(&sim, &w);
    assert_eq!(restored.minecart_slots(only_cart(&restored)).map(<[_]>::len), Some(5));
}

#[test]
fn a_furnace_carts_fuel_and_push_are_saved_and_reload() {
    let w = world();
    let mut sim = MobSim::new(&w);
    let id = sim.spawn_minecart(MinecartKind::Furnace, Vec3::new(0.5, 64.0, 0.5));
    {
        let cart = sim.minecarts.get_mut(&id).expect("cart");
        cart.fuel = 1234;
        cart.push = lodestone_physics::Vec3d::new(0.5, 0.0, -0.25);
    }
    let saved = saved_cart(&sim, "furnace_minecart");
    assert_eq!(field(&saved, "Fuel"), Some(&Nbt::Short(1234)));
    assert_eq!(field(&saved, "PushX"), Some(&Nbt::Double(0.5)));
    assert_eq!(field(&saved, "PushZ"), Some(&Nbt::Double(-0.25)));
    for restored in [reload(&sim, &w), reload_native(&sim, &w)] {
        let id = only_cart(&restored);
        assert_eq!(restored.minecart_fuel(id), 1234);
        let cart = &restored.minecarts[&id];
        assert_eq!((cart.push.x, cart.push.z), (0.5, -0.25));
    }
}

#[test]
fn a_tnt_carts_fuse_is_saved_and_reloads() {
    let w = world();
    let mut sim = MobSim::new(&w);
    let id = sim.spawn_minecart(MinecartKind::Tnt, Vec3::new(0.5, 64.0, 0.5));
    sim.minecarts.get_mut(&id).expect("cart").fuse = 37;
    assert_eq!(field(&saved_cart(&sim, "tnt_minecart"), "fuse"), Some(&Nbt::Int(37)));
    for restored in [reload(&sim, &w), reload_native(&sim, &w)] {
        assert_eq!(restored.minecart_fuse(only_cart(&restored)), 37);
    }
}

#[test]
fn command_and_spawner_cart_data_rides_through_a_reload() {
    let w = world();
    for (kind, entity, name, value) in [
        (MinecartKind::CommandBlock, "command_block_minecart", "Command", Nbt::String("say hi".to_owned())),
        (MinecartKind::Spawner, "spawner_minecart", "Delay", Nbt::Short(20)),
    ] {
        let mut sim = MobSim::new(&w);
        sim.spawn_minecart(kind, Vec3::new(0.5, 64.0, 0.5));
        let mut record = saved_cart(&sim, entity);
        record.extra.push((name.to_owned(), value.clone()));
        let mut loaded = MobSim::new(&w);
        loaded.restore_saved(&[SavedEntity::from_nbt(&record.to_nbt()).expect("decodes")]);
        assert_eq!(field(&saved_cart(&loaded, entity), name), Some(&value), "{entity} keeps {name}");
        let again = reload_native(&loaded, &w);
        assert_eq!(field(&saved_cart(&again, entity), name), Some(&value), "{entity} keeps {name} natively");
    }
}

#[test]
fn an_empty_boat_survives_the_native_store() {
    let w = world();
    let mut sim = MobSim::new(&w);
    sim.spawn_vehicle(key("oak_boat"), Vec3::new(0.5, 64.0, 0.5), 0.0);
    sim.spawn_vehicle(key("bamboo_raft"), Vec3::new(4.5, 64.0, 0.5), 0.0);
    let restored = reload_native(&sim, &w);
    let mut ids: Vec<_> = restored.saved_entities().into_iter().map(|s| s.id).collect();
    ids.sort_by_key(ToString::to_string);
    assert_eq!(ids, vec![key("bamboo_raft"), key("oak_boat")]);
}

#[test]
fn a_broken_container_cart_spills_its_stacks_and_drops_its_item() {
    let w = world();
    let mut sim = MobSim::new(&w);
    let id = sim.spawn_minecart(MinecartKind::Chest, Vec3::new(0.5, 64.0, 0.5));
    {
        let slots = &mut sim.minecarts.get_mut(&id).expect("cart").slots;
        slots[0] = Some(ItemStack::new(key("diamond"), 3));
        slots[9] = Some(ItemStack::new(key("stick"), 1));
    }
    let uuid = saved_cart(&sim, "chest_minecart").uuid;
    sim.kill_entity(uuid).expect("the cart is killable");
    assert_eq!(sim.minecart_count(), 0);
    assert_eq!(sim.item_count(), 3, "two spilled stacks and the cart item");
}

#[test]
fn entity_data_shows_the_same_cart_fields() {
    let w = world();
    let mut sim = MobSim::new(&w);
    let id = sim.spawn_minecart(MinecartKind::Furnace, Vec3::new(0.5, 64.0, 0.5));
    sim.minecarts.get_mut(&id).expect("cart").fuel = 99;
    let uuid = saved_cart(&sim, "furnace_minecart").uuid;
    let Some(Nbt::Compound(fields)) = sim.entity_nbt(uuid) else { panic!("compound") };
    assert!(fields.iter().any(|(n, v)| n == "Fuel" && *v == Nbt::Short(99)));
}
