//! Shared read-side reassembly for the WebSocket transports.
//!
//! A WebSocket delivers **messages**, but a Minecraft connection is a **byte
//! stream**: a single length-prefixed packet may arrive split across two binary
//! frames, and several packets may be coalesced into one frame. A message
//! boundary has nothing to do with a packet boundary. If the transport ever let
//! a message boundary leak upward, the [`Codec`](crate::Codec) above would
//! mis-frame — the same class of bug as an RCON server doing one `read()` per
//! request and closing on a split frame.
//!
//! [`ByteInbox`] is the one place that reassembly happens. It is pure (no I/O,
//! no async, no target-specific types beyond Tokio's [`ReadBuf`], which exists
//! on wasm too), so both the native [`WsTransport`](crate::WsTransport) and the
//! browser `WsWebTransport` share the exact same, exhaustively-tested logic and
//! the browser path gets it for free.

use std::collections::VecDeque;

use tokio::io::ReadBuf;

/// A bounded byte window used by transports that need explicit flow control.
///
/// The window is directional: a sender starts with no credit and receives
/// credit as the peer drains bytes; a receiver starts with its full capacity
/// and spends credit when bytes arrive. Keeping this accounting independent of
/// the JavaScript transport makes the partial-write and over-credit rules
/// testable on every target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ByteCreditWindow {
    limit: usize,
    available: usize,
}

impl ByteCreditWindow {
    /// Creates an empty window. This is the sender side before the peer's
    /// initial grant arrives.
    pub(crate) const fn empty(limit: usize) -> Self {
        Self { limit, available: 0 }
    }

    /// Creates a full window. This is the receiver side before any bytes have
    /// arrived.
    pub(crate) const fn full(limit: usize) -> Self {
        Self { limit, available: limit }
    }

    /// Returns the currently spendable byte count.
    pub(crate) const fn available(self) -> usize {
        self.available
    }

    /// Spends up to `requested` bytes and returns the amount granted. A
    /// partial grant is intentional: `AsyncWrite::poll_write` can then make
    /// progress without ever exceeding the peer's receive window.
    pub(crate) fn take(&mut self, requested: usize) -> usize {
        let granted = requested.min(self.available);
        self.available -= granted;
        granted
    }

    /// Spends exactly `bytes`, rejecting a payload larger than the remaining
    /// receive credit.
    pub(crate) fn consume_exact(&mut self, bytes: usize) -> Result<(), CreditError> {
        if bytes > self.available {
            return Err(CreditError::Exceeded {
                available: self.available,
                requested: bytes,
            });
        }
        self.available -= bytes;
        Ok(())
    }

    /// Returns drained bytes to the window, rejecting a peer that grants more
    /// than the configured capacity. This also detects duplicated credit
    /// messages rather than allowing the sender to grow without bound.
    pub(crate) fn release(&mut self, bytes: usize) -> Result<(), CreditError> {
        let Some(available) = self.available.checked_add(bytes) else {
            return Err(CreditError::Overflow {
                available: self.available,
                returned: bytes,
                limit: self.limit,
            });
        };
        if available > self.limit {
            return Err(CreditError::Overflow {
                available: self.available,
                returned: bytes,
                limit: self.limit,
            });
        }
        self.available = available;
        Ok(())
    }
}

/// A malformed or out-of-window credit operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CreditError {
    /// A payload consumed more credit than remained.
    Exceeded { available: usize, requested: usize },
    /// A credit grant would exceed the configured receive capacity.
    Overflow { available: usize, returned: usize, limit: usize },
}

/// Details returned when a bounded inbox cannot accept a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InboxOverflow {
    pub(crate) buffered: usize,
    pub(crate) incoming: usize,
    pub(crate) capacity: usize,
}

/// A FIFO byte buffer that concatenates received WebSocket frame payloads and
/// serves them into arbitrarily-sized [`ReadBuf`] reads.
///
/// Pushing frames of any size and reading in chunks of any size reproduces the
/// original byte stream exactly, regardless of how the two are aligned.
#[derive(Debug)]
pub(crate) struct ByteInbox {
    buf: VecDeque<u8>,
    /// `usize::MAX` is the legacy unbounded mode used by the WebSocket
    /// adapters. The MessagePort adapter opts into an explicit finite limit.
    capacity: usize,
}

impl Default for ByteInbox {
    fn default() -> Self {
        Self { buf: VecDeque::new(), capacity: usize::MAX }
    }
}

impl ByteInbox {
    /// Creates a FIFO with a finite byte capacity.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self { buf: VecDeque::new(), capacity }
    }

    /// Appends the bytes of one received binary frame.
    #[cfg(any(test, feature = "ws-native", feature = "ws-web"))]
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
    }

    /// Appends a frame only if it fits in the configured capacity.
    pub(crate) fn try_push(&mut self, bytes: &[u8]) -> Result<(), InboxOverflow> {
        if bytes.len() > self.capacity.saturating_sub(self.buf.len()) {
            return Err(InboxOverflow {
                buffered: self.buf.len(),
                incoming: bytes.len(),
                capacity: self.capacity,
            });
        }
        self.buf.extend(bytes);
        Ok(())
    }

    /// Returns the number of bytes that can be accepted without exceeding the
    /// configured capacity.
    pub(crate) fn remaining_capacity(&self) -> usize {
        self.capacity.saturating_sub(self.buf.len())
    }

    /// Returns the number of buffered, not-yet-served bytes. Test-only: the
    /// transports drive draining via [`serve`](Self::serve) and
    /// [`is_empty`](Self::is_empty) and never need the exact count.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    /// Returns whether there are no buffered bytes to serve.
    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Serves up to `buf.remaining()` buffered bytes into `buf`, returning how
    /// many were written.
    ///
    /// Bytes are taken from the front in order; a serve smaller than the buffer
    /// leaves the remainder for the next call, which is exactly how a reader
    /// draining one coalesced frame across several `poll_read`s behaves.
    pub(crate) fn serve(&mut self, buf: &mut ReadBuf<'_>) -> usize {
        let want = buf.remaining();
        if want == 0 || self.buf.is_empty() {
            return 0;
        }
        // `VecDeque` may store the bytes as two contiguous runs; copy from each
        // without allocating an intermediate buffer.
        let (front, back) = self.buf.as_slices();
        let take_front = front.len().min(want);
        buf.put_slice(&front[..take_front]);
        let mut served = take_front;
        if served < want {
            let take_back = back.len().min(want - served);
            buf.put_slice(&back[..take_back]);
            served += take_back;
        }
        self.buf.drain(..served);
        served
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drains the whole inbox in reads of `chunk` bytes each, returning the
    /// reconstructed stream.
    fn drain_in_chunks(inbox: &mut ByteInbox, chunk: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut scratch = vec![0u8; chunk];
        loop {
            let mut rb = ReadBuf::new(&mut scratch);
            let n = inbox.serve(&mut rb);
            if n == 0 {
                break;
            }
            out.extend_from_slice(rb.filled());
        }
        out
    }

    #[test]
    fn single_frame_served_whole() {
        let mut inbox = ByteInbox::default();
        inbox.push(&[1, 2, 3, 4]);
        assert_eq!(inbox.len(), 4);
        let got = drain_in_chunks(&mut inbox, 16);
        assert_eq!(got, vec![1, 2, 3, 4]);
        assert!(inbox.is_empty());
    }

    #[test]
    fn many_frames_coalesced_into_one_read() {
        // Several "packets" pushed as separate frames must read back as one
        // contiguous stream.
        let mut inbox = ByteInbox::default();
        inbox.push(&[1, 2]);
        inbox.push(&[3]);
        inbox.push(&[4, 5, 6]);
        let got = drain_in_chunks(&mut inbox, 64);
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn one_frame_split_across_tiny_reads() {
        // A single frame drained one byte at a time — the split-read trap.
        let mut inbox = ByteInbox::default();
        inbox.push(&[10, 20, 30, 40, 50]);
        let got = drain_in_chunks(&mut inbox, 1);
        assert_eq!(got, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn interleaved_pushes_and_partial_serves() {
        // Serve part of a frame, push more, keep serving: the boundary between
        // pushes must be invisible to the reader.
        let mut inbox = ByteInbox::default();
        inbox.push(&[1, 2, 3]);
        let mut s = [0u8; 2];
        let mut rb = ReadBuf::new(&mut s);
        assert_eq!(inbox.serve(&mut rb), 2);
        assert_eq!(rb.filled(), &[1, 2]);
        // One byte left; add another frame behind it.
        inbox.push(&[4, 5]);
        let rest = drain_in_chunks(&mut inbox, 10);
        assert_eq!(rest, vec![3, 4, 5]);
    }

    #[test]
    fn serve_into_zero_capacity_is_noop() {
        let mut inbox = ByteInbox::default();
        inbox.push(&[9]);
        let mut empty: [u8; 0] = [];
        let mut rb = ReadBuf::new(&mut empty);
        assert_eq!(inbox.serve(&mut rb), 0);
        assert_eq!(inbox.len(), 1);
    }

    #[test]
    fn serve_from_empty_is_zero() {
        let mut inbox = ByteInbox::default();
        let mut s = [0u8; 4];
        let mut rb = ReadBuf::new(&mut s);
        assert_eq!(inbox.serve(&mut rb), 0);
    }

    #[test]
    fn wraparound_two_run_layout_is_served_correctly() {
        // Force the VecDeque into a two-run (wrapped) layout, then serve across
        // the internal seam in one read to exercise the `back` slice path.
        let mut inbox = ByteInbox::default();
        inbox.push(&[1, 2, 3, 4]);
        // Consume 3, leaving head advanced.
        let mut s3 = [0u8; 3];
        let mut rb = ReadBuf::new(&mut s3);
        inbox.serve(&mut rb);
        // Push enough to wrap around the ring.
        inbox.push(&[5, 6, 7, 8, 9]);
        let got = drain_in_chunks(&mut inbox, 100);
        assert_eq!(got, vec![4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn buffered_bytes_survive_until_eof_can_be_reported() {
        // A transport checks `inbox` before its closed flag. This is the
        // control for that ordering: a port may close in the same turn that
        // delivered its final bytes, and EOF is correct only after those bytes
        // have been handed to the protocol reader.
        let mut inbox = ByteInbox::default();
        inbox.push(&[0x02, 0x7f]);
        assert!(!inbox.is_empty(), "a closing transport must drain first");
        assert_eq!(drain_in_chunks(&mut inbox, 1), vec![0x02, 0x7f]);
        assert!(inbox.is_empty(), "only an empty inbox may yield EOF");
    }

    #[test]
    fn bounded_inbox_rejects_overflow_without_dropping_buffered_bytes() {
        let mut inbox = ByteInbox::with_capacity(4);
        inbox.try_push(&[1, 2, 3]).unwrap();
        assert_eq!(inbox.remaining_capacity(), 1);
        assert_eq!(
            inbox.try_push(&[4, 5]),
            Err(InboxOverflow { buffered: 3, incoming: 2, capacity: 4 })
        );
        assert_eq!(drain_in_chunks(&mut inbox, 8), vec![1, 2, 3]);
    }

    #[test]
    fn credit_window_grants_partial_writes_and_reopens_after_drain() {
        let mut send = ByteCreditWindow::empty(4);
        assert_eq!(send.take(7), 0);
        send.release(4).unwrap();
        assert_eq!(send.take(7), 4);
        assert_eq!(send.available(), 0);
        assert_eq!(send.take(1), 0);
        send.release(2).unwrap();
        assert_eq!(send.take(7), 2);
        assert_eq!(send.available(), 0);
    }

    #[test]
    fn credit_window_rejects_over_credit_and_over_consumption() {
        let mut receiver = ByteCreditWindow::full(4);
        receiver.consume_exact(3).unwrap();
        assert_eq!(
            receiver.consume_exact(2),
            Err(CreditError::Exceeded { available: 1, requested: 2 })
        );
        receiver.release(3).unwrap();
        assert_eq!(
            receiver.release(2),
            Err(CreditError::Overflow { available: 4, returned: 2, limit: 4 })
        );
    }
}
