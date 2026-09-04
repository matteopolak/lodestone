//! Property: a length prefix claiming a huge element count must not force
//! an allocation disproportionate to the bytes actually available.
//!
//! ## Reservation rule
//!
//! `lodestone-macros`' `decode_vec` caps a `Vec<T>`'s *pre-allocation* at
//! `len.min(r.remaining())`. Every element this
//! loop can possibly decode consumes at least one byte from the reader (no
//! `Decode` impl in this wire format reads zero bytes: every primitive,
//! VarInt and string read consumes >=1 byte — see `fixed_codec!` and
//! `Decode for String` in `lodestone-core/src/lib.rs`), so no more than
//! `r.remaining()` elements can ever be produced regardless of what `len`
//! claims. This is the same shape as `lodestone-core`'s own
//! `ensure_nbt_length_fits_remaining`, generalised with the safe universal
//! per-element minimum of 1 byte (a per-type minimum would allow a tighter
//! cap but risks a wrong minimum quietly re-opening the hole — see the
//! policy writeup in `docs/fuzz-harness.md`). `len` itself, the
//! `#[mc(max = ...)]` check, and the `0..len` loop
//! bound are all unchanged: a payload that legitimately has more bytes
//! available still decodes every element; only the up-front reservation is
//! bounded by what's actually in the buffer.
//!
//! The tests assert the bounded reservation, prove that it tracks
//! `remaining()` rather than collapsing to zero, and decode a captured fixture
//! whose vector has multiple real elements.

// This test binary opts out of the workspace's `unsafe_code = "deny"` lint
// solely to measure the allocator. The scope is as narrow as the lint allows:
// `#![allow]` is a crate-root attribute, and cargo compiles every
// `tests/*.rs` file as its own separate binary/crate, so this cannot leak
// into `src/lib.rs`, any other test binary, or any other crate. The
// alternative to measuring the real allocator is a purely static claim
// ("the source at `lodestone-macros`' `decode_vec` calls
// `Vec::with_capacity(len)` before checking `len` against remaining bytes")
// — true, and cited below, but CLAUDE.md's whole record is built from static
// claims that turned out to be wrong, so this test pays the one-file unsafe
// cost to make the claim a real measurement instead. `alloc`/`dealloc` are
// pure pass-throughs to `System` plus an atomic counter — no allocation
// logic of their own to get wrong.
#![allow(unsafe_code)]
// This file drives `lodestone_v26_2` directly, so it exists only in a build that
// compiles that family in. On by default; see the crate manifest's `[features]`.
#![cfg(feature = "v26-2")]

use lodestone_core::{Ctx, Decode, Reader, Writer};
use lodestone_v26_2::packets::game::GameLogin;
use lodestone_v26_2::packets::registry::RegistryData;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAlloc;

thread_local! {
    /// Largest single allocation request observed **on the calling thread**
    /// since that thread's last reset.
    ///
    /// ## Why measurements are per-thread
    ///
    /// `#[global_allocator]` receives allocations from every thread, but its
    /// accounting storage need not be process-wide. A shared atomic cannot
    /// distinguish the code under measurement from unrelated allocations. A
    /// mutex around selected callers cannot repair that ambiguity because an
    /// uncoordinated fixture read or decode can allocate while the measurement
    /// window is open.
    ///
    /// Thread-local storage needs no cooperation from other code: allocations
    /// made by another thread land in its own cell and are structurally
    /// invisible here. `a_sibling_threads_allocation_does_not_contaminate_a_measurement`
    /// verifies that isolation with barriers that place the sibling allocation
    /// inside this thread's measurement window.
    ///
    /// `const`-initialised on a `Cell<usize>` deliberately: that form compiles
    /// to a plain per-thread slot with no lazy initialisation and no
    /// destructor, so reading it from inside `alloc` cannot itself allocate and
    /// recurse.
    static PEAK_SINGLE_ALLOC: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with`, not `with`: an allocation during thread teardown (after
        // TLS destruction) must be recorded nowhere rather than panic inside
        // the global allocator. No measurement can be in flight at that point
        // anyway, so a dropped sample cannot mask a regression.
        let _ = PEAK_SINGLE_ALLOC.try_with(|peak| peak.set(peak.get().max(layout.size())));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Peak single allocation performed by `f` **on this thread**.
///
/// `f` must not move its allocation onto another thread; nothing here does, and
/// a decode of a byte slice structurally cannot.
fn peak_alloc_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    PEAK_SINGLE_ALLOC.with(|peak| peak.set(0));
    let result = f();
    (result, PEAK_SINGLE_ALLOC.with(Cell::get))
}

const CTX: Ctx = Ctx { version: 776 };

/// A little slack above an exact prediction of 0: the tests using this assert
/// "no disproportionate allocation happened", not "the allocator's internal
/// bookkeeping is byte-exact", so a small ceiling is used instead of an exact
/// equality.
///
/// **Do not widen this.** The ceiling admits allocator bookkeeping while
/// remaining far below an attacker-chosen multi-million-element reservation.
/// A failure means the reservation or the measurement boundary needs scrutiny;
/// widening the ceiling would weaken the allocation bound.
const SMALL_CEILING: usize = 4096;

/// `entity_id: i32 = 0`, `hardcore: bool = false`, then a `levels: Vec<String>`
/// length prefix of `claimed_len`, followed by `trailing_bytes` more bytes
/// (garbage, or nothing) — isolating the up-front `Vec::with_capacity` cost
/// from any per-element work.
fn game_login_with_huge_levels_prefix(claimed_len: i32, trailing_bytes: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(0);
    w.bool(false);
    w.var_i32(claimed_len);
    let mut bytes = w.into_vec();
    bytes.extend_from_slice(trailing_bytes);
    bytes
}

/// Predicted-before-measured: with zero bytes left after the length prefix,
/// `len.min(r.remaining())` caps the reservation at 0 elements, and
/// `Vec::with_capacity(0)` is guaranteed by the standard library to perform
/// no allocation at all. The predicted peak is exactly 0 bytes. The rejected
/// hypothesis is an attacker-chosen reservation of at least 32 MiB
/// (33,554,432 bytes), more than 10^7 times the predicted value.
#[test]
fn huge_length_prefix_no_longer_forces_disproportionate_allocation() {
    const CLAIMED_LEN: i32 = 2_000_000;
    let payload = game_login_with_huge_levels_prefix(CLAIMED_LEN, &[]);
    assert!(
        payload.len() < 16,
        "sanity: the malicious payload itself must be tiny ({} bytes)",
        payload.len()
    );

    let (decode_result, peak) = peak_alloc_during(|| GameLogin::decode(&mut Reader::new(&payload), CTX));

    // The decode must still fail cleanly: the reservation bound does not alter
    // the error produced for an incomplete element stream.
    assert!(
        decode_result.is_err(),
        "expected UnexpectedEof after the oversized levels prefix, got {decode_result:?}"
    );

    const PREDICTED_PEAK: usize = 0;
    assert!(
        peak <= SMALL_CEILING,
        "predicted a peak allocation of {PREDICTED_PEAK} bytes (capacity capped at \
         len.min(remaining) == len.min(0) == 0, and Vec::with_capacity(0) never allocates); \
         measured {peak} bytes instead, which is not within the {SMALL_CEILING}-byte slack of \
         that prediction. The old, now-rejected hypothesis was >= 32 MiB — if this assertion is \
         failing because `peak` is back in that range, `decode_vec`'s bound has regressed."
    );
}

/// The cap must track `remaining()`, not collapse to "always ~0 regardless
/// of input" — otherwise this whole test file would trivially pass for the
/// wrong reason (a decoder that always reserved 0 bytes regardless of input
/// would also pass the test above). This payload leaves exactly 100 bytes
/// after the claimed length of 2,000,000, so the predicted cap is
/// `len.min(remaining) == 2_000_000.min(100) == 100` elements, and the
/// predicted peak allocation is `100 * size_of::<String>()` — 2,400 bytes on
/// a 64-bit target where `String` is a 24-byte (ptr, len, cap) triple. That
/// is *not* zero, which is the point: the bound scales with the bytes actually
/// supplied, not with a constant. The prediction is 2,400 bytes on a 64-bit
/// target, rather than merely an upper bound near that value.
#[test]
fn claimed_length_beyond_remaining_bytes_caps_allocation_to_remaining_not_zero() {
    const CLAIMED_LEN: i32 = 2_000_000;
    const TRAILING: usize = 100;
    let payload = game_login_with_huge_levels_prefix(CLAIMED_LEN, &vec![0xAB; TRAILING]);

    let (decode_result, peak) = peak_alloc_during(|| GameLogin::decode(&mut Reader::new(&payload), CTX));

    // 100 garbage bytes are not a valid `Vec<String>` element stream, so
    // this must still error rather than silently succeeding.
    assert!(
        decode_result.is_err(),
        "expected a decode error from 100 garbage trailing bytes, got {decode_result:?}"
    );

    let predicted_peak = TRAILING * std::mem::size_of::<String>();
    // Same generous-but-bounded slack rationale as the test above: not an
    // exact equality (allocator/std-internals could round), but must land
    // close to the prediction and nowhere near an attacker-chosen reservation.
    let ceiling = predicted_peak * 2;
    assert!(
        peak <= ceiling,
        "predicted peak allocation ~{predicted_peak} bytes (100 remaining bytes . \
         size_of::<String>()); measured {peak} bytes, over the {ceiling}-byte ceiling. The \
         allocation is supposed to scale with `r.remaining()`, not with the claimed length of \
         {CLAIMED_LEN}."
    );
    assert!(
        peak >= TRAILING,
        "measured only {peak} bytes for a cap of {TRAILING} elements — suspiciously small, as \
         though the cap collapsed to 0 regardless of how many bytes were actually available \
         (which would make this test pass for the wrong reason)."
    );
}

/// The measurement technique has its own isolation gate.
///
/// Every other test in this file measures a peak allocation and compares it to
/// a prediction. That is only meaningful if the number it reads back was
/// produced by the code under test and nothing else. A process-global
/// `static PEAK_SINGLE_ALLOC: AtomicUsize` cannot provide that guarantee, so
/// this test asserts the *isolation property* directly.
///
/// It is deterministic, not probabilistic: the two barriers pin the sibling's
/// 48,000,000-byte allocation to the window strictly between this thread's reset
/// and its read. There is no interleaving in which the sibling allocates outside
/// the measurement window, so no scheduling order can place the sibling
/// allocation outside the asserted interval.
///
/// 48,000,000 is not an arbitrary large number: it is a deliberately distinct
/// sibling allocation that is large enough to cross every ceiling in this file.
/// A contaminated reading is therefore indistinguishable from the oversized
/// reservation the regression tests exist to catch. A guard that reports a
/// sibling's allocations is not a usable guard for a reservation bound.
#[test]
fn a_sibling_threads_allocation_does_not_contaminate_a_measurement() {
    use std::sync::{Arc, Barrier};

    const SIBLING_ALLOC: usize = 48_000_000;

    // Both barriers are constructed and both threads spawned *before* the
    // measurement window opens, so the thread-spawn bookkeeping itself cannot
    // land inside the reading.
    let opened = Arc::new(Barrier::new(2));
    let allocated = Arc::new(Barrier::new(2));
    let sibling = {
        let opened = Arc::clone(&opened);
        let allocated = Arc::clone(&allocated);
        std::thread::spawn(move || {
            opened.wait();
            // `with_capacity` rather than a growing push loop: one allocator
            // call of exactly the size we are asserting about.
            let hog: Vec<u8> = Vec::with_capacity(SIBLING_ALLOC);
            std::hint::black_box(&hog);
            allocated.wait();
            drop(hog);
        })
    };

    let ((), peak) = peak_alloc_during(|| {
        opened.wait();
        allocated.wait();
    });

    sibling.join().expect("sibling thread panicked");

    assert!(
        peak <= SMALL_CEILING,
        "a sibling thread's {SIBLING_ALLOC}-byte allocation showed up in this thread's \
         measurement as {peak} bytes (ceiling {SMALL_CEILING}). The allocation counter is not \
         scoped to the measuring thread, so every prediction-vs-measurement assertion in this \
         file is reading a number other tests contributed to. Do NOT relax the ceilings to make \
         them pass — scope the counter."
    );
}

/// Control for the control above: a well-formed small payload (`levels`
/// length 0) must **not** trigger a large allocation, so the measurement
/// technique itself isn't just reporting a big number unconditionally.
#[test]
fn well_formed_small_payload_does_not_trigger_a_large_allocation() {
    let payload = game_login_with_huge_levels_prefix(0, &[]);
    let (decode_result, peak) = peak_alloc_during(|| GameLogin::decode(&mut Reader::new(&payload), CTX));

    // `levels` decodes fine (0 elements); the packet still errors overall
    // because nothing after `levels` is present, but that error must come
    // from later fields, not from the allocation this test is about.
    let _ = decode_result;

    const SUSPICIOUSLY_LARGE: usize = 1024 * 1024;
    assert!(
        peak < SUSPICIOUSLY_LARGE,
        "a `levels` length of 0 caused a {peak}-byte allocation — the measurement \
         technique is not isolating the bug, it is just reporting noise"
    );
}

/// Positive control: a legitimately large, valid vector must retain every
/// element and decode without trailing bytes. `registry_data_dimension_type.hex`
/// contains captured 26.2 server bytes (not our own encoder — see
/// `docs/fuzz-harness.md`'s corpus notes) and decodes `RegistryData::entries:
/// Vec<PackedRegistryEntry>`, a vector field whose length prefix exercises the
/// reservation bound. If the cap were computed wrong (e.g. against the wrong
/// `remaining()` snapshot, or off by a field), this is the kind of real
/// packet that would start failing to decode or would leave trailing bytes.
#[test]
fn real_registry_data_fixture_still_decodes_cleanly_after_the_fix() {
    let path = lodestone_fuzz::v26_2_fixture_path("registry_data_dimension_type.hex");
    let bytes = lodestone_fuzz::read_hex_fixture(&path);
    let mut reader = Reader::new(&bytes);

    let data = RegistryData::decode(&mut reader, CTX)
        .unwrap_or_else(|err| panic!("real registry_data fixture must still decode: {err}"));
    reader
        .ensure_empty()
        .unwrap_or_else(|err| panic!("real registry_data fixture left trailing bytes: {err}"));

    assert_eq!(data.registry, "minecraft:dimension_type");
    assert!(
        !data.entries.is_empty(),
        "the real fixture is known to carry multiple dimension-type entries; an empty \
         result would mean the fixed decode path is silently dropping real elements"
    );
}
