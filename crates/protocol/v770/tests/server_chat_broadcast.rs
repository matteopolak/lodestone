//! End-to-end gate: **a message typed by connection A reaches
//! connection B's wire, carrying both the text and the sender.**
//!
//! # What makes this non-vacuous
//!
//! Three deliberate choices, each closing a way this test could have passed
//! against the bug:
//!
//! * **Two real connections against one shared world**, the shape
//!   `IntegratedServer::bind` uses in production — not one connection
//!   observing its own echo, which would pass even if the broadcast reached
//!   nobody else.
//! * **The serverbound frame is hand-built from the 26.2 wire layout**
//!   (`ServerboundChatPacket`'s own constructor: `readUtf(256)`,
//!   `readInstant()`, `readLong()` salt, `readNullable(MessageSignature::read)`,
//!   then `LastSeenMessages.Update`'s VarInt offset + fixed 20-bit set +
//!   checksum byte). It is **not** produced by our own `ChatMessage` encoder,
//!   so this is an external anchor rather than `decode(encode(x)) == x`.
//! * **B's reply is decoded by the pre-existing client-side decoder**
//!   (`V770Adapter`'s `SYSTEM_CHAT` arm, written for the client half long
//!   before this issue and gated by `tests/chat_dispatch.rs`) rather than by a
//!   hand-rolled NBT reader written in this same file. Agreement between two
//!   independently-authored halves is evidence about the wire; agreement
//!   between an encoder and its mirror image is not. This is the same
//!   argument `server_player_entity_stream.rs`'s `decode_player_info` makes.
//!
//! Asserting merely that *a* chat packet arrived would be the "connected wire,
//! wrong value" failure `cargo xtask connectedness` structurally cannot see —
//! the same class as `MOVE_PLAYER_POS_ROT` discarding yaw/pitch and block
//! placement writing `STONE`. So every assertion here names the exact string.
//!
//! # The sender receives their own message
//!
//! Checked against the jar rather than assumed:
//! `PlayerList.broadcastChatMessage` (`PlayerList.java`) loops
//! `for (ServerPlayer player : this.players)` with no sender exclusion, and a
//! vanilla client does not echo its own chat locally — it waits for the
//! server. So A must see A's own message too, and this test asserts it.

use std::time::Duration;

use lodestone_core::Writer;
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, PlayerAwareSource,
    PlayerRegistry, serve_connection,
};
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::{V770Adapter, V770ServerProtocol};
use lodestone_world::World;
use uuid::Uuid;

mod common;
use common::unique_username;

/// A never-sampled terrain source; this test's subject is the chat path.
struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is all air and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). `ChunkSource::set_block` has no default, so this is
    // stated explicitly rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// Hand-written `Intention`: VarInt protocol, host, big-endian port, VarInt
/// next_state (`2` = Login). Same bytes as `server_player_entity_stream.rs`.
fn handshake_bytes() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(2);
    w.into_vec()
}

/// Hand-written login `hello`: a length-prefixed name then a raw 16-byte uuid.
fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.uuid(uuid);
    w.into_vec()
}

/// Hand-written serverbound `chat`, straight from 26.2's
/// `ServerboundChatPacket` constructor — **the external anchor**.
///
/// ```text
/// readUtf(256)                          message
/// readInstant()                         i64 epoch millis
/// readLong()                            i64 salt
/// readNullable(MessageSignature::read)  bool present, then 256 bytes if so
/// LastSeenMessages.Update(input):
///   readVarInt()                        offset
///   readFixedBitSet(20)                 3 bytes, no length prefix
///   readByte()                          checksum (0 = ignore)
/// ```
///
/// Written with a bare `Writer` rather than through our own `ChatMessage`
/// `Encode` impl on purpose: if the struct's field order were wrong, encoding
/// with it and decoding with it would agree perfectly and this test would
/// still pass. These bytes come from the packet definition instead.
fn chat_bytes(message: &str) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(message);
    w.i64(0); // timestamp
    w.i64(0); // salt
    w.bool(false); // no signature
    w.var_i32(0); // last-seen offset
    w.u8(0); // acknowledged bit set, bytes 1..3
    w.u8(0);
    w.u8(0);
    w.i8(0); // checksum: 0 means "ignore"
    w.into_vec()
}

/// Reads packets until the server goes quiet for `QUIET`.
async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(250);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

/// Drives one connection handshake → login → configuration → play.
async fn join<T: Transport>(client: &mut Connection<T>, name: &str, uuid: Uuid) -> Vec<(i32, Vec<u8>)> {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client.write_packet(0, &hello_bytes(name, uuid)).await.unwrap();
    let mut seen = Vec::new();
    if let Ok(Some(p)) = common::read_login_packet(client).await {
        seen.push(p);
    }
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    if let Ok(Some(p)) = common::read_login_packet(client).await {
        seen.push(p);
    }
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    seen.extend(drain(client).await);
    seen
}

/// Every `system_chat` in `packets`, rendered to plain text **through the
/// pre-existing client decoder** rather than a hand-rolled NBT reader here.
fn system_chat_lines(packets: &[(i32, Vec<u8>)]) -> Vec<String> {
    let adapter = V770Adapter::new();
    let mut lines = Vec::new();
    for (id, payload) in packets {
        if *id != play::clientbound::SYSTEM_CHAT {
            continue;
        }
        let directives = adapter
            .handle_packet(
                &mut World::new(),
                ConnectionState::Play,
                play::clientbound::SYSTEM_CHAT,
                payload,
            )
            .expect("our own system_chat must parse under the pre-existing client decoder");
        for directive in directives {
            if let Directive::Emit(ClientEvent::Chat { text, .. }) = directive {
                lines.push(text.to_plain_string());
            }
        }
    }
    lines
}

#[tokio::test]
async fn a_message_typed_by_one_player_reaches_another_players_wire() {
    let registry = PlayerRegistry::new();
    let name_a = unique_username();
    let name_b = unique_username();
    assert_ne!(
        name_a, name_b,
        "the two subjects must be different players, or 'B received A's message' \
         is indistinguishable from 'A received its own'"
    );
    let uuid_a = Uuid::from_u128(0x4690_0000_0000_0000_0000_0000_0000_0001);
    let uuid_b = Uuid::from_u128(0x4690_0000_0000_0000_0000_0000_0000_0002);

    let source_a = PlayerAwareSource::new(NoEntities, registry.clone());
    let source_b = PlayerAwareSource::new(NoEntities, registry.clone());

    let (client_a_io, server_a_io) = memory_pair();
    let (client_b_io, server_b_io) = memory_pair();

    let task_a = tokio::spawn(async move {
        let mut conn = Connection::new(server_a_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &source_a,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });
    let task_b = tokio::spawn(async move {
        let mut conn = Connection::new(server_b_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &source_b,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });

    let mut client_a = Connection::new(client_a_io);
    let mut client_b = Connection::new(client_b_io);

    join(&mut client_a, &name_a, uuid_a).await;
    let b_join = join(&mut client_b, &name_b, uuid_b).await;
    assert_eq!(registry.len(), 2, "both players must be registered");

    // **Precondition, asserted rather than assumed.** B's join burst carries a
    // welcome `system_chat` of its own, so "B received a system_chat" is
    // already true before anyone says anything. Without this the assertions
    // below could be satisfied by the join banner.
    let joined_lines = system_chat_lines(&b_join);
    assert!(
        !joined_lines.iter().any(|line| line.contains("hello from A")),
        "B must not have seen the message before it was sent: {joined_lines:?}"
    );

    // A types a message.
    const MESSAGE: &str = "hello from A";
    client_a
        .write_packet(play::serverbound::CHAT, &chat_bytes(MESSAGE))
        .await
        .unwrap();

    let b_after = drain(&mut client_b).await;
    let a_after = drain(&mut client_a).await;

    let b_lines = system_chat_lines(&b_after);
    let a_lines = system_chat_lines(&a_after);

    // The whole issue, in one assertion: B's wire carries the text **and** the
    // sender, in vanilla's own `chat.type.text` (`"<%s> %s"`) rendering.
    let expected = format!("<{name_a}> {MESSAGE}");
    assert!(
        b_lines.contains(&expected),
        "B's wire must carry {expected:?}; got {b_lines:?}"
    );

    // The sender is not excluded — see this file's own header for the jar
    // reference. A must see its own message.
    assert!(
        a_lines.contains(&expected),
        "the sender must receive their own message too (vanilla broadcasts to \
         every player with no sender exclusion); got {a_lines:?}"
    );

    // Exactly once each: a broadcast that delivered twice would be a visible
    // duplicate in the chat history, and `contains` alone cannot see that.
    assert_eq!(
        b_lines.iter().filter(|line| *line == &expected).count(),
        1,
        "B must receive the message exactly once, got {b_lines:?}"
    );
    assert_eq!(
        a_lines.iter().filter(|line| *line == &expected).count(),
        1,
        "A must receive its own message exactly once, got {a_lines:?}"
    );

    drop(client_a);
    drop(client_b);
    let _ = tokio::time::timeout(Duration::from_secs(10), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), task_b).await;
}

/// A second player's message must reach the first — the same path in the other
/// direction, which is what proves the broadcast is not wired one-way from
/// whichever connection happened to join first.
#[tokio::test]
async fn the_broadcast_works_in_both_directions() {
    let registry = PlayerRegistry::new();
    let name_a = unique_username();
    let name_b = unique_username();
    let source_a = PlayerAwareSource::new(NoEntities, registry.clone());
    let source_b = PlayerAwareSource::new(NoEntities, registry.clone());

    let (client_a_io, server_a_io) = memory_pair();
    let (client_b_io, server_b_io) = memory_pair();

    let task_a = tokio::spawn(async move {
        let mut conn = Connection::new(server_a_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &source_a,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });
    let task_b = tokio::spawn(async move {
        let mut conn = Connection::new(server_b_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &source_b,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });

    let mut client_a = Connection::new(client_a_io);
    let mut client_b = Connection::new(client_b_io);
    join(&mut client_a, &name_a, Uuid::from_u128(0x11)).await;
    join(&mut client_b, &name_b, Uuid::from_u128(0x22)).await;

    // B speaks this time.
    const MESSAGE: &str = "reply from B";
    client_b
        .write_packet(play::serverbound::CHAT, &chat_bytes(MESSAGE))
        .await
        .unwrap();

    let a_after = drain(&mut client_a).await;
    let a_lines = system_chat_lines(&a_after);
    let expected = format!("<{name_b}> {MESSAGE}");
    assert!(
        a_lines.contains(&expected),
        "A's wire must carry B's message {expected:?}; got {a_lines:?}"
    );

    drop(client_a);
    drop(client_b);
    let _ = tokio::time::timeout(Duration::from_secs(10), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), task_b).await;
}

/// The enforcement half of server-side chat signing, through the **real
/// production dispatch path** — not `lodestone_server::chat_session::decide` called directly
/// (covered by that crate's own hermetic unit tests), but a real `chat`
/// packet through `serve_connection`/`dispatch_play_packet` with
/// `PlayerRegistry::enforce_secure_profile()` actually consulted.
///
/// Two things this proves that a hermetic `decide()` test cannot: the flag
/// set on the registry actually reaches the connection loop, and a rejected
/// message is reported back to the **sender alone** — B must see nothing at
/// all, not even a degraded/unverified version of it.
#[tokio::test]
async fn enforcement_rejects_an_unsigned_message_and_replies_only_to_the_sender() {
    let registry = PlayerRegistry::new();
    registry.set_enforce_secure_profile(true);
    let name_a = unique_username();
    let name_b = unique_username();
    let source_a = PlayerAwareSource::new(NoEntities, registry.clone());
    let source_b = PlayerAwareSource::new(NoEntities, registry.clone());

    let (client_a_io, server_a_io) = memory_pair();
    let (client_b_io, server_b_io) = memory_pair();

    let task_a = tokio::spawn(async move {
        let mut conn = Connection::new(server_a_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &source_a,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });
    let task_b = tokio::spawn(async move {
        let mut conn = Connection::new(server_b_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &source_b,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });

    let mut client_a = Connection::new(client_a_io);
    let mut client_b = Connection::new(client_b_io);
    join(&mut client_a, &name_a, Uuid::from_u128(0x33)).await;
    join(&mut client_b, &name_b, Uuid::from_u128(0x44)).await;

    // A never announced a chat session and this server now requires one.
    const MESSAGE: &str = "unsigned and should be rejected";
    client_a
        .write_packet(play::serverbound::CHAT, &chat_bytes(MESSAGE))
        .await
        .unwrap();

    let a_after = drain(&mut client_a).await;
    let b_after = drain(&mut client_b).await;

    let a_lines = system_chat_lines(&a_after);
    let b_lines = system_chat_lines(&b_after);

    let broadcast_form = format!("<{name_a}> {MESSAGE}");
    assert!(
        !b_lines.contains(&broadcast_form),
        "B must never see a rejected message: {b_lines:?}"
    );
    assert!(
        !a_lines.contains(&broadcast_form),
        "the rejected message must not come back to A as a broadcast either: {a_lines:?}"
    );
    assert!(
        a_lines.iter().any(|line| line.contains("not sent")),
        "the sender alone must be told the message was rejected: {a_lines:?}"
    );

    drop(client_a);
    drop(client_b);
    let _ = tokio::time::timeout(Duration::from_secs(10), task_a).await;
    let _ = tokio::time::timeout(Duration::from_secs(10), task_b).await;
}
