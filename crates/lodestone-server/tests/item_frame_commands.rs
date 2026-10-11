//! **`/summon` makes item frames from reference data, `@e` finds them, and
//! `/kill` removes them.**
//!
//! Every command runs through the production [`ServerCommands`] tree against a
//! live [`MobHandle`]; the observations are the entity snapshots the streamer
//! diffs, so a frame that exists only inside the sim and never streams would
//! fail here.
//!
//! The expected values are the reference's: the spawn packet carries the cell's
//! integer corner, `Facing` is the 3D data value (down 0, up 1, north 2, south
//! 3, west 4, east 5), a missing `Facing` hangs down, and the invisible flag is
//! shared-flags bit 0x20. A summoned east-facing frame in cell (6, 65, 3) has
//! its box centre at x 6 + 0.5 - 0.46875, so alice at (0, 64, 0) is
//! sqrt(6.03125^2 + 1.5^2 + 3.5^2) = 7.1325 blocks from it.

use lodestone_model::{GameMode, Rotation, Vec3};
use lodestone_server::commands::registrar::RuleStore;
use lodestone_server::commands::{
    CommandSource, CommandWorld, PlayerCandidate, ServerCommands, overworld_dimension,
};
use lodestone_server::{EntitySnapshot, EntitySource, MetadataField, MobHandle};
use uuid::Uuid;

fn alice() -> (CommandSource, Vec<PlayerCandidate>) {
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
    (source, vec![player])
}

struct Server {
    commands: ServerCommands,
    state: lodestone_server::world_state::WorldStateHandle,
    mobs: MobHandle,
    source: CommandSource,
    players: Vec<PlayerCandidate>,
}

impl Server {
    fn new() -> Self {
        let (source, players) = alice();
        Self {
            commands: ServerCommands::new(),
            state: lodestone_server::world_state::WorldStateHandle::new(),
            mobs: MobHandle::default(),
            source,
            players,
        }
    }

    /// Runs `text`; `Ok(feedback)` when it ran, `Err(text)` when it was refused.
    fn run(&self, text: &str) -> Result<Vec<String>, String> {
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
        let outcome = self.commands.run(&world, &self.source, text).expect("the root matched");
        match outcome.response {
            lodestone_server::CommandResponse::Ran { feedback } => Ok(feedback),
            other => Err(format!("{other:?}")),
        }
    }

    fn frames(&self) -> Vec<EntitySnapshot> {
        let mut frames: Vec<_> = self
            .mobs
            .snapshots()
            .into_iter()
            .filter(|s| s.entity_type.path().ends_with("item_frame"))
            .collect();
        frames.sort_by_key(|s| s.id);
        frames
    }
}

#[test]
fn summon_creates_frames_from_reference_data() {
    let server = Server::new();
    server
        .run(
            "summon minecraft:item_frame 6 65 3 \
             {Facing:5b,Item:{id:\"minecraft:diamond\",count:1},ItemRotation:3b,Invisible:1b}",
        )
        .expect("summon ran");
    let [frame] = server.frames().try_into().expect("one frame streams");
    assert_eq!(frame.entity_type.to_string(), "minecraft:item_frame");
    assert_eq!(frame.position, Vec3::new(6.0, 65.0, 3.0), "the spawn packet carries the cell corner");
    assert_eq!(frame.object_data, 5, "east is 5");
    assert!(frame.metadata.contains(&MetadataField::HangingFacing(5)));
    assert!(frame.metadata.contains(&MetadataField::FrameRotation(3)));
    assert!(frame.metadata.contains(&MetadataField::SharedFlags(0x20)), "invisible");
    assert!(
        frame.metadata.iter().any(|f| matches!(
            f,
            MetadataField::FrameItem { item, count: 1, .. } if item.to_string() == "minecraft:diamond"
        )),
        "{:?}",
        frame.metadata
    );

    // No data: the glowing kind, empty, hanging down.
    server.run("summon minecraft:glow_item_frame 2 70 2").expect("summon ran");
    let frames = server.frames();
    let glow = frames.iter().find(|s| s.entity_type.path() == "glow_item_frame").expect("glow frame streams");
    assert_eq!(glow.position, Vec3::new(2.0, 70.0, 2.0));
    assert_eq!(glow.object_data, 0, "a missing Facing hangs down");
    assert!(glow.metadata.contains(&MetadataField::FrameRotation(0)));

    // Entity data is not frame-only: a cow takes it too.
    server.run("summon minecraft:cow ~ ~ ~ {NoAI:1b}").expect("a cow takes entity data");
}

#[test]
fn a_fixed_summoned_frame_ignores_a_survival_hit() {
    let server = Server::new();
    server
        .run("summon minecraft:item_frame 6 65 3 {Facing:5b,Fixed:1b,Item:{id:\"minecraft:stick\",count:1}}")
        .expect("summon ran");
    let id = server.frames()[0].id;
    let survival = server.mobs.with(|sim| sim.hurt_frame(id, false));
    assert_eq!(format!("{survival:?}"), "Some(Ignored)");
    let creative = server.mobs.with(|sim| sim.hurt_frame(id, true));
    assert!(format!("{creative:?}").starts_with("Some(Broken"), "{creative:?}");
}

#[test]
fn selectors_find_frames_by_type_and_distance_and_kill_removes_them() {
    let server = Server::new();
    server.run("summon minecraft:item_frame 6 65 3 {Facing:5b}").expect("summon ran");
    server.run("summon minecraft:item_frame 30 65 3 {Facing:5b,Fixed:1b}").expect("summon ran");
    server.run("summon minecraft:glow_item_frame 2 70 2 {Facing:1b}").expect("summon ran");
    assert_eq!(server.frames().len(), 3);

    // The near frame is 7.1325 blocks from alice: a 7.2 radius takes it and a 7.0 radius does not.
    let missed = server.run("kill @e[type=minecraft:item_frame,distance=..7]").expect_err("nothing in reach");
    assert!(missed.contains("No entity was found"), "{missed}");
    assert_eq!(server.frames().len(), 3);
    let feedback = server.run("kill @e[type=minecraft:item_frame,distance=..7.2]").expect("one frame in reach");
    assert_eq!(feedback, vec!["Killed minecraft:item_frame".to_owned()]);
    assert_eq!(server.frames().len(), 2, "the near frame is gone from the stream");

    // Players-only selectors never touch frames.
    server.run("kill @a").expect("alice");
    assert_eq!(server.frames().len(), 2);

    // The remaining item frame is the far fixed one; `/kill` ignores `Fixed`.
    let feedback = server.run("kill @e[type=item_frame]").expect("the far frame");
    assert_eq!(feedback, vec!["Killed minecraft:item_frame".to_owned()]);
    let left = server.frames();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].entity_type.path(), "glow_item_frame", "the glowing kind is another type");

    let feedback = server.run("kill @e[type=glow_item_frame]").expect("the glow frame");
    assert_eq!(feedback, vec!["Killed minecraft:glow_item_frame".to_owned()]);
    assert!(server.frames().is_empty());
}
