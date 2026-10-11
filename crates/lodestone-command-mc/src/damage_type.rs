//! `minecraft:resource` narrowed to the `minecraft:damage_type` registry,
//! `/damage`'s `<damageType>`.
//!
//! Validated at parse time against the generated 26.3 damage-type table
//! (`lodestone_data::damage_types`), so an unknown type is a parse error rather
//! than a refusal after the amount has been read.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_data::damage_types::DamageType;
use lodestone_model::command_tree::ArgumentParser;

use crate::McArg;

/// A damage type argument.
#[derive(Debug, Default, Clone, Copy)]
pub struct DamageTypeArg;

impl ArgumentType for DamageTypeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        while reader.peek().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | ':' | '/' | '.' | '-')) {
            reader.skip();
        }
        let text: String = reader.source().chars().skip(start).take(reader.cursor() - start).collect();
        let Some(ty) = DamageType::from_name(&text) else {
            reader.set_cursor(start);
            return Err(ParseError::new(
                start,
                ParseErrorKind::InvalidArgument(format!("unknown damage type '{text}'")),
            ));
        };
        Ok(ParsedValue::dynamic(ty))
    }
}

impl McArg for DamageTypeArg {
    type Value = DamageType;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Resource {
            registry: "minecraft:damage_type".parse().expect("a static registry key is valid"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Option<DamageType> {
        let mut reader = StringReader::new(text);
        DamageTypeArg.parse(&mut reader).ok().map(|v| *v.downcast_ref::<DamageType>().expect("DamageType"))
    }

    #[test]
    fn bare_and_namespaced_names_resolve_and_unknowns_are_refused() {
        assert_eq!(parse("fall").map(DamageType::name), Some("fall"));
        assert_eq!(parse("minecraft:magic").map(DamageType::name), Some("magic"));
        assert!(parse("nonsense").is_none());
    }
}
