//! Live acceptance test against a real 1.12.2 server (protocol 340).
//!
//! This test is gated behind the `live-v1-9` feature AND `#[ignore]`, so the
//! default `cargo test` stays hermetic and version-free. Run it against a real
//! 1.12.2 server (offline mode) with:
//!
//! ```text
//! cargo test -p lodestone-client --features live-v1-9 -- --ignored
//! ```
//!
//! The server host/port default to `127.0.0.1:25567` (the 1.12.2 container uses
//! a different port from the 26.2 container on 25565 and the 1.8 container on
//! 25566) and can be overridden with the `LODESTONE_V340_HOST` /
//! `LODESTONE_V340_PORT` environment variables.
//!
//! It exercises the full public API and the real 1.12.2 join flow: connect over
//! TCP, complete handshake -> login -> play (there is no configuration state
//! before 1.20.2), confirm the client reaches Play (a `Login` event) and
//! receives a keep-alive. Reaching Play at all also proves the 1.9+ teleport
//! confirm choreography works, because a 1.12.2 server rubber-bands (and
//! eventually times out) a client that never confirms its initial teleport.
#![cfg(feature = "live-v1-9")]

use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use uuid::Uuid;

mod common;
use common::unique_username;

#[tokio::test]
#[ignore = "requires a live 1.12.2 Minecraft server (default 127.0.0.1:25567)"]
async fn joins_real_1_12_2_server_and_receives_keep_alive() {
    let host = std::env::var("LODESTONE_V340_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = std::env::var("LODESTONE_V340_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(25567);

    let server = ServerAddress { host, port };
    let profile = LoginProfile {
        // Per-run unique: shared offline-mode player files can be poisoned by a
        // death persisted from any other run under the same name. See
        // `common::unique_username`.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };

    let adapter = lodestone_registry::adapter_for_protocol(340)
        .expect("v1-9 family compiled into the registry via the live-v1-9 feature");

    let (handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to live 1.12.2 server");

    let mut reached_play = false;
    let mut got_keep_alive = false;

    let deadline = Duration::from_secs(30);
    let result = tokio::time::timeout(deadline, async {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::Login { .. } => reached_play = true,
                ClientEvent::KeepAlive { .. } => {
                    got_keep_alive = true;
                    break;
                }
                ClientEvent::Disconnect { reason } => {
                    panic!("server disconnected us: {}", reason.to_plain_string());
                }
                _ => {}
            }
        }
    })
    .await;

    assert!(result.is_ok(), "timed out before receiving a keep-alive");
    assert!(reached_play, "never reached Play state");
    assert!(got_keep_alive, "never received a keep-alive");

    drop(handle);
}
