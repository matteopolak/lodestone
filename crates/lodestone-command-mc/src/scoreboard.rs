//! Scoreboard argument types — `minecraft:objective`, `minecraft:
//! objective_criteria`, `minecraft:operation`, `minecraft:int_range` and
//! `minecraft:score_holder`, the five wire parsers `/scoreboard` and
//! `/execute … score` need. [`lodestone_model::command_tree::ArgumentParser`]
//! already modelled all five (built ahead of a caller, for the captured
//! vanilla tree's own decode); this module is their first production use.
//!
//! # What each one is, minimally
//!
//! * [`ObjectiveArg`] — a bare word naming an objective. Not validated
//!   against a live scoreboard here (this crate has no world access at all —
//!   see the crate doc's "grammar here, resolution there" split); an unknown
//!   name is the executor's problem, exactly like `/gamerule`'s rule name.
//! * [`ObjectiveCriteriaArg`] — a bare token for the criteria name
//!   (`dummy`, `health`, `minecraft.custom:minecraft.deaths`, …). No
//!   criteria *semantics* are modelled anywhere in this server — every score
//!   is set by a command, never incremented automatically — so this stores
//!   whatever text was typed and nothing reads it back for meaning.
//! * [`OperationArg`] — one of vanilla's nine operation tokens,
//!   for `/scoreboard players operation`.
//! * [`IntRangeArg`] — `minecraft:int_range`, for `/execute if score …
//!   matches <range>`. Reuses [`crate::Bounds`]'s shape (an inclusive range
//!   with either end optional) with its own reader, since
//!   `entity::read_bounds_f64`'s home module is `f64`-specific and this
//!   needs `i32`.
//! * [`ScoreHolderArg`] — `minecraft:score_holder`. A holder is `*` (every
//!   name this server has ever recorded a score for), an entity selector
//!   (resolved server-side against the player roster, exactly like
//!   [`crate::EntityArg`]), or a bare word (a "fake player" — a counter name
//!   with no corresponding entity at all, which is the dominant real use of
//!   a scoreboard in redstone/adventure-map contexts and the reason this is
//!   not simply [`crate::EntityArg`] reused).

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::entity::parse_selector;
use crate::{EntityArg, EntitySelector, McArg};

/// `minecraft:objective` — a bare word.
#[derive(Debug, Clone, Copy, Default)]
pub struct ObjectiveArg;

impl ArgumentType for ObjectiveArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let word = reader.read_unquoted_string();
        if word.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::InvalidInt(String::new())));
        }
        Ok(ParsedValue::dynamic(word))
    }
}

impl McArg for ObjectiveArg {
    type Value = String;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Objective
    }
}

/// `minecraft:objective_criteria` — a dotted/colon-separated token
/// (vanilla's own invalid-name pattern reads up to a `[ \n]`, i.e. one
/// argument-length token, more permissive than an unquoted word).
#[derive(Debug, Clone, Copy, Default)]
pub struct ObjectiveCriteriaArg;

impl ArgumentType for ObjectiveCriteriaArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let text_start = reader.cursor();
        while reader.can_read() && reader.peek().is_some_and(|c| !c.is_whitespace()) {
            reader.skip();
        }
        let text: String =
            reader.source().chars().skip(text_start).take(reader.cursor() - text_start).collect();
        if text.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::InvalidInt(String::new())));
        }
        Ok(ParsedValue::dynamic(text))
    }
}

impl McArg for ObjectiveCriteriaArg {
    type Value = String;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::ObjectiveCriteria
    }
}

/// Vanilla's own operation tokens — `/scoreboard players operation`'s nine tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreOperation {
    /// `=`
    Assign,
    /// `+=`
    Add,
    /// `-=`
    Subtract,
    /// `*=`
    Multiply,
    /// `/=`
    Divide,
    /// `%=`
    Modulo,
    /// `<` — set target to the lesser of the two.
    Min,
    /// `>` — set target to the greater of the two.
    Max,
    /// `><` — swap the two scores.
    Swap,
}

impl ScoreOperation {
    const TOKENS: &'static [(&'static str, Self)] = &[
        ("><", Self::Swap),
        ("=", Self::Assign),
        ("+=", Self::Add),
        ("-=", Self::Subtract),
        ("*=", Self::Multiply),
        ("/=", Self::Divide),
        ("%=", Self::Modulo),
        ("<", Self::Min),
        (">", Self::Max),
    ];
}

/// `minecraft:operation`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OperationArg;

impl ArgumentType for OperationArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let token_start = reader.cursor();
        while reader.can_read() && reader.peek().is_some_and(|c| !c.is_whitespace()) {
            reader.skip();
        }
        let token: String =
            reader.source().chars().skip(token_start).take(reader.cursor() - token_start).collect();
        // Longest-match-first: `><` must not be read as `>` with `<` left
        // over, which `TOKENS`' own declaration order guarantees since it is
        // matched as one whole token rather than a prefix.
        match ScoreOperation::TOKENS.iter().find(|(text, _)| *text == token) {
            Some((_, op)) => Ok(ParsedValue::dynamic(*op)),
            None => {
                reader.set_cursor(start);
                Err(ParseError::new(start, ParseErrorKind::InvalidBool(format!("invalid operation '{token}'"))))
            }
        }
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        ScoreOperation::TOKENS.iter().map(|(text, _)| (*text).to_string()).collect()
    }
}

impl McArg for OperationArg {
    type Value = ScoreOperation;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Operation
    }
}

/// Vanilla's own int-bounds shape — an inclusive `i32` range with either end optional.
/// `5` is `min == max == Some(5)`; `1..3`, `1..`, `..3` are the other three
/// shapes; both ends absent is a parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntRange {
    pub min: Option<i32>,
    pub max: Option<i32>,
}

impl IntRange {
    /// Whether `value` falls within both present ends.
    #[must_use]
    pub fn matches(&self, value: i32) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

/// `minecraft:int_range`.
#[derive(Debug, Clone, Copy, Default)]
pub struct IntRangeArg;

impl ArgumentType for IntRangeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        if !reader.can_read() {
            return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
        }
        let min_text = read_range_number(reader);
        let is_double_dot = reader.can_read_n(2)
            && reader.peek() == Some('.')
            && peek_at(reader, 1) == Some('.');
        let max_text = if is_double_dot {
            reader.skip();
            reader.skip();
            read_range_number(reader)
        } else {
            min_text.clone()
        };
        if min_text.is_none() && max_text.is_none() {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
        }
        let parse = |text: Option<String>| -> Result<Option<i32>, ParseError> {
            match text {
                None => Ok(None),
                Some(text) => text
                    .parse::<i32>()
                    .map(Some)
                    .map_err(|_| ParseError::new(start, ParseErrorKind::InvalidInt(text))),
            }
        };
        let range = IntRange { min: parse(min_text)?, max: parse(max_text)? };
        if let (Some(min), Some(max)) = (range.min, range.max) {
            if min > max {
                reader.set_cursor(start);
                return Err(ParseError::new(start, ParseErrorKind::IntegerTooLow { found: max, min }));
            }
        }
        Ok(ParsedValue::dynamic(range))
    }
}

impl McArg for IntRangeArg {
    type Value = IntRange;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::IntRange
    }
}

/// One end of a range: the run of `[0-9-]` plus any `.` that is not the first
/// character of a `..`, same rule as `entity::read_bounds_number` for `f64`.
fn read_range_number(reader: &mut StringReader) -> Option<String> {
    let start = reader.cursor();
    while reader.can_read() {
        match reader.peek() {
            Some(c) if c.is_ascii_digit() || c == '-' => reader.skip(),
            _ => break,
        }
    }
    if reader.cursor() == start {
        return None;
    }
    let source = reader.source();
    Some(source.chars().skip(start).take(reader.cursor() - start).collect())
}

fn peek_at(reader: &StringReader, offset: usize) -> Option<char> {
    let index = reader.cursor() + offset;
    reader.source().chars().nth(index)
}

/// One `minecraft:score_holder` value, before resolution — resolving `All`
/// and `Selector` against a live roster needs a world, so (as with
/// [`crate::EntitySelector`]) that step happens server-side.
#[derive(Debug, Clone, PartialEq)]
pub enum ScoreHolderInput {
    /// `*` — every holder this server has ever recorded a score for.
    All,
    /// A bare word: a real player's username, typed literally, or a "fake
    /// player" counter name with no corresponding entity at all.
    Name(String),
    /// `@a`, `@s`, … — resolved against the player roster like any other
    /// selector.
    Selector(EntitySelector),
}

/// `minecraft:score_holder`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreHolderArg {
    /// Whether more than one holder may result — `Info`'s own `multiple` bit.
    pub multiple: bool,
}

impl ScoreHolderArg {
    #[must_use]
    pub const fn single() -> Self {
        Self { multiple: false }
    }

    #[must_use]
    pub const fn multiple() -> Self {
        Self { multiple: true }
    }
}

impl ArgumentType for ScoreHolderArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        if reader.peek() == Some('@') {
            let arg = EntityArg { single: !self.multiple, players_only: true };
            return parse_selector(reader, arg)
                .map(|selector| ParsedValue::dynamic(ScoreHolderInput::Selector(selector)));
        }
        // `*` is not in `is_allowed_in_unquoted_string`'s set (vanilla's own
        // unquoted-string character class excludes it too), so
        // vanilla's own score-holder argument checks for it explicitly before
        // falling back to a plain word — matched here the same way.
        if reader.peek() == Some('*') {
            reader.skip();
            return Ok(ParsedValue::dynamic(ScoreHolderInput::All));
        }
        let word = reader.read_unquoted_string();
        if word.is_empty() {
            return Err(ParseError::new(start, ParseErrorKind::InvalidInt(String::new())));
        }
        Ok(ParsedValue::dynamic(ScoreHolderInput::Name(word)))
    }
}

impl McArg for ScoreHolderArg {
    type Value = ScoreHolderInput;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::ScoreHolder { multiple: self.multiple }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(text: &str) -> Result<IntRange, ParseError> {
        let mut reader = StringReader::new(text);
        IntRangeArg.parse(&mut reader).map(|v| *v.downcast_ref::<IntRange>().unwrap())
    }

    #[test]
    fn a_bare_integer_is_both_ends() {
        assert_eq!(range("5"), Ok(IntRange { min: Some(5), max: Some(5) }));
    }

    #[test]
    fn open_ended_ranges_parse_one_side() {
        assert_eq!(range("5.."), Ok(IntRange { min: Some(5), max: None }));
        assert_eq!(range("..5"), Ok(IntRange { min: None, max: Some(5) }));
        assert_eq!(range("1..3"), Ok(IntRange { min: Some(1), max: Some(3) }));
    }

    #[test]
    fn a_swapped_range_is_refused() {
        assert!(range("5..1").is_err());
    }

    #[test]
    fn matches_is_inclusive_on_both_present_ends() {
        let r = IntRange { min: Some(2), max: Some(4) };
        assert!(!r.matches(1));
        assert!(r.matches(2));
        assert!(r.matches(3));
        assert!(r.matches(4));
        assert!(!r.matches(5));
    }

    /// Longer tokens are matched whole, not as a shorter token plus leftover
    /// input — `><` must not read as `>` with a dangling `<`.
    #[test]
    fn operation_tokens_match_whole_not_as_a_prefix() {
        let mut reader = StringReader::new("><");
        let value = OperationArg.parse(&mut reader).unwrap();
        assert_eq!(*value.downcast_ref::<ScoreOperation>().unwrap(), ScoreOperation::Swap);
        assert_eq!(reader.cursor(), 2, "the whole two-character token was consumed");
    }

    #[test]
    fn every_operation_token_round_trips() {
        for (text, op) in ScoreOperation::TOKENS {
            let mut reader = StringReader::new(text);
            let value = OperationArg.parse(&mut reader).unwrap();
            assert_eq!(value.downcast_ref::<ScoreOperation>().unwrap(), op);
        }
    }

    #[test]
    fn a_star_holder_is_all_and_a_word_is_a_name() {
        let mut reader = StringReader::new("*");
        let v = ScoreHolderArg::multiple().parse(&mut reader).unwrap();
        assert_eq!(*v.downcast_ref::<ScoreHolderInput>().unwrap(), ScoreHolderInput::All);

        let mut reader = StringReader::new("COUNTER");
        let v = ScoreHolderArg::single().parse(&mut reader).unwrap();
        assert_eq!(
            *v.downcast_ref::<ScoreHolderInput>().unwrap(),
            ScoreHolderInput::Name("COUNTER".to_string())
        );
    }

    #[test]
    fn a_selector_holder_parses_as_a_selector() {
        let mut reader = StringReader::new("@a");
        let v = ScoreHolderArg::multiple().parse(&mut reader).unwrap();
        assert!(matches!(
            v.downcast_ref::<ScoreHolderInput>().unwrap(),
            ScoreHolderInput::Selector(_)
        ));
    }
}
