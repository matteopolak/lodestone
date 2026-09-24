//! `minecraft:resource` narrowed to the `minecraft:worldgen/biome` registry —
//! vanilla's own resource-or-tag argument narrowed to the biome registry,
//! `/execute if`/`unless biome <pos> <biome>`.
//!
//! # v1 ships without tag support, for the same reason [`crate::BlockArg`] does
//!
//! Vanilla's argument accepts a tag (`#minecraft:is_forest`) as well as a bare
//! id. This parses a bare id only — a single-biome filter is exact rather
//! than a superset, the same safe-direction-to-be-wrong-in reduction
//! `crate::block`/`crate::item`'s own docs give.
//!
//! # Validated at parse time, against the real 26.2 census
//!
//! Same posture as [`crate::BlockArg`]/[`crate::EntityTypeArg`]:
//! [`lodestone_data::biomes::is_biome`] is generated from the 66
//! `data/minecraft/worldgen/biome/*.json` files 26.2 ships as its own base
//! data (see that module's own doc for why this one census needs no
//! network-id table alongside it), so `/execute if biome ~ ~ ~ not_a_biome`
//! is a parse error rather than a runtime refusal three nodes later.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// A validated biome id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomeInput {
    /// The canonical, namespace-qualified biome id (`minecraft:plains`).
    pub biome: ResourceKey,
}

/// Vanilla's own resource-or-tag argument narrowed to the biome registry, narrowed to
/// its bare-id case — `minecraft:resource`.
#[derive(Debug, Default, Clone, Copy)]
pub struct BiomeArg;

impl ArgumentType for BiomeArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let id = read_biome_id(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected a biome"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        if !lodestone_data::biomes::is_biome(&qualified) {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unknown biome '{qualified}'")));
        }
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable biome id '{qualified}'")));
        };
        Ok(ParsedValue::dynamic(BiomeInput { biome: key }))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        lodestone_data::biomes::all().collect()
    }
}

impl McArg for BiomeArg {
    type Value = BiomeInput;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::Resource {
            registry: "minecraft:worldgen/biome".parse().expect("a static registry key is valid"),
        }
    }
}

/// Vanilla's own resource-location reader's character class — the same set every other
/// resource-shaped argument in this crate accepts.
fn read_biome_id(reader: &mut StringReader) -> String {
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

    fn parse(text: &str) -> Result<BiomeInput, ParseError> {
        let mut reader = StringReader::new(text);
        BiomeArg.parse(&mut reader).map(|value| value.downcast_ref::<BiomeInput>().expect("BiomeInput").clone())
    }

    #[test]
    fn a_bare_biome_resolves_the_default_namespace_and_validates_against_the_census() {
        assert_eq!(parse("plains").unwrap(), BiomeInput { biome: "minecraft:plains".parse().unwrap() });
        assert_eq!(
            parse("minecraft:warped_forest").unwrap(),
            BiomeInput { biome: "minecraft:warped_forest".parse().unwrap() }
        );
    }

    #[test]
    fn an_unknown_biome_is_a_parse_error_not_a_runtime_refusal() {
        let mut reader = StringReader::new("not_a_real_biome");
        assert!(BiomeArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0, "a failed parse rewinds");
    }

    #[test]
    fn the_wire_identity_names_the_biome_registry() {
        assert_eq!(
            BiomeArg.wire(),
            ArgumentParser::Resource { registry: "minecraft:worldgen/biome".parse().unwrap() }
        );
    }
}
