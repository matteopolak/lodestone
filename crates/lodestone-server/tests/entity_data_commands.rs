//! **`/data` on entities and block entities, the `nbt=` selector option,
//! `/damage` and `/ride` act on the entities the mob simulation owns and the
//! block entities the registry holds.**
//!
//! Every command runs through the production [`ServerCommands`] tree against a
//! live [`MobHandle`]. Expected values are hand arithmetic or the reference's
//! message text, never read back from the code under test. Alice stands at
//! (0, 64, 0), survival, entity id 1001.

use lodestone_model::{GameMode, Rotation, Vec3};
use lodestone_server::commands::registrar::RuleStore;
use lodestone_server::commands::{
    CommandSource, CommandWorld, PlayerCandidate, ServerCommands, overworld_dimension,
};
use lodestone_model::{BlockPos, ItemStack};
use lodestone_server::{
    BlockEntity, BlockEntityHandle, BlockEntityKind, ChunkColumn, ChunkSource, CommandEntity, CommandResponse, MobHandle,
};
use uuid::Uuid;

/// Chunk data that holds block entities generation placed, which no player has touched.
struct StoredBlockEntities(Vec<(BlockPos, BlockEntity)>);

impl ChunkSource for StoredBlockEntities {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 16)
    }

    fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> lodestone_data::block_states::StateId {
        lodestone_data::block_states::StateId::AIR
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}

    fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<BlockEntity> {
        let at = BlockPos::new(x, y, z);
        self.0.iter().find(|(pos, _)| *pos == at).map(|(_, entity)| entity.clone())
    }
}

struct Server {
    commands: ServerCommands,
    state: lodestone_server::world_state::WorldStateHandle,
    mobs: MobHandle,
    block_entities: BlockEntityHandle,
    /// The chunk data under the registry, when a test gives it block entities.
    stored: Option<StoredBlockEntities>,
    source: CommandSource,
    players: Vec<PlayerCandidate>,
}

impl Server {
    fn new() -> Self {
        let uuid = Uuid::from_u128(1);
        let source = CommandSource::player(
            uuid,
            1001,
            "alice",
            Vec3::new(0.0, 64.0, 0.0),
            Rotation { yaw: 0.0, pitch: 0.0 },
            overworld_dimension(),
            4,
        );
        let player = PlayerCandidate {
            uuid,
            entity_id: 1001,
            username: "alice".to_owned(),
            position: Vec3::new(0.0, 64.0, 0.0),
            rotation: Rotation { yaw: 0.0, pitch: 0.0 },
            game_mode: GameMode::Survival,
            xp_level: 0,
            xp_points: 0,
            nbt: None,
        };
        Self {
            commands: ServerCommands::new(),
            state: lodestone_server::world_state::WorldStateHandle::new(),
            mobs: MobHandle::default(),
            block_entities: BlockEntityHandle::new(),
            stored: None,
            source,
            players: vec![player],
        }
    }

    /// Runs `text` as `source`; the feedback lines, or the refusal text.
    fn run_as(&self, source: &CommandSource, text: &str) -> Result<Vec<String>, String> {
        let world = CommandWorld {
            rules: &self.state as &(dyn RuleStore + Sync),
            players: &self.players,
            state: &self.state,
            mobs: Some(&self.mobs),
            border: None,
            access: None,
            blocks: self.stored.as_ref().map(|stored| stored as &dyn ChunkSource),
            block_entities: Some(&self.block_entities),
        };
        let outcome = self.commands.run(&world, source, text).expect("the root matched");
        match outcome.response {
            CommandResponse::Ran { feedback } => {
                if outcome.effects.is_empty() {
                    Ok(feedback)
                } else {
                    Ok([feedback, vec![format!("effects: {:?}", outcome.effects)]].concat())
                }
            }
            other => Err(format!("{other:?}")),
        }
    }

    fn run(&self, text: &str) -> Result<Vec<String>, String> {
        self.run_as(&self.source, text)
    }

    fn summon(&self, kind: &str, x: f64, y: f64, z: f64) {
        self.run(&format!("summon minecraft:{kind} {x:?} {y:?} {z:?}")).expect("summon ran");
    }

    fn entities(&self, kind: &str) -> Vec<CommandEntity> {
        self.mobs
            .with(|sim| sim.command_entities())
            .into_iter()
            .filter(|e| e.entity_type.path() == kind)
            .collect()
    }

    fn xs(&self, kind: &str) -> Vec<f64> {
        let mut xs: Vec<f64> = self.entities(kind).iter().map(|e| e.position.x).collect();
        xs.sort_by(f64::total_cmp);
        xs
    }

    fn tagged(&self, tag: &str) -> Vec<String> {
        let mut kinds: Vec<String> = self
            .mobs
            .with(|sim| sim.command_entities())
            .into_iter()
            .filter(|e| e.tags.iter().any(|t| t == tag))
            .map(|e| e.entity_type.path().to_owned())
            .collect();
        kinds.sort();
        kinds
    }
}

/// The refusal text of a command that was refused.
fn refusal(result: Result<Vec<String>, String>) -> String {
    let text = result.expect_err("the command was refused");
    text.strip_prefix("Refused { message: \"").and_then(|t| t.strip_suffix("\" }")).map(str::to_owned).unwrap_or(text)
}

fn only(lines: Result<Vec<String>, String>) -> String {
    let lines = lines.expect("the command ran");
    lines.into_iter().find(|l| !l.starts_with("effects:")).expect("a feedback line")
}

const ZOMBIE: &str = "@e[type=zombie,limit=1]";

#[test]
fn data_get_prints_the_summoned_health() {
    let server = Server::new();
    server.run("summon minecraft:zombie 5 64 0 {Health:12.5f}").unwrap();
    assert_eq!(
        only(server.run(&format!("data get entity {ZOMBIE} Health"))),
        "minecraft:zombie has the following entity data: 12.5f"
    );
    // 12.5 scaled by 2 and truncated.
    assert_eq!(only(server.run(&format!("data get entity {ZOMBIE} Health 2"))), "Health on minecraft:zombie after scale factor of 2.00 is 25");
}

#[test]
fn data_get_without_a_path_prints_the_whole_compound() {
    let server = Server::new();
    server.run("summon minecraft:zombie 5 64 0 {NoAI:1b}").unwrap();
    let text = only(server.run(&format!("data get entity {ZOMBIE}")));
    assert!(text.starts_with("minecraft:zombie has the following entity data: {"), "{text}");
    assert!(text.contains("NoAI: 1b"), "{text}");
    assert!(text.contains("id: \"minecraft:zombie\""), "{text}");
}

#[test]
fn data_get_of_a_missing_path_is_refused() {
    let server = Server::new();
    server.summon("zombie", 5.0, 64.0, 0.0);
    assert_eq!(
        refusal(server.run(&format!("data get entity {ZOMBIE} Nope"))).contains("Found no elements matching Nope"),
        true
    );
}

#[test]
fn the_nbt_selector_option_filters_by_subset_and_type() {
    let server = Server::new();
    server.run("summon minecraft:zombie 3 64 0 {NoAI:1b}").unwrap();
    server.run("summon minecraft:zombie 9 64 0").unwrap();
    server.run("summon minecraft:zombie 20 64 0 {NoAI:1b}").unwrap();
    let count = |sel: &str| -> usize {
        server.run(&format!("tag {sel} add marked")).map(|_| ()).ok();
        let n = server.tagged("marked").len();
        server.run("tag @e[tag=marked] remove marked").ok();
        n
    };
    assert_eq!(count("@e[type=zombie,nbt={NoAI:1b}]"), 2);
    assert_eq!(count("@e[type=zombie,nbt=!{NoAI:1b}]"), 1);
    // `NoAI:1` is an int, and the comparison is type strict.
    assert_eq!(count("@e[type=zombie,nbt={NoAI:1}]"), 0);
    assert_eq!(count("@e[type=zombie,nbt={Health:20.0f}]"), 3);
}

#[test]
fn data_merge_changes_behaviour_and_keeps_the_entity() {
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    let before = server.entities("zombie")[0].entity_id;
    assert_eq!(
        only(server.run(&format!("data merge entity {ZOMBIE} {{NoAI:1b,Health:7.0f}}"))),
        "Modified entity data of minecraft:zombie"
    );
    assert_eq!(server.entities("zombie")[0].entity_id, before);
    assert_eq!(
        only(server.run(&format!("data get entity {ZOMBIE} Health"))),
        "minecraft:zombie has the following entity data: 7.0f"
    );
    assert_eq!(server.run("tag @e[nbt={NoAI:1b}] add frozen").map(|_| ()), Ok(()));
    assert_eq!(server.tagged("frozen"), vec!["zombie"]);
    // The same merge again changes nothing.
    assert_eq!(
        refusal(server.run(&format!("data merge entity {ZOMBIE} {{NoAI:1b}}"))),
        "Nothing changed. The specified properties already have these values"
    );
}

#[test]
fn data_modify_set_moves_an_entity_and_renames_it() {
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    server.run(&format!("data modify entity {ZOMBIE} Pos[0] set value 11.5d")).unwrap();
    assert_eq!(server.xs("zombie"), vec![11.5]);
    server.run(&format!("data modify entity {ZOMBIE} CustomName set value \"Bob\"")).unwrap();
    assert_eq!(server.entities("zombie")[0].name, "Bob");
}

#[test]
fn players_cannot_be_modified_through_data() {
    let server = Server::new();
    assert_eq!(
        refusal(server.run("data merge entity @s {Health:1.0f}")),
        "Unable to modify player data"
    );
    // But their data reads, and the selector matches on it.
    assert!(only(server.run("data get entity @s UUID")).starts_with("alice has the following entity data: [I; "));
}

fn health_tenths(server: &Server) -> String {
    let line = only(server.run(&format!("data get entity {ZOMBIE} Health 10")));
    line.rsplit(" is ").next().expect("a value").to_owned()
}

#[test]
fn damage_reduces_health_by_the_amount() {
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    let line = only(server.run(&format!("damage {ZOMBIE} 6 minecraft:fall")));
    assert!(line.starts_with("Applied 6") && line.contains("minecraft:zombie"), "{line}");
    assert_eq!(health_tenths(&server), "140");
}

#[test]
fn damage_goes_through_armor() {
    // A zombie's base armor is 2. Armor 2 absorbs 2 / 25 = 8% on top of the
    // 4 * ... floor: min(20, max(2/5, 2 - 10/(8+4))) = 0.4 points of 25, so
    // 10 points of mob attack deal 10 * (1 - 0.4/25) = 9.84; 20 - 9.84 = 10.16.
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    server.run(&format!("damage {ZOMBIE} 10 minecraft:mob_attack")).unwrap();
    assert_eq!(health_tenths(&server), "101");
}

#[test]
fn damage_that_bypasses_armor_is_the_full_amount() {
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    server.run(&format!("damage {ZOMBIE} 10 minecraft:out_of_world")).unwrap();
    assert_eq!(health_tenths(&server), "100");
}

#[test]
fn damage_can_kill() {
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    server.run(&format!("damage {ZOMBIE} 100 minecraft:out_of_world")).unwrap();
    assert!(server.entities("zombie").is_empty(), "a lethal hit removes the zombie");
}

#[test]
fn damage_to_a_player_is_an_effect_for_their_connection() {
    let server = Server::new();
    let out = only(server.run("damage @s 4 minecraft:fall"));
    assert!(out.starts_with("Applied 4"), "{out}");
}

#[test]
fn ride_mounts_a_player_on_a_boat_and_dismounts() {
    let server = Server::new();
    let boat = server.mobs.with(|sim| sim.spawn_vehicle("minecraft:oak_boat".parse().unwrap(), Vec3::new(2.0, 64.0, 0.0), 0.0));
    let lines = server.run("ride @s mount @e[type=oak_boat,limit=1]").expect("mounted");
    assert_eq!(lines[0], "alice started riding minecraft:oak_boat");
    assert!(lines[1].contains(&format!("Mount {{ vehicle: {boat} }}")), "{lines:?}");
    assert_eq!(server.mobs.with(|sim| sim.vehicle_rider(boat)), Some(1001));

    assert_eq!(
        refusal(server.run("ride @s mount @e[type=oak_boat,limit=1]")),
        "alice is already riding minecraft:oak_boat"
    );
    let lines = server.run("ride @s dismount").expect("dismounted");
    assert_eq!(lines[0], "alice stopped riding minecraft:oak_boat");
    assert_eq!(server.mobs.with(|sim| sim.vehicle_rider(boat)), None);
    assert_eq!(server.mobs.with(|sim| sim.take_ejections_of(1001)), vec![boat]);
    assert_eq!(refusal(server.run("ride @s dismount")), "alice is not riding any vehicle");
}

#[test]
fn ride_refuses_players_and_self() {
    let server = Server::new();
    assert_eq!(refusal(server.run("ride @s mount @s")), "Players can't be ridden");
    server.summon("zombie", 3.0, 64.0, 0.0);
    assert_eq!(
        refusal(server.run(&format!("ride {ZOMBIE} mount {ZOMBIE}"))),
        "Can't mount entity on itself or any of its passengers"
    );
}

fn sign_at(server: &Server, pos: BlockPos, front: [&str; 4]) {
    let (_, sign) = lodestone_server::block_entity_for_item("minecraft:oak_sign").expect("a sign item");
    let lines = front.map(str::to_owned);
    // Written through the save form, the one shape the sign has outside the crate.
    let mut nbt = lodestone_server::chunk_nbt::block_entity_to_nbt(pos, &sign);
    if let lodestone_core::Nbt::Compound(fields) = &mut nbt {
        for (name, value) in fields.iter_mut() {
            if name == "front_text"
                && let lodestone_core::Nbt::Compound(side) = value
            {
                for (key, messages) in side.iter_mut() {
                    if key == "messages" {
                        *messages = lodestone_core::Nbt::List {
                            element_type: lodestone_core::NbtTag::String,
                            elements: lines.iter().cloned().map(lodestone_core::Nbt::String).collect(),
                        };
                    }
                }
            }
        }
    }
    server.block_entities.with(|registry| {
        registry.insert(pos, sign);
        registry.apply_entity_nbt(pos, &nbt).expect("the sign takes its own record");
        registry.take_client_dirty();
    });
}

const SIGN: BlockPos = BlockPos::new(3, 64, 3);

fn waxed(server: &Server) -> bool {
    server.block_entities.with(|registry| {
        let nbt = lodestone_server::chunk_nbt::block_entity_to_nbt(SIGN, registry.get(SIGN).expect("a sign"));
        matches!(nbt, lodestone_core::Nbt::Compound(fields) if fields.contains(&("is_waxed".to_owned(), lodestone_core::Nbt::Byte(1))))
    })
}

#[test]
fn data_get_block_prints_the_signs_save_record() {
    let server = Server::new();
    sign_at(&server, SIGN, ["LODESTONE", "PROBE", "", ""]);
    assert_eq!(
        only(server.run("data get block 3 64 3 front_text.messages[1]")),
        "3, 64, 3 has the following block data: \"PROBE\""
    );
    assert_eq!(only(server.run("data get block 3 64 3 x")), "3, 64, 3 has the following block data: 3");
    assert_eq!(only(server.run("data get block 3 64 3 x 2")), "x on block 3, 64, 3 after scale factor of 2.00 is 6");
    let whole = only(server.run("data get block 3 64 3"));
    assert!(whole.contains("id: \"minecraft:sign\"") && whole.contains("is_waxed: 0b"), "{whole}");
}

#[test]
fn data_on_a_cell_without_a_block_entity_is_refused() {
    let server = Server::new();
    for verb in ["get block 9 64 9", "merge block 9 64 9 {a:1b}", "remove block 9 64 9 a", "modify block 9 64 9 a set value 1b"] {
        assert_eq!(refusal(server.run(&format!("data {verb}"))), "The target block is not a block entity", "{verb}");
    }
}

#[test]
fn data_merge_block_rewrites_the_sign_and_queues_its_client_update() {
    let server = Server::new();
    sign_at(&server, SIGN, ["", "", "", ""]);
    assert!(!waxed(&server));
    assert_eq!(only(server.run("data merge block 3 64 3 {is_waxed:1b}")), "Modified block data of 3, 64, 3");
    assert!(waxed(&server), "the registry holds the merged record");
    let updates = server.block_entities.with(|registry| registry.take_client_dirty());
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert_eq!((updates[0].0, &updates[0].1), (SIGN, &BlockEntityKind::Sign));
    let lodestone_core::Nbt::Compound(tag) = &updates[0].2 else { panic!("a compound update tag") };
    assert!(tag.contains(&("is_waxed".to_owned(), lodestone_core::Nbt::Byte(1))), "{tag:?}");
    assert_eq!(
        refusal(server.run("data merge block 3 64 3 {is_waxed:1b}")),
        "Nothing changed. The specified properties already have these values"
    );
    assert!(server.block_entities.with(|registry| registry.take_client_dirty()).is_empty(), "a refused merge sends nothing");
}

#[test]
fn data_modify_and_remove_block_edit_single_paths() {
    let server = Server::new();
    sign_at(&server, SIGN, ["", "", "", ""]);
    server.run("data modify block 3 64 3 front_text.messages[2] set value \"HELLO\"").unwrap();
    assert_eq!(
        only(server.run("data get block 3 64 3 front_text.messages[2]")),
        "3, 64, 3 has the following block data: \"HELLO\""
    );
    server.run("data merge block 3 64 3 {is_waxed:1b}").unwrap();
    assert!(waxed(&server));
    assert_eq!(only(server.run("data remove block 3 64 3 is_waxed")), "Modified block data of 3, 64, 3");
    assert!(!waxed(&server), "a removed flag reads back as its default");
}

#[test]
fn data_merge_block_into_a_chest_wakes_comparators() {
    let server = Server::new();
    let at = BlockPos::new(4, 64, 4);
    server.block_entities.with(|registry| {
        registry.insert(at, BlockEntity::container("minecraft:chest"));
        assert_eq!(registry.take_analog_dirty(), vec![at], "the insert itself is noticed");
    });
    server
        .run("data merge block 4 64 4 {Items:[{Slot:0b,id:\"minecraft:diamond\",count:3}]}")
        .expect("the merge ran");
    let slots = server.block_entities.with(|registry| registry.get(at).expect("a chest").container_slots());
    assert_eq!(slots[0], Some(ItemStack::new("minecraft:diamond".parse().unwrap(), 3)));
    assert!(slots[1..].iter().all(Option::is_none));
    assert_eq!(server.block_entities.with(|registry| registry.take_analog_dirty()), vec![at]);
    assert!(
        server.block_entities.with(|registry| registry.take_client_dirty()).is_empty(),
        "a container streams no update"
    );
}

#[test]
fn a_block_entity_only_chunk_data_holds_is_read_and_enters_the_registry_on_write() {
    let mut server = Server::new();
    let at = BlockPos::new(6, 64, 6);
    server.stored = Some(StoredBlockEntities(vec![(at, BlockEntity::container("minecraft:chest"))]));
    assert!(only(server.run("data get block 6 64 6")).contains("id: \"minecraft:chest\""));
    assert!(server.block_entities.with(|registry| registry.get(at).is_none()), "a read does not register it");
    server.run("data merge block 6 64 6 {Items:[{Slot:2b,id:\"minecraft:stone\",count:1}]}").unwrap();
    let slots = server.block_entities.with(|registry| registry.get(at).expect("registered").container_slots());
    assert_eq!(slots[2], Some(ItemStack::new("minecraft:stone".parse().unwrap(), 1)));
}

fn ids(server: &Server) -> (i32, i32, i32) {
    let id = |kind: &str| server.entities(kind)[0].entity_id;
    (id("zombie"), id("cow"), id("oak_boat"))
}

fn riders(server: &Server, vehicle: i32) -> Vec<i32> {
    let passengers = server.mobs.with(|sim| sim.snapshots()).into_iter().find(|s| s.id == vehicle).expect("a snapshot").passengers;
    assert_eq!(passengers, server.mobs.with(|sim| sim.mob_riders_of(vehicle)));
    passengers
}

fn ride_fixture() -> Server {
    let server = Server::new();
    server.mobs.with(|sim| sim.spawn_vehicle("minecraft:oak_boat".parse().unwrap(), Vec3::new(2.0, 64.0, 0.0), 0.0));
    server.summon("zombie", 5.0, 64.0, 0.0);
    server.summon("cow", 8.0, 64.0, 0.0);
    server
}

#[test]
fn ride_seats_a_mob_on_a_boat_and_lists_it_as_a_passenger() {
    let server = ride_fixture();
    let (zombie, _, boat) = ids(&server);
    let lines = server.run("ride @e[type=zombie,limit=1] mount @e[type=oak_boat,limit=1]").expect("mounted");
    assert_eq!(lines, vec!["minecraft:zombie started riding minecraft:oak_boat"], "a mob needs no connection effect");
    assert_eq!(riders(&server, boat), vec![zombie]);
    // The next tick carries the rider to the boat's seat, which has no rise.
    server.mobs.with(|sim| sim.tick());
    let at = server.entities("zombie")[0].position;
    assert!((at.x - 2.0).abs() < 1e-6 && (at.z - 0.0).abs() < 1e-6, "{at:?}");
    assert!((at.y - 64.0).abs() < 1e-6, "{at:?}");
}

#[test]
fn ride_dismounts_a_mob_and_empties_the_passenger_list() {
    let server = ride_fixture();
    let (_, _, boat) = ids(&server);
    server.run("ride @e[type=zombie,limit=1] mount @e[type=oak_boat,limit=1]").unwrap();
    let lines = server.run("ride @e[type=zombie,limit=1] dismount").expect("dismounted");
    assert_eq!(lines, vec!["minecraft:zombie stopped riding minecraft:oak_boat"]);
    assert!(riders(&server, boat).is_empty());
    assert_eq!(refusal(server.run("ride @e[type=zombie,limit=1] dismount")), "minecraft:zombie is not riding any vehicle");
}

#[test]
fn ride_mounts_a_mob_on_another_mob_and_refuses_the_cycle() {
    let server = ride_fixture();
    let (zombie, cow, _) = ids(&server);
    server.run("ride @e[type=zombie,limit=1] mount @e[type=cow,limit=1]").unwrap();
    assert_eq!(riders(&server, cow), vec![zombie]);
    assert_eq!(
        refusal(server.run("ride @e[type=cow,limit=1] mount @e[type=zombie,limit=1]")),
        "Can't mount entity on itself or any of its passengers"
    );
    assert_eq!(
        refusal(server.run("ride @e[type=zombie,limit=1] mount @e[type=oak_boat,limit=1]")),
        "minecraft:zombie is already riding minecraft:cow"
    );
}

#[test]
fn ride_refuses_a_rider_that_is_not_a_mob_and_a_player_on_a_crowded_vehicle() {
    let server = ride_fixture();
    assert_eq!(
        refusal(server.run("ride @e[type=oak_boat,limit=1] mount @e[type=cow,limit=1]")),
        "minecraft:oak_boat couldn't start riding minecraft:cow"
    );
    server.run("ride @e[type=zombie,limit=1] mount @e[type=oak_boat,limit=1]").unwrap();
    assert_eq!(
        refusal(server.run("ride @s mount @e[type=oak_boat,limit=1]")),
        "alice couldn't start riding minecraft:oak_boat"
    );
}

#[test]
fn a_rider_is_released_when_its_vehicle_is_removed() {
    let server = ride_fixture();
    let (zombie, _, _) = ids(&server);
    server.run("ride @e[type=zombie,limit=1] mount @e[type=oak_boat,limit=1]").unwrap();
    server.run("kill @e[type=oak_boat]").unwrap();
    server.mobs.with(|sim| sim.tick());
    assert_eq!(server.mobs.with(|sim| sim.mob_rider_vehicle(zombie)), None);
}
