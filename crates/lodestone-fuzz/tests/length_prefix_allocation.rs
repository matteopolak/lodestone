//! Property: a length prefix claiming a huge element count must not force
//! an allocation disproportionate to the bytes actually available.
//!
//! ## History (issue #417, fixed)
//!
//! This file originally both stated the property and demonstrated that it
//! was **violated**: `lodestone-macros`' `decode_vec` called
//! `Vec::with_capacity(len)` on an attacker-chosen VarInt length *before*
//! checking `len` against the bytes actually remaining, for any `Vec<T>`
//! field (`T != u8`, or a `#[mc(varint)]` element) with no
//! `#[mc(max = ...)]` attribute. An 8-byte `GameLogin` payload drove a real
//! **48,000,000-byte** single allocation, measured with the counting
//! allocator below.
//!
//! Fixed in `decode_vec` (`crates/lodestone-macros/src/lib.rs`) by capping
//! the *pre-allocation* at `len.min(r.remaining())` — every element this
//! loop can possibly decode consumes at least one byte from the reader (no
//! `Decode` impl in this wire format reads zero bytes: every primitive,
//! VarInt and string read consumes >=1 byte — see `fixed_codec!` and
//! `Decode for String` in `lodestone-core/src/lib.rs`), so no more than
//! `r.remaining()` elements can ever be produced regardless of what `len`
//! claims. This is the same shape as `lodestone-core`'s own
//! `ensure_nbt_length_fits_remaining`, generalised with the safe universal
//! per-element minimum of 1 byte (a per-type minimum would allow a tighter
//! cap but risks a wrong minimum quietly re-opening the hole — see the
//! policy writeup in `docs/fuzz-harness.md` and the commit that closed
//! #417). `len` itself, the `#[mc(max = ...)]` check, and the `0..len` loop
//! bound are all unchanged: a payload that legitimately has more bytes
//! available still decodes every element; only the up-front reservation is
//! bounded by what's actually in the buffer.
//!
//! This test file now asserts the *fixed*, bounded behaviour instead of the
//! bug, plus a new test proving the cap tracks `remaining()` (not just
//! "always near zero"), plus a positive control against a real captured
//! fixture proving the fix doesn't reject legitimately large vectors.

// This file's global allocator was originally the only place in the
// workspace (per `grep -rn "allow(unsafe_code)" crates/`) that opts out of
// `unsafe_code = "deny"` (`Cargo.toml`'s `[workspace.lints.rust]`) — now one
// of two, alongside `container_set_content_unbounded_allocation.rs` (a
// second, independent instance of this exact defect shape, found by
// `fuzz/fuzz_targets/v770_clientbound_decode.rs`). Both are scoped as
// narrowly as that lint allows:
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
// This file drives `lodestone_v770` directly, so it exists only in a build that
// compiles that family in. On by default; see the crate manifest's `[features]`.
#![cfg(feature = "v770")]

use lodestone_core::{Ctx, Decode, Reader, Writer};
use lodestone_v770::packets::game::GameLogin;
use lodestone_v770::packets::registry::RegistryData;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAlloc;

thread_local! {
    /// Largest single allocation request observed **on the calling thread**
    /// since that thread's last reset.
    ///
    /// ## Why per-thread, and the two designs this replaces (issue #450)
    ///
    /// A `#[global_allocator]` is unavoidably process-wide: it sees every
    /// allocation on every thread. What is *not* forced is where it records
    /// them, and the first two attempts both recorded process-wide too:
    ///
    /// 1. A bare `static PEAK_SINGLE_ALLOC: AtomicUsize`. One test's
    ///    allocation showed up in another's measurement — caught when the
    ///    "small payload" test failed only when run alongside the "huge" one,
    ///    with the identical 48,000,000-byte peak leaking across.
    /// 2. The same atomic plus a `MEASUREMENT_LOCK: Mutex<()>` held across each
    ///    reset-call-read span, serialising the *measuring* tests. This is the
    ///    one that flaked, and the reason is instructive: a lock only excludes
    ///    code that takes it. `real_registry_data_fixture_still_decodes_cleanly_after_the_fix`
    ///    in this same file never calls `peak_alloc_during`, so it never takes
    ///    the lock, and its fixture read plus `RegistryData` decode allocate
    ///    freely into the shared atomic from a parallel harness thread. The
    ///    result passed alone and failed in a full parallel run — the classic
    ///    order-dependent green, on issue #417's own DoS regression gate.
    ///
    /// A thread-local needs no cooperation from anything: allocations made by
    /// other threads land in *their* cell and are structurally invisible here,
    /// whether or not those threads know this file exists. That property is
    /// asserted by `a_sibling_threads_allocation_does_not_contaminate_a_measurement`
    /// below, which fails against both designs above.
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
/// **Do not widen this.** It is deliberately *far* below the old
/// ~48,000,000-byte measurement rather than merely under it, so that a partial
/// regression is still caught. Every observed reason to want it wider so far has
/// been contamination of the measurement (issue #450) rather than a genuinely
/// larger correct allocation — fix the measurement, not the ceiling.
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
/// no allocation at all. So the correct hypothesis is a peak of exactly 0
/// bytes. The *rejected* hypothesis is the pre-fix behaviour this same test
/// used to assert: >=32 MiB (33,554,432 bytes), on the way to the exact
/// 48,000,000 bytes measured when the bug was filed — a factor of well over
/// 10^7 away from the correct answer of 0. Measured after the fix: exactly 0.
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

    // The decode must still fail cleanly — the fix does not change error
    // behaviour, only the allocation that happens before the error.
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
/// is still four orders of magnitude below the old ~48,000,000-byte
/// measurement, but it is *not* zero, which is the point: the bound scales
/// with the bytes actually supplied, not with a constant. Measured after the
/// fix: exactly 2,400 — the prediction was exact, not just "close".
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
    // close to the prediction and nowhere near the old ~48,000,000-byte
    // figure.
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

/// The gate on the measurement technique itself (issue #450's second half).
///
/// Every other test in this file measures a peak allocation and compares it to
/// a prediction. That is only meaningful if the number it reads back was
/// produced by the code under test and nothing else — which the original
/// process-global `static PEAK_SINGLE_ALLOC: AtomicUsize` could not guarantee,
/// and which no amount of reading the assertions would reveal. So this test
/// asserts the *isolation property* directly rather than trusting it.
///
/// It is deterministic, not probabilistic: the two barriers pin the sibling's
/// 48,000,000-byte allocation to the window strictly between this thread's reset
/// and its read. There is no interleaving in which the sibling allocates outside
/// the measurement window, so this cannot pass by luck of scheduling — which
/// matters, because scheduling luck is exactly what made the real defect look
/// like flake (passing under a filter, failing in a full run).
///
/// 48,000,000 is not an arbitrary large number: it is the exact single
/// allocation issue #417 measured, so a contaminated reading here is
/// indistinguishable from the very regression `huge_length_prefix_…` exists to
/// catch. That is the whole reason this matters — a DoS guard that reports its
/// sibling's allocations flakes, a flaky guard gets muted, and a muted guard
/// stops guarding.
///
/// Observed before the fix: `measured 48000000 bytes`. After: 0.
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

/// Positive control: the fix must not reject a legitimately large-but-valid
/// vector by, say, truncating real elements or erroring where the old code
/// wouldn't have. `registry_data_dimension_type.hex` is real bytes captured
/// from a live vanilla 26.2 server (not our own encoder — see
/// `docs/fuzz-harness.md`'s corpus notes) and decodes `RegistryData::entries:
/// Vec<PackedRegistryEntry>`, one of the fields issue #417 named as
/// vulnerable. If the cap were computed wrong (e.g. against the wrong
/// `remaining()` snapshot, or off by a field), this is the kind of real
/// packet that would start failing to decode or would leave trailing bytes.
#[test]
fn real_registry_data_fixture_still_decodes_cleanly_after_the_fix() {
    let path = lodestone_fuzz::v770_fixture_path("registry_data_dimension_type.hex");
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
