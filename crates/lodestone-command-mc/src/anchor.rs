//! `minecraft:entity_anchor` — vanilla's own entity-anchor argument,
//! `/execute anchored <anchor>` and `/execute facing entity <targets>
//! <anchor>`.
//!
//! # Resolution lives with the caller, not here
//!
//! Exactly the split [`crate::position`]'s module doc states for coordinates:
//! this crate produces the parsed value (`feet`/`eyes`), never a position. The
//! eye-height addition (vanilla's own eyes-anchor variant adds the
//! entity's own eye height to its feet position) needs a live entity and belongs in
//! `lodestone_server::commands`, which already carries the position/rotation a
//! `CommandSource` needs.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// Vanilla's own entity-anchor enum — which point on an entity `^`-local
/// coordinates and `facing` resolve from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnchorInput {
    #[default]
    Feet,
    Eyes,
}

/// Vanilla's own entity-anchor argument — `minecraft:entity_anchor`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntityAnchorArg;

impl ArgumentType for EntityAnchorArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let word = reader.read_unquoted_string();
        let anchor = match word.as_str() {
            "feet" => AnchorInput::Feet,
            "eyes" => AnchorInput::Eyes,
            _ => {
                reader.set_cursor(start);
                return Err(refuse(start, format!("invalid anchor '{word}'")));
            }
        };
        Ok(ParsedValue::dynamic(anchor))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        vec!["eyes".to_string(), "feet".to_string()]
    }
}

impl McArg for EntityAnchorArg {
    type Value = AnchorInput;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::EntityAnchor
    }
}

fn refuse(position: usize, message: impl Into<String>) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidBool(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<AnchorInput, ParseError> {
        let mut reader = StringReader::new(text);
        EntityAnchorArg
            .parse(&mut reader)
            .map(|value| *value.downcast_ref::<AnchorInput>().expect("AnchorInput"))
    }

    #[test]
    fn feet_and_eyes_are_the_only_two_valid_names() {
        assert_eq!(parse("feet"), Ok(AnchorInput::Feet));
        assert_eq!(parse("eyes"), Ok(AnchorInput::Eyes));
    }

    #[test]
    fn anything_else_is_refused() {
        assert!(parse("head").is_err());
        let mut reader = StringReader::new("head");
        assert!(EntityAnchorArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn the_wire_identity_is_the_no_payload_entity_anchor_parser() {
        assert_eq!(EntityAnchorArg.wire(), ArgumentParser::EntityAnchor);
    }
}
