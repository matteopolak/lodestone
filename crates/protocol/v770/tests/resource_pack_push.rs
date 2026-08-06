//! **The wire path for a server-initiated resource pack push, end to end**
//! (issue #334).
//!
//! The server side of the resource-pack lifecycle landed as a version-free
//! vocabulary struct ([`ResourcePackPush`]), a defaulted
//! `ServerProtocol::encode_resource_pack_push` seam method, its `v770`
//! implementation, and a host-observable [`ResourcePackPushFeed`] drained by
//! `serve_play`'s `container_sync_tick` timer into a real clientbound
//! `resource_pack_push` frame. A decoder with no encoder, or an encoder nobody
//! reaches, is the island shape this file exists to close: the gate below
//! starts at a push *published into a feed* — the exact call a future
//! `IntegratedServer` config surface makes — and ends at a real client
//! decoding the real `v770` bytes into `ClientEvent::ResourcePackPushed`.
//!
//! # What is real here, and why each piece has to be
//!
//! Nothing in this file is a stand-in for the thing it is testing:
//!
//! | piece | the real thing |
//! |---|---|
//! | the producer | `ResourcePackPushFeed::publish`, the same call a host's config surface makes |
//! | the drain | `serve_play`'s `container_sync_tick` arm calling `encode_resource_pack_push` |
//! | the encoder | `V770ServerProtocol::encode_resource_pack_push` (hand-written, play id 81) |
//! | the wire | protocol 776 `resource_pack_push` on a real in-memory `Connection` |
//! | the client | the real `V770Adapter` decode arm into `ClientEvent::ResourcePackPushed` |
//! | the reply | the client's `ClientAction::ResourcePackResponse`, decoded by the server without a disconnect |
//!
//! The decode side is already asserted byte-for-byte against hand-built payloads
//! in `clientbound_backlog.rs`; this file drives the *whole* path so an encoder
//! that is never called, or a drain that is never wired, fails here and nowhere
//! else.
//!
//! # Why the push arrives in Play, not Configuration
//!
//! Vanilla pushes during Configuration (its `ServerResourcePackConfigurationTask`);
//! this crate's `begin_configuration` is a static vec with no arguments to carry
//! a pack, and the feed's drain point is `serve_play`'s timer — so the push
//! reaches the client after the configuration handoff. Both `v770` decode arms
//! (Configuration and Play) are wire-identical, so the play-phase push is what
//! the current wiring can emit; a configuration-phase push is a documented
//! follow-up that needs a state-carrying call site.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ClientAction, ClientEvent, ResourcePackResponseKind, Text};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    NoEntities, ResourcePackPush, ResourcePackPushFeed, WorldgenChunkSource,
    serve_connection_with_resource_pack,
};
use lodestone_v770::{V770ServerProtocol, adapter};
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
/// decode misaligns. Same source `command_wire_path.rs` uses, for the same
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

/// A push with every field populated, including a 40-character hash (vanilla's
/// `MAX_HASH_LENGTH`) — so the gate exercises the hash cap's edge, not a
/// trivial short string.
fn sample_push() -> ResourcePackPush {
    ResourcePackPush {
        id: Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0),
        url: "https://example.com/pack.zip".to_owned(),
        hash: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        required: true,
        prompt: Some(Text::literal("Accept this pack?")),
    }
}

/// The first `ResourcePackPushed` event the client decodes off the wire, or a
/// panic with `what` as context if none arrives within the deadline — a bounded
/// wait (like `command_wire_path.rs`), never an unbounded hang.
async fn wait_for_push(events: &mut lodestone_client::EventStream) -> (Uuid, String, String, bool, Option<String>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::ResourcePackPushed {
                id,
                url,
                hash,
                required,
                prompt,
            })) => return (id, url, hash, required, prompt.map(|t| t.to_plain_string())),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => panic!("the pushed resource pack never reached the client"),
        }
    }
}

/// **The gate this issue is about.** A [`ResourcePackPush`] published into the
/// feed — the exact call a host's config surface makes — travels through
/// `serve_play`'s timer drain, the hand-written `v770` encoder, and a real
/// in-memory `Connection`, and comes out the other side as a real client
/// `ClientEvent::ResourcePackPushed` with every field intact.
///
/// The predictions are exact, not directional: the id, url, hash (the full
/// 40-character edge), the `required` flag, and the prompt's plain text are
/// each compared byte-for-byte, so a field dropped by the encoder or a hash
/// clipped by a length cap both fail here.
#[tokio::test]
async fn a_pushed_resource_pack_reaches_the_client_and_round_trips() {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();
    let resource_packs = ResourcePackPushFeed::default();
    // A second handle for the server task, so this test can keep publishing
    // into the feed after the spawn (a `ResourcePackPushFeed` is `Arc`-backed —
    // cloning the handle is not cloning the events).
    let server_resource_packs = resource_packs.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_resource_pack(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &NoEntities,
            0,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &server_resource_packs,
        )
        .await;
    });

    let (mut handle, mut events) = ClientBuilder::new(
        address(),
        profile("Packo", Uuid::new_v4()),
        Box::new(adapter()),
    )
    .connect_with(client_end);

    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("client never spawned");

    // Publish *after* the client is in Play: the drain runs on
    // `container_sync_tick` (50 ms), so a push published before the handoff
    // would sit in the feed until play starts, and a push published after the
    // event below is read would never be observed — both would still pass a
    // sloppy gate, so the publish point is load-bearing.
    let push = sample_push();
    resource_packs.publish(push.clone());

    let (id, url, hash, required, prompt) = wait_for_push(&mut events).await;
    assert_eq!(id, push.id);
    assert_eq!(url, push.url);
    assert_eq!(hash, push.hash);
    assert_eq!(required, push.required);
    assert_eq!(
        prompt,
        push.prompt.as_ref().map(Text::to_plain_string),
        "the prompt must survive the NBT component round trip"
    );

    // The response half of the round trip: the client reports the pack id back
    // through `ClientAction::ResourcePackResponse`, and the server's decode arm
    // accepts it without dropping the connection. The server tolerates any
    // response bytes by design (decode-then-drop, vanilla's own fallback), so
    // the meaningful assertion is negative: the connection must still be alive
    // after the reply has had a chance to be transmitted and decoded.
    handle
        .send_action(ClientAction::ResourcePackResponse {
            id,
            response: ResourcePackResponseKind::Accepted,
        })
        .expect("send the client's response to the pushed pack");

    // Give the response a real chance to cross the transport and be decoded
    // before asserting the serve task is still running (a decode error in
    // `serve_play`'s dispatch would return early and finish the task).
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !server.is_finished(),
        "the server must not drop the connection after the client's resource pack response"
    );

    handle.shutdown();
    server.abort();
}
