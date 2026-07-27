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
    tokio::spawn(lodestone_relay::serve(relay_listener, echo_addr));

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
