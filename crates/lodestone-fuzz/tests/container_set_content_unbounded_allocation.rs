//! Bug found by `fuzz/fuzz_targets/v770_clientbound_decode.rs` (this
//! session's first libFuzzer target added to this repo): a **9-byte**
//! `CONTAINER_SET_CONTENT` (play clientbound packet id 18) payload from a
//! hostile or merely broken server crashes any connecting lodestone client
//! with an out-of-memory abort, before a single item is decoded.
//!
//! ## Where the bug lives
//!
//! `crates/protocol/v770/src/adapter/inventory.rs`'s hand-rolled
//! `CONTAINER_SET_CONTENT` decode arm:
//!
//! ```text
//! let len = reader.var_i32().map_err(dec_err)?;
//! let len = usize::try_from(len)...?;
//! let mut items = Vec::with_capacity(len);   // <-- unchecked against remaining bytes
//! ```
//!
//! This is the **same defect class** `docs/fuzz-harness.md` already documents
//! as fixed once, in `lodestone-macros`' `decode_vec` — an attacker-controlled
//! VarInt length feeding `Vec::with_capacity` before any check against
//! `reader.remaining()` — but this specific decode arm is hand-written, not
//! generated through `decode_vec`, so that fix never touched it. It is a
//! second, independent instance of the same hole, in a different file, found
//! by treating "no panic on arbitrary bytes" as a *reachability* problem
//! (coverage-guided fuzzing) rather than by re-reading every hand-rolled
//! decoder for the same shape by hand.
//!
//! `crates/protocol/v770/**` is out of scope for this session (owned by a
//! live agent) — this file only reports and measures the bug through the
//! real production entry point (`V770Adapter::handle_packet`, via
//! `lodestone_fuzz::decode_clientbound`), it does not patch
//! `adapter/inventory.rs`. The fix, when applied, is the same shape
//! `decode_vec`'s fix used: cap the reservation at
//! `len.min(reader.remaining())` — every `ItemStack` this loop can decode
//! consumes at least one byte (a present/absent flag at minimum), so no more
//! than `remaining()` elements can ever be produced regardless of what `len`
//! claims.
//!
//! ## This test's own status: expected to FAIL once the bug above is fixed
//!
//! Unlike `length_prefix_allocation.rs` (which asserts the *fixed* behaviour
//! for `decode_vec`, because that fix already landed), this file asserts the
//! *current, buggy* behaviour — it is a live, executable bug report, exactly
//! the shape `length_prefix_allocation.rs`'s own module doc says it used to
//! be before its fix landed ("This file originally both stated the property
//! and demonstrated that it was **violated**"). When `adapter/inventory.rs`
//! is fixed,
//! `container_set_content_allocation_is_disproportionate_to_the_tiny_payload`
//! below will start failing — that is the signal to flip its assertion to
//! the bounded form (see that file's `huge_length_prefix_no_longer_forces_disproportionate_allocation`
//! for the shape to copy) rather than a spurious CI failure to chase down.
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
/// prefix of `claimed_len` and nothing else — isolating the up-front
/// `Vec::with_capacity` cost the same way
/// `game_login_with_huge_levels_prefix` does in `length_prefix_allocation.rs`.
fn container_set_content_with_huge_items_prefix(claimed_len: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(0); // window_id
    w.var_i32(0); // state_id
    w.var_i32(claimed_len); // items length prefix
    w.into_vec()
}

/// **Currently passing** — documents the live bug. `Vec::with_capacity(len)`
/// in `adapter/inventory.rs`'s `CONTAINER_SET_CONTENT` arm reserves for the
/// full, unchecked `claimed_len` regardless of `payload.len()`, so a 5-byte
/// payload (three single-byte VarInts) forces a multi-megabyte allocation.
/// See this file's module doc for the exact production fix once
/// `crates/protocol/v770` is back in scope.
#[test]
fn container_set_content_allocation_is_disproportionate_to_the_tiny_payload() {
    const CLAIMED_LEN: i32 = 2_000_000;
    let payload = container_set_content_with_huge_items_prefix(CLAIMED_LEN);
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

    // It still errors cleanly (or would, once item decode runs out of bytes)
    // — the bug is entirely in the up-front reservation, not in error
    // handling. Not asserted strictly here since the point of this file is
    // the allocation, not the `Result` shape.
    let _ = result;

    assert!(
        peak > SMALL_CEILING,
        "expected the CURRENTLY-BUGGY behaviour (a disproportionate up-front \
         allocation well above {SMALL_CEILING} bytes from a {}-byte payload); \
         measured only {peak} bytes. If this assertion is failing, \
         `adapter/inventory.rs`'s CONTAINER_SET_CONTENT decode has been fixed \
         — flip this test to assert the bounded behaviour instead, matching \
         `length_prefix_allocation.rs`'s post-fix shape.",
        payload.len()
    );
    assert!(
        peak >= 2_000_000,
        "expected the reservation to scale with the claimed length ({CLAIMED_LEN} \
         items), not just be 'some large constant' — measured {peak} bytes, which \
         is not even proportional to a 2,000,000-element Vec::with_capacity. This \
         would indicate a different, unexplained source of allocation rather than \
         the one this file documents."
    );
}
