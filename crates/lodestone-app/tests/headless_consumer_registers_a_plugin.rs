//! The conformance gate for milestone zero: **can an external consumer register
//! a plugin into the client's real composed `App`, and does the plugin's own
//! behaviour actually happen?**
//!
//! This is a stronger claim than "`add_plugins` compiles". The plugin under test
//! is [`lodestone_autopilot::AutopilotPlugin`], which was removed from the
//! shell's tuple entirely (`crates/lodestone-shell/Cargo.toml`'s note where the
//! dependency line used to be), so it is a genuinely external plugin rather than
//! a fixture written for this test. The assertion is its own reason to exist:
//! setting an [`AutopilotGoal`] must move the local player to it.
//!
//! # How this differs from the plugin's own gate
//!
//! `crates/plugins/lodestone-autopilot/tests/drives_to_goal.rs` builds
//! `App::new()` + `(CorePlugin, LocalPlayerPlugin, AutopilotPlugin)` — a
//! *partial* stack, chosen because at the time the shell's composition was
//! unreachable and the plugin's own crate could only approximate it. This test
//! adapts that gate's fixtures (the same [`FixtureAdapter`], the same
//! `FlatFloor` physics world, the same flat [`ChunkWorld`], the same arrival
//! assertions) and runs them through [`lodestone_app::client_app`] instead: the
//! **whole** production plugin set, including `ControllerPlugin`'s
//! `TickSet::Input`/`Send`, `SessionHudPlugin`'s `TickSet::Animate`, and both
//! net-thread folds.
//!
//! That difference is load-bearing rather than cosmetic. `ControllerPlugin` also
//! writes `MovementIntent`, from human input, in `TickSet::Input` — which runs
//! *before* the autopilot's `TickSet::Intent`. A plugin that worked against the
//! three-plugin stack and lost every tick to the controller in the real one
//! would pass the plugin's own gate and fail here, and "the seam exists but
//! nothing survives it" is exactly the island this milestone is about.
//!
//! # The negative control
//!
//! [`the_same_harness_without_the_plugin_does_not_move`] is the same `App`, the
//! same goal resource, the same tick count, with `AutopilotPlugin` **not**
//! added — and it must fail to arrive. Without it, a harness in which
//! `ControllerPlugin` happened to walk the player east for an unrelated reason
//! would read as a pass.

use std::sync::Arc;

use lodestone_app::client_app;
use lodestone_autopilot::{AutopilotGoal, AutopilotPlugin, AutopilotStatus};
use lodestone_ecs::GameTick;
use lodestone_ecs::app::App;
use lodestone_ecs::player::{CollisionSource, PhysicsState, PlayerCollision};
use lodestone_ecs::{ChunkWorld, VersionData};
use lodestone_model::{
    AdapterError, BlockAabb, BlockPos, ClientAction, ConnectionState, Directive, LoginProfile,
    ServerAddress, VersionAdapter, WorldSink,
};
use lodestone_physics::{CollisionView, PlayerState, Vec3d};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

const AIR: u32 = 0;
const STONE: u32 = 1;

const FULL_CUBE: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 1.0, 1.0],
}];

/// A two-block `VersionAdapter`, byte-for-byte the shape
/// `drives_to_goal.rs`'s own `FixtureAdapter` has: it answers only the three
/// questions the search asks (`block_collision`, `block_name`,
/// `block_blocks_motion`) and refuses everything connection-shaped, because
/// nothing here drives a connection.
#[derive(Debug)]
struct FixtureAdapter;

impl VersionAdapter for FixtureAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &[]
    }

    fn supports(&self, _protocol: i32) -> bool {
        false
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Err(AdapterError::Unsupported(
            "fixture adapter: no login".to_owned(),
        ))
    }

    fn handle_packet(
        &self,
        _world: &mut dyn WorldSink,
        _state: ConnectionState,
        _packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Err(AdapterError::Unsupported(
            "fixture adapter: no packets".to_owned(),
        ))
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        Err(AdapterError::Unsupported(
            "fixture adapter: no actions".to_owned(),
        ))
    }

    fn block_collision(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        match state_id {
            AIR => Some(&[]),
            STONE => Some(FULL_CUBE),
            _ => None,
        }
    }

    fn block_name(&self, state_id: u32) -> Option<&'static str> {
        match state_id {
            AIR => Some("minecraft:air"),
            STONE => Some("minecraft:stone"),
            _ => None,
        }
    }

    fn block_blocks_motion(&self, state_id: u32) -> Option<bool> {
        match state_id {
            AIR => Some(false),
            STONE => Some(true),
            _ => None,
        }
    }
}

/// The **physics** collision world: a flat stone floor at `y = 0` spanning every
/// `(x, z)`. Deliberately not derived from the [`ChunkWorld`] below —
/// `player_physics` reads `PlayerCollision` and the search reads `ChunkWorld`,
/// two different seams in production too.
#[derive(Debug)]
struct FlatFloor;

impl CollisionView for FlatFloor {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
        if y == 0 {
            out.push(lodestone_physics::Aabb {
                min_x: f64::from(x),
                min_y: f64::from(y),
                min_z: f64::from(z),
                max_x: f64::from(x) + 1.0,
                max_y: f64::from(y) + 1.0,
                max_z: f64::from(z) + 1.0,
            });
        }
    }
}

impl CollisionSource for FlatFloor {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(self);
    }
}

/// The **planning** world the search reads: every column in `-radius..=radius`
/// loaded with the same flat stone floor.
fn flat_chunk_world(radius: i32) -> ChunkWorld {
    let mut world = World::new();
    let block_kind = PaletteKind::block_states();
    let biome_kind = PaletteKind::biomes();
    const SECTION_COUNT: usize = 4;

    for cx in -radius..=radius {
        for cz in -radius..=radius {
            let mut column = ChunkColumn::new(0, SECTION_COUNT, block_kind, biome_kind, AIR, 0);
            for lx in 0..16usize {
                for lz in 0..16usize {
                    column.set_block(lx, 0, lz, STONE);
                }
            }
            let light = ColumnLight::new(SECTION_COUNT);
            let chunk = LoadedChunk::new(column, light, Heightmaps::default(), Vec::new());
            world.load(ChunkPos::new(cx, cz), chunk);
        }
    }

    ChunkWorld::new(world)
}

/// **The seam under test.** A consumer's four lines: call [`client_app`], add
/// their plugin, insert their session-scoped resources, spawn the session
/// entity. No shell, no renderer, no hand-assembled plugin tuple — and no
/// privileged access of any kind.
fn consumer_app(with_autopilot: bool) -> (App, bevy_ecs::entity::Entity) {
    let mut app = client_app();
    if with_autopilot {
        app.add_plugins(AutopilotPlugin);
    }

    app.insert_resource(PlayerCollision::View(Arc::new(FlatFloor)));
    app.insert_resource(flat_chunk_world(4));
    app.insert_resource(VersionData(Some(Box::new(FixtureAdapter))));

    let session = lodestone_app::spawn_session(
        &mut app,
        PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0),
    );
    (app, session)
}

fn position(app: &App, entity: bevy_ecs::entity::Entity) -> Vec3d {
    app.world()
        .get::<PhysicsState>(entity)
        .expect("the session entity carries PhysicsState")
        .0
        .position
}

fn run_ticks(app: &mut App, n: u32) {
    for _ in 0..n {
        app.world_mut().run_schedule(GameTick);
    }
}

/// Milestone zero's behavioural gate. An external plugin, registered from
/// outside through `client_app()`, drives the local player to a commanded block
/// through the *full* production plugin set.
#[test]
fn an_externally_registered_plugin_reaches_its_goal() {
    let (mut app, session) = consumer_app(true);
    assert!(
        app.is_plugin_added::<AutopilotPlugin>(),
        "the consumer's plugin must be registered in the composed App"
    );
    let start = position(&app, session);

    app.insert_resource(AutopilotGoal(Some(BlockPos::new(5, 1, 0))));
    run_ticks(&mut app, 400);

    let end = position(&app, session);
    assert!(
        (end.x - start.x).abs() > 3.0,
        "expected real horizontal progress toward the goal, start={start:?} end={end:?}"
    );
    assert!(
        (end.x - 5.5).abs() < 0.6 && (end.z - 0.5).abs() < 0.6,
        "expected arrival near block (5, 1, 0), end={end:?}"
    );
    assert_eq!(
        *app.world().resource::<AutopilotStatus>(),
        AutopilotStatus::Arrived,
        "the plugin's own status resource must agree that it arrived"
    );
}

/// The negative control. Identical harness, identical goal resource, identical
/// tick budget, `AutopilotPlugin` **absent** — so `AutopilotStatus` does not
/// even exist and the player must not have moved horizontally at all. Run it and
/// watch the positive assertions above fail against it; without this, a harness
/// whose player drifted east for any unrelated reason would read as a pass.
#[test]
fn the_same_harness_without_the_plugin_does_not_move() {
    let (mut app, session) = consumer_app(false);
    assert!(
        !app.is_plugin_added::<AutopilotPlugin>(),
        "control premise: the plugin must be absent"
    );
    let start = position(&app, session);

    app.insert_resource(AutopilotGoal(Some(BlockPos::new(5, 1, 0))));
    run_ticks(&mut app, 400);

    let end = position(&app, session);
    assert!(
        (end.x - start.x).abs() < 0.01 && (end.z - start.z).abs() < 0.01,
        "control: with no autopilot registered nothing may drive the player, \
         start={start:?} end={end:?}"
    );
    assert!(
        app.world().get_resource::<AutopilotStatus>().is_none(),
        "control: the plugin's status resource must not exist when the plugin is absent"
    );
}
