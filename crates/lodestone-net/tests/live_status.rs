//! Live gates for the server-list status decoder ([`lodestone_net::status`]).
//!
//! These are `#[ignore]`d: they need a real vanilla server and must never run in
//! the hermetic suite. Run explicitly:
//!
//! ```text
//! cargo test -p lodestone-net --test live_status -- --ignored --nocapture
//! ```
//!
//! ## Why a live gate and not just the unit tests
//!
//! `status.rs`'s unit tests feed it JSON that *this repo wrote*, which proves
//! the flattener handles the shapes we thought of — not that a real 26.2 server
//! emits any of them. The expected values here originate outside the code under
//! test: they come off the wire from the actual server jar.

use lodestone_net::{ServerStatus, server_status};

/// The local oracle. `scripts/live-oracles/survival.sh` publishes 25565.
const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
/// Protocol advertised in the handshake; vanilla ignores it in the status state.
const PROTOCOL: i32 = 776;

async fn live_status() -> ServerStatus {
    server_status(HOST, Some(PORT), PROTOCOL)
        .await
        .expect("status ping should succeed against the local oracle")
}

/// The headline gate: a real server's status decodes to a **non-empty MOTD**
/// and a player count.
#[tokio::test]
#[ignore = "requires the live vanilla server on 127.0.0.1:25565"]
async fn live_ping_yields_a_parsed_motd() {
    let s = live_status().await;
    println!("motd      = {:?}", s.motd);
    println!("players   = {}", s.players_line());
    println!("version   = {:?} (protocol {:?})", s.version, s.protocol);
    println!("favicon   = {:?} bytes", s.favicon_png.as_ref().map(Vec::len));
    println!("latency   = {:?} ms", s.latency_ms);

    assert!(
        !s.motd.trim().is_empty(),
        "a real server always sends a description; got an empty MOTD, which is \
         what a flattener that only handles the *other* JSON shape produces"
    );
    assert!(
        !s.motd.contains('\u{a7}'),
        "formatting codes must be stripped: {:?}",
        s.motd
    );
    assert!(s.max.is_some(), "vanilla reports a slot count");
    assert!(s.online.is_some(), "vanilla reports an online count");
    assert!(
        s.online.unwrap() <= s.max.unwrap(),
        "online {:?} exceeds max {:?}",
        s.online,
        s.max
    );
    assert!(s.version.is_some(), "vanilla reports a version name");
    assert_eq!(
        s.protocol,
        Some(PROTOCOL),
        "the oracle should speak the protocol this workspace targets"
    );
    assert!(s.latency_ms.is_some(), "the ping/pong exchange should time");
}

/// Negative control for the gate above: pointing the same code at a port with
/// nothing on it must **fail**, not quietly return an empty [`ServerStatus`].
///
/// Without this, `live_ping_yields_a_parsed_motd` could be passing on a
/// default-constructed struct and nobody would know — the "assertion of an
/// absence needs a control proving the detector works" rule, applied to a
/// presence.
#[tokio::test]
#[ignore = "requires the live vanilla server on 127.0.0.1:25565"]
async fn a_dead_port_errors_rather_than_returning_an_empty_status() {
    // 1 is reserved and never has a Minecraft server on it.
    let out = server_status(HOST, Some(1), PROTOCOL).await;
    assert!(
        out.is_err(),
        "a dead port must error; got {out:?} — if this is Ok, the live gate \
         above proves nothing"
    );
    println!("dead-port error (expected): {}", out.unwrap_err());
}

/// The favicon path end to end: whatever the server sends must either decode to
/// real PNG bytes or be absent — never to non-PNG garbage.
#[tokio::test]
#[ignore = "requires the live vanilla server on 127.0.0.1:25565"]
async fn live_favicon_is_png_or_absent() {
    let s = live_status().await;
    match &s.favicon_png {
        Some(bytes) => {
            assert!(
                bytes.starts_with(&[0x89, b'P', b'N', b'G']),
                "decoded favicon is not a PNG"
            );
            assert!(bytes.len() > 64, "implausibly small PNG: {} bytes", bytes.len());
            println!("favicon decoded: {} bytes", bytes.len());
        }
        None => println!("server sent no favicon (server-icon.png absent) — allowed"),
    }
}
