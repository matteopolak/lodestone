//! The control CLAUDE.md's evidence rules require before trusting an
//! absence: "assertions of an absence need a control proving the detector
//! works." Every other test in this crate asserts real decoders *never*
//! panic under [`lodestone_fuzz::catch`]; that assertion is worthless if
//! `catch` would report "no panic" regardless of what actually happened. So
//! this file runs a decoder with a known, textbook bug — unchecked
//! length-prefixed indexing, the single most common wire-decoder mistake —
//! through the exact same wrapper, and asserts the wrapper *does* observe
//! the panic.
//!
//! This test is deliberately not neuter-and-restore against a shared file:
//! the buggy decoder lives entirely in this file, so there is nothing to
//! restore and no window where a red test could be mistaken for someone
//! else's in-flight regression (see `CLAUDE.md`'s "a red test in this
//! checkout may be someone else's deliberate neuter" hazard) — the "bug"
//! here is permanent and intentional.

/// A decoder with the exact defect class this harness exists to catch: it
/// trusts a length byte and indexes with it, with no check against the
/// buffer's actual length. Real instances of this mistake are what turned
/// into 49 × "unexpected end of input" against a live server per
/// `CLAUDE.md`; this one is a minimal stand-in so the control does not
/// depend on any shared or off-limits file.
fn naive_length_prefixed_decode(bytes: &[u8]) -> u8 {
    let len = bytes[0] as usize;
    bytes[1 + len]
}

#[test]
fn control_harness_detects_a_naive_bounds_bug() {
    // Three bytes total, so `len` (0..=255) almost always names an index
    // past the end. Deterministic, not proptest-driven: the point is to
    // prove `catch` reports the panic, not to explore an input space.
    let bytes = [3u8, 0u8, 1u8];

    let result = lodestone_fuzz::catch(|| naive_length_prefixed_decode(&bytes));

    assert!(
        result.is_err(),
        "control decoder did not panic on out-of-bounds input — the control's \
         premise is false, which means the panic-catching wrapper below has \
         never actually been exercised by anything in this crate, and every \
         'never panics' assertion elsewhere is vacuous. See CLAUDE.md's rule \
         that absence-of-panic assertions need a proven-firing detector."
    );

    // And the boring case: `len` small enough to stay in bounds must not be
    // reported as a panic — if it were, the wrapper itself would be the bug,
    // silently turning every clean decode into a false failure.
    let in_bounds = [1u8, 0u8, 42u8];
    let ok = lodestone_fuzz::catch(|| naive_length_prefixed_decode(&in_bounds));
    assert_eq!(ok, Ok(42), "wrapper must not report a panic for a call that did not panic");
}
