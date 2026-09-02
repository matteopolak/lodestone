//! `minecraft:swizzle` — vanilla's own swizzle argument, `/execute align <axes>`.
//!
//! # An unquoted run of `x`/`y`/`z`, each at most once, order irrelevant
//!
//! Vanilla's own swizzle-argument parser reads characters up to the next space, mapping each
//! to an axis and rejecting a repeat; the *set* of axes is all that matters
//! downstream (vanilla's own axis-alignment routine), so this stores three flags rather than the
//! original character order. An **empty** swizzle (`align ` immediately
//! followed by whatever comes next) is legal — vanilla's loop simply never
//! executes and returns the empty set — so this does not require at least one
//! axis either.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// Which axes `/execute align` floors — vanilla's own axis enum set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Axes {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

/// Vanilla's own swizzle argument — `minecraft:swizzle`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SwizzleArg;

impl ArgumentType for SwizzleArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let mut axes = Axes::default();
        while reader.can_read() && reader.peek() != Some(' ') {
            let position = reader.cursor();
            let c = reader.read().expect("can_read() just checked");
            let duplicate = match c {
                'x' => std::mem::replace(&mut axes.x, true),
                'y' => std::mem::replace(&mut axes.y, true),
                'z' => std::mem::replace(&mut axes.z, true),
                _ => {
                    reader.set_cursor(start);
                    return Err(refuse(position, format!("invalid swizzle character '{c}'")));
                }
            };
            if duplicate {
                reader.set_cursor(start);
                return Err(refuse(position, format!("axis '{c}' repeated in swizzle")));
            }
        }
        Ok(ParsedValue::dynamic(axes))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        // `SwizzleArgument` has no `listSuggestions` override — vanilla itself
        // offers zero completions for this parser (see `ArgumentParser::Swizzle`'s
        // own doc comment in `lodestone-model`).
        Vec::new()
    }
}

impl McArg for SwizzleArg {
    type Value = Axes;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Swizzle
    }
}

fn refuse(position: usize, message: impl Into<String>) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidBool(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axes(text: &str) -> Axes {
        let mut reader = StringReader::new(text);
        let value = SwizzleArg.parse(&mut reader).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        *value.downcast_ref::<Axes>().expect("SwizzleArg produces Axes")
    }

    #[test]
    fn every_axis_letter_sets_its_own_flag() {
        assert_eq!(axes("xyz"), Axes { x: true, y: true, z: true });
        assert_eq!(axes("y"), Axes { x: false, y: true, z: false });
        assert_eq!(axes("xz"), Axes { x: true, y: false, z: true });
    }

    #[test]
    fn order_does_not_matter() {
        assert_eq!(axes("zx"), axes("xz"));
    }

    #[test]
    fn a_repeated_axis_is_refused() {
        let mut reader = StringReader::new("xx");
        assert!(SwizzleArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn an_unknown_letter_is_refused() {
        let mut reader = StringReader::new("xw");
        assert!(SwizzleArg.parse(&mut reader).is_err());
    }

    #[test]
    fn an_empty_swizzle_is_legal() {
        assert_eq!(axes(""), Axes::default());
    }

    #[test]
    fn the_wire_identity_is_the_no_payload_swizzle_parser() {
        assert_eq!(SwizzleArg.wire(), ArgumentParser::Swizzle);
    }
}
