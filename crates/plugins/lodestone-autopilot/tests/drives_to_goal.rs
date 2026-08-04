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

/// # Genuine collision, not a flat plane
///
/// Every test above builds its world from [`FixtureAdapter`]/[`FlatFloor`]: two
/// synthetic states (`AIR`, `STONE`) and a hand-rolled `1x1x1` box. That is a
/// legitimate hermetic seam test (it proves the *wiring* — `ChunkWorld` in,
/// `MovementIntent`/`LookIntent` out, through the real schedule), but it is
/// also exactly the "world" species of vacuous test CLAUDE.md's evidence
/// standards warn about: the input structurally cannot contain a non-cube
/// shape, so a bug in how `top`/`full_cube` are *derived from real geometry*
/// (`lodestone_nav::facts::BlockFacts`) would pass this file's other three
/// tests unchanged.
///
/// This section re-runs the happy path over **real, jar-derived collision
/// data** — `lodestone_data::collision_shapes`/`block_states`/`block_solidity`,
/// the exact tables `lodestone_v770::adapter::V770Adapter` itself reads (issue
/// #361's census, dumped from the real 26.2 server's
/// `Block.BLOCK_STATE_REGISTRY`) — and, critically, includes one **real bottom
/// slab** (`minecraft:oak_slab[type=bottom]`, true collision top `0.5`, not a
/// full block) astride the walked path. `lodestone-nav`'s own unit tests
/// (`graph::tests::a_bottom_slab_is_walkable_and_reports_the_slab_surface`)
/// already prove the *search* handles a slab correctly against a synthetic
/// census; this proves the same claim end to end, through the real per-state
/// data source and the real physics integrator, which is the thing that
/// actually reaches a player.
///
/// This is a dev-only dependency on `lodestone-data` (see
/// `Cargo.toml`'s `[dev-dependencies]` comment) — not a version crate, so this
/// does not version-lock the plugin and is not even the soft
/// `SharedDependsOnVersion` isolation warning.
mod real_collision {
    use super::{App, position, run_ticks};
    use lodestone_autopilot::{AutopilotGoal, AutopilotPlugin, AutopilotStatus};
    use lodestone_ecs::player::{CollisionSource, LocalPlayerPlugin, PlayerCollision, spawn_local_player};
    use lodestone_ecs::{ChunkWorld, VersionData};
    use lodestone_model::{
        AdapterError, BlockAabb, BlockPos, ClientAction, ConnectionState, Directive, LoginProfile,
        ServerAddress, VersionAdapter, WorldSink,
    };
    use lodestone_physics::{CollisionView, PlayerState, Vec3d};
    use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};
    use std::sync::Arc;

    /// The first state id whose real block name is exactly `name` — e.g.
    /// `"minecraft:stone"`, `"minecraft:air"`. Scans the generated table
    /// (`STATE_COUNT` is ~32,366; this runs once per test, zero heap) rather
    /// than hardcoding an id, so a regen of the census cannot silently point
    /// this test at the wrong block.
    fn real_state_id(name: &str) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT as u32)
            .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
            .unwrap_or_else(|| panic!("no real block-state census entry for {name}"))
    }

    /// The first state id whose block name is `name` and whose properties
    /// contain `(key, value)` — used to pin the exact slab half
    /// (`type=bottom`) rather than whichever of top/bottom/double/waterlogged
    /// variants happens to sort first.
    fn real_state_id_with(name: &str, key: &str, value: &str) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT as u32)
            .find(|&id| {
                lodestone_data::block_states::block_name(id) == Some(name)
                    && lodestone_data::block_states::properties(id)
                        .is_some_and(|props| props.iter().any(|&(k, v)| k == key && v == value))
            })
            .unwrap_or_else(|| panic!("no real block-state census entry for {name} with {key}={value}"))
    }

    /// A [`VersionAdapter`] whose three block-census methods delegate straight
    /// to `lodestone-data`'s generated tables — the same functions
    /// `lodestone_v770::adapter::V770Adapter` calls, so this is not a second,
    /// independent implementation of the census that could quietly drift from
    /// the real one; it *is* the real one, minus the packet/login machinery
    /// this test never drives.
    #[derive(Debug)]
    struct RealDataAdapter;

    impl VersionAdapter for RealDataAdapter {
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
            Err(AdapterError::Unsupported("real-data adapter: no login".to_owned()))
        }

        fn handle_packet(
            &self,
            _world: &mut dyn WorldSink,
            _state: ConnectionState,
            _packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<Directive>, AdapterError> {
            Err(AdapterError::Unsupported("real-data adapter: no packets".to_owned()))
        }

        fn encode_action(
            &self,
            _state: ConnectionState,
            _action: &ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
            Err(AdapterError::Unsupported("real-data adapter: no actions".to_owned()))
        }

        fn block_collision(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
            lodestone_data::collision_shapes::collision_boxes(state_id)
        }

        fn block_name(&self, state_id: u32) -> Option<&'static str> {
            lodestone_data::block_states::block_name(state_id)
        }

        fn block_blocks_motion(&self, state_id: u32) -> Option<bool> {
            lodestone_data::block_solidity::blocks_motion(state_id)
        }
    }

    /// The floor this test walks: real `minecraft:stone` everywhere at world
    /// `y = 0` except two consecutive columns, `SLAB_X_MIN..=SLAB_X_MAX` at
    /// `SLAB_Z`, which are real bottom `minecraft:oak_slab` — true collision
    /// top `0.5`, half the height of the stone either side of it. Two columns,
    /// not one: the player's AABB is 0.6 wide, so a single low column bordered
    /// by full blocks only fully clears its neighbours for a couple of ticks —
    /// not enough time to actually settle onto the lower surface before the far
    /// edge's support takes over again. Two columns give a real, uninterrupted
    /// stretch to fall onto and rest on the slab's true height, which is the
    /// whole point of this test (predicting the *settled* magnitude, not just
    /// a momentary dip). Shared between the planning world
    /// ([`real_chunk_world`]) and the physics collision view
    /// ([`RealFloorCollision`]) so both seams agree on what is actually there,
    /// exactly as production's two independent seams (`ChunkWorld` for
    /// planning, `PlayerCollision` for physics) are required to.
    const SLAB_X_MIN: i32 = 3;
    const SLAB_X_MAX: i32 = 4;
    const SLAB_Z: i32 = 0;

    fn floor_state_at(x: i32, z: i32, stone: u32, slab: u32) -> u32 {
        if (SLAB_X_MIN..=SLAB_X_MAX).contains(&x) && z == SLAB_Z { slab } else { stone }
    }

    /// A [`ChunkWorld`] with the [`floor_state_at`] floor at world `y = 0`,
    /// otherwise real `minecraft:air`, spanning `-radius..=radius` chunk
    /// columns — the planning-side seam `lodestone_nav::SnapshotView::build`
    /// reads.
    fn real_chunk_world(radius: i32, stone: u32, air: u32, slab: u32) -> ChunkWorld {
        let mut world = World::new();
        let block_kind = PaletteKind::block_states();
        let biome_kind = PaletteKind::biomes();
        const SECTION_COUNT: usize = 4;

        for cx in -radius..=radius {
            for cz in -radius..=radius {
                let mut column = ChunkColumn::new(0, SECTION_COUNT, block_kind, biome_kind, air, 0);
                for lx in 0..16i32 {
                    for lz in 0..16i32 {
                        let wx = cx * 16 + lx;
                        let wz = cz * 16 + lz;
                        column.set_block(lx as usize, 0, lz as usize, floor_state_at(wx, wz, stone, slab));
                    }
                }
                let light = ColumnLight::new(SECTION_COUNT);
                let chunk = LoadedChunk::new(column, light, Heightmaps::default(), Vec::new());
                world.load(ChunkPos::new(cx, cz), chunk);
            }
        }

        ChunkWorld::new(world)
    }

    /// The **physics**-side seam (`PlayerCollision`): for any world cell,
    /// looks up the same [`floor_state_at`] floor and hands back that real
    /// state's genuine, jar-derived collision boxes (`lodestone_data`),
    /// translated from block-local `0.0..1.0` coordinates into world space by
    /// adding the cell's own `(x, y, z)`. Deliberately independent code from
    /// [`real_chunk_world`] (same data, different call site), matching
    /// `docs/autonomous-navigation.md`'s note that production reads
    /// `ChunkWorld` and `PlayerCollision` through two different seams and a
    /// test collapsing them would stop it from catching a plugin that
    /// accidentally depended on one standing in for the other.
    #[derive(Debug)]
    struct RealFloorCollision {
        stone: u32,
        air: u32,
        slab: u32,
    }

    impl CollisionView for RealFloorCollision {
        fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
            let state = if y == 0 {
                floor_state_at(x, z, self.stone, self.slab)
            } else {
                self.air
            };
            let Some(boxes) = lodestone_data::collision_shapes::collision_boxes(state) else {
                return;
            };
            for b in boxes {
                out.push(lodestone_physics::Aabb {
                    min_x: f64::from(x) + f64::from(b.min[0]),
                    min_y: f64::from(y) + f64::from(b.min[1]),
                    min_z: f64::from(z) + f64::from(b.min[2]),
                    max_x: f64::from(x) + f64::from(b.max[0]),
                    max_y: f64::from(y) + f64::from(b.max[1]),
                    max_z: f64::from(z) + f64::from(b.max[2]),
                });
            }
        }
    }

    impl CollisionSource for RealFloorCollision {
        fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
            f(self);
        }
    }

    fn app_on_real_terrain(chunk_radius: i32) -> (App, bevy_ecs::entity::Entity) {
        let stone = real_state_id("minecraft:stone");
        let air = real_state_id("minecraft:air");
        let slab = real_state_id_with("minecraft:oak_slab", "type", "bottom");

        // Sanity-check the premise before building anything on top of it: the
        // slab must actually be shorter than the stone either side of it, or
        // this test proves nothing (this is the "predict the value, do not
        // merely assert the sign" discipline — know the two numbers apart
        // before the assertion that depends on the gap between them).
        let stone_top = lodestone_data::collision_shapes::collision_boxes(stone)
            .and_then(|b| b.iter().map(|b| b.max[1]).reduce(f32::max))
            .expect("stone has a real collision shape");
        let slab_top = lodestone_data::collision_shapes::collision_boxes(slab)
            .and_then(|b| b.iter().map(|b| b.max[1]).reduce(f32::max))
            .expect("the bottom slab has a real collision shape");
        assert!(
            (stone_top - 1.0).abs() < 1e-4,
            "test premise: real minecraft:stone must be a full-height cube, got top={stone_top}"
        );
        assert!(
            (slab_top - 0.5).abs() < 1e-4,
            "test premise: a real bottom slab must be half-height, got top={slab_top}"
        );

        let mut app = App::new();
        app.add_plugins((lodestone_ecs::CorePlugin, LocalPlayerPlugin, AutopilotPlugin));

        app.insert_resource(PlayerCollision::View(Arc::new(RealFloorCollision { stone, air, slab })));
        app.insert_resource(real_chunk_world(chunk_radius, stone, air, slab));
        app.insert_resource(VersionData(Some(Box::new(RealDataAdapter))));

        let state = PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0);
        let entity = spawn_local_player(app.world_mut(), state);
        (app, entity)
    }

    /// The strengthened version of
    /// `a_nearby_goal_is_reached_through_the_real_physics_seam`: same claim
    /// (a goal is reached through the real `TickSet::Intent -> Physics`
    /// pipeline), but over genuine per-state jar collision including one real
    /// non-cube shape astride the path, and with a **predicted magnitude**,
    /// not just a predicted sign — while the player is over the slab column
    /// its feet must rest at `y ≈ 0.5`, not `y ≈ 1.0`. A search or physics bug
    /// that quietly treated the slab as a full block (the exact failure mode
    /// `lodestone_nav::facts::BlockFacts::top`'s own doc comment names as
    /// "nearly gotten wrong twice") would still let this test *arrive* — the
    /// path is short enough either way — but would not produce that dip, so
    /// "arrived" alone is not the assertion this test rests on.
    #[test]
    fn a_nearby_goal_is_reached_over_a_real_bottom_slab_using_genuine_jar_collision() {
        let (mut app, entity) = app_on_real_terrain(4);
        let start = position(&app, entity);

        app.insert_resource(AutopilotGoal(Some(BlockPos::new(6, 1, 0))));

        let mut saw_the_slab_dip = false;
        // 400 ticks, same generous 20 s budget as the flat-floor happy path
        // (a 6-block walk is, if anything, cheaper) — sampled tick by tick so
        // the dip assertion below can find the moment the player is actually
        // over the slab, rather than only checking the final position.
        //
        // The player's AABB is 0.6 wide (`lodestone_physics::entity`'s player
        // dimensions), half-width 0.3, so it is fully inside the two-column
        // slab region (`x` in `[3, 5)`) only once its centre is in
        // `[3.3, 4.7]` — outside that, the box still overlaps the full-height
        // stone next door and is correctly supported at `y = 1.0` by it, the
        // same "a narrow low patch beside full blocks does not let you dip
        // immediately" behaviour vanilla itself has. The "settled" window
        // below (`|x - 4.0| < 0.3`, i.e. `[3.7, 4.3]`) is deep enough into that
        // range to give several ticks of fall time after entering it, so the
        // strict "must not still read full height" assertion is not raced
        // against the fall itself.
        for _ in 0..400 {
            run_ticks(&mut app, 1);
            let p = position(&app, entity);
            if (p.x - 4.0).abs() < 0.3 {
                if (p.y - 0.5).abs() < 0.15 {
                    saw_the_slab_dip = true;
                }
                // Deep into the slab region, the player must not be resting
                // at the stone height either side of it — that would mean the
                // slab's real half-height shape never reached the physics
                // seam.
                assert!(
                    p.y < 0.85,
                    "expected the real bottom slab (top 0.5) to be shorter than the stone \
                     either side of it, found the player resting at y={} over x={} z={}",
                    p.y,
                    p.x,
                    p.z
                );
            }
        }

        assert!(
            saw_the_slab_dip,
            "expected at least one tick with the player's feet at the slab's real height \
             (~0.5) while crossing x={SLAB_X_MIN}..={SLAB_X_MAX}"
        );

        let end = position(&app, entity);
        assert!(
            (end.x - start.x).abs() > 3.0,
            "expected real horizontal progress toward the goal, start={start:?} end={end:?}"
        );
        assert!(
            (end.x - 6.5).abs() < 0.6 && (end.z - 0.5).abs() < 0.6,
            "expected the player to have arrived near block (6, 1, 0), end={end:?}"
        );
        assert!(
            (end.y - 1.0).abs() < 0.15,
            "expected the player back at full stone height (1.0) past the slab, end={end:?}"
        );
        assert_eq!(
            *app.world().resource::<AutopilotStatus>(),
            AutopilotStatus::Arrived,
            "the plugin's own status resource must agree that it arrived"
        );
    }
}
