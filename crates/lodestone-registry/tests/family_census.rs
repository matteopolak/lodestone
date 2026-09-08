//! Cross-family protocol census and registry-resolution control.
//!
//! Version-specific fixtures belong to their family crates. This test owns the
//! aggregation boundary instead: every protocol exposed by the registry must
//! be a member of the declared ten-family census, must appear once, and must
//! resolve to an adapter that claims it. The exact full set is asserted when
//! the workspace test is run with all family features.

use std::collections::BTreeSet;

const EXPECTED_PROTOCOLS: &[i32] = &[
    5, 47, 110, 210, 316, 340, 404, 498, 578, 754, 756, 758, 762, 766, 774, 776,
];

const EXPECTED_FAMILIES: &[&str] = &[
    "v1-7",
    "v1-8",
    "v1-9",
    "v1-13",
    "v1-14",
    "v1-17",
    "v1-19",
    "v1-20-6",
    "v1-21-11",
    "v26-2",
];

#[test]
fn compiled_family_census_has_unique_resolvable_protocol_rows() {
    let mut protocols = lodestone_registry::supported_protocols();
    protocols.sort_unstable();
    assert_eq!(
        protocols.windows(2).filter(|window| window[0] == window[1]).count(),
        0,
        "a protocol may belong to only one compiled family"
    );
    assert!(
        protocols.iter().all(|protocol| EXPECTED_PROTOCOLS.contains(protocol)),
        "registry exposed a protocol outside the checked-in family census: {protocols:?}"
    );
    for protocol in &protocols {
        let adapter = lodestone_registry::adapter_for_protocol(*protocol)
            .unwrap_or_else(|| panic!("registry protocol {protocol} must resolve"));
        assert!(
            adapter.supports(*protocol),
            "adapter resolved for protocol {protocol} must claim that protocol"
        );
    }

    let families = lodestone_registry::compiled_families();
    let family_set: BTreeSet<_> = families.iter().copied().collect();
    assert_eq!(
        family_set.len(),
        families.len(),
        "a family may appear only once in the compiled registry"
    );
    assert!(
        families
            .iter()
            .all(|family| EXPECTED_FAMILIES.contains(family)),
        "registry exposed a family outside the checked-in family census: {families:?}"
    );

    if cfg!(all(
        feature = "v1-7",
        feature = "v1-8",
        feature = "v1-9",
        feature = "v1-13",
        feature = "v1-14",
        feature = "v1-17",
        feature = "v1-19",
        feature = "v1-20-6",
        feature = "v1-21-11",
        feature = "v26-2"
    )) {
        assert_eq!(protocols, EXPECTED_PROTOCOLS);
        let expected_family_set: BTreeSet<_> = EXPECTED_FAMILIES.iter().copied().collect();
        assert_eq!(family_set, expected_family_set);
    }
}
