//! Production-path proof that the integrated world's authoritative tick loop
//! advances a dropped item and its lifecycle counters, rather than leaving
//! item physics in a closed `MobSim`-only test.

#![cfg(not(target_arch = "wasm32"))]

use std::str::FromStr;
use std::time::{Duration, Instant};

use lodestone_core::{Reader, State, Writer};
use lodestone_entity::item_entity::ItemLifecycle;
use lodestone_net::Connection;
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
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        FakeProtocol,
        FlatFloor,
        (0..=0, 0..=0),
        (0, 0),
        0,
    );
    let mut client = Connection::new(client_io);
    client.write_packet(0, &[2]).await.expect("handshake");
    let mut login = Writer::default();
    login.string("ItemTick");
    client
        .write_packet(0, login.as_slice())
        .await
        .expect("login start");
    let (id, payload) = client
        .read_packet()
        .await
        .expect("read login success")
        .expect("login success packet");
    assert_eq!(id, 2);
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.string(16).expect("login username"), "ItemTick");
    client
        .write_packet(3, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(3, &[])
        .await
        .expect("configuration finish");
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
        let (current_y, lifecycle) = mobs.with(|sim| {
            (
                sim.item_position(item_id).map(|position| position.y),
                sim.item_lifecycle(item_id).copied(),
            )
        });
        if current_y.is_some_and(|y| y < initial_y - 0.01) {
            let lifecycle = lifecycle.expect("a moving item retains its lifecycle");
            assert!(lifecycle.age > 0, "the live tick must advance item age");
            assert!(
                lifecycle.pickup_delay < 10,
                "the live tick must advance item pickup delay"
            );
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
        state: State,
        packet_id: i32,
        payload: &[u8],
    ) -> lodestone_server::ServerBound {
        match state {
            State::Handshaking if packet_id == 0 => lodestone_server::ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == 0 => {
                let mut reader = Reader::new(payload);
                lodestone_server::ServerBound::LoginStart {
                    username: reader.string(16).expect("username"),
                    uuid: uuid::Uuid::nil(),
                }
            }
            State::Login if packet_id == 3 => lodestone_server::ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == 3 => {
                lodestone_server::ServerBound::ConfigurationFinished
            }
            _ => lodestone_server::ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, _uuid: uuid::Uuid) -> Vec<lodestone_server::ServerDirective> {
        let mut payload = Writer::default();
        payload.string(username);
        vec![lodestone_server::ServerDirective::Send {
            packet_id: 2,
            payload: payload.as_slice().to_vec(),
        }]
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
