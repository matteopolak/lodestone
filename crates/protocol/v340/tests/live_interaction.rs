//! Live 1.12.2 interaction acceptance test (the V-next gate for protocol 340).
//!
//! Gated behind the `live-interaction` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run against the real vanilla 1.12.2 server with
//! a console FIFO (`lodestone-tw1122` on `127.0.0.1:25569`):
//!
//! ```text
//! cargo test -p lodestone-v340 --features live-interaction -- --ignored live_interaction
//! ```
//!
//! # What it proves
//!
//! It drives a real join at the packet level, then exercises this crate's
//! serverbound [`encode_action`](lodestone_model::VersionAdapter::encode_action)
//! path for **block breaking** and **block placement** and confirms each with a
//! `testforblock` read-back issued through the server console — an oracle the
//! server computes independently of us. The player is put into creative through
//! the console first, so the break is instant (no survival mining-time race).
//!
//! # Non-vacuous by construction
//!
//! Every step is a **transition** the server computes, not a static read:
//!
//! * before the break, `testforblock <target> stone` must *succeed* (the console
//!   `setblock` landed and the coordinate is right) — this is the negative
//!   control that proves the later "is Air" assertion can fail;
//! * after the client's `block_dig`, `testforblock <target> stone` must report
//!   `is Air` — the server saw and applied our break;
//! * for placement, the target cell is cleared to air and confirmed air first,
//!   then after the client's `block_place` it must read back as stone.
//!
//! If the server or its console is unreachable, or any read-back never reaches
//! the expected state within the poll window, the test **fails** (it does not
//! skip): a missing precondition on an opted-in `#[ignore]` test is a failure.

#![cfg(feature = "live-interaction")]

use std::process::Command;
use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ClientAction, ClientEvent, ConnectionState, Directive,
    Hand, LoginProfile, ServerAddress, Vec3f, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::unique_username;
use lodestone_v340::V340Adapter;
use lodestone_v340::packet_ids::play;
use lodestone_v340::packets::game::ClientboundPositionLook;
use tokio::net::TcpStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 340 };

fn server_port() -> u16 {
    std::env::var("LODESTONE_V340_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25569)
}

fn container() -> String {
    std::env::var("LODESTONE_V340_CONTAINER").unwrap_or_else(|_| "lodestone-tw1122".into())
}

fn console_path() -> String {
    std::env::var("LODESTONE_V340_CONSOLE").unwrap_or_else(|_| "/w/console".into())
}

/// Sends one command to the server console FIFO. Panics on a docker failure —
/// this test has opted in to needing the live container.
fn console(cmd: &str) {
    let redirect = format!("printf '{cmd}\\n' > {}", console_path());
    let status = Command::new("docker")
        .args(["exec", &container(), "sh", "-c", &redirect])
        .status()
        .expect("spawn docker exec for server console");
    assert!(status.success(), "server console command failed: {cmd}");
}

/// Reads the last `secs` seconds of the container log.
fn logs_since(secs: u32) -> String {
    let out = Command::new("docker")
        .args(["logs", "--since", &format!("{secs}s"), &container()])
        .output()
        .expect("spawn docker logs");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

/// Polls `testforblock` until the block at `(x,y,z)` matches `expect_air`
/// (`true` = the coordinate must read back as air, `false` = as stone). Returns
/// `true` on success within the timeout.
fn poll_block_is_air(x: i32, y: i32, z: i32, expect_air: bool, timeout: Duration) -> bool {
    let coord = format!("{x}, {y}, {z}");
    let deadline = Instant::now() + timeout;
    loop {
        console(&format!("testforblock {x} {y} {z} minecraft:stone"));
        std::thread::sleep(Duration::from_millis(700));
        let log = logs_since(6);
        // A "Successfully found" line means the block IS stone; an "is Air" line
        // means it is not. We scan for our exact coordinate to avoid picking up a
        // concurrent player's activity elsewhere in the shared world.
        let is_stone = log
            .lines()
            .any(|l| l.contains("Successfully found the block at") && l.contains(&coord));
        let is_air = log
            .lines()
            .any(|l| l.contains(&format!("block at {coord} is Air")));
        if expect_air && is_air {
            return true;
        }
        if !expect_air && is_stone {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
    }
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
    adapter: &V340Adapter,
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
#[ignore = "requires a live 1.12.2 server with console FIFO on 127.0.0.1:25569"]
async fn breaks_and_places_blocks_on_live_1_12_server() {
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
    let adapter = V340Adapter::new();
    let mut world = lodestone_world::World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.12.2 server (is lodestone-tw1122 up on :25569?)");
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
        // Route every packet through the adapter so the required teleport_confirm
        // is emitted (a client that never confirms is corrected forever).
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
    console(&format!("gamemode creative {username}"));
    std::thread::sleep(Duration::from_millis(400));

    // ---- BREAK -----------------------------------------------------------
    // Place a stone within reach, confirm it is there (the control that makes
    // the later "is Air" meaningful), then break it through encode_action.
    console(&format!(
        "setblock {break_x} {feet_y} {base_z} minecraft:stone"
    ));
    assert!(
        poll_block_is_air(break_x, feet_y, base_z, false, Duration::from_secs(8)),
        "console setblock never made ({break_x},{feet_y},{base_z}) read back as stone — is the \
         chunk loaded / the player near it?"
    );

    let break_pos = BlockPos::new(break_x, feet_y, base_z);
    send_action(
        &adapter,
        &mut conn,
        &ClientAction::BlockAction {
            action: BlockActionKind::StartDestroy,
            pos: break_pos,
            face: BlockFace::Up,
            sequence: 0,
        },
    )
    .await;
    send_action(
        &adapter,
        &mut conn,
        &ClientAction::BlockAction {
            action: BlockActionKind::StopDestroy,
            pos: break_pos,
            face: BlockFace::Up,
            sequence: 0,
        },
    )
    .await;
    // Keep the connection serviced while the server processes the dig.
    pump(
        &adapter,
        &mut conn,
        &mut state,
        &mut world,
        Duration::from_millis(600),
    )
    .await;

    assert!(
        poll_block_is_air(break_x, feet_y, base_z, true, Duration::from_secs(10)),
        "the block at ({break_x},{feet_y},{base_z}) was NOT air after the client's block_dig — the \
         server did not accept our break"
    );
    eprintln!("BREAK confirmed: server reports ({break_x},{feet_y},{base_z}) is now air");

    // ---- PLACE -----------------------------------------------------------
    // Give the player a stack, select the hotbar slot through encode_action,
    // clear the target cell to air (and confirm), then place against the ground
    // block below it. The resulting cell must read back as stone.
    console(&format!(
        "replaceitem entity {username} slot.hotbar.0 minecraft:stone 4"
    ));
    std::thread::sleep(Duration::from_millis(300));
    send_action(
        &adapter,
        &mut conn,
        &ClientAction::SetCarriedItem { slot: 0 },
    )
    .await;

    console(&format!(
        "setblock {place_x} {feet_y} {base_z} minecraft:air"
    ));
    assert!(
        poll_block_is_air(place_x, feet_y, base_z, true, Duration::from_secs(8)),
        "failed to clear the place target to air before placing"
    );
    // Reference block is the ground one cell below the (now-air) target.
    let reference = BlockPos::new(place_x, feet_y - 1, base_z);
    // A single `block_place` can be transiently ignored by the server (the held
    // item / player position settling one tick behind our packets), so re-send
    // the placement each round until the server-authored `testforblock` confirms
    // stone or the window expires. Re-sending is still an honest oracle: the cell
    // only reads back as stone if the *server* actually accepted a placement, and
    // a second place onto an already-stone cell is a harmless no-op.
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
        pump(
            &adapter,
            &mut conn,
            &mut state,
            &mut world,
            Duration::from_millis(400),
        )
        .await;
        if poll_block_is_air(place_x, feet_y, base_z, false, Duration::from_millis(800)) {
            placed = true;
            break;
        }
    }

    assert!(
        placed,
        "the block at ({place_x},{feet_y},{base_z}) did NOT become stone after the client's \
         block_place — the server did not accept our placement"
    );
    eprintln!("PLACE confirmed: server reports ({place_x},{feet_y},{base_z}) is now stone");

    // Cleanup: leave the world as we found it (best effort).
    console(&format!(
        "setblock {break_x} {feet_y} {base_z} minecraft:air"
    ));
    console(&format!(
        "setblock {place_x} {feet_y} {base_z} minecraft:air"
    ));
}

/// Reads and services packets for `dur`, answering keep-alives so the server
/// does not time us out while it processes an action.
async fn pump(
    adapter: &V340Adapter,
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
