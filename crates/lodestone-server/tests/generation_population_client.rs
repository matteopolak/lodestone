//! Source-published generation population through the integrated tick, protocol
//! 776, and the real client's entity ECS. Terrain and candidates are deterministic
//! fixtures; this does not test generator selection or rendered pixels.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientHandle, EventStream, LoginProfile, ServerAddress};
use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_data::entity_type::EntityType;
use lodestone_model::{ClientEvent, ResourceKey, Vec3};
use lodestone_server::generation_population::{
    GenerationSpawnBatch, PendingGenerationPopulationPublication,
};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v26_2::{V770ServerProtocol, adapter};
use lodestone_worldgen::spawn_stage::GenerationSpawn;
use uuid::Uuid;

const DEADLINE: Duration = Duration::from_secs(10);
const COW_ID: i32 = 1000;
const COW_POS: Vec3 = Vec3::new(3.5, 64.0, 7.5);

#[derive(Clone, Default)]
struct PopulationSource {
    columns: Arc<Mutex<HashMap<(i32, i32), ChunkColumn>>>,
    publication: Arc<PendingGenerationPopulationPublication>,
}

impl PopulationSource {
    fn fresh_column() -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block_id(x, 62, z, Block::Stone.default_state());
                column.set_block_id(x, 63, z, Block::GrassBlock.default_state());
            }
        }
        column
    }
}

impl ChunkSource for PopulationSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.columns.lock().expect("fixture columns")
            .entry((cx, cz)).or_insert_with(Self::fresh_column).clone()
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        self.columns.lock().expect("fixture columns").get(&(cx, cz)).cloned()
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        self.columns.lock().expect("fixture columns")
            .get(&(x.div_euclid(16), z.div_euclid(16)))
            .map(|column| column.block_state_id(x.rem_euclid(16), y, z.rem_euclid(16)))
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }

    fn biome_state_at(&self, _: i32, _: i32, _: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        self.columns.lock().expect("fixture columns")
            .get_mut(&(x.div_euclid(16), z.div_euclid(16)))
            .expect("mutation targets retained terrain")
            .set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
    }

    fn pending_generation_spawn_batches(&self, limit: usize) -> Vec<Arc<GenerationSpawnBatch>> {
        self.publication.pending(limit)
    }
}

async fn join() -> (IntegratedServer, ClientHandle, EventStream, PopulationSource) {
    let source = PopulationSource::default();
    let (server, io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol, source.clone(), (0..=0, 0..=0), (0, 0), 0,
    );
    for (rule, value) in [("spawn_mobs", "false"), ("random_tick_speed", "0")] {
        server.world_state().set_rule(rule, value).expect("fixture game rule");
    }
    let (handle, events) = ClientBuilder::new(
        ServerAddress { host: "memory".into(), port: 0 },
        LoginProfile { username: "PopulationWatch".into(), uuid: Uuid::nil() },
        Box::new(adapter()),
    ).connect_with(io);
    handle.wait_for_spawn(DEADLINE).await.expect("real client placement");
    handle.wait_for_chunks(1, DEADLINE).await.expect("real initial chunk decode");
    let mobs = server.mobs().expect("integrated mob simulation");
    tokio::time::timeout(DEADLINE, async {
        while mobs.with(|sim| sim.next_id()) != COW_ID
            || !server.tick_stats().is_some_and(|stats| stats.tick_count > 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("client acknowledgements and initial seed must release tick holds");
    assert!(handle.entities().is_empty(), "initial population must be empty");
    (server, handle, events, source)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_population_reaches_client_ecs_and_continues_ticking_after_ready() {
    // Independent registry report: minecraft:entity_type / minecraft:cow is 30.
    assert_eq!(EntityType::Cow.registry_id(), 30);
    let cow_type = ResourceKey::new_borrowed("minecraft", "cow").expect("known cow identifier");
    let (server, handle, mut events, source) = join().await;
    // `join` holds the rule off so the initial population is provably empty; the
    // rule gates generation-time animals as well, so it is switched on here. The
    // one-chunk area keeps every position within 24 blocks of the player, so no
    // natural spawn can land and the cow below is the only possible entity.
    server.world_state().set_rule("spawn_mobs", "true").expect("known game rule");
    let batch = GenerationSpawnBatch::new(vec![GenerationSpawn {
        entity_type: EntityType::Cow.into(), x: 3, y: 64, z: 7,
    }]).expect("one generation creature");
    source.publication.publish(&batch);

    let spawn = tokio::time::timeout(DEADLINE, async {
        loop {
            let event = events.recv().await.expect("client remains connected before cow spawn");
            if let ClientEvent::EntitySpawned { entity_id, entity_type, pos, .. } = event {
                break (entity_id, entity_type, pos);
            }
        }
    }).await.expect("generation cow must reach the real spawn decoder");
    assert_eq!(spawn.0, COW_ID);
    assert_eq!(spawn.1, cow_type);
    assert_eq!(spawn.2, COW_POS);
    handle.wait_for(DEADLINE, |client| client.entity_from_wire(COW_ID)
        .is_some_and(|cow| cow.entity_type == cow_type && cow.position == COW_POS))
        .await.expect("spawn decoder must populate client ECS");
    assert!(!batch.is_pending(), "successful materialization completes the source batch");
    assert!(source.publication.pending(1).is_empty(), "completed publication must be pruned");

    let ticks_before = server.tick_stats().expect("production tick statistics").tick_count;
    // Remove the whole top layer so wandering cannot leave a surviving support.
    // The stone layer at y=62 independently predicts a resting foot position y=63.
    for x in 0..16 {
        for z in 0..16 {
            server.set_resident_block_state_id(x, 63, z, StateId::AIR)
                .expect("remove support through the authoritative resident source");
        }
    }
    assert_eq!(server.resident_block_state_id(3, 63, 7), Some(StateId::AIR));
    assert_eq!(source.resident_block_state_id(3, 63, 7), Some(StateId::AIR));
    assert_eq!(server.resident_block_state_id(3, 62, 7), Some(Block::Stone.default_state()));
    handle.wait_for(DEADLINE, |client| client.entity_from_wire(COW_ID)
        .is_some_and(|cow| (cow.position.y - 63.0).abs() < 1.0 / 4096.0))
        .await.expect("later integrated ticks must carry the falling cow into client ECS");
    let simulated = server.mobs().expect("live mob handle").with(|sim| {
        let cow = sim.get(COW_ID).expect("generation cow survives terrain edit");
        assert!(cow.is_persistent(), "generation creatures retain persistence");
        cow.position()
    });
    assert_eq!(simulated.y, 63.0);
    assert!(server.tick_stats().expect("production tick statistics").tick_count > ticks_before);
    assert_eq!(handle.entities().len(), 1, "source publication cannot duplicate the cow");
    eprintln!("generation cow {COW_ID}: source batch -> protocol 776 -> client ECS at {COW_POS:?}; later shared ticks settled y=63");
    drop(handle);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_generation_batch_keeps_the_live_client_population_empty() {
    let (server, handle, _events, source) = join().await;
    let initial = server.tick_stats().expect("production tick statistics").tick_count;
    tokio::time::timeout(DEADLINE, async {
        while server.tick_stats().expect("production tick statistics").tick_count < initial + 5 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("negative control must observe real completed ticks");
    assert!(source.publication.pending(1).is_empty());
    assert!(server.mobs().expect("live mob handle").with(|sim| sim.snapshots().is_empty()));
    assert!(handle.entities().is_empty(), "ticks and a real join alone cannot fabricate creatures");
    eprintln!("no-batch control: real protocol 776 client remains empty after five additional integrated ticks");
    drop(handle);
    server.shutdown().await;
}
