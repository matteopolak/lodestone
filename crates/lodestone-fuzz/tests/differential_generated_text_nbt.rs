//! Deterministic, shrinkable model check for modern NBT chat components.
//!
//! `Text::from_nbt` is the production fold used by modern chat, inventory and
//! scoreboard packet adapters. The generated side starts as a small component
//! grammar, then constructs `Nbt` values directly; it never uses `Text` to
//! produce either an input or an expected value. That keeps the comparison out
//! of the parser's implementation while covering scalar, list and compound
//! `text`/`extra` forms that reach those adapter consumers.
//!
//! The fixed proptest seed and bounded grammar make failures reproducible and
//! shrinkable. A separate reader which discards an `extra` child is required to
//! fail, proving the detector can observe a production/parser disagreement.

use lodestone_core::{Nbt, NbtTag};
use lodestone_model::Text;
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 192;
const SEED: u64 = 0x54_45_58_54_5f_4e_42_54;

#[derive(Clone, Debug)]
enum Component {
    Text(String),
    Number(i16),
    Empty,
    Sequence(Vec<Self>),
    Compound {
        text: Option<String>,
        extra: Vec<Self>,
    },
}

impl Component {
    fn to_nbt(&self) -> Nbt {
        match self {
            Self::Text(text) => Nbt::String(text.clone()),
            Self::Number(number) => Nbt::Int(i32::from(*number)),
            Self::Empty => Nbt::ByteArray(Vec::new()),
            Self::Sequence(items) => Nbt::List {
                element_type: NbtTag::Compound,
                elements: items.iter().map(Self::to_nbt).collect(),
            },
            Self::Compound { text, extra } => {
                let mut fields = Vec::new();
                if let Some(text) = text {
                    fields.push(("text".to_owned(), Nbt::String(text.clone())));
                }
                if !extra.is_empty() {
                    fields.push((
                        "extra".to_owned(),
                        Nbt::List {
                            element_type: NbtTag::Compound,
                            elements: extra.iter().map(Self::to_nbt).collect(),
                        },
                    ));
                }
                Nbt::Compound(fields)
            }
        }
    }

    /// Independent plain-text semantics for the bounded NBT component grammar.
    fn expected_plain(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Number(number) => number.to_string(),
            Self::Empty => String::new(),
            Self::Sequence(items) => items.iter().map(Self::expected_plain).collect(),
            Self::Compound { text, extra } => {
                let mut plain = text.clone().unwrap_or_default();
                for child in extra {
                    plain.push_str(&child.expected_plain());
                }
                plain
            }
        }
    }
}

fn ascii_string() -> impl Strategy<Value = String> {
    collection::vec(0x20_u8..0x7f, 0..24).prop_map(|bytes| {
        String::from_utf8(bytes).expect("the bounded ASCII strategy is valid UTF-8")
    })
}

fn component_strategy() -> impl Strategy<Value = Component> {
    let leaf = prop_oneof![
        ascii_string().prop_map(Component::Text),
        any::<i16>().prop_map(Component::Number),
        Just(Component::Empty),
    ];

    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            collection::vec(inner.clone(), 0..4).prop_map(Component::Sequence),
            (
                prop::option::of(ascii_string()),
                collection::vec(inner, 0..4)
            )
                .prop_map(|(text, extra)| Component::Compound { text, extra },),
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
    Text::from_nbt(&component.to_nbt()).to_plain_string()
}

#[test]
fn generated_nbt_components_match_the_independent_plain_text_model() {
    runner(CASES)
        .run(&component_strategy(), |component| {
            let expected = component.expected_plain();
            let actual = production_plain(&component);
            prop_assert_eq!(actual, expected, "generated component: {:?}", component);
            Ok(())
        })
        .expect("the production NBT parser must match the independent model");
}

#[test]
fn wrong_reader_that_discards_extra_is_detected() {
    let control = Component::Compound {
        text: Some("parent".to_owned()),
        extra: vec![Component::Text("child".to_owned())],
    };
    let failure = runner(1)
        .run(&Just(control.clone()), |component| {
            let actual = production_plain(&component);
            let intentionally_wrong = match component {
                Component::Compound { text, .. } => text.unwrap_or_default(),
                _ => String::new(),
            };
            prop_assert_eq!(intentionally_wrong, actual, "detector control");
            Ok(())
        })
        .expect_err("a reader that drops an NBT extra child must disagree");

    match failure {
        TestError::Fail(_, minimal) => assert_eq!(minimal.expected_plain(), "parentchild"),
        TestError::Abort(reason) => panic!("the detector control must fail, not abort: {reason}"),
    }
}
