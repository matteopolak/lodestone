//! Diagnostic gate for the reported defect "integrated-server chat has no
//! sender name": a **single** connection through
//! [`IntegratedServer::open_in_memory_with_mobs`] — the exact constructor
//! `lodestone-shell`'s native singleplayer join uses (`net.rs`'s `None =>
//! lodestone_server::IntegratedServer::open_in_memory_with_mobs(...)` arm) —
//! rather than `server_chat_broadcast.rs`'s two-hand-rolled-`PlayerAwareSource`
//! shape. Drives the **real** `lodestone-client` driver (not the bare
//! adapter), so this also exercises `driver.rs`'s own `ClientEvent::Chat`
//! handling.
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::ClientEvent;
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v770::{V770ServerProtocol, adapter};

struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

fn profile(name: &str) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid: uuid::Uuid::new_v4(),
    }
}

#[tokio::test]
async fn a_lone_singleplayer_connection_sees_its_own_name_in_chat() {
    let mob_area = (-1..=1, -1..=1);
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        AirSource,
        mob_area,
        (0, 0),
        0,
        0,
    );

    let name = "SoloPlayer";
    let (mut handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: "memory".into(),
            port: 0,
        },
        profile(name),
        Box::new(adapter()),
    )
    .connect_with(client_io);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    handle.chat("hello from solo").expect("client still connected");

    let expected = format!("<{name}> hello from solo");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seen_lines = Vec::new();
    let found = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break false;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(ClientEvent::Chat { text, .. })) => {
                let plain = text.to_plain_string();
                seen_lines.push(plain.clone());
                if plain == expected {
                    break true;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break false,
            Err(_) => break false,
        }
    };

    assert!(
        found,
        "the lone singleplayer connection's own chat message must come back as {expected:?}; saw {seen_lines:?}"
    );

    handle.shutdown();
    let _ = handle.join().await;
    drop(server);
}
