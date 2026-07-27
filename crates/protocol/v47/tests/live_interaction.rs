//! Live 1.8.9 interaction acceptance test (the V-next gate for protocol 47).
//!
//! Gated behind the `live-interaction` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run against the real vanilla 1.8.9 server on
//! `127.0.0.1:25566` (`lodestone-mc189`):
//!
//! ```text
//! cargo test -p lodestone-v47 --features live-interaction -- --ignored live_interaction
//! ```
//!
//! # What it proves, and why the oracle differs from v340
//!
//! The 1.8.9 test server has **no RCON and no console FIFO** (unlike the 1.12.2
//! `tw1122` container), so there is no out-of-band way to `testforblock` a
//! coordinate. Instead this test uses the only server-computed oracle 1.8
//! offers: the clientbound **`block_change`** packet the server broadcasts when
//! a block actually changes. The client drives a real break through this crate's
//! [`encode_action`](lodestone_model::VersionAdapter::encode_action) `block_dig`
//! path, and the assertion is that the *server* reports the dug coordinate
//! transitioning to air.
//!
//! Because the server is in survival (we cannot switch it to creative without
//! console access), the break targets the soft ground block beneath the player
//! and sends `START_DIGGING`, waits past the hand-mining time, then
//! `STOP_DIGGING` — the ordinary two-phase 1.8 dig.
//!
//! # Non-vacuous by construction
//!
//! * The confirmation is a `block_change` **to air (state id 0) at the exact dug
//!   coordinate** — a server-authored transition, not a static read. A break the
//!   server rejected produces no such packet and the test fails.
//! * Negative control: a deliberately **truncated** `block_change` payload fed
//!   through the same decode path must `Err`, proving the decoder that backs the
//!   assertion would notice a malformed packet rather than silently accepting
//!   one.
//!
//! If the server is unreachable or the break is never confirmed within the poll
//! window, the test **fails** (it does not skip): a missing precondition on an
//! opted-in `#[ignore]` test is a failure, not a pass.

#![cfg(feature = "live-interaction")]

use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::unique_username;
use lodestone_v47::V47Adapter;
use lodestone_v47::packet_ids::play;
use lodestone_v47::packets::game::ClientboundPositionLook;
use lodestone_v47::packets::position::unpack_position;
use tokio::net::TcpStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 47 };

fn server_port() -> u16 {
    std::env::var("LODESTONE_V47_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25566)
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
        Directive::Emit(_) => {}
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string());
        }
        _ => {}
    }
}

async fn send_action(
    adapter: &V47Adapter,
    conn: &mut Connection<TcpStream>,
    action: &ClientAction,
) {
    let (packet_id, body) = adapter
        .encode_action(ConnectionState::Play, action)
        .expect("encode action")
        .expect("action produced a packet");
    conn.write_packet(packet_id, &body)
        .await
        .expect("write serverbound action");
}

/// Decodes a 1.8 `block_change` payload into `(position, block_state_id)`.
///
/// Wire layout: packed `position` (`i64`), then the new block state as a varint
/// (`id << 4 | meta`). Air is state id `0`.
fn decode_block_change(payload: &[u8]) -> Result<(BlockPos, i32), lodestone_core::Error> {
    let mut r = Reader::new(payload);
    let packed = r.i64()?;
    let state = r.var_i32()?;
    r.ensure_empty()?;
    Ok((unpack_position(packed), state))
}

#[tokio::test]
#[ignore = "requires a live 1.8.9 server on 127.0.0.1:25566"]
async fn breaks_a_block_on_live_1_8_server() {
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
    let mut world = lodestone_world::World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.8.9 server (is lodestone-mc189 up on :25566?)");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Reach Play and capture the player's spawn position.
    let mut player: Option<(f64, f64, f64)> = None;
    let overall = Duration::from_secs(45);
    let read_timeout = Duration::from_secs(5);
    let started = Instant::now();
    while player.is_none() && started.elapsed() < overall {
        let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
        let (packet_id, payload) = match read {
            Err(_) => continue,
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => panic!("connection closed before reaching Play"),
            Ok(Err(err)) => panic!("read error: {err}"),
        };
        if state == ConnectionState::Play && packet_id == play::clientbound::POSITION {
            let mut r = Reader::new(&payload);
            if let Ok(pos) = ClientboundPositionLook::decode(&mut r, CTX) {
                player = Some((pos.x, pos.y, pos.z));
            }
        }
        if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload) {
            for directive in directives {
                if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                    send_action(
                        &adapter,
                        &mut conn,
                        &ClientAction::KeepAliveResponse { id: *id },
                    )
                    .await;
                    continue;
                }
                apply(&mut conn, &mut state, directive).await;
            }
        }
    }

    let (px, py, pz) = player.expect("never received a player position from the live server");
    // The block directly beneath the player's feet: soft ground in the flat
    // survival world, so it is hand-breakable within a second or two.
    let target = BlockPos::new(px.floor() as i32, py.floor() as i32 - 1, pz.floor() as i32);
    eprintln!(
        "player at ({px:.1},{py:.1},{pz:.1}); breaking ground block at \
         ({},{},{})",
        target.x, target.y, target.z
    );

    // Two-phase survival dig: start, mine past the hand-break time, then finish.
    send_action(
        &adapter,
        &mut conn,
        &ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: target,
            face: BlockFace::Up,
            sequence: 0,
        },
    )
    .await;

    // Service the connection for the mining interval (answer keep-alives).
    let mine_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < mine_until {
        if let Ok(Ok(Some((pid, payload)))) =
            tokio::time::timeout(Duration::from_millis(200), conn.read_packet()).await
            && let Ok(directives) = adapter.handle_packet(&mut world, state, pid, &payload)
        {
            for directive in directives {
                if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                    send_action(
                        &adapter,
                        &mut conn,
                        &ClientAction::KeepAliveResponse { id: *id },
                    )
                    .await;
                    continue;
                }
                apply(&mut conn, &mut state, directive).await;
            }
        }
    }

    send_action(
        &adapter,
        &mut conn,
        &ClientAction::BlockAction {
            action: BlockActionKind::StopDestroy,
            pos: target,
            face: BlockFace::Up,
            sequence: 0,
        },
    )
    .await;

    // Wait for the server to report the block turning to air at our exact
    // coordinate. Any block_change decoded here doubles as live coverage of the
    // packet's wire shape (zero trailing bytes).
    let mut broke = false;
    let mut seen_changes = 0usize;
    let mut last_change_payload: Option<Vec<u8>> = None;
    let confirm_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < confirm_deadline {
        let read = tokio::time::timeout(Duration::from_millis(500), conn.read_packet()).await;
        let (packet_id, payload) = match read {
            Err(_) => continue,
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("read error: {err}"),
        };
        if state == ConnectionState::Play && packet_id == play::clientbound::BLOCK_CHANGE {
            let (pos, block_state) =
                decode_block_change(&payload).expect("real block_change decodes cleanly");
            seen_changes += 1;
            last_change_payload = Some(payload.clone());
            eprintln!(
                "block_change: ({},{},{}) -> state {block_state}",
                pos.x, pos.y, pos.z
            );
            if pos == target && block_state == 0 {
                broke = true;
                break;
            }
        }
        if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload) {
            for directive in directives {
                if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                    send_action(
                        &adapter,
                        &mut conn,
                        &ClientAction::KeepAliveResponse { id: *id },
                    )
                    .await;
                    continue;
                }
                apply(&mut conn, &mut state, directive).await;
            }
        }
    }

    assert!(
        broke,
        "the server never reported ({},{},{}) turning to air after our block_dig \
         ({seen_changes} block_change packets seen) — the break was not accepted",
        target.x, target.y, target.z
    );
    eprintln!(
        "BREAK confirmed by server block_change: ({},{},{}) is now air",
        target.x, target.y, target.z
    );

    // Negative control: the decoder that backs the assertion must reject a
    // truncated block_change rather than silently accepting a short packet.
    let full = last_change_payload.expect("a block_change payload was captured");
    assert!(
        decode_block_change(&full[..full.len() - 1]).is_err(),
        "a truncated block_change must fail to decode — otherwise the oracle could pass on garbage"
    );
}
