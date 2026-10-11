//! **`/data` on entities, the `nbt=` selector option, `/damage` and `/ride` act
//! on the entities the mob simulation owns.**
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
use lodestone_server::{CommandEntity, CommandResponse, MobHandle};
use uuid::Uuid;

struct Server {
    commands: ServerCommands,
    state: lodestone_server::world_state::WorldStateHandle,
    mobs: MobHandle,
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
            blocks: None,
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
