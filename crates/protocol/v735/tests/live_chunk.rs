//! Live 1.16.5 chunk-decode acceptance test (the V735 gate).
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against a real vanilla 1.16.5 server
//! (offline mode, flat world) on `127.0.0.1:25573` with:
//!
//! ```text
//! cargo test -p lodestone-v735 --features live-chunk --test live_chunk -- --ignored --nocapture
//! ```
//!
//! This drives the join at the packet level over [`lodestone_net::Connection`]
//! and routes every packet — including `map_chunk` (id 32) and the separate
//! `update_light` (id 35) that 1.14 split off — through the **real**
//! [`V735Adapter::handle_packet`], which decodes the flattened, paletted
//! 1.16.5 column (flat block-state ids, non-straddling long packing, a 1024-
//! entry 3-D biome array, a heightmaps NBT) and applies it to a client-owned
//! [`lodestone_world::World`] through the version-free `WorldSink` seam,
//! emitting only a lightweight `ClientEvent::ChunkLoaded { pos }`. The test then
//! **queries the world back** and asserts *real* properties against the stored
//! columns:
//!
//! * every column decodes with **zero trailing bytes** (the adapter's
//!   `ensure_empty`, surfaced here as a decode error) — the single best
//!   detector of a subtly wrong layout, and in particular of the 1.16
//!   **non-straddling** long unpacking (entries never cross an i64 boundary)
//!   being decoded with the pre-1.16 straddling scheme;
//! * the flat world's known **bedrock floor** (state id 33 after flattening)
//!   fills the whole `y=0` plane of every column (catches a byte-correct but
//!   YZX-transposed decode, and a scrambled long unpack), and the grass layer
//!   sits at `y=3` (catches a section-relative Y offset error);
//! * it reports the column count read out of `World`.
//!
//! Because the chunk bytes cross the same `WorldSink` seam the client uses, a
//! green run proves the store — not a local decode — holds the world.
//!
//! It lives in the version crate (not `lodestone-client`) precisely because it
//! names this crate's concrete chunk types; keeping it here means
//! `lodestone-client` references no protocol version at all.
#![cfg(feature = "live-chunk")]

use lodestone_testsupport::unique_username;
use std::time::{Duration, Instant};

use lodestone_core::Reader;
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v735::V735Adapter;
use lodestone_v735::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

/// Flattened 1.16.5 block-state id of `minecraft:bedrock` (its only state).
/// This is the world-independent floor of every vanilla overworld column.
const BEDROCK_STATE: u32 = 33;
/// Flattened 1.16.5 block-state id of `minecraft:grass_block[snowy=false]`,
/// the top layer of the default `flat` preset (bedrock, 2×dirt, grass).
const GRASS_STATE: u32 = 9;

/// Applies one login directive against the live connection.
async fn apply(
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    directive: Directive,
) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload)
                .await
                .expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Emit(_) => {}
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string());
        }
        _ => {}
    }
}

fn server_port() -> u16 {
    std::env::var("LODESTONE_V735_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25573)
}

#[tokio::test]
#[ignore = "requires a live 1.16.5 server on 127.0.0.1:25573"]
async fn decodes_real_chunks_from_live_1_16_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V735Adapter::new();
    // The client owns the world; the adapter applies decoded chunks to it via
    // the WorldSink. This test never touches the chunk codec directly — it
    // proves the seam by querying this store after the bytes cross it.
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.16.5 server (is lodestone-mc1165 up on :25573?)");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut chunk_loaded = 0usize; // ChunkLoaded events (one per loaded column)
    let mut chunk_unloaded = 0usize;
    let mut light_updates = 0usize;
    let mut reached_play = false;
    let mut last_health: Option<f32> = None;
    let mut decode_errors: Vec<String> = Vec::new();

    let overall = Duration::from_secs(45);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(8);
    let target_columns = 20usize; // view-distance 2 → a small but real cluster
    let mut first_chunk_at: Option<Instant> = None;

    let _ = tokio::time::timeout(overall, async {
        loop {
            if let Some(t) = first_chunk_at
                && (chunk_loaded >= target_columns || t.elapsed() >= collect_window)
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => break, // quiet server
                Ok(Ok(Some(packet))) => packet,
                Ok(Ok(None)) => break, // clean EOF
                Ok(Err(err)) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Play && !reached_play {
                reached_play = true;
                eprintln!("reached Play; receiving chunks…");
            }

            // Death-trap detector: the adapter does not model health, so dump
            // update_health here. A value <= 0 means an inherited dead player and
            // a silent chunk blackout — not a decoder bug.
            if state == ConnectionState::Play && packet_id == play::clientbound::UPDATE_HEALTH {
                let mut reader = Reader::new(&payload);
                if let Ok(health) = reader.f32() {
                    last_health = Some(health);
                    if health <= 0.0 {
                        eprintln!(
                            "WARNING: update_health = {health} (<=0). Inherited a dead player? \
                             Chunk blackout is expected until respawn — NOT a decoder bug."
                        );
                    }
                }
                continue;
            }

            // The seam under test: chunk + light bytes go through the real
            // adapter, which decodes them and applies them to `world` via the
            // WorldSink, emitting only a lightweight ChunkLoaded { pos }. Zero
            // trailing bytes is enforced inside the adapter (ensure_empty), so a
            // misparse surfaces here as an Err rather than a silently truncated
            // chunk.
            match adapter.handle_packet(&mut world, state, packet_id, &payload) {
                Ok(directives) => {
                    if state == ConnectionState::Play
                        && packet_id == play::clientbound::UPDATE_LIGHT
                    {
                        light_updates += 1;
                    }
                    for directive in directives {
                        match &directive {
                            Directive::Emit(ClientEvent::ChunkLoaded { .. }) => {
                                chunk_loaded += 1;
                                first_chunk_at.get_or_insert_with(Instant::now);
                            }
                            Directive::Emit(ClientEvent::ChunkUnloaded { .. }) => {
                                chunk_unloaded += 1;
                            }
                            Directive::Emit(ClientEvent::KeepAlive { id }) => {
                                if let Ok(Some((pid, body))) = adapter.encode_action(
                                    ConnectionState::Play,
                                    &ClientAction::KeepAliveResponse { id: *id },
                                ) {
                                    conn.write_packet(pid, &body).await.expect("keep-alive ack");
                                }
                            }
                            _ => apply(&mut conn, &mut state, directive).await,
                        }
                    }
                }
                Err(err) if state == ConnectionState::Play => {
                    decode_errors.push(format!("packet {packet_id}: {err}"));
                }
                Err(_) => {}
            }
        }
    })
    .await;

    if let Some(h) = last_health {
        eprintln!("last update_health    : {h}");
    }
    assert!(reached_play, "never reached Play");
    assert!(
        decode_errors.is_empty(),
        "adapter reported chunk decode errors (non-zero trailing bytes or bad \
         geometry): {decode_errors:?}"
    );
    assert!(
        chunk_loaded > 0 && !world.is_empty(),
        "no chunks reached the world — if update_health was <=0 above, an \
         inherited dead player is the cause, not the decoder"
    );

    // ---- Query the world back. Running the detector on columns pulled from the
    //      store (not from a local decode) is what proves the seam. ----
    //
    // The default 1.16.5 `flat` preset is bedrock(33) at y=0, dirt at y=1..2 and
    // grass_block(9) at y=3, in *every* column. "y=0 all == bedrock" is a
    // world-independent known-block-at-known-Y detector that also proves the
    // non-straddling long unpack is correct — a straddling (pre-1.16) unpack
    // would not land bedrock uniformly on the bottom plane. Checking grass at
    // y=3 additionally catches a section-relative Y offset error that a single
    // bottom-plane check would miss.
    let mut checked = 0usize;
    for loaded in world.values() {
        let col = &loaded.column;
        checked += 1;
        let uniform_bedrock =
            (0..16).all(|x| (0..16).all(|z| col.get_block(x, 0, z) == BEDROCK_STATE));
        assert!(
            uniform_bedrock,
            "y=0 plane is not uniform bedrock (state {BEDROCK_STATE}) — decode is \
             likely YZX-transposed, endian-swapped, or using the pre-1.16 \
             straddling long unpack"
        );
        let uniform_grass = (0..16).all(|x| (0..16).all(|z| col.get_block(x, 3, z) == GRASS_STATE));
        assert!(
            uniform_grass,
            "y=3 plane is not uniform grass_block (state {GRASS_STATE}) — the \
             section-relative Y offset is likely wrong"
        );
    }
    assert!(checked > 0, "no columns to check");

    eprintln!("\n=== LIVE 1.16.5 CHUNK -> WORLD SEAM REPORT ===");
    eprintln!("chunk_loaded events      : {chunk_loaded}");
    eprintln!("chunk_unloaded events    : {chunk_unloaded}");
    eprintln!("update_light packets      : {light_updates}");
    eprintln!("distinct columns in World: {}", world.len());
    eprintln!("columns bedrock@y0/grass@y3: {checked}/{checked}");
    eprintln!(
        "trailing bytes/column    : 0 (adapter ensure_empty; {} decode errors)",
        decode_errors.len()
    );
    eprintln!("==============================================\n");
}
