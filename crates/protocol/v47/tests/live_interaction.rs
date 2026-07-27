//! Live 1.8.9 interaction acceptance test (the V-next gate for protocol 47).
//!
//! Gated behind the `live-interaction` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run against the real vanilla 1.8.9 server on
//! `127.0.0.1:25566` (`lodestone-mc189`), which now also exposes RCON on
//! `127.0.0.1:25576` (password `lodestone`):
//!
//! ```text
//! cargo test -p lodestone-v47 --features live-interaction -- --ignored live_interaction
//! ```
//!
//! # Two gates, two oracles
//!
//! * **`breaks_a_block_on_live_1_8_server`** needs no admin channel. The server
//!   is survival, so it digs the soft ground block beneath the player with the
//!   ordinary two-phase 1.8 dig (`START_DIGGING`, wait past the hand-mining time,
//!   `STOP_DIGGING`) through this crate's
//!   [`encode_action`](lodestone_model::VersionAdapter::encode_action) `block_dig`
//!   path, and asserts on the clientbound **`block_change`** the server
//!   broadcasts — a server-authored transition to air at the exact dug
//!   coordinate.
//! * **`places_a_block_on_live_1_8_server`** needs an out-of-band channel,
//!   because 1.8's `block_place` sends an *empty* inline held item and the server
//!   resolves the block from its own inventory view — impossible to satisfy on a
//!   survival server without granting the item first. It uses **RCON** (added to
//!   `mc189` for exactly this) to switch the player to creative, `replaceitem` a
//!   stone stack into the hotbar, and read the result back with a server-computed
//!   **`testforblock`** (`execute if block` does not exist until 1.13). `RCON`
//!   returns only the issuing command's output, so a concurrent run cannot
//!   contaminate the oracle.
//!
//! # Non-vacuous by construction
//!
//! * Each confirmation is a **server-computed transition** at the exact target
//!   coordinate (`block_change` to air for the break; `testforblock` air→stone
//!   for the place), not a static read. An interaction the server rejected
//!   produces no such transition and the test fails.
//! * Negative control (break): a deliberately **truncated** `block_change`
//!   payload fed through the same decode path must `Err`, proving the decoder
//!   that backs the assertion would notice a malformed packet rather than
//!   silently accepting one.
//!
//! If the server is unreachable or an interaction is never confirmed within the
//! poll window, the test **fails** (it does not skip): a missing precondition on
//! an opted-in `#[ignore]` test is a failure, not a pass.

#![cfg(feature = "live-interaction")]

use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    Hand, LoginProfile, ServerAddress, Vec3f, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{RconClient, unique_username};
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

/// RCON port for the 1.8.9 server. Unlike the break test — whose oracle is the
/// clientbound `block_change` the survival server broadcasts — the place test
/// needs an out-of-band admin channel to *grant* the player a placeable item
/// (the block only resolves server-side if the held item actually exists) and to
/// read the result back with a server-computed `testforblock`.
fn rcon_port() -> u16 {
    std::env::var("LODESTONE_V47_RCON_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25576)
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

/// Drives login through to Play and returns the connection, its state, and the
/// player's spawn position. Keep-alives are answered while waiting so the server
/// does not correct or drop us before we have located the player.
async fn join_and_locate(
    adapter: &V47Adapter,
    world: &mut lodestone_world::World,
    profile: &LoginProfile,
    server: &ServerAddress,
    port: u16,
) -> (Connection<TcpStream>, ConnectionState, (f64, f64, f64)) {
    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.8.9 server (is lodestone-mc189 up on :25566?)");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(profile, server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut player: Option<(f64, f64, f64)> = None;
    let overall = Duration::from_secs(45);
    let started = Instant::now();
    while player.is_none() && started.elapsed() < overall {
        let read = tokio::time::timeout(Duration::from_secs(5), conn.read_packet()).await;
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
        if let Ok(directives) = adapter.handle_packet(world, state, packet_id, &payload) {
            for directive in directives {
                if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                    send_action(
                        adapter,
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

    let pos = player.expect("never received a player position from the live server");
    (conn, state, pos)
}

/// Services the connection for `dur`, answering keep-alives so the server keeps
/// us connected while it processes an interaction.
async fn service(
    adapter: &V47Adapter,
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    world: &mut lodestone_world::World,
    dur: Duration,
) {
    let until = Instant::now() + dur;
    while Instant::now() < until {
        if let Ok(Ok(Some((pid, payload)))) =
            tokio::time::timeout(Duration::from_millis(200), conn.read_packet()).await
            && let Ok(directives) = adapter.handle_packet(world, *state, pid, &payload)
        {
            for directive in directives {
                if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                    send_action(adapter, conn, &ClientAction::KeepAliveResponse { id: *id }).await;
                    continue;
                }
                apply(conn, state, directive).await;
            }
        }
    }
}

/// Polls `testforblock <x> <y> <z> minecraft:stone` over RCON until the block
/// matches `want_stone` (`true` = must read back as stone, `false` = must not),
/// or the timeout expires. The oracle is the *server-computed* testforblock
/// result — independent of this crate's own decoders, which is the whole point:
/// the expected value originates outside the code under test. RCON returns only
/// this command's output, so a concurrent run elsewhere in the shared world
/// cannot contaminate the result.
fn poll_testforblock_stone(
    rcon: &mut RconClient,
    x: i32,
    y: i32,
    z: i32,
    want_stone: bool,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let out = rcon.cmd(&format!("testforblock {x} {y} {z} minecraft:stone"));
        let is_stone = out.contains("Successfully found the block at");
        if want_stone == is_stone {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// Places a block on the live 1.8.9 server and confirms it server-side.
///
/// # What it proves, and why it needs RCON where the break test did not
///
/// 1.8's `block_place` carries an **empty** inline held item (see
/// [`lodestone_v47::packets::game::BlockPlace`]); the server resolves the actual
/// block from its own inventory view. So a placement only succeeds if the player
/// truly holds a placeable block server-side — which, on a survival server with
/// no creative and no give, is impossible. We therefore grant the item and read
/// the result back through the 1.8 admin channel (`replaceitem` + `testforblock`
/// over RCON; `execute if block` does not exist until 1.13).
///
/// The assertion is a server-computed `testforblock` transition of the target
/// cell from air to stone — the server decides independently of us whether the
/// placement was legal. A single `block_place` can be transiently ignored (held
/// item / position settling a tick behind our packets), so the placement is
/// re-sent each round until confirmed or the window expires; re-sending is still
/// honest because the cell only reads back as stone if the server accepted a
/// placement, and a second place onto an occupied cell is a harmless no-op.
#[tokio::test]
#[ignore = "requires a live 1.8.9 server with RCON on 127.0.0.1:25576 (lodestone-mc189)"]
async fn places_a_block_on_live_1_8_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let username = unique_username();
    let profile = LoginProfile {
        username: username.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V47Adapter::new();
    let mut world = lodestone_world::World::new();

    let (mut conn, mut state, (px, py, pz)) =
        join_and_locate(&adapter, &mut world, &profile, &server, port).await;

    let base_x = px.floor() as i32;
    let feet_y = py.floor() as i32;
    let base_z = pz.floor() as i32;
    // A per-process nonce keeps concurrent runs from fighting over one cell.
    let nonce = (std::process::id() % 3) as i32;
    let place_x = base_x + 2 + nonce;
    eprintln!(
        "player {username} at ({px:.1},{py:.1},{pz:.1}); place target ({place_x},{feet_y},{base_z})"
    );

    let mut rcon = RconClient::connect(("127.0.0.1", rcon_port()), "lodestone")
        .expect("connect to mc189 RCON on :25576 (is enable-rcon=true?)");

    // Creative + a real stone stack in hotbar slot 0, then select that slot so
    // the server's view of our held item is a placeable block.
    rcon.cmd(&format!("gamemode 1 {username}"));
    rcon.cmd(&format!(
        "replaceitem entity {username} slot.hotbar.0 minecraft:stone 4"
    ));
    send_action(
        &adapter,
        &mut conn,
        &ClientAction::SetCarriedItem { slot: 0 },
    )
    .await;

    // Clear the target cell to air and confirm — the control that makes the
    // later "is stone" a real transition rather than a static read.
    rcon.cmd(&format!(
        "setblock {place_x} {feet_y} {base_z} minecraft:air"
    ));
    assert!(
        poll_testforblock_stone(
            &mut rcon,
            place_x,
            feet_y,
            base_z,
            false,
            Duration::from_secs(8)
        ),
        "failed to clear the place target ({place_x},{feet_y},{base_z}) to air before placing"
    );

    // Reference block is the solid ground one cell below the (now-air) target;
    // placing against its up-face puts the new block in the target cell.
    let reference = BlockPos::new(place_x, feet_y - 1, base_z);
    let place_deadline = Instant::now() + Duration::from_secs(15);
    let mut placed = false;
    while Instant::now() < place_deadline {
        send_action(
            &adapter,
            &mut conn,
            &ClientAction::UseItemOn {
                hand: Hand::Main,
                pos: reference,
                face: BlockFace::Up,
                cursor: Vec3f {
                    x: 0.5,
                    y: 1.0,
                    z: 0.5,
                },
                inside_block: false,
                sequence: 0,
            },
        )
        .await;
        service(
            &adapter,
            &mut conn,
            &mut state,
            &mut world,
            Duration::from_millis(400),
        )
        .await;
        if poll_testforblock_stone(
            &mut rcon,
            place_x,
            feet_y,
            base_z,
            true,
            Duration::from_millis(800),
        ) {
            placed = true;
            break;
        }
    }

    assert!(
        placed,
        "the block at ({place_x},{feet_y},{base_z}) did NOT become stone after the client's \
         block_place — the server did not accept our placement"
    );
    eprintln!("PLACE confirmed by server testforblock: ({place_x},{feet_y},{base_z}) is now stone");

    // Cleanup: leave the world as we found it (best effort).
    rcon.cmd(&format!(
        "setblock {place_x} {feet_y} {base_z} minecraft:air"
    ));
}
