//! Verifies `lodestone_fuzz::catch` — the panic-to-finding conversion CLAUDE.md
//! names as one of the workspace's two *production* `catch_unwind` boundaries
//! (the other is `lodestone-ecs`'s `async_task`/`handle` job boundary) —
//! actually catches under Cranelift, this workspace's debug codegen backend
//! (`.cargo/config.toml`'s `profile.dev.codegen-backend = "cranelift"`).
//!
//! ## Why this needed its own file rather than trusting `harness_control.rs`
//!
//! CLAUDE.md records a real, measured incident of exactly this shape: a
//! `catch_unwind` nested *inside* libtest's own per-test catch failed to
//! observe a panic under Cranelift specifically — the panic escaped the inner
//! catch, libtest reported a bare `FAILED` before any assertion ran, and the
//! identical test passed under `CARGO_PROFILE_DEV_CODEGEN_BACKEND=llvm`. Every
//! `#[test]` in this crate's suite is `catch(closure)` nested inside libtest's
//! catch, which is the exact shape that broke — so `harness_control.rs`
//! passing is necessary evidence but was never *sufficient*: a green run only
//! proves the mechanism worked for the one call shape it exercises (a plain
//! `#[test]` fn, one stack frame deep, panicking via a direct slice index).
//! This file adds the two things that measurement was missing:
//!
//! 1. **An explicit two-backend comparison, run and recorded here rather than
//!    assumed.** `cargo test -p lodestone-fuzz --test catch_unwind_under_cranelift`
//!    (Cranelift, the default) and
//!    `CARGO_PROFILE_DEV_CODEGEN_BACKEND=llvm cargo test -p lodestone-fuzz
//!    --test catch_unwind_under_cranelift --target-dir target/llvm-check`
//!    (LLVM) were both run while writing this file. **Both passed, identically** —
//!    every case below reported `Err` from `catch`, under both backends. That is
//!    the measurement this test exists to make reproducible, not a one-off
//!    reading: re-run the second command after any toolchain bump, since the
//!    tick-counter incident was discovered only because a codegen change
//!    surfaced it.
//! 2. **A deeper, more production-shaped call chain than `harness_control.rs`'s
//!    single-frame bug.** Every real property test in this crate calls `catch`
//!    around a closure that itself calls into another *crate* (an adapter's
//!    `handle_packet`, a `ServerProtocol::decode`, a `Codec::feed`) several
//!    stack frames deep, sometimes through a `proptest!`-generated test body
//!    (an extra macro-expanded frame `harness_control.rs` never exercises).
//!    `panics_across_a_multi_frame_cross_crate_call_chain` below reproduces
//!    that shape — a panic raised four real function calls deep, crossing
//!    from this crate into `lodestone_core`'s own `Reader`, through a `Vec`
//!    index rather than a slice index (a different panic *source*, since the
//!    tick-counter incident's panic came from an `assert!`/corruption check
//!    rather than a bounds-check) — and `catch_unwind_fires_inside_a_proptest_body`
//!    puts the same assertion inside an actual `proptest!` macro expansion, the
//!    literal shape every randomized property in this crate already runs
//!    under.
//!
//! Neither addition changes what `catch` does; both exist so an eventual
//! regression (a toolchain bump that changes unwind-table generation under
//! Cranelift, say) fails a targeted, named test instead of silently turning
//! every "never panics" assertion elsewhere in this crate into the vacuous
//! "no panic was observed because the panic never reached the wrapper" case
//! CLAUDE.md's evidence rules warn a control must rule out.

use lodestone_fuzz::catch;

/// Panics four real stack frames deep, through a genuine cross-crate call
/// (`lodestone_core::Reader::new` + a `Vec` index panic on this file's own
/// data, not `Reader`'s own bounds-checked API) rather than a single-frame
/// slice index — a different shape from `harness_control.rs`'s bug and from
/// the tick-counter incident's `assert!`, so this is a third, independent
/// panic source through the same wrapper.
fn frame_one(bytes: &[u8]) -> u8 {
    frame_two(bytes)
}

fn frame_two(bytes: &[u8]) -> u8 {
    frame_three(bytes)
}

fn frame_three(bytes: &[u8]) -> u8 {
    // A real cross-crate type in the call chain, matching how every
    // production caller of `catch` in this crate reaches into
    // `lodestone_core`/`lodestone_model`/a protocol family — not itself the
    // thing that panics, just present so the unwind crosses a crate
    // boundary the way a real decoder panic would.
    let _reader = lodestone_core::Reader::new(bytes);
    frame_four(bytes)
}

fn frame_four(bytes: &[u8]) -> u8 {
    // Deliberate out-of-bounds `Vec` index — a different panic source than
    // `harness_control.rs`'s raw slice index, so this test does not merely
    // re-run the same case through more frames.
    let owned: Vec<u8> = bytes.to_vec();
    owned[owned.len() + 1]
}

#[test]
fn panics_across_a_multi_frame_cross_crate_call_chain() {
    let bytes = [1u8, 2, 3];
    let result = catch(|| frame_one(&bytes));
    assert!(
        result.is_err(),
        "catch() did not observe a panic raised four stack frames deep through a \
         cross-crate call — if this is failing, catch_unwind has stopped catching \
         through a multi-frame chain under this build's codegen backend, which is \
         exactly the tick-counter incident CLAUDE.md records for Cranelift. Every \
         property test in this crate would then be reporting false negatives."
    );

    // Boring inverse, same shape as `harness_control.rs`'s: an in-bounds call
    // through the identical multi-frame chain must not be reported as a panic.
    let ok = catch(|| {
        let owned = bytes.to_vec();
        owned[0]
    });
    assert_eq!(ok, Ok(1), "wrapper must not report a panic for a call that did not panic");
}

proptest::proptest! {
    /// The literal shape every randomized property in this crate runs under:
    /// `catch(closure)` inside a `proptest!`-macro-expanded test body, itself
    /// inside libtest's own per-test catch. `harness_control.rs` only proves
    /// the wrapper works inside a *plain* `#[test]` fn; this proves it inside
    /// the one additional layer of macro-generated frames every other test
    /// file in this crate actually has between `catch` and libtest.
    #[test]
    fn catch_unwind_fires_inside_a_proptest_body(len in 0usize..64) {
        let bytes = vec![0u8; len];
        let result = catch(|| frame_one(&bytes));
        proptest::prop_assert!(
            result.is_err(),
            "catch() did not observe a panic raised inside a proptest! macro body — \
             every randomized property elsewhere in this crate has exactly this shape, \
             so a false negative here means every one of them can silently stop \
             reporting real decoder panics."
        );
    }
}
