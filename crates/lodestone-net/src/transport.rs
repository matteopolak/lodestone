//! Async transport abstraction for packet connections.
//!
//! A [`Transport`] is any bidirectional byte stream. Defining it as a marker
//! trait over Tokio's [`AsyncRead`]/[`AsyncWrite`] (rather than a bespoke
//! read/write interface) means a real [`tokio::net::TcpStream`] and an in-memory
//! [`tokio::io::DuplexStream`] both satisfy it for free, keeping integration
//! tests hermetic and paving the way for an in-process singleplayer server.

use tokio::io::{AsyncRead, AsyncWrite, DuplexStream};

/// A bidirectional, async byte stream usable by a [`crate::Connection`].
///
/// This is a marker trait with a blanket implementation, so any type that is
/// [`AsyncRead`] + [`AsyncWrite`] + [`Unpin`] (+ [`Send`] off-wasm) is a
/// `Transport`. It is deliberately not object-safe-friendly beyond the
/// underlying traits; [`crate::Connection`] is generic over the transport to keep
/// dispatch static and avoid the object-safety limits of native async fns in
/// traits.
///
/// # The `Send` bound is target-conditional
///
/// Off-wasm the bound includes [`Send`], because the client driver is spawned
/// onto a multi-threaded runtime and the transport crosses threads. On
/// `wasm32` the browser's `WebSocket` (and the `Rc`/closures a web transport
/// needs) are `!Send`, and the runtime is single-threaded, so requiring `Send`
/// there would make a browser transport impossible to express without `unsafe`.
/// Relaxing it on wasm is sound precisely because that target never moves the
/// transport across threads. Native behaviour is unchanged.
#[cfg(not(target_arch = "wasm32"))]
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

/// A bidirectional, async byte stream usable by a [`crate::Connection`].
///
/// See the off-wasm definition for details; on `wasm32` the [`Send`] bound is
/// dropped because the browser's `WebSocket` is `!Send` and the runtime is
/// single-threaded.
#[cfg(target_arch = "wasm32")]
pub trait Transport: AsyncRead + AsyncWrite + Unpin {}

#[cfg(target_arch = "wasm32")]
impl<T: AsyncRead + AsyncWrite + Unpin> Transport for T {}

/// Default buffer size for [`memory_pair`] duplex streams (64 KiB each way).
pub const DEFAULT_MEMORY_BUFFER: usize = 64 * 1024;

/// Creates a connected pair of in-memory transports.
///
/// Bytes written to one half are readable from the other. This is the hermetic
/// substitute for a socket in tests and, later, for singleplayer.
#[must_use]
pub fn memory_pair() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(DEFAULT_MEMORY_BUFFER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn assert_transport<T: Transport>() {}

    #[test]
    fn duplex_and_tcp_are_transports() {
        assert_transport::<DuplexStream>();
        #[cfg(not(target_arch = "wasm32"))]
        assert_transport::<tokio::net::TcpStream>();
    }

    #[tokio::test]
    async fn memory_pair_moves_bytes_both_ways() {
        let (mut a, mut b) = memory_pair();
        a.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        b.write_all(b"pong").await.unwrap();
        let mut buf2 = [0u8; 4];
        a.read_exact(&mut buf2).await.unwrap();
        assert_eq!(&buf2, b"pong");
    }
}
