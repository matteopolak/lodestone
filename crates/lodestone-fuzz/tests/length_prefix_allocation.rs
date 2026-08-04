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

// This one file's global allocator is the only place in this crate — and,
// per `grep -rn "allow(unsafe_code)" crates/`, the only place in the
// workspace — that opts out of `unsafe_code = "deny"` (`Cargo.toml`'s
// `[workspace.lints.rust]`). It is scoped as narrowly as that lint allows:
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

use lodestone_core::{Ctx, Decode, Reader, Writer};
use lodestone_v770::packets::game::GameLogin;
use lodestone_v770::packets::registry::RegistryData;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAlloc;

/// Largest single allocation request observed since the last reset.
static PEAK_SINGLE_ALLOC: AtomicUsize = AtomicUsize::new(0);

/// `cargo test` runs `#[test]` functions in parallel threads *within one
/// process* by default, and the counting allocator below is necessarily
/// process-global — so without this, one test's allocation shows up in the
/// other's measurement. Caught exactly that way: both tests passed in
/// isolation (`--test length_prefix_allocation huge_length_prefix…`) and the
/// "small payload" one failed only when run alongside the "huge" one, with
/// the identical 48,000,000-byte peak leaking across. Every measurement in
/// this file holds this lock for the reset-call-read span of each measurement,
/// which serializes the tests in it without needing `--test-threads=1` for
/// the whole binary.
static MEASUREMENT_LOCK: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        PEAK_SINGLE_ALLOC.fetch_max(layout.size(), Ordering::SeqCst);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn peak_alloc_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let _guard = MEASUREMENT_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    PEAK_SINGLE_ALLOC.store(0, Ordering::SeqCst);
    let result = f();
    (result, PEAK_SINGLE_ALLOC.load(Ordering::SeqCst))
}

const CTX: Ctx = Ctx { version: 776 };

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
    // A little slack above the exact prediction: this asserts "no
    // disproportionate allocation happened", not "the allocator's internal
    // bookkeeping is byte-exact", so a small ceiling is used instead of an
    // exact equality — but it must stay far below the old ~48,000,000-byte
    // measurement, not just "smaller than that".
    const SMALL_CEILING: usize = 4096;
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
