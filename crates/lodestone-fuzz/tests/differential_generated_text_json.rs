//! Deterministic, shrinkable model check for JSON chat-component parsing.
//!
//! `lodestone_model::Text::from_json` uses the production JSON front end. The
//! generated side has no `Text` values and never calls the production parser:
//! it is a small grammar for the literal, scalar, sequence and `extra` forms
//! and a separate plain-text fold. `serde_json` writes the wire JSON, including
//! quotes and escapes, so this is neither an encode/decode round trip nor a
//! hand-spelled collection of syntactically easy examples.
//!
//! A fixed proptest seed makes the 192-case slice repeatable. If the parser and
//! fold disagree, `TestRunner` returns the smallest grammar value which still
//! differs; the detector control below proves that path is live.

use std::cell::Cell;

use lodestone_model::Text;
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};
use serde_json::{Map, Value};

const CASES: u32 = 192;
const SEED: u64 = 0x54_45_58_54_5f_4a_53_4f;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Component {
    Text(String),
    Number(i16),
    Bool(bool),
    Null,
    Sequence(Vec<Self>),
    Object {
        text: Option<String>,
        extra: Vec<Self>,
    },
}

impl Component {
    fn to_json(&self) -> Value {
        match self {
            Self::Text(text) => Value::String(text.clone()),
            Self::Number(number) => Value::Number((*number).into()),
            Self::Bool(value) => Value::Bool(*value),
            Self::Null => Value::Null,
            Self::Sequence(items) => Value::Array(items.iter().map(Self::to_json).collect()),
            Self::Object { text, extra } => {
                let mut object = Map::new();
                if let Some(text) = text {
                    object.insert("text".to_owned(), Value::String(text.clone()));
                }
                if !extra.is_empty() {
                    object.insert(
                        "extra".to_owned(),
                        Value::Array(extra.iter().map(Self::to_json).collect()),
                    );
                }
                Value::Object(object)
            }
        }
    }

    /// Independent plain-text semantics for the deliberately bounded grammar.
    ///
    /// The production parser's sequence implementation makes its first item
    /// the parent and appends subsequent items as children. Rendering that tree
    /// still visits the original sequence left-to-right, so concatenation is
    /// the model's rule without using any production `Text` method.
    fn expected_plain(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Null => String::new(),
            Self::Sequence(items) => items.iter().map(Self::expected_plain).collect(),
            Self::Object { text, extra } => {
                let mut plain = text.clone().unwrap_or_default();
                for child in extra {
                    plain.push_str(&child.expected_plain());
                }
                plain
            }
        }
    }
}

fn ascii_json_string() -> impl Strategy<Value = String> {
    // Includes JSON's quote, slash and control escapes once serialized, without
    // mixing Unicode normalisation into a parser-order property.
    collection::vec(0x08_u8..0x7f, 0..24).prop_map(|bytes| {
        String::from_utf8(bytes).expect("the bounded ASCII strategy is valid UTF-8")
    })
}

fn component_strategy() -> impl Strategy<Value = Component> {
    let leaf = prop_oneof![
        ascii_json_string().prop_map(Component::Text),
        any::<i16>().prop_map(Component::Number),
        any::<bool>().prop_map(Component::Bool),
        Just(Component::Null),
    ];

    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            collection::vec(inner.clone(), 0..4).prop_map(Component::Sequence),
            (prop::option::of(ascii_json_string()), collection::vec(inner, 0..4)).prop_map(
                |(text, extra)| Component::Object { text, extra },
            ),
        ]
    })
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

fn production_plain(component: &Component) -> String {
    let json = serde_json::to_string(&component.to_json()).expect("generated JSON serializes");
    Text::from_json(&json).to_plain_string()
}

#[test]
fn generated_json_components_match_the_independent_plain_text_model() {
    runner(CASES)
        .run(&component_strategy(), |component| {
            let expected = component.expected_plain();
            let actual = production_plain(&component);
            prop_assert_eq!(
                actual,
                expected,
                "serialized generated component: {}",
                serde_json::to_string(&component.to_json()).expect("generated JSON serializes"),
            );
            Ok(())
        })
        .expect("the production parser must match the independent model");
}

#[test]
fn wrong_plain_text_reader_is_detected_and_shrunk() {
    let evaluations = Cell::new(0usize);
    let failure = runner(CASES)
        .run(&component_strategy(), |component| {
            evaluations.set(evaluations.get() + 1);
            let expected = component.expected_plain();
            let intentionally_wrong = String::new();
            prop_assert_eq!(intentionally_wrong, expected, "detector control");
            Ok(())
        })
        .expect_err("the control must not agree with every generated component");

    match failure {
        TestError::Fail(_, minimal) => {
            assert!(
                !minimal.expected_plain().is_empty(),
                "the shrunk counterexample must still exercise a visible component"
            );
        }
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
    assert!(
        evaluations.get() > 1,
        "the fixed-seed control must evaluate shrink candidates after detecting a mismatch"
    );
}
