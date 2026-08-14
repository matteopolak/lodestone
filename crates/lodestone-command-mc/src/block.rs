//! `minecraft:block_state` — `BlockStateArgument`, v1.
//!
//! # v1 ships without the property list, for the same reason [`crate::item`] does
//!
//! Vanilla's `BlockStateParser` accepts `minecraft:furnace[facing=north]` —
//! properties in brackets, resolved against that block's own state table. This
//! parses the block id only and refuses a `[` explicitly, exactly as
//! [`crate::ItemArg`] refuses a component patch: `minecraft:block_state` carries
//! **no** network payload (`SingletonArgumentInfo`), so the wire node is complete
//! now and a property grammar is additive later, not a redesign.
//!
//! # The id is validated at parse time
//!
//! Against `lodestone_data::block::Block::from_name`, so `/setblock ~ ~ ~ ded_
//! furnace` fails as a parse error rather than reaching an executor.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// A validated block id — [`crate::item::ItemInput`]'s counterpart for blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockInput {
    /// The canonical, namespace-qualified block id, at its **default** state:
    /// v1 carries no properties, so every block this produces is placed the way
    /// `Block::default_state` describes it.
    pub block: ResourceKey,
}

/// `BlockStateArgument.block()` — `minecraft:block_state`.
#[derive(Debug, Default, Clone, Copy)]
pub struct BlockArg;

impl ArgumentType for BlockArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let id = read_block_id(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected a block"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        if lodestone_data::block::Block::from_name(&qualified).is_none() {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unknown block '{qualified}'")));
        }
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable block id '{qualified}'")));
        };
        if reader.peek() == Some('[') {
            let position = reader.cursor();
            reader.set_cursor(start);
            return Err(refuse(
                position,
                "block states are not supported yet — give the block without '[...]'",
            ));
        }
        Ok(ParsedValue::dynamic(BlockInput { block: key }))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        lodestone_data::block::Block::all()
            .map(|block| block.name().to_string())
            .collect()
    }
}

impl McArg for BlockArg {
    type Value = BlockInput;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::BlockState
    }
}

/// `Identifier.read`'s character class, for a block id.
fn read_block_id(reader: &mut StringReader) -> String {
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

    fn parse(text: &str) -> Result<BlockInput, ParseError> {
        let mut reader = StringReader::new(text);
        BlockArg
            .parse(&mut reader)
            .map(|value| value.downcast_ref::<BlockInput>().expect("BlockArg produces a BlockInput").clone())
    }

    fn input(id: &str) -> BlockInput {
        BlockInput { block: id.parse().expect("a valid block id") }
    }

    #[test]
    fn a_bare_block_resolves_the_default_namespace_and_validates_against_the_census() {
        assert_eq!(parse("stone"), Ok(input("minecraft:stone")));
        assert_eq!(parse("minecraft:oak_planks"), Ok(input("minecraft:oak_planks")));
        assert!(parse("nonexistent_block").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn a_property_list_is_refused_by_name_rather_than_ignored() {
        let refused = parse("minecraft:furnace[facing=north]").expect_err("v1 cannot parse properties");
        assert!(refused.to_string().contains("not supported"), "{refused}");
        assert!(parse("minecraft:furnace").is_ok(), "the control");
    }

    #[test]
    fn the_wire_identity_carries_no_payload() {
        assert_eq!(BlockArg.wire(), ArgumentParser::BlockState);
    }

    #[test]
    fn a_failed_parse_rewinds_the_cursor() {
        let mut reader = StringReader::new("not_a_real_block more");
        assert!(BlockArg.parse(&mut reader).is_err());
        assert_eq!(reader.cursor(), 0);
    }
}
