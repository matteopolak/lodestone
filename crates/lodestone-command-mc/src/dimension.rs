//! `minecraft:dimension` — `DimensionArgument.dimension()`, `/execute in
//! <dimension>`.
//!
//! # Validated at parse time, against the dimensions this server actually hosts
//!
//! `DimensionArgument.parse` reads a resource location and then checks it
//! against `context.getSource().levelKeys()` — the *registered* level keys, not
//! merely well-formed syntax — refusing with `ERROR_INVALID_VALUE` for anything
//! else (`DimensionArgument.java`). This crate hosts exactly one dimension
//! (`crates/lodestone-server/src/commands/mod.rs`'s `overworld_dimension`), so
//! [`HOSTED_DIMENSIONS`] is a one-entry census rather than a real registry
//! lookup — the same posture [`crate::BlockArg`]/[`crate::EntityTypeArg`] take
//! against their own real censuses, just a smaller one. **Widen this list
//! rather than removing the validation** the day a second dimension is hosted;
//! refusing an unregistered dimension at parse time is the vanilla behaviour,
//! not a shortcut this crate is taking around it.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// Every dimension this server can currently resolve `<dimension>` against.
/// See this module's own doc for why widening this list, rather than deleting
/// the check, is the correct response to a second dimension landing.
pub const HOSTED_DIMENSIONS: &[&str] = &["minecraft:overworld"];

/// `DimensionArgument.dimension()` — `minecraft:dimension`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DimensionArg;

impl ArgumentType for DimensionArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let id = read_resource_location(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected a dimension"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        if !HOSTED_DIMENSIONS.contains(&qualified.as_str()) {
            reader.set_cursor(start);
            return Err(refuse(start, format!("Unknown dimension '{qualified}'")));
        }
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable dimension id '{qualified}'")));
        };
        Ok(ParsedValue::dynamic(key))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        HOSTED_DIMENSIONS.iter().map(|s| (*s).to_string()).collect()
    }
}

impl McArg for DimensionArg {
    type Value = ResourceKey;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Dimension
    }
}

/// `Identifier.read`'s character class — the same set
/// [`crate::entity_type::EntityTypeArg`]'s own reader accepts.
fn read_resource_location(reader: &mut StringReader) -> String {
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
        DimensionArg
            .parse(&mut reader)
            .map(|value| value.downcast_ref::<ResourceKey>().expect("ResourceKey").clone())
    }

    #[test]
    fn the_hosted_dimension_parses() {
        assert_eq!(parse("minecraft:overworld").unwrap(), "minecraft:overworld".parse().unwrap());
        // A bare path resolves the default namespace, same as every other
        // resource-shaped argument in this crate.
        assert_eq!(parse("overworld").unwrap(), "minecraft:overworld".parse().unwrap());
    }

    #[test]
    fn an_unhosted_dimension_is_a_parse_error_not_a_runtime_refusal() {
        let mut reader = StringReader::new("minecraft:the_nether");
        assert!(DimensionArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn the_wire_identity_names_the_dimension_parser() {
        assert_eq!(DimensionArg.wire(), ArgumentParser::Dimension);
    }
}
