//! **Fixed**, at `b3597cef` ("bound two attacker-controlled list
//! reservations") — this file's own doc used to describe the bug as live and
//! assert the *buggy* behaviour; a later run of `cargo test -p lodestone-fuzz
//! --no-fail-fast` found this file failing exactly as its own prior module
//! doc predicted it would once fixed ("If this assertion is failing,
//! `adapter/inventory.rs`'s CONTAINER_SET_CONTENT decode has been fixed —
//! flip this test"), which is the signal this rewrite responds to. The two
//! tests below now assert the fixed, bounded behaviour instead, matching
//! `length_prefix_allocation.rs`'s own post-fix shape (its module doc
//! documents the identical before/after pattern for a different packet).
//!
//! ## Original bug, for context
//!
//! Bug found by `fuzz/fuzz_targets/v770_clientbound_decode.rs` (this
//! repo's first libFuzzer target): a **9-byte** `CONTAINER_SET_CONTENT`
//! (play clientbound packet id 18) payload from a hostile or merely broken
//! server crashed any connecting lodestone client with an out-of-memory
//! abort, before a single item was decoded.
//!
//! `crates/protocol/v770/src/adapter/inventory.rs`'s hand-rolled
//! `CONTAINER_SET_CONTENT` decode arm used to read:
//!
//! ```text
//! let len = reader.var_i32().map_err(dec_err)?;
//! let len = usize::try_from(len)...?;
//! let mut items = Vec::with_capacity(len);   // <-- unchecked against remaining bytes
//! ```
//!
//! This was the **same defect class** `docs/fuzz-harness.md` already
//! documents as fixed once, in `lodestone-macros`' `decode_vec` — an
//! attacker-controlled VarInt length feeding `Vec::with_capacity` before any
//! check against `reader.remaining()` — but this specific decode arm is
//! hand-written, not generated through `decode_vec`, so that fix never
//! touched it. A second, independent instance of the same hole, in a
//! different file, found by treating "no panic on arbitrary bytes" as a
//! *reachability* problem (coverage-guided fuzzing) rather than by
//! re-reading every hand-rolled decoder for the same shape by hand.
//!
//! ## The fix
//!
//! `adapter/inventory.rs` now reads
//! `Vec::with_capacity(len.min(reader.remaining()))` — capping the
//! reservation at the readable bytes, since every `ItemStack` this loop can
//! decode consumes at least one byte, so no more than `remaining()` of them
//! can ever be produced regardless of what `len` claims. This file only
//! measures the fix through the real production entry point
//! (`V770Adapter::handle_packet`, via `lodestone_fuzz::decode_clientbound`).
//!
//! ## Why the claimed length is 2,000,000, not the fuzzer's own ~136,000,000
//!
//! The fuzzer's actual crashing input claimed a length of roughly 136 million
//! items, which really did trigger libFuzzer's out-of-memory killer in under
//! a second. Reproducing that exact magnitude in a `cargo test` that runs on
//! a machine shared with other agents' builds is itself the hazard
//! `CLAUDE.md`'s memory section warns about ("unbounded test memory
//! force-rebooted the machine") — so, per the same precedent
//! `length_prefix_allocation.rs` already set for its own bug (chose
//! 2,000,000 over `i32::MAX` for the identical reason), this file proves the
//! same defect at a bounded, safe magnitude instead. 2,000,000 is enough:
//! `ItemStack` is well over a handful of bytes, so
//! `Vec::with_capacity(2_000_000)` of it is already tens of megabytes — four
//! to five orders of magnitude past `SMALL_CEILING` below, which is all the
//! measurement needs to say.

// Second file in this crate with this allow (the first is
// `length_prefix_allocation.rs`, for its own already-fixed instance of the
// identical defect shape) — see that file's own comment for the safety
// argument, unchanged here: `alloc`/`dealloc` are pure pass-throughs to
// `System` plus a thread-local counter, no allocation logic of their own to
// get wrong, and each `tests/*.rs` file compiles as its own separate
// binary/process, so two files each declaring `#[global_allocator]` cannot
// conflict with each other or with `src/lib.rs`.
#![allow(unsafe_code)]
#![cfg(feature = "v770")]

use lodestone_core::Writer;
use lodestone_fuzz::{Family, decode_clientbound};
use lodestone_model::ConnectionState;
use lodestone_v770::packet_ids::play;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAlloc;

thread_local! {
    /// See `length_prefix_allocation.rs`'s identically-named cell for the
    /// full per-thread-contamination reasoning — copied here rather than
    /// shared because each `tests/*.rs` file is its own crate.
    static PEAK_SINGLE_ALLOC: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = PEAK_SINGLE_ALLOC.try_with(|peak| peak.set(peak.get().max(layout.size())));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn peak_alloc_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    PEAK_SINGLE_ALLOC.with(|peak| peak.set(0));
    let result = f();
    (result, PEAK_SINGLE_ALLOC.with(Cell::get))
}

/// Same slack rationale as `length_prefix_allocation.rs`'s constant of the
/// same name: far below the smallest plausible "legitimately large but
/// real" allocation this packet could ever need, so a partial regression
/// still trips it.
const SMALL_CEILING: usize = 4096;

/// `window_id: VarInt = 0`, `state_id: VarInt = 0`, then an `items` length
/// prefix of `claimed_len`, followed by `trailing_bytes` more zero bytes —
/// isolating the up-front `Vec::with_capacity` cost the same way
/// `game_login_with_huge_levels_prefix` does in `length_prefix_allocation.rs`,
/// while letting a caller control `reader.remaining()` after the length
/// prefix (0 for the "tiny payload" case below, non-zero for the "scales
/// with input" case).
fn container_set_content_with_huge_items_prefix(claimed_len: i32, trailing_bytes: usize) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(0); // window_id
    w.var_i32(0); // state_id
    w.var_i32(claimed_len); // items length prefix
    let mut out = w.into_vec();
    out.extend(std::iter::repeat_n(0u8, trailing_bytes));
    out
}

/// **Fixed.** A malicious server claiming 2,000,000 items in a payload that
/// leaves zero readable bytes after the length prefix must not reserve for
/// anything close to that — `adapter/inventory.rs` now caps the reservation
/// at `len.min(reader.remaining())`, and `remaining() == 0` here.
#[test]
fn container_set_content_allocation_is_bounded_by_available_bytes() {
    const CLAIMED_LEN: i32 = 2_000_000;
    let payload = container_set_content_with_huge_items_prefix(CLAIMED_LEN, 0);
    assert!(
        payload.len() < 16,
        "sanity: the malicious payload itself must be tiny ({} bytes)",
        payload.len()
    );

    let (result, peak) = peak_alloc_during(|| {
        decode_clientbound(
            Family::V770,
            ConnectionState::Play,
            play::clientbound::CONTAINER_SET_CONTENT,
            &payload,
        )
    });

    // Still errors cleanly (there is nothing left to decode a carried item
    // from either) — the fix only changes the up-front reservation, not
    // error handling.
    assert!(
        result.is_err(),
        "expected a clean decode error for a payload with no item bytes, got {result:?}"
    );

    assert!(
        peak <= SMALL_CEILING,
        "expected the FIXED, bounded behaviour (peak allocation at or below \
         {SMALL_CEILING} bytes from a {}-byte payload claiming {CLAIMED_LEN} items); \
         measured {peak} bytes instead. If this assertion is failing because `peak` \
         is back in the multi-megabyte range, `adapter/inventory.rs`'s \
         `len.min(reader.remaining())` cap has regressed.",
        payload.len()
    );
}

/// The cap must track `remaining()`, not collapse to "always ~0 regardless
/// of input" — otherwise the test above would trivially pass for the wrong
/// reason (a decoder that always reserved 0 bytes would also pass it). This
/// payload leaves 200 real bytes after the claimed length of 2,000,000, so
/// the reservation is capped at `2_000_000.min(200) == 200` elements — still
/// bounded, but non-zero and tracking the actual input rather than a
/// constant. Not predicting an exact byte count here (the loop that reads
/// `ItemStack`s does not stop at exactly 200 reservations' worth of memory,
/// since the `Vec` only grows if the loop keeps pushing), just that it stays
/// far below the old, truly unbounded magnitude a 2,000,000-element
/// reservation would cost.
#[test]
fn container_set_content_allocation_scales_with_remaining_bytes_not_a_constant() {
    const CLAIMED_LEN: i32 = 2_000_000;
    const TRAILING: usize = 200;
    let payload = container_set_content_with_huge_items_prefix(CLAIMED_LEN, TRAILING);

    let (_, peak) = peak_alloc_during(|| {
        decode_clientbound(
            Family::V770,
            ConnectionState::Play,
            play::clientbound::CONTAINER_SET_CONTENT,
            &payload,
        )
    });

    // Four to five orders of magnitude below what an unchecked
    // Vec::with_capacity(2_000_000) of ItemStack would cost (tens of
    // megabytes), while still allowing real per-item decode work over 200
    // bytes of trailing input.
    const UNBOUNDED_MAGNITUDE_FLOOR: usize = 1_000_000;
    assert!(
        peak < UNBOUNDED_MAGNITUDE_FLOOR,
        "expected an allocation bounded by ~{TRAILING} readable bytes, not the \
         unbounded {CLAIMED_LEN}-item claim; measured {peak} bytes, which is at or \
         above the {UNBOUNDED_MAGNITUDE_FLOOR}-byte floor this test uses to detect \
         a regression back to the unbounded reservation."
    );
}
