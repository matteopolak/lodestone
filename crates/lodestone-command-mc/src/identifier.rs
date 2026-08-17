//! `minecraft:resource_location` — `IdentifierArgument.id()`, `/stopwatch`'s
//! `<id>` and `/execute … stopwatch <id>`'s own argument (issue #48's
//! remainder).
//!
//! # No registry to validate against, unlike [`crate::BiomeArg`]/[`crate::BlockArg`]
//!
//! A stopwatch id is a user-chosen name (`StopwatchCommand.createStopwatch`
//! stores whatever the caller typed), not a lookup into a fixed census — the
//! same reason [`crate::nbt_path::StorageIdArg`], which already carries this
//! wire identity, accepts any well-formed identifier too. This module exists
//! rather than reusing that one because `StorageIdArg`'s own doc scopes it to
//! `/data storage`'s target and this crate keeps one argument type per
//! vanilla argument class rather than reusing a same-shaped type across
//! unrelated commands.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// `IdentifierArgument.id()` — `minecraft:resource_location`.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentifierArg;

impl ArgumentType for IdentifierArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let id = read_identifier(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected an identifier"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable identifier '{qualified}'")));
        };
        Ok(ParsedValue::dynamic(key))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        Vec::new()
    }
}

impl McArg for IdentifierArg {
    type Value = ResourceKey;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::ResourceLocation
    }
}

/// `Identifier.read`'s character class — the same set every other
/// resource-shaped argument in this crate accepts.
fn read_identifier(reader: &mut StringReader) -> String {
    let start = reader.cursor();
    while reader.can_read() {
        match reader.peek() {
            Some(c)
                if c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '_' | ':' | '/' | '.' | '-') =>
            {
                reader.skip();
            }
            _ => break,
        }
    }
    reader.source().chars().skip(start).take(reader.cursor() - start).collect()
}

fn refuse(position: usize, message: impl Into<String>) -> ParseError {
    ParseError::new(position, ParseErrorKind::InvalidBool(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ResourceKey, ParseError> {
        let mut reader = StringReader::new(text);
        IdentifierArg.parse(&mut reader).map(|value| value.downcast_ref::<ResourceKey>().expect("ResourceKey").clone())
    }

    #[test]
    fn a_bare_path_resolves_the_default_namespace() {
        assert_eq!(parse("my_timer").unwrap(), "minecraft:my_timer".parse().unwrap());
        assert_eq!(parse("ns:my_timer").unwrap(), "ns:my_timer".parse().unwrap());
    }

    #[test]
    fn an_empty_input_is_a_parse_error() {
        let mut reader = StringReader::new("");
        assert!(IdentifierArg.parse(&mut reader).is_err());
    }

    #[test]
    fn the_wire_identity_carries_no_payload() {
        assert_eq!(IdentifierArg.wire(), ArgumentParser::ResourceLocation);
    }

    #[test]
    fn a_failed_parse_rewinds_the_cursor() {
        let mut reader = StringReader::new("");
        assert!(IdentifierArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0);
    }
}
