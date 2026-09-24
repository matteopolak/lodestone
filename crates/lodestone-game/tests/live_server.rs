//! Live-server oracle tests (`#[ignore]`d).
//!
//! # Why these are ignored and mostly documentation
//!
//! The brief's strongest instruction is to validate container click handling
//! against a *real* server: open a chest, click items around, and compare
//! [`ClientMenu`](lodestone_game::reconcile::ClientMenu)'s prediction against
//! what the server actually reports. That is the one oracle that catches a
//! click machine which is self-consistent but desynchronised.
//!
//! Doing it end-to-end requires driving the **play**-state protocol: login,
//! join, `open_screen`, `container_click`, and reading
//! `container_set_slot` / `container_set_content`. Those wire shapes live in the
//! `crates/protocol/*` and `lodestone-client` crates, which other agents are
//! actively restructuring (there is now a `lodestone-registry` for version
//! selection). Wiring a full play session from here would mean either depending
//! on a version crate — which violates this crate's version-freedom and is
//! caught by `cargo xtask check-isolation` — or reaching into files another
//! agent owns. So per the brief I have **designed** the oracle and left it
//! unwired, rather than forcing it.
//!
//! ## The oracle, concretely
//!
//! Against `lodestone-mc262` on `:25565`:
//! 1. Log in and join; open a chest (`open_screen` gives the window id + size).
//! 2. Build a [`ClientMenu`] over a [`Menu::generic`](lodestone_game::menu::Menu::generic)
//!    of that size, seeded from the initial `container_set_content`.
//! 3. For a scripted sequence of clicks (left-pick, right-place, shift-move,
//!    number-key swap, and a left-drag distribute), call
//!    [`ClientMenu::predict`] to get a [`ClickIntent`], lower it to a
//!    `container_click` packet, and send it.
//! 4. Feed every resulting server packet through
//!    [`ClientMenu::reconcile`] as a [`ServerUpdate`].
//! 5. **Assert `corrected == false` for every reconcile.** Any correction means
//!    the prediction diverged from the server — i.e. a bug in the click machine
//!    or slot indexing. This is the whole point: the server is the judge.
//!
//! The seam is deliberately shaped so this test needs only: a way to send a
//! `ClickIntent`'s fields and a way to deliver `ServerUpdate`s. Neither touches
//! version-specific types in *this* crate.
//!
//! ## Offline-mode landmine for whoever wires this up
//!
//! The live servers run in **offline mode**, where the server derives the
//! account UUID from the *username* (`OfflinePlayer:<name>`) and discards the
//! UUID the client sends. Consequences that cost another session hours:
//!
//! * **Every login with the same username shares one persisted player file.**
//!   A `Uuid::new_v4()` per run does **not** isolate you — the server throws it
//!   away. Generate a unique *username* per run instead (see
//!   `unique_username()` in `crates/lodestone-client/tests/live_chunk.rs`:
//!   prefix + a pid⊕nanos suffix, kept under vanilla's 16-char limit).
//! * If a mob kills the test player once (the flat test worlds have hostile
//!   mobs), vanilla persists `Health = 0.0` + a death location, and every
//!   later join with that username is held on the **death screen** — a dead
//!   player receives **zero chunks** until the client sends
//!   `client_command(perform_respawn)`. Login, join, keep-alives, entity
//!   spawns and `set_chunk_cache_center` all look healthy; only the chunk
//!   stream is empty. So **if a login test suddenly gets no chunks, dump
//!   `set_health` and check for `0.0` before suspecting your own code** — that
//!   means you inherited a dead player, not that you broke something. (The
//!   decompiled `hasClientLoaded()` gate is a dead end: `sendNextChunks` runs
//!   unconditionally per tick from `MinecraftServer`.)
//! * Death/respawn handling (`set_health`, `combat_death`,
//!   `client_command(perform_respawn)`, auto-respawn) is being implemented by
//!   the `impl-world` agent — coordinate rather than duplicating it here.
//! * Do **not** quietly disable hostile mobs via shared `server.properties` to
//!   dodge this; other agents (`impl-entity`) depend on mobs being present. If
//!   a test needs an unkillable player, say so explicitly in the report.
//!
//! [`ClientMenu`]: lodestone_game::reconcile::ClientMenu
//! [`ClientMenu::predict`]: lodestone_game::reconcile::ClientMenu::predict
//! [`ClientMenu::reconcile`]: lodestone_game::reconcile::ClientMenu::reconcile
//! [`ClickIntent`]: lodestone_game::reconcile::ClickIntent
//! [`ServerUpdate`]: lodestone_game::reconcile::ServerUpdate

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Writes a VarInt (used by the handshake framing). Version-free: the Server
/// List Ping framing has been stable across every protocol this project
/// targets, so this needs no version crate.
fn write_varint(buf: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_varint(stream: &mut impl Read) -> std::io::Result<u32> {
    let mut result = 0u32;
    for shift in 0..5 {
        let mut b = [0u8; 1];
        stream.read_exact(&mut b)?;
        result |= u32::from(b[0] & 0x7f) << (shift * 7);
        if b[0] & 0x80 == 0 {
            break;
        }
    }
    Ok(result)
}

fn framed(packet_id: u32, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    write_varint(&mut inner, packet_id);
    inner.extend_from_slice(body);
    let mut out = Vec::new();
    write_varint(&mut out, inner.len() as u32);
    out.extend_from_slice(&inner);
    out
}

/// A minimal, version-free Server List Ping against the live 26.2 server.
///
/// This does not exercise the game-state code; it exists to prove the test
/// harness can reach the real oracle host, so the container-reconciliation test
/// above is only a protocol-wiring away. It performs the status handshake
/// (next-state 1) and asserts the server returns a non-empty JSON status.
#[test]
#[ignore = "requires the live lodestone-mc262 server on :25565"]
fn live_server_status_ping_is_reachable() {
    let addr = "127.0.0.1:25565";
    let mut stream = TcpStream::connect(addr).expect("connect to lodestone-mc262");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // Handshake: protocol version (VarInt, 0 = "any" for status), server
    // address, port, next state = 1 (status).
    let host = "127.0.0.1";
    let mut hs = Vec::new();
    write_varint(&mut hs, 0); // protocol version (unused for status)
    write_varint(&mut hs, host.len() as u32);
    hs.extend_from_slice(host.as_bytes());
    hs.extend_from_slice(&25565u16.to_be_bytes());
    write_varint(&mut hs, 1); // next state: status
    stream.write_all(&framed(0x00, &hs)).unwrap();

    // Status request (empty body, id 0).
    stream.write_all(&framed(0x00, &[])).unwrap();

    // Response: length, packet id (0x00), then a VarInt-prefixed JSON string.
    let _len = read_varint(&mut stream).expect("read response length");
    let pkt_id = read_varint(&mut stream).expect("read packet id");
    assert_eq!(pkt_id, 0x00, "status response packet id");
    let json_len = read_varint(&mut stream).expect("read json length") as usize;
    let mut json = vec![0u8; json_len];
    stream.read_exact(&mut json).expect("read json body");

    let text = String::from_utf8_lossy(&json);
    assert!(json_len > 0, "status JSON should be non-empty");
    assert!(
        text.contains("version") || text.contains("description") || text.contains("players"),
        "status JSON should look like a server status: {text}"
    );
    eprintln!("live 26.2 status ({json_len} bytes): {text}");
}
