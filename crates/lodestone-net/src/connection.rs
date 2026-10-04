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

use lodestone_core::{Reader, State, Writer};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(not(target_arch = "wasm32"))]
use tokio::net::{TcpStream, ToSocketAddrs};

use crate::codec::Codec;
use crate::error::{NetError, Result};
use crate::transport::Transport;

/// Diagnostic instrument for the wasm32 join stall (browser singleplayer sitting
/// on "Joining world…" against a near-full `memory_pair` duplex).
///
/// A single global, monotonic sequence shared by every [`Connection`] on the
/// thread — there are exactly two on a wasm32 singleplayer session (the
/// server's write-heavy half and the client's read-heavy half), one process,
/// one thread. Interleaved `write:start`/`write:done`/`read:polled` lines
/// sharing this sequence are what proved the mechanism: the reader kept being
/// scheduled and kept draining (`read:polled` lines interleave with a pending
/// `write:start` and the write eventually completes) for hundreds of columns,
/// right up until the reader's own `read:polling` line stops appearing at all —
/// not because the duplex ran dry, but because the *caller* stopped asking to
/// read. The actual cause lived one layer up, in `lodestone-shell`'s
/// `net.rs`: an un-cfg-gated `tokio::time::timeout` around the client's inbound
/// `ClientEvent` drain hangs forever on its first poll on `wasm32` (no timer
/// driver is ever entered there), which backs up the bounded event channel,
/// which stops `Driver::run` from reading further packets, which is what
/// finally starves this duplex. Left in at `debug` level (silent at the
/// `Info` level `web/src/main.rs` configures) rather than removed, because
/// this class of stall reappears one layer at a time and the next instance
/// will want the same seq-numbered write/read trace. A counter, not a
/// duration, per this repo's own rule about timings taken on a shared machine.
#[cfg(target_arch = "wasm32")]
static NETBUF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(target_arch = "wasm32")]
fn netbuf_seq() -> u64 {
    NETBUF_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Size of the scratch buffer used per read from the transport.
const READ_CHUNK: usize = 8 * 1024;

/// Maximum raw read-ahead retained while the application is sending packets.
const MAX_WRITE_READ_AHEAD: usize = 8 * 1024 * 1024;

/// Rewrites an outgoing packet id for a connection whose protocol numbers its
/// packets differently from the id the writer produced. Given the connection
/// state and the id as built, it returns the id to put on the wire, or `None`
/// when the protocol has no such packet in that state.
#[derive(Clone)]
pub struct PacketIdMap(std::sync::Arc<dyn Fn(State, i32) -> Option<i32> + Send + Sync>);

impl PacketIdMap {
    /// Wraps a mapping function.
    #[must_use]
    pub fn new(map: impl Fn(State, i32) -> Option<i32> + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(map))
    }

    /// Applies the mapping.
    #[must_use]
    pub fn apply(&self, state: State, packet_id: i32) -> Option<i32> {
        (self.0)(state, packet_id)
    }
}

impl std::fmt::Debug for PacketIdMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PacketIdMap")
    }
}

/// An async, framed packet connection over a [`Transport`].
///
/// Generic over the transport so dispatch stays static; both TCP and in-memory
/// streams work without a `dyn` boundary.
#[derive(Debug)]
pub struct Connection<T: Transport> {
    transport: T,
    codec: Codec,
    scratch: Box<[u8]>,
    read_ahead: Vec<u8>,
    read_eof: bool,
    write_failure: Option<String>,
    outbound_ids: Option<PacketIdMap>,
}

impl<T: Transport> Connection<T> {
    /// Wraps an existing transport in a fresh connection (compression disabled).
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            codec: Codec::new(),
            scratch: vec![0u8; READ_CHUNK].into_boxed_slice(),
            read_ahead: Vec::new(),
            read_eof: false,
            write_failure: None,
            outbound_ids: None,
        }
    }

    /// Installs (or clears) the rewrite [`Self::write_packet_in`] applies to
    /// outgoing packet ids.
    pub fn set_outbound_ids(&mut self, map: Option<PacketIdMap>) {
        self.outbound_ids = map;
    }

    /// Writes one packet built under the base numbering while the connection is
    /// in `state`, rewriting its id through the installed [`PacketIdMap`].
    pub async fn write_packet_in(&mut self, state: State, packet_id: i32, fields: &[u8]) -> Result<()> {
        let packet_id = match &self.outbound_ids {
            Some(map) => map.apply(state, packet_id).ok_or(NetError::MalformedFrame(
                "outgoing packet has no id in this protocol and state",
            ))?,
            None => packet_id,
        };
        self.write_packet(packet_id, fields).await
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
        self.check_write_failure()?;
        let mut body = Writer::default();
        body.var_i32(packet_id);
        body.bytes(fields);

        let mut frame = Vec::new();
        self.codec.encode(body.as_slice(), &mut frame)?;
        #[cfg(target_arch = "wasm32")]
        let (seq, len) = (netbuf_seq(), frame.len());
        #[cfg(target_arch = "wasm32")]
        tracing::trace!(target: "netbuf", seq, len, "write:start");
        let mut offset = 0;
        if let Err(error) = std::future::poll_fn(|cx| self.poll_frame(cx, &frame, &mut offset)).await {
            self.write_failure = Some(error.to_string());
            return Err(error.into());
        }
        #[cfg(target_arch = "wasm32")]
        tracing::trace!(target: "netbuf", seq, done_seq = netbuf_seq(), len, "write:done");
        Ok(())
    }

    fn check_write_failure(&self) -> std::io::Result<()> {
        match &self.write_failure {
            Some(message) => Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, message.clone())),
            None => Ok(()),
        }
    }

    fn poll_frame(
        &mut self,
        cx: &mut Context<'_>,
        frame: &[u8],
        offset: &mut usize,
    ) -> Poll<std::io::Result<()>> {
        while *offset < frame.len() {
            match Pin::new(&mut self.transport).poll_write(cx, &frame[*offset..]) {
                Poll::Ready(Ok(0)) => return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into())),
                Poll::Ready(Ok(written)) => *offset += written,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => break,
            }
        }
        if *offset == frame.len() {
            match Pin::new(&mut self.transport).poll_flush(cx) {
                Poll::Ready(result) => return Poll::Ready(result),
                Poll::Pending => {}
            }
        }
        if self.read_eof {
            return Poll::Pending;
        }
        let remaining = MAX_WRITE_READ_AHEAD - self.read_ahead.len();
        let capacity = self.scratch.len().min(remaining + 1);
        let mut buf = tokio::io::ReadBuf::new(&mut self.scratch[..capacity]);
        match Pin::new(&mut self.transport).poll_read(cx, &mut buf) {
            Poll::Ready(Ok(())) => {
                let received = buf.filled();
                if received.is_empty() {
                    self.read_eof = true;
                } else if received.len() > remaining {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "connection write read-ahead exceeded its byte limit",
                    )));
                } else {
                    // Decode only after the caller applies any handshake state transition.
                    self.read_ahead.extend_from_slice(received);
                    tracing::trace!(target: "netbuf", bytes = received.len(),
                        buffered = self.read_ahead.len(), "write:read-ahead");
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    /// Reads the next packet's raw body (`[VarInt id][fields...]`) without
    /// interpreting it.
    ///
    /// Returns `Ok(None)` on a clean EOF at a frame boundary. This is the lever
    /// the client uses to skip unknown packets wholesale.
    pub async fn read_packet_raw(&mut self) -> Result<Option<Vec<u8>>> {
        self.check_write_failure()?;
        loop {
            if let Some(body) = self.codec.next_packet()? {
                return Ok(Some(body));
            }

            if !self.read_ahead.is_empty() {
                self.codec.feed(&self.read_ahead);
                self.read_ahead.clear();
                continue;
            }
            if self.read_eof {
                return match self.codec.buffered_len() {
                    0 => Ok(None),
                    buffered => Err(NetError::UnexpectedClose(buffered)),
                };
            }

            #[cfg(target_arch = "wasm32")]
            let poll_seq = netbuf_seq();
            #[cfg(target_arch = "wasm32")]
            tracing::trace!(target: "netbuf", seq = poll_seq, "read:polling");
            let n = self.transport.read(&mut self.scratch).await?;
            #[cfg(target_arch = "wasm32")]
            tracing::trace!(target: "netbuf", seq = poll_seq, done_seq = netbuf_seq(), n, "read:polled");
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
    use crate::transport::{DEFAULT_MEMORY_BUFFER, memory_pair};

    #[tokio::test(start_paused = true)]
    async fn write_only_control_deadlocks_before_resuming_reads() {
        let (a, b) = tokio::io::duplex(31);
        let mut left = Connection::new(a);
        let mut right = Connection::new(b);
        let left_wire = [&[0xfc, 0x01, 17][..], &[0x35; 251]].concat();
        let right_wire = [&[0x86, 0x03, 29][..], &[0xa7; 389]].concat();
        let exchange = async {
            tokio::join!(
                async {
                    left.transport.write_all(&left_wire).await.unwrap();
                    left.read_packet().await.unwrap()
                },
                async {
                    right.transport.write_all(&right_wire).await.unwrap();
                    right.read_packet().await.unwrap()
                },
            )
        };
        assert!(tokio::time::timeout(std::time::Duration::from_secs(1), exchange).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn simultaneous_writes_drain_both_directions() {
        let (a, b) = tokio::io::duplex(31);
        let mut left = Connection::new(a);
        let mut right = Connection::new(b);
        let exchange = async {
            let (a, b) = tokio::join!(
                async {
                    left.write_packet(17, &[0x35; 251]).await.unwrap();
                    left.read_packet().await.unwrap()
                },
                async {
                    right.write_packet(29, &[0xa7; 389]).await.unwrap();
                    right.read_packet().await.unwrap()
                },
            );
            assert_eq!(a, Some((29, vec![0xa7; 389])));
            assert_eq!(b, Some((17, vec![0x35; 251])));
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), exchange)
            .await
            .expect("both writers must drain inbound bytes while waiting for capacity");
    }

    #[tokio::test(start_paused = true)]
    async fn read_ahead_preserves_encrypted_compressed_packet_order() {
        let (a, b) = tokio::io::duplex(3);
        let mut left = Connection::new(a);
        let mut right = Connection::new(b);
        for conn in [&mut left, &mut right] {
            conn.set_compression(16);
            conn.enable_encryption(&[0x93; 16]).unwrap();
        }
        let exchange = async {
            for index in 0..3_u8 {
                let (a, b) = tokio::join!(
                    async {
                        left.write_packet(17, &[index; 251]).await.unwrap();
                        left.read_packet().await.unwrap()
                    },
                    async {
                        right.write_packet(29, &[index + 7; 389]).await.unwrap();
                        right.read_packet().await.unwrap()
                    },
                );
                assert_eq!(a, Some((29, vec![index + 7; 389])));
                assert_eq!(b, Some((17, vec![index; 251])));
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), exchange).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn read_ahead_waits_for_handshake_codec_transition() {
        let (a, mut peer) = tokio::io::duplex(1);
        let mut conn = Connection::new(a);
        let mut encrypted = [3, 0, 41, 0x9e];
        crate::crypto::Cfb8Cipher::new(&[0x73; 16]).unwrap().encrypt(&mut encrypted);
        let exchange = async {
            let (sent, ()) = tokio::join!(conn.write_packet(17, &[0x35; 251]), async {
                peer.write_all(&encrypted).await.unwrap();
                let mut wire = [0; 254];
                peer.read_exact(&mut wire).await.unwrap();
                assert_eq!(&wire[..3], &[0xfc, 0x01, 17]);
                assert_eq!(&wire[3..], &[0x35; 251]);
            });
            sent.unwrap();
            assert_eq!(conn.read_ahead, encrypted);
            conn.set_compression(16);
            conn.enable_encryption(&[0x73; 16]).unwrap();
            assert_eq!(conn.read_packet().await.unwrap(), Some((41, vec![0x9e])));
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), exchange).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn read_ahead_preserves_buffered_frames_and_half_close() {
        let (a, mut peer) = tokio::io::duplex(1);
        let mut conn = Connection::new(a);
        conn.codec.feed(&[2, 7, 0x23]);
        let exchange = async {
            let (sent, ()) = tokio::join!(conn.write_packet(17, &[0x35; 251]), async {
                peer.write_all(&[2, 9, 0x35]).await.unwrap();
                peer.shutdown().await.unwrap();
                tokio::task::yield_now().await;
                let mut wire = [0; 254];
                peer.read_exact(&mut wire).await.unwrap();
            });
            sent.unwrap();
            assert!(conn.read_eof, "the blocked writer must observe the half close");
            assert_eq!(conn.read_packet().await.unwrap(), Some((7, vec![0x23])));
            assert_eq!(conn.read_packet().await.unwrap(), Some((9, vec![0x35])));
            assert!(conn.read_packet().await.unwrap().is_none());
        };
        tokio::time::timeout(std::time::Duration::from_secs(1), exchange).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn read_ahead_overflow_poisoning_prevents_partial_frame_reuse() {
        let (a, mut peer) = tokio::io::duplex(31);
        let mut conn = Connection::new(a);
        conn.read_ahead.resize(MAX_WRITE_READ_AHEAD, 0);
        peer.write_all(&[0x61]).await.unwrap();
        let error = conn.write_packet(17, &[0x35; 251]).await.unwrap_err();
        assert!(matches!(error, NetError::Io(ref error) if error.kind() == std::io::ErrorKind::InvalidData));
        assert_eq!(conn.read_ahead.len(), MAX_WRITE_READ_AHEAD);
        assert!(matches!(conn.read_packet().await, Err(NetError::Io(_))));
        assert!(matches!(conn.write_packet(29, &[0x71]).await, Err(NetError::Io(_))));
    }

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

    /// Reproduces the shape of the browser join stall this repo localised to
    /// `write_packet`'s `write_all` against a near-full `memory_pair` duplex
    /// (real `LEVEL_CHUNK_WITH_LIGHT` packets at `view_radius = 9` measured
    /// ~55-63 KiB against the 64 KiB `DEFAULT_MEMORY_BUFFER`) — but for
    /// `write_packet`/`read_packet` themselves, not for the wasm32-only bug
    /// that turned out to be the actual cause. The real defect was one layer
    /// up (`lodestone-shell`'s `net.rs`: an un-cfg-gated `tokio::time::timeout`
    /// with no timer driver on `wasm32`, which stopped the reader from ever
    /// calling back in, which is what starved this duplex) — nothing wrong
    /// with the framing/backpressure code lived here. This test still earns
    /// its place as the buffer-contention regression guard `DESIGN.md`'s
    /// evidence standards ask for: it proves the *other* candidate mechanism
    /// (a genuinely undersized buffer, or a broken `write_all` retry loop)
    /// stays fixed, by sending a batch whose largest member is bigger than
    /// `DEFAULT_MEMORY_BUFFER` **itself** — only true incremental streaming
    /// (write what fits, wait for the reader to drain, write more) can ever
    /// deliver that; a `write_all` that assumed one poll would suffice could
    /// not.
    ///
    /// Pairwise-distinct sizes (never two equal, per this repo's own
    /// transposition rule) so a swapped or truncated frame cannot hide behind
    /// two packets that happened to be the same length.
    #[tokio::test]
    async fn a_batch_larger_than_the_buffer_drains_through_memory_pair() {
        let (server_io, client_io) = memory_pair();
        let mut server = Connection::new(server_io);
        let mut client = Connection::new(client_io);

        // Climbs from 50 KiB (inside the localisation's measured range) past
        // 70 KiB — i.e. past `DEFAULT_MEMORY_BUFFER` (64 KiB) outright — over
        // 40 pairwise-distinct sizes.
        let sizes: Vec<usize> = (0..40).map(|i: usize| 50_000 + i * 733).collect();
        assert!(
            sizes.iter().any(|&s| s > DEFAULT_MEMORY_BUFFER),
            "fixture must include a packet larger than the buffer itself, \
             or a `write_all` that merely fits everything in one poll would \
             pass this test for the wrong reason"
        );
        // And distinct, or a transposition between two same-sized packets
        // survives a byte-perfect round trip undetected (this repo's own
        // transposition rule).
        assert_eq!(
            sizes.iter().collect::<std::collections::HashSet<_>>().len(),
            sizes.len(),
            "fixture sizes must be pairwise-distinct"
        );

        let write_sizes = sizes.clone();
        let writer = tokio::spawn(async move {
            for (i, &size) in write_sizes.iter().enumerate() {
                // Fill with the index so a payload cannot be mistaken for a
                // neighbour's even if two happened to share a length.
                let payload = vec![(i % 256) as u8; size];
                server
                    .write_packet(i32::try_from(i).unwrap(), &payload)
                    .await
                    .expect("write_packet must stream a frame larger than the buffer");
            }
        });

        let mut mismatches: Vec<String> = Vec::new();
        for (i, &expected_size) in sizes.iter().enumerate() {
            let (id, fields) = client
                .read_packet()
                .await
                .expect("transport error while draining the batch")
                .unwrap_or_else(|| panic!("packet {i} never arrived (buffer/backpressure regression)"));
            if id != i32::try_from(i).unwrap() {
                mismatches.push(format!("packet {i}: id {id} != {i}"));
            }
            if fields.len() != expected_size {
                mismatches.push(format!(
                    "packet {i}: len {} != expected {expected_size}",
                    fields.len()
                ));
            }
            if fields.iter().any(|&b| b != (i % 256) as u8) {
                mismatches.push(format!("packet {i}: payload byte mismatch"));
            }
        }
        assert!(mismatches.is_empty(), "mismatches: {mismatches:#?}");

        writer.await.expect("writer task must not panic");
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
