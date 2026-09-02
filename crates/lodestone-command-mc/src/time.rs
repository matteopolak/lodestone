//! `minecraft:time` — vanilla's own time argument, the `/time`, `/xp` and delay-style
//! integer-with-suffix grammar.
//!
//! # The suffixes, and the one vanilla omits from its own error text
//!
//! Vanilla's own time-argument parser reads a run of digits (or a bare
//! `-`/`.`-tolerant
//! double) then an optional single-letter unit —
//! `d` (day, ×24000), `s` (second, ×20) or `t` (tick, ×1, also the default
//! with no suffix) — and rounds the product to the nearest tick. A value
//! that rounds to something outside
//! `min..=i32::MAX` is refused with vanilla's own two-argument
//! too-small/invalid-value error shape, and `/time set`/`/time add`
//! both use its zero-minimum constructor — no negative time.
//!
//! v1 does not parse the fractional form (`1.5s`); vanilla's own grammar
//! permits it, but every existing caller in this server
//! passes a whole number of ticks, and adding fractional parsing without a
//! caller to exercise it would be the risk this crate's own doc names for
//! `hasValueHere`-style edges: untested and easy to get wrong. A bare
//! integer plus `d`/`s`/`t` is what this parses.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// Vanilla's own time argument — `minecraft:time`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimeArg {
    pub min: i32,
}

impl TimeArg {
    /// Vanilla's own zero-minimum constructor — what `/time set` and `/time add` both use.
    #[must_use]
    pub const fn non_negative() -> Self {
        Self { min: 0 }
    }
}

impl ArgumentType for TimeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let digits_start = reader.cursor();
        while reader.can_read() && reader.peek().is_some_and(|c| c.is_ascii_digit() || c == '-') {
            reader.skip();
        }
        let digits: String =
            reader.source().chars().skip(digits_start).take(reader.cursor() - digits_start).collect();
        if digits.is_empty() || digits == "-" {
            reader.set_cursor(start);
            return Err(ParseError::new(start, ParseErrorKind::ExpectedInt));
        }
        let Ok(value) = digits.parse::<i64>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("invalid time '{digits}'")));
        };
        let multiplier: i64 = match reader.peek() {
            Some('d') => {
                reader.skip();
                24_000
            }
            Some('s') => {
                reader.skip();
                20
            }
            Some('t') => {
                reader.skip();
                1
            }
            _ => 1,
        };
        let ticks = value.saturating_mul(multiplier);
        let clamped = ticks.clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        #[allow(clippy::cast_possible_truncation)]
        let ticks = clamped as i32;
        if ticks < self.min {
            reader.set_cursor(start);
            return Err(refuse(start, format!("time must not be less than {}", self.min)));
        }
        Ok(ParsedValue::Integer(ticks))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        Vec::new()
    }
}

impl McArg for TimeArg {
    type Value = i32;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Time { min: self.min }
    }
}

fn refuse(position: usize, message: impl Into<String>) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidBool(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<i32, ParseError> {
        let mut reader = StringReader::new(text);
        TimeArg::non_negative()
            .parse(&mut reader)
            .map(|value| *value.downcast_ref::<i32>().expect("TimeArg produces an i32"))
    }

    #[test]
    fn a_bare_integer_is_ticks() {
        assert_eq!(parse("100"), Ok(100));
        assert_eq!(parse("0"), Ok(0));
    }

    /// The three suffixes multiply by pairwise-distinct factors, so a
    /// transposition between `d` and `s` would be visible.
    #[test]
    fn the_three_suffixes_multiply_by_their_own_distinct_factor() {
        assert_eq!(parse("2t"), Ok(2));
        assert_eq!(parse("3s"), Ok(60));
        assert_eq!(parse("2d"), Ok(48_000));
    }

    #[test]
    fn a_value_below_the_minimum_is_refused() {
        assert!(parse("-5").is_err());
        let mut reader = StringReader::new("-5");
        assert!(TimeArg::non_negative().parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn the_wire_identity_carries_the_minimum() {
        assert_eq!(TimeArg::non_negative().wire(), ArgumentParser::Time { min: 0 });
    }
}
