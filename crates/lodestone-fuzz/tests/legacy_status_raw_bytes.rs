//! Bounded raw-byte robustness coverage for the legacy status boundary.
//!
//! `legacy_status_model.rs` generates valid UTF-16BE responses and compares
//! their fields with an independent model. That is the correctness lane, but
//! it cannot exercise malformed framing, truncated code units, or arbitrary
//! invalid UTF-16. This companion lane keeps the input boundary hostile and
//! asks only the safety property: malformed input may return an error, but it
//! must never panic.

use lodestone_fuzz::catch;
use lodestone_net::parse_legacy_status;
use proptest::prelude::*;

const CASES: u32 = 256;
const MAX_INPUT_BYTES: usize = 4096;

/// Fixed boundary inputs keep the most important framing cases deterministic
/// even when a proptest run is filtered or its RNG strategy changes.
const EDGE_INPUTS: &[&[u8]] = &[
    &[],
    &[0xff],
    &[0xff, 0xff, 0xff],
    &[0xff, 0x00, 0x00],
    &[0xff, 0xff, 0xff, 0xff],
    &[0xff, 0x00, 0x01, 0x00],
    &[0xff, 0x00, 0x02, 0xd8, 0x00],
    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
];

#[test]
fn fixed_malformed_status_edges_never_panic() {
    for input in EDGE_INPUTS {
        let result = catch(|| parse_legacy_status(input));
        assert!(
            result.is_ok(),
            "legacy status parser panicked for {} bytes {input:02x?}: {}",
            input.len(),
            result.unwrap_err(),
        );
    }
}
proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// Drives the complete response boundary, including the packet id,
    /// UTF-16-unit length, and payload, rather than only testing the already
    /// decoded status text. The size cap makes this suitable for the normal
    /// workspace test run and prevents an accidental strategy expansion from
    /// becoming an unbounded allocation test.
    #[test]
    fn arbitrary_status_bytes_never_panic(
        bytes in proptest::collection::vec(any::<u8>(), 0..=MAX_INPUT_BYTES),
    ) {
        let result = catch(|| parse_legacy_status(&bytes));
        prop_assert!(
            result.is_ok(),
            "legacy status parser panicked for {} bytes: {}",
            bytes.len(),
            result.unwrap_err(),
        );
    }
}
