//! Live 1.12.2 chunk-decode acceptance test (the V4 gate).
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against a real vanilla 1.12.2 server
//! (offline mode) on `127.0.0.1:25568` with:
//!
//! ```text
//! cargo test -p lodestone-v1-9 --features live-chunk -- --ignored live_chunk
//! ```
//!
//! This drives the join at the packet level over [`lodestone_net::Connection`]
//! and routes every packet — including `map_chunk` (id 32) — through the
//! **real** [`V340Adapter::handle_packet`], which decodes the paletted,
//! pre-flattening 1.12.2 column and applies it to a client-owned
//! [`lodestone_world::World`] through the version-free `WorldSink` seam,
//! emitting only a lightweight `ClientEvent::ChunkLoaded { pos }`. The test then
//! **queries the world back** and asserts *real* properties against the stored
//! columns:
//!
//! * every column decodes with **zero trailing bytes** (the adapter's
//!   `ensure_empty`, surfaced here as a decode error) — the single best
//!   detector of a subtly wrong layout, and in particular of the pre-1.16
//!   **straddling** long unpacking being wrong;
//! * the world-independent **bedrock floor** (legacy block id 7, meta 0 →
//!   canonical `minecraft:bedrock`, per [`lodestone_v1_9::canonical::resolve`])
//!   fills the whole `y=0` plane of every column, in both flat and default
//!   worlds (catches a byte-correct but YZX-transposed or mis-shifted decode,
//!   and a scrambled straddling unpack) — note this is the **canonical 26.2**
//!   state id now, not the legacy `112` composite, since `packets/chunk.rs`
//!   translates every block through `crate::canonical` before it reaches
//!   [`lodestone_world`] storage;
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
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v1_9::V340Adapter;
use lodestone_v1_9::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

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
    std::env::var("LODESTONE_V340_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25568)
}

#[tokio::test]
#[ignore = "requires a live 1.12.2 server on 127.0.0.1:25568"]
async fn decodes_real_chunks_from_live_1_12_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V340Adapter::new();
    // The client owns the world; the adapter applies decoded chunks to it via
    // the WorldSink. This test never touches the chunk codec directly — it
    // proves the seam by querying this store after the bytes cross it.
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.12.2 server");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut chunk_loaded = 0usize; // ChunkLoaded events (one per loaded column)
    let mut chunk_unloaded = 0usize;
    let mut reached_play = false;
    let mut last_health: Option<f32> = None;
    let mut decode_errors: Vec<String> = Vec::new();

    let overall = Duration::from_secs(45);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(8);
    let target_columns = 100usize;
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

            // The seam under test: chunk bytes go through the real adapter,
            // which decodes them and applies them to `world` via the WorldSink,
            // emitting only a lightweight ChunkLoaded { pos }. Zero trailing
            // bytes is enforced inside the adapter (ensure_empty), so a misparse
            // surfaces here as an Err rather than a silently truncated chunk.
            match adapter.handle_packet(&mut world, state, packet_id, &payload) {
                Ok(directives) => {
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
                                    &lodestone_model::ClientAction::KeepAliveResponse { id: *id },
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
    // The y=0 plane is a solid bedrock floor in *every* vanilla 1.12.2 column,
    // flat or default (only y=1..4 are patchy), so "y=0 all == canonical
    // bedrock" is a world-independent known-block-at-known-Y detector that
    // also proves the straddling unpack *and* the legacy->canonical
    // translation are both correct — a scrambled unpack would not land
    // bedrock uniformly on the bottom plane, and a broken translation would
    // land air (or some other wrong id) instead of bedrock's canonical id.
    let bedrock_state = match lodestone_v1_9::canonical::resolve(7, 0) {
        lodestone_v1_9::canonical::CanonicalBlockState::Resolved(id) => id.raw(),
        other => panic!("legacy bedrock (7,0) did not resolve to a canonical state: {other:?}"),
    };
    let mut checked = 0usize;
    let mut bedrock_planes = 0usize;
    for loaded in world.values() {
        let col = &loaded.column;
        checked += 1;
        let uniform_bedrock =
            (0..16).all(|x| (0..16).all(|z| col.get_block(x, 0, z) == bedrock_state));
        assert!(
            uniform_bedrock,
            "y=0 plane is not uniform canonical bedrock (state {bedrock_state}) — decode is \
             likely YZX-transposed, endian-swapped, the straddling unpack is wrong, or the \
             legacy->canonical translation is wrong"
        );
        bedrock_planes += 1;
    }
    assert!(checked > 0, "no columns to check");

    eprintln!("\n=== LIVE 1.12.2 CHUNK -> WORLD SEAM REPORT ===");
    eprintln!("chunk_loaded events      : {chunk_loaded}");
    eprintln!("chunk_unloaded events    : {chunk_unloaded}");
    eprintln!("distinct columns in World: {}", world.len());
    eprintln!("columns with bedrock y=0 : {bedrock_planes}/{checked}");
    eprintln!(
        "trailing bytes/column    : 0 (adapter ensure_empty; {} decode errors)",
        decode_errors.len()
    );
    eprintln!("==============================================\n");
}
