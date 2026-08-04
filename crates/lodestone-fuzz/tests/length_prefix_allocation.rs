//! Property: a length prefix claiming a huge element count must not force
//! an allocation disproportionate to the bytes actually available.
//!
//! This file both states the property and demonstrates that it is
//! **currently violated** — see the bug report at the bottom of this
//! comment and `docs/fuzz-harness.md`. It does not patch the bug: the fix
//! lives in `lodestone-macros`' shared `decode_vec` (used by all four
//! protocol families) and in the individual packet fields that omit
//! `#[mc(max = ...)]`, none of which are this crate's files to edit per this
//! task's ownership rules, and a shared macro fix needs a policy decision
//! (what default cap, and whether it becomes compile-time-enforced) that is
//! not this harness's call to make.
//!
//! ## Why this test measures rather than infers
//!
//! Reading `lodestone-macros`' `decode_vec` shows `Vec::with_capacity(len)`
//! called before any check that `len` fits the remaining bytes, for any
//! `Vec<T>` field (`T != u8`, or a `#[mc(varint)]` element) with no
//! `#[mc(max = ...)]` attribute. That is a static reading, not a
//! measurement — and CLAUDE.md's own record is full of static readings that
//! were wrong. So this test installs a counting `#[global_allocator]`
//! wrapper scoped to *this test binary only* (cargo builds every
//! `tests/*.rs` file as its own binary, so this cannot affect any other
//! test or the shared allocator features `lodestone-allocbench` guards
//! against) and directly measures the single largest allocation requested
//! while decoding a **6-byte** payload. If the bug is real, that allocation
//! will be tens of megabytes; if it is not (e.g. someone fixes it after this
//! file is written), the assertion below will fail loudly, which is exactly
//! what should happen — this test is written to go red the day the bug is
//! fixed, not to quietly stop meaning anything.
//!
//! ## The bug, for the GitHub issue
//!
//! `GameLogin` (`crates/protocol/v770/src/packets/game.rs`) decodes
//! `entity_id: i32`, `hardcore: bool`, then `levels: Vec<String>` with no
//! `#[mc(max = ...)]`. A payload of `[0,0,0,0, 0x00, <var_i32(2_000_000)>]` —
//! 4 + 1 + 3 = 8 bytes, no bytes after the length prefix — makes
//! `decode_vec`'s generated code run `Vec::<String>::with_capacity(2_000_000)`
//! (48 MB, `size_of::<String>() == 24`) before reading a single string,
//! immediately followed by `Err(UnexpectedEof)` on the first string read.
//! This crate deliberately stops at 2,000,000 (48 MB) rather than pushing
//! toward `i32::MAX` (~48 GB for this element size) — that would demonstrate
//! a full process-abort DoS, but doing so on a machine shared with other
//! agents' builds (per `CLAUDE.md`'s Docker/memory notes) is not a
//! responsible way to prove a point already provable at a safe scale. The
//! same shape affects `v47`, `v340`, and `v735` (`entity_ids: Vec<i32>` with
//! `#[mc(varint)]` and no `max`) and several more `v770` fields
//! (`packets::configuration::KnownPacks::packs`,
//! `packets::player_info::{PlayerInfoUpdate::entries, PlayerInfoRemove::uuids}`,
//! `packets::game::{GameRuleEntries}::entries` (x2),
//! `packets::registry::RegistryData::entries`,
//! `packets::scoreboard::*::players`, `packets::time::WorldClocks::clocks`,
//! `packets::login::GameProfile::properties`) — filed as issue-#282-adjacent
//! per this task's brief, not fixed here.

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
/// this file holds this lock for the reset-call-read span, which serializes
/// the two tests without needing `--test-threads=1` for the whole binary.
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
/// length prefix of `claimed_len` and nothing else — chosen so the loop's
/// first `r.string(..)` call hits end-of-input immediately, isolating the
/// up-front `Vec::with_capacity` cost from any per-element work.
fn game_login_with_huge_levels_prefix(claimed_len: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(0);
    w.bool(false);
    w.var_i32(claimed_len);
    w.into_vec()
}

#[test]
fn huge_length_prefix_forces_disproportionate_allocation() {
    const CLAIMED_LEN: i32 = 2_000_000;
    let payload = game_login_with_huge_levels_prefix(CLAIMED_LEN);
    assert!(
        payload.len() < 16,
        "sanity: the malicious payload itself must be tiny ({} bytes)",
        payload.len()
    );

    let (decode_result, peak) = peak_alloc_during(|| GameLogin::decode(&mut Reader::new(&payload), CTX));

    // The decode must still fail cleanly — this bug does not corrupt memory
    // or hang, it just allocates memory disconnected from the input size.
    assert!(
        decode_result.is_err(),
        "expected UnexpectedEof after the oversized levels prefix, got {decode_result:?}"
    );

    // 2,000,000 * size_of::<String>() (24 bytes on a 64-bit target) = 48 MB.
    // A fixed, generous floor rather than an exact equality: this must not
    // be sensitive to String's exact layout, only to "way more than an
    // 8-byte payload should ever cause".
    const DISPROPORTIONATE_FLOOR: usize = 32 * 1024 * 1024;
    assert!(
        peak >= DISPROPORTIONATE_FLOOR,
        "expected a single allocation of at least {DISPROPORTIONATE_FLOOR} bytes while decoding \
         an {}-byte payload (proves `Vec::with_capacity({CLAIMED_LEN})` runs before any bound \
         check against the input's actual remaining bytes) — measured only {peak} bytes. Either \
         `lodestone-macros`' `decode_vec` has been fixed (update/delete this test and the matching \
         GitHub issue) or this measurement technique stopped working.",
        payload.len(),
    );
}

/// Control for the control above: a well-formed small payload (`levels`
/// length 0) must **not** trigger a large allocation, so the measurement
/// technique itself isn't just reporting a big number unconditionally.
#[test]
fn well_formed_small_payload_does_not_trigger_a_large_allocation() {
    let payload = game_login_with_huge_levels_prefix(0);
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
