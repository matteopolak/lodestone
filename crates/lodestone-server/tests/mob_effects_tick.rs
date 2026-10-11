//! **Status effects on mobs tick by the reference rules.** Commands run through the
//! production tree against a live sim; expected values are arithmetic on the
//! reference intervals (poison fires every 25 ticks remaining at amplifier 0).

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

    fn summon(&self, kind: &str, x: f64, y: f64, z: f64) -> CommandEntity {
        self.run(&format!("summon minecraft:{kind} {x:?} {y:?} {z:?}")).expect("summon ran");
        self.mobs.with(|sim| sim.command_entities()).into_iter().find(|e| e.entity_type.path() == kind).expect("summoned")
    }

    fn health(&self, e: &CommandEntity) -> f32 {
        self.mobs.with(|sim| sim.get(e.entity_id).map(|m| m.health())).expect("alive")
    }

    fn tick(&self, n: usize) {
        self.mobs.with(|sim| (0..n).for_each(|_| sim.tick()));
    }
}

/// Poison at amplifier 0 hurts once per 25 remaining ticks (100, 75, 50, 25
/// remaining: four hits of 1.0), then expires. A cow has 10 health.
#[test]
fn poison_hurts_a_summoned_cow_on_its_interval_then_expires() {
    let server = Server::new();
    let cow = server.summon("cow", 4.0, 64.0, 0.0);
    assert_eq!(server.health(&cow), 10.0);
    server.run(&"effect give @e[type=cow] poison 5 0").expect("effect ran");
    assert!(server.mobs.with(|sim| sim.get(cow.entity_id).is_some_and(|m| !m.effects().is_empty())));
    server.tick(130);
    assert_eq!(server.health(&cow), 6.0, "four hits of 1.0 over the 100 tick duration");
    assert!(
        server.mobs.with(|sim| sim.get(cow.entity_id).is_some_and(|m| m.effects().is_empty())),
        "the effect expired"
    );
    server.tick(100);
    assert_eq!(server.health(&cow), 6.0, "no further damage once expired");
}

/// Undead ignore poison (a husk, unlike a zombie, does not burn in daylight), so it keeps full health and stores nothing.
#[test]
fn a_husk_ignores_poison() {
    let server = Server::new();
    let zombie = server.summon("husk", 4.0, 64.0, 0.0);
    server.run(&"effect give @e[type=husk] poison 5 0").ok();
    server.tick(130);
    assert_eq!(server.health(&zombie), 20.0);
    assert!(server.mobs.with(|sim| sim.get(zombie.entity_id).is_some_and(|m| m.effects().is_empty())));
}

/// `Silent` and `NoGravity` set through `/summon` reach the entity-data a
/// client is sent; a plain mob streams neither.
#[test]
fn silent_and_no_gravity_are_streamed_as_entity_data() {
    use lodestone_server::MetadataField;
    let server = Server::new();
    server.run("summon minecraft:cow 2 64 0 {Silent:1b,NoGravity:1b}").expect("summon ran");
    server.run("summon minecraft:pig 4 64 0").expect("summon ran");
    let fields = |kind: &str| -> Vec<MetadataField> {
        server
            .mobs
            .with(|sim| sim.snapshots())
            .into_iter()
            .find(|s| s.entity_type.path() == kind)
            .map(|s| s.metadata)
            .expect("snapshot")
    };
    let silent = |f: &[MetadataField]| f.iter().any(|m| *m == MetadataField::Silent(true));
    let floats = |f: &[MetadataField]| f.iter().any(|m| *m == MetadataField::NoGravity(true));
    let cow = fields("cow");
    assert!(silent(&cow) && floats(&cow), "{cow:?}");
    let pig = fields("pig");
    assert!(!silent(&pig) && !floats(&pig), "{pig:?}");
}
