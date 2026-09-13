//! Live **`LevelEvent` 2001 (`PARTICLES_DESTROY_BLOCK`)** oracle — the wire
//! evidence behind "break-particle debris takes the right block's texture".
//!
//! # Why this gate exists
//!
//! Break particles resolve their sprite from a **block state id** carried in the
//! `data` field of `level_event` 2001. Everything downstream of that id — the
//! shell's `NetUpdate::BlockDestroyed`, `Particles::destroy_block`, the
//! per-state `#particle` table — is only as correct as the id, and *nothing in
//! this repo can validate the id against itself*: `decode(encode(x)) == x`
//! passes for two symmetric misunderstandings of the field, and a fixture built
//! with our own encoder (`world_events.rs`'s `level_event_bytes`) proves only
//! that we agree with ourselves.
//!
//! So the expected value here comes from **two places outside the code under
//! test**:
//!
//! 1. the real vanilla 26.2 server's own bytes, captured off the socket; and
//! 2. `crate::block_states`' generated census of `vanilla's own block's own block state registry`
//!    (32,366 states, dumped from the real jar) for what `minecraft:torch`'s
//!    state id actually *is*.
//!
//! # The cascade case, which is the one that matters
//!
//! A user reported that "if I break a block that causes another to break, the
//! particles for the other block are white". A block the player punches has a
//! *second* particle emitter behind it (`Particles::breaking_block`, one
//! fragment per mining hit, driven from the client's own world read), so a wrong
//! 2001 id there is masked by correct-looking debris in the same cell. A
//! **cascading** break — a torch losing its support — has no local prediction
//! at all: 2001 is the only emitter, so it is the only place the raw wire value
//! is visible on screen.
//!
//! This gate therefore triggers a cascade rather than a punch: `setblock` the
//! supporting block to air and let vanilla's `vanilla's own block's own update or destroy` →
//! `vanilla's own level's own destroy block` path emit the torch's 2001 itself.
//!
//! Full invocation (all three parts are required):
//!
//! ```text
//! cargo test -p lodestone-v26-2 --features live-destroy-block \
//!     --test live_destroy_block_event -- --ignored --nocapture
//! ```
//!
//! * Without `--features live-destroy-block` the file compiles to nothing and
//!   the run prints `ok. 0 passed`, which reads exactly like success.
//! * Without `--ignored` the test is skipped.
//! * It targets the **flat creative 26.2 oracle** (game `:25570`, RCON `:25571`,
//!   password `lodestone`; `scripts/live-oracles/creative.sh`). An unreachable
//!   oracle **fails**, never skips (§12.52).
#![cfg(feature = "live-destroy-block")]

use std::time::{Duration, Instant};

use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LevelEventData, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::RconClient;
use lodestone_v26_2::V770Adapter;
use lodestone_data::block_states;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

#[path = "../common/mod.rs"]
mod common;
use common::unique_username;

/// The flat creative 26.2 oracle: game on `:25570`, RCON on `:25571`.
const GAME_ADDR: &str = "127.0.0.1:25570";
const RCON_ADDR: &str = "127.0.0.1:25571";
const RCON_PASSWORD: &str = "lodestone";

/// Vanilla's `vanilla's own level event's own particles destroy block`.
const PARTICLES_DESTROY_BLOCK: i32 = 2001;

/// The first state id of `block`, straight from the generated census of
/// `vanilla's own block's own block state registry` — the id space the wire uses.
fn first_state_of(block: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(block))
        .unwrap_or_else(|| panic!("{block} is not in the 26.2 block-state census"))
}

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
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string())
        }
        _ => {}
    }
}

async fn answer_keep_alive(
    conn: &mut Connection<TcpStream>,
    state: ConnectionState,
    adapter: &V770Adapter,
    id: i64,
) {
    if let Ok(Some((packet_id, payload))) =
        adapter.encode_action(state, &ClientAction::KeepAliveResponse { id })
    {
        conn.write_packet(packet_id, &payload)
            .await
            .expect("write keep-alive response");
    }
}

/// Parse a `data get entity ... Pos` RCON response's `[x, y, z]` list.
fn parse_list3(resp: &str) -> Option<(f64, f64, f64)> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let inner = &resp[open + 1..close];
    let nums: Vec<f64> = inner
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    (nums.len() == 3).then(|| (nums[0], nums[1], nums[2]))
}

/// One captured `level_event` frame: the raw payload plus the canonical event
/// the adapter produced from it.
#[derive(Debug)]
struct Captured {
    raw: Vec<u8>,
    event: i32,
    pos: (i32, i32, i32),
    data: i32,
}

/// Pumps packets, answering keep-alive, recording every `level_event` frame,
/// until `deadline`.
async fn pump_capturing(
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    adapter: &V770Adapter,
    world: &mut World,
    deadline: Instant,
    read_timeout: Duration,
    out: &mut Vec<Captured>,
) {
    while Instant::now() < deadline {
        let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
        let (packet_id, payload) = match read {
            Err(_) => continue,
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("read error: {err}"),
        };
        let is_level_event =
            *state == ConnectionState::Play && packet_id == play::clientbound::LEVEL_EVENT;
        let raw = payload.clone();
        let directives = adapter
            .handle_packet(world, *state, packet_id, &payload)
            .unwrap_or_else(|e| {
                if is_level_event {
                    panic!("real level_event must decode with zero trailing bytes: {e}");
                }
                Vec::new()
            });
        for directive in directives {
            match directive {
                Directive::Emit(ClientEvent::KeepAlive { id }) => {
                    answer_keep_alive(conn, *state, adapter, id).await;
                }
                Directive::Emit(ClientEvent::LevelEvent {
                    event,
                    pos,
                    data,
                    global: _,
                }) => {
                    if event == PARTICLES_DESTROY_BLOCK {
                        assert!(
                            matches!(data, LevelEventData::BlockState(_)),
                            "the 26.2 block-break event must retain a canonical state source"
                        );
                    }
                    out.push(Captured {
                        raw: raw.clone(),
                        event,
                        pos: (pos.x, pos.y, pos.z),
                        data: data.raw_i32(),
                    });
                }
                Directive::Emit(_) => {}
                other => apply(conn, state, other).await,
            }
        }
    }
}

/// A cascading break (torch loses its support) must produce a `level_event`
/// 2001 at the **torch's** position whose `data` is the **torch's** block state
/// id, hand-decoded from the captured bytes and cross-checked against the
/// generated `vanilla's own block's own block state registry` census.
#[tokio::test]
#[ignore = "requires the live flat creative 26.2 oracle on :25570 (+ RCON :25571)"]
async fn cascading_break_reports_the_cascaded_blocks_own_state_id() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25570,
    };
    let username = unique_username();
    let profile = LoginProfile {
        // Unique per run: in offline mode a shared name is a mutual eviction that
        // presents as a silent chunk blackout while login and keep-alives look
        // healthy (§7 trap).
        username: username.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();
    let read_timeout = Duration::from_secs(2);

    let mut conn = Connection::connect(GAME_ADDR)
        .await
        .expect("connect to the flat creative 26.2 oracle on :25570 (gate fails, never skips)");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut world = World::new();
    let mut captured = Vec::new();

    // Settle the join (chunks, position, keep-alive) before touching the world.
    pump_capturing(
        &mut conn,
        &mut state,
        &adapter,
        &mut world,
        Instant::now() + Duration::from_secs(12),
        read_timeout,
        &mut captured,
    )
    .await;
    assert_eq!(
        state,
        ConnectionState::Play,
        "join never reached Play — is the oracle healthy?"
    );

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
        "oracle RCON reachable/authenticated at :25571 — is the flat creative 26.2 oracle up? \
         start it with scripts/live-oracles/creative.sh",
    );
    let pos = rcon.cmd(&format!("data get entity {username} Pos"));
    let (px, py, pz) = parse_list3(&pos)
        .unwrap_or_else(|| panic!("could not parse our own position from {pos:?}"));

    // Three blocks to the east of the player and one above their feet: inside
    // the 64-block `levelEvent` broadcast radius, and clear of the flat world's
    // surface so the support block is unambiguous.
    #[expect(clippy::cast_possible_truncation, reason = "world coords fit i32")]
    let (gx, gy, gz) = (
        px.floor() as i32 + 3,
        py.floor() as i32 + 1,
        pz.floor() as i32,
    );
    rcon.cmd(&format!("forceload add {gx} {gz}"));
    rcon.cmd(&format!("setblock {gx} {gy} {gz} minecraft:stone"));
    let placed = rcon.cmd(&format!("setblock {gx} {} {gz} minecraft:torch", gy + 1));
    eprintln!("support at ({gx},{gy},{gz}), torch at ({gx},{},{gz}): {placed:?}", gy + 1);
    // `data get block` only answers for block entities, so the standing check is
    // an `execute if block` predicate ("Test passed" / "Test failed").
    let torch_check = rcon.cmd(&format!(
        "execute if block {gx} {} {gz} minecraft:torch",
        gy + 1
    ));
    assert!(
        torch_check.contains("passed"),
        "precondition failed: no torch stands at ({gx},{},{gz}) — got {torch_check:?}",
        gy + 1
    );

    // Drain anything the two `setblock`s themselves produced, so what follows is
    // attributable to the cascade alone.
    captured.clear();
    pump_capturing(
        &mut conn,
        &mut state,
        &adapter,
        &mut world,
        Instant::now() + Duration::from_secs(2),
        read_timeout,
        &mut captured,
    )
    .await;
    captured.clear();

    // Pull the support out. `setblock ... air` does **not** emit 2001 for the
    // support itself (that is `vanilla's own level's own set block`, not `destroyBlock`), so the only
    // 2001 this can produce is the torch's own cascade — the isolation this gate
    // depends on.
    let removed = rcon.cmd(&format!("setblock {gx} {gy} {gz} minecraft:air"));
    eprintln!("support removed: {removed:?}");

    pump_capturing(
        &mut conn,
        &mut state,
        &adapter,
        &mut world,
        Instant::now() + Duration::from_secs(6),
        read_timeout,
        &mut captured,
    )
    .await;

    for c in &captured {
        eprintln!(
            "level_event event={} pos={:?} data={} raw={:02x?}",
            c.event, c.pos, c.data, c.raw
        );
    }

    let torch_pos = (gx, gy + 1, gz);
    let destroys: Vec<&Captured> = captured
        .iter()
        .filter(|c| c.event == PARTICLES_DESTROY_BLOCK)
        .collect();
    assert!(
        !destroys.is_empty(),
        "the server sent no level_event {PARTICLES_DESTROY_BLOCK} at all — the cascade did not \
         happen, so nothing was measured (captured {} level events)",
        captured.len()
    );
    let cascade = destroys
        .iter()
        .find(|c| c.pos == torch_pos)
        .unwrap_or_else(|| {
            panic!(
                "no 2001 at the torch position {torch_pos:?}; got {:?}",
                destroys.iter().map(|c| c.pos).collect::<Vec<_>>()
            )
        });

    // --- The load-bearing assertions -------------------------------------
    //
    // 1. Hand-decode the captured bytes independently of `LevelEvent`'s
    //    derived `Decode`, so a wrong field width/order in the decoder is
    //    caught rather than reproduced. Vanilla's `ClientboundLevelEventPacket`
    //    writes: int event, long position, int data, boolean global — all
    //    big-endian fixed width, *not* VarInt.
    assert_eq!(cascade.raw.len(), 17, "2001 frame is 4 + 8 + 4 + 1 bytes");
    let hand_event = i32::from_be_bytes(cascade.raw[0..4].try_into().unwrap());
    let hand_pos = i64::from_be_bytes(cascade.raw[4..12].try_into().unwrap());
    let hand_data = i32::from_be_bytes(cascade.raw[12..16].try_into().unwrap());
    let hand_global = cascade.raw[16];
    // `vanilla's own block pos's own as long`: x in bits 38..64, z in bits 12..38, y in bits 0..12.
    // Each field is sign-extended by shifting it up to the sign bit and then
    // arithmetic-shifting it back down — x needs no up-shift because it is
    // already top-aligned.
    let (hand_x, hand_y, hand_z) = (
        (hand_pos >> 38) as i32,
        (hand_pos << 52 >> 52) as i32,
        (hand_pos << 26 >> 38) as i32,
    );
    assert_eq!(hand_event, PARTICLES_DESTROY_BLOCK);
    assert_eq!((hand_x, hand_y, hand_z), torch_pos, "hand-unpacked position");
    assert_eq!(hand_global, 0, "2001 is distance-limited, not global");
    assert_eq!(
        hand_data, cascade.data,
        "the adapter's `data` must equal the hand-decoded big-endian i32 — a VarInt \
         or unsigned misread would diverge here"
    );

    // 2. The id must be the *torch's* state id, taken from the generated
    //    `vanilla's own block's own block state registry` census rather than from anything in the
    //    particle path.
    let torch_state = first_state_of("minecraft:torch");
    eprintln!(
        "census: minecraft:torch first state id = {torch_state}; wire data = {}",
        cascade.data
    );
    assert_eq!(
        u32::try_from(cascade.data).expect("2001 data is a non-negative state id"),
        torch_state,
        "level_event 2001's data must be the cascaded block's own block-state id"
    );
    assert_eq!(
        block_states::block_name(cascade.data as u32),
        Some("minecraft:torch"),
        "the wire id must resolve to a torch in the 26.2 census"
    );

    rcon.cmd(&format!("setblock {gx} {} {gz} minecraft:air", gy + 1));
    rcon.cmd(&format!("forceload remove {gx} {gz}"));
}
