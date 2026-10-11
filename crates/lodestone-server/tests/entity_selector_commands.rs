//! **`@e` and `@n` reach every entity the mob simulation owns, and the commands
//! that take entity targets act on them.**
//!
//! Every command runs through the production [`ServerCommands`] tree against a
//! live [`MobHandle`]; entities are summoned by `/summon` (mobs) or placed
//! with the sim's own spawn calls (items), and observed through
//! [`MobSim::command_entities`](lodestone_server::MobSim::command_entities), the
//! same enumeration selectors resolve over.
//!
//! The expected values are arithmetic on the positions placed below, never read
//! back from the code under test. Alice stands at (0, 64, 0).

use lodestone_model::{GameMode, ItemStack, Rotation, Vec3};
use lodestone_server::commands::registrar::RuleStore;
use lodestone_server::commands::{
    CommandSource, CommandWorld, PlayerCandidate, ServerCommands, SourceEntity, overworld_dimension,
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
            block_entities: None,
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

    fn drop_item(&self, at: Vec3) {
        self.mobs.with(|sim| {
            sim.spawn_item(
                &ItemStack::new("minecraft:dirt".parse().unwrap(), 1),
                at,
                Vec3::new(0.0, 0.0, 0.0),
                lodestone_entity::item_entity::ItemLifecycle::default(),
            );
        });
    }
}

/// Zombies at x = 10, 3 and 7 are summoned in that order, so the nearest (x = 3)
/// is neither first nor last in insertion order and the furthest (x = 10) is
/// first: a selector that ignored `sort` would tag the wrong one in at least one
/// of the two runs.
#[test]
fn sort_and_limit_pick_the_expected_zombie() {
    let server = Server::new();
    for x in [10.0, 3.0, 7.0] {
        server.summon("zombie", x, 64.0, 0.0);
    }
    server.summon("cow", 1.0, 64.0, 0.0);

    server.run("execute as @e[type=zombie,limit=1,sort=nearest] run tag @s add near").expect("ran");
    server.run("execute as @e[type=zombie,limit=1,sort=furthest] run tag @s add far").expect("ran");

    let position_of = |tag: &str| -> Vec<f64> {
        server
            .entities("zombie")
            .iter()
            .filter(|e| e.tags.iter().any(|t| t == tag))
            .map(|e| e.position.x)
            .collect()
    };
    assert_eq!(position_of("near"), [3.0]);
    assert_eq!(position_of("far"), [10.0]);
    assert!(server.tagged("near").iter().all(|k| k == "zombie"), "the cow is untouched");

    // `@n` is nearest-with-limit-1 over every entity kind (alice herself is nearer than any
    // mob, hence the type filter): the cow at x = 1.
    server.run("tag @n[type=!player] add closest").expect("ran");
    assert_eq!(server.tagged("closest"), ["cow"]);
}

#[test]
fn kill_removes_every_dropped_item_and_only_those() {
    let server = Server::new();
    server.drop_item(Vec3::new(2.0, 64.0, 0.0));
    server.drop_item(Vec3::new(-5.0, 64.0, 3.0));
    server.summon("cow", 4.0, 64.0, 0.0);
    assert_eq!(server.entities("item").len(), 2);

    let feedback = server.run("kill @e[type=item]").expect("kill ran");
    assert_eq!(feedback, ["Killed 2 entities"]);
    assert!(server.entities("item").is_empty(), "items are gone");
    assert_eq!(server.entities("cow").len(), 1, "the cow survives");

    // Nothing left to match is the reference refusal, not a silent success.
    assert_eq!(server.run("kill @e[type=item]").unwrap_err().contains("No entity was found"), true);
}

#[test]
fn kill_kills_a_mob_through_its_death() {
    let server = Server::new();
    server.summon("pig", 6.0, 64.0, 0.0);
    server.summon("pig", -6.0, 64.0, 0.0);
    server.run("kill @e[type=pig,limit=1,sort=nearest,x=7]").expect("kill ran");
    // `x=7` moves the origin, so the pig at x = 6 is the nearest and dies.
    assert_eq!(server.xs("pig"), [-6.0]);
}

#[test]
fn tp_moves_every_matching_mob_relative_to_the_source() {
    let server = Server::new();
    server.summon("cow", 4.0, 64.0, 4.0);
    server.summon("cow", -6.0, 64.0, 9.0);
    server.summon("pig", 8.0, 64.0, 8.0);

    let feedback = server.run("tp @e[type=cow] ~ ~5 ~").expect("tp ran");
    assert_eq!(feedback.len(), 1);
    // `~ ~5 ~` resolves against alice at (0, 64, 0), for every target.
    for cow in server.entities("cow") {
        assert_eq!(cow.position, Vec3::new(0.0, 69.0, 0.0));
    }
    assert_eq!(server.entities("pig")[0].position, Vec3::new(8.0, 64.0, 8.0), "the pig stays");
}

#[test]
fn execute_as_at_a_mob_moves_each_mob_from_its_own_position() {
    let server = Server::new();
    server.summon("cow", 4.0, 64.0, 4.0);
    server.summon("cow", -6.0, 64.0, 9.0);
    server.run("execute as @e[type=cow] at @s run tp @s ~ ~5 ~").expect("ran");
    let mut moved: Vec<(f64, f64, f64)> =
        server.entities("cow").iter().map(|e| (e.position.x, e.position.y, e.position.z)).collect();
    moved.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert_eq!(moved, [(-6.0, 69.0, 9.0), (4.0, 69.0, 4.0)]);
}

#[test]
fn tp_to_a_mob_destination_and_rotation_options() {
    let server = Server::new();
    server.summon("cow", 4.0, 64.0, 4.0);
    server.summon("pig", 20.0, 70.0, -3.0);
    server.run("tp @e[type=cow] @e[type=pig,limit=1]").expect("tp ran");
    assert_eq!(server.entities("cow")[0].position, Vec3::new(20.0, 70.0, -3.0));

    // A yaw argument turns the mob; `y_rotation=` finds it by that yaw.
    server.run("tp @e[type=cow] 0 64 0 90 0").expect("tp ran");
    assert_eq!(server.entities("cow")[0].rotation.yaw, 90.0);
    server.run("tag @e[y_rotation=80..100] add east").expect("ran");
    assert_eq!(server.tagged("east"), ["cow"]);
    assert!(server.run("tag @e[y_rotation=-100..-80] add west").unwrap_err().contains("No entity"));
}

#[test]
fn tagging_pigs_through_execute_makes_them_selectable_by_tag() {
    let server = Server::new();
    for x in [2.0, 5.0, 9.0] {
        server.summon("pig", x, 64.0, 0.0);
    }
    server.summon("cow", 3.0, 64.0, 0.0);

    server.run("execute as @e[type=pig] run tag @s add x").expect("ran");
    assert_eq!(server.tagged("x"), ["pig", "pig", "pig"]);

    // The selector's own count, observed as `/kill`'s feedback.
    assert_eq!(server.run("kill @e[tag=x,type=!cow]").expect("kill ran"), ["Killed 3 entities"]);
    assert_eq!(server.entities("cow").len(), 1);
    assert!(server.entities("pig").is_empty());
}

#[test]
fn tag_add_remove_list_and_the_empty_tag_form() {
    let server = Server::new();
    server.summon("cow", 4.0, 64.0, 0.0);
    server.summon("cow", 5.0, 64.0, 0.0);
    assert_eq!(server.run("tag @e[type=cow] add a").unwrap(), ["Added tag 'a' to 2 entities"]);
    assert!(server.run("tag @e[type=cow] add a").unwrap_err().contains("already has the tag"));
    server.run("tag @e[type=cow,limit=1] add b").expect("ran");
    // The single entity's name falls back to its type id.
    assert_eq!(server.run("tag @e[type=cow,tag=b] list").unwrap(), ["minecraft:cow has 2 tag(s): a, b"]);
    // `tag=` with no name selects the untagged, and both cows hold tags.
    assert!(server.run("tag @e[type=cow,tag=] add z").unwrap_err().contains("No entity"));
    server.run("tag @e[type=cow] remove a").expect("ran");
    assert!(server.run("tag @e[type=cow] remove a").unwrap_err().contains("does not have this tag"));
    assert_eq!(server.tagged("b").len(), 1);
}

#[test]
fn distance_dx_type_tags_names_and_inversion_narrow_non_players() {
    let server = Server::new();
    // Distances from alice: 5 (3-4-0), 10, and 5.0 exactly for the husk.
    server.summon("cow", 3.0, 64.0, 4.0);
    server.summon("cow", 6.0, 64.0, 8.0);
    server.summon("zombie", 0.0, 64.0, 5.0);
    server.summon("husk", 5.0, 64.0, 0.0);
    server.summon("pig", 30.0, 64.0, 0.0);

    assert_eq!(server.run("kill @e[type=cow,distance=..5]").unwrap(), ["Killed minecraft:cow"]);
    assert_eq!(server.xs("cow"), [6.0], "only the cow at distance 5 died; distance is inclusive");

    // `#minecraft:zombies` holds zombie and husk (the reference tag file), not the pig.
    server.run("tag @e[type=#minecraft:zombies] add undead").expect("ran");
    assert_eq!(server.tagged("undead"), ["husk", "zombie"]);
    // Inverting the tag selects everything outside it; the dead cow left loot behind.
    server.run("tag @e[type=!#zombies,type=!item,tag=!undead] add living").expect("ran");
    assert_eq!(server.tagged("living"), ["cow", "pig"]);

    // `dx` is a box from the origin: x in [0, 5 + 1].
    server.run("tag @e[dx=5,dy=0,dz=0] add box").expect("ran");
    assert_eq!(server.tagged("box"), ["husk"]);

    // `name=` compares the custom name; `!` inverts it.
    let pig = server.entities("pig")[0].entity_id;
    server.mobs.with(|sim| assert!(sim.apply_name_tag(pig, lodestone_core::Nbt::String("Bob".to_owned()))));
    server.run("tag @e[name=Bob] add named").expect("ran");
    assert_eq!(server.tagged("named"), ["pig"]);
    server.run("tag @e[name=!Bob,type=!player,type=!item,tag=!undead] add unnamed").expect("ran");
    assert_eq!(server.tagged("unnamed"), ["cow"]);
}

#[test]
fn effects_reach_living_mobs_and_fail_for_items() {
    let server = Server::new();
    server.summon("zombie", 3.0, 64.0, 0.0);
    server.drop_item(Vec3::new(1.0, 64.0, 0.0));
    server.run("effect give @e[type=zombie] speed 30 1").expect("effect ran");
    let id = server.entities("zombie")[0].entity_id;
    let speed = server.mobs.with(|sim| sim.get(id).unwrap().effects().active());
    assert!(speed.iter().any(|(name, amplifier)| name.ends_with("speed") && *amplifier == 1), "{speed:?}");

    // An item cannot hold an effect: the command fails rather than reporting success.
    assert!(server.run("effect give @e[type=item] speed 30").unwrap_err().contains("Unable to apply"));

    server.run("effect clear @e[type=zombie]").expect("clear ran");
    assert!(server.mobs.with(|sim| sim.get(id).unwrap().effects().is_empty()));
    assert!(server.run("effect clear @e[type=zombie]").unwrap_err().contains("no effects"));
}

#[test]
fn player_only_commands_reject_a_mob_source_and_entity_selectors() {
    let server = Server::new();
    server.summon("pig", 4.0, 64.0, 0.0);
    let pig = server.entities("pig")[0].clone();

    // `@s` resolved by a pig: the pig is not a player, so `/gamemode` has no one to act on.
    let as_pig = CommandSource {
        entity: Some(SourceEntity { uuid: pig.uuid, entity_id: pig.entity_id, username: "pig".to_owned() }),
        name: "pig".to_owned(),
        position: pig.position,
        rotation: pig.rotation,
        dimension: overworld_dimension(),
        permission_level: 4,
        anchor: Default::default(),
    };
    let refused = server.run_as(&as_pig, "gamemode creative @s").unwrap_err();
    assert!(refused.contains("No player was found"), "{refused}");
    // The same command from alice works, so the refusal above is about the pig.
    assert!(server.run("gamemode creative @s").is_ok());

    // A players-only argument refuses an entity selector at parse time.
    let parse = server.run("gamemode creative @e[type=pig]").unwrap_err();
    assert!(parse.contains("Only players may be affected"), "{parse}");
    let give = server.run("give @e[type=pig] minecraft:dirt").unwrap_err();
    assert!(give.contains("Only players may be affected"), "{give}");

    // Through `execute as`, the pig reaches the same refusal and nothing is applied.
    let nested = server.run("execute as @e[type=pig] run gamemode creative @s").expect("execute ran");
    assert!(nested.iter().all(|line| !line.contains("effects:")), "no game-mode effect: {nested:?}");
}

/// One entity from each store the sim keeps (loose items, orbs, primed TNT,
/// boats, cushions, thrown eyes, arrows and a hanging frame) is selected by
/// `@e`, moved by `/tp` to a point none of them started at, and removed by
/// `/kill`.
#[test]
fn every_kind_of_entity_is_selectable_movable_and_killable() {
    let server = Server::new();
    server.mobs.with(|sim| {
        let at = |x: f64| Vec3::new(x, 64.0, 0.0);
        sim.spawn_orb(3, at(1.0), Vec3::new(0.0, 0.0, 0.0));
        sim.spawn_tnt(at(2.0), 80);
        sim.spawn_vehicle("minecraft:oak_boat".parse().unwrap(), at(3.0), 0.0);
        sim.spawn_cushion(at(4.0), 0.0, 0);
        sim.spawn_eye_of_ender(at(5.0), at(50.0));
        sim.spawn_projectile(
            "minecraft:arrow".parse().unwrap(),
            lodestone_entity::projectile::Projectile::arrow(at(6.0), Vec3::new(0.0, 0.0, 0.0)),
        );
    });
    server.drop_item(Vec3::new(7.0, 64.0, 0.0));
    server.run("summon minecraft:item_frame 8 65 0 {Facing:5b}").expect("summon ran");

    let all = || server.mobs.with(|sim| sim.command_entities());
    let kinds: Vec<String> = all().iter().map(|e| e.entity_type.path().to_owned()).collect();
    for expected in ["experience_orb", "tnt", "oak_boat", "cushion", "eye_of_ender", "arrow", "item", "item_frame"] {
        assert!(kinds.iter().any(|k| k == expected), "{expected} missing from {kinds:?}");
    }

    // Every kind but the frame can be moved; the frame is reported as skipped, not moved.
    let moved = server.run("tp @e[type=!player,type=!item_frame] 0 80 0").expect("tp ran");
    assert_eq!(moved, ["Teleported 7 entities to 0.50, 80.00, 0.50"]);
    for entity in all().iter().filter(|e| e.entity_type.path() != "item_frame") {
        assert_eq!(entity.position, Vec3::new(0.5, 80.0, 0.5), "{}", entity.entity_type);
    }

    server.run("kill @e[type=!player]").expect("kill ran");
    assert_eq!(all(), [], "nothing but players is left");
}

/// Tags ride the saved entity record, so a restored mob keeps them and is
/// found by `tag=` again.
#[test]
fn tags_survive_a_save_and_restore() {
    let server = Server::new();
    server.summon("cow", 4.0, 64.0, 0.0);
    server.summon("pig", 8.0, 64.0, 0.0);
    server.run("tag @e[type=cow] add keep").expect("ran");

    let saved = server.mobs.with(|sim| sim.saved_entities());
    let reloaded = Server::new();
    reloaded.mobs.with(|sim| sim.restore_saved(&saved));
    assert_eq!(reloaded.tagged("keep"), ["cow"]);
    assert_eq!(reloaded.entities("pig")[0].tags, Vec::<String>::new());
}

/// A custom name is a text component, and a string component is literal text, so
/// `CustomName:"Bob"` is named Bob while `CustomName:'"Bob"'` would carry the quotes.
///
/// `/summon` data goes through the load path's record reader: a named, tagged
/// `NoAI` zombie is found by `@e[name=Bob,tag=a]` and holds its horizontal
/// position while an identical zombie without `NoAI` closes on the player.
///
/// The floor top is y = 0; both zombies start 6 blocks east of a perceived
/// player at (0.5, 0, 0.5), the unmodified one at z = 10.5 and the `NoAI` one
/// at z = 0.5. 300 ticks is the same order the melee gate allots for an 8 block
/// approach.
#[test]
fn summon_nbt_names_tags_and_freezes_a_zombie() {
    use lodestone_server::{ChunkWorld, PerceivedPlayer, PlayerIdentity, PlayerPerception, StoneFloorSource};

    let mut server = Server::new();
    server.mobs = MobHandle::new(ChunkWorld::from_source(&StoneFloorSource::new(-64, 128, 0), -1..=1, -1..=1));
    server
        .run("summon minecraft:zombie 6.5 0 0.5 {CustomName:\"Bob\",Tags:[\"a\"],NoAI:1b}")
        .expect("summon ran");
    server.run("summon minecraft:zombie 6.5 0 10.5").expect("control summon ran");

    let found = server.run("tag @e[name=Bob,tag=a] add found").expect("selector ran");
    assert_eq!(found, ["Added tag 'found' to Bob"], "the named, tagged zombie is the one selected: {found:?}");
    assert_eq!(server.tagged("found"), ["zombie"]);

    server.mobs.with(|sim| {
        sim.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity { uuid: Uuid::from_u128(7), entity_id: 77 }),
            perception: PlayerPerception {
                position: Vec3::new(0.5, 0.0, 0.5),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, -1.0),
            },
        }]);
        for _ in 0..300 {
            sim.tick();
        }
    });

    let zombies = server.entities("zombie");
    let bob = zombies.iter().find(|z| z.name == "Bob").expect("Bob is still there");
    assert_eq!((bob.position.x, bob.position.z), (6.5, 0.5), "NoAI holds position");
    let control = zombies.iter().find(|z| z.name.is_empty()).expect("control is still there");
    assert!(control.position.x < 5.5, "the unfrozen control walked at least a block: x = {}", control.position.x);
}

/// Every common and mob field `/summon` accepts survives a save and a restore
/// into a fresh sim, because both go through one record reader. Expected values
/// are the literals typed in the command.
#[test]
fn summon_nbt_fields_apply_and_survive_a_save_and_restore() {
    let server = Server::new();
    server
        .run(
            "summon minecraft:zombie 3.5 64 3.5 {NoGravity:1b,Silent:1b,Invulnerable:1b,\
             active_effects:[{id:\"minecraft:speed\",amplifier:1b,duration:600}],\
             equipment:{mainhand:{id:\"minecraft:iron_sword\",count:1},head:{id:\"minecraft:iron_helmet\",count:1}},\
             Bogus:5b}",
        )
        .expect("unknown keys are ignored");

    let check = |server: &Server| {
        server.mobs.with(|sim| {
            let mob = sim
                .command_entities()
                .into_iter()
                .find(|e| e.entity_type.path() == "zombie")
                .and_then(|e| sim.get(e.entity_id).map(|_| e.entity_id))
                .expect("the zombie exists");
            let mob = sim.get(mob).expect("mob");
            assert!(mob.is_silent() && mob.is_invulnerable());
            assert!(mob.is_gravity_disabled() && !mob.is_ai_disabled());
            assert_eq!(mob.effects().active(), [("minecraft:speed", 1)]);
            let worn: Vec<(String, String)> = mob
                .equipment_snapshot()
                .into_iter()
                .filter_map(|e| e.item.map(|item| (format!("{:?}", e.slot), item.item.to_string())))
                .collect();
            assert_eq!(
                worn,
                [
                    ("MainHand".to_owned(), "minecraft:iron_sword".to_owned()),
                    ("Head".to_owned(), "minecraft:iron_helmet".to_owned()),
                ]
            );
        });
    };
    check(&server);

    let saved = server.mobs.with(|sim| sim.saved_entities());
    let reloaded = Server::new();
    reloaded.mobs.with(|sim| sim.restore_saved(&saved));
    check(&reloaded);

    // Gravity off holds the mob in mid-air; an Invulnerable mob takes no hit.
    reloaded.mobs.with(|sim| {
        for _ in 0..40 {
            sim.tick();
        }
        let id = sim.command_entities()[0].entity_id;
        assert_eq!(sim.command_entities()[0].position.y, 64.0, "NoGravity holds the mob in the air");
        assert_eq!(sim.get_mut(id).expect("mob").apply_damage(5.0, Default::default()), 0.0);
    });

    // A dead-on-arrival record is refused instead of leaving a corpse.
    assert!(server.run("summon minecraft:zombie ~ ~ ~ {Health:0f}").is_err());
}
