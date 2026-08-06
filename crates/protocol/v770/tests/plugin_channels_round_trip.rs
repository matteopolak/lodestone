//! **The wire path for wire-level plugin messaging, end to end** (issue #335).
//!
//! The server side of plugin messaging landed as a version-free module
//! ([`PluginChannelRegistry`]/[`ClientChannels`] in `lodestone-server`'s
//! `plugin_channels.rs`), a new [`ServerBound::CustomPayload`] variant lifted
//! by the protocol seam, a defaulted `ServerProtocol::encode_custom_payload`
//! seam method, its `v770` implementation, and a broadcast drain in
//! `serve_play`'s `container_sync_tick` timer — the same shape as issue #334's
//! resource-pack push. A decoder with no encoder, an encoder nobody reaches,
//! or a registry nobody dispatches through is the island shape this file
//! exists to close: every gate starts at a real action a real client sends (or
//! a [`PluginChannelRegistry::broadcast`] a host publishes) and ends at the
//! other side of a real in-memory `Connection`.
//!
//! # What is real here, and why each piece has to be
//!
//! Nothing in this file is a stand-in for the thing it is testing:
//!
//! | piece | the real thing |
//! |---|---|
//! | the inbound producer | the real client's `ClientAction::SendCustomPayload` action, encoded by `V770Adapter` |
//! | the decode | `V770ServerProtocol`'s `custom_payload` arms lifting `ServerBound::CustomPayload` |
//! | the dispatch | `PluginChannelRegistry::dispatch` in `serve_play`'s packet loop |
//! | the handler | a real `PluginChannelHandler` installed on the registry |
//! | the outbound producer | `PluginChannelRegistry::broadcast`, the same call a future #77 plugin makes |
//! | the outbound drain | `serve_play`'s `container_sync_tick` arm + `encode_custom_payload` |
//! | the client | the real `V770Adapter` decode arm into `ClientEvent::CustomPayload` |
//!
//! # Scope
//!
//! Issue #77 owns the plugin-facing API; this file verifies the wire-level
//! registry and dispatch underneath it: registered-channel delivery,
//! unregistered-channel drop, and the server→client broadcast filter
//! (`ClientChannels`) with the real encoder/decoder join.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ClientAction, ClientEvent, ConnectionState, Directive, ResourceKey, VersionAdapter};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    BlockEntityHandle, BlockTickFeed, ExplosionFeed, MobHandle, NoEntities, PluginChannelHandler,
    PluginChannelRegistry, ServerDirective, ServerProtocol, WorldgenChunkSource,
    serve_connection_with_plugin_channels,
};
use lodestone_v770::packet_ids::play;
use lodestone_v770::{V770Adapter, V770ServerProtocol, adapter};
use lodestone_worldgen::density::Density;
use uuid::Uuid;

fn profile(name: &str, uuid: Uuid) -> LoginProfile {
    LoginProfile {
        username: name.into(),
        uuid,
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// Deterministic, noise-free terrain — content is irrelevant here, but the
/// vertical extent must be the real overworld shape or the client's hardcoded
/// decode misaligns. Same source `resource_pack_push.rs` uses, for the same
/// reason.
fn cheap_source() -> WorldgenChunkSource {
    WorldgenChunkSource::new(
        Density::YClampedGradient {
            from_y: -64.0,
            to_y: 64.0,
            from_value: 1.0,
            to_value: -1.0,
        },
        -64,
        384,
    )
}

fn key(name: &str) -> ResourceKey {
    name.parse().expect("valid channel name")
}

/// A recording handler: captures every `(channel, data)` delivered to it, so a
/// test can assert exactly what the server dispatched — and, for the negative
/// control, assert that the *unregistered* channel's payload never arrived.
#[derive(Debug, Default)]
struct RecordingHandler {
    calls: Mutex<Vec<(ResourceKey, Vec<u8>)>>,
}

impl PluginChannelHandler for RecordingHandler {
    fn on_payload(&self, channel: &ResourceKey, data: &[u8]) {
        self.calls
            .lock()
            .expect("recording handler poisoned")
            .push((channel.clone(), data.to_vec()));
    }
}

/// Blocks until `calls` holds at least `n` entries, then returns a snapshot.
/// A bounded poll (like `resource_pack_push.rs`'s `wait_for_push`), never an
/// unbounded hang: the server processes inbound frames asynchronously, and
/// this is the seam where "the packet never arrived" would otherwise deadlock
/// the test.
async fn wait_for_calls(
    calls: &Mutex<Vec<(ResourceKey, Vec<u8>)>>,
    n: usize,
) -> Vec<(ResourceKey, Vec<u8>)> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let got = calls.lock().expect("poisoned").clone();
        if got.len() >= n {
            return got;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("handler never received {n} payload(s); got {got:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// **The inbound half of the issue.** A payload the client sends on a channel
/// the server registered reaches the server's handler, with the channel and
/// the raw bytes intact — through the real client encoder, the real `v770`
/// decode arm, and the real registry dispatch.
#[tokio::test]
async fn a_payload_on_a_registered_channel_reaches_the_servers_handler() {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();
    let plugin_channels = PluginChannelRegistry::new();
    let handler = Arc::new(RecordingHandler::default());
    plugin_channels.register(key("mod:greeting"), handler.clone());
    let server_plugin_channels = plugin_channels.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_plugin_channels(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &server_plugin_channels,
        )
        .await;
    });

    let (mut handle, mut _events) = ClientBuilder::new(
        address(),
        profile("Channelo", Uuid::new_v4()),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    handle
        .send_action(ClientAction::SendCustomPayload {
            channel: key("mod:greeting"),
            data: b"hello from the client".to_vec(),
        })
        .expect("send the payload on the registered channel");

    // The server processes frames asynchronously, so the assertion is a bounded
    // wait for the handler record rather than an immediate check.
    let calls = wait_for_calls(&handler.calls, 1).await;
    assert_eq!(calls[0].0, key("mod:greeting"));
    assert_eq!(calls[0].1, b"hello from the client".to_vec());

    handle.shutdown();
    server.abort();
}

/// **The negative control for the issue's second requirement.** A payload on a
/// channel the server registered no interest in must be dropped — not crash
/// the connection, not surface an error, and not reach any handler — exactly
/// vanilla's `DiscardedPayload` fallback. The assertion of absence is paired
/// with a control proving the detector fires: the *same* handler then receives
/// a payload on the registered channel, so an empty record is an empty record
/// because the channel was dropped, not because the handler never fires.
#[tokio::test]
async fn a_payload_on_an_unregistered_channel_is_dropped_and_the_connection_survives() {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();
    let plugin_channels = PluginChannelRegistry::new();
    let handler = Arc::new(RecordingHandler::default());
    plugin_channels.register(key("mod:greeting"), handler.clone());
    let server_plugin_channels = plugin_channels.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_plugin_channels(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &server_plugin_channels,
        )
        .await;
    });

    let (mut handle, mut _events) = ClientBuilder::new(
        address(),
        profile("Channelo", Uuid::new_v4()),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    // No handler registered for this channel.
    handle
        .send_action(ClientAction::SendCustomPayload {
            channel: key("mod:unrelated"),
            data: b"ignored".to_vec(),
        })
        .expect("send the payload on the unregistered channel");

    // Give the drop a real chance to cross the transport and be dispatched
    // before asserting anything. The handler must not have seen it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        handler.calls.lock().expect("poisoned").is_empty(),
        "the unregistered channel's payload must be dropped before any handler"
    );
    assert!(
        !server.is_finished(),
        "an unregistered channel's payload must not take down the connection \
         (a dispatch error in `serve_play` would finish the server task)"
    );

    // The control: the *same* handler must still receive a registered-channel
    // payload — proving the empty record above was the drop, not a dead handler.
    handle
        .send_action(ClientAction::SendCustomPayload {
            channel: key("mod:greeting"),
            data: b"control".to_vec(),
        })
        .expect("send the control payload");
    let calls = wait_for_calls(&handler.calls, 1).await;
    assert_eq!(calls[0].0, key("mod:greeting"));
    assert_eq!(calls[0].1, b"control".to_vec());

    handle.shutdown();
    server.abort();
}

/// **The outbound half: the broadcast drain reaches a client that announced
/// the channel.** A host-published [`PluginChannelRegistry::broadcast`]
/// travels through `serve_play`'s `container_sync_tick` drain, the
/// hand-written `v770` encoder, and a real in-memory `Connection`, and comes
/// out the other side as a real client `ClientEvent::CustomPayload` with both
/// fields intact.
///
/// The client-support filter (`ClientChannels`) is exercised honestly: the
/// test client first announces `mod:greeting` over `minecraft:register` (the
/// historical control channel), then the test confirms its own inbound
/// "ping" reached the handler — packet order on one connection guarantees the
/// register landed *before* this broadcast is drained, so the filter has
/// something to pass. Without the register the broadcast would be skipped by
/// the filter and never reach the client, which is the same mechanism a mod
/// channel the client never announced is skipped by.
#[tokio::test]
async fn a_broadcast_reaches_a_client_that_registered_the_channel() {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();
    let plugin_channels = PluginChannelRegistry::new();
    let handler = Arc::new(RecordingHandler::default());
    plugin_channels.register(key("mod:greeting"), handler.clone());
    let server_plugin_channels = plugin_channels.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_plugin_channels(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &server_plugin_channels,
        )
        .await;
    });

    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("Channelo", Uuid::new_v4()),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    // The client announces support for `mod:greeting` (the `minecraft:register`
    // control channel), then sends a "ping" on it. The server's inbound loop
    // processes frames in arrival order on one connection, so once the handler
    // has recorded the ping, the register is definitely applied to this
    // connection's `ClientChannels` — a sound synchronization for the broadcast
    // below.
    handle
        .send_action(ClientAction::SendCustomPayload {
            channel: key("minecraft:register"),
            data: b"mod:greeting".to_vec(),
        })
        .expect("announce the channel");
    handle
        .send_action(ClientAction::SendCustomPayload {
            channel: key("mod:greeting"),
            data: b"ping".to_vec(),
        })
        .expect("send the sync ping");
    let _ = wait_for_calls(&handler.calls, 1).await;

    // Publish *after* the register is known-applied (see above): the drain runs
    // on `container_sync_tick` (50 ms), so a broadcast published too early
    // would be filtered by an empty `ClientChannels` and never observed — the
    // publish point is load-bearing, exactly like `resource_pack_push.rs`.
    plugin_channels.broadcast(key("mod:greeting"), b"hello to the client");

    // The client must decode the broadcast as a real `ClientEvent::CustomPayload`.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::CustomPayload { channel, data })) => {
                assert_eq!(channel.to_string(), "mod:greeting");
                assert_eq!(data, b"hello to the client".to_vec());
                break;
            }
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => {
                panic!("the broadcast never reached the client as a custom payload")
            }
        }
    }

    handle.shutdown();
    server.abort();
}

/// **The encoder/decoder join, without any timing.** The server's
/// `encode_custom_payload` (play id) must decode back into
/// `ClientEvent::CustomPayload` under the pre-existing client decoder — the
/// same independent-halves argument `server_chat_broadcast.rs` makes about
/// `system_chat`: agreement between two independently-authored halves is
/// evidence about the wire; agreement between an encoder and its mirror image
/// is not.
#[test]
fn the_server_broadcast_encoder_is_decoded_by_the_client_as_a_custom_payload() {
    let directive = V770ServerProtocol.encode_custom_payload(&key("mod:greeting"), b"hi");
    let (packet_id, payload) = match directive {
        ServerDirective::Send { packet_id, payload } => (packet_id, payload),
        other => panic!("encode_custom_payload must produce a Send, got {other:?}"),
    };
    assert_eq!(
        packet_id, play::clientbound::CUSTOM_PAYLOAD,
        "the drain runs in play, so the encoder must use the play id"
    );

    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut lodestone_world::World::new(),
            ConnectionState::Play,
            packet_id,
            &payload,
        )
        .expect("our own custom_payload frame must parse under the client decoder");
    let mut seen = None;
    for directive in directives {
        if let Directive::Emit(ClientEvent::CustomPayload { channel, data }) = directive {
            seen = Some((channel.to_string(), data));
        }
    }
    assert_eq!(
        seen,
        Some(("mod:greeting".to_owned(), b"hi".to_vec())),
        "the client decoder must recover exactly the channel and bytes the server encoded"
    );
}
