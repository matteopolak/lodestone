//! End-to-end proof that the WebSocket relay path works — from *native* code.
//!
//! This deliberately isolates "does the WebSocket byte pipe carry a real
//! Minecraft session" from "does wasm work". It spins up the protocol-blind
//! relay in-process, points it at a real vanilla server, and drives a normal
//! `lodestone-client` through a [`WsTransport`] instead of TCP. If this passes,
//! a later browser failure is a browser problem, not a protocol or relay
//! problem.
//!
//! `#[ignore]` by default (it needs a live server). Run it with:
//!
//! ```text
//! docker start lodestone-mc262   # a vanilla server on 127.0.0.1:25565
//! cargo test -p lodestone-relay --test live_ws_join -- --ignored --nocapture
//! ```

use lodestone_testsupport::unique_username;
use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress, VersionAdapter};
use lodestone_net::WsTransport;
use tokio::net::TcpListener;
use uuid::Uuid;

/// The real vanilla server (offline mode, flat world) the relay bridges to.
const MC_SERVER: &str = "127.0.0.1:25565";
/// v26-2 family protocol number (matches the live TCP join test).
const PROTOCOL: i32 = 776;

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn joins_real_server_through_relay_over_websocket() {
    // 0. Precondition, checked loudly. An `#[ignore]`d test is already an explicit
    //    opt-in to run, so a missing server is a FAILURE with a fix — not a silent
    //    pass, and not the ambiguous 30 s timeout below that reads like a protocol
    //    regression. Probing the real server here makes "your env is down" crisply
    //    distinguishable from "the relay/protocol broke".
    if tokio::net::TcpStream::connect(MC_SERVER).await.is_err() {
        panic!(
            "live relay test requires a vanilla server on {MC_SERVER}, which is not \
             reachable.\n  fix:  docker start lodestone-mc262\n  run:  cargo test -p \
             lodestone-relay --test live_ws_join -- --ignored --nocapture"
        );
    }

    // 1. Start the protocol-blind relay in-process on an ephemeral port,
    //    bridging WebSocket clients to the real TCP server.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind relay listener");
    let relay_addr = listener.local_addr().expect("relay local addr");
    tokio::spawn(lodestone_relay::serve(listener, Some(MC_SERVER.to_string())));

    // 2. Dial the relay over WebSocket — no raw TCP to the server anywhere in
    //    the client path.
    let ws_url = format!("ws://{relay_addr}");
    let transport = WsTransport::connect(&ws_url)
        .await
        .expect("connect to relay over websocket");

    // 3. Drive the ordinary client over that WebSocket transport. The handshake
    //    still advertises the real server address; only the byte pipe changed.
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        // Unique per run. In offline mode the server derives the player's UUID
        // from the *username* (`OfflinePlayer:<name>`) and discards the UUID we
        // send, so a shared username silently shares one on-disk player file
        // across runs — inherit a dead one and you get zero chunks while login,
        // keep-alives and entity spawns all look healthy. `Uuid::new_v4()` does
        // NOT isolate runs for this reason; the username must.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter: Box<dyn VersionAdapter> = lodestone_registry::adapter_for_protocol(PROTOCOL)
        .expect("v26-2 family compiled into the registry via the v26-2 feature");

    let (handle, mut events) = ClientBuilder::new(server, profile, adapter).connect_with(transport);

    // 4. Confirm the full handshake→login→configuration→play flow completes and
    //    a keep-alive arrives — exactly the assertions the TCP live test makes.
    let mut reached_play = false;
    let mut got_keep_alive = false;

    let result = tokio::time::timeout(Duration::from_secs(30), async {
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
    assert!(
        reached_play,
        "never reached Play state over the WebSocket relay"
    );
    assert!(
        got_keep_alive,
        "never received a keep-alive over the WebSocket relay"
    );

    drop(handle);
}
