//! Deterministic, shrinkable grammar check for production resource keys.
//!
//! `lodestone_model::ResourceKey` is the identifier gate packet adapters use
//! before they retain a registry, sound, channel, or command-parser key. The
//! generated side is deliberately just character vectors plus an independent
//! grammar result; it never constructs a `ResourceKey` to make either input or
//! expected output. That makes separator handling, implicit namespaces, and
//! the distinct namespace/path alphabets observable at this shared model seam.
//!
//! The fixed seed bounds this slice to short strings that shrink directly to
//! the offending character or separator. The wrong-default-namespace control
//! is required to fail and shrink, proving the comparison sees bare keys rather
//! than accepting every parser result.

use std::{cell::Cell, str::FromStr};

use lodestone_model::ResourceKey;
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 256;
const SEED: u64 = 0x52_45_53_4f_55_52_43_45;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Expected {
    Key { namespace: String, path: String },
    Empty,
    EmptyNamespace,
    EmptyPath,
    TooManySeparators,
    InvalidNamespace(char),
    InvalidPath(char),
}

fn valid_namespace_character(character: char) -> bool {
    matches!(character, 'a'..='z' | '0'..='9' | '_' | '.' | '-')
}

fn valid_path_character(character: char) -> bool {
    valid_namespace_character(character) || character == '/'
}

fn expected_key(value: &str) -> Expected {
    if value.is_empty() {
        return Expected::Empty;
    }

    let (namespace, path) = match value.split_once(':') {
        Some((namespace, path)) => {
            if path.contains(':') {
                return Expected::TooManySeparators;
            }
            (namespace, path)
        }
        None => ("minecraft", value),
    };

    if namespace.is_empty() {
        return Expected::EmptyNamespace;
    }
    if let Some(character) = namespace
        .chars()
        .find(|character| !valid_namespace_character(*character))
    {
        return Expected::InvalidNamespace(character);
    }
    if path.is_empty() {
        return Expected::EmptyPath;
    }
    if let Some(character) = path.chars().find(|character| !valid_path_character(*character)) {
        return Expected::InvalidPath(character);
    }

    Expected::Key {
        namespace: namespace.to_owned(),
        path: path.to_owned(),
    }
}

fn production_key(value: &str) -> Expected {
    match ResourceKey::from_str(value) {
        Ok(key) => Expected::Key {
            namespace: key.namespace().to_owned(),
            path: key.path().to_owned(),
        },
        Err(error) => match error {
            lodestone_model::ids::ParseIdentifierError::Empty => Expected::Empty,
            lodestone_model::ids::ParseIdentifierError::EmptyNamespace => Expected::EmptyNamespace,
            lodestone_model::ids::ParseIdentifierError::EmptyPath => Expected::EmptyPath,
            lodestone_model::ids::ParseIdentifierError::TooManySeparators => Expected::TooManySeparators,
            lodestone_model::ids::ParseIdentifierError::InvalidNamespaceChar(character) => {
                Expected::InvalidNamespace(character)
            }
            lodestone_model::ids::ParseIdentifierError::InvalidPathChar(character) => {
                Expected::InvalidPath(character)
            }
        },
    }
}

fn character_strategy() -> impl Strategy<Value = char> {
    prop_oneof![
        prop::sample::select(vec!['a', 'm', 'z', '0', '9', '_', '.', '-', '/', ':']),
        prop::sample::select(vec!['A', ' ', '#', '|', '\\', 'é', '🪨']),
    ]
}

fn key_strategy() -> impl Strategy<Value = String> {
    collection::vec(character_strategy(), 0..32).prop_map(|characters| characters.into_iter().collect())
}

fn bare_valid_key_strategy() -> impl Strategy<Value = String> {
    collection::vec(
        prop::sample::select(vec!['a', 'm', 'z', '0', '9', '_', '.', '-', '/']),
        1..32,
    )
    .prop_map(|characters| characters.into_iter().collect())
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
fn generated_resource_keys_match_the_independent_grammar() {
    runner(CASES)
        .run(&key_strategy(), |value| {
            prop_assert_eq!(
                production_key(&value),
                expected_key(&value),
                "generated key: {:?}",
                value
            );
            Ok(())
        })
        .expect("the production resource-key parser must match the independent grammar");
}

#[test]
fn wrong_default_namespace_is_detected_and_shrunk() {
    let evaluations = Cell::new(0usize);
    let failure = runner(CASES)
        .run(&bare_valid_key_strategy(), |value| {
            evaluations.set(evaluations.get() + 1);
            let actual = production_key(&value);
            let intentionally_wrong = Expected::Key {
                namespace: "lodestone".to_owned(),
                path: value.clone(),
            };
            prop_assert_eq!(
                intentionally_wrong,
                actual,
                "detector control for bare key: {:?}",
                value
            );
            Ok(())
        })
        .expect_err("the wrong default namespace must disagree with the production parser");

    match failure {
        TestError::Fail(_, minimal) => assert!(
            minimal.len() <= 1,
            "the wrong-default control should shrink to one valid path character, got {minimal:?}"
        ),
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(
        evaluations.get() > 1,
        "the fixed-seed control must evaluate shrink candidates after detecting a mismatch"
    );
}
