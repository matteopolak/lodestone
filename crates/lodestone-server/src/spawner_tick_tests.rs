//! Spawner block entities through the real tick loop.

use std::sync::Arc;

use crate::block_entities::{BlockEntity, BlockEntityHandle};
use crate::chunk::{ChunkColumn, ChunkSource};
use crate::dimension::Dimension;
use crate::mob_spawner::{SpawnData, SpawnerState};
use crate::scheduled_tick::ScheduledTickHandle;
use crate::sleep::{SleepFeed, SleepVote};
use crate::tick::{BlockTickFeed, ExplosionFeed, TickClock};
use crate::tick_area::TickFollow;
use crate::weather::{WeatherFeed, WeatherState};
use crate::world_state::WorldStateHandle;
use lodestone_model::BlockPos;
use lodestone_data::block_states::StateId;

/// Stone through y 70 everywhere, air above.
struct Floor;

impl ChunkSource for Floor {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 128);
        let stone = StateId::from_state_str("minecraft:stone").expect("stone");
        for y in 0..=70 {
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block_id(x, y, z, stone);
                }
            }
        }
        column.prime_client_heightmaps();
        column
    }
    fn block_state_id(&self, _x: i32, y: i32, _z: i32) -> StateId {
        if y <= 70 {
            StateId::from_state_str("minecraft:stone").expect("stone")
        } else {
            StateId::AIR
        }
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::Overworld)
    }
}

fn zombie_spawner() -> SpawnerState {
    // Delay 0: the first tick with a player in range attempts a spawn.
    SpawnerState::restore(
        0,
        200,
        800,
        4,
        6,
        16,
        4,
        Vec::new(),
        Some(SpawnData { entity_type: Some("minecraft:zombie".parse().expect("zombie")) }),
    )
}

/// A spawner whose candidate cell falls in a column that is not resident must
/// wait on its own, without stopping the spawners after it in the same tick.
///
/// Only chunks -1..=1 are resident. Eight spawners stand on the east edge of chunk
/// 1, where a candidate up to four blocks east lands in chunk 2 (cold); one more
/// stands mid-chunk, where every candidate is resident. Registry order is
/// arbitrary, so the cold spawners are numerous enough that the good one follows
/// at least one of them in all but a small share of orders.
#[tokio::test(start_paused = true)]
async fn a_cold_spawner_does_not_stop_the_spawners_after_it() {
    let source = Arc::new(crate::chunk_store::ChunkStore::with_capacity(Floor, 9));
    let tickets = source.tickets();
    let admission = tickets.grant_player(91, (0, 0), 1);
    tickets.tick();
    for cz in -1..=1 {
        for cx in -1..=1 {
            source.column(cx, cz);
        }
    }
    assert_eq!(source.len(), 9);

    let block_entities = BlockEntityHandle::default();
    let cold: Vec<BlockPos> = (0..8).map(|i| BlockPos::new(31, 71, 2 * i)).collect();
    let good = BlockPos::new(20, 71, 8);
    block_entities.with(|registry| {
        for &pos in &cold {
            registry.insert(pos, BlockEntity::Spawner(zombie_spawner()));
        }
        registry.insert(good, BlockEntity::Spawner(zombie_spawner()));
    });

    let world = WorldStateHandle::new();
    for (rule, value) in [
        ("random_tick_speed", "0"),
        ("spawn_patrols", "false"),
        ("spawn_wandering_traders", "false"),
        ("spawn_phantoms", "false"),
        ("spawn_mobs", "false"),
        ("advance_time", "false"),
        ("advance_weather", "false"),
    ] {
        world.set_rule(rule, value).unwrap();
    }
    assert!(world.set_difficulty(lodestone_model::Difficulty::Normal));
    let runtime = world.ensure_dimension_runtime(Dimension::Overworld);
    let player = world.player_registry().join_in_dimension(
        "Spawners",
        uuid::Uuid::from_u128(31),
        lodestone_model::Vec3::new(20.5, 71.0, 8.5),
        Dimension::Overworld,
    );

    let clock = Arc::new(TickClock::new());
    let task = {
        let clock = Arc::clone(&clock);
        let source = Arc::clone(&source);
        let world = world.clone();
        let runtime = Arc::clone(&runtime);
        let block_entities = block_entities.clone();
        tokio::spawn(async move {
            let sleep_vote = SleepVote::new();
            let sleep_feed = SleepFeed::default();
            let follow = TickFollow {
                dimension: Dimension::Overworld,
                radius: 3,
                anchors: world.tick_anchors().clone(),
            };
            let mut server_world = crate::ecs::ServerApp::bootstrap().into_world();
            let snapshot: Arc<dyn ChunkSource> = source.clone();
            server_world.insert_resource(crate::ecs::ServerWorldSnapshot::new(snapshot));
            crate::tick::run_primary_tick_loop_with_weather(
                server_world,
                runtime.mobs().clone(),
                runtime.entities().clone(),
                block_entities,
                clock,
                source,
                BlockTickFeed::default(),
                (-3..=3, -3..=3),
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
        })
    };
    tokio::task::yield_now().await;
    for _ in 0..6 {
        tokio::time::advance(crate::tick::TICK_PERIOD).await;
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    // Precondition: at least one cold spawner really was left uncommitted.
    let stuck = block_entities.with(|registry| {
        cold.iter()
            .filter(|&&pos| matches!(registry.get(pos), Some(BlockEntity::Spawner(s)) if *s == zombie_spawner()))
            .count()
    });
    assert!(stuck >= 1, "no spawner met a cold probe, so the fixture proves nothing");

    let zombies_near_good = runtime.mobs().with(|sim| {
        sim.snapshots()
            .iter()
            .filter(|s| s.entity_type.path() == "zombie" && (s.position.x - 20.5).abs() <= 5.0)
            .count()
    });
    assert!(zombies_near_good >= 1, "the resident spawner spawned nothing in six ticks");
    drop(player);
    drop(admission);
}
