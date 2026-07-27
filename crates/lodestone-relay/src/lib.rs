//! `lodestone-relay` — a deliberately protocol-blind WebSocket→TCP relay.
//!
//! Browsers cannot open raw TCP sockets, but a real Minecraft server speaks only
//! raw TCP. This crate bridges the gap: it accepts a WebSocket connection from a
//! browser (or a native [`lodestone_net::WsTransport`]) and opens a plain TCP
//! connection to the real server, then shuttles bytes between the two.
//!
//! # Why it must stay dumb
//!
//! The relay never parses a Minecraft packet. Each inbound binary WebSocket frame
//! is written to TCP verbatim, and TCP bytes are forwarded back as binary frames.
//! Because Minecraft's framing is length-prefixed and carried entirely inside the
//! byte stream, the codec on the client reassembles frames regardless of how the
//! relay chunks them — so the relay needs no knowledge of packet ids, states, or
//! versions. That is the whole point: **one relay serves every protocol version
//! and every server.** The moment it inspected a packet it would become a
//! per-version component, and we would need one per protocol.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

/// Size of the scratch buffer used when forwarding TCP → WebSocket.
const FORWARD_CHUNK: usize = 16 * 1024;

/// Accepts WebSocket clients on `listener` forever, bridging each to a fresh TCP
/// connection to `target`.
///
/// Every accepted connection is handled on its own task, so one slow or stuck
/// client never blocks another.
///
/// # Errors
///
/// Returns an error only if accepting a connection fails fatally; per-client
/// bridge errors are logged and do not stop the loop.
pub async fn serve(listener: TcpListener, target: String) -> Result<()> {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "accept failed");
                continue;
            }
        };
        let target = target.clone();
        tokio::spawn(async move {
            match bridge(stream, peer, &target).await {
                Ok(()) => tracing::info!(%peer, "bridge closed"),
                Err(error) => tracing::warn!(%peer, %error, "bridge ended with error"),
            }
        });
    }
}

/// Bridges one accepted WebSocket client to a fresh TCP connection to `target`.
///
/// # Errors
///
/// Returns an error if the WebSocket handshake fails, the target is unreachable,
/// or forwarding fails in a way other than a clean close.
pub async fn bridge(stream: TcpStream, peer: SocketAddr, target: &str) -> Result<()> {
    stream.set_nodelay(true).ok();
    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .context("websocket handshake failed")?;
    let tcp = TcpStream::connect(target)
        .await
        .with_context(|| format!("failed to reach target {target}"))?;
    tcp.set_nodelay(true).ok();
    tracing::info!(%peer, %target, "bridge open");

    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut tcp_rd, mut tcp_wr) = tcp.into_split();

    // WebSocket -> TCP: unwrap each binary frame straight onto the socket.
    let ws_to_tcp = async move {
        while let Some(message) = ws_rx.next().await {
            match message? {
                Message::Binary(data) => tcp_wr.write_all(&data).await?,
                Message::Close(_) => break,
                // Control/text frames are not part of the byte pipe; ignore them.
                Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => {}
            }
        }
        tcp_wr.shutdown().await.ok();
        Ok::<(), anyhow::Error>(())
    };

    // TCP -> WebSocket: forward raw bytes as binary frames.
    let tcp_to_ws = async move {
        let mut buf = vec![0u8; FORWARD_CHUNK];
        loop {
            let n = tcp_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ws_tx.send(Message::Binary(buf[..n].to_vec())).await?;
        }
        ws_tx.close().await.ok();
        Ok::<(), anyhow::Error>(())
    };

    // When either direction finishes (server closed, client left, error), the
    // whole bridge is done; the other future is dropped.
    tokio::select! {
        result = ws_to_tcp => result,
        result = tcp_to_ws => result,
    }
}
