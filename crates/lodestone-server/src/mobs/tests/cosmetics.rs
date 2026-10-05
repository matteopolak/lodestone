use super::*;
use crate::entity_storage::SavedEntity;
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

fn bob() -> PlayerIdentity {
    PlayerIdentity { uuid: Uuid::from_u128(0xB0B), entity_id: 4343 }
}

fn spawn(sim: &mut MobSim<'_>, species: &str) -> i32 {
    sim.spawn_species(key(species), Vec3::new(0.5, 0.0, 0.5)).id()
}

fn metadata_of(sim: &MobSim<'_>, id: i32) -> Vec<MetadataField> {
    sim.get(id).expect("alive").snapshot().metadata
}

fn wool_items(sim: &MobSim<'_>, color: &str) -> usize {
    sim.saved_entities()
        .iter()
        .filter(|saved| saved.item.as_ref().is_some_and(|(item, _)| *item == key(&format!("{color}_wool"))))
        .count()
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

/// Shears take the wool once: 1 to 3 wool items of the sheep's colour drop, the
/// sheared flag reaches the wire metadata, and a second use or a baby refuses.
#[test]
fn shearing_a_sheep_drops_its_coloured_wool_once_and_streams_the_flag() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = spawn(&mut sim, "sheep");
    let color = sim.get(id).unwrap().wool_color();
    let name = appearance::DYE_NAMES[usize::from(color)];
    assert!(metadata_of(&sim, id).contains(&MetadataField::SheepWool { color, sheared: false }));

    let outcome = sim.interact(id, alice(), Some(&key("shears")));
    assert_eq!(outcome, InteractOutcome::Sheared);
    assert!(!outcome.consumes_item(), "shears are not consumed");
    assert!(sim.get(id).unwrap().is_sheared());
    let dropped = wool_items(&sim, name);
    assert!((1..=3).contains(&dropped), "dropped {dropped} wool");
    assert!(metadata_of(&sim, id).contains(&MetadataField::SheepWool { color, sheared: true }));

    assert_eq!(sim.interact(id, alice(), Some(&key("shears"))), InteractOutcome::Pass);
    assert_eq!(wool_items(&sim, name), dropped, "a sheared sheep drops nothing more");

    let baby = spawn(&mut sim, "sheep");
    sim.get_mut(baby).unwrap().set_age(BABY_START_AGE);
    assert_eq!(sim.interact(baby, alice(), Some(&key("shears"))), InteractOutcome::Pass);
    let cow = spawn(&mut sim, "cow");
    assert_eq!(sim.interact(cow, alice(), Some(&key("shears"))), InteractOutcome::Pass);
}

/// Eating grass grows the wool back.
#[test]
fn grazing_regrows_sheared_wool() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = spawn(&mut sim, "sheep");
    sim.interact(id, alice(), Some(&key("shears")));
    assert!(sim.get(id).unwrap().is_sheared());
    sim.get_mut(id).unwrap().mob.ate(EatenBlock::Below);
    sim.tick();
    assert!(!sim.get(id).unwrap().is_sheared(), "grazing must unshear");

    let bare = spawn(&mut sim, "sheep");
    sim.interact(bare, alice(), Some(&key("shears")));
    sim.tick();
    assert!(sim.get(bare).unwrap().is_sheared(), "control: no graze, no regrowth");
}

/// Dye recolours an unsheared sheep, is refused on a sheared one and on the same
/// colour, and a tamed wolf or cat accepts it only from its owner.
#[test]
fn dye_recolours_wool_and_collars_under_vanilla_gates() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let sheep = spawn(&mut sim, "sheep");
    sim.get_mut(sheep).unwrap().appearance.wool = 0;
    let red = key("red_dye");
    let outcome = sim.interact(sheep, alice(), Some(&red));
    assert_eq!(outcome, InteractOutcome::Dyed { color: 14 });
    assert!(outcome.consumes_item());
    assert_eq!(sim.get(sheep).unwrap().wool_color(), 14);
    assert!(metadata_of(&sim, sheep).contains(&MetadataField::SheepWool { color: 14, sheared: false }));
    assert_eq!(sim.interact(sheep, alice(), Some(&red)), InteractOutcome::Pass, "same colour");
    sim.interact(sheep, alice(), Some(&key("shears")));
    assert_eq!(sim.interact(sheep, alice(), Some(&key("blue_dye"))), InteractOutcome::Pass, "sheared");

    for species in ["wolf", "cat"] {
        let pet = spawn(&mut sim, species);
        assert_eq!(sim.interact(pet, alice(), Some(&key("blue_dye"))), InteractOutcome::Pass, "{species} wild");
        sim.get_mut(pet).unwrap().tame(MobOwner::Player(alice().uuid));
        assert_eq!(sim.interact(pet, bob(), Some(&key("blue_dye"))), InteractOutcome::Pass, "{species} stranger");
        assert_eq!(sim.get(pet).unwrap().collar_color(), 14, "{species} default collar is red");
        assert_eq!(
            sim.interact(pet, alice(), Some(&key("blue_dye"))),
            InteractOutcome::Dyed { color: 11 }
        );
        let md = metadata_of(&sim, pet);
        let want = if species == "wolf" { MetadataField::WolfCollar(11) } else { MetadataField::CatCollar(11) };
        assert!(md.contains(&want), "{species}: {md:?}");
    }
}

/// A name tag names a mob, keeps it from despawning and survives a reload; the
/// metadata carries the name. Control: an unnamed hostile mob stays despawnable.
#[test]
fn a_name_tag_names_persists_and_syncs() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let zombie = spawn(&mut sim, "zombie");
    let plain = spawn(&mut sim, "zombie");
    sim.get_mut(zombie).unwrap().set_persistent(false);
    sim.get_mut(plain).unwrap().set_persistent(false);

    assert!(sim.apply_name_tag(zombie, Nbt::String("Grumm".to_owned())));
    assert_eq!(sim.get(zombie).unwrap().custom_name().as_deref(), Some("Grumm"));
    assert!(sim.get(zombie).unwrap().is_persistent());
    assert!(!sim.get(plain).unwrap().is_persistent(), "control");
    assert!(metadata_of(&sim, zombie).contains(&MetadataField::CustomName(Some(
        lodestone_model::Text::literal("Grumm")
    ))));
    assert!(!metadata_of(&sim, plain).iter().any(|f| matches!(f, MetadataField::CustomName(_))));

    let restored = reload(&sim, &world);
    let named = restored
        .snapshots()
        .into_iter()
        .map(|s| s.id)
        .find_map(|id| restored.get(id).filter(|m| m.custom_name().is_some()))
        .expect("the named zombie came back");
    assert_eq!(named.custom_name().as_deref(), Some("Grumm"));
    assert!(named.is_persistent());
}

/// Dye, shear state, collar and name each survive a save and restore.
#[test]
fn cosmetic_state_survives_a_reload() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let sheep = spawn(&mut sim, "sheep");
    sim.get_mut(sheep).unwrap().appearance.wool = 0;
    sim.interact(sheep, alice(), Some(&key("green_dye")));
    sim.interact(sheep, alice(), Some(&key("shears")));
    let wolf = spawn(&mut sim, "wolf");
    sim.get_mut(wolf).unwrap().tame(MobOwner::Player(alice().uuid));
    sim.interact(wolf, alice(), Some(&key("purple_dye")));

    let restored = reload(&sim, &world);
    let find = |species: &str| {
        restored.mobs.iter().find(|m| m.entity_type == key(species)).expect("restored")
    };
    assert_eq!((find("sheep").wool_color(), find("sheep").is_sheared()), (13, true));
    assert_eq!(find("wolf").collar_color(), 10);
}

/// The spawn-time variant follows the biome tags and weights of the data files,
/// with the expected value for each biome taken from those tag lists.
#[test]
fn spawn_variants_follow_the_biome_tables() {
    let u = Uuid::from_u128(7);
    let name = |species: &str, biome: &str| match appearance::choose_variant(species, biome, u) {
        Some(appearance::MobVariant::Name(n)) => n,
        other => panic!("{species} in {biome}: {other:?}"),
    };
    for species in ["cow", "pig", "chicken"] {
        assert_eq!(name(species, "minecraft:desert"), "minecraft:warm");
        assert_eq!(name(species, "minecraft:jungle"), "minecraft:warm");
        assert_eq!(name(species, "minecraft:lukewarm_ocean"), "minecraft:warm");
        assert_eq!(name(species, "minecraft:taiga"), "minecraft:cold");
        assert_eq!(name(species, "minecraft:snowy_plains"), "minecraft:cold");
        assert_eq!(name(species, "minecraft:plains"), "minecraft:temperate");
    }
    // Frogs use their own, narrower, lists: lukewarm ocean and taiga are not in them.
    assert_eq!(name("frog", "minecraft:lukewarm_ocean"), "minecraft:temperate");
    assert_eq!(name("frog", "minecraft:taiga"), "minecraft:temperate");
    assert_eq!(name("frog", "minecraft:mangrove_swamp"), "minecraft:warm");
    for (biome, variant) in [
        ("minecraft:snowy_taiga", "minecraft:ashen"),
        ("minecraft:old_growth_pine_taiga", "minecraft:black"),
        ("minecraft:old_growth_spruce_taiga", "minecraft:chestnut"),
        ("minecraft:grove", "minecraft:snowy"),
        ("minecraft:forest", "minecraft:woods"),
        ("minecraft:sparse_jungle", "minecraft:rusty"),
        ("minecraft:savanna_plateau", "minecraft:spotted"),
        ("minecraft:eroded_badlands", "minecraft:striped"),
        ("minecraft:plains", "minecraft:pale"),
    ] {
        assert_eq!(name("wolf", biome), variant, "{biome}");
    }
    assert_eq!(name("fox", "minecraft:snowy_taiga"), "snow");
    assert_eq!(name("fox", "minecraft:forest"), "red");
    assert_eq!(appearance::choose_variant("rabbit", "minecraft:desert", u), Some(appearance::MobVariant::Int(4)));
    assert!(appearance::choose_variant("zombie", "minecraft:plains", u).is_none());
}

/// Rolled variants are real choices: across many spawns the random species vary
/// within their legal ranges, and the cat variant is always a registered key.
#[test]
fn random_variants_vary_within_their_ranges() {
    let mut cats = std::collections::HashSet::new();
    let mut horses = std::collections::HashSet::new();
    let mut parrots = std::collections::HashSet::new();
    for n in 0..400u128 {
        let u = Uuid::from_u128(n.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835));
        if let Some(appearance::MobVariant::Name(c)) = appearance::choose_variant("cat", "minecraft:plains", u) {
            cats.insert(c);
        }
        if let Some(appearance::MobVariant::Int(h)) = appearance::choose_variant("horse", "minecraft:plains", u) {
            assert!((h & 0xFF) < 7 && (h >> 8) < 5, "horse variant {h:#x}");
            horses.insert(h);
        }
        if let Some(appearance::MobVariant::Int(p)) = appearance::choose_variant("parrot", "minecraft:jungle", u) {
            assert!((0..5).contains(&p));
            parrots.insert(p);
        }
    }
    assert_eq!(cats.len(), 10, "all ten ordinary cat variants must occur: {cats:?}");
    assert!(horses.len() > 20 && parrots.len() == 5);
}

/// Every variant species spawns with a variant that is saved under vanilla's
/// field, restored, and (for the species the client draws) streamed.
#[test]
fn every_variant_species_spawns_persists_and_streams_a_variant() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let species = [
        ("cat", "variant"), ("wolf", "variant"), ("cow", "variant"), ("pig", "variant"),
        ("chicken", "variant"), ("frog", "variant"), ("horse", "Variant"), ("llama", "Variant"),
        ("parrot", "Variant"), ("axolotl", "Variant"), ("rabbit", "RabbitType"), ("fox", "Type"),
        ("mooshroom", "Type"),
    ];
    for (name, _) in species {
        spawn(&mut sim, name);
    }
    let saved = sim.saved_entities();
    for (name, field) in species {
        let record = saved.iter().find(|s| s.id == key(name)).unwrap();
        let value = record.extra.iter().find(|(k, _)| k == field).map(|(_, v)| v);
        assert!(value.is_some(), "{name} saved no `{field}`: {:?}", record.extra);
    }
    let restored = reload(&sim, &world);
    for (name, _) in species {
        let before = sim.mobs.iter().find(|m| m.entity_type == key(name)).unwrap();
        let after = restored.mobs.iter().find(|m| m.entity_type == key(name)).unwrap();
        assert_eq!(before.variant_name(), after.variant_name(), "{name}");
    }
    let streamed = |name: &str| {
        let id = sim.mobs.iter().find(|m| m.entity_type == key(name)).unwrap().id;
        metadata_of(&sim, id)
    };
    assert!(streamed("cow").iter().any(|f| matches!(f, MetadataField::HolderVariant { .. })));
    assert!(streamed("horse").iter().any(|f| matches!(f, MetadataField::HorseVariant(_))));
    assert!(streamed("fox").iter().any(|f| matches!(f, MetadataField::FoxType(_))));
    assert!(streamed("axolotl").iter().any(|f| matches!(f, MetadataField::AxolotlVariant(_))));
}
