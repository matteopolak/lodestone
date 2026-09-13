//! Live chunk-batch **flow-control** regression test (duration-based).
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-v26-2 --features live-chunk --test live_chunk_flow -- --ignored --nocapture
//! ```
//!
//! # What this proves that `live_chunk` cannot
//!
//! `live_chunk` connects, receives the spawn area, asserts, and disconnects —
//! it never stays alive long enough to exhaust the server's unacknowledged-batch
//! budget. That makes it structurally blind to a whole *class* of bug: the
//! server throttles chunk delivery with a credit window
//! (`PlayerChunkSender`), handing out at most `maxUnacknowledgedBatches`
//! batches before it stops and waits for the client's
//! `chunk_batch_received` ACK. That limit **starts at 1** and only ramps to 10
//! after the first ACK is seen. So a client that never ACKs receives *exactly
//! one* batch and then a permanent, silent chunk blackout — spawn loads, then
//! walking produces void forever.
//!
//! The bug is therefore invisible to any assertion about *shape* (the first
//! batch decodes perfectly) and invisible to any *short* run (it manifests only
//! once the credit window is spent). This test closes that gap by
//! **construction**: it drives the real join through the public
//! [`VersionAdapter`] seam and asserts a property that is unreachable without a
//! working ACK loop — the number of finished batches climbing well past the
//! credit window — paired with a **negative control** that suppresses only the
//! ACK write and confirms delivery stalls at the very first batch.
//!
//! The batch *count* is itself the duration proof: the server's own gate makes
//! batch N (N > 1) unreachable unless batch N-1 was acknowledged, so observing
//! many finished batches is direct evidence that the ACK round-trip kept
//! flowing over the whole streaming window, not merely that one early burst
//! arrived.
#![cfg(feature = "live-chunk")]

use std::time::{Duration, Instant};

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use tokio::net::TcpStream;
use uuid::Uuid;

#[path = "../common/mod.rs"]
mod common;
use common::unique_username;

/// A `WorldSink` that discards chunk state. This gate measures *delivery
/// cadence*, not decoded contents, so the adapter's world-application seam is
/// satisfied with a no-op; chunk decoding itself is covered by `live_chunk`.
struct NullSink;

impl lodestone_world::WorldSink for NullSink {
    fn load(&mut self, _pos: lodestone_world::ChunkPos, _chunk: lodestone_world::LoadedChunk) {}
    fn merge(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::ColumnPatch) {}
    fn merge_biomes(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::BiomePatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(&mut self, _sx: i32, _sy: i32, _sz: i32, _blocks: &[(u8, u8, u8, u32)]) {}
    fn set_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _type_id: u32,
        _nbt: lodestone_core::Nbt,
    ) {
    }
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> lodestone_world::BlockEntitySync {
        lodestone_world::BlockEntitySync::ChunkAbsent
    }
    fn merge_light(
        &mut self,
        _pos: lodestone_world::ChunkPos,
        _patch: lodestone_world::LightPatch,
    ) {
    }
    fn unload(&mut self, _pos: lodestone_world::ChunkPos) {}
}

/// Applies one directive against the live connection, updating tracked state.
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

/// Decodes the leading `teleportId` (VarInt) and absolute `position` (three
/// big-endian doubles) of a clientbound `player_position`.
fn decode_teleport(payload: &[u8]) -> (i32, [f64; 3]) {
    let mut r = Reader::new(payload);
    let id = r.var_i32().expect("teleport id varint");
    let x = r.f64().expect("teleport x");
    let y = r.f64().expect("teleport y");
    let z = r.f64().expect("teleport z");
    (id, [x, y, z])
}

/// Sends `accept_teleportation(id)`.
async fn accept_teleport(conn: &mut Connection<TcpStream>, id: i32) {
    let mut w = Writer::default();
    w.var_i32(id);
    conn.write_packet(play::serverbound::ACCEPT_TELEPORTATION, &w.into_vec())
        .await
        .expect("accept teleportation");
}

/// Sends a `move_player_pos_rot` at the given absolute position (flags byte:
/// bit0 = on_ground). Used to keep the connection alive and nudging so the run
/// is a genuine *duration* exercise rather than an instantaneous burst.
async fn send_move(conn: &mut Connection<TcpStream>, pos: [f64; 3], yaw: f32) {
    let mut w = Writer::default();
    w.f64(pos[0]);
    w.f64(pos[1]);
    w.f64(pos[2]);
    w.f32(yaw);
    w.f32(0.0);
    w.u8(1); // on_ground
    conn.write_packet(play::serverbound::MOVE_PLAYER_POS_ROT, &w.into_vec())
        .await
        .expect("move player");
}

/// The result of driving one connection: whether we reached Play, how many
/// chunk batches the server finished, and how many chunks it delivered.
#[derive(Debug)]
struct FlowOutcome {
    reached_play: bool,
    batches_finished: usize,
    chunks: usize,
    /// Wall-clock span between the first and last finished batch — reported so a
    /// failure can be read as "one early burst" vs "sustained streaming".
    stream_span: Duration,
}

/// Joins the live server and drives the play stream for a bounded window,
/// routing **every** chunk-batch packet through the public
/// [`VersionAdapter::handle_packet`] seam (the code under test).
///
/// When `ack_enabled` is `true` the resulting `chunk_batch_received` `Send`
/// directive is written to the socket, exactly as a real consumer's transport
/// would flush it. When `false` — the negative control — that specific `Send`
/// is *dropped*; every other code path (batch timing, teleport ACKs,
/// keep-alives, movement) is byte-for-byte identical, so the only variable
/// between the two runs is whether the ACK reaches the server.
///
/// `stop_after_batches` lets the positive run finish promptly once it has
/// climbed well past the credit window; the negative run passes `None` and runs
/// the full window so the stall is shown to be *durable*, not merely momentary.
async fn drive_flow(ack_enabled: bool, stop_after_batches: Option<usize>) -> FlowOutcome {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        // Unique per run: offline mode derives the UUID from the *name*, so a
        // shared name shares one persisted player file and an inherited dead one
        // yields a silent chunk blackout that would look exactly like this bug.
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();

    let mut conn = Connection::connect("127.0.0.1:25565")
        .await
        .unwrap_or_else(|e| {
            panic!(
                "cannot reach the live 26.2 server on 127.0.0.1:25565 ({e}). \
                 This #[ignore]d gate REQUIRES it — start the `lodestone-mc262` \
                 container (offline mode, flat world) and re-run. A missing \
                 server is a FAILURE here, never a skip."
            )
        });
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut reached_play = false;
    let mut batches_finished = 0usize;
    let mut chunks = 0usize;
    let mut first_batch_at: Option<Instant> = None;
    let mut last_batch_at: Option<Instant> = None;
    let mut pos = [0.0f64; 3];
    let mut have_pos = false;
    let mut loaded_sent = false;
    let mut yaw = 0.0f32;

    // The overall window. Chosen long enough that, if the ACK loop were broken,
    // a healthy server would have streamed dozens more batches within it — so a
    // negative control that sees only one batch across this whole span is
    // strong evidence of a durable stall, not a slow tick.
    let window = Duration::from_secs(25);
    let started = Instant::now();
    let mut next_move = Instant::now() + Duration::from_millis(500);

    while started.elapsed() < window {
        if let Some(limit) = stop_after_batches
            && batches_finished >= limit
        {
            break;
        }

        // Keep nudging so the run stays alive. Rotate only — do not drift the
        // position: the server's own collision/on-ground check does not know
        // about our claimed position, and a long enough straight-line walk
        // eventually crosses a ledge or an un-flat spot and trips vanilla's
        // "floating too long" kick (`vanilla's own server game packet listener impl's own tick player`),
        // which has nothing to do with the property under test and previously
        // aborted only the (much longer) negative-control run.
        if reached_play && have_pos && Instant::now() >= next_move {
            yaw = (yaw + 3.0) % 360.0;
            send_move(&mut conn, pos, yaw).await;
            next_move = Instant::now() + Duration::from_millis(250);
        }

        let read = tokio::time::timeout(Duration::from_secs(2), conn.read_packet()).await;
        let (packet_id, payload) = match read {
            // Silence is expected in the negative control once the server stalls;
            // it is NOT a reason to stop — the durability of the stall over the
            // whole window is exactly what we are measuring.
            Err(_) => continue,
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => panic!("clean EOF — server closed the connection mid-run"),
            Ok(Err(err)) => panic!("read error during run: {err}"),
        };

        if state == ConnectionState::Play && !reached_play {
            reached_play = true;
        }

        if state == ConnectionState::Play {
            if packet_id == play::clientbound::PLAYER_POSITION {
                let (id, p) = decode_teleport(&payload);
                accept_teleport(&mut conn, id).await;
                pos = p;
                have_pos = true;
                if !loaded_sent {
                    // Tell the server we're loaded, like a real client, so it
                    // validates our movement. Sent identically in both runs.
                    conn.write_packet(play::serverbound::PLAYER_LOADED, &[])
                        .await
                        .expect("player loaded");
                    loaded_sent = true;
                }
                continue;
            }
            if packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
                chunks += 1;
            }
            if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                // Route through the adapter (the code under test) to produce the
                // ACK, then either flush it (positive) or drop it (negative).
                let now = Instant::now();
                first_batch_at.get_or_insert(now);
                last_batch_at = Some(now);
                batches_finished += 1;

                let directives = adapter
                    .handle_packet(&mut NullSink, state, packet_id, &payload)
                    .expect("chunk_batch_finished handled by adapter seam");
                if ack_enabled {
                    for directive in directives {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
                continue;
            }
            if packet_id == play::clientbound::KEEP_ALIVE {
                // Answer the keep-alive through the seam so a long run isn't
                // dropped for timing out; identical in both directions.
                let directives = adapter
                    .handle_packet(&mut NullSink, state, packet_id, &payload)
                    .unwrap_or_default();
                for directive in directives {
                    if let Directive::Emit(ClientEvent::KeepAlive { id }) = directive
                        && let Some((pid, body)) = adapter
                            .encode_action(state, &ClientAction::KeepAliveResponse { id })
                            .expect("encode keep-alive response")
                    {
                        conn.write_packet(pid, &body).await.expect("keep-alive ack");
                    }
                }
                continue;
            }
        }

        // Everything else flows through the public seam; loosely-modelled play
        // packets may be unhandled and are tolerated.
        for directive in adapter
            .handle_packet(&mut NullSink, state, packet_id, &payload)
            .unwrap_or_default()
        {
            apply(&mut conn, &mut state, directive).await;
        }
    }

    let stream_span = match (first_batch_at, last_batch_at) {
        (Some(first), Some(last)) => last.saturating_duration_since(first),
        _ => Duration::ZERO,
    };
    FlowOutcome {
        reached_play,
        batches_finished,
        chunks,
        stream_span,
    }
}

/// The credit window the server ramps to after the first ACK
/// (`vanilla's own player chunk sender's own max unacknowledged batches`). Reaching more finished
/// batches than this is impossible without a continuously-flowing ACK loop.
const CREDIT_WINDOW: usize = 10;

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn chunk_delivery_does_not_stall_past_the_credit_window() {
    // Positive: with the adapter's ACK flushed, delivery must climb well past
    // the credit window. Stop promptly once we're comfortably past it.
    let target = CREDIT_WINDOW + 5;
    let positive = drive_flow(true, Some(target)).await;
    eprintln!(
        "positive (ack on):  reached_play={} batches={} chunks={} span={:?}",
        positive.reached_play, positive.batches_finished, positive.chunks, positive.stream_span,
    );
    assert!(
        positive.reached_play,
        "positive run never reached Play — the join itself failed"
    );
    assert!(
        positive.batches_finished > CREDIT_WINDOW,
        "with ACKs flowing, chunk delivery must continue past the {CREDIT_WINDOW}-batch credit \
         window, but only {} batches finished — the ACK loop is not unblocking the server",
        positive.batches_finished,
    );
    assert!(
        positive.chunks > CREDIT_WINDOW,
        "expected a real chunk stream once ACKs flow, saw only {} chunks",
        positive.chunks,
    );

    // Negative control: identical drive, but the `chunk_batch_received` Send is
    // dropped. The server must stall at the first (unacknowledged) batch and
    // stay stalled for the whole window. If this run *also* streamed many
    // batches, the positive assertion above would be proving nothing.
    let negative = drive_flow(false, None).await;
    eprintln!(
        "negative (ack off): reached_play={} batches={} chunks={} span={:?}",
        negative.reached_play, negative.batches_finished, negative.chunks, negative.stream_span,
    );
    assert!(
        negative.reached_play,
        "negative control never reached Play — the join itself failed, so the control proves \
         nothing about the ACK"
    );
    assert!(
        negative.batches_finished <= 1,
        "NEGATIVE CONTROL FAILED: with the ACK suppressed the server should stall after its \
         first unacknowledged batch, but {} batches finished over the full window. Either the \
         server no longer gates on ACKs or the driver is acking through another path — without \
         a working control the positive assertion is vacuous",
        negative.batches_finished,
    );

    // Tie them together: the ACK is the only difference, so it must be the cause
    // of the difference in delivery.
    assert!(
        positive.batches_finished > negative.batches_finished,
        "chunk delivery ({} batches with ACK) was not greater than the stalled control ({} \
         without) — the ACK made no observable difference",
        positive.batches_finished,
        negative.batches_finished,
    );
}
