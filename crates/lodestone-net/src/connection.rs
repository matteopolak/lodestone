//! Async packet [`Connection`] combining a [`Transport`] with a [`Codec`].
//!
//! Layering, from outermost to innermost: length framing wraps compression,
//! which wraps `[VarInt packet id][fields...]`. The connection owns that whole
//! stack and exposes packets as `(packet_id, body)` pairs, plus a raw variant
//! that lets the client layer skip packets it does not understand without ever
//! parsing them.
//!
//! # Protocol-blind invariant
//!
//! This layer never interprets a packet beyond splitting off its leading
//! packet-id VarInt; the field bytes are opaque. That byte-transparency is what
//! makes a *single* connection/relay serve every protocol version and every
//! server — the moment it parses a field it becomes a per-version component and
//! we would need one per version. **Do not** add a `match packet_id { .. }` or
//! any field-level validation here; that belongs in the version adapter. The
//! `codec_is_protocol_blind` integration test guards this boundary by pushing
//! ids and bodies that are invalid under every real schema and asserting they
//! survive a round trip untouched — it fails the instant a special case is
//! introduced.

use lodestone_core::{Reader, Writer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(not(target_arch = "wasm32"))]
use tokio::net::{TcpStream, ToSocketAddrs};

use crate::codec::Codec;
use crate::error::{NetError, Result};
use crate::transport::Transport;

/// Size of the scratch buffer used per read from the transport.
const READ_CHUNK: usize = 8 * 1024;

/// An async, framed packet connection over a [`Transport`].
///
/// Generic over the transport so dispatch stays static; both TCP and in-memory
/// streams work without a `dyn` boundary.
#[derive(Debug)]
pub struct Connection<T: Transport> {
    transport: T,
    codec: Codec,
    scratch: Box<[u8]>,
}

impl<T: Transport> Connection<T> {
    /// Wraps an existing transport in a fresh connection (compression disabled).
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            codec: Codec::new(),
            scratch: vec![0u8; READ_CHUNK].into_boxed_slice(),
        }
    }

    /// Enables or disables compression, mirroring `login_compression`.
    ///
    /// A negative `threshold` disables compression.
    pub fn set_compression(&mut self, threshold: i32) {
        self.codec.set_compression(threshold);
    }

    /// Returns the active compression threshold, or `None` when disabled.
    #[must_use]
    pub fn compression_threshold(&self) -> Option<usize> {
        self.codec.compression_threshold()
    }

    /// Enables AES-128-CFB8 encryption for both directions from the shared
    /// secret.
    ///
    /// Call this at exactly the right moment in the online-mode handshake: after
    /// the `EncryptionResponse` bytes have been handed to [`Connection::write_packet`]
    /// (so that packet goes out in cleartext), and before reading the next
    /// inbound packet (the server switches its cipher on the instant it accepts
    /// the response, so the very next byte it sends is enciphered). Getting this
    /// ordering wrong is the classic point where online-mode login breaks.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::EncryptionAlreadyEnabled`] if called twice or
    /// [`NetError::BadSharedSecret`] if `secret` is not 16 bytes.
    pub fn enable_encryption(&mut self, secret: &[u8]) -> Result<()> {
        self.codec.enable_encryption(secret)
    }

    /// Returns whether encryption has been enabled.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.codec.is_encrypted()
    }

    /// Consumes the connection and returns the underlying transport.
    pub fn into_inner(self) -> T {
        self.transport
    }

    /// Writes one packet given its id and field bytes.
    ///
    /// The id is prepended as a VarInt inside the compressed region, then the
    /// whole body is framed by the codec.
    pub async fn write_packet(&mut self, packet_id: i32, fields: &[u8]) -> Result<()> {
        let mut body = Writer::default();
        body.var_i32(packet_id);
        body.bytes(fields);

        let mut frame = Vec::new();
        self.codec.encode(body.as_slice(), &mut frame)?;
        self.transport.write_all(&frame).await?;
        self.transport.flush().await?;
        Ok(())
    }

    /// Reads the next packet's raw body (`[VarInt id][fields...]`) without
    /// interpreting it.
    ///
    /// Returns `Ok(None)` on a clean EOF at a frame boundary. This is the lever
    /// the client uses to skip unknown packets wholesale.
    pub async fn read_packet_raw(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(body) = self.codec.next_packet()? {
                return Ok(Some(body));
            }

            let n = self.transport.read(&mut self.scratch).await?;
            if n == 0 {
                let buffered = self.codec.buffered_len();
                if buffered == 0 {
                    return Ok(None);
                }
                return Err(NetError::UnexpectedClose(buffered));
            }
            self.codec.feed(&self.scratch[..n]);
        }
    }

    /// Reads the next packet and splits it into `(packet_id, fields)`.
    ///
    /// Returns `Ok(None)` on a clean EOF at a frame boundary.
    pub async fn read_packet(&mut self) -> Result<Option<(i32, Vec<u8>)>> {
        loop {
            let Some(body) = self.read_packet_raw().await? else {
                return Ok(None);
            };
            // An **empty** frame body is skipped, not an error.
            //
            // In compression mode a one-byte frame of `0x00` declares
            // "uncompressed, zero bytes of packet data" — no packet id at all.
            // Reading an id out of it raises `UnexpectedEof`, which surfaces as
            // `NetError::Codec` and so as a **fatal transport error**: the client
            // driver fails open on an adapter decode error but never on a
            // transport one. That is a whole session lost to one junk frame.
            //
            // Vanilla tolerates it, and worth knowing *how*, because there is no
            // explicit guard to point at: `Varint21FrameDecoder` rejects only a
            // zero *length*, `CompressionDecoder` turns `uncompressedLength == 0`
            // into `in.readBytes(in.readableBytes())` — an empty buffer — and
            // `PacketDecoder` has no empty check. It never needs one, because
            // netty's `ByteToMessageDecoder` only calls `decode` while the buffer
            // `isReadable()`, so an empty one is silently dropped before
            // `PacketDecoder` ever sees it. The tolerance is a property of the
            // pipeline, not of the packet code — which is exactly why reading the
            // packet classes alone suggests vanilla would die here too.
            //
            // Measured against a live Velocity proxy (protocol 776, compression
            // threshold 256): it emits exactly this frame, and it is what ended
            // sessions with `protocol codec error: unexpected end of input` right
            // after the lobby inventory arrived. The item-component warnings in
            // that log were a coincidence of timing, not the cause.
            if body.is_empty() {
                continue;
            }
            let mut reader = Reader::new(&body);
            let packet_id = reader.var_i32()?;
            let fields = reader.remaining_bytes().to_vec();
            return Ok(Some((packet_id, fields)));
        }
    }

    /// Flushes and cleanly half-closes the write side (graceful disconnect).
    ///
    /// After this returns, the peer's next read observes end-of-stream — a clean
    /// `Ok(None)` from its own [`read_packet`](Self::read_packet) — rather than a
    /// connection reset. That is the sending side of the disconnect taxonomy: it
    /// lets a peer distinguish "the other end closed deliberately" from "the
    /// socket errored mid-frame" ([`NetError::UnexpectedClose`]) or "we timed
    /// out" ([`NetError::Timeout`]). Already-buffered inbound packets can still
    /// be drained by continuing to read.
    ///
    /// Works on every transport, including the browser WebSocket, since it only
    /// uses the `AsyncWrite` shutdown contract.
    ///
    /// # Errors
    ///
    /// Propagates any [`NetError::Io`] from flushing or shutting down the
    /// transport.
    pub async fn shutdown(&mut self) -> Result<()> {
        self.transport.flush().await?;
        self.transport.shutdown().await?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Transport> Connection<T> {
    /// Reads the next packet, failing with [`NetError::Timeout`] if no full
    /// packet arrives within `timeout`.
    ///
    /// This lets a client distinguish "we timed out waiting" from the two clean
    /// outcomes ([`Ok(None)`] for a server-closed socket at a frame boundary, and
    /// a normal packet the caller then interprets — e.g. a disconnect-with-reason
    /// the protocol layer parses). The transport itself stays protocol-blind.
    ///
    /// Native-only: `tokio::time` is unavailable on `wasm32`, where the browser
    /// event loop already bounds waits.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Timeout`] on expiry; otherwise propagates the same
    /// errors as [`read_packet`](Self::read_packet).
    pub async fn read_packet_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<(i32, Vec<u8>)>> {
        match tokio::time::timeout(timeout, self.read_packet()).await {
            Ok(result) => result,
            Err(_) => Err(NetError::Timeout {
                operation: "read",
                seconds: timeout.as_secs(),
            }),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Connection<TcpStream> {
    /// Opens a TCP connection to `addr` and wraps it (compression disabled).
    ///
    /// Nagle's algorithm is disabled so small handshake packets are not delayed.
    ///
    /// Not available on `wasm32`, which cannot open raw TCP sockets; a browser
    /// build reaches a server through a WebSocket [`Transport`] instead (see the
    /// `ws-web` feature and the WebSocket relay).
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        Ok(Self::new(stream))
    }

    /// Opens a TCP connection to `addr`, failing with [`NetError::Timeout`] if it
    /// does not complete within `timeout`.
    ///
    /// A bare [`TcpStream::connect`] can hang for the OS default (often ~30–90s)
    /// against a black-holed address, which is far too long for an interactive
    /// client. This bounds that wait explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Timeout`] on expiry, or [`NetError::Io`] on a connect
    /// or socket-option failure.
    pub async fn connect_timeout<A: ToSocketAddrs>(
        addr: A,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        match tokio::time::timeout(timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                stream.set_nodelay(true)?;
                Ok(Self::new(stream))
            }
            Ok(Err(e)) => Err(NetError::Io(e)),
            Err(_) => Err(NetError::Timeout {
                operation: "connect",
                seconds: timeout.as_secs(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::memory_pair;

    #[tokio::test]
    async fn write_then_read_uncompressed() {
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);

        client.write_packet(0x00, &[1, 2, 3]).await.unwrap();
        let (id, fields) = server.read_packet().await.unwrap().unwrap();
        assert_eq!(id, 0x00);
        assert_eq!(fields, vec![1, 2, 3]);
    }

    /// A one-byte `0x00` frame in compression mode carries **no packet data at
    /// all**, and it must be skipped rather than ending the connection.
    ///
    /// This is a measured frame, not a hypothetical: a live Velocity proxy at
    /// protocol 776 with threshold 256 emits it, and reading a packet id out of
    /// it raised `NetError::Codec(UnexpectedEof)` — a *transport* error, which
    /// the client driver treats as fatal (it fails open only on adapter decode
    /// errors). Whole sessions were lost to it.
    ///
    /// The trailing real packet is the load-bearing part: without it the test
    /// could pass on a `read_packet` that simply returned `Ok(None)` and hung up,
    /// which is the same session loss wearing a clean exit. The pairwise-distinct
    /// id and body prove the *next* frame is delivered intact, i.e. the empty one
    /// was skipped rather than resynchronised past.
    #[tokio::test]
    async fn an_empty_compressed_frame_is_skipped_not_fatal() {
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);
        client.set_compression(256);
        server.set_compression(256);

        // Hand-built, because no encoder here will produce it: frame length 1,
        // then the single `0x00` uncompressed-length VarInt and nothing else.
        client.transport.write_all(&[0x01, 0x00]).await.unwrap();
        client.transport.flush().await.unwrap();
        client.write_packet(0x26, &[0x11, 0x04]).await.unwrap();

        let (id, fields) = server
            .read_packet()
            .await
            .expect("an empty frame must not be a transport error")
            .expect("nor a clean end of stream: that loses the session just as surely");
        assert_eq!(id, 0x26, "the frame after the empty one is delivered");
        assert_eq!(fields, vec![0x11, 0x04]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(start_paused = true)]
    async fn connect_timeout_fires_on_blackhole() {
        // 10.255.255.1 is a routable private address with (in a lab) no host, so
        // the SYN goes unanswered and the connect pends. With the clock paused,
        // tokio auto-advances to the timeout deadline, so this is deterministic
        // and takes no real time.
        let err =
            Connection::connect_timeout("10.255.255.1:65000", std::time::Duration::from_secs(3))
                .await
                .unwrap_err();
        assert!(
            matches!(
                err,
                NetError::Timeout {
                    operation: "connect",
                    ..
                }
            ),
            "expected connect timeout, got {err:?}"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn connect_timeout_succeeds_within_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let conn = Connection::connect_timeout(addr, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        drop(conn);
        accept.await.unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test(start_paused = true)]
    async fn read_packet_timeout_fires_when_peer_is_silent() {
        // The peer connects but never sends. With the clock paused, tokio
        // auto-advances to the deadline, so a silent peer deterministically
        // yields a read Timeout rather than hanging.
        let (a, b) = memory_pair();
        let _keep = a; // hold the write half open so this is a stall, not EOF
        let mut server = Connection::new(b);
        let err = server
            .read_packet_timeout(std::time::Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                NetError::Timeout {
                    operation: "read",
                    ..
                }
            ),
            "expected read timeout, got {err:?}"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn read_packet_timeout_returns_packet_within_budget() {
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);
        client.write_packet(0x11, &[7, 8, 9]).await.unwrap();
        let (id, fields) = server
            .read_packet_timeout(std::time::Duration::from_secs(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(id, 0x11);
        assert_eq!(fields, vec![7, 8, 9]);
    }

    #[tokio::test]
    async fn read_packet_raw_preserves_id_prefix() {
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);

        client.write_packet(0x2A, &[9, 9]).await.unwrap();
        let raw = server.read_packet_raw().await.unwrap().unwrap();
        // Raw body starts with the packet-id VarInt (0x2A) then the fields.
        assert_eq!(raw, vec![0x2A, 9, 9]);
    }

    #[tokio::test]
    async fn clean_eof_returns_none() {
        let (a, b) = memory_pair();
        let client = Connection::new(a);
        let mut server = Connection::new(b);
        drop(client); // closes the write half
        assert!(server.read_packet().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn graceful_shutdown_drains_then_eofs() {
        // A deliberate `shutdown()` must let the peer read every packet already
        // sent and *then* observe a clean EOF — distinguishable from the
        // mid-frame `UnexpectedClose` a reset produces.
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);

        client.write_packet(0x00, &[1, 2, 3]).await.unwrap();
        client.write_packet(0x01, &[4, 5]).await.unwrap();
        client.shutdown().await.unwrap();

        let (id0, f0) = server.read_packet().await.unwrap().unwrap();
        assert_eq!((id0, f0), (0x00, vec![1, 2, 3]));
        let (id1, f1) = server.read_packet().await.unwrap().unwrap();
        assert_eq!((id1, f1), (0x01, vec![4, 5]));
        assert!(
            server.read_packet().await.unwrap().is_none(),
            "graceful shutdown must surface as clean EOF, not an error"
        );
    }

    #[tokio::test]
    async fn mid_frame_eof_is_error() {
        let (mut a, b) = memory_pair();
        let mut server = Connection::new(b);
        // Announce a 5-byte frame but only send 2 bytes, then close.
        AsyncWriteExt::write_all(&mut a, &[0x05, 0x00, 0x01])
            .await
            .unwrap();
        drop(a);
        assert!(matches!(
            server.read_packet().await,
            Err(NetError::UnexpectedClose(_))
        ));
    }

    #[tokio::test]
    async fn compressed_packets_roundtrip_over_transport() {
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);
        client.set_compression(16);
        server.set_compression(16);

        let big = vec![7u8; 500];
        client.write_packet(0x10, &big).await.unwrap();
        let (id, fields) = server.read_packet().await.unwrap().unwrap();
        assert_eq!(id, 0x10);
        assert_eq!(fields, big);
    }

    #[tokio::test]
    async fn encrypted_then_compressed_stream_roundtrips() {
        // Mirror a real login: a couple of cleartext packets, then enable
        // encryption on both ends, then compression, then a burst of packets.
        let (a, b) = memory_pair();
        let mut client = Connection::new(a);
        let mut server = Connection::new(b);
        let secret = [0x5au8; 16];

        client.write_packet(0x00, b"hello").await.unwrap();
        assert_eq!(
            server.read_packet().await.unwrap().unwrap(),
            (0x00, b"hello".to_vec())
        );

        client.enable_encryption(&secret).unwrap();
        server.enable_encryption(&secret).unwrap();
        assert!(client.is_encrypted() && server.is_encrypted());

        client.set_compression(32);
        server.set_compression(32);

        let payloads: Vec<(i32, Vec<u8>)> = vec![
            (0x01, vec![1, 2, 3]),
            (0x24, vec![8; 200]),
            (0x7f, (0..50u8).collect()),
        ];
        for (id, body) in &payloads {
            client.write_packet(*id, body).await.unwrap();
        }
        for expected in &payloads {
            assert_eq!(&server.read_packet().await.unwrap().unwrap(), expected);
        }
    }
}
