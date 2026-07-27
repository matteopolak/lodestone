//! Version-free chat **acknowledgement** state: the last-seen sliding window and
//! the signature cache that together keep a client from being disconnected.
//!
//! ## Why this exists (the disconnect nobody's test can see)
//!
//! Every *signed* player-chat message the server sends us is pushed onto a
//! server-side pending list, drained only by our acknowledgement offset. In
//! 26.2's `ServerGamePacketListenerImpl` (behavioural reference only), once that
//! list exceeds **4096** entries the server kicks us with
//! `multiplayer.disconnect.too_many_pending_chats`. A client that receives chat
//! but never acknowledges it therefore climbs monotonically to that ceiling — on
//! a populated server, a matter of hours; on a quiet one, never — which is
//! exactly why an integration test that trades a handful of messages passes while
//! the defect is wide open. The honest coverage for this is a **golden** unit
//! test of the tracker against hand-computed offsets and bit positions, not a
//! round trip (a round trip is satisfied by any wrong-but-self-consistent
//! scheme).
//!
//! Only messages carrying a **non-null signature** accumulate; system chat and
//! disguised chat are exempt. So a correct tracker keys strictly off *signed
//! player chat*.
//!
//! ## What lives here vs. what a version adapter owns
//!
//! This layer performs no cryptography and touches no packet bytes. A version
//! adapter decodes the wire (the *packed* signature form — an index into the
//! cache or a full 256-byte blob), verifies signatures, and drives these types:
//! it [`push`](MessageSignatureCache::push)es incoming bodies into the
//! [`MessageSignatureCache`] (so future packed ids resolve),
//! [`add_pending`](LastSeenTracker::add_pending)s each signed message it shows,
//! and encodes the [`LastSeenUpdate`] this layer produces into that version's
//! `chat` / `chat_command` / `chat_ack` packets. The 20-entry window, the
//! `offset` bookkeeping, the acknowledged bitset and the checksum are identical
//! across every protocol that has signed chat, so they belong here.
//!
//! The semantics below are re-derived from vanilla's `LastSeenMessagesTracker`,
//! `LastSeenMessages` and `MessageSignatureCache`; the implementation is original.

/// An opaque message signature. This layer needs only value-equality and a
/// checksum; the bytes themselves are meaningless to it (a real signature is a
/// fixed 256-byte Ed25519/RSA blob, but nothing here depends on the length).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageSignature {
    bytes: Vec<u8>,
}

impl MessageSignature {
    /// Wrap raw signature bytes.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self { bytes: bytes.into() }
    }

    /// The raw signature bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The Java-`Arrays.hashCode(byte[])`-compatible checksum vanilla folds into
    /// the last-seen update. The `31 * h + b` recurrence uses **signed** bytes
    /// (Java's `byte` is signed) and wrapping 32-bit arithmetic (Java `int`
    /// overflow wraps). Only the low byte is ever observed downstream, but we
    /// compute the full value faithfully.
    pub fn checksum(&self) -> i32 {
        let mut hash: i32 = 1;
        for &b in &self.bytes {
            hash = hash.wrapping_mul(31).wrapping_add(i32::from(b as i8));
        }
        hash
    }
}

impl From<&[u8]> for MessageSignature {
    fn from(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }
}

/// Fold a list of last-seen signatures into vanilla's one-byte checksum:
/// `checksum = 1; for e: checksum = 31*checksum + e.checksum();` truncated to a
/// byte, with `0` remapped to `1` (vanilla reserves `0` as "ignore checksum").
fn compute_checksum(entries: &[MessageSignature]) -> u8 {
    let mut checksum: i32 = 1;
    for entry in entries {
        checksum = checksum.wrapping_mul(31).wrapping_add(entry.checksum());
    }
    let byte = checksum as u8;
    if byte == 0 { 1 } else { byte }
}

/// The acknowledgement a client sends with (or instead of) a chat message: the
/// number of newly-tracked messages since the last update ([`offset`]), the
/// fixed-width bitset of which window slots are still populated, the running
/// [`checksum`], and the signatures those set bits correspond to.
///
/// [`offset`]: LastSeenUpdate::offset
/// [`checksum`]: LastSeenUpdate::checksum
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastSeenUpdate {
    /// Count of messages added to the window since the previous update was
    /// generated (or since construction). Consumed and reset each generation.
    pub offset: i32,
    /// One flag per window slot, in transmission order (index 0 = oldest still
    /// tracked). Length equals the tracker's capacity (20 on vanilla).
    pub acknowledged: Vec<bool>,
    /// The one-byte checksum over [`last_seen`](Self::last_seen).
    pub checksum: u8,
    /// The signatures for the set bits, oldest first. A version adapter both
    /// transmits these (packed against the cache) and signs outgoing chat over
    /// them.
    pub last_seen: Vec<MessageSignature>,
}

impl LastSeenUpdate {
    /// Pack [`acknowledged`](Self::acknowledged) into the fixed-width byte form
    /// vanilla puts on the wire: `ceil(capacity / 8)` bytes, **LSB-first** within
    /// each byte (bit *i* → byte `i / 8`, position `i % 8`), matching
    /// `FriendlyByteBuf.writeFixedBitSet` / `BitSet.toByteArray`.
    pub fn acknowledged_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![0u8; self.acknowledged.len().div_ceil(8)];
        for (i, &set) in self.acknowledged.iter().enumerate() {
            if set {
                bytes[i / 8] |= 1 << (i % 8);
            }
        }
        bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedEntry {
    signature: MessageSignature,
    /// `true` until an update transmits this entry; a still-pending entry can be
    /// retracted by [`LastSeenTracker::ignore_pending`] (i.e. a `delete_chat`).
    pending: bool,
}

/// The client's last-seen sliding window — a fixed-capacity ring of the most
/// recent signed messages, tracking which are acknowledged and how many have
/// arrived since the last acknowledgement was sent.
///
/// Vanilla uses a capacity of [`VANILLA_CAPACITY`](Self::VANILLA_CAPACITY) (20).
#[derive(Debug, Clone)]
pub struct LastSeenTracker {
    tracked: Vec<Option<TrackedEntry>>,
    tail: usize,
    offset: i32,
    last_tracked: Option<MessageSignature>,
}

impl LastSeenTracker {
    /// Vanilla's window size (`LastSeenMessages.LAST_SEEN_MESSAGES_MAX_LENGTH`).
    pub const VANILLA_CAPACITY: usize = 20;

    /// Vanilla's flush threshold: `markMessageAsProcessed` sends a standalone
    /// acknowledgement once more than this many messages are pending, which keeps
    /// the server-side pending count far below its 4096 disconnect ceiling.
    pub const ACK_THRESHOLD: i32 = 64;

    /// A tracker with vanilla's 20-slot window.
    pub fn vanilla() -> Self {
        Self::with_capacity(Self::VANILLA_CAPACITY)
    }

    /// A tracker with a custom window size. Panics if `capacity` is zero (the
    /// ring index arithmetic requires a non-empty buffer).
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "last-seen window capacity must be non-zero");
        Self {
            tracked: vec![None; capacity],
            tail: 0,
            offset: 0,
            last_tracked: None,
        }
    }

    /// The window capacity.
    pub fn capacity(&self) -> usize {
        self.tracked.len()
    }

    /// Number of messages added since the last update was generated.
    pub fn offset(&self) -> i32 {
        self.offset
    }

    /// Record a freshly-received signed message. `was_shown` is vanilla's flag
    /// for whether the message was actually displayed (a filtered/blocked message
    /// still advances the window but leaves an empty slot, so its position is
    /// preserved without acknowledging content).
    ///
    /// Returns `false` — recording nothing — when `signature` equals the most
    /// recently tracked one, matching vanilla's consecutive-duplicate guard.
    pub fn add_pending(&mut self, signature: MessageSignature, was_shown: bool) -> bool {
        if self.last_tracked.as_ref() == Some(&signature) {
            return false;
        }
        self.last_tracked = Some(signature.clone());
        let entry = was_shown.then(|| TrackedEntry {
            signature,
            pending: true,
        });
        self.add_entry(entry);
        true
    }

    fn add_entry(&mut self, entry: Option<TrackedEntry>) {
        let index = self.tail;
        self.tail = (index + 1) % self.tracked.len();
        self.offset += 1;
        self.tracked[index] = entry;
    }

    /// Retract a still-pending entry (a `delete_chat` for a message we have not
    /// yet acknowledged): the first pending slot with a matching signature is
    /// cleared, so the next update neither reports nor acknowledges it.
    pub fn ignore_pending(&mut self, signature: &MessageSignature) {
        for slot in &mut self.tracked {
            if let Some(entry) = slot
                && entry.pending
                && &entry.signature == signature
            {
                *slot = None;
                break;
            }
        }
    }

    /// Read and reset the pending offset.
    pub fn get_and_clear_offset(&mut self) -> i32 {
        std::mem::replace(&mut self.offset, 0)
    }

    /// Produce the acknowledgement for the current window and mark every reported
    /// entry as no-longer-pending (so a later `delete_chat` can no longer retract
    /// it). Slots are walked oldest-first starting at `tail`, so bit *i* of the
    /// result corresponds to the *i*-th oldest slot — the transmission order the
    /// server expects.
    pub fn generate_and_apply_update(&mut self) -> LastSeenUpdate {
        let offset = self.get_and_clear_offset();
        let capacity = self.tracked.len();
        let mut acknowledged = vec![false; capacity];
        let mut last_seen = Vec::with_capacity(capacity);

        for (i, ack) in acknowledged.iter_mut().enumerate() {
            let index = (self.tail + i) % capacity;
            if let Some(entry) = &mut self.tracked[index] {
                *ack = true;
                last_seen.push(entry.signature.clone());
                entry.pending = false;
            }
        }

        let checksum = compute_checksum(&last_seen);
        LastSeenUpdate {
            offset,
            acknowledged,
            checksum,
            last_seen,
        }
    }

    /// Record a shown/processed message and, matching vanilla's
    /// `markMessageAsProcessed`, return a standalone acknowledgement offset when
    /// the pending count crosses [`ACK_THRESHOLD`](Self::ACK_THRESHOLD). The
    /// caller sends that offset as this version's `chat_ack` packet. Returns
    /// `None` when nothing needs sending yet (including for a duplicate).
    pub fn mark_processed(&mut self, signature: MessageSignature, was_shown: bool) -> Option<i32> {
        if self.add_pending(signature, was_shown) && self.offset > Self::ACK_THRESHOLD {
            self.take_acknowledgement()
        } else {
            None
        }
    }

    /// Flush any pending acknowledgement offset (vanilla's `sendChatAcknowledgement`,
    /// called on the client's tick cadence): returns the offset to transmit, or
    /// `None` when there is nothing outstanding.
    pub fn take_acknowledgement(&mut self) -> Option<i32> {
        let offset = self.get_and_clear_offset();
        (offset > 0).then_some(offset)
    }
}

/// A bounded cache of recently-seen signatures, letting both peers replace a
/// full 256-byte signature with a small index on the wire. Incoming packed ids
/// are resolved with [`unpack`](Self::unpack); outgoing signatures are compressed
/// with [`pack`](Self::pack).
///
/// Vanilla's default capacity is [`DEFAULT_CAPACITY`](Self::DEFAULT_CAPACITY) (128).
#[derive(Debug, Clone)]
pub struct MessageSignatureCache {
    entries: Vec<Option<MessageSignature>>,
}

impl MessageSignatureCache {
    /// Vanilla's default cache size.
    pub const DEFAULT_CAPACITY: usize = 128;

    /// A cache with vanilla's default capacity.
    pub fn vanilla() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    /// A cache with a custom capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: vec![None; capacity],
        }
    }

    /// The index a signature is currently stored at, or `None` if absent
    /// (vanilla's `NOT_FOUND`). A version adapter sends this index in place of the
    /// full signature when present.
    pub fn pack(&self, signature: &MessageSignature) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.as_ref() == Some(signature))
    }

    /// The signature stored at `id`, if any.
    pub fn unpack(&self, id: usize) -> Option<&MessageSignature> {
        self.entries.get(id).and_then(Option::as_ref)
    }

    /// Insert the signatures a received message referenced — its `last_seen`
    /// entries followed by the message's own signature — at the front of the
    /// cache, evicting the oldest and preserving vanilla's exact reordering:
    /// entries already present are not duplicated, and survivors are pushed back
    /// in order.
    pub fn push(&mut self, last_seen: &[MessageSignature], signature: Option<&MessageSignature>) {
        let mut queue: std::collections::VecDeque<MessageSignature> = last_seen.iter().cloned().collect();
        if let Some(sig) = signature {
            queue.push_back(sig.clone());
        }
        if queue.is_empty() {
            return;
        }

        let new_entries: std::collections::HashSet<MessageSignature> = queue.iter().cloned().collect();
        let capacity = self.entries.len();
        let mut i = 0;
        while !queue.is_empty() && i < capacity {
            let displaced = self.entries[i].take();
            // `removeLast` — the most recently queued signature lands first.
            self.entries[i] = queue.pop_back();
            if let Some(entry) = displaced
                && !new_entries.contains(&entry)
            {
                queue.push_front(entry);
            }
            i += 1;
        }
    }
}
