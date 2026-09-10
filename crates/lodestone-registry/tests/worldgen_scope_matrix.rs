//! Independent coverage for the registry's hosted-worldgen capability seam.
//!
//! The unit test beside `SERVER_FAMILIES` can accidentally agree with an
//! omitted row because both the expected values and the observed values come
//! from the same table. This integration test spells the hosted protocol rows
//! from the feature contract, then checks the registry, the boxed protocol and
//! the checked source constructor as three separate consumers of that contract.

use lodestone_server::{ServerProtocol, WorldgenScope};

fn expected_hosted_protocols() -> Vec<i32> {
    let mut protocols = Vec::new();
    #[cfg(feature = "v1-7")]
    protocols.extend([5]);
    #[cfg(feature = "v1-8")]
    protocols.extend([47]);
    #[cfg(feature = "v26-2")]
    protocols.extend([776]);
    #[cfg(feature = "v1-9")]
    protocols.extend([110, 210, 316, 340]);
    #[cfg(feature = "v1-13")]
    protocols.extend([404]);
    #[cfg(feature = "v1-14")]
    protocols.extend([498, 578, 754]);
    #[cfg(feature = "v1-17")]
    protocols.extend([756, 758]);
    #[cfg(feature = "v1-19")]
    protocols.extend([762]);
    #[cfg(feature = "v1-20-6")]
    protocols.extend([766]);
    #[cfg(feature = "v1-21-11")]
    protocols.extend([774]);
    protocols
}

fn expected_scope_for_protocol(protocol: i32) -> WorldgenScope {
    match protocol {
        776 => WorldgenScope::V26_2,
        _ => WorldgenScope::None,
    }
}

#[test]
fn registry_and_host_protocols_share_the_explicit_worldgen_matrix() {
    let expected = expected_hosted_protocols();
    assert_eq!(
        lodestone_registry::hosted_protocols(),
        expected,
        "hosted protocol coverage must match the feature-gated product contract"
    );

    for protocol in expected {
        let expected_scope = expected_scope_for_protocol(protocol);
        let registry_scope = lodestone_registry::worldgen_scope_for_protocol(protocol)
            .unwrap_or_else(|| panic!("hosted protocol {protocol} has no registry scope"));
        assert_eq!(
            registry_scope, expected_scope,
            "registry scope drifted for hosted protocol {protocol}"
        );

        let host = lodestone_registry::server_protocol_for_protocol(protocol)
            .unwrap_or_else(|| panic!("hosted protocol {protocol} has no server protocol"));
        assert_eq!(
            host.worldgen_scope(), expected_scope,
            "boxed server protocol drifted from registry scope for {protocol}"
        );

        let checked = lodestone_server::overworld_chunk_source_checked(registry_scope, 42);
        match expected_scope {
            WorldgenScope::V26_2 => {
                let source = checked
                    .expect("the embedded source must be constructible for its own scope");
                assert!(
                    source.height() > 0,
                    "positive scope control must build a non-empty embedded source"
                );
            }
            WorldgenScope::None => {
                let error = checked.err().unwrap_or_else(|| {
                    panic!("legacy protocol {protocol} received the embedded source")
                });
                assert_eq!(
                    error,
                    lodestone_server::WorldgenScopeMismatch {
                        requested: WorldgenScope::None,
                    },
                    "legacy protocol {protocol} must fail with the visible capability boundary"
                );
            }
        }
    }

    // These controls distinguish an exact protocol lookup from a table that
    // merely returns a default scope for every nearby number.
    assert!(lodestone_registry::worldgen_scope_for_protocol(775).is_none());
    assert!(lodestone_registry::worldgen_scope_for_protocol(777).is_none());
}

#[test]
fn no_feature_build_has_no_hosting_or_worldgen_scope_rows() {
    if !expected_hosted_protocols().is_empty() {
        return;
    }

    assert!(lodestone_registry::hosted_protocols().is_empty());
    assert!(lodestone_registry::compiled_server_families().is_empty());
    assert!(lodestone_registry::server_protocol_for_protocol(776).is_none());
    assert!(lodestone_registry::worldgen_scope_for_protocol(776).is_none());
    let error = lodestone_server::overworld_chunk_source_checked(WorldgenScope::None, 42)
        .err()
        .expect("a no-feature worldgen request must remain visibly refused");
    assert_eq!(
        error,
        lodestone_server::WorldgenScopeMismatch {
            requested: WorldgenScope::None,
        }
    );
}
