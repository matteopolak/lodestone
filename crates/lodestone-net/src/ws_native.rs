//! A native WebSocket-backed [`Transport`], for talking to a Minecraft server
//! through a WebSocket→TCP relay.
//!
//! Browsers cannot open raw TCP sockets, so a browser build reaches a server via
//! a relay that speaks WebSocket on one side and TCP on the other. This module
//! is the *native* counterpart of that browser transport: it lets a normal,
//! non-wasm client dial the same relay. That is deliberate — it means the exact
//! relay path a browser will use can be exercised and proven from native code,
//! isolating "does the WebSocket byte pipe work" from "does wasm work".
//!
//! [`WsTransport`] adapts a binary WebSocket into the byte-stream shape the rest
//! of the stack expects: it is [`AsyncRead`] + [`AsyncWrite`], so it satisfies
//! [`crate::Transport`] and drops straight into
//! [`crate::Connection::new`]/`connect_with`. Each outbound write becomes one
//! binary WebSocket frame; inbound binary frames are concatenated into the read
//! stream, so frame boundaries are invisible to the [`crate::Codec`] above — the
//! relay stays a dumb, protocol-blind byte pipe.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::inbox::ByteInbox;

/// The default underlying socket a [`WsTransport`] rides: a (possibly TLS)
/// TCP stream, as produced by [`WsTransport::connect`].
pub type DefaultWsSocket = MaybeTlsStream<TcpStream>;

/// A [`Transport`](crate::Transport) that rides a binary WebSocket.
///
/// Construct one with [`WsTransport::connect`] against a relay URL such as
/// `ws://127.0.0.1:25580`, then hand it to
/// [`crate::Connection::new`] or `ClientBuilder::connect_with`.
///
/// It is generic over the underlying socket `S` (defaulting to a TCP/TLS
/// stream) so the relay byte pipe can be exercised over an in-memory duplex in
/// tests, decoupled from any real socket.
#[derive(Debug)]
pub struct WsTransport<S = DefaultWsSocket> {
    ws: WebSocketStream<S>,
    /// Bytes received from binary frames, not yet handed to the reader. This is
    /// the shared, exhaustively-tested reassembler that makes WebSocket message
    /// boundaries invisible to the [`crate::Codec`] above.
    inbox: ByteInbox,
}

impl WsTransport<DefaultWsSocket> {
    /// Dials a WebSocket relay and returns a ready transport.
    ///
    /// `url` is a WebSocket URL, e.g. `ws://127.0.0.1:25580`. The relay is
    /// expected to open a TCP connection to the real server and shuttle bytes;
    /// this transport neither knows nor cares what the relay's TCP target is.
    ///
    /// # Errors
    ///
    /// Returns [`crate::NetError::Io`] if the WebSocket handshake fails.
    pub async fn connect(url: &str) -> crate::Result<Self> {
        let (ws, _response) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(ws_to_io)?;
        Ok(Self::from_ws(ws))
    }
}

impl<S> WsTransport<S> {
    /// Wraps an already-established [`WebSocketStream`] as a transport.
    ///
    /// This is the seam that lets the relay byte pipe be driven over any
    /// `AsyncRead + AsyncWrite` socket — a real TCP/TLS stream in production, or
    /// an in-memory duplex in tests — so the reframing behaviour can be proven
    /// hermetically.
    #[must_use]
    pub fn from_ws(ws: WebSocketStream<S>) -> Self {
        Self {
            ws,
            inbox: ByteInbox::default(),
        }
    }
}

/// Maps a tungstenite error into the `io::Error` the async traits speak.
fn ws_to_io(error: tokio_tungstenite::tungstenite::Error) -> io::Error {
    io::Error::other(error)
}

impl<S> AsyncRead for WsTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // `WsTransport` is `Unpin` (all fields are), so a plain `&mut` is sound.
        let this = self.get_mut();
        loop {
            // Serve buffered bytes from previously received frames first.
            if !this.inbox.is_empty() {
                this.inbox.serve(buf);
                return Poll::Ready(Ok(()));
            }

            // Otherwise pull the next WebSocket message.
            match Pin::new(&mut this.ws).poll_next(cx) {
                Poll::Ready(Some(Ok(message))) => match message {
                    Message::Binary(data) => {
                        if data.is_empty() {
                            continue;
                        }
                        this.inbox.push(&data);
                        // Loop back to drain the freshly buffered bytes.
                    }
                    // A clean WebSocket close, or the stream ending, is EOF at a
                    // frame boundary — exactly what the codec treats as a clean
                    // shutdown.
                    Message::Close(_) => return Poll::Ready(Ok(())),
                    // Control and text frames are not part of the byte pipe.
                    Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => {
                        continue;
                    }
                },
                Poll::Ready(Some(Err(error))) => return Poll::Ready(Err(ws_to_io(error))),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S> AsyncWrite for WsTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.ws).poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                let message = Message::Binary(data.to_vec());
                match Pin::new(&mut this.ws).start_send(message) {
                    Ok(()) => Poll::Ready(Ok(data.len())),
                    Err(error) => Poll::Ready(Err(ws_to_io(error))),
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(ws_to_io(error))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws).poll_flush(cx).map_err(ws_to_io)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.ws).poll_close(cx).map_err(ws_to_io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Connection;
    use crate::error::NetError;
    use crate::transport::memory_pair;
    use futures_util::SinkExt;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::tungstenite::protocol::Role;

    type Packet = (i32, Vec<u8>);

    /// Encodes `packets` to the exact on-wire byte stream a peer would send,
    /// honoring the same compression/encryption the reader will use, by driving
    /// a real [`Connection`] over an in-memory pipe and draining the result.
    async fn encode_wire(
        packets: &[Packet],
        compression: Option<i32>,
        secret: Option<[u8; 16]>,
    ) -> Vec<u8> {
        let (tx, mut rx) = memory_pair();
        let mut enc = Connection::new(tx);
        if let Some(t) = compression {
            enc.set_compression(t);
        }
        if let Some(s) = secret {
            enc.enable_encryption(&s).unwrap();
        }
        for (id, fields) in packets {
            enc.write_packet(*id, fields).await.unwrap();
        }
        drop(enc); // closes the writer so read_to_end terminates
        let mut wire = Vec::new();
        rx.read_to_end(&mut wire).await.unwrap();
        wire
    }

    /// Splits `wire` into WebSocket-message-sized chunks, cycling `sizes`. An
    /// empty `sizes` yields one giant message (maximal coalescing). Small sizes
    /// split packets across frames; large ones coalesce several packets per
    /// frame — the two hazards a relay must survive.
    fn rechunk(wire: &[u8], sizes: &[usize]) -> Vec<Vec<u8>> {
        if sizes.is_empty() {
            return vec![wire.to_vec()];
        }
        let mut out = Vec::new();
        let mut i = 0;
        let mut k = 0;
        while i < wire.len() {
            let n = sizes[k % sizes.len()].clamp(1, wire.len() - i);
            out.push(wire[i..i + n].to_vec());
            i += n;
            k += 1;
        }
        out
    }

    /// Builds a connected pair of raw-socket WebSocket streams over an in-memory
    /// duplex — a hermetic stand-in for the relay's WebSocket hop.
    async fn ws_pair() -> (
        WebSocketStream<tokio::io::DuplexStream>,
        WebSocketStream<tokio::io::DuplexStream>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(256 * 1024);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        (client, server)
    }

    /// Drives a full relay round trip: the server sends `packets` as WS binary
    /// messages chunked by `sizes` (mis-aligned to packet boundaries), then
    /// closes; the client reads through a [`WsTransport`] and must recover every
    /// packet byte-identical and in order, followed by a clean EOF.
    async fn assert_roundtrip(
        packets: &[Packet],
        compression: Option<i32>,
        secret: Option<[u8; 16]>,
        sizes: &[usize],
    ) {
        let wire = encode_wire(packets, compression, secret).await;
        let frames = rechunk(&wire, sizes);
        let (client_ws, mut server_ws) = ws_pair().await;

        let server = tokio::spawn(async move {
            for frame in frames {
                server_ws.send(Message::Binary(frame)).await.unwrap();
            }
            server_ws.close(None).await.unwrap();
        });

        let mut conn = Connection::new(WsTransport::from_ws(client_ws));
        if let Some(t) = compression {
            conn.set_compression(t);
        }
        if let Some(s) = secret {
            conn.enable_encryption(&s).unwrap();
        }

        for (want_id, want_fields) in packets {
            let (id, fields) = conn.read_packet().await.unwrap().unwrap();
            assert_eq!(id, *want_id, "packet id mismatch through relay");
            assert_eq!(&fields, want_fields, "packet body mismatch through relay");
        }
        assert!(
            conn.read_packet().await.unwrap().is_none(),
            "clean WS close must surface as EOF (Ok(None))"
        );
        server.await.unwrap();
    }

    fn sample_packets() -> Vec<Packet> {
        vec![
            (0x00, b"handshake".to_vec()),
            (0x01, vec![]),
            (0x02, vec![0xAB; 40]),
            (0x03, b"another packet with some length".to_vec()),
            (0x7F, vec![1, 2, 3]),
        ]
    }

    #[tokio::test]
    async fn relay_reframes_split_and_coalesced_uncompressed() {
        let packets = sample_packets();
        // One byte per frame: maximal split. Empty sizes: one giant frame,
        // maximal coalescing. Irregular: a mix of both in one stream.
        assert_roundtrip(&packets, None, None, &[1]).await;
        assert_roundtrip(&packets, None, None, &[]).await;
        assert_roundtrip(&packets, None, None, &[3, 1, 50, 7, 2]).await;
    }

    #[tokio::test]
    async fn relay_passes_compression_through() {
        // Threshold 16: bodies below stay raw, at/above compress. Packets span
        // the boundary, and the frames are mis-aligned to packet boundaries.
        let packets = vec![
            (0x00, vec![1, 2, 3]),   // below threshold: raw
            (0x01, vec![0x5A; 16]),  // exactly at threshold
            (0x02, vec![0xC3; 200]), // well above: zlib
            (0x03, b"tiny".to_vec()),
        ];
        assert_roundtrip(&packets, Some(16), None, &[1]).await;
        assert_roundtrip(&packets, Some(16), None, &[5, 40, 2]).await;
    }

    #[tokio::test]
    async fn relay_passes_encryption_through_stateful() {
        // The relay is protocol-blind, so an encrypted session must pass through
        // untouched. Feeding many packets one byte per WS frame combines the
        // split-read trap with the cross-packet CFB8 feedback register: if the
        // cipher desynchronised at any boundary, a later packet would corrupt.
        let secret = [0x42u8; 16];
        let packets: Vec<Packet> = (0..12i32)
            .map(|i| (i, vec![i as u8; (i * 7 % 33) as usize + 1]))
            .collect();
        assert_roundtrip(&packets, None, Some(secret), &[1]).await;
        assert_roundtrip(&packets, Some(24), Some(secret), &[9, 1, 60]).await;
    }

    #[tokio::test]
    async fn mid_frame_close_surfaces_unexpected_close() {
        // Server sends a complete packet, then only a partial second packet's
        // bytes, then closes. The completed packet reads; the truncated one must
        // be a distinguishable error, not a hang or a silent EOF.
        let packets = vec![(0x00, b"complete".to_vec()), (0x01, vec![0x99; 50])];
        let wire = encode_wire(&packets, None, None).await;
        // Cut the stream partway into the second packet.
        let cut = wire.len() - 10;
        let truncated = wire[..cut].to_vec();

        let (client_ws, mut server_ws) = ws_pair().await;
        let server = tokio::spawn(async move {
            server_ws.send(Message::Binary(truncated)).await.unwrap();
            server_ws.close(None).await.unwrap();
        });

        let mut conn = Connection::new(WsTransport::from_ws(client_ws));
        let (id, fields) = conn.read_packet().await.unwrap().unwrap();
        assert_eq!(id, 0x00);
        assert_eq!(fields, b"complete");
        let err = conn.read_packet().await.unwrap_err();
        assert!(
            matches!(err, NetError::UnexpectedClose(_)),
            "truncated tail must be UnexpectedClose, got {err:?}"
        );
        server.await.unwrap();
    }
}
