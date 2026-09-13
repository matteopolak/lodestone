//! Cross-format text oracle: capture harness + live equivalence proof.
//!
//! F3 introduced a single format-agnostic `Text` tree in `lodestone-model` with
//! two thin front-ends: `Text::from_json` (pre-1.20.3 / 1.8 chat) and
//! `Text::from_nbt` (modern chat). This test grounds that seam in *real* data
//! rather than fixtures: it joins both live servers, drives the real 1.8 (JSON)
//! and modern (NBT) join choreography through the registry adapters at the raw
//! `Connection` level, and records the exact chat component bytes each server
//! sends.
//!
//! Two things run here, both `#[ignore]`d and behind `capture-text-oracle`:
//!
//! * [`capture_real_chat_components`] is a **regeneration tool**. It writes the
//!   captured JSON payload and NBT component to
//!   `crates/lodestone-model/tests/data/`, where a *hermetic* model test
//!   (`cross_format_oracle_*`) replays them through both front-ends and asserts
//!   they flatten to the same plain text. That hermetic test is the actual
//!   cross-format oracle and runs with no servers.
//! * [`live_join_messages_flatten_equally`] is the **live proof**: on each server
//!   a *watcher* client joins and idles while a second *joiner* client connects,
//!   so the server broadcasts a `<name> joined the game` message to the watcher.
//!   The same logical message — a `multiplayer.player.joined` translate — arrives
//!   as JSON from the 1.8 server and as NBT from the modern one, and the test
//!   asserts the two flatten identically. (A watcher joins a second client rather
//!   than sending its own chat because modern servers kick unsigned player chat.)
//!
//! Run against all three-of-a-kind live servers with:
//!
//! ```text
//! cargo test -p lodestone-client --features capture-text-oracle -- --ignored
//! ```
#![cfg(feature = "capture-text-oracle")]

use std::path::PathBuf;
use std::time::Duration;

use lodestone_model::{
    ClientAction, ClientEvent, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use tokio::net::TcpStream;
use uuid::Uuid;

mod common;
use common::unique_username;

/// Where the model's hermetic oracle test reads its golden data from.
fn model_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("lodestone-model")
        .join("tests")
        .join("data")
}

/// Captures a real "`<name>` joined the game" broadcast from `host:port` in the
/// server's native chat format.
///
/// A *watcher* client joins and idles in Play; a second *joiner* client then
/// connects, and the server broadcasts the join message to the watcher. This
/// yields identical logical content — the `multiplayer.player.joined` translate
/// component with the joiner's name — from both a 1.8 (JSON) and a modern (NBT)
/// server, with no client-sent chat (modern servers kick unsigned chat). The
/// returned tuple is the raw inbound packet payload that carried the component
/// plus the flattened plain text the adapter decoded from it.
///
/// This re-implements the connection layer's directive loop in miniature so the
/// capture can observe the raw bytes a `handle_packet` call consumes.
async fn capture_join_broadcast(
    host: &str,
    port: u16,
    adapter: &dyn VersionAdapter,
    watcher_name: &str,
    joiner_name: &str,
) -> (Vec<u8>, String) {
    let mut watcher = Session::connect(host, port, adapter, watcher_name).await;
    // Get the watcher fully into Play before the joiner connects, so it is
    // present to receive the broadcast.
    watcher.pump_until_play(adapter).await;

    // Spawn the joiner on its own connection; it stays alive (answering
    // keep-alives) until this capture returns and drops the handle.
    let joiner_host = host.to_owned();
    let joiner_name = joiner_name.to_owned();
    let protocol = adapter.protocol_version();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let joiner = tokio::spawn(async move {
        let joiner_adapter =
            lodestone_registry::adapter_for_protocol(protocol).expect("registry has this family");
        let mut session =
            Session::connect(&joiner_host, port, joiner_adapter.as_ref(), &joiner_name).await;
        session.pump_until_play(joiner_adapter.as_ref()).await;
        // Idle in Play, answering keep-alives, until told to stop.
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                stepped = session.step(joiner_adapter.as_ref()) => {
                    if !stepped { break; }
                }
            }
        }
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let captured = loop {
        let step = tokio::time::timeout_at(deadline, watcher.next_chat(adapter)).await;
        match step {
            Ok(Some((payload, _adapter_flat))) => {
                // Flatten the *raw* component through F3's own front-end rather
                // than trusting the version adapter's flattening: this is the
                // cross-format oracle exercising `Text::from_json` /
                // `Text::from_nbt` directly on real server bytes.
                let flat = flatten_component(adapter.protocol_version(), &payload);
                eprintln!("[{}] chat: {flat:?}", adapter.protocol_version());
                if flat.contains("joined the game") {
                    break (payload, flat);
                }
            }
            Ok(None) => panic!("watcher connection closed before a join broadcast arrived"),
            Err(_) => panic!("timed out waiting for join broadcast"),
        }
    };

    let _ = stop_tx.send(());
    let _ = joiner.await;
    captured
}

/// Flattens a raw clientbound chat packet payload through F3's format-agnostic
/// `Text` front-ends: JSON for 1.8 (protocol 47), NBT for modern. This is the
/// heart of the cross-format oracle — the same tree operations flatten two
/// different serializations of the same logical message.
fn flatten_component(protocol: i32, payload: &[u8]) -> String {
    let mut reader = lodestone_core::Reader::new(payload);
    match protocol {
        47 => {
            // 1.8 clientbound chat: VarInt-prefixed JSON string, then a position
            // byte. Only the string matters for the component.
            let json = reader.string(usize::MAX).unwrap_or_default();
            lodestone_model::Text::from_json(&json).to_plain_string()
        }
        _ => {
            // Modern clientbound chat leads with the network NBT component.
            match lodestone_core::read_network_nbt(&mut reader) {
                Ok(nbt) => lodestone_model::Text::from_nbt(&nbt).to_plain_string(),
                Err(_) => String::new(),
            }
        }
    }
}

/// A minimal raw-connection session that drives one adapter's directive loop.
struct Session {
    conn: Connection<TcpStream>,
    state: lodestone_model::ConnectionState,
    world: lodestone_world::World,
}

impl Session {
    /// Connects and issues `begin_login`.
    async fn connect(host: &str, port: u16, adapter: &dyn VersionAdapter, username: &str) -> Self {
        let profile = LoginProfile {
            username: username.to_owned(),
            uuid: Uuid::new_v4(),
        };
        let server = ServerAddress {
            host: host.to_owned(),
            port,
        };
        let conn = Connection::<TcpStream>::connect((host, port))
            .await
            .expect("connect to live server");
        let mut session = Self {
            conn,
            state: lodestone_model::ConnectionState::Handshaking,
            world: lodestone_world::World::new(),
        };
        let directives = adapter.begin_login(&profile, &server).expect("begin_login");
        session.apply(directives).await;
        session
    }

    /// Reads and processes one packet, answering keep-alives. Returns `false`
    /// when the connection closed.
    async fn step(&mut self, adapter: &dyn VersionAdapter) -> bool {
        let Some((packet_id, payload)) = self.conn.read_packet().await.expect("read") else {
            return false;
        };
        if let Ok(directives) =
            adapter.handle_packet(&mut self.world, self.state, packet_id, &payload)
        {
            self.after(adapter, &directives).await;
            self.apply(directives).await;
        }
        true
    }

    /// Pumps packets until the adapter reports Play.
    async fn pump_until_play(&mut self, adapter: &dyn VersionAdapter) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while self.state != lodestone_model::ConnectionState::Play {
            let ok = tokio::time::timeout_at(deadline, self.step(adapter))
                .await
                .expect("timed out reaching Play");
            assert!(ok, "connection closed before reaching Play");
        }
    }

    /// Reads packets until the next chat component, returning its raw payload and
    /// flattened text, or `None` if the connection closed.
    async fn next_chat(&mut self, adapter: &dyn VersionAdapter) -> Option<(Vec<u8>, String)> {
        loop {
            let (packet_id, payload) = self.conn.read_packet().await.expect("read")?;
            let Ok(directives) =
                adapter.handle_packet(&mut self.world, self.state, packet_id, &payload)
            else {
                continue;
            };
            let chat = directives.iter().find_map(|directive| match directive {
                Directive::Emit(ClientEvent::Chat { text, .. }) => Some(text.to_plain_string()),
                _ => None,
            });
            self.after(adapter, &directives).await;
            self.apply(directives).await;
            if let Some(flat) = chat {
                return Some((payload, flat));
            }
        }
    }

    /// Answers keep-alives surfaced by a directive batch.
    async fn after(&mut self, adapter: &dyn VersionAdapter, directives: &[Directive]) {
        for directive in directives {
            if let Directive::Emit(ClientEvent::KeepAlive { id }) = directive
                && let Ok(Some((packet_id, payload))) =
                    adapter.encode_action(self.state, &ClientAction::KeepAliveResponse { id: *id })
            {
                self.conn
                    .write_packet(packet_id, &payload)
                    .await
                    .expect("write keep-alive");
            }
        }
    }

    /// Executes a directive batch, mirroring the driver's ordering rules.
    async fn apply(&mut self, directives: Vec<Directive>) {
        for directive in directives {
            match directive {
                Directive::Send { packet_id, payload } => {
                    self.conn
                        .write_packet(packet_id, &payload)
                        .await
                        .expect("write");
                }
                Directive::SetState(next) => self.state = next,
                Directive::SetCompression(threshold) => self.conn.set_compression(threshold),
                _ => {}
            }
        }
    }
}

#[tokio::test]
#[ignore = "regeneration tool: captures real chat bytes from both live servers"]
async fn capture_real_chat_components() {
    let json_host = std::env::var("LODESTONE_V47_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let json_port = std::env::var("LODESTONE_V47_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(25566);
    let nbt_host = std::env::var("LODESTONE_V770_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let nbt_port = std::env::var("LODESTONE_V770_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(25565);

    let v1_8 = lodestone_registry::adapter_for_protocol(47).expect("v1_8 in registry");
    let v26_2 = lodestone_registry::adapter_for_protocol(776).expect("v26_2 in registry");

    let json = capture_join_broadcast(
        &json_host,
        json_port,
        v1_8.as_ref(),
        &unique_username(),
        &unique_username(),
    )
    .await;
    let nbt = capture_join_broadcast(
        &nbt_host,
        nbt_port,
        v26_2.as_ref(),
        &unique_username(),
        &unique_username(),
    )
    .await;
    let json_payload = json.0;
    let nbt_payload = nbt.0;

    // Inspect the raw NBT the modern server sent so we can confirm it carries a
    // real component before baking it as a golden.
    let mut reader = lodestone_core::Reader::new(&nbt_payload);
    if let Ok(nbt) = lodestone_core::read_network_nbt(&mut reader) {
        let text = lodestone_model::Text::from_nbt(&nbt);
        eprintln!(
            "modern NBT component flattens (via from_nbt) to: {:?}",
            text.to_plain_string()
        );
    }

    let dir = model_data_dir();
    std::fs::create_dir_all(&dir).expect("create data dir");
    std::fs::write(dir.join("join_1_8_json.bin"), &json_payload).expect("write json golden");
    std::fs::write(dir.join("join_modern_nbt.bin"), &nbt_payload).expect("write nbt golden");

    eprintln!(
        "captured JSON payload ({} bytes) and NBT payload ({} bytes) to {}",
        json_payload.len(),
        nbt_payload.len(),
        dir.display()
    );
}

#[tokio::test]
#[ignore = "requires both live servers (1.8 on :25566, modern on :25565)"]
async fn live_join_messages_flatten_equally() {
    let json_host = std::env::var("LODESTONE_V47_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let json_port = std::env::var("LODESTONE_V47_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(25566);
    let nbt_host = std::env::var("LODESTONE_V770_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let nbt_port = std::env::var("LODESTONE_V770_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(25565);

    let v1_8 = lodestone_registry::adapter_for_protocol(47).expect("v1_8 in registry");
    let v26_2 = lodestone_registry::adapter_for_protocol(776).expect("v26_2 in registry");

    let (_, json_text) = capture_join_broadcast(
        &json_host,
        json_port,
        v1_8.as_ref(),
        &unique_username(),
        &unique_username(),
    )
    .await;
    let (_, nbt_text) = capture_join_broadcast(
        &nbt_host,
        nbt_port,
        v26_2.as_ref(),
        &unique_username(),
        &unique_username(),
    )
    .await;

    eprintln!("JSON server said: {json_text:?}");
    eprintln!("NBT  server said: {nbt_text:?}");

    assert!(
        json_text.contains("joined the game"),
        "expected a join broadcast from the 1.8 (JSON) server, got {json_text:?}"
    );
    assert_eq!(
        json_text, nbt_text,
        "the same join broadcast must flatten identically across JSON and NBT"
    );
}
