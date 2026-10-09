//! A mob never steps against terrain that is not there, through the real tick
//! loop over a real generated world.
//!
//! The mob is placed the way `/summon` places one: straight into the mob
//! simulation at an exact coordinate. Nothing between the tick loop and the
//! terrain its physics reads is stubbed, so the landing height is a property of
//! the live column reads and not of a hand-written block closure.

use std::sync::Arc;

use lodestone_model::Vec3;

use crate::block_entities::BlockEntityHandle;
use crate::chunk::ChunkSource;
use crate::dimension::Dimension;
use crate::scheduled_tick::ScheduledTickHandle;
use crate::sleep::{SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock};
use crate::tick_area::TickFollow;
use crate::weather::{WeatherFeed, WeatherState};
use crate::world_state::WorldStateHandle;

const SEED: i64 = 1;
const PLAYER_CHUNK: (i32, i32) = (14, 8);
/// Columns resident around the player. Smaller than the default simulation
/// distance, so the follow area reaches columns that are not loaded.
const RESIDENT_RADIUS: i32 = 3;

/// The height a body rests at on the topmost collidable cell of the column at
/// `(x, z)`, read from the generator directly: the cell's floor plus the top of
/// its collision shape.
fn surface_height(terrain: &crate::chunk::Terrain263ChunkSource, x: i32, z: i32) -> f64 {
    for y in (-64..=320).rev() {
        let boxes = lodestone_data::collision_shapes::collision_boxes(terrain.block_state_id(x, y, z));
        let top = boxes.iter().map(|b| f64::from(b.max[1])).fold(0.0, f64::max);
        if top > 0.0 {
            return f64::from(y) + top;
        }
    }
    panic!("no collidable cell in column ({x}, {z})");
}

struct Harness {
    source: Arc<crate::chunk_store::ChunkStore<Arc<crate::chunk::Terrain263ChunkSource>>>,
    runtime: Arc<crate::dimension_runtime::DimensionRuntime>,
    task: tokio::task::JoinHandle<()>,
    _player: crate::players::PlayerTicket,
}

async fn start(terrain: &Arc<crate::chunk::Terrain263ChunkSource>) -> Harness {
    let (px, pz) = PLAYER_CHUNK;
    let source = Arc::new(crate::chunk_store::ChunkStore::with_capacity(Arc::clone(terrain), 600));
    for cz in pz - RESIDENT_RADIUS..=pz + RESIDENT_RADIUS {
        for cx in px - RESIDENT_RADIUS..=px + RESIDENT_RADIUS {
            source.column(cx, cz);
        }
    }
    let world = WorldStateHandle::new();
    for (rule, value) in [("spawn_mobs", "false"), ("advance_time", "false"), ("advance_weather", "false")] {
        world.set_rule(rule, value).unwrap();
    }
    let runtime = world.ensure_dimension_runtime(Dimension::Overworld);
    let position = lodestone_model::Vec3::new(f64::from(px * 16) + 8.5, 120.0, f64::from(pz * 16) + 8.5);
    let player = world
        .player_registry()
        .join_in_dimension("Summoner", uuid::Uuid::from_u128(23), position, Dimension::Overworld);
    let clock = Arc::new(TickClock::new());
    let tick_source = Arc::clone(&source);
    let tick_runtime = Arc::clone(&runtime);
    let task = tokio::spawn(async move {
        let sleep_vote = SleepVote::new();
        let sleep_feed = SleepFeed::default();
        let follow = TickFollow {
            dimension: Dimension::Overworld,
            radius: world.simulation_distance(),
            anchors: world.tick_anchors().clone(),
        };
        let mut server_world = crate::ecs::ServerApp::bootstrap().into_world();
        let snapshot: Arc<dyn ChunkSource> = tick_source.clone();
        server_world.insert_resource(crate::ecs::ServerWorldSnapshot::new(snapshot));
        crate::tick::run_primary_tick_loop_with_weather(
            server_world,
            tick_runtime.mobs().clone(),
            tick_runtime.entities().clone(),
            BlockEntityHandle::default(),
            clock,
            tick_source,
            BlockTickFeed::default(),
            (px - 3..=px + 3, pz - 3..=pz + 3),
            ExplosionFeed::default(),
            WeatherFeed::default(),
            WeatherState::default(),
            &sleep_vote,
            &sleep_feed,
            ScheduledTickHandle::default(),
            world,
            follow,
            crate::border::BorderFeed::default(),
        )
        .await;
    });
    tokio::task::yield_now().await;
    Harness { source, runtime, task, _player: player }
}

impl Harness {
    fn summon(&self, at: Vec3) -> i32 {
        self.runtime
            .mobs()
            .with(|sim| sim.spawn_species("minecraft:cow".parse().unwrap(), at).id())
    }

    async fn ticks(&self, count: u32) {
        for _ in 0..count {
            tokio::time::advance(crate::tick::TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }
    }

    fn position(&self, id: i32) -> Vec3 {
        self.runtime
            .mobs()
            .with(|sim| sim.iter().find(|mob| mob.id() == id).expect("the mob is alive").position())
    }

    async fn stop(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}

/// Summoned high above a resident column, at the middle and at the far edge of
/// the resident area, a mob lands on the surface; summoned over a column that
/// is inside the follow area but not loaded, it stays where it was put until
/// that column arrives, and then lands.
#[tokio::test(start_paused = true)]
async fn a_summoned_mob_lands_on_loaded_terrain_and_waits_for_unloaded_terrain() {
    let terrain = Arc::new(crate::overworld_chunk_source(SEED));
    let (px, pz) = PLAYER_CHUNK;
    let harness = start(&terrain).await;

    let centre = (px * 16 + 8, pz * 16 + 8);
    // The last loaded column to the east, at its far edge.
    let edge = ((px + RESIDENT_RADIUS) * 16 + 15, pz * 16 + 8);
    // The first column past it: inside the default follow area, never loaded.
    let outside = ((px + RESIDENT_RADIUS + 1) * 16 + 8, pz * 16 + 8);

    let at_centre = harness.summon(Vec3::new(f64::from(centre.0) + 0.5, 200.0, f64::from(centre.1) + 0.5));
    let at_edge = harness.summon(Vec3::new(f64::from(edge.0) + 0.5, 200.0, f64::from(edge.1) + 0.5));
    let beyond = harness.summon(Vec3::new(f64::from(outside.0) + 0.5, 200.0, f64::from(outside.1) + 0.5));
    harness.ticks(160).await;

    for (id, (x, z)) in [(at_centre, centre), (at_edge, edge)] {
        let rested = harness.position(id);
        let expected = surface_height(&terrain, rested.x.floor() as i32, rested.z.floor() as i32);
        assert_eq!(rested.y, expected, "the mob summoned over ({x}, {z}) must rest on the surface at {rested:?}");
        assert!(expected < 200.0, "the control surface must be below the summon height");
    }

    // Terrain that is not there is not air: the mob has not moved at all.
    let held = harness.position(beyond);
    assert_eq!(held.y, 200.0, "a mob over an unloaded column must not fall: {held:?}");

    // The column arrives, and the mob lands on it.
    harness.source.column(px + RESIDENT_RADIUS + 1, pz);
    harness.ticks(160).await;
    let landed = harness.position(beyond);
    let expected = surface_height(&terrain, landed.x.floor() as i32, landed.z.floor() as i32);
    assert_eq!(landed.y, expected, "once loaded the terrain is solid at {landed:?}");

    harness.stop().await;
}
