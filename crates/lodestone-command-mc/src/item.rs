//! `minecraft:item_stack` — vanilla's own item argument, v1.
//!
//! # v1 ships without the component patch, and that is clean
//!
//! Vanilla's own item parser accepts `minecraft:diamond_sword[minecraft:damage=5,
//! minecraft:custom_name='…']` — an SNBT-adjacent component patch. **No textual
//! SNBT parser exists anywhere in this workspace** (the only `snbt` hits are
//! live-test strings, and `read_component_patch` is *wire* decode, a genuinely
//! different problem), so v1 parses the item id and answers a `[` with an
//! explicit refusal.
//!
//! That is a truthful refusal at the right layer rather than a fudge, and the
//! reason is the wire: `minecraft:item_stack` carries **no network payload**,
//! so the transmitted node, the client's
//! autocompletion, and `/give @s minecraft:diamond_sword 3` are all *complete
//! now*. The later SNBT unit replaces the single `[` arm in
//! [`ItemArg::parse`] — not the tree, not the wire, and not
//! [`ItemInput`]'s shape, since a patch becomes fields on that struct.
//!
//! # The id is validated at parse time
//!
//! Vanilla's own item parser resolves against the item registry and throws
//! an unknown-item error on a miss. This does the same against
//! `lodestone_data::items`, so a typo fails as a *parse error the player sees*
//! rather than reaching an executor that has to invent a response. That is the
//! same layering `ChoicesArgument::strict` exists for and the same reason
//! `/gamerule` types its value at the tree.

use lodestone_command::{ArgumentType, ParseError, ParseErrorKind, ParsedValue, StringReader};
use lodestone_model::ItemStack;
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use crate::McArg;

/// `ItemInput` — a validated item id, ready to become stacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInput {
    /// The canonical item id. A [`ResourceKey`] rather than a `String` so it
    /// drops straight into [`ItemStack::new`] and cannot carry an unqualified
    /// name past this point.
    pub item: ResourceKey,
}

impl ItemInput {
    /// The item's effective `minecraft:max_stack_size`, from the real 26.2
    /// prototype census.
    ///
    /// `1` for an item the census does not know, which cannot happen for an
    /// `ItemInput` built by [`ItemArg`] — the parse already rejected an unknown
    /// id — but is the safe answer if one is constructed by hand.
    #[must_use]
    pub fn max_stack_size(&self) -> u32 {
        lodestone_data::item_prototypes::prototype(&self.item.to_string())
            .map_or(1, |proto| u32::from(proto.max_stack_size))
    }

    /// One stack of exactly `count`, without splitting.
    #[must_use]
    pub fn stack(&self, count: u32) -> ItemStack {
        ItemStack::new(self.item.clone(), count)
    }

    /// `count` items split into whole stacks, largest first — vanilla's own
    /// give command's own loop: while remaining stock is positive, take a
    /// stack sized to the smaller of the max stack size and what remains.
    ///
    /// A count of `0` yields no stacks, which is why the tree's `count` argument
    /// is `integer(1)` rather than `integer(0)`.
    #[must_use]
    pub fn split_into_stacks(&self, count: u32) -> Vec<ItemStack> {
        let max = self.max_stack_size().max(1);
        let mut remaining = count;
        let mut out = Vec::new();
        while remaining > 0 {
            let size = max.min(remaining);
            remaining -= size;
            out.push(self.stack(size));
        }
        out
    }
}

/// Vanilla's own give-command item-stack-count multiplier — the multiplier
/// on the item's own max stack size that bounds `/give`'s count
/// (`commands.give.failed.toomanyitems`).
pub const MAX_ALLOWED_ITEMSTACKS: u32 = 100;

/// Vanilla's own item argument.
#[derive(Debug, Default, Clone, Copy)]
pub struct ItemArg;

impl ArgumentType for ItemArg {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        let start = reader.cursor();
        let id = read_item_id(reader);
        if id.is_empty() {
            reader.set_cursor(start);
            return Err(refuse(start, "expected an item"));
        }
        let qualified = if id.contains(':') { id } else { format!("minecraft:{id}") };
        if lodestone_data::items::item_id(&qualified).is_none() {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unknown item '{qualified}'")));
        }
        // Infallible given the census hit above — a name in `ITEM_NAMES` is
        // namespace-qualified and uses only `Identifier`'s own character set —
        // but parsed rather than assumed, so a future census entry that is not
        // fails here rather than downstream.
        let Ok(key) = qualified.parse::<ResourceKey>() else {
            reader.set_cursor(start);
            return Err(refuse(start, format!("unusable item id '{qualified}'")));
        };
        // The one arm the later SNBT unit replaces. Refused rather than ignored:
        // silently dropping a component patch would hand the player a plain
        // sword when they asked for a sharpened one, with no indication why.
        if reader.peek() == Some('[') {
            let position = reader.cursor();
            reader.set_cursor(start);
            return Err(refuse(
                position,
                "item components are not supported yet — give the item without '[...]'",
            ));
        }
        Ok(ParsedValue::dynamic(ItemInput { item: key }))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        // Every item in the 26.2 census. `CommandTree::suggest` applies the
        // case-insensitive prefix filter, exactly as
        // vanilla's own resource-suggestion helper does, so this is offered
        // unfiltered.
        //
        // Both the namespaced and bare forms, because vanilla's own
        // resource-suggestion helper offers `minecraft:stone` *and* `stone` for the
        // default namespace, and a player who types `sto` expects a hit.
        let mut out = Vec::with_capacity(lodestone_data::items::ITEM_COUNT as usize * 2);
        for id in 0..i32::try_from(lodestone_data::items::ITEM_COUNT).unwrap_or(i32::MAX) {
            if let Some(name) = lodestone_data::items::item_name(id) {
                out.push(name.to_string());
                if let Some(path) = name.strip_prefix("minecraft:") {
                    out.push(path.to_string());
                }
            }
        }
        out
    }
}

impl McArg for ItemArg {
    type Value = ItemInput;

    fn wire(&self) -> ArgumentParser {
        ArgumentParser::ItemStack
    }
}

/// Vanilla's own resource-location reader's character class, for an item id.
fn read_item_id(reader: &mut StringReader) -> String {
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

    fn parse(text: &str) -> Result<ItemInput, ParseError> {
        let mut reader = StringReader::new(text);
        ItemArg
            .parse(&mut reader)
            .map(|value| value.downcast_ref::<ItemInput>().expect("ItemArg produces an ItemInput").clone())
    }

    fn input(id: &str) -> ItemInput {
        ItemInput { item: id.parse().expect("a valid item id") }
    }

    #[test]
    fn a_bare_item_resolves_the_default_namespace_and_validates_against_the_census() {
        assert_eq!(parse("diamond"), Ok(input("minecraft:diamond")));
        assert_eq!(parse("minecraft:diamond_sword"), Ok(input("minecraft:diamond_sword")));

        // Validated at parse, so a typo never reaches an executor.
        assert!(parse("diamnod").is_err());
        assert!(parse("minecraft:not_an_item").is_err());
        assert!(parse("").is_err());
    }

    /// The refusal that the later SNBT unit replaces, and the control that the
    /// same item without a patch parses — so the refusal is about the components
    /// and not about the item.
    #[test]
    fn a_component_patch_is_refused_by_name_rather_than_ignored() {
        let refused = parse("minecraft:diamond_sword[minecraft:damage=5]")
            .expect_err("v1 cannot parse a component patch");
        assert!(
            refused.to_string().contains("components are not supported"),
            "the refusal must name components: {refused}"
        );
        assert!(parse("minecraft:diamond_sword").is_ok(), "the control");
    }

    /// Stack splitting against the real prototype census, with values predicted
    /// from vanilla's own max stack sizes rather than read back from the code:
    /// a diamond stacks to 64, a sword to 1.
    #[test]
    fn a_count_splits_into_whole_stacks_at_the_items_own_max() {
        let diamond = input("minecraft:diamond");
        assert_eq!(diamond.max_stack_size(), 64);
        assert_eq!(
            diamond.split_into_stacks(100).iter().map(|s| s.count).collect::<Vec<_>>(),
            [64, 36]
        );
        assert_eq!(diamond.split_into_stacks(3).iter().map(|s| s.count).collect::<Vec<_>>(), [3]);
        assert!(diamond.split_into_stacks(0).is_empty());

        let sword = input("minecraft:diamond_sword");
        assert_eq!(sword.max_stack_size(), 1);
        assert_eq!(sword.split_into_stacks(3).len(), 3, "an unstackable item is one per stack");

        // The total is conserved — the property a per-stack assertion cannot see.
        assert_eq!(
            diamond.split_into_stacks(517).iter().map(|s| s.count).sum::<u32>(),
            517
        );
    }

    #[test]
    fn the_wire_identity_carries_no_payload() {
        assert_eq!(ItemArg.wire(), ArgumentParser::ItemStack);
        assert_eq!(ItemArg.suggestion_provider(), None);
    }

    /// A failed parse rewinds, including the component-patch case — where the
    /// cursor had already advanced past a *valid* item id before the refusal.
    #[test]
    fn every_failure_path_rewinds_the_cursor() {
        for bad in ["diamnod", "minecraft:diamond_sword[x=1]", "!"] {
            let mut reader = StringReader::new(bad);
            assert!(ItemArg.parse(&mut reader).is_err(), "{bad:?}");
            assert_eq!(reader.cursor(), 0, "{bad:?} must rewind");
        }
    }
}
