//! `minecraft:function` — `FunctionArgument`'s own grammar: a resource
//! location, optionally prefixed with `#` to name a function *tag* instead of
//! a single function (issue #48's remainder — `/function`).
//!
//! # No registry to validate against here either
//!
//! Like [`crate::IdentifierArg`]/[`crate::nbt_path::StorageIdArg`], a function
//! or tag id is resolved against whatever a loaded datapack actually
//! contains — there is no fixed census to check a typed name against at
//! *parse* time. An unknown single function is a *runtime* refusal
//! (`crate::commands::function`'s own module doc, in `lodestone-server`,
//! names the exact asymmetry); an unknown tag is not an error at all,
//! matching vanilla's own `getOrDefault(tag, List.of())`.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// One `/function <name>` argument: a single function, or (with a leading
/// `#`) a function tag naming zero or more of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionRef {
    Single(ResourceKey),
    Tag(ResourceKey),
}

/// `FunctionArgument` — `minecraft:function`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FunctionArg;

impl ArgumentType for FunctionArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let is_tag = reader.peek() == Some('#');
        if is_tag {
            reader.skip();
        }
        let id = read_identifier(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected a function or function tag"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable identifier '{qualified}'")));
        };
        let value = if is_tag { FunctionRef::Tag(key) } else { FunctionRef::Single(key) };
        Ok(ParsedValue::dynamic(value))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        Vec::new()
    }
}

impl McArg for FunctionArg {
    type Value = FunctionRef;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Function
    }
}

/// `Identifier.read`'s character class, plus the leading `#` this argument
/// alone accepts (handled by the caller before this runs) — the same set
/// [`crate::identifier`]/[`crate::nbt_path`] each carry their own copy of.
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

    #[test]
    fn a_bare_identifier_parses_as_a_single_function_defaulted_to_minecraft() {
        let mut reader = StringReader::new("foo");
        let value = FunctionArg.parse(&mut reader).unwrap().downcast_ref::<FunctionRef>().unwrap().clone();
        assert_eq!(value, FunctionRef::Single("minecraft:foo".parse().unwrap()));
    }

    #[test]
    fn a_hash_prefixed_identifier_parses_as_a_tag_with_the_hash_stripped() {
        let mut reader = StringReader::new("#test:cycle");
        let value = FunctionArg.parse(&mut reader).unwrap().downcast_ref::<FunctionRef>().unwrap().clone();
        assert_eq!(value, FunctionRef::Tag("test:cycle".parse().unwrap()));
    }

    #[test]
    fn an_empty_identifier_is_refused_and_rewinds() {
        let mut reader = StringReader::new("#");
        assert!(FunctionArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn the_wire_identity_is_function() {
        assert_eq!(FunctionArg.wire(), ArgumentParser::Function);
    }
}
