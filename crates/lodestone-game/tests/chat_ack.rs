//! Golden tests for the version-free chat acknowledgement machinery
//! ([`chat_ack`]). These are **goldens, not round trips**: every expected value
//! (offset, bit position, checksum byte, cache ordering) is derived by hand or by
//! an independent reference against vanilla's algorithm and baked as a literal, so
//! a wrong-but-self-consistent implementation fails instead of round-tripping
//! happily. The comment on each constant records how it was derived.
//!
//! Anti-vacuity: a `checked` counter is incremented at every assertion and a
//! floor is asserted at the end, so a future refactor that deletes assertions
//! trips the floor rather than silently reducing coverage.

use lodestone_game::chat_ack::{LastSeenTracker, MessageSignature, MessageSignatureCache};

fn sig(bytes: &[u8]) -> MessageSignature {
    MessageSignature::new(bytes.to_vec())
}

// ---------------------------------------------------------------------------
// Checksum goldens (independently computed; see module doc).
// MessageSignature::checksum == Java Arrays.hashCode(byte[]); the folded byte ==
// vanilla's own last-seen-messages checksum step, with 0 remapped to 1.
// ---------------------------------------------------------------------------

#[test]
fn signature_checksum_matches_java_arrays_hashcode() {
    // Arrays.hashCode([5]) = 31*1 + 5 = 36.
    assert_eq!(sig(&[5]).checksum(), 36);
    // Arrays.hashCode([1,2]) = 31*(31*1+1)+2 = 31*32+2 = 994.
    assert_eq!(sig(&[1, 2]).checksum(), 994);
    // Sign extension: byte 200 is -56 in Java. 31*1 + (-56) = -25.
    assert_eq!(sig(&[200]).checksum(), -25);
}

#[test]
fn last_seen_checksum_goldens() {
    // computeChecksum begins at 1; an empty list stays 1.
    assert_eq!(tracker_checksum(&[]), 1);
    // [5]: 31*1 + 36 = 67.
    assert_eq!(tracker_checksum(&[&[5]]), 67);
    // [200]: 31*1 + (-25) = 6 (the low byte is unaffected by sign extension,
    // but we compute it faithfully anyway).
    assert_eq!(tracker_checksum(&[&[200]]), 6);
    // [194]: 31*1 + (31*1 + (-62)) = 31 + (-31) = 0 -> remapped to 1. This is the
    // only case that exercises the "0 becomes 1" reservation.
    assert_eq!(tracker_checksum(&[&[194]]), 1);
    // [1,2,3]: folds to low byte 128.
    assert_eq!(tracker_checksum(&[&[1, 2, 3]]), 128);
}

/// Drive `generate_and_apply_update` over a tracker seeded with exactly the given
/// signatures (all shown) and return the resulting checksum byte.
fn tracker_checksum(sigs: &[&[u8]]) -> u8 {
    let mut t = LastSeenTracker::vanilla();
    for s in sigs {
        t.add_pending(sig(s), true);
    }
    t.generate_and_apply_update().checksum
}

// ---------------------------------------------------------------------------
// Offset goldens (pure counting).
// ---------------------------------------------------------------------------

#[test]
fn offset_counts_additions_and_resets_on_generate() {
    let mut checked = 0;
    let mut t = LastSeenTracker::vanilla();
    for i in 0..5u8 {
        t.add_pending(sig(&[i]), true);
    }
    assert_eq!(t.offset(), 5);
    checked += 1;

    let update = t.generate_and_apply_update();
    assert_eq!(update.offset, 5);
    checked += 1;
    assert_eq!(t.offset(), 0, "generate must clear the offset");
    checked += 1;

    for i in 5..8u8 {
        t.add_pending(sig(&[i]), true);
    }
    assert_eq!(t.offset(), 3);
    checked += 1;

    assert!(checked >= 4);
}

// ---------------------------------------------------------------------------
// Bit-position golden: the anti-transpose heavy-hitter.
// A fresh 20-slot window with N shown messages has entries at ring indices
// 0..N and tail==N. The update walks index=(tail+i)%20, so the set bits are at
// i in [20-N, 20). For N=3 that is exactly {17,18,19} — a reversed or offset
// walk fails this instantly, though it would round-trip fine under its own scheme.
// ---------------------------------------------------------------------------

#[test]
fn acknowledged_bits_are_oldest_first_from_tail() {
    let mut checked = 0;
    let mut t = LastSeenTracker::vanilla();
    let a = sig(&[10]);
    let b = sig(&[11]);
    let c = sig(&[12]);
    t.add_pending(a.clone(), true);
    t.add_pending(b.clone(), true);
    t.add_pending(c.clone(), true);

    let update = t.generate_and_apply_update();

    let set_bits: Vec<usize> = update
        .acknowledged
        .iter()
        .enumerate()
        .filter_map(|(i, &s)| s.then_some(i))
        .collect();
    assert_eq!(
        set_bits,
        vec![17, 18, 19],
        "3 messages set exactly bits 17,18,19"
    );
    checked += 1;

    // Oldest-first order preserved.
    assert_eq!(update.last_seen, vec![a, b, c]);
    checked += 1;

    // Wire packing: LSB-first, ceil(20/8)=3 bytes. bits 17,18,19 -> byte 2,
    // positions 1,2,3 -> 0b0000_1110 = 0x0E. A big-endian/MSB-first packing
    // produces a different byte.
    assert_eq!(update.acknowledged_bytes(), vec![0x00, 0x00, 0x0E]);
    checked += 1;

    assert!(checked >= 3);
}

#[test]
fn unshown_message_leaves_a_gap_but_advances_the_window() {
    let mut checked = 0;
    let mut t = LastSeenTracker::vanilla();
    let a = sig(&[1]);
    let b = sig(&[2]);
    // shown, NOT shown, shown -> ring: [A, null, B], tail=3, offset=3.
    t.add_pending(a.clone(), true);
    t.add_pending(sig(&[99]), false);
    t.add_pending(b.clone(), true);
    assert_eq!(
        t.offset(),
        3,
        "an unshown message still advances the window"
    );
    checked += 1;

    let update = t.generate_and_apply_update();
    let set_bits: Vec<usize> = update
        .acknowledged
        .iter()
        .enumerate()
        .filter_map(|(i, &s)| s.then_some(i))
        .collect();
    // A at ring 0 -> bit 17; gap at ring 1 -> bit 18 clear; B at ring 2 -> bit 19.
    assert_eq!(
        set_bits,
        vec![17, 19],
        "the unshown slot is a gap, not acknowledged"
    );
    checked += 1;
    assert_eq!(
        update.last_seen,
        vec![a, b],
        "only shown messages are reported"
    );
    checked += 1;

    assert!(checked >= 3);
}

// ---------------------------------------------------------------------------
// Consecutive-duplicate guard.
// ---------------------------------------------------------------------------

#[test]
fn consecutive_duplicate_signature_is_ignored() {
    let mut t = LastSeenTracker::vanilla();
    let x = sig(&[7]);
    assert!(t.add_pending(x.clone(), true), "first add is recorded");
    assert!(
        !t.add_pending(x.clone(), true),
        "identical consecutive add is dropped"
    );
    assert_eq!(t.offset(), 1, "the duplicate does not advance the window");
    // A different signature in between re-arms the guard.
    assert!(t.add_pending(sig(&[8]), true));
    assert!(
        t.add_pending(x, true),
        "same signature after another is recorded again"
    );
    assert_eq!(t.offset(), 3);
}

// ---------------------------------------------------------------------------
// delete_chat -> ignore_pending, and the pending-flag gate.
// ---------------------------------------------------------------------------

#[test]
fn ignore_pending_retracts_only_unacknowledged_entries() {
    let mut checked = 0;
    let a = sig(&[1]);
    let b = sig(&[2]);

    // Retracting a pending entry removes it from the next update entirely.
    let mut t = LastSeenTracker::vanilla();
    t.add_pending(a.clone(), true);
    t.add_pending(b.clone(), true);
    t.ignore_pending(&a);
    let update = t.generate_and_apply_update();
    assert_eq!(update.last_seen, vec![b.clone()], "retracted entry is gone");
    checked += 1;

    // Once an update has reported an entry it is no longer pending, so a later
    // delete cannot retract it — it stays in the window.
    let mut t2 = LastSeenTracker::vanilla();
    t2.add_pending(a.clone(), true);
    let first = t2.generate_and_apply_update();
    assert_eq!(first.last_seen, vec![a.clone()]);
    checked += 1;
    t2.ignore_pending(&a); // no-op: a is acknowledged, not pending
    let second = t2.generate_and_apply_update();
    assert_eq!(
        second.last_seen,
        vec![a],
        "an acknowledged entry cannot be retracted by a later delete_chat"
    );
    checked += 1;

    assert!(checked >= 3);
}

// ---------------------------------------------------------------------------
// The disconnect-preventing flush schedule (vanilla markMessageAsProcessed):
// a standalone ack fires the moment the pending count exceeds ACK_THRESHOLD (64).
// ---------------------------------------------------------------------------

#[test]
fn mark_processed_flushes_once_past_the_threshold() {
    let mut t = LastSeenTracker::vanilla();
    // The first 64 processed messages accumulate without a flush.
    for i in 0..64u32 {
        let s = sig(&i.to_le_bytes());
        assert_eq!(
            t.mark_processed(s, true),
            None,
            "no flush at offset {}",
            i + 1
        );
    }
    assert_eq!(t.offset(), 64);
    // The 65th crosses offset > 64 and flushes exactly 65.
    let s65 = sig(&64u32.to_le_bytes());
    assert_eq!(t.mark_processed(s65, true), Some(65));
    assert_eq!(t.offset(), 0, "flushing clears the pending offset");
    // Immediately after a flush, the next message does not flush again.
    let s66 = sig(&65u32.to_le_bytes());
    assert_eq!(t.mark_processed(s66, true), None);
}

#[test]
fn take_acknowledgement_flushes_pending_offset_then_reports_nothing() {
    let mut t = LastSeenTracker::vanilla();
    assert_eq!(t.take_acknowledgement(), None, "nothing pending initially");
    t.add_pending(sig(&[1]), true);
    t.add_pending(sig(&[2]), true);
    assert_eq!(t.take_acknowledgement(), Some(2));
    assert_eq!(t.take_acknowledgement(), None, "offset cleared after flush");
}

// ---------------------------------------------------------------------------
// MessageSignatureCache: pack/unpack and vanilla's exact eviction/reorder.
// Expected orderings verified against an independent reference of vanilla's
// ArrayDeque removeLast / addFirst push algorithm.
// ---------------------------------------------------------------------------

#[test]
fn signature_cache_packs_most_recent_first() {
    let a = sig(&[1]);
    let b = sig(&[2]);
    let mut cache = MessageSignatureCache::with_capacity(3);
    // push(sig=A) -> [A, _, _]; push(sig=B) -> [B, A, _].
    cache.push(&[], Some(&a));
    cache.push(&[], Some(&b));
    assert_eq!(cache.pack(&b), Some(0));
    assert_eq!(cache.pack(&a), Some(1));
    assert_eq!(
        cache.pack(&sig(&[99])),
        None,
        "absent signature is NOT_FOUND"
    );
    assert_eq!(cache.unpack(0), Some(&b));
    assert_eq!(cache.unpack(1), Some(&a));
}

#[test]
fn signature_cache_reinsert_dedups_and_reorders() {
    let a = sig(&[1]);
    let b = sig(&[2]);
    let mut cache = MessageSignatureCache::with_capacity(3);
    cache.push(&[], Some(&a)); // [A, _, _]
    cache.push(&[], Some(&b)); // [B, A, _]
    cache.push(&[], Some(&a)); // reinsert A: [A, B, _] (A to front, B kept once)
    assert_eq!(cache.pack(&a), Some(0));
    assert_eq!(cache.pack(&b), Some(1));
    // Exactly two slots populated: no duplicate A.
    assert_eq!(cache.unpack(2), None);
}

#[test]
fn signature_cache_push_with_last_seen_list() {
    let a = sig(&[1]);
    let b = sig(&[2]);
    let c = sig(&[3]);
    let d = sig(&[4]);
    let mut cache = MessageSignatureCache::with_capacity(4);
    cache.push(&[], Some(&a));
    cache.push(&[], Some(&b));
    cache.push(&[], Some(&c)); // [C, B, A, _]
    // A message referencing last_seen [A, B], itself signed D.
    cache.push(&[a.clone(), b.clone()], Some(&d));
    // Independent reference: queue [A,B,D] over [C,B,A,_] -> [D, B, A, C].
    assert_eq!(cache.pack(&d), Some(0));
    assert_eq!(cache.pack(&b), Some(1));
    assert_eq!(cache.pack(&a), Some(2));
    assert_eq!(cache.pack(&c), Some(3));
}
