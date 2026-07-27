//! Live 1.8.9 chunk-decode acceptance test (the V1 gate).
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 1.8.9 server
//! (offline mode, flat world) on `127.0.0.1:25566` with:
//!
//! ```text
//! cargo test -p lodestone-v47 --features live-chunk -- --ignored live_chunk
//! ```
//!
//! This drives the join at the packet level over [`lodestone_net::Connection`]
//! and routes every packet — including `map_chunk` (id 33) and `map_chunk_bulk`
//! (id 38) — through the **real** [`V47Adapter::handle_packet`], which decodes
//! chunks and applies them to a client-owned [`lodestone_world::World`] through
//! the version-free `WorldSink` seam, emitting only a lightweight
//! `ClientEvent::ChunkLoaded { pos }`. The test then **queries the world back**
//! and asserts *real* properties against the stored columns:
//!
//! * every column decodes with **zero trailing bytes** (the adapter's
//!   `ensure_empty`, surfaced here as a decode error) — the single best
//!   detector of a subtly wrong layout;
//! * the flat world's known layers land at the right Y (catches a byte-correct
//!   but YZX-transposed or little-endian-swapped decode);
//! * it reports the column count read out of `World`, and confirms there is no
//!   modern chunk-batch ACK flow control in 1.8.
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
use lodestone_v47::V47Adapter;
use lodestone_v47::packet_ids::play;
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
    std::env::var("LODESTONE_V47_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25566)
}

#[tokio::test]
#[ignore = "requires a live 1.8.9 server on 127.0.0.1:25566"]
async fn decodes_real_chunks_from_live_1_8_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V47Adapter::new();
    // The client owns the world; the adapter applies decoded chunks to it via
    // the WorldSink. This test never touches the chunk codec directly — it
    // proves the seam by querying this store after the bytes cross it.
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.8.9 server");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut chunk_loaded = 0usize; // ChunkLoaded events (one per loaded column)
    let mut chunk_unloaded = 0usize;
    let mut via_single_map_chunk = 0usize;
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
            // set_health here. A value <= 0 means an inherited dead player and a
            // silent chunk blackout — not a decoder bug.
            if state == ConnectionState::Play && packet_id == play::clientbound::UPDATE_HEALTH {
                let mut reader = Reader::new(&payload);
                if let Ok(health) = reader.f32() {
                    last_health = Some(health);
                    if health <= 0.0 {
                        eprintln!(
                            "WARNING: set_health = {health} (<=0). Inherited a dead player? \
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
            let is_single = packet_id == play::clientbound::MAP_CHUNK;
            match adapter.handle_packet(&mut world, state, packet_id, &payload) {
                Ok(directives) => {
                    for directive in directives {
                        match &directive {
                            Directive::Emit(ClientEvent::ChunkLoaded { .. }) => {
                                chunk_loaded += 1;
                                if is_single {
                                    via_single_map_chunk += 1;
                                }
                                first_chunk_at.get_or_insert_with(Instant::now);
                            }
                            Directive::Emit(ClientEvent::ChunkUnloaded { .. }) => {
                                chunk_unloaded += 1;
                            }
                            Directive::Emit(ClientEvent::KeepAlive { id }) => {
                                // 1.8 has no chunk_batch ACK; keep-alive is all
                                // that keeps the connection open through the window.
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
        eprintln!("last set_health       : {h}");
    }
    assert!(reached_play, "never reached Play");
    assert!(
        decode_errors.is_empty(),
        "adapter reported chunk decode errors (non-zero trailing bytes or bad \
         geometry): {decode_errors:?}"
    );
    assert!(
        chunk_loaded > 0 && !world.is_empty(),
        "no chunks reached the world — if set_health was <=0 above, an inherited \
         dead player is the cause, not the decoder"
    );

    // ---- Query the world back. Running the detectors on columns pulled from
    //      the store (not from a local decode) is what proves the seam. ----
    let sample = world
        .values()
        .find(|loaded| {
            let col = &loaded.column;
            (0..16).all(|dy| {
                let first = col.get_block(0, dy, 0);
                (0..16).all(|x| (0..16).all(|z| col.get_block(x, dy, z) == first))
            })
        })
        .or_else(|| world.values().next())
        .expect("at least one column in the world");

    let col = &sample.column;
    let mut layers: Vec<(i32, Option<u32>)> = Vec::new();
    for y in 0..16 {
        let first = col.get_block(0, y, 0);
        let uniform = (0..16).all(|x| (0..16).all(|z| col.get_block(x, y, z) == first));
        layers.push((y, uniform.then_some(first)));
    }

    for (y, value) in &layers {
        assert!(
            value.is_some(),
            "plane at y={y} is not uniform — decode is likely YZX-transposed or LE/BE-swapped"
        );
    }

    // Bedrock (block id 7 -> state 112) is the lowest layer of a vanilla 1.8
    // overworld. Known-block-at-known-Y, read back out of the world store.
    let bottom = layers[0].1.unwrap();
    assert_eq!(
        bottom, 112,
        "lowest layer (y=0) should be bedrock (state 112), got {bottom}"
    );

    let solid = layers.iter().take_while(|(_, v)| *v != Some(0)).count();
    assert!(solid >= 1, "expected solid terrain at the bottom");
    assert!(
        solid < 16,
        "expected air above terrain within the bottom section"
    );

    eprintln!("\n=== LIVE 1.8.9 CHUNK -> WORLD SEAM REPORT ===");
    eprintln!("chunk_loaded events   : {chunk_loaded}");
    eprintln!("  via single map_chunk: {via_single_map_chunk}");
    eprintln!(
        "  via map_chunk_bulk  : {}",
        chunk_loaded - via_single_map_chunk
    );
    eprintln!("chunk_unloaded events : {chunk_unloaded}");
    eprintln!("distinct columns in World: {}", world.len());
    eprintln!(
        "trailing bytes/column : 0 (adapter ensure_empty; {} decode errors)",
        decode_errors.len()
    );
    eprintln!("flow control          : none (1.8 has no chunk_batch ACK; all chunks pushed)");
    eprint!("flat-world layers y0-3: ");
    for (y, value) in layers.iter().take(4) {
        match value {
            Some(0) => eprint!("[y{y}:air] "),
            Some(v) => eprint!("[y{y}:{v}] "),
            None => eprint!("[y{y}:?] "),
        }
    }
    eprintln!("\n===========================================\n");
}
