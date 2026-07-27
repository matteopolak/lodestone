//! Live physics acceptance test (Phase 3 gate).
//!
//! **Scope — read before trusting a green result.** This is a *white-box,
//! version-crate* test: it hand-builds serverbound packets
//! (`move_player_pos_rot`, `accept_teleportation`, `player_loaded`, …) directly
//! against a raw [`Connection`] and the [`V770Adapter`], and drives the engine
//! itself ([`lodestone_physics::tick`]). It therefore validates **the physics
//! arithmetic** — that our per-tick positions match what vanilla re-simulates
//! closely enough that the server never corrects us — and **not the client
//! seam**. It does *not* establish that a bot can walk end-to-end, because the
//! movement never leaves the engine through the public `ClientHandle`/
//! `ClientAction::Move` API; it is constructed here by hand. A public-seam,
//! `ClientHandle`-driven version of this gate belongs in a crate that owns that
//! seam (`lodestone-client`/`lodestone-server`), not here: driving movement
//! through `ClientHandle` from this crate would both cycle the dependency graph
//! (`lodestone-client` depends on this crate via the registry) and violate the
//! isolation rule these tests live here to honour — they belong in v770
//! *because* they name v770's concrete packet types, and a client-seam test
//! names none (see `cargo run -p xtask -- check-isolation`).
//!
//! Gated behind the `live-physics` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-v770 --features live-physics --test live_physics -- --ignored --nocapture
//! ```
//!
//! # Why this gate is stronger than any offline oracle
//!
//! Every other physics check in this project is *self-authored*: the JVM
//! oracle, the Python golden generator and the Rust engine were all written by
//! the same hand from the same reading of the decompiled source, so they can
//! only catch transcription slips between the three ports — never a shared
//! misreading (see the `restituteMovementAfterCollisions` near-miss, plan
//! §12.31). This gate removes that ceiling: the **server itself** is the
//! authority. Vanilla runs its own movement validation
//! (`ServerGamePacketListenerImpl.handleMovePlayer`) and, whenever the position
//! a client reports disagrees with what the server re-simulates, it sends a
//! `player_position` (a *corrective teleport*) to snap the client back. So
//! "we walked for N ticks and received zero corrective teleports" is the server
//! asserting our physics matches its own — an assertion no self-authored oracle
//! can fake.
//!
//! # What it does
//!
//! 1. Joins via the public [`VersionAdapter`] seam, reaches `Play`, and adopts
//!    the server's absolute spawn position from the initial (expected)
//!    `player_position`, ACKing it with `accept_teleportation`.
//! 2. Sends `player_loaded` so the server actually *validates* our movement.
//!    (Without it, `hasClientLoaded()` stays false for 60 ticks and the server
//!    silently ignores movement instead of validating it — a green result that
//!    proves nothing. This gate makes the server do the work.)
//! 3. Drives [`lodestone_physics::tick`] on a synthetic flat-ground
//!    [`CollisionView`] seeded at the exact spawn Y, walking forward, and sends
//!    `move_player_pos_rot` at vanilla's own send cadence
//!    (`lengthSqr(delta) > (2e-4)²` OR `positionReminder >= 20`).
//! 4. Asserts **zero** corrective `player_position` packets arrive after the
//!    spawn sync. Any correction is reported with its magnitude — divergence is
//!    the valuable result, so we surface it rather than hide it.
//!
//! The world interface stays a synthetic flat ground rather than the decoded
//! live chunks on purpose: the flat 26.2 test world is uniform near spawn, so a
//! solid plane at the spawn Y is faithful, and it keeps this gate testing the
//! *engine* (does our arithmetic match the server's) rather than the chunk
//! decoder (already covered by `live_chunk`).
#![cfg(feature = "live-physics")]

use std::time::{Duration, Instant};

use lodestone_core::{Reader, Writer};
use lodestone_model::{ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter};
use lodestone_net::Connection;
use lodestone_physics::{Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d};
use lodestone_testsupport::RconClient;
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use tokio::net::TcpStream;
use uuid::Uuid;

mod common;
use common::unique_username;

/// A `WorldSink` that discards chunk state. The physics gate never inspects
/// decoded chunks (it walks on a synthetic flat ground), so the adapter's
/// world-application seam is satisfied with a no-op. Chunk *decoding* itself is
/// covered by `live_chunk`; here we only need the adapter to accept play
/// packets (keep-alive, etc.) without owning a real `World`.
struct NullSink;

impl lodestone_world::WorldSink for NullSink {
    fn load(&mut self, _pos: lodestone_world::ChunkPos, _chunk: lodestone_world::LoadedChunk) {}
    fn merge(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(&mut self, _sx: i32, _sy: i32, _sz: i32, _blocks: &[(u8, u8, u8, u32)]) {}
    fn merge_light(
        &mut self,
        _pos: lodestone_world::ChunkPos,
        _patch: lodestone_world::LightPatch,
    ) {
    }
    fn unload(&mut self, _pos: lodestone_world::ChunkPos) {}
}

/// A synthetic, uniformly flat world: every cell whose *top* is at or below
/// `top` is a full solid cube, everything above is air. Seeded with `top` equal
/// to the server's reported spawn Y so the player rests exactly where the
/// server placed it — any mismatch would itself show up as a corrective
/// teleport, which is precisely what this gate measures.
struct FlatGround {
    top: f64,
}

impl CollisionView for FlatGround {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        // Cell (x,y,z) spans [y, y+1]; it is solid iff its top (y+1) is not
        // above the ground plane. The 1e-6 slack absorbs the integer spawn Y
        // round-trip without ever making the resting cell itself solid.
        if f64::from(y) + 1.0 <= self.top + 1e-6 {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
    // friction/speed/jump factors all take the trait defaults (0.6 friction,
    // 1.0 factors), matching the flat world's grass/dirt surface.
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
/// big-endian doubles) of a clientbound `player_position`. The trailing
/// deltaMovement / rotation / relatives are irrelevant to this gate — we adopt
/// the position and ACK the id.
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

/// Sends a `move_player_pos_rot`: three doubles, yaw+pitch floats, then a flags
/// byte (bit0 = on_ground, bit1 = horizontal_collision), exactly as vanilla's
/// `LocalPlayer.sendPosition` does when both position and rotation are dirty.
async fn send_move(
    conn: &mut Connection<TcpStream>,
    pos: Vec3d,
    yaw: f32,
    pitch: f32,
    on_ground: bool,
    horizontal_collision: bool,
) {
    let mut w = Writer::default();
    w.f64(pos.x);
    w.f64(pos.y);
    w.f64(pos.z);
    w.f32(yaw);
    w.f32(pitch);
    let mut flags = 0u8;
    if on_ground {
        flags |= 1;
    }
    if horizontal_collision {
        flags |= 2;
    }
    w.u8(flags);
    conn.write_packet(play::serverbound::MOVE_PLAYER_POS_ROT, &w.into_vec())
        .await
        .expect("move player");
}

/// Outcome of the shared join sequence: a connection sitting in `Play`, synced
/// to the server's spawn and marked loaded, ready to be driven.
struct Joined {
    conn: Connection<TcpStream>,
    adapter: V770Adapter,
    state: ConnectionState,
    spawn: [f64; 3],
    health: Option<f32>,
    chunks: usize,
    /// The offline-mode username we joined as. Needed to target the player from
    /// RCON console commands (`item replace entity <name> …`, `tp <name> …`) in
    /// the elytra gate, which has to equip an elytra and get airborne before the
    /// server will accept `START_FALL_FLYING`.
    username: String,
}

/// Joins the live server, reaches `Play`, adopts + ACKs the spawn teleport,
/// drains the join-time settle window, then sends `player_loaded` so the server
/// actually validates subsequent movement. Shared by both the parity gate and
/// its negative control so they exercise an identical join path.
async fn join_and_load(_prefix: &str) -> Joined {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        // Unique per run: offline mode derives the UUID from the *name*, so a
        // shared name shares one persisted player file; an inherited dead one
        // gives a silent chunk blackout (dump set_health == 0.0 to spot it).
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let username = profile.username.clone();
    let adapter = V770Adapter::new();

    let mut conn = Connection::connect("127.0.0.1:25565")
        .await
        .unwrap_or_else(|e| {
            panic!(
                "cannot reach the live 26.2 server on 127.0.0.1:25565 ({e}). \
                 This #[ignore]d gate REQUIRES it — start the `lodestone-mc262` \
                 container (offline mode, flat world) and re-run. A missing server \
                 is a FAILURE here, never a skip."
            )
        });
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut reached_play = false;
    let mut spawn: Option<[f64; 3]> = None;
    let mut health: Option<f32> = None;
    let mut chunks = 0usize;
    let mut synced_at: Option<Instant> = None;

    let join_deadline = Instant::now() + Duration::from_secs(45);
    while Instant::now() < join_deadline {
        // Proceed once we're synced, in Play, have some chunks, and a short
        // settle window has passed to drain any join-time teleport burst.
        if let Some(t) = synced_at
            && reached_play
            && chunks >= 1
            && t.elapsed() >= Duration::from_millis(500)
        {
            break;
        }

        let read = tokio::time::timeout(Duration::from_secs(5), conn.read_packet()).await;
        let (packet_id, payload) = match read {
            Err(_) => panic!(
                "server on 127.0.0.1:25565 went quiet for 5s during join before we synced — \
                 is `lodestone-mc262` healthy? (this is a FAILURE, not a skip)"
            ),
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => panic!("clean EOF during join — server closed the connection"),
            Ok(Err(err)) => panic!("read error during join: {err}"),
        };

        if state == ConnectionState::Play && !reached_play {
            reached_play = true;
            eprintln!("reached Play");
        }

        if state == ConnectionState::Play {
            if packet_id == play::clientbound::PLAYER_POSITION {
                let (id, pos) = decode_teleport(&payload);
                accept_teleport(&mut conn, id).await;
                spawn = Some(pos);
                synced_at = Some(Instant::now());
                eprintln!("spawn sync: teleport id={id} pos={pos:?}");
                continue;
            }
            if packet_id == play::clientbound::SET_HEALTH {
                let mut r = Reader::new(&payload);
                let h = r.f32().expect("health");
                health = Some(h);
                continue;
            }
            if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                let mut w = Writer::default();
                w.f32(32.0);
                conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
                    .await
                    .expect("ack chunk batch");
                continue;
            }
            if packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT {
                chunks += 1;
            }
        }

        // Everything else (incl. keep-alive, which the adapter answers) goes
        // through the public seam; loosely-modelled play packets may be
        // unhandled and are tolerated.
        for directive in adapter
            .handle_packet(&mut NullSink, state, packet_id, &payload)
            .unwrap_or_default()
        {
            apply(&mut conn, &mut state, directive).await;
        }
    }

    if let Some(h) = health {
        assert!(
            h > 0.0,
            "spawned with set_health={h} — an inherited dead player (chunk blackout); \
             the unique username should prevent this"
        );
    }
    let spawn = spawn.expect(
        "never received a spawn teleport / reached Play — the server did not complete the \
         join handshake (FAILURE, not a skip; check `lodestone-mc262`)",
    );
    assert!(chunks >= 1, "received no chunks before proceeding");

    // Tell the server we're loaded so it *validates* our movement rather than
    // silently ignoring it for the first 60 ticks. Without this, "zero
    // corrections" would be vacuous — the server would simply be discarding our
    // movement. `assert_server_corrects` (called by the gate before it walks)
    // proves this switch is actually effective.
    conn.write_packet(play::serverbound::PLAYER_LOADED, &[])
        .await
        .expect("player loaded");

    Joined {
        conn,
        adapter,
        state,
        spawn,
        health,
        chunks,
        username,
    }
}

/// Anti-vacuity guard. Sends one deliberately impossible move — a single
/// ~30-block horizontal jump, far over vanilla's `moved too quickly` threshold
/// (`movedDist - expectedDist > 100` per packet) — and asserts the server
/// **does** snap us back with a corrective `player_position` landing near
/// spawn. Returns the snap-back position (which becomes our resynced origin).
///
/// This exists because the parity gate asserts the *absence* of corrections,
/// and an absence proves nothing unless the server is demonstrably issuing them
/// when it should. Calling this immediately before the clean walk makes "zero
/// corrections" impossible to report as success unless the server is provably
/// validating our movement this very session — so a down/ignoring/misconfigured
/// server fails here, loudly, instead of yielding a green walk that asserted
/// nothing. Poll (never assert immediately): the server reacts on its next tick.
async fn assert_server_corrects(
    conn: &mut Connection<TcpStream>,
    adapter: &V770Adapter,
    mut state: ConnectionState,
    spawn: [f64; 3],
) -> [f64; 3] {
    let bad = Vec3d::new(spawn[0] + 30.0, spawn[1], spawn[2]);
    send_move(conn, bad, 0.0, 0.0, true, false).await;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), conn.read_packet()).await {
            Ok(Ok(Some((packet_id, payload)))) => {
                if packet_id == play::clientbound::PLAYER_POSITION {
                    let (id, pos) = decode_teleport(&payload);
                    accept_teleport(conn, id).await;
                    let dx = pos[0] - spawn[0];
                    let dz = pos[2] - spawn[2];
                    let back = (dx * dx + dz * dz).sqrt();
                    assert!(
                        back < 5.0,
                        "server correction landed {back:.3} blocks from spawn — expected a \
                         snap-back near spawn after an impossible move"
                    );
                    eprintln!("teeth check: server corrected the 30-block move to {pos:?}");
                    return pos;
                }
                if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                    let mut w = Writer::default();
                    w.f32(32.0);
                    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
                        .await
                        .expect("ack chunk batch");
                    continue;
                }
                for directive in adapter
                    .handle_packet(&mut NullSink, state, packet_id, &payload)
                    .unwrap_or_default()
                {
                    apply(conn, &mut state, directive).await;
                }
            }
            Ok(Ok(None)) => panic!("clean EOF while awaiting the teeth-check correction"),
            Ok(Err(err)) => panic!("read error while awaiting the teeth-check correction: {err}"),
            Err(_) => {}
        }
    }
    panic!(
        "server did NOT correct a 30-block teleport within 5s — it is not validating our \
         movement, so the walk's 'zero corrections' would be a FALSE GREEN. Check that \
         `lodestone-mc262` is up and that player_loaded was accepted."
    )
}

/// The gate: drive a real player walking on flat ground on the live 26.2
/// server and assert the server issues **zero** corrective `player_position`
/// packets. Vanilla resynchronises any client whose reported position it
/// disagrees with, so an absence of corrections is the server itself asserting
/// our physics matches — a claim no self-authored oracle can fake.
#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn server_does_not_correct_a_walking_player() {
    const WALK_TICKS: u32 = 100; // 5 s at 20 TPS.
    const YAW: f32 = 0.0; // yaw 0 faces +Z; forward input walks +Z.
    const PITCH: f32 = 0.0;

    let Joined {
        mut conn,
        adapter,
        mut state,
        spawn,
        health,
        chunks,
        ..
    } = join_and_load("PhysGate").await;

    // ---- Anti-vacuity guard: prove the server is validating us THIS session
    // before we trust an absence-of-corrections result. If it isn't (down,
    // ignoring, player_loaded not honoured), this fails loudly instead of
    // letting the walk below pass while asserting nothing. We resync to the
    // snap-back position it returns and walk from there. ----
    let origin = assert_server_corrects(&mut conn, &adapter, state, spawn).await;

    // ---- Phase B: walk, and watch for corrective teleports ----
    let ground_top = origin[1];
    let view = FlatGround { top: ground_top };
    let profile = PhysicsProfile::mc_1_21();
    let mut player = PlayerState::at(Vec3d::new(origin[0], origin[1], origin[2]), YAW);
    player.pitch = PITCH;

    let walk = MovementInput {
        forward: 1.0,
        strafe: 0.0,
        jump: false,
        sneak: false,
        sprint: false,
    };

    // Vanilla's send-cadence state (LocalPlayer.sendPosition).
    let mut last_sent = player.position;
    let mut position_reminder: u32 = 0;

    let mut corrections: Vec<(i32, [f64; 3], f64)> = Vec::new();
    let mut ticks_done: u32 = 0;
    let mut packets_sent: u32 = 0;
    let tick_period = Duration::from_millis(50);
    let mut next_tick = Instant::now();

    let walk_deadline = Instant::now() + Duration::from_secs(15);
    while ticks_done < WALK_TICKS && Instant::now() < walk_deadline {
        let now = Instant::now();
        let until_tick = next_tick.saturating_duration_since(now);
        // Read (draining server traffic) but never past the next tick boundary.
        let read_budget = until_tick.min(Duration::from_millis(20));

        match tokio::time::timeout(read_budget, conn.read_packet()).await {
            Ok(Ok(Some((packet_id, payload)))) => {
                if packet_id == play::clientbound::PLAYER_POSITION {
                    // A teleport *after* we started walking is a correction.
                    let (id, pos) = decode_teleport(&payload);
                    accept_teleport(&mut conn, id).await;
                    let dx = pos[0] - player.position.x;
                    let dy = pos[1] - player.position.y;
                    let dz = pos[2] - player.position.z;
                    let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                    corrections.push((id, pos, dist));
                    eprintln!(
                        "CORRECTION tick={ticks_done} id={id} to={pos:?} \
                         (moved us {dist:.6} from predicted)"
                    );
                    // Adopt the corrected position so we don't cascade further.
                    player.position = Vec3d::new(pos[0], pos[1], pos[2]);
                    player.velocity = Vec3d::ZERO;
                    last_sent = player.position;
                } else if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                    let mut w = Writer::default();
                    w.f32(32.0);
                    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
                        .await
                        .expect("ack chunk batch");
                } else {
                    for directive in adapter
                        .handle_packet(&mut NullSink, state, packet_id, &payload)
                        .unwrap_or_default()
                    {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
            }
            Ok(Ok(None)) => panic!("clean EOF while walking"),
            Ok(Err(err)) => panic!("read error while walking: {err}"),
            Err(_) => {} // read budget elapsed: fall through to the tick.
        }

        if Instant::now() < next_tick {
            continue;
        }
        next_tick += tick_period;

        // Step the engine one tick, then reproduce vanilla's send cadence.
        lodestone_physics::tick(&mut player, walk, &view, &profile);
        ticks_done += 1;

        let dx = player.position.x - last_sent.x;
        let dy = player.position.y - last_sent.y;
        let dz = player.position.z - last_sent.z;
        position_reminder += 1;
        let moved = (dx * dx + dy * dy + dz * dz) > (2.0e-4 * 2.0e-4);
        if moved || position_reminder >= 20 {
            send_move(
                &mut conn,
                player.position,
                player.yaw,
                player.pitch,
                player.on_ground,
                player.horizontal_collision,
            )
            .await;
            packets_sent += 1;
            if moved {
                last_sent = player.position;
                position_reminder = 0;
            }
        }
    }

    // ---- Report ----
    let end = player.position;
    let walked = {
        let dx = end.x - origin[0];
        let dz = end.z - origin[2];
        (dx * dx + dz * dz).sqrt()
    };
    eprintln!("\n=== LIVE PHYSICS GATE REPORT ===");
    eprintln!("spawn position         : {spawn:?}");
    eprintln!("walk origin (resynced) : {origin:?}");
    eprintln!(
        "end position           : [{:.5}, {:.5}, {:.5}]",
        end.x, end.y, end.z
    );
    eprintln!("ticks simulated        : {ticks_done}");
    eprintln!("horizontal distance    : {walked:.4} blocks");
    eprintln!("move packets sent      : {packets_sent}");
    eprintln!("chunks received        : {chunks}");
    eprintln!("set_health at spawn    : {health:?}");
    eprintln!("corrective teleports   : {}", corrections.len());
    for (id, pos, dist) in &corrections {
        eprintln!("  - id={id} to={pos:?} (Δ {dist:.6})");
    }
    eprintln!("================================\n");

    assert!(
        ticks_done >= WALK_TICKS,
        "walk loop timed out after {ticks_done} ticks — server stalled?"
    );
    assert!(
        walked > 1.0,
        "player barely moved ({walked:.4} blocks) — walk input was not applied"
    );
    assert_eq!(
        corrections.len(),
        0,
        "server issued {} corrective teleport(s) — our physics diverged from vanilla",
        corrections.len()
    );
}

/// Negative control, kept as a focused, independently-runnable teeth check.
///
/// The parity gate already embeds [`assert_server_corrects`] before it walks,
/// so it can never report "zero corrections" unless the server is provably
/// validating us this session. This standalone test documents that mechanism in
/// isolation and asserts the same property via the shared helper, so a reader
/// can run just this one to confirm the server corrects an impossible move.
#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn server_corrects_an_impossible_move() {
    let Joined {
        mut conn,
        adapter,
        state,
        spawn,
        ..
    } = join_and_load("PhysBad").await;

    // Panics if the server does not snap us back near spawn.
    let pos = assert_server_corrects(&mut conn, &adapter, state, spawn).await;
    eprintln!("negative control: server corrected an impossible move to {pos:?}");
}

// ---------------------------------------------------------------------------
// Elytra live gate
// ---------------------------------------------------------------------------
//
// Elytra flight is where client and server prediction diverge most visibly in
// real gameplay, and its rotation-to-thrust coupling is exactly the sort of
// formulation where a self-authored oracle can silently encode the author's own
// misreading (the §12.31 lesson). So the strongest possible evidence that our
// `tick_elytra` matches vanilla is not another oracle agreeing with us — it is
// the *real server* declining to correct a multi-second glide.
//
// Reaching a gliding state on a survival server needs server-side setup the
// client cannot do itself: the player must be wearing an elytra and be airborne
// before the server will honour `START_FALL_FLYING`. We do that over RCON (the
// server console runs at permission level 4), then drive the flight with the
// same engine the walk gate uses.

/// Sends `serverbound player_command(START_FALL_FLYING)`. The server ignores the
/// packet's entity-id field for player commands (it always targets the sending
/// player), so we send id 0. `writeEnum` serialises the ordinal as a VarInt;
/// in 26.2 `START_FALL_FLYING` is ordinal 6.
async fn send_start_fall_flying(conn: &mut Connection<TcpStream>) {
    const START_FALL_FLYING_ORDINAL: i32 = 6;
    let mut w = Writer::default();
    w.var_i32(0); // entity id (ignored by the server for player commands)
    w.var_i32(START_FALL_FLYING_ORDINAL);
    w.var_i32(0); // data
    conn.write_packet(play::serverbound::PLAYER_COMMAND, &w.into_vec())
        .await
        .expect("send START_FALL_FLYING");
}

/// In-flight negative control. Sends one impossible move (30 blocks horizontally
/// from `from`) and asserts the server issues a corrective teleport, proving the
/// detector is armed **in the fall-flying regime specifically** — the walk gate's
/// on-ground control cannot vouch for a different server code path. Returns the
/// corrected position so the caller can resync.
async fn assert_corrects_in_flight(
    conn: &mut Connection<TcpStream>,
    adapter: &V770Adapter,
    mut state: ConnectionState,
    from: Vec3d,
) -> [f64; 3] {
    let bad = Vec3d::new(from.x + 30.0, from.y, from.z);
    send_move(conn, bad, 0.0, 0.0, false, false).await;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), conn.read_packet()).await {
            Ok(Ok(Some((packet_id, payload)))) => {
                if packet_id == play::clientbound::PLAYER_POSITION {
                    let (id, pos) = decode_teleport(&payload);
                    accept_teleport(conn, id).await;
                    let dx = pos[0] - bad.x;
                    let dz = pos[2] - bad.z;
                    let pulled_back = (dx * dx + dz * dz).sqrt();
                    assert!(
                        pulled_back > 20.0,
                        "in-flight correction only moved us {pulled_back:.3} blocks — expected a \
                         large snap-back after a 30-block teleport"
                    );
                    eprintln!(
                        "in-flight teeth check: server corrected the 30-block move to {pos:?}"
                    );
                    return pos;
                }
                if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                    let mut w = Writer::default();
                    w.f32(32.0);
                    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
                        .await
                        .expect("ack chunk batch");
                    continue;
                }
                for directive in adapter
                    .handle_packet(&mut NullSink, state, packet_id, &payload)
                    .unwrap_or_default()
                {
                    apply(conn, &mut state, directive).await;
                }
            }
            Ok(Ok(None)) => panic!("clean EOF while awaiting the in-flight teeth-check correction"),
            Ok(Err(err)) => panic!("read error while awaiting the in-flight correction: {err}"),
            Err(_) => {}
        }
    }
    panic!(
        "server did NOT correct a 30-block in-flight teleport within 5s — it is not validating \
         fall-flying movement, so the glide's 'zero corrections' would be a FALSE GREEN."
    )
}

/// The elytra gate: equip an elytra and lift the player into the air over RCON,
/// start fall-flying, then drive a real glide with our engine on the live 26.2
/// server and assert the server issues **zero** corrective `player_position`
/// packets. Because 26.2 removed the `disableElytraMovementCheck` gamerule, the
/// server validates elytra movement unconditionally — so an absence of
/// corrections is the server itself asserting `tick_elytra` matches vanilla.
#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565 + RCON on 25575"]
async fn server_does_not_correct_an_elytra_flight() {
    const GLIDE_TICKS: u32 = 60; // 3 s at 20 TPS.
    const YAW: f32 = 0.0; // yaw 0 → forward is +Z.
    const PITCH: f32 = 0.0; // level look: matches the golden `elytra_glide_level`.
    const LIFT: f64 = 60.0; // blocks to hoist the player before the glide.

    let password = std::env::var("LODESTONE_MC262_RCON").unwrap_or_else(|_| {
        panic!(
            "this #[ignore]d elytra gate needs RCON to equip an elytra and get airborne. \
             Set LODESTONE_MC262_RCON to lodestone-mc262's rcon.password \
             (see /w/server.properties inside the container). A missing password is a \
             FAILURE here, never a skip."
        )
    });
    // mc262's RCON port (25575) is container-internal and NOT published to the
    // host, so point this at a proxy that reaches it, e.g. a socat sidecar on the
    // bridge network:
    //   docker run --rm -d --name els-rcon-proxy --network bridge -p 25599:25599 \
    //     alpine/socat TCP-LISTEN:25599,fork,reuseaddr TCP:<mc262-ip>:25575
    // then run with LODESTONE_MC262_RCON_ADDR=127.0.0.1:25599. Defaults to the
    // conventional host RCON port. (Do NOT point this at :25575 on this box — that
    // is `lodestone-entity-oracle`, a different server that will show 0 players.)
    let rcon_addr = std::env::var("LODESTONE_MC262_RCON_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:25575".to_string());

    let Joined {
        mut conn,
        adapter,
        mut state,
        spawn,
        health,
        chunks,
        username,
    } = join_and_load("ElytraGate").await;

    // ---- Session-level control: prove the server validates us at all before we
    // trust any absence-of-corrections result (same instinct as the walk gate). ----
    let origin = assert_server_corrects(&mut conn, &adapter, state, spawn).await;

    // ---- Setup over RCON: equip an elytra and hoist the player into the air.
    // RCON runs at the console permission level, so no op is needed. ----
    let target_y = origin[1] + LIFT;
    let commands = vec![
        format!("item replace entity {username} armor.chest with minecraft:elytra"),
        format!("tp {username} {} {} {}", origin[0], target_y, origin[2]),
    ];
    let rcon_password = password.clone();
    let rcon_addr_owned = rcon_addr.clone();
    let rcon_out = tokio::task::spawn_blocking(move || {
        let mut rcon = RconClient::connect(&rcon_addr_owned, &rcon_password)?;
        rcon.commands(&commands)
    })
    .await
    .expect("rcon task join")
    .expect("rcon setup (equip elytra + tp up) failed");
    for line in &rcon_out {
        eprintln!("rcon: {line}");
    }
    assert!(
        !rcon_out.iter().any(|l| l.contains("No entity was found")),
        "RCON could not find player {username} on the server it reached via {rcon_addr} — the \
         address is pointing at the wrong server (entity-oracle shows 0 players). Point \
         LODESTONE_MC262_RCON_ADDR at a proxy to mc262's own RCON."
    );

    // ---- Adopt the teleport the `tp` produced. Poll (never assert immediately):
    // the command lands on the server's next tick, then it teleports us. ----
    let mut flight_origin: Option<[f64; 3]> = None;
    let tp_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < tp_deadline {
        match tokio::time::timeout(Duration::from_millis(500), conn.read_packet()).await {
            Ok(Ok(Some((packet_id, payload)))) => {
                if packet_id == play::clientbound::PLAYER_POSITION {
                    let (id, pos) = decode_teleport(&payload);
                    accept_teleport(&mut conn, id).await;
                    // Only accept the teleport that actually lifted us near target_y;
                    // ignore any earlier settling teleport at ~origin height.
                    if (pos[1] - target_y).abs() < 3.0 {
                        flight_origin = Some(pos);
                        eprintln!("airborne: tp teleport id={id} pos={pos:?}");
                        break;
                    }
                } else if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                    let mut w = Writer::default();
                    w.f32(32.0);
                    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
                        .await
                        .expect("ack chunk batch");
                } else {
                    for directive in adapter
                        .handle_packet(&mut NullSink, state, packet_id, &payload)
                        .unwrap_or_default()
                    {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
            }
            Ok(Ok(None)) => panic!("clean EOF while waiting for the lift teleport"),
            Ok(Err(err)) => panic!("read error while waiting for the lift teleport: {err}"),
            Err(_) => {}
        }
    }
    let flight_origin = flight_origin.expect(
        "never received the lift teleport near target Y — did the `tp` RCON command run? \
         (FAILURE, not a skip)",
    );

    // ---- Start fall-flying. The server only honours this while we are airborne
    // and wearing an elytra, both of which the RCON setup just arranged. ----
    send_start_fall_flying(&mut conn).await;

    // ---- Phase F: glide, and watch for corrective teleports. ----
    let view = FlatGround { top: origin[1] }; // solid ground far below; no contact mid-flight.
    let profile = PhysicsProfile::mc_1_21();
    let mut player = PlayerState::at(
        Vec3d::new(flight_origin[0], flight_origin[1], flight_origin[2]),
        YAW,
    );
    player.pitch = PITCH;
    player.fall_flying = true;
    player.on_ground = false;

    let glide = MovementInput {
        forward: 0.0, // elytra ignores WASD thrust; direction is pure look-angle.
        strafe: 0.0,
        jump: false,
        sneak: false,
        sprint: false,
    };

    let mut last_sent = player.position;
    let mut position_reminder: u32 = 0;
    let mut corrections: Vec<(i32, [f64; 3], f64)> = Vec::new();
    let mut ticks_done: u32 = 0;
    let mut packets_sent: u32 = 0;
    let tick_period = Duration::from_millis(50);
    let mut next_tick = Instant::now();

    let glide_deadline = Instant::now() + Duration::from_secs(12);
    while ticks_done < GLIDE_TICKS && Instant::now() < glide_deadline {
        let now = Instant::now();
        let until_tick = next_tick.saturating_duration_since(now);
        let read_budget = until_tick.min(Duration::from_millis(20));

        match tokio::time::timeout(read_budget, conn.read_packet()).await {
            Ok(Ok(Some((packet_id, payload)))) => {
                if packet_id == play::clientbound::PLAYER_POSITION {
                    let (id, pos) = decode_teleport(&payload);
                    accept_teleport(&mut conn, id).await;
                    let dx = pos[0] - player.position.x;
                    let dy = pos[1] - player.position.y;
                    let dz = pos[2] - player.position.z;
                    let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                    corrections.push((id, pos, dist));
                    eprintln!(
                        "CORRECTION tick={ticks_done} id={id} to={pos:?} \
                         (moved us {dist:.6} from predicted)"
                    );
                    player.position = Vec3d::new(pos[0], pos[1], pos[2]);
                    player.velocity = Vec3d::ZERO;
                    last_sent = player.position;
                } else if packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                    let mut w = Writer::default();
                    w.f32(32.0);
                    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
                        .await
                        .expect("ack chunk batch");
                } else {
                    for directive in adapter
                        .handle_packet(&mut NullSink, state, packet_id, &payload)
                        .unwrap_or_default()
                    {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
            }
            Ok(Ok(None)) => panic!("clean EOF while gliding"),
            Ok(Err(err)) => panic!("read error while gliding: {err}"),
            Err(_) => {}
        }

        if Instant::now() < next_tick {
            continue;
        }
        next_tick += tick_period;

        lodestone_physics::tick(&mut player, glide, &view, &profile);
        ticks_done += 1;

        let dx = player.position.x - last_sent.x;
        let dy = player.position.y - last_sent.y;
        let dz = player.position.z - last_sent.z;
        position_reminder += 1;
        let moved = (dx * dx + dy * dy + dz * dz) > (2.0e-4 * 2.0e-4);
        if moved || position_reminder >= 20 {
            send_move(
                &mut conn,
                player.position,
                player.yaw,
                player.pitch,
                player.on_ground,
                player.horizontal_collision,
            )
            .await;
            packets_sent += 1;
            if moved {
                last_sent = player.position;
                position_reminder = 0;
            }
        }
    }

    let end = player.position;
    let travelled = {
        let dx = end.x - flight_origin[0];
        let dz = end.z - flight_origin[2];
        (dx * dx + dz * dz).sqrt()
    };
    let descended = flight_origin[1] - end.y;

    // ---- Regime-matched negative control: prove the detector is still armed
    // for fall-flying movement, so the zero above is not a false green. ----
    assert_corrects_in_flight(&mut conn, &adapter, state, player.position).await;

    eprintln!("\n=== LIVE ELYTRA GATE REPORT ===");
    eprintln!("spawn position         : {spawn:?}");
    eprintln!("flight origin (airborne): {flight_origin:?}");
    eprintln!(
        "end position           : [{:.5}, {:.5}, {:.5}]",
        end.x, end.y, end.z
    );
    eprintln!("ticks simulated        : {ticks_done}");
    eprintln!("forward distance       : {travelled:.4} blocks");
    eprintln!("descent                : {descended:.4} blocks");
    eprintln!("move packets sent      : {packets_sent}");
    eprintln!("chunks received        : {chunks}");
    eprintln!("set_health at spawn    : {health:?}");
    eprintln!("corrective teleports   : {}", corrections.len());
    for (id, pos, dist) in &corrections {
        eprintln!("  - id={id} to={pos:?} (Δ {dist:.6})");
    }
    eprintln!("================================\n");

    assert!(
        ticks_done >= GLIDE_TICKS,
        "glide loop timed out after {ticks_done} ticks — server stalled?"
    );
    // Anti-vacuity: a real glide must move forward AND descend. A dead/level
    // integrator that reported the spawn point every tick would pass "zero
    // corrections" trivially; requiring several blocks of travel and descent
    // makes the absence-of-corrections claim mean something.
    assert!(
        travelled > 3.0,
        "player barely moved forward ({travelled:.4} blocks) — elytra thrust was not applied"
    );
    assert!(
        descended > 1.0,
        "player did not descend ({descended:.4} blocks) — gravity/lift was not applied"
    );
    assert_eq!(
        corrections.len(),
        0,
        "server issued {} corrective teleport(s) during the glide — our elytra physics diverged \
         from vanilla",
        corrections.len()
    );
}
