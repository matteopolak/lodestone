//! Issue #438: **a player is an entity that another connection receives.**
//!
//! # Why this test is shaped the way it is
//!
//! The dominant defect class in this repo is the island — a subsystem that is
//! individually built, individually green, and reaches zero pixels because
//! nothing consumes it. `lodestone-server`'s own unit tests for the player
//! registry (`crates/lodestone-server/src/players.rs`) are a **closed loop**:
//! they prove the registry excludes a viewer from its own view, and they would
//! pass in their entirety with no player ever reaching a socket. So does any
//! assertion about `World` contents, a snapshot count, or `MobSim::len()`.
//!
//! Every assertion here is therefore made against **bytes a second connection
//! actually received**, decoded from the wire, over two real
//! [`Connection`]s driven through a real handshake against the real
//! [`V770ServerProtocol`] and the real `serve_connection`.
//!
//! # Where the expected values come from
//!
//! [`PLAYER_ENTITY_TYPE_ID`] is `156`, read out of **Mojang's own
//! `registries.json`** for 26.2 (`minecraft:entity_type` → `minecraft:player`
//! → `protocol_id`), not out of our generated table. That distinction is the
//! whole point: `encode_add_entity_body` resolves the type with
//! `entity_type_id(&entity.entity_type.to_string()).unwrap_or(0)`, so a
//! misspelled or wrong key streams entity type **`0`** — `minecraft:acacia_boat`
//! — with no error logged anywhere, and an assertion that merely said "an
//! `ADD_ENTITY` arrived" would pass on a fleet of boats. Asserting the type id
//! against a number sourced outside our code is what closes that.
//!
//! The player-info ordering requirement comes from the jar too, and it is not a
//! nicety: `ClientPacketListener.createEntityFromPacket`
//! (`.cache/mc/26.2/client-src/net/minecraft/client/multiplayer/ClientPacketListener.java:591-604`)
//! returns `null` for a `PLAYER`-typed spawn whose uuid has no `PlayerInfo`,
//! logging *"Server attempted to add player prior to sending player info"* —
//! the entity is never added to the level. A server that streamed a
//! byte-perfect `ADD_ENTITY` and no `player_info_update` would satisfy a naive
//! version of this test and still draw nothing on a real client. Hence
//! [`player_info_precedes_the_spawn`]'s index comparison.
//!
//! # The negative controls, and what each one caught
//!
//! Four, each **run and observed** to fail before being reverted, with the
//! assertion and message recorded as they actually appeared — a described
//! control is not a control, and the first control below did not fail where it
//! was predicted to:
//!
//! | control | what was broken | observed failure |
//! |---|---|---|
//! | doppelgänger | `registry.view(ticket…)` → `registry.view(None)` in `stream_pass` | *"A joined an empty world and must receive no entity spawns at all, got [AddEntity { id: 1073741824, uuid: 43800000-…-0001, type_id: 156, … }]"*. Note this is **not** the self-exclusion assertion further down — the lone-player case catches it first, and does so while naming the offending uuid. |
//! | spawn broadcast | `snapshots.extend(view.entities)` deleted | *"B must receive exactly one spawn — A's player — got []"* |
//! | ordering | player-list directives emitted *after* the entity diff | *"player_info_update for A arrived at index 13, after A's ADD_ENTITY at index 12"* |
//! | wrong type key | `PLAYER_ENTITY_TYPE` → `"minecraft:playr"` | *"B received entity type 0 where `minecraft:player` is 156"* — the `unwrap_or(0)` trap, firing exactly as designed. An assertion that merely checked "an entity arrived" passed under this control. |
//!
//! # `unique_username`, and why it matters more than usual here
//!
//! Offline mode derives an account uuid from the username, so two connections
//! sharing a name are the *same player*. Here that would not merely share a
//! persisted file — it would make the two subjects indistinguishable in the
//! registry and turn the self-exclusion assertion vacuous, since "the other
//! player" and "myself" would be the same entry. Both names come from
//! `lodestone_testsupport::unique_username`, and the test asserts they differ.

use std::time::Duration;

use lodestone_core::{Decode, Reader, Writer};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, PlayerAwareSource,
    PlayerRegistry, serve_connection,
};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use lodestone_v770::packets::player_info::{PlayerInfoRemove, PlayerInfoUpdate};
use uuid::Uuid;

mod common;
use common::unique_username;

/// `minecraft:player`'s network entity-type id in protocol 776.
///
/// **Sourced outside this tree**: Mojang's generated
/// `registries.json` for 26.2, `minecraft:entity_type` → `minecraft:player` →
/// `"protocol_id": 156`. Deliberately *not* obtained by calling
/// `lodestone_data::entity_types::entity_type_id("minecraft:player")`, which
/// would be comparing our own table against itself — the
/// `decode(encode(x)) == x` shape this repo's evidence standard rejects.
const PLAYER_ENTITY_TYPE_ID: i32 = 156;

/// A never-sampled terrain source; the same shape `server_status.rs` and
/// `server_disconnect.rs` use. This test's subject is the entity path.
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
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// The `add_entity` fields this test reads back off the wire.
#[derive(Debug, Clone, PartialEq)]
struct AddEntity {
    id: i32,
    uuid: Uuid,
    type_id: i32,
    x: f64,
    y: f64,
    z: f64,
}

/// Decodes the leading fields of an `add_entity` body.
///
/// Layout per `encode_add_entity_body`: VarInt id, uuid, VarInt type, `f64`×3
/// position, then the length-prefixed velocity and the three packed angles this
/// test does not read.
fn decode_add_entity(payload: &[u8]) -> AddEntity {
    let mut r = Reader::new(payload);
    AddEntity {
        id: r.var_i32().expect("add_entity id"),
        uuid: r.uuid().expect("add_entity uuid"),
        type_id: r.var_i32().expect("add_entity type id"),
        x: r.f64().expect("add_entity x"),
        y: r.f64().expect("add_entity y"),
        z: r.f64().expect("add_entity z"),
    }
}

/// Decodes a `teleport_entity` body far enough to read the id and position.
///
/// Layout per `encode_teleport_entity`: VarInt id, `f64`×3 position, `f64`×3
/// delta movement, `f32` yaw, `f32` pitch, `i32` relative flags, `bool`
/// on-ground.
fn decode_teleport_entity(payload: &[u8]) -> (i32, f64, f64, f64) {
    let mut r = Reader::new(payload);
    (
        r.var_i32().expect("teleport id"),
        r.f64().expect("teleport x"),
        r.f64().expect("teleport y"),
        r.f64().expect("teleport z"),
    )
}

/// Decodes a `remove_entities` body: a VarInt-prefixed list of VarInt ids.
fn decode_remove_entities(payload: &[u8]) -> Vec<i32> {
    let mut r = Reader::new(payload);
    let count = r.var_i32().expect("remove_entities count");
    (0..count)
        .map(|_| r.var_i32().expect("remove_entities id"))
        .collect()
}

/// Decodes a `player_info_update` body through the **decoder that already
/// existed** (`crate::packets::player_info`, written for the client half long
/// before this issue and gated by `tests/player_list.rs`).
///
/// That independence is the point: it is not this encoder's mirror image
/// written by the same hand in the same hour, so agreement between them is
/// evidence about the wire layout rather than about one author's consistent
/// misunderstanding of it.
fn decode_player_info(payload: &[u8]) -> PlayerInfoUpdate {
    let mut r = Reader::new(payload);
    let decoded =
        PlayerInfoUpdate::decode(&mut r, lodestone_core::Ctx { version: 776 }).expect(
            "our own player_info_update must parse under the pre-existing client-side decoder",
        );
    assert!(
        r.remaining() == 0,
        "player_info_update left {} trailing bytes — the action mask and the entry body disagree",
        r.remaining()
    );
    decoded
}

fn decode_player_info_remove(payload: &[u8]) -> PlayerInfoRemove {
    let mut r = Reader::new(payload);
    PlayerInfoRemove::decode(&mut r, lodestone_core::Ctx { version: 776 })
        .expect("our own player_info_remove must parse under the pre-existing decoder")
}

/// Hand-written `Intention`: VarInt protocol, host, big-endian port, VarInt
/// next_state (`2` = Login). Same bytes as `server_disconnect.rs`'s own copy.
fn handshake_bytes() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(2);
    w.into_vec()
}

/// Hand-written login `hello`: a length-prefixed name then a raw 16-byte uuid.
///
/// A **distinct** uuid per player, unlike `server_disconnect.rs`'s fixed one:
/// the whole subject here is telling two players apart, and a shared uuid would
/// make the tab-list roster collapse to one entry.
fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.uuid(uuid);
    w.into_vec()
}

/// Hand-written serverbound `move_player_pos`: `f64`×3 then a flags byte with
/// the on-ground bit set.
fn move_bytes(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.u8(1); // MOVE_FLAG_ON_GROUND
    w.into_vec()
}

/// Reads packets until the server goes quiet for `QUIET`, returning everything
/// received.
///
/// The join burst is where a newcomer learns about everyone already online, so
/// the returned vector is the subject of most assertions below — and it is
/// returned in **arrival order**, which is what makes the player-info-before-
/// spawn ordering assertion possible at all.
async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(250);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

/// Drives one connection handshake → login → configuration → play and returns
/// every clientbound packet it received on the way, in order.
async fn join<T: Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client.write_packet(0, &hello_bytes(name, uuid)).await.unwrap();
    let mut seen = Vec::new();
    // login_finished
    if let Ok(Some(p)) = client.read_packet().await {
        seen.push(p);
    }
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    // finish_configuration
    if let Ok(Some(p)) = client.read_packet().await {
        seen.push(p);
    }
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    seen.extend(drain(client).await);
    seen
}

/// Sends one movement packet — which both drives a streaming pass and moves
/// this player — then drains whatever the server sends back.
async fn move_and_drain<T: Transport>(
    client: &mut Connection<T>,
    x: f64,
    y: f64,
    z: f64,
) -> Vec<(i32, Vec<u8>)> {
    client
        .write_packet(play::serverbound::MOVE_PLAYER_POS, &move_bytes(x, y, z))
        .await
        .unwrap();
    drain(client).await
}

fn adds(packets: &[(i32, Vec<u8>)]) -> Vec<AddEntity> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::ADD_ENTITY)
        .map(|(_, payload)| decode_add_entity(payload))
        .collect()
}

fn roster(packets: &[(i32, Vec<u8>)]) -> Vec<(Uuid, Option<String>)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::PLAYER_INFO_UPDATE)
        .flat_map(|(_, payload)| decode_player_info(payload).entries)
        .map(|entry| (entry.uuid, entry.name))
        .collect()
}

/// A uuid that is visibly derived from the player it belongs to, so a failure
/// message names a subject rather than a random 128-bit number.
fn uuid_for(slot: u128) -> Uuid {
    Uuid::from_u128(0x4380_0000_0000_0000_0000_0000_0000_0000 + slot)
}

/// The whole issue, in one test: two connections against one shared world, and
/// connection B receives an `ADD_ENTITY` for connection A's **player**.
///
/// Split into named sections rather than several `#[tokio::test]`s because
/// standing up two joined connections is the expensive part and every assertion
/// below is about the same pair; the section comments say which named claim each
/// block is the body of.
#[tokio::test]
async fn two_connections_see_each_other_as_player_entities() {
    let registry = PlayerRegistry::new();
    let name_a = unique_username();
    let name_b = unique_username();
    assert_ne!(
        name_a, name_b,
        "the two subjects must be different players — see this file's own docs \
         on why a shared username makes the self-exclusion assertion vacuous"
    );
    let uuid_a = uuid_for(1);
    let uuid_b = uuid_for(2);

    // One registry, two connections. This is the shape `IntegratedServer::bind`
    // uses in production: every accepted socket gets its own task and they share
    // the world through cloned handles.
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

    // A joins an empty world.
    let a_join = join(&mut client_a, &name_a, uuid_a).await;
    assert!(
        adds(&a_join).is_empty(),
        "A joined an empty world and must receive no entity spawns at all, got {:?}",
        adds(&a_join)
    );
    assert_eq!(
        registry.len(),
        1,
        "A's join must have registered exactly one player"
    );

    // B joins, and A is already online.
    let b_join = join(&mut client_b, &name_b, uuid_b).await;
    assert_eq!(registry.len(), 2, "both players must now be registered");

    // ------------------------------------------------------------------
    // `another_players_entity_reaches_this_connection`
    // ------------------------------------------------------------------
    // The assertion this whole file exists for. Not "an entity arrived": the
    // *type id*, from Mojang's own registry dump, because `unwrap_or(0)` makes
    // a wrong key silently stream a boat.
    let b_adds = adds(&b_join);
    assert_eq!(
        b_adds.len(),
        1,
        "B must receive exactly one spawn — A's player — got {b_adds:?}"
    );
    let a_entity = &b_adds[0];
    assert_eq!(
        a_entity.type_id, PLAYER_ENTITY_TYPE_ID,
        "B received entity type {} where `minecraft:player` is {PLAYER_ENTITY_TYPE_ID} \
         (Mojang registries.json for 26.2). Type 0 means the entity-type key did not \
         resolve and `entity_type_id(...).unwrap_or(0)` substituted `minecraft:acacia_boat`.",
        a_entity.type_id
    );
    // Guards the assertion above against the one value that would make it
    // accidentally satisfiable by the failure mode it exists to catch.
    assert_ne!(PLAYER_ENTITY_TYPE_ID, 0);
    assert_eq!(
        a_entity.uuid, uuid_a,
        "the spawned entity must carry A's profile uuid — the key B's client \
         resolves the spawn against in its own PlayerInfo map"
    );
    // Spawn position, so a player entity that arrived at the origin (an
    // uninitialised `Vec3::default()`) rather than at the join spawn point
    // cannot pass.
    //
    // This expected `y = 100.0` and had to change. `100.0` is
    // `ServerProtocol::begin_play`'s *default* spawn
    // (`server_protocol.rs`'s `begin_play_at(view_radius, Vec3::new(8.0, 100.0,
    // 8.0))`), but `serve_connection` does not use that default: since issues #461
    // and #329 it calls `begin_play_at` with the result of
    // `world_spawn::find_initial_spawn`, a real search over the source. Nothing in
    // the adapter changed — the root cause is entirely on the server side, and the
    // constant here was a stale copy of a default this path stopped taking.
    //
    // For [`AirSource`] that search finds no solid block in any of its 121 spiral
    // candidates, so it returns its documented full-invalid-box fallback of
    // `(8, getSpawnHeight, 8)`.
    //
    // **This expected `min_y + 1` (`-63`) and had to change a second time.** That
    // was `find_initial_spawn`'s fallback until it was measured against the real
    // generator: on two of four probe seeds the whole ±5 box is ocean, the fallback
    // fires, and `-63` is *inside the bedrock floor* — the player is buried in the
    // dark, which reads as a server hang. `world_spawn::GENERATOR_SPAWN_HEIGHT` is
    // now vanilla's own `ChunkGenerator.getSpawnHeight`
    // (`.cache/mc/26.2/src/net/minecraft/world/level/chunk/ChunkGenerator.java:432`,
    // a literal `64` that `NoiseBasedChunkGenerator` does not override), which is
    // what `MinecraftServer.setInitialSpawn` pre-seeds the world spawn with. See
    // DESIGN.md §12.125.
    //
    // Derived from the jar plus `find_initial_spawn`'s contract, not read off the
    // failure. X and Z stay `8.0`, which is what keeps this assertion able to do
    // its job: all three components remain non-zero, so an uninitialised
    // `Vec3::default()` still fails on every axis.
    const GENERATOR_SPAWN_HEIGHT: f64 = 64.0;
    assert_eq!(
        (a_entity.x, a_entity.y, a_entity.z),
        (8.0, GENERATOR_SPAWN_HEIGHT, 8.0),
        "A's entity must stand at the join spawn position `begin_play_at` teleported A to — \
         `find_initial_spawn`'s fallback for a source with no solid block anywhere"
    );
    assert_ne!(
        (a_entity.x, a_entity.y, a_entity.z),
        (0.0, 0.0, 0.0),
        "and it must not be an uninitialised Vec3::default()"
    );

    // ------------------------------------------------------------------
    // `a_connection_never_receives_its_own_player_entity`
    // ------------------------------------------------------------------
    // Vanilla never sends a player their own entity; doing so puts a
    // doppelgänger inside the camera. Asserted on both connections, and by uuid
    // rather than by count, so it cannot be satisfied by simply sending
    // nothing.
    assert!(
        !b_adds.iter().any(|e| e.uuid == uuid_b),
        "B received its own player entity ({uuid_b}) — a doppelgänger"
    );
    let a_after_b_joined = move_and_drain(&mut client_a, 8.0, 100.0, 8.0).await;
    let a_adds = adds(&a_after_b_joined);
    assert!(
        !a_adds.iter().any(|e| e.uuid == uuid_a),
        "A received its own player entity ({uuid_a}) — a doppelgänger"
    );

    // ------------------------------------------------------------------
    // A learns about B too — the reverse direction, which the pull-diff gets
    // for free but which nothing had proven.
    // ------------------------------------------------------------------
    assert_eq!(
        a_adds.len(),
        1,
        "A's next streaming pass must spawn B's player, got {a_adds:?}"
    );
    assert_eq!(a_adds[0].uuid, uuid_b);
    assert_eq!(a_adds[0].type_id, PLAYER_ENTITY_TYPE_ID);
    let b_entity_id = a_adds[0].id;
    assert_ne!(
        b_entity_id, a_entity.id,
        "the two players must have distinct network entity ids"
    );

    // ------------------------------------------------------------------
    // `player_info_precedes_the_spawn`
    // ------------------------------------------------------------------
    // The ordering the jar requires. Index comparison inside B's own arrival
    // sequence, because "both packets were sent" is exactly the state in which
    // a real client still draws nothing.
    let first_info = b_join
        .iter()
        .position(|(id, payload)| {
            *id == play::clientbound::PLAYER_INFO_UPDATE
                && decode_player_info(payload)
                    .entries
                    .iter()
                    .any(|e| e.uuid == uuid_a)
        })
        .expect(
            "B must receive a player_info_update carrying A's uuid — without it a real \
             client discards A's ADD_ENTITY entirely (ClientPacketListener.java:591-604)",
        );
    let first_spawn = b_join
        .iter()
        .position(|(id, _)| *id == play::clientbound::ADD_ENTITY)
        .expect("B must receive A's spawn");
    assert!(
        first_info < first_spawn,
        "player_info_update for A arrived at index {first_info}, after A's ADD_ENTITY at \
         index {first_spawn}. A real client drops a PLAYER spawn it has no PlayerInfo for."
    );

    // The roster also carries the *username*, which is what the tab list draws.
    let b_roster = roster(&b_join);
    let a_listing = b_roster
        .iter()
        .find(|(uuid, _)| *uuid == uuid_a)
        .expect("A must appear in B's roster");
    assert_eq!(
        a_listing.1.as_deref(),
        Some(name_a.as_str()),
        "A's tab-list entry must carry A's username"
    );
    // Vanilla lists you in your own tab list, unlike entities — so B's own
    // entry is present even though B's own entity is not.
    assert!(
        b_roster.iter().any(|(uuid, _)| *uuid == uuid_b),
        "B must appear in B's own roster: the roster and the entity list have \
         deliberately opposite self-inclusion rules"
    );

    // ------------------------------------------------------------------
    // Movement thereafter
    // ------------------------------------------------------------------
    // B walks; A's next pass must carry it. The value is a position A could not
    // have guessed from the spawn point.
    let _ = move_and_drain(&mut client_b, 12.5, 100.0, 3.5).await;
    let a_after_b_moved = move_and_drain(&mut client_a, 8.0, 100.0, 8.0).await;
    let teleport = a_after_b_moved
        .iter()
        .filter(|(id, _)| *id == play::clientbound::TELEPORT_ENTITY)
        .map(|(_, payload)| decode_teleport_entity(payload))
        .find(|(id, _, _, _)| *id == b_entity_id)
        .expect("A must receive a position update for B's entity after B moved");
    assert_eq!(
        (teleport.1, teleport.2, teleport.3),
        (12.5, 100.0, 3.5),
        "A must receive the position B actually reported"
    );

    // ------------------------------------------------------------------
    // Leaving
    // ------------------------------------------------------------------
    // Dropping B's client closes the socket, so B's serving task returns and
    // its `PlayerTicket` drops. A's next pass must remove the entity *and* the
    // tab-list entry — a `REMOVE_ENTITIES` alone would leave B's name in A's
    // tab list forever.
    drop(client_b);
    let _ = task_b.await;
    assert_eq!(
        registry.len(),
        1,
        "B's ticket must have deregistered B when its connection task ended"
    );

    let a_after_b_left = move_and_drain(&mut client_a, 8.0, 100.0, 8.0).await;
    let removed: Vec<i32> = a_after_b_left
        .iter()
        .filter(|(id, _)| *id == play::clientbound::REMOVE_ENTITIES)
        .flat_map(|(_, payload)| decode_remove_entities(payload))
        .collect();
    assert!(
        removed.contains(&b_entity_id),
        "A must receive a REMOVE_ENTITIES for B's entity id {b_entity_id}, got {removed:?}"
    );
    let dropped: Vec<Uuid> = a_after_b_left
        .iter()
        .filter(|(id, _)| *id == play::clientbound::PLAYER_INFO_REMOVE)
        .flat_map(|(_, payload)| decode_player_info_remove(payload).uuids)
        .collect();
    assert!(
        dropped.contains(&uuid_b),
        "A must receive a player_info_remove for B ({uuid_b}), got {dropped:?}"
    );

    drop(client_a);
    let _ = task_a.await;
}

/// A control for the *premise* of the test above, not for the feature: it
/// proves the assertions there can distinguish "player streaming is on" from
/// "player streaming is off", by running the identical join against a source
/// that reports no registry.
///
/// This is the arm that would have failed for the entire life of this repo
/// before #438, and it is what stops the main test from being satisfiable by
/// some unrelated entity happening to be in range — the *world* species of
/// vacuous test that issue #438's own body warns about.
#[tokio::test]
async fn without_a_player_registry_no_player_entity_is_streamed_at_all() {
    let name_a = unique_username();
    let name_b = unique_username();

    let (client_a_io, server_a_io) = memory_pair();
    let (client_b_io, server_b_io) = memory_pair();

    // `NoEntities` — not `PlayerAwareSource`. Its `EntitySource::players()`
    // default returns `None`, which is precisely the pre-#438 world.
    let task_a = tokio::spawn(async move {
        let mut conn = Connection::new(server_a_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &NoEntities,
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
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
        .map(|_| ())
    });

    let mut client_a = Connection::new(client_a_io);
    let mut client_b = Connection::new(client_b_io);
    let _ = join(&mut client_a, &name_a, uuid_for(1)).await;
    let b_join = join(&mut client_b, &name_b, uuid_for(2)).await;

    assert!(
        adds(&b_join).is_empty(),
        "with no registry there is no player entity to stream, got {:?}",
        adds(&b_join)
    );
    assert!(
        roster(&b_join).is_empty(),
        "with no registry there is no tab-list entry either, got {:?}",
        roster(&b_join)
    );

    drop(client_a);
    drop(client_b);
    let _ = task_a.await;
    let _ = task_b.await;
}
