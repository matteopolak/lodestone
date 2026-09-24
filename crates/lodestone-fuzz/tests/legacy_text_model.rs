//! Deterministic, shrinkable model check for legacy `§`-formatted text.
//!
//! `Text::from_legacy` is the production parser used by the 1.8 and 1.9
//! adapter paths before their display names, prefixes and suffixes reach the
//! shared styled-text renderers. The generated side owns the token stream,
//! formatting-code table, reset semantics and expected spans; it does not call
//! `Text` or its helpers to construct an input or expected output.
//!
//! The fixed ChaCha seed makes the bounded campaign reproducible and lets
//! proptest shrink a failing token stream. A literal reset witness runs a
//! deliberately wrong model which retains the preceding colour, proving the
//! assertion distinguishes the reset style from an inert parser result.

use lodestone_model::{Text, TextColor, TextSpan, TextStyle};
use proptest::collection;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, RngSeed, TestError, TestRunner};

const CASES: u32 = 256;
const SEED: u64 = 0x4c_45_47_41_43_59_54_58;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Text(String),
    Code(char),
    DanglingPrefix,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExpectedSpan {
    text: String,
    style: TextStyle,
}

impl Token {
    fn write_to(&self, output: &mut String) {
        match self {
            Self::Text(text) => output.push_str(text),
            Self::Code(code) => {
                output.push('§');
                output.push(*code);
            }
            Self::DanglingPrefix => output.push('§'),
        }
    }
}

fn text_character() -> impl Strategy<Value = char> {
    prop::sample::select(vec!['a', 'B', '0', ' ', '\n', 'é', '🪨'])
}

fn token_strategy() -> impl Strategy<Value = Token> {
    let text = collection::vec(text_character(), 0..8)
        .prop_map(|characters| Token::Text(characters.into_iter().collect()));
    let codes = prop::sample::select(vec![
        '0', '7', 'a', 'F', 'k', 'L', 'm', 'N', 'o', 'R', 'x', 'z', '☃',
    ])
    .prop_map(Token::Code);
    prop_oneof![text, codes]
}

fn case_strategy() -> impl Strategy<Value = Vec<Token>> {
    (collection::vec(token_strategy(), 0..32), any::<bool>()).prop_map(
        |(mut tokens, trailing_prefix)| {
            if trailing_prefix {
                tokens.push(Token::DanglingPrefix);
            }
            tokens
        },
    )
}

fn input(tokens: &[Token]) -> String {
    let mut input = String::new();
    for token in tokens {
        token.write_to(&mut input);
    }
    input
}

fn colour(code: char) -> Option<TextColor> {
    Some(match code.to_ascii_lowercase() {
        '0' => TextColor::Black,
        '1' => TextColor::DarkBlue,
        '2' => TextColor::DarkGreen,
        '3' => TextColor::DarkAqua,
        '4' => TextColor::DarkRed,
        '5' => TextColor::DarkPurple,
        '6' => TextColor::Gold,
        '7' => TextColor::Gray,
        '8' => TextColor::DarkGray,
        '9' => TextColor::Blue,
        'a' => TextColor::Green,
        'b' => TextColor::Aqua,
        'c' => TextColor::Red,
        'd' => TextColor::LightPurple,
        'e' => TextColor::Yellow,
        'f' => TextColor::White,
        _ => return None,
    })
}

/// Independent state machine for the legacy-code grammar.
fn expected_spans(tokens: &[Token], apply_reset: bool) -> Vec<ExpectedSpan> {
    let mut spans = Vec::new();
    let mut style = TextStyle::default();
    let mut buffer = String::new();

    let flush = |spans: &mut Vec<ExpectedSpan>, buffer: &mut String, style: TextStyle| {
        if !buffer.is_empty() {
            spans.push(ExpectedSpan {
                text: std::mem::take(buffer),
                style,
            });
        }
    };

    for token in tokens {
        match token {
            Token::Text(text) => buffer.push_str(text),
            Token::DanglingPrefix => break,
            Token::Code(code) => {
                let next = if let Some(color) = colour(*code) {
                    Some(TextStyle {
                        color: Some(color),
                        bold: Some(false),
                        italic: Some(false),
                        underlined: Some(false),
                        strikethrough: Some(false),
                        obfuscated: Some(false),
                        font: None,
                    })
                } else {
                    let mut next = style;
                    match code.to_ascii_lowercase() {
                        'k' => next.obfuscated = Some(true),
                        'l' => next.bold = Some(true),
                        'm' => next.strikethrough = Some(true),
                        'n' => next.underlined = Some(true),
                        'o' => next.italic = Some(true),
                        'r' if apply_reset => next = TextStyle::default(),
                        'r' => {}
                        _ => continue,
                    }
                    Some(next)
                };
                if let Some(next) = next {
                    flush(&mut spans, &mut buffer, style);
                    style = next;
                }
            }
        }
    }
    flush(&mut spans, &mut buffer, style);
    spans
}

fn production_spans(tokens: &[Token]) -> Vec<ExpectedSpan> {
    Text::from_legacy(&input(tokens))
        .resolve(&|_| None)
        .to_spans()
        .into_iter()
        .map(|TextSpan { text, style }| ExpectedSpan { text, style })
        .collect()
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
fn generated_legacy_text_matches_the_independent_span_model() {
    runner(CASES)
        .run(&case_strategy(), |tokens| {
            let expected = expected_spans(&tokens, true);
            let actual = production_spans(&tokens);
            prop_assert_eq!(actual, expected, "generated legacy input: {:?}", input(&tokens));
            Ok(())
        })
        .expect("the production legacy-text parser must match the independent span model");
}

#[test]
fn reset_control_rejects_a_model_that_keeps_the_preceding_colour() {
    let control = vec![
        Token::Text("first".to_owned()),
        Token::Code('c'),
        Token::Text("red".to_owned()),
        Token::Code('r'),
        Token::Text("tail".to_owned()),
    ];
    let failure = runner(1)
        .run(&Just(control.clone()), |tokens| {
            let actual = production_spans(&tokens);
            let intentionally_wrong = expected_spans(&tokens, false);
            prop_assert_eq!(intentionally_wrong, actual, "reset detector control");
            Ok(())
        })
        .expect_err("a reset model that retains its colour must disagree");

    match failure {
        TestError::Fail(_, minimal) => assert_eq!(minimal, control),
        TestError::Abort(reason) => panic!("the reset control must fail, not abort: {reason}"),
    }
}
