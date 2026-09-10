//! Production-path proof that the integrated world's authoritative tick loop
//! advances a dropped item, rather than leaving item physics in a closed
//! `MobSim`-only test.

#![cfg(not(target_arch = "wasm32"))]

use std::time::{Duration, Instant};
use std::str::FromStr;

use lodestone_entity::item_entity::ItemLifecycle;
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};

struct FlatFloor;

impl ChunkSource for FlatFloor {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block(x, 0, z, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: &str) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn integrated_tick_loop_advances_a_live_dropped_item() {
    let (server, _client) = IntegratedServer::open_in_memory_with_mobs(
        FakeProtocol,
        FlatFloor,
        (0..=0, 0..=0),
        (0, 0),
        0,
        0,
    );
    let mobs = server.mobs().expect("the integrated world owns a MobHandle");

    // World-open reseeding replaces the initial simulation. Wait for the
    // production handoff before inserting the item, or the test could seed a
    // population that is immediately discarded by the normal startup path.
    let deadline = Instant::now() + Duration::from_secs(5);
    while mobs.with(|sim| sim.next_id()) < 1000 {
        assert!(Instant::now() < deadline, "integrated mob seed did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let item = ResourceKey::from_str("minecraft:cobblestone").expect("valid item key");
    let (item_id, initial_y) = mobs.with(|sim| {
        let id = sim.spawn_item(
            item,
            Vec3::new(0.5, 10.0, 0.5),
            Vec3::default(),
            ItemLifecycle::newly_dropped(1, 64),
        );
        (id, sim.item_position(id).expect("spawned item has a position").y)
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current_y = mobs.with(|sim| sim.item_position(item_id).map(|position| position.y));
        if current_y.is_some_and(|y| y < initial_y - 0.01) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the authoritative integrated tick loop never advanced item {item_id} from y={initial_y}; ticks={:?}",
            server.tick_stats().map(|stats| stats.tick_count),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        server.tick_stats().is_some_and(|stats| stats.tick_count > 0),
        "item movement must come from the integrated world's production tick loop"
    );
    server.shutdown().await;
}

struct FakeProtocol;

impl lodestone_server::ServerProtocol for FakeProtocol {
    fn decode(
        &self,
        _state: lodestone_core::State,
        _packet_id: i32,
        _payload: &[u8],
    ) -> lodestone_server::ServerBound {
        lodestone_server::ServerBound::Ignored
    }

    fn login_success(&self, _username: &str, _uuid: uuid::Uuid) -> Vec<lodestone_server::ServerDirective> {
        Vec::new()
    }

    fn begin_configuration(&self) -> Vec<lodestone_server::ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<lodestone_server::ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> lodestone_server::ServerDirective {
        lodestone_server::ServerDirective::Send {
            packet_id: 0,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
    ) -> lodestone_server::ServerDirective {
        lodestone_server::ServerDirective::Send {
            packet_id: 0,
            payload: Vec::new(),
        }
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> lodestone_server::ServerDirective {
        lodestone_server::ServerDirective::Send {
            packet_id: 0,
            payload: Vec::new(),
        }
    }
}
