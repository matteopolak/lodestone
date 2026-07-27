//! Live acceptance test against a real vanilla server (Phase 1 gate).
//!
//! This test is gated behind the `live-v770` feature AND `#[ignore]`, so the
//! default `cargo test` stays hermetic and version-free. Run it against a real
//! server (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-client --features live-v770 -- --ignored
//! ```
//!
//! It exercises the full public API and the real join flow: connect over TCP,
//! complete handshake -> login -> configuration -> play, and confirm the client
//! reaches Play (a `Login` event) and receives a keep-alive.
//!
//! Version selection goes through `lodestone-registry`: enabling the `live-v770`
//! feature turns on the registry's `v770` family, and this test asks the
//! registry for the adapter by protocol number. `lodestone-client` never names a
//! concrete version crate.
#![cfg(feature = "live-v770")]

use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use uuid::Uuid;

mod common;
use common::unique_username;

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn joins_real_server_and_receives_keep_alive() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        // Per-run unique: shared offline-mode player files can be poisoned by a
        // death persisted from any other run under the same name. See
        // `common::unique_username`.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };

    // Version selection via the registry: the `live-v770` feature enables the
    // registry's v770 family, which this resolves by protocol number.
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via the live-v770 feature");

    let (handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to live server");

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
