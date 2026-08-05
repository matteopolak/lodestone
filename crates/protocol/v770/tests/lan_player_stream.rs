//! Issue #438, **production path**: two real TCP clients against one
//! `IntegratedServer::bind` see each other as player entities.
//!
//! # Why this exists alongside `server_player_entity_stream.rs`
//!
//! That file drives `serve_connection` directly over `memory_pair` and proves
//! the *mechanism*. This one proves the **wiring** — that `bind`, the
//! constructor open-to-LAN actually uses, really composes the player registry
//! into the entity source it hands each accepted socket. Without this file
//! `bind` could ship with `relay_mobs.clone()` unchanged, every assertion in
//! the other file would still pass, and LAN multiplayer would remain exactly as
//! invisible as it was before #438: the island failure mode, one level up from
//! the code.
//!
//! **Verified as a control, not described**: with
//! `crates/lodestone-server/src/integrated.rs` reverted to its pre-#438 form
//! this test fails with *"B must receive A's player entity over LAN — got []"*.
//! It lands together with that file's `PlayerAwareSource` composition and is
//! meaningless without it.
//!
//! # Why `flavor = "multi_thread"`
//!
//! Not decoration. `bind` spawns `run_tick_loop` at 20 TPS into the same
//! runtime, and on tokio's default **current-thread** test runtime that loop
//! starves this test's own `tokio::time::timeout` timers: login and
//! configuration complete, then every subsequent read hangs forever with the
//! timeout never firing. Measured while writing this file — the plain
//! `#[tokio::test]` version ran past 60 s with no output. This is the same
//! one-thread contention `crate::server::SourceRef`'s doc comment describes for
//! issue #293, seen from the test side; the shell runs its own runtime and is
//! unaffected.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::Connection;
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_testsupport::unique_username;
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use uuid::Uuid;

/// `minecraft:player`'s network entity-type id in protocol 776 — Mojang's own
/// generated `registries.json` for 26.2, not our table. See
/// `server_player_entity_stream.rs` for why the distinction is load-bearing
/// (`entity_type_id(...).unwrap_or(0)` streams a boat for a wrong key).
const PLAYER_ENTITY_TYPE_ID: i32 = 156;

/// A never-sampled terrain source; the same shape `server_status.rs` uses.
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

fn move_bytes(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.u8(1); // MOVE_FLAG_ON_GROUND
    w.into_vec()
}

async fn drain<T: lodestone_net::Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(Ok(Some(p))) =
        tokio::time::timeout(Duration::from_millis(400), client.read_packet()).await
    {
        out.push(p);
    }
    out
}

async fn join<T: lodestone_net::Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client
        .write_packet(0, &hello_bytes(name, uuid))
        .await
        .unwrap();
    let _ = client.read_packet().await.unwrap(); // login_finished
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = client.read_packet().await.unwrap(); // finish_configuration
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    drain(client).await
}

/// Decodes `add_entity`'s leading id / uuid / type-id triple.
fn add_entities(packets: &[(i32, Vec<u8>)]) -> Vec<(i32, Uuid, i32)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::ADD_ENTITY)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            (
                r.var_i32().expect("id"),
                r.uuid().expect("uuid"),
                r.var_i32().expect("type id"),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_lan_clients_receive_each_others_player_entities() {
    let server = IntegratedServer::bind("127.0.0.1:0", V770ServerProtocol, AirSource, 0)
        .await
        .expect("bind on an ephemeral port");
    let addr = server.local_addr().expect("a bound listener has an address");

    // Offline mode derives the account uuid from the username, so two
    // connections sharing a name are the *same player* — which would make the
    // self-exclusion assertion below vacuous rather than merely wrong.
    let name_a = unique_username();
    let name_b = unique_username();
    assert_ne!(name_a, name_b);
    let uuid_a = Uuid::from_u128(0x4380_0000_0000_0000_0000_0000_0000_0001);
    let uuid_b = Uuid::from_u128(0x4380_0000_0000_0000_0000_0000_0000_0002);

    let mut client_a = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client A connects"),
    );
    let mut client_b = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client B connects"),
    );

    let a_join = join(&mut client_a, &name_a, uuid_a).await;
    assert!(
        add_entities(&a_join).is_empty(),
        "A joined an empty LAN world, got {:?}",
        add_entities(&a_join)
    );

    let b_join = join(&mut client_b, &name_b, uuid_b).await;
    let spawns = add_entities(&b_join);
    let a_spawn = spawns
        .iter()
        .find(|(_, uuid, _)| *uuid == uuid_a)
        .unwrap_or_else(|| {
            panic!(
                "B must receive A's player entity over LAN — got {spawns:?}. \
                 An empty list means `bind` is still handing each connection the \
                 bare mob source instead of a `PlayerAwareSource`."
            )
        });
    assert_eq!(
        a_spawn.2, PLAYER_ENTITY_TYPE_ID,
        "A's LAN entity must stream as `minecraft:player` ({PLAYER_ENTITY_TYPE_ID}), \
         not type {}",
        a_spawn.2
    );
    assert!(
        !spawns.iter().any(|(_, uuid, _)| *uuid == uuid_b),
        "B must not receive its own player entity over LAN — a doppelgänger"
    );

    // A learns about B on its next pass, driven by A's own movement packet.
    client_a
        .write_packet(
            play::serverbound::MOVE_PLAYER_POS,
            &move_bytes(8.0, 100.0, 8.0),
        )
        .await
        .unwrap();
    let a_after = drain(&mut client_a).await;
    assert!(
        add_entities(&a_after)
            .iter()
            .any(|(_, uuid, type_id)| *uuid == uuid_b && *type_id == PLAYER_ENTITY_TYPE_ID),
        "A must receive B's player entity over LAN, got {:?}",
        add_entities(&a_after)
    );

    drop(client_a);
    drop(client_b);
    server.shutdown().await;
}
