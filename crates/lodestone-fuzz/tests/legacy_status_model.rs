//! Deterministic, shrinkable model check for legacy server-list status bytes.
//!
//! The production consumer is `lodestone_net::parse_legacy_status`, the pure
//! parser used by the legacy ping fallback. The generated side owns the text
//! layout, UTF-16BE framing, and expected fields; it never asks production code
//! to encode a packet or derive the expected value. Both supported wire layouts
//! are covered, including trimmed numeric fields, optional protocol metadata,
//! and non-BMP text that exercises surrogate-pair framing.
//!
//! A fixed ChaCha seed bounds the campaign and makes every case replayable.
//! The detector control intentionally drops the modern protocol version and is
//! required to fail after shrinking, proving that the assertion observes the
//! parsed result rather than accepting every generated packet.

use std::cell::Cell;

use lodestone_net::{LegacyStatus, parse_legacy_status};
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 256;
const SEED: u64 = 0x4c_45_47_41_43_59_53;

#[derive(Clone, Debug)]
enum StatusCase {
    Modern {
        protocol: Option<i32>,
        server_version: String,
        motd: String,
        online: i32,
        max: i32,
    },
    Old {
        motd: String,
        online: i32,
        max: i32,
    },
}

impl StatusCase {
    fn text(&self) -> String {
        match self {
            Self::Modern {
                protocol,
                server_version,
                motd,
                online,
                max,
            } => {
                let protocol = protocol.map_or_else(
                    || "not-a-number".to_owned(),
                    |value| value.to_string(),
                );
                format!(
                    "§1\0 {protocol} \0{server_version}\0{motd}\0 {online} \0 {max} "
                )
            }
            Self::Old { motd, online, max } => format!("{motd}§ {online} § {max} "),
        }
    }

    fn expected(&self) -> LegacyStatus {
        match self {
            Self::Modern {
                protocol,
                server_version,
                motd,
                online,
                max,
            } => LegacyStatus {
                protocol_version: *protocol,
                server_version: Some(server_version.clone()),
                motd: motd.clone(),
                online_players: *online,
                max_players: *max,
            },
            Self::Old { motd, online, max } => LegacyStatus {
                protocol_version: None,
                server_version: None,
                motd: motd.clone(),
                online_players: *online,
                max_players: *max,
            },
        }
    }
}

/// Frames text as a legacy kick response without using the production writer.
fn packet_for(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let length =
        u16::try_from(units.len()).expect("the generated response fits its length field");
    let mut packet = Vec::with_capacity(3 + units.len() * 2);
    packet.push(0xff);
    packet.extend_from_slice(&length.to_be_bytes());
    for unit in units {
        packet.extend_from_slice(&unit.to_be_bytes());
    }
    packet
}

fn field_character() -> impl Strategy<Value = char> {
    prop::sample::select(vec![
        'a', 'M', '0', ' ', '\t', '-', '_', '.', '/', '\n', 'é', '☃', '🪨',
    ])
}

fn field_strategy() -> impl Strategy<Value = String> {
    collection::vec(field_character(), 0..16)
        .prop_map(|characters| characters.into_iter().collect())
}

fn number_strategy() -> impl Strategy<Value = i32> {
    any::<i16>().prop_map(i32::from)
}

fn modern_case_strategy() -> impl Strategy<Value = StatusCase> {
    (
        prop_oneof![Just(true), Just(false)],
        number_strategy(),
        field_strategy(),
        field_strategy(),
        number_strategy(),
        number_strategy(),
    )
        .prop_map(|(has_protocol, protocol, server_version, motd, online, max)| {
            StatusCase::Modern {
                protocol: has_protocol.then_some(protocol),
                server_version,
                motd,
                online,
                max,
            }
        })
}

fn status_case_strategy() -> impl Strategy<Value = StatusCase> {
    prop_oneof![
        modern_case_strategy(),
        (field_strategy(), number_strategy(), number_strategy()).prop_map(|(motd, online, max)| {
            StatusCase::Old { motd, online, max }
        }),
    ]
}

fn runner(cases: u32) -> TestRunner {
    TestRunner::new(Config {
        cases,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    })
}

#[test]
fn generated_legacy_status_packets_match_the_independent_model() {
    runner(CASES)
        .run(&status_case_strategy(), |case| {
            let packet = packet_for(&case.text());
            let actual =
                parse_legacy_status(&packet).expect("generated legacy packet is valid");
            prop_assert_eq!(actual, case.expected(), "generated text: {:?}", case.text());
            Ok(())
        })
        .expect("the production legacy status parser must match the independent model");
}

#[test]
fn dropping_modern_protocol_metadata_is_detected_and_shrunk() {
    let evaluations = Cell::new(0usize);
    let failure = runner(CASES)
        .run(&modern_case_strategy(), |case| {
            evaluations.set(evaluations.get() + 1);
            let packet = packet_for(&case.text());
            let actual =
                parse_legacy_status(&packet).expect("generated modern packet is valid");
            let mut intentionally_wrong = case.expected();
            intentionally_wrong.protocol_version = None;
            prop_assert_eq!(
                intentionally_wrong,
                actual,
                "detector control for generated text: {:?}",
                case.text()
            );
            Ok(())
        })
        .expect_err("dropping the modern protocol must disagree with the parser");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(
                matches!(minimal, StatusCase::Modern { protocol: Some(_), .. }),
                "the shrunk control must retain protocol metadata: {minimal:?}"
            );
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(
        evaluations.get() > 1,
        "the fixed-seed control must evaluate shrink candidates after detecting a mismatch"
    );
}
