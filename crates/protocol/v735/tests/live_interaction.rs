//! Live 1.16.5 interaction acceptance test (the V-next gate for protocol 754).
//!
//! Gated behind the `live-interaction` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 1.16.5 server
//! (offline mode, flat world, RCON enabled) on `127.0.0.1:25573` with RCON on
//! `127.0.0.1:25574`:
//!
//! ```text
//! cargo test -p lodestone-v735 --features live-interaction --test live_interaction \
//!     -- --ignored --nocapture
//! ```
//!
//! # What it proves
//!
//! It drives a real join at the packet level, then exercises this crate's
//! serverbound [`encode_action`](lodestone_model::VersionAdapter::encode_action)
//! path for **block breaking** and **block placement** and confirms each with an
//! `execute if block` read-back issued over RCON — an oracle the server computes
//! independently of us. The player is put into creative through RCON first, so
//! the break is instant (no survival mining-time race).
//!
//! # Non-vacuous by construction
//!
//! Every step is a **transition** the server computes, not a static read. The
//! oracle is `execute if block <pos> minecraft:<block>`, whose response body is
//! the literal string `Test passed` when the block matches and `Test failed`
//! when it does not — two distinguishable, server-authored strings:
//!
//! * before the break, the target must read back as `stone` (the RCON
//!   `setblock` landed and the coordinate is right) — the negative control that
//!   proves the later "is air" assertion can fail;
//! * after the client's `player_digging` start+finish, the target must read back
//!   as `air` — the server saw and applied our break;
//! * for placement, the target cell is cleared to air and confirmed air first,
//!   then after the client's `block_place` it must read back as stone.
//!
//! If the server or its RCON port is unreachable, or any read-back never reaches
//! the expected state within the poll window, the test **fails** (it does not
//! skip): a missing precondition on an opted-in `#[ignore]` test is a failure.
//!
//! It lives in the version crate (not `lodestone-client`) because it names this
//! crate's concrete adapter; keeping it here means `lodestone-client` references
//! no protocol version at all.
#![cfg(feature = "live-interaction")]

use std::time::{Duration, Instant};

use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    Hand, LoginProfile, ServerAddress, Vec3f, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{RconClient, poll_until_blocking, unique_username};
use lodestone_v735::V735Adapter;
use tokio::net::TcpStream;
use uuid::Uuid;

fn server_port() -> u16 {
    std::env::var("LODESTONE_V735_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25573)
}

fn rcon_addr() -> String {
    std::env::var("LODESTONE_V735_RCON").unwrap_or_else(|_| "127.0.0.1:25574".into())
}

fn rcon_password() -> String {
    std::env::var("LODESTONE_V735_RCON_PASSWORD").unwrap_or_else(|_| "lodestone".into())
}

/// Polls `execute if block` over RCON until the block at `(x,y,z)` matches
/// `block` (when `want_match` is true) or stops matching (when false). Returns
/// `true` on success within the timeout. The oracle is server-computed: its
/// response body is `Test passed` on a match and `Test failed` otherwise.
fn poll_block(
    rcon: &mut RconClient,
    x: i32,
    y: i32,
    z: i32,
    block: &str,
    want_match: bool,
    timeout: Duration,
) -> bool {
    poll_until_blocking(timeout, Duration::from_millis(400), || {
        let out = rcon.cmd(&format!("execute if block {x} {y} {z} minecraft:{block}"));
        let matched = out.contains("Test passed");
        if matched == want_match { Some(()) } else { None }
    })
    .is_some()
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

/// Sends a serverbound action produced by the adapter's `encode_action`.
async fn send_action(
    adapter: &V735Adapter,
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

#[tokio::test]
#[ignore = "requires a live 1.16.5 server with RCON on 127.0.0.1:25573 (rcon :25574)"]
async fn breaks_and_places_blocks_on_live_1_16_server() {
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
    let adapter = V735Adapter::new();
    let mut world = lodestone_world::World::new();

    let mut rcon = RconClient::connect(rcon_addr(), &rcon_password())
        .expect("connect to 1.16.5 RCON (is lodestone-mc1165 up with enable-rcon=true?)");

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.16.5 server (is lodestone-mc1165 up on :25573?)");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Reach Play and capture the player's spawn position from the seam's own
    // TeleportPlayer event (routing every packet through the adapter also emits
    // the required teleport_confirm — a client that never confirms is corrected
    // forever).
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
        if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload) {
            for directive in directives {
                match &directive {
                    Directive::Emit(ClientEvent::TeleportPlayer { pos, .. }) => {
                        player = Some((pos.x, pos.y, pos.z));
                    }
                    Directive::Emit(ClientEvent::KeepAlive { id }) => {
                        send_action(&adapter, &mut conn, &ClientAction::KeepAliveResponse {
                            id: *id,
                        })
                        .await;
                    }
                    _ => apply(&mut conn, &mut state, directive).await,
                }
            }
        }
    }

    let (px, py, pz) = player.expect("never received a player position from the live server");
    let base_x = px.floor() as i32;
    let feet_y = py.floor() as i32;
    let base_z = pz.floor() as i32;
    // A per-process nonce keeps concurrent runs from fighting over one cell.
    let nonce = (std::process::id() % 3) as i32;
    let break_x = base_x + 1 + nonce;
    let place_x = base_x + 2 + nonce;

    eprintln!(
        "player {username} at ({px:.1},{py:.1},{pz:.1}); break target ({break_x},{feet_y},{base_z}), \
         place target ({place_x},{feet_y},{base_z})"
    );

    // Put the player into creative so the break is instant and the give lands.
    rcon.cmd(&format!("gamemode creative {username}"));

    // ---- BREAK -----------------------------------------------------------
    // Place a stone within reach, confirm it is there (the control that makes
    // the later "is air" meaningful), then break it through encode_action.
    rcon.cmd(&format!("setblock {break_x} {feet_y} {base_z} minecraft:stone"));
    assert!(
        poll_block(&mut rcon, break_x, feet_y, base_z, "stone", true, Duration::from_secs(8)),
        "RCON setblock never made ({break_x},{feet_y},{base_z}) read back as stone — is the \
         chunk loaded / the player near it?"
    );

    let break_pos = BlockPos::new(break_x, feet_y, base_z);
    send_action(&adapter, &mut conn, &ClientAction::BlockAction {
        action: BlockActionKind::StartDestroy,
        pos: break_pos,
        face: BlockFace::Up,
        sequence: 0,
    })
    .await;
    send_action(&adapter, &mut conn, &ClientAction::BlockAction {
        action: BlockActionKind::StopDestroy,
        pos: break_pos,
        face: BlockFace::Up,
        sequence: 0,
    })
    .await;
    // Keep the connection serviced while the server processes the dig.
    pump(&adapter, &mut conn, &mut state, &mut world, Duration::from_millis(600)).await;

    assert!(
        poll_block(&mut rcon, break_x, feet_y, base_z, "air", true, Duration::from_secs(10)),
        "the block at ({break_x},{feet_y},{base_z}) was NOT air after the client's player_digging — \
         the server did not accept our break"
    );
    eprintln!("BREAK confirmed: server reports ({break_x},{feet_y},{base_z}) is now air");

    // ---- PLACE -----------------------------------------------------------
    // Give the player a stack, select the hotbar slot through encode_action,
    // clear the target cell to air (and confirm), then place against the ground
    // block below it. The resulting cell must read back as stone.
    rcon.cmd(&format!("replaceitem entity {username} hotbar.0 minecraft:stone 4"));
    send_action(&adapter, &mut conn, &ClientAction::SetCarriedItem { slot: 0 }).await;

    rcon.cmd(&format!("setblock {place_x} {feet_y} {base_z} minecraft:air"));
    assert!(
        poll_block(&mut rcon, place_x, feet_y, base_z, "air", true, Duration::from_secs(8)),
        "failed to clear the place target to air before placing"
    );
    // Reference block is the ground one cell below the (now-air) target.
    let reference = BlockPos::new(place_x, feet_y - 1, base_z);
    send_action(&adapter, &mut conn, &ClientAction::UseItemOn {
        hand: Hand::Main,
        pos: reference,
        face: BlockFace::Up,
        cursor: Vec3f { x: 0.5, y: 1.0, z: 0.5 },
        inside_block: false,
        sequence: 0,
    })
    .await;
    pump(&adapter, &mut conn, &mut state, &mut world, Duration::from_millis(600)).await;

    assert!(
        poll_block(&mut rcon, place_x, feet_y, base_z, "stone", true, Duration::from_secs(10)),
        "the block at ({place_x},{feet_y},{base_z}) did NOT become stone after the client's \
         block_place — the server did not accept our placement"
    );
    eprintln!("PLACE confirmed: server reports ({place_x},{feet_y},{base_z}) is now stone");

    // Cleanup: leave the world as we found it (best effort).
    rcon.cmd(&format!("setblock {break_x} {feet_y} {base_z} minecraft:air"));
    rcon.cmd(&format!("setblock {place_x} {feet_y} {base_z} minecraft:air"));
}

/// Reads and services packets for `dur`, answering keep-alives so the server
/// does not time us out while it processes an action.
async fn pump(
    adapter: &V735Adapter,
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    world: &mut lodestone_world::World,
    dur: Duration,
) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        let read = tokio::time::timeout(Duration::from_millis(200), conn.read_packet()).await;
        let (packet_id, payload) = match read {
            Err(_) => continue,
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => break,
            Ok(Err(err)) => panic!("read error: {err}"),
        };
        if let Ok(directives) = adapter.handle_packet(world, *state, packet_id, &payload) {
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
