//! Property: decoding arbitrary bytes must never panic.
//!
//! Covers `VersionAdapter::handle_packet` for all four client families
//! (`v47`, `v340`, `v735`, `v770`) — the exact function every real
//! connection drives, per `crates/protocol/v770/tests/entity_encoders.rs`'s
//! own rationale for testing through it rather than a lower-level decode fn.
//! `v735` speaks protocol 754 despite the folder name; that only matters for
//! interpreting results, not for driving the fuzz, since this harness never
//! derives a protocol number from the family name.
//!
//! Two layers, per `docs/fuzz-harness.md`:
//!
//! 1. A **deterministic sweep**: every declared clientbound packet id, in
//!    every state, in every family, against a handful of fixed short
//!    payloads. This guarantees each of the packet ids the four families
//!    together declare is exercised at least once, independent of whatever
//!    proptest's RNG happens to land on.
//! 2. A **randomized property**: proptest picks the family, state, an id
//!    (mostly from the declared set, sometimes a value with no meaning at
//!    all), and an arbitrary-length payload up to 4 KiB. Capped at 512 cases
//!    per run (`PROPTEST_CASES` overrides it) — bounded specifically so this
//!    is affordable inside `cargo test --workspace`, not a job of its own.
//!
//! Neither layer requires an expected output. A `Result::Err` from malformed
//! input is a completely acceptable outcome; a panic is not.

use lodestone_fuzz::{Family, catch};
use lodestone_model::ConnectionState;
use proptest::prelude::*;

const FIXED_PAYLOADS: &[&[u8]] = &[
    &[],
    &[0x00],
    &[0xFF],
    &[0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
    &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    &[0x00; 32],
    &[0xAA; 64],
];

#[test]
fn deterministic_sweep_over_every_declared_clientbound_packet_id() {
    let mut cases = 0usize;
    for family in Family::ALL {
        for state in Family::STATES {
            for &(name, id) in family.clientbound_entries(state) {
                for payload in FIXED_PAYLOADS {
                    cases += 1;
                    let result = catch(|| lodestone_fuzz::decode_clientbound(family, state, id, payload));
                    assert!(
                        result.is_ok(),
                        "{}: {name} (id {id}) panicked on {state:?} payload {payload:02x?}: {}",
                        family.name(),
                        result.unwrap_err(),
                    );
                }
            }
        }
    }
    // A sanity floor, not a target: CLAUDE.md's connectedness numbers put
    // total declared clientbound packets across all four families near 400
    // (v47 74 + v340 80 + v735 92 + v770 141). If this collapses toward zero,
    // the sweep is iterating an empty table, not proving anything.
    assert!(
        cases > 1000,
        "expected well over 1000 (packet id x fixed payload) cases, got {cases} \
         — the packet_ids tables are probably not being reached"
    );
}

fn arb_family() -> impl Strategy<Value = Family> {
    (0..Family::ALL.len()).prop_map(|i| Family::ALL[i])
}

fn arb_state() -> impl Strategy<Value = ConnectionState> {
    (0..Family::STATES.len()).prop_map(|i| Family::STATES[i])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Mostly picks a declared packet id (so most cases actually reach a
    /// real decode body rather than an immediate "unknown id" bail-out), but
    /// 1 in 8 cases uses a fully arbitrary `i32`, including negative and
    /// huge values, so the "packet id we've never heard of" path gets
    /// exercised too.
    #[test]
    fn handle_packet_never_panics(
        family in arb_family(),
        state in arb_state(),
        use_declared_id in prop::bool::weighted(0.875),
        id_pick in any::<usize>(),
        arbitrary_id in any::<i32>(),
        payload in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        let entries = family.clientbound_entries(state);
        let packet_id = if use_declared_id && !entries.is_empty() {
            entries[id_pick % entries.len()].1
        } else {
            arbitrary_id
        };

        let result = catch(|| lodestone_fuzz::decode_clientbound(family, state, packet_id, &payload));
        prop_assert!(
            result.is_ok(),
            "{}: state {:?} packet_id {} payload len {} panicked: {}",
            family.name(), state, packet_id, payload.len(), result.unwrap_err(),
        );
    }
}
