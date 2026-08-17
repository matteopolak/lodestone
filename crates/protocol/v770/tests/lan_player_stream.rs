//! **Production path**: two real TCP clients against one
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
//! invisible as it once was: the island failure mode, one level up from
//! the code.
//!
//! **Verified as a control, not described**: with
//! `crates/lodestone-server/src/integrated.rs` reverted to its pre-fix form
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
//! one-thread contention `crate::server::SourceRef`'s doc comment describes,
//! seen from the test side; the shell runs its own runtime and is
//! unaffected.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::Connection;
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use uuid::Uuid;

mod common;
use common::unique_username;

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

/// How long a silence means "the server has finished sending for now".
///
/// Only sound where an **absence** is being established, because a gap is the
/// only evidence of an absence there is. Where a packet is *expected*, use
/// [`drain_until`] — see its doc comment for why this one flaked.
const QUIET_GAP: Duration = Duration::from_millis(400);

/// Ceiling on waiting for a packet that must arrive. Fails the test rather than
/// hanging, and is deliberately far larger than [`QUIET_GAP`]: it is only ever
/// reached when the packet genuinely never comes.
const ARRIVAL_DEADLINE: Duration = Duration::from_secs(30);

/// Reads until the stream goes quiet for [`QUIET_GAP`].
///
/// A **gap-terminated** drain can only ever *under*-collect, so it is the right
/// instrument for "no `add_entity` arrived" and the wrong one for "A's
/// `add_entity` arrived" — see [`drain_until`].
async fn drain<T: lodestone_net::Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    while let Ok(Ok(Some(p))) = tokio::time::timeout(QUIET_GAP, client.read_packet()).await {
        out.push(p);
    }
    out
}

/// Reads until `done` is satisfied by the packets collected so far, or until
/// [`ARRIVAL_DEADLINE`] — then keeps draining whatever is already queued so the
/// caller still sees the full picture for its other assertions.
///
/// # Why this exists: a wall-clock stopping condition on a *positive* assertion
///
/// Both "B receives A" assertions used [`drain`], whose stopping condition is a
/// 400 ms silence. That silence is not evidence the server is done — it is
/// evidence that *nothing arrived in 400 ms*, and the two differ under load. This
/// test passed standalone (4.3 s and 30.8 s on two runs) and failed inside
/// `cargo test --workspace -- --test-threads=2`, reporting *"An empty list means
/// `bind` is still handing each connection the bare mob source"* — a confident
/// accusation against production for a drain that simply stopped early.
///
/// The join path also got slower underneath it: the world-spawn search
/// runs before chunk streaming and, against [`AirSource`], finds no valid spawn in
/// any of its 121 spiral candidates — so it generates 121 full-height columns
/// (~196 KiB each) on the connection task between `FINISH_CONFIGURATION` and the
/// first packet. 400 ms of quiet in that window stopped being unusual.
///
/// So the stopping condition is now the **event**, and the timeout is only a
/// failure ceiling. This cannot mask the defect the test exists to catch: with
/// `bind` reverted to its pre-fix form the `add_entity` never arrives at all, and
/// this waits the full deadline and then fails on the same assertion.
async fn drain_until<T: lodestone_net::Transport>(
    client: &mut Connection<T>,
    done: impl Fn(&[(i32, Vec<u8>)]) -> bool,
) -> Vec<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + ARRIVAL_DEADLINE;
    while !done(&out) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, client.read_packet()).await {
            Ok(Ok(Some(p))) => out.push(p),
            // Closed socket or decode error: stop and let the caller's own
            // assertion report what is missing.
            Ok(_) => break,
            Err(_) => break,
        }
    }
    out.extend(drain(client).await);
    out
}

/// Drives the handshake/login/configuration exchange, stopping just before the
/// play-state stream so the caller chooses its own drain.
async fn handshake<T: lodestone_net::Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) {
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client
        .write_packet(0, &hello_bytes(name, uuid))
        .await
        .unwrap();
    let _ = common::read_login_packet(client).await.unwrap(); // login_finished
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = common::read_login_packet(client).await.unwrap(); // finish_configuration
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
}

/// A join whose caller asserts an **absence**, so the gap-terminated [`drain`] is
/// the only instrument available.
async fn join<T: lodestone_net::Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
) -> Vec<(i32, Vec<u8>)> {
    handshake(client, name, uuid).await;
    drain(client).await
}

/// A join whose caller asserts a packet **arrives**, so it waits for the event
/// rather than for a silence. See [`drain_until`].
async fn join_awaiting<T: lodestone_net::Transport>(
    client: &mut Connection<T>,
    name: &str,
    uuid: Uuid,
    done: impl Fn(&[(i32, Vec<u8>)]) -> bool,
) -> Vec<(i32, Vec<u8>)> {
    handshake(client, name, uuid).await;
    drain_until(client, done).await
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

    let b_join = join_awaiting(&mut client_b, &name_b, uuid_b, |packets| {
        add_entities(packets)
            .iter()
            .any(|(_, uuid, _)| *uuid == uuid_a)
    })
    .await;
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
    // Also a positive assertion, so also event-terminated. Note the predicate
    // matches on the uuid alone while the assertion additionally pins the type id:
    // a wrong type id must fail the assertion, not silently extend the wait.
    let a_after = drain_until(&mut client_a, |packets| {
        add_entities(packets)
            .iter()
            .any(|(_, uuid, _)| *uuid == uuid_b)
    })
    .await;
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
