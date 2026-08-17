//! `minecraft:float_range` — `RangeArgument.floatRange()`, `/execute
//! if`/`unless stopwatch <id> <range>`'s own argument (issue #48's
//! remainder). The `f64` twin of [`crate::IntRangeArg`]; see that module's
//! doc for the four-shape grammar (`5`, `1..3`, `1..`, `..3`) this mirrors
//! exactly, substituting a float parse for the integer one.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// `MinMaxBounds.Doubles` — an inclusive `f64` range with either end optional.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FloatRange {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl FloatRange {
    /// Whether `value` falls within both present ends.
    #[must_use]
    pub fn matches(&self, value: f64) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

/// `minecraft:float_range`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FloatRangeArg;

impl ArgumentType for FloatRangeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        if !reader.can_read() {
            return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
        }
        let min_text = read_range_number(reader);
        let is_double_dot =
            reader.can_read_n(2) && reader.peek() == Some('.') && peek_at(reader, 1) == Some('.');
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
        let parse = |text: Option<String>| -> Result<Option<f64>, ParseError> {
            match text {
                None => Ok(None),
                Some(text) => text
                    .parse::<f64>()
                    .map(Some)
                    .map_err(|_| ParseError::new(start, ParseErrorKind::InvalidInt(text))),
            }
        };
        let range = FloatRange { min: parse(min_text)?, max: parse(max_text)? };
        if let (Some(min), Some(max)) = (range.min, range.max) {
            if min > max {
                reader.set_cursor(start);
                #[allow(clippy::cast_possible_truncation)]
                return Err(ParseError::new(
                    start,
                    ParseErrorKind::IntegerTooLow { found: max as i32, min: min as i32 },
                ));
            }
        }
        Ok(ParsedValue::dynamic(range))
    }
}

impl McArg for FloatRangeArg {
    type Value = FloatRange;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::FloatRange
    }
}

/// One end of a range: the run of `[0-9.-]` with at most one `.` that is not
/// the first character of a `..` — [`crate::scoreboard::IntRangeArg`]'s own
/// `read_range_number`, widened to accept a decimal point.
fn read_range_number(reader: &mut StringReader) -> Option<String> {
    let start = reader.cursor();
    let mut seen_dot = false;
    while reader.can_read() {
        match reader.peek() {
            Some(c) if c.is_ascii_digit() || c == '-' => reader.skip(),
            Some('.') if !seen_dot && peek_at(reader, 1) != Some('.') => {
                seen_dot = true;
                reader.skip();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<FloatRange, ParseError> {
        let mut reader = StringReader::new(text);
        FloatRangeArg.parse(&mut reader).map(|value| *value.downcast_ref::<FloatRange>().expect("FloatRange"))
    }

    #[test]
    fn an_exact_value_sets_both_ends() {
        assert_eq!(parse("2.5").unwrap(), FloatRange { min: Some(2.5), max: Some(2.5) });
    }

    #[test]
    fn the_four_range_shapes_all_parse() {
        assert_eq!(parse("1.0..3.0").unwrap(), FloatRange { min: Some(1.0), max: Some(3.0) });
        assert_eq!(parse("1.0..").unwrap(), FloatRange { min: Some(1.0), max: None });
        assert_eq!(parse("..3.0").unwrap(), FloatRange { min: None, max: Some(3.0) });
    }

    #[test]
    fn matches_checks_both_present_ends() {
        let range = FloatRange { min: Some(1.0), max: Some(3.0) };
        assert!(range.matches(2.0));
        assert!(!range.matches(0.5));
        assert!(!range.matches(3.5));
    }

    #[test]
    fn an_inverted_range_is_a_parse_error() {
        assert!(parse("5.0..1.0").is_err());
    }

    #[test]
    fn the_wire_identity_names_the_float_range_parser() {
        assert_eq!(FloatRangeArg.wire(), ArgumentParser::FloatRange);
    }

    #[test]
    fn a_failed_parse_rewinds_the_cursor() {
        let mut reader = StringReader::new("");
        assert!(FloatRangeArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0);
    }
}
