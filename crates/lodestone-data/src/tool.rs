//! Version-selected `minecraft:tool` evaluation: how fast the
//! held item mines a given block state, and whether it is the correct tool for
//! that block's drops.
//!
//! `lodestone-game`'s `mining` module already replays vanilla's break-time math
//! bit-exactly; what it does not own is the *data*. [`crate::hardness`] supplies
//! half of it (the block's "destroy speed" and whether it demands a correct
//! tool). This module supplies the other half — the two `BreakInputs` fields
//! that depend on the **item**:
//!
//! * `tool_speed` = vanilla's own item-stack "get destroy speed" accessor
//! * `correct_tool` = vanilla's own player "has correct tool for drops" check
//!
//! # Why this needs a version-owned census at all
//!
//! The obvious reading — "decode `minecraft:tool` off the wire and evaluate it"
//! — is only half the story, and the half that never fires in normal play:
//!
//! 1. **A vanilla pickaxe does not send `minecraft:tool`.** A clientbound stack
//!    carries a data-component patch, which is the *delta* from the item's
//!    built-in prototype component map. 26.2 registers a pickaxe's
//!    `minecraft:tool` in that prototype (vanilla's own tool-material
//!    properties-apply step),
//!    so `/give …  diamond_pickaxe` arrives as an **empty patch** and the client
//!    is expected to already know the component. That prototype is version data
//!    → [`generated::ITEM_TOOLS`].
//! 2. **A rule names blocks by tag.** Vanilla's own tool-rule's block set is a
//!    holder set of blocks, in practice `#minecraft:mineable/pickaxe` and
//!    `#minecraft:incorrect_for_<material>_tool`. Tag membership is version data
//!    → [`generated::BLOCK_TAGS`].
//! 3. **When a rule names blocks directly it uses registry ids**, and matching
//!    them against a *block-state* id needs the state→block map, which is
//!    renumbered every version → [`crate::generated_block_registry`].
//!
//! A wire-supplied `minecraft:tool` (`/give …[minecraft:tool={…}]`, datapack
//! items) still overrides the prototype — [`ToolPatch::Set`] — and is evaluated
//! by exactly the same code path, so the two cannot drift.
//!
//! # Datapack-retagged blocks
//!
//! A connection owns its synchronized [`BlockTagSnapshot`]. It passes that
//! immutable snapshot to [`mining_with_tags`] after translating wire members
//! into canonical blocks. An installed snapshot is complete: a missing tag
//! matches nothing, while no snapshot selects the release's built-in tags.
//! Queries never install or read process-wide network overrides.

use std::borrow::Cow;
use std::collections::HashMap;

use lodestone_model::{ItemStack, ToolBlocks, ToolMining, ToolPatch, ToolRule};

use crate::block::Block;
use crate::block_states::StateId;
use crate::generated_tools as generated;
use crate::item::Item;
use crate::GameDataVersion;

pub use generated::{BLOCK_TAG_COUNT, ITEM_TOOL_COUNT};

/// The block set a generated [`ToolRuleDef`] matches against — the static
/// mirror of [`lodestone_model::ToolBlocks`].
#[derive(Clone, Copy, Debug)]
pub enum ToolBlocksDef {
    /// A block tag, keyed into [`generated::BLOCK_TAGS`] by name.
    Tag(&'static str),
    /// An explicit block set as sorted canonical block ids.
    /// Sorted because only membership matters, and sorting makes the match a
    /// binary search.
    Blocks(&'static [u16]),
}

/// One rule of a generated (item-prototype) tool component; the static mirror of
/// [`lodestone_model::ToolRule`].
#[derive(Clone, Copy, Debug)]
pub struct ToolRuleDef {
    /// Blocks this rule applies to.
    pub blocks: ToolBlocksDef,
    /// The rule's mining-speed override, if any.
    pub speed: Option<f32>,
    /// The rule's correct-for-drops verdict, if any.
    pub correct_for_drops: Option<bool>,
}

/// An item's built-in `minecraft:tool` prototype; the static mirror of
/// [`lodestone_model::ItemTool`].
#[derive(Clone, Copy, Debug)]
pub struct ToolDef {
    /// Match rules in vanilla's order. First match wins.
    pub rules: &'static [ToolRuleDef],
    /// Vanilla's own tool record's default-mining-speed field, used when no rule supplies a speed.
    pub default_mining_speed: f32,
    /// Vanilla's own tool record's damage-per-block field.
    pub damage_per_block: u32,
    /// Vanilla's own tool record's can-destroy-blocks-in-creative field.
    pub can_destroy_blocks_in_creative: bool,
}

/// A complete connection-owned set of canonical block tags.
#[derive(Debug, Default)]
pub struct BlockTagSnapshot {
    tags: HashMap<String, Vec<u16>>,
}

impl BlockTagSnapshot {
    /// Builds immutable membership from validated canonical block identities.
    #[must_use]
    pub fn new(tags: HashMap<String, Vec<Block>>) -> Self {
        let tags = tags.into_iter()
            .map(|(name, blocks)| {
                let mut members: Vec<_> = blocks.into_iter().map(Block::registry_id).collect();
                members.sort_unstable();
                members.dedup();
                (name, members)
            })
            .collect();
        Self { tags }
    }

    #[must_use]
    pub fn contains(&self, tag: &str, block: Block) -> bool {
        self.contains_id(tag, block.registry_id())
    }

    fn contains_id(&self, tag: &str, block: u16) -> bool {
        self.tags.get(tag)
            .is_some_and(|members| members.binary_search(&block).is_ok())
    }
}

fn builtin_tags(version: GameDataVersion) -> &'static [(&'static str, &'static [u16])] {
    match version {
        GameDataVersion::V26_2 => &generated::BLOCK_TAGS,
        GameDataVersion::V26_3 => &crate::generated_tools_26_3::BLOCK_TAGS,
    }
}

const _: () = assert!(crate::generated_tools_26_3::BLOCK_TAG_COUNT
    == crate::generated_tools_26_3::BLOCK_TAGS.len());
const _: () = assert!(crate::generated_tools_26_3::ITEM_TOOL_COUNT
    == crate::generated_tools_26_3::ITEM_TOOLS.len());

/// Built-in 26.2 tag members, independent of every connection's synchronized tags.
#[must_use]
pub fn block_tag_members(tag: &str) -> Option<std::borrow::Cow<'static, [u16]>> {
    generated::BLOCK_TAGS
        .binary_search_by_key(&tag, |&(name, _)| name)
        .ok()
        .map(|index| Cow::Borrowed(generated::BLOCK_TAGS[index].1))
}

/// Whether the built-in latest-release block tag contains a canonical block.
#[must_use]
pub fn block_tag_contains(tag: &str, block: Block) -> bool {
    tag_contains_for(tag, block.registry_id(), GameDataVersion::V26_3, None)
}

/// The built-in `minecraft:tool` prototype of `item` (for example
/// `minecraft:diamond_pickaxe`), or `None` for an item that has none.
///
/// This is what a stack's component patch is a delta *against*; see the module
/// docs for why the wire alone is not enough. The lookup accepts canonical
/// built-in names only; unlike [`Item::from_name`], it deliberately rejects a
/// bare path because this boundary receives component keys, not user input.
#[must_use]
pub fn default_tool(item: &str) -> Option<&'static ToolDef> {
    default_tool_for(GameDataVersion::V26_2, item)
}

/// The selected release's built-in tool prototype.
#[must_use]
pub fn default_tool_for(version: GameDataVersion, item: &str) -> Option<&'static ToolDef> {
    let resolved = Item::from_name(item)?;
    (resolved.name() == item).then_some(())?;
    let tools: &[(u16, ToolDef)] = match version {
        GameDataVersion::V26_2 => &generated::ITEM_TOOLS,
        GameDataVersion::V26_3 => &crate::generated_tools_26_3::ITEM_TOOLS,
    };
    tools
        .binary_search_by_key(&resolved.registry_id(), |&(id, _)| id)
        .ok()
        .map(|index| &tools[index].1)
}

/// Resolves the held item's break-time contribution for a block state: vanilla's
/// own item-stack "get destroy speed" accessor and player "has correct tool
/// for drops" check.
///
/// `held` is the main-hand stack; `None` is the bare hand. `state_id` is
/// validated by [`StateId::new`] at the version or wire boundary.
///
/// The returned `correct_tool` is the **player's** flag, already folded with the
/// block's own "requires correct tool for drops" flag — see [`ToolMining::correct_tool`].
#[must_use]
pub fn mining(held: Option<&ItemStack>, state_id: StateId) -> ToolMining {
    mining_with_tags(GameDataVersion::V26_2, held, state_id, None)
        .expect("26.2 mining requires a state supported by the 26.2 data profile")
}

/// Evaluates tool rules against one release and one complete session tag snapshot.
#[must_use]
pub fn mining_with_tags(
    version: GameDataVersion,
    held: Option<&ItemStack>,
    state_id: StateId,
    tags: Option<&BlockTagSnapshot>,
) -> Option<ToolMining> {
    if !version.supports_state(state_id) {
        return None;
    }
    let requires_correct_tool = crate::hardness::hardness(state_id).requires_correct_tool;
    let block = state_id.block().registry_id();

    // The effective `minecraft:tool`, resolved exactly as vanilla's own
    // component-map accessor for that component does: the patch wins if it
    // says anything, otherwise the item's prototype.
    let patch = held.map_or(&ToolPatch::Inherited, |stack| &stack.components.tool);
    Some(match patch {
        ToolPatch::Set(tool) => evaluate(
            tool.rules.len(),
            |index| {
                let rule: &ToolRule = &tool.rules[index];
                (
                    model_rule_matches(rule, block, version, tags),
                    rule.speed(),
                    rule.correct_for_drops,
                )
            },
            tool.default_mining_speed(),
            tool.damage_per_block,
            requires_correct_tool,
        ),
        ToolPatch::Removed => bare_handed(requires_correct_tool),
        ToolPatch::Inherited => {
            let Some(item) = held else {
                return Some(bare_handed(requires_correct_tool));
            };
            match default_tool_for(version, &item.item.to_string()) {
                Some(tool) => evaluate(
                    tool.rules.len(),
                    |index| {
                        let rule = &tool.rules[index];
                        (
                            def_rule_matches(rule, block, version, tags),
                            rule.speed,
                            rule.correct_for_drops,
                        )
                    },
                    tool.default_mining_speed,
                    tool.damage_per_block,
                    requires_correct_tool,
                ),
                None => bare_handed(requires_correct_tool),
            }
        }
    })
}

/// The contribution of an item with no `minecraft:tool` at all — a bare hand, a
/// block, a torch.
///
/// Vanilla's own item "get destroy speed" accessor returns `1.0F` when the component is absent and
/// its own "is correct tool for drops" check returns `false`, which leaves
/// the player's own "has correct tool for drops" check as the plain negation of the block's own
/// requirement. That negation is the whole reason this seam exists rather than
/// callers reading `BlockHardness::requires_correct_tool` directly.
fn bare_handed(requires_correct_tool: bool) -> ToolMining {
    ToolMining {
        speed: 1.0,
        correct_tool: !requires_correct_tool,
        damage_per_block: 0,
    }
}

/// Vanilla's own tool "get mining speed" and "is correct for drops" accessors, over any rule
/// representation.
///
/// `rule(i)` yields `(matches this block, speed override, correct-for-drops
/// verdict)` for rule `i`. Both walks are first-match-wins and **independent**:
/// a rule that only denies drops does not stop the speed search, which is how
/// `#incorrect_for_<material>_tool` (no speed) sits ahead of
/// `#mineable/<class>` (speed) without shadowing it.
fn evaluate(
    rule_count: usize,
    rule: impl Fn(usize) -> (bool, Option<f32>, Option<bool>),
    default_mining_speed: f32,
    damage_per_block: u32,
    requires_correct_tool: bool,
) -> ToolMining {
    let mut speed = None;
    let mut correct = None;
    for index in 0..rule_count {
        let (matches, rule_speed, rule_correct) = rule(index);
        if !matches {
            continue;
        }
        if speed.is_none() {
            speed = rule_speed;
        }
        if correct.is_none() {
            correct = rule_correct;
        }
        if speed.is_some() && correct.is_some() {
            break;
        }
    }
    ToolMining {
        speed: speed.unwrap_or(default_mining_speed),
        // Vanilla's own "has correct tool for drops" check: a block that does not demand a
        // correct tool always drops, whatever the item says.
        correct_tool: !requires_correct_tool || correct.unwrap_or(false),
        damage_per_block,
    }
}

/// Whether a generated prototype rule covers `block` (a registry id).
fn def_rule_matches(
    rule: &ToolRuleDef,
    block: u16,
    version: GameDataVersion,
    tags: Option<&BlockTagSnapshot>,
) -> bool {
    match rule.blocks {
        ToolBlocksDef::Tag(tag) => tag_contains_for(tag, block, version, tags),
        ToolBlocksDef::Blocks(blocks) => blocks.binary_search(&block).is_ok(),
    }
}

/// Whether a wire-decoded rule covers `block` (a registry id).
///
/// The model carries a wire rule's explicit block set in wire order, not sorted,
/// so this is a linear scan — such sets are single-digit in practice (vanilla's
/// only one is `[minecraft:cobweb]`).
fn model_rule_matches(
    rule: &ToolRule,
    block: u16,
    version: GameDataVersion,
    tags: Option<&BlockTagSnapshot>,
) -> bool {
    match &rule.blocks {
        ToolBlocks::Tag(tag) => tag_contains_for(&tag.to_string(), block, version, tags),
        ToolBlocks::Blocks(blocks) => blocks.iter().any(|raw| {
            u32::try_from(*raw).ok()
                .and_then(|raw| version.block_from_wire(raw))
                .is_some_and(|resolved| resolved.registry_id() == block)
        }),
    }
}

fn tag_contains_for(
    tag: &str,
    block: u16,
    version: GameDataVersion,
    tags: Option<&BlockTagSnapshot>,
) -> bool {
    if let Some(tags) = tags {
        return tags.contains_id(tag, block);
    }
    let tags = builtin_tags(version);
    tags.binary_search_by_key(&tag, |&(name, _)| name).ok()
        .is_some_and(|index| tags[index].1.binary_search(&block).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_tool_members_are_wire_ids_not_canonical_ids() {
        // The independent reports assign wire 51 to birch in 26.2 and oak in 26.3.
        let rule = ToolRule::new(ToolBlocks::Blocks(vec![51]), Some(7.25), Some(true));
        assert!(model_rule_matches(&rule, Block::BirchLog.registry_id(), GameDataVersion::V26_2, None));
        assert!(!model_rule_matches(&rule, Block::OakLog.registry_id(), GameDataVersion::V26_2, None));
        assert!(model_rule_matches(&rule, Block::OakLog.registry_id(), GameDataVersion::V26_3, None));
        assert!(!model_rule_matches(&rule, Block::BirchLog.registry_id(), GameDataVersion::V26_3, None));
    }
}
