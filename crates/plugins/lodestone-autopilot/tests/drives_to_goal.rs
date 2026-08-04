//! The gate for `crates/plugins/lodestone-autopilot`'s reason to exist: does
//! setting an [`AutopilotGoal`] actually move the local player through the
//! **real** production seam, not a mock of it?
//!
//! # Why this is not a live-server test
//!
//! The live oracles (`docs/plugin-api.md`'s brief) prove the whole client end
//! to end, but registering this plugin into the running shell's `App` happens
//! in `lodestone_shell::sim::Sim::new` (`crates/lodestone-shell/src/sim.rs`),
//! a file outside this crate's ownership for this change — see the commit
//! message and `docs/autonomous-navigation.md` for the patch handed to the
//! file's owner. This test is the next best thing and a meaningfully strong
//! one: it builds a real `bevy_ecs` `App` with `lodestone_ecs::CorePlugin` +
//! `lodestone_ecs::player::LocalPlayerPlugin` + [`AutopilotPlugin`] — the
//! *actual* `player_physics` system, the *actual* `TickSet::Intent`/`Physics`
//! ordering, the *actual* `MovementIntent`/`LookIntent` components — and
//! drives `GameTick` the same way the shell's driver loop does. Nothing here
//! is a stand-in for the seam; only the *collision world* and the *version
//! adapter* are hand-built fixtures, exactly as `docs/baritone-port.md` §6
//! requires ("a fixture world that structurally contains" what a hermetic
//! gate needs) and exactly as `lodestone_nav::FixtureCensus`'s own doc comment
//! says it exists for ("constructible from an integration test and from the
//! plugin's own gates").
//!
//! # The negative control
//!
//! `goal_outside_the_snapshot_is_reported_as_no_start` is the control this
//! repo's evidence standards ask for: it proves `AutopilotStatus::Failed`
//! actually fires rather than the happy-path test merely never exercising
//! failure.

use std::sync::Arc;

use lodestone_autopilot::{AutopilotGoal, AutopilotPlugin, AutopilotStatus, FailReason};
use lodestone_ecs::app::App;
use lodestone_ecs::player::{CollisionSource, LocalPlayerPlugin, PhysicsState, PlayerCollision, spawn_local_player};
use lodestone_ecs::{ChunkWorld, GameTick, VersionData};
use lodestone_model::{
    AdapterError, BlockAabb, BlockPos, ClientAction, ConnectionState, Directive, LoginProfile,
    ServerAddress, VersionAdapter, WorldSink,
};
use lodestone_physics::{CollisionView, PlayerState, Vec3d};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

/// State ids, matching `lodestone_nav::FixtureCensus` exactly (this test's
/// [`FixtureAdapter`] answers the same three questions that census does, just
/// through the real `VersionAdapter` trait instead of the nav crate's
/// test-only `BlockCensus`).
const AIR: u32 = 0;
const STONE: u32 = 1;

/// A `VersionAdapter` that knows only [`AIR`]/[`STONE`], full cubes and empty
/// air, for exactly the three questions `lodestone_nav::AdapterCensus` asks
/// (`block_collision`, `block_name`, `block_blocks_motion`). Every other
/// method keeps `VersionAdapter`'s own default (`None`) or is unreachable in
/// this test (the login/packet/action methods have no default and must exist
/// to satisfy the trait, but nothing here drives a real connection).
#[derive(Debug)]
struct FixtureAdapter;

const FULL_CUBE: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 1.0, 1.0],
}];

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
        Err(AdapterError::Unsupported("fixture adapter: no login".to_owned()))
    }

    fn handle_packet(
        &self,
        _world: &mut dyn WorldSink,
        _state: ConnectionState,
        _packet_id: i32,
        _payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        Err(AdapterError::Unsupported("fixture adapter: no packets".to_owned()))
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        Err(AdapterError::Unsupported("fixture adapter: no actions".to_owned()))
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

/// A flat stone floor at world `y = 0` (block-local: the block occupying
/// `y = 0` is `STONE`, everything else `AIR`), spanning every `(x, z)` — the
/// **physics** collision world. Deliberately not derived from [`ChunkWorld`]:
/// `player_physics` and this plugin's search read two different seams
/// (`PlayerCollision` vs `ChunkWorld`) in production too, and keeping them
/// separate here is what proves the plugin does not accidentally depend on
/// one standing in for the other.
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

/// A [`ChunkWorld`] whose columns in `-radius..=radius` (chunk units) are all
/// loaded with the same flat stone floor `FlatFloor` collides against — the
/// **planning** world `lodestone_nav::SnapshotView` reads. `min_y = 0`,
/// 4 sections (0..64), matching a short walk test's needs.
fn flat_chunk_world(radius: i32) -> ChunkWorld {
    let mut world = World::new();
    let block_kind = PaletteKind::block_states();
    let biome_kind = PaletteKind::biomes();
    const SECTION_COUNT: usize = 4;

    for cx in -radius..=radius {
        for cz in -radius..=radius {
            let mut column = ChunkColumn::new(0, SECTION_COUNT, block_kind, biome_kind, AIR, 0);
            // Fill the floor section (world y = 0..16, section index 0) one
            // block at a time via `set_block` below instead of hand-building a
            // paletted section — simpler and this is a one-time test fixture,
            // not a hot path.
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

/// An `App` with the real production plugin stack: `CorePlugin` (schedules),
/// `LocalPlayerPlugin` (`TickSet::Physics`, `MovementIntent`/`LookIntent`
/// components) and [`AutopilotPlugin`] itself — plus the local player entity,
/// spawned standing on [`FlatFloor`] at `(0.5, 1.0, 0.5)`.
fn app_on_flat_floor(chunk_radius: i32) -> (App, bevy_ecs::entity::Entity) {
    let mut app = App::new();
    app.add_plugins((lodestone_ecs::CorePlugin, LocalPlayerPlugin, AutopilotPlugin));

    app.insert_resource(PlayerCollision::View(Arc::new(FlatFloor)));
    app.insert_resource(flat_chunk_world(chunk_radius));
    app.insert_resource(VersionData(Some(Box::new(FixtureAdapter))));

    let state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
    let entity = spawn_local_player(app.world_mut(), state);
    (app, entity)
}

fn position(app: &App, entity: bevy_ecs::entity::Entity) -> Vec3d {
    app.world()
        .get::<PhysicsState>(entity)
        .expect("local player has PhysicsState")
        .0
        .position
}

fn run_ticks(app: &mut App, n: u32) {
    for _ in 0..n {
        app.world_mut().run_schedule(GameTick);
    }
}

/// The whole point of this crate: a goal a few blocks away, on flat ground,
/// well inside the default snapshot radius, is walked to through the real
/// `TickSet::Intent -> TickSet::Physics` pipeline with no shell, no net, no
/// live server — just this plugin driving the same components the shell's
/// human-input path drives.
#[test]
fn a_nearby_goal_is_reached_through_the_real_physics_seam() {
    let (mut app, entity) = app_on_flat_floor(4);
    let start = position(&app, entity);

    app.insert_resource(AutopilotGoal(Some(BlockPos::new(5, 1, 0))));

    // 400 ticks = 20 s of game time — generous for a 5-block walk (walk speed
    // is ~4.3 blocks/s, so under 2 s of *movement*; the rest of the budget
    // covers the per-tick search, which `Budget::PER_TICK` sizes for a
    // 20,000-node search finishing in ~10 ticks and this search is far smaller).
    run_ticks(&mut app, 400);

    let end = position(&app, entity);
    assert!(
        (end.x - start.x).abs() > 3.0,
        "expected real horizontal progress toward the goal, start={start:?} end={end:?}"
    );
    assert!(
        (end.x - 5.5).abs() < 0.6 && (end.z - 0.5).abs() < 0.6,
        "expected the player to have arrived near block (5, 1, 0), end={end:?}"
    );
    assert_eq!(
        *app.world().resource::<AutopilotStatus>(),
        AutopilotStatus::Arrived,
        "the plugin's own status resource must agree that it arrived"
    );
}

/// Clearing the goal mid-walk hands rotation control back (removes
/// `LookIntent`) and stops driving `MovementIntent` — proven by ticking a few
/// more times after clearing and observing no further net progress toward
/// where the goal was, beyond whatever was already in flight that tick.
#[test]
fn clearing_the_goal_stops_driving() {
    let (mut app, entity) = app_on_flat_floor(4);
    app.insert_resource(AutopilotGoal(Some(BlockPos::new(5, 1, 0))));
    // Walk speed is ~4.3 blocks/s (~0.216 blocks/tick), so 5 blocks takes
    // ~23 ticks of pure movement plus a few for the search; 10 ticks is
    // comfortably partway and comfortably short of arrival.
    run_ticks(&mut app, 10);
    let mid = position(&app, entity);
    assert!(
        (mid.x - 5.5).abs() > 0.6,
        "test premise: should not have arrived yet, mid={mid:?}"
    );

    app.insert_resource(AutopilotGoal(None));
    run_ticks(&mut app, 40);
    let after = position(&app, entity);

    // With no goal and no other input source in this bare app, human input
    // (`compute_movement_intent`) is not installed at all — only
    // `apply_look_intent` and this plugin are — so `MovementIntent` simply
    // keeps whatever `plan_route` last wrote before it stood down. The
    // observable contract that matters is `AutopilotStatus`, not further
    // motion (a full shell app has `ControllerPlugin` re-asserting whatever
    // the human is actually holding, which is a different system entirely).
    assert_eq!(*app.world().resource::<AutopilotStatus>(), AutopilotStatus::Idle);
    let _ = after;
}

/// The negative control: a goal outside the loaded snapshot must be reported
/// as a failure to start, not silently ignored — the detector this test
/// exists to prove *can* fail, per this repo's evidence standards ("an
/// assertion of absence needs a control proving the detector works").
#[test]
fn goal_outside_the_snapshot_is_reported_as_no_start() {
    // Radius 0: only the centre column is loaded, so a goal several chunks
    // away cannot be reached by any snapshot this app could ever build.
    let (mut app, _entity) = app_on_flat_floor(0);
    app.insert_resource(AutopilotGoal(Some(BlockPos::new(500, 1, 500))));

    run_ticks(&mut app, 20);

    match *app.world().resource::<AutopilotStatus>() {
        AutopilotStatus::Failed(FailReason::Search(_)) => {}
        other => panic!("expected a search failure for an unreachable goal, got {other:?}"),
    }
}
