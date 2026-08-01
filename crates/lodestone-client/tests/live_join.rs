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

/// End-to-end gate for issue #288: the server's `minecraft:dimension_type`
/// registry must reach the **read model**, not merely decode.
///
/// This is the island check. Everything between the wire and here can be
/// individually correct and still deliver nothing: the decode
/// (`lodestone_v770::packets::registry`), the event
/// (`ClientEvent::DimensionTypeChanged`), the component
/// (`lodestone_ecs::session::ServerDimensionType`), the fold
/// (`apply_local_player_state`) — and the chain still reaches zero pixels unless
/// `lodestone_ecs::session::handles_event` lists the variant, because
/// `SharedState::apply` routes on that switch alone. A hermetic test of any one
/// link passes either way. This asserts the far end.
///
/// `min_y`/`height` are checked as well as `has_skylight` because those are the
/// values the live mesher's column geometry comes from, and they are the ones a
/// hardcoded stand-in would most plausibly get right by accident — so all three
/// are read off the same snapshot the shell's `sky_default_for_dimension` reads.
#[tokio::test]
#[ignore = "requires a live Minecraft 26.2 server on 127.0.0.1:25565"]
async fn the_servers_dimension_type_registry_reaches_the_read_model() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via the live-v770 feature");

    let (handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to live server");

    // The pre-login control: nothing has been folded yet, so the field must be
    // absent. Without this, a hardcoded `Some(overworld)` would satisfy every
    // assertion below.
    assert_eq!(
        handle.player().dimension_type,
        None,
        "before login there is no dimension type — not a defaulted overworld"
    );

    let deadline = Duration::from_secs(30);
    let reached_play = tokio::time::timeout(deadline, async {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::Login { .. } => return true,
                ClientEvent::Disconnect { reason } => {
                    panic!("server disconnected us: {}", reason.to_plain_string());
                }
                _ => {}
            }
        }
        false
    })
    .await;
    assert_eq!(reached_play, Ok(true), "never reached Play state");

    let player = handle.player();
    let dimension_type = player.dimension_type.as_ref().unwrap_or_else(|| {
        panic!(
            "the dimension type never reached PlayerSnapshot — the decode is an \
             island. Check `lodestone_ecs::session::handles_event` has a \
             `DimensionTypeChanged` arm. (dimension = {:?})",
            player.dimension
        )
    });
    // Expected values from Mojang's own
    // `.cache/mc/26.2/client-src/data/minecraft/dimension_type/overworld.json`.
    assert_eq!(dimension_type.name.to_string(), "minecraft:overworld");
    assert!(dimension_type.has_skylight);
    assert!(!dimension_type.has_ceiling);
    assert_eq!(dimension_type.min_y, -64);
    assert_eq!(dimension_type.height, 384);
    assert_eq!(dimension_type.section_count(), 24);

    drop(handle);
}
