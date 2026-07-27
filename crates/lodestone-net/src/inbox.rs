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

/// A FIFO byte buffer that concatenates received WebSocket frame payloads and
/// serves them into arbitrarily-sized [`ReadBuf`] reads.
///
/// Pushing frames of any size and reading in chunks of any size reproduces the
/// original byte stream exactly, regardless of how the two are aligned.
#[derive(Debug, Default)]
pub(crate) struct ByteInbox {
    buf: VecDeque<u8>,
}

impl ByteInbox {
    /// Appends the bytes of one received binary frame.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes);
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
}
