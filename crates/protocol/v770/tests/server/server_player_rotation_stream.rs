//! **A player's facing reaches another connection.**
//!
//! # What was broken
//!
//! Four serverbound movement packets exist, and vanilla's
//! `vanilla's own local player's own send position` sends exactly **one** of them per tick, chosen
//! by which of position/look changed:
//!
//! | packet | position | look |
//! |---|---|---|
//! | `move_player_pos` | yes | — |
//! | `move_player_pos_rot` | yes | yes |
//! | `move_player_rot` | — | yes |
//! | `move_player_status_only` | — | — |
//!
//! They *partition* the movement stream. Before this wiring, `v770`'s decoder
//! read `move_player_pos_rot`'s yaw and pitch and **threw them away**, and
//! mapped the two position-less siblings to `ServerBound::Ignored` outright.
//! `PlayerRegistry::view` therefore hard-coded `yaw: 0.0, pitch: 0.0,
//! head_yaw: 0.0` with a comment saying no server-side rotation existed to
//! lower — an island in the "declared, unconsumed" sense, and one whose
//! user-visible symptom is that every other player stands rigidly facing due
//! south no matter where they are looking.
//!
//! # Where the expected values come from
//!
//! **Not from our own encoder.** The two packed-angle bytes are computed by
//! hand from vanilla's own formula, `vanilla's own mth's own pack degrees` —
//! `degrees * 256 / 360`, truncated into a signed byte — and the inputs were
//! chosen so that formula lands on an *exact integer*, making the expectation
//! independent of whether an implementation floors or rounds:
//!
//! | degrees | `deg * 256 / 360` | wire byte (`i8`) |
//! |---|---|---|
//! | `90.0` | `64.0` | `64` |
//! | `-45.0` | `-32.0` | `-32` |
//! | `180.0` | `128.0` | `-128` (wraps, as vanilla's cast does) |
//! | `0.0` | `0.0` | `0` |
//!
//! [`PLAYER_ENTITY_TYPE_ID`] is `156` from Mojang's generated
//! `registries.json` for 26.2, for the reason
//! `server_player_entity_stream.rs` spells out at length: the spawn encoder
//! does `entity_type_id(name).unwrap_or(0)`, and index `0` is
//! `minecraft:acacia_boat`, so a test that merely asserted "an entity
//! arrived" passes against a misspelled key. This file asserts the type id
//! for the same reason even though rotation is its subject — otherwise the
//! rotation assertions would be describing a boat.
//!
//! # Why both phases, and why the walking one is not redundant
//!
//! Phase 1 turns **while moving** (`move_player_pos_rot`) and phase 2 turns
//! **on the spot** (`move_player_rot`). Testing only the second would leave
//! the far more common case unguarded, and — this is the part that is easy to
//! get backwards — a walking player never sends `move_player_rot` at all, so
//! a stationary-only test passes against an implementation in which every
//! moving player's avatar is frozen.
//!
//! # Negative controls, run and observed
//!
//! Recorded in this file's companion entry in the task report; each was
//! applied to the tree, watched failing with the message quoted, and
//! restored from an md5-verified backup.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, PlayerAwareSource,
    PlayerRegistry, serve_connection,
};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use uuid::Uuid;

#[path = "../common/mod.rs"]
mod common;
use common::unique_username;

/// `minecraft:player`'s network entity-type id in protocol 776, from Mojang's
/// generated `registries.json` for 26.2 — see this file's module docs.
const PLAYER_ENTITY_TYPE_ID: i32 = 156;

/// A never-sampled terrain source; this test's subject is the entity path.
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

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this
        // fixture is all air and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). `ChunkSource::set_block` has no default, so this is
    // stated explicitly rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// Hand-written serverbound `move_player_pos_rot`: `f64`×3, `f32` yaw, `f32`
/// pitch, then the flags byte — the layout of
/// `vanilla's own serverbound move player packet's own pos rot` in `.cache/mc/26.2/src`, written here
/// rather than obtained from `crate::adapter` so the decode side is not being
/// compared against its own mirror image.
fn pos_rot_bytes(x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.f32(yaw);
    w.f32(pitch);
    w.u8(1); // MOVE_FLAG_ON_GROUND
    w.into_vec()
}

/// Hand-written serverbound `move_player_rot`: `f32` yaw, `f32` pitch, flags.
/// **No position fields at all** — that is the whole point of the packet.
fn rot_bytes(yaw: f32, pitch: f32) -> Vec<u8> {
    let mut w = Writer::default();
    w.f32(yaw);
    w.f32(pitch);
    w.u8(1); // MOVE_FLAG_ON_GROUND
    w.into_vec()
}

/// Hand-written serverbound `move_player_pos`: `f64`×3 then the flags byte.
fn pos_bytes(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.u8(1); // MOVE_FLAG_ON_GROUND
    w.into_vec()
}

/// The `teleport_entity` fields this test reads back.
///
/// Layout per `encode_teleport_entity`: VarInt id, `f64`×3 position, `f64`×3
/// delta movement, `f32` yaw, `f32` pitch, `i32` relative flags, `bool`
/// on-ground.
#[derive(Debug, Clone, PartialEq)]
struct TeleportEntity {
    id: i32,
    yaw: f32,
    pitch: f32,
}

fn decode_teleport_entity(payload: &[u8]) -> TeleportEntity {
    let mut r = Reader::new(payload);
    let id = r.var_i32().expect("teleport id");
    for _ in 0..6 {
        r.f64().expect("teleport position/delta");
    }
    TeleportEntity {
        id,
        yaw: r.f32().expect("teleport yaw"),
        pitch: r.f32().expect("teleport pitch"),
    }
}

/// Decodes a `rotate_head` body: VarInt entity id then one signed packed byte.
fn decode_rotate_head(payload: &[u8]) -> (i32, i8) {
    let mut r = Reader::new(payload);
    (
        r.var_i32().expect("rotate_head id"),
        r.i8().expect("rotate_head packed yaw"),
    )
}

/// The leading `add_entity` fields, far enough to reach the packed angles.
///
/// Layout per `encode_add_entity_body`: VarInt id, uuid, VarInt type, `f64`×3
/// position, then packed pitch, packed yaw and packed head-yaw.
#[derive(Debug, Clone, PartialEq)]
struct AddEntity {
    id: i32,
    uuid: Uuid,
    type_id: i32,
}

fn decode_add_entity(payload: &[u8]) -> AddEntity {
    let mut r = Reader::new(payload);
    AddEntity {
        id: r.var_i32().expect("add_entity id"),
        uuid: r.uuid().expect("add_entity uuid"),
        type_id: r.var_i32().expect("add_entity type id"),
    }
}

fn handshake_bytes() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(776);
    w.string("localhost");
    w.u16(25565);
    w.var_i32(2);
    w.into_vec()
}

fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut w = Writer::default();
    w.string(name);
    w.uuid(uuid);
    w.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(250);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

async fn join<T: Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
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

/// Drives one entity-streaming pass **on the observer's own connection**.
///
/// This is not incidental plumbing, it is how the server is built: each
/// connection's streaming diff runs inside that connection's own task, on that
/// connection's own inbound packets. An observer who sends nothing therefore
/// receives nothing, no matter what other players do — the first draft of this
/// test failed with `got []` for exactly that reason. B re-sends the *same*
/// position every time, so B's own snapshot never changes and the only diff
/// the pass can find is A's.
async fn pump<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    client
        .write_packet(
            play::serverbound::MOVE_PLAYER_POS,
            &pos_bytes(0.0, 65.0, 0.0),
        )
        .await
        .unwrap();
    drain(client).await
}

fn teleports(packets: &[(i32, Vec<u8>)]) -> Vec<TeleportEntity> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::TELEPORT_ENTITY)
        .map(|(_, payload)| decode_teleport_entity(payload))
        .collect()
}

fn head_rotations(packets: &[(i32, Vec<u8>)]) -> Vec<(i32, i8)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::ROTATE_HEAD)
        .map(|(_, payload)| decode_rotate_head(payload))
        .collect()
}

fn adds(packets: &[(i32, Vec<u8>)]) -> Vec<AddEntity> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::ADD_ENTITY)
        .map(|(_, payload)| decode_add_entity(payload))
        .collect()
}

fn uuid_for(slot: u128) -> Uuid {
    Uuid::from_u128(0x4381_0000_0000_0000_0000_0000_0000_0000 + slot)
}

/// Two connections, one shared world: B observes A turning, both while walking
/// and while standing still.
#[tokio::test]
async fn a_players_facing_reaches_another_connection() {
    let registry = PlayerRegistry::new();
    let name_a = unique_username();
    let name_b = unique_username();
    assert_ne!(
        name_a, name_b,
        "offline mode derives the account uuid from the username, so a shared \
         name would make the two subjects the same player"
    );
    let uuid_a = uuid_for(1);
    let uuid_b = uuid_for(2);

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

    let _a_join = join(&mut client_a, &name_a, uuid_a).await;
    let b_join = join(&mut client_b, &name_b, uuid_b).await;
    assert_eq!(registry.len(), 2, "both players must be registered");

    // Establish which entity id B knows A by, and — critically — that it is a
    // *player* and not the `unwrap_or(0)` boat. Every rotation assertion below
    // is about this id, so without this the whole file could be describing
    // `minecraft:acacia_boat`.
    let b_adds = adds(&b_join);
    assert_eq!(
        b_adds.len(),
        1,
        "B must receive exactly one spawn — A's player — got {b_adds:?}"
    );
    assert_eq!(
        b_adds[0].type_id, PLAYER_ENTITY_TYPE_ID,
        "B received entity type {} where `minecraft:player` is \
         {PLAYER_ENTITY_TYPE_ID}; type 0 means the key did not resolve and \
         `entity_type_id(..).unwrap_or(0)` substituted `minecraft:acacia_boat`",
        b_adds[0].type_id
    );
    assert_ne!(PLAYER_ENTITY_TYPE_ID, 0);
    assert_eq!(b_adds[0].uuid, uuid_a, "the spawn must be A's profile uuid");
    let a_entity_id = b_adds[0].id;

    // ------------------------------------------------------------------
    // Phase 1 — A walks *and* turns: `move_player_pos_rot`.
    // ------------------------------------------------------------------
    // This packet's angles used to be decoded and discarded, so
    // this is the assertion that fails on the old tree.
    client_a
        .write_packet(
            play::serverbound::MOVE_PLAYER_POS_ROT,
            &pos_rot_bytes(8.0, 65.0, 8.0, 90.0, -45.0),
        )
        .await
        .unwrap();
    // Let A's own connection task finish dispatching before asking B to
    // look: the republish into the registry happens on A's task, so pumping
    // B first is a race that reads the pre-turn snapshot.
    let _ = drain(&mut client_a).await;
    let after_walk = pump(&mut client_b).await;

    let tps = teleports(&after_walk);
    let a_tp = tps
        .iter()
        .find(|t| t.id == a_entity_id)
        .unwrap_or_else(|| panic!("B must receive a teleport_entity for A ({a_entity_id}), got {tps:?}; B saw packet ids {:?}", after_walk.iter().map(|(id, _)| *id).collect::<Vec<_>>()));
    assert_eq!(
        a_tp.yaw, 90.0_f32,
        "B must see A's yaw as the exact 90.0 A sent; 0.0 means \
         `move_player_pos_rot`'s angles are still being decoded and dropped"
    );
    assert_eq!(
        a_tp.pitch, -45.0_f32,
        "B must see A's pitch as the exact -45.0 A sent"
    );

    // The packed byte, predicted by hand from vanilla's `vanilla's own mth's own pack degrees`
    // (`90 * 256 / 360 == 64` exactly, so floor and round agree and this
    // expectation does not encode our rounding choice).
    let heads = head_rotations(&after_walk);
    let a_head = heads
        .iter()
        .find(|(id, _)| *id == a_entity_id)
        .unwrap_or_else(|| panic!("B must receive a rotate_head for A, got {heads:?}"));
    assert_eq!(
        a_head.1, 64_i8,
        "B must see A's head yaw packed as 64 (90 deg * 256 / 360); 0 means the \
         registry is still lowering the hard-coded `head_yaw: 0.0`"
    );

    // ------------------------------------------------------------------
    // Phase 2 — A turns *on the spot*: `move_player_rot`, no position.
    // ------------------------------------------------------------------
    // A packet that used to decode to `ServerBound::Ignored`.
    client_a
        .write_packet(play::serverbound::MOVE_PLAYER_ROT, &rot_bytes(180.0, 0.0))
        .await
        .unwrap();
    let _ = drain(&mut client_a).await;
    let after_turn = pump(&mut client_b).await;

    let tps = teleports(&after_turn);
    let a_tp = tps
        .iter()
        .find(|t| t.id == a_entity_id)
        .unwrap_or_else(|| panic!("B must receive a teleport_entity for A after a turn-on-the-spot, got {tps:?}"));
    assert_eq!(
        a_tp.yaw, 180.0_f32,
        "B must see A's new yaw of exactly 180.0; 90.0 means `move_player_rot` \
         is still `Ignored` and only the earlier pos_rot was observed"
    );
    assert_eq!(a_tp.pitch, 0.0_f32, "B must see A's new pitch of exactly 0.0");

    let heads = head_rotations(&after_turn);
    let a_head = heads
        .iter()
        .find(|(id, _)| *id == a_entity_id)
        .unwrap_or_else(|| panic!("B must receive a rotate_head for A after a turn, got {heads:?}"));
    // `180 * 256 / 360 == 128`, which wraps to `-128` in a signed byte exactly
    // as vanilla's own `(byte)` cast does.
    assert_eq!(
        a_head.1, -128_i8,
        "B must see A's head yaw packed as -128 (180 deg * 256 / 360, wrapped)"
    );

    drop(client_a);
    drop(client_b);
    let _ = task_a.await;
    let _ = task_b.await;
}
