//! `minecraft:resource` narrowed to the `minecraft:entity_type` registry —
//! `ResourceArgument.resource(ctx, Registries.ENTITY_TYPE)`, `/summon`'s
//! `<entity>`.
//!
//! # Validated at parse time, against the real 26.2 census
//!
//! Same posture as [`crate::BlockArg`]/[`crate::ItemArg`]:
//! `lodestone_data::entity_types::entity_type_id` is the generated table from
//! Mojang's own `registries.json` for protocol 776 (issue #343's canonical
//! version), so `/summon minecraft:not_a_mob` is a parse error rather than a
//! runtime refusal three nodes later.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// A validated entity-type id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityTypeInput {
    /// The canonical, namespace-qualified entity-type id (`minecraft:zombie`).
    pub entity_type: ResourceKey,
}

/// `ResourceArgument.resource(.., Registries.ENTITY_TYPE)` — `minecraft:resource`.
#[derive(Debug, Default, Clone, Copy)]
pub struct EntityTypeArg;

impl ArgumentType for EntityTypeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let id = read_entity_type_id(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected an entity type"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        if lodestone_data::entity_types::entity_type_id(&qualified).is_none() {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unknown entity type '{qualified}'")));
        }
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable entity type id '{qualified}'")));
        };
        Ok(ParsedValue::dynamic(EntityTypeInput { entity_type: key }))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        Vec::new()
    }
}

impl McArg for EntityTypeArg {
    type Value = EntityTypeInput;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Resource {
            registry: "minecraft:entity_type".parse().expect("a static registry key is valid"),
        }
    }
}

/// `Identifier.read`'s character class, for an entity-type id — the same set
/// [`crate::block::BlockArg`]'s own reader accepts.
fn read_entity_type_id(reader: &mut StringReader) -> String {
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

    fn parse(text: &str) -> Result<EntityTypeInput, ParseError> {
        let mut reader = StringReader::new(text);
        EntityTypeArg
            .parse(&mut reader)
            .map(|value| value.downcast_ref::<EntityTypeInput>().expect("EntityTypeInput").clone())
    }

    #[test]
    fn a_bare_path_resolves_the_default_namespace_and_validates_against_the_census() {
        assert_eq!(
            parse("zombie").expect("zombie is real"),
            EntityTypeInput { entity_type: "minecraft:zombie".parse().unwrap() }
        );
        assert_eq!(
            parse("minecraft:cow").expect("cow is real"),
            EntityTypeInput { entity_type: "minecraft:cow".parse().unwrap() }
        );
    }

    #[test]
    fn an_unknown_entity_type_is_a_parse_error() {
        assert!(parse("not_a_real_mob").is_err());
    }

    #[test]
    fn the_wire_identity_names_the_entity_type_registry() {
        assert_eq!(
            EntityTypeArg.wire(),
            ArgumentParser::Resource { registry: "minecraft:entity_type".parse().unwrap() }
        );
    }
}
