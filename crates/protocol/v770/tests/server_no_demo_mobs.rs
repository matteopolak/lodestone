//! **A singleplayer world greets the player with no mobs at all** — measured on
//! the wire, from a real join to a real [`IntegratedServer`].
//!
//! # What was broken
//!
//! `MobSim`'s only production path to a client-visible mob was
//! `mobs::seed_demo_mobs`: a fixed ring of six, placed once around the world spawn
//! at world open. `DEMO_SPECIES` says outright what it is for — "this exists purely
//! so computed AI motion reaching the wire has a
//! population to move" — and it lists the six in order: zombie, cow, wolf, blaze,
//! **guardian**, creeper, one per roster family. `lodestone-shell/src/net.rs`
//! passed `6`, so every new singleplayer world opened with a guardian flopping
//! about on dry land next to a blaze. `DEMO_SPECIES`' own doc even conceded it:
//! "a guardian on land is a real consequence and an accepted one".
//!
//! It was the right call while the alternative was a goal table nothing
//! instantiated. It is the wrong call for a world someone is playing.
//!
//! # Why this gate is on the wire and not on a counter
//!
//! `IntegratedServer` exposes no mob accessor, and it should not need to: the
//! property the owner reported is *"I can see mobs I did not ask for"*, and the
//! only faithful measurement of that is what the client is actually sent. A
//! counter on the sim would also pass against a sim that seeded mobs and failed to
//! stream them, which is a different bug wearing the same green tick.
//!
//! So this joins with the real [`V770ServerProtocol`] and counts `add_entity`
//! packets. `mobs::seed_demo_mobs` runs in a background task
//! (`integrated.rs`'s `seed_task`), so the gate ticks the connection repeatedly
//! and drains, rather than asserting once — the same "a freshly seeded entity is
//! not visible until the next tick" hazard a live oracle has.
//!
//! # The control
//!
//! [`the_demo_population_is_still_reachable_for_a_debug_session`] proves the
//! detector works, and it is the *whole* reason this file can assert an absence:
//! it drives the same code path with `LODESTONE_DEMO_MOBS` set and requires
//! `add_entity` packets to appear. Without it, "no mobs" would also pass against a
//! server whose entity streaming was broken outright, or against a join that never
//! reached Play.

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, Transport};
use lodestone_server::IntegratedServer;
use lodestone_server::{ChunkColumn, ChunkSource};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::{configuration, login, play};
use uuid::Uuid;

mod common;
use common::unique_username;

/// `minecraft:player`'s network entity-type id in protocol 776, from Mojang's
/// generated `registries.json` for 26.2 — the same constant
/// `server_player_entity_stream.rs` pins and for the same reason: `add_entity`'s
/// encoder does `entity_type_id(name).unwrap_or(0)` and index `0` is
/// `minecraft:acacia_boat`, so counting *any* `add_entity` without looking at the
/// type would be counting the wrong thing.
const PLAYER_ENTITY_TYPE_ID: i32 = 156;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// Top of the flat floor. Demo mobs are placed on the real terrain surface
/// (`mobs::surface_y`), so the fixture needs one for them to stand on — a void
/// world would make the control pass for the wrong reason.
const FLOOR_TOP_Y: i32 = 64;

/// A flat, solid floor everywhere: the surface `seed_demo_mobs` needs.
struct FlatSource;

impl ChunkSource for FlatSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                for y in MIN_Y..=FLOOR_TOP_Y {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.column(cx, cz)
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.column(cx, cz)
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // Read-only fixture.
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

fn pos_bytes(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut w = Writer::default();
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.u8(1); // on ground
    w.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    const QUIET: Duration = Duration::from_millis(200);
    let mut out = Vec::new();
    while let Ok(Ok(Some(packet))) = tokio::time::timeout(QUIET, client.read_packet()).await {
        out.push(packet);
    }
    out
}

/// The `(entity id, type id)` of every `add_entity` in `packets`.
///
/// Layout per `encode_add_entity_body`: VarInt id, uuid, VarInt type, then the
/// position and angles this gate does not read.
fn add_entities(packets: &[(i32, Vec<u8>)]) -> Vec<(i32, i32)> {
    packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::ADD_ENTITY)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            let id = r.var_i32().expect("add_entity id");
            let _uuid = r.uuid().expect("add_entity uuid");
            let type_id = r.var_i32().expect("add_entity type id");
            (id, type_id)
        })
        .collect()
}

/// Everything that is not this connection's own player entity — i.e. mobs.
fn non_player_entities(packets: &[(i32, Vec<u8>)]) -> Vec<(i32, i32)> {
    add_entities(packets)
        .into_iter()
        .filter(|&(_, type_id)| type_id != PLAYER_ENTITY_TYPE_ID)
        .collect()
}

/// Joins, then pumps the connection long enough for the background mob-seeding
/// task to have run and for a streaming pass to publish anything it created.
///
/// Returns every packet seen from the handshake onward. Pumping matters: each
/// connection's entity-streaming diff runs on that connection's own inbound
/// packets, so an observer that sends nothing receives nothing regardless of what
/// the sim contains — the trap `server_player_entity_stream.rs` documents.
async fn join_and_pump<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    let name = unique_username();
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    client
        .write_packet(0, &hello_bytes(&name, Uuid::new_v4()))
        .await
        .unwrap();
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

    // The seed task is spawned, not awaited, so poll rather than assert once.
    let feet = f64::from(FLOOR_TOP_Y + 1);
    for _ in 0..12 {
        client
            .write_packet(play::serverbound::MOVE_PLAYER_POS, &pos_bytes(8.5, feet, 8.5))
            .await
            .unwrap();
        seen.extend(drain(client).await);
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    seen
}

/// A default singleplayer world streams **no** mob to the joining player, even
/// though its constructor is asked for six.
///
/// `6` is passed deliberately: it is exactly what `lodestone-shell/src/net.rs`
/// passes in production, so this gate measures the real configuration rather than
/// a configuration chosen to make it pass. `integrated.rs` routes it through
/// `demo_mob_count`, which answers `0` with `LODESTONE_DEMO_MOBS` unset.
#[tokio::test]
async fn a_default_singleplayer_world_streams_no_mobs() {
    // Not set by any test in this workspace; asserted rather than assumed, because
    // a leaked variable from another process would make this gate pass vacuously
    // in the *other* direction and be very hard to explain.
    assert!(
        std::env::var_os("LODESTONE_DEMO_MOBS").is_none(),
        "precondition: LODESTONE_DEMO_MOBS must be unset for the default-world arm"
    );

    let (_server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        FlatSource,
        (-2..=2, -2..=2),
        (8, 8),
        // Production's own value.
        6,
        1,
    );
    let mut client = Connection::new(client_io);
    let seen = join_and_pump(&mut client).await;

    // Premise: the join actually completed and entity streaming actually ran, or
    // the absence below is meaningless. The player's own entity is not sent to
    // itself, so the evidence of a live Play session is the login packet plus
    // terrain.
    assert!(
        seen.iter().any(|(id, _)| *id == play::clientbound::LOGIN),
        "premise: the join must have reached Play"
    );
    assert!(
        seen.iter()
            .any(|(id, _)| *id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT),
        "premise: terrain must have been streamed, so the session is live"
    );

    let mobs = non_player_entities(&seen);
    assert!(
        mobs.is_empty(),
        "a fresh singleplayer world must stream no mobs; got {} add_entity packets \
         for non-player types {:?} — the demo ring (zombie, cow, wolf, blaze, \
         guardian, creeper) is back",
        mobs.len(),
        mobs
    );
}

/// **The control.** With `LODESTONE_DEMO_MOBS` set, the same constructor and the
/// same fixture must stream the demo population — proving the gate above measures
/// a decision and not a broken entity path.
///
/// `#[ignore]`d and run in its own process, because it must mutate a process-global
/// environment variable and the other test in this file asserts that variable is
/// unset. Cargo runs tests in threads of one process, so the two cannot safely
/// share it:
///
/// ```text
/// LODESTONE_DEMO_MOBS=1 cargo test -p lodestone-v770 --test server_no_demo_mobs \
///     the_demo_population_is_still_reachable -- --ignored --nocapture
/// ```
///
/// Observed passing that way while writing this file; the quoted failure text for
/// the inverse (the seeding left in place) is in the task report.
#[tokio::test]
#[ignore = "needs LODESTONE_DEMO_MOBS in the environment; see the doc comment"]
async fn the_demo_population_is_still_reachable_for_a_debug_session() {
    assert!(
        std::env::var_os("LODESTONE_DEMO_MOBS").is_some(),
        "this control is meaningless without LODESTONE_DEMO_MOBS set — run it as \
         the doc comment shows, do not just un-ignore it"
    );

    let (_server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        V770ServerProtocol,
        FlatSource,
        (-2..=2, -2..=2),
        (8, 8),
        6,
        1,
    );
    let mut client = Connection::new(client_io);
    let seen = join_and_pump(&mut client).await;

    let mobs = non_player_entities(&seen);
    assert!(
        !mobs.is_empty(),
        "with LODESTONE_DEMO_MOBS set the demo ring must still reach the wire, or \
         `a_default_singleplayer_world_streams_no_mobs` is passing because entity \
         streaming is broken rather than because seeding is off"
    );
}
