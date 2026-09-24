//! Hermetic proof that the relay is a transparent byte pipe.
//!
//! No Minecraft server involved: a tiny echo server stands in for "the TCP
//! server", the relay bridges a WebSocket to it, and a [`WsTransport`] round-trips
//! bytes through the whole chain. This runs in the default test suite (unlike the
//! live join test), so the relay's byte-transparency is always covered.

use lodestone_net::WsTransport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Spawns a TCP echo server on an ephemeral port and returns its address.
async fn spawn_echo() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if socket.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn relay_pipes_bytes_transparently() {
    let echo_addr = spawn_echo().await;

    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    tokio::spawn(lodestone_relay::serve(relay_listener, Some(echo_addr)));

    // No `?host=&port=` on this connection — proving `default_target` (what
    // `--target` now means) still works for a caller that names no destination
    // of its own, exactly as every pre-existing caller of this crate does.
    let mut transport = WsTransport::connect(&format!("ws://{relay_addr}"))
        .await
        .expect("connect to relay");

    // A payload that spans what would be several frames, to prove the relay does
    // not care about message boundaries.
    let payload: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
    transport.write_all(&payload).await.expect("write");
    transport.flush().await.expect("flush");

    let mut received = vec![0u8; payload.len()];
    transport
        .read_exact(&mut received)
        .await
        .expect("read echoed bytes back");

    assert_eq!(received, payload, "relay must round-trip bytes unchanged");
}

/// Spawns a TCP server that writes a fixed `banner` immediately on accept, then
/// stays open echoing whatever it receives. The banner is the discriminating
/// signal: two servers with two *different* banners are what makes "routed to
/// the right one" distinguishable from "routed to *a* one" — a single shared
/// backend cannot tell those apart, which is exactly the coincidence that hid
/// the original bug (every row's ping and every live-oracle gate pointed
/// `--target` and the fixture server at the same host).
async fn spawn_banner_server(banner: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                if socket.write_all(banner.as_bytes()).await.is_err() {
                    return;
                }
                let mut buf = [0u8; 4096];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if socket.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    addr
}

/// The discriminating-input proof this bug needed: **two different
/// destinations reached through one relay in one session**, each asserted
/// against its own banner. A single-destination test (what this crate had
/// before) cannot tell "the relay routes per connection" apart from "the relay
/// always dials the same place" — both pass it identically. This is that
/// second, distinguishing case, and it also cross-checks that connection A's
/// bytes are *not* connection B's banner (or vice versa), the control that
/// would fail under the old fixed-`--target` behaviour.
#[tokio::test]
async fn each_connection_reaches_its_own_destination() {
    let addr_a = spawn_banner_server("SERVER-A-BANNER").await;
    let addr_b = spawn_banner_server("SERVER-B-BANNER").await;
    let (host_a, port_a) = addr_a.rsplit_once(':').expect("host:port");
    let (host_b, port_b) = addr_b.rsplit_once(':').expect("host:port");

    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    // No `default_target`: every connection below must name its own
    // destination, so this test cannot pass by accident via a fallback.
    tokio::spawn(lodestone_relay::serve(relay_listener, None));

    let mut transport_a = WsTransport::connect(&format!(
        "ws://{relay_addr}/?host={host_a}&port={port_a}"
    ))
    .await
    .expect("connect to relay, routed to server A");
    let mut transport_b = WsTransport::connect(&format!(
        "ws://{relay_addr}/?host={host_b}&port={port_b}"
    ))
    .await
    .expect("connect to relay, routed to server B");

    let mut received_a = vec![0u8; "SERVER-A-BANNER".len()];
    transport_a
        .read_exact(&mut received_a)
        .await
        .expect("read banner from whatever A was routed to");
    let mut received_b = vec![0u8; "SERVER-B-BANNER".len()];
    transport_b
        .read_exact(&mut received_b)
        .await
        .expect("read banner from whatever B was routed to");

    assert_eq!(
        received_a, b"SERVER-A-BANNER",
        "connection naming server A's host:port must reach server A"
    );
    assert_eq!(
        received_b, b"SERVER-B-BANNER",
        "connection naming server B's host:port must reach server B"
    );
    // Redundant with the two assertions above given the fixture is
    // pairwise-distinct, but stated explicitly: this is the exact shape of
    // the reported bug (every row showing the *same* server's MOTD).
    assert_ne!(received_a, received_b, "A and B must not reach the same server");
}

/// A connection that names no destination, against a relay started with no
/// `default_target` either, must be refused — visibly and immediately, never
/// left open with nothing happening. This is the control for
/// [`each_connection_reaches_its_own_destination`]'s assumption that omitting
/// `default_target` actually forces per-connection addressing rather than
/// silently falling back to *something*.
#[tokio::test]
async fn a_connection_naming_no_destination_is_refused_not_hung() {
    let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_addr = relay_listener.local_addr().unwrap();
    tokio::spawn(lodestone_relay::serve(relay_listener, None));

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        WsTransport::connect(&format!("ws://{relay_addr}")),
    )
    .await
    .expect("must fail fast, not hang until the timeout");

    assert!(
        result.is_err(),
        "a connection with no destination and no default must be refused, not accepted"
    );
}
