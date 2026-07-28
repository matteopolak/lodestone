//! `minecraft:tool` evaluation for protocol 776 (Minecraft 26.2): how fast the
//! held item mines a given block state, and whether it is the correct tool for
//! that block's drops.
//!
//! `lodestone-game`'s `mining` module already replays vanilla's break-time math
//! bit-exactly; what it does not own is the *data*. [`crate::hardness`] supplies
//! half of it (the block's `destroySpeed` and whether it demands a correct
//! tool). This module supplies the other half — the two `BreakInputs` fields
//! that depend on the **item**:
//!
//! * `tool_speed` = `ItemStack.getDestroySpeed(state)`
//! * `correct_tool` = `Player.hasCorrectToolForDrops(state)`
//!
//! # Why this needs a version-owned census at all
//!
//! The obvious reading — "decode `minecraft:tool` off the wire and evaluate it"
//! — is only half the story, and the half that never fires in normal play:
//!
//! 1. **A vanilla pickaxe does not send `minecraft:tool`.** A clientbound stack
//!    carries a `DataComponentPatch`, which is the *delta* from the item's
//!    built-in prototype component map. 26.2 registers a pickaxe's
//!    `minecraft:tool` in that prototype (`ToolMaterial.applyToolProperties`),
//!    so `/give …  diamond_pickaxe` arrives as an **empty patch** and the client
//!    is expected to already know the component. That prototype is version data
//!    → [`generated::ITEM_TOOLS`].
//! 2. **A rule names blocks by tag.** `Tool.Rule.blocks` is a
//!    `HolderSet<Block>`, in practice `#minecraft:mineable/pickaxe` and
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
//! # Known gap: datapack-retagged blocks
//!
//! Block tags are *synced* to the client (`update_tags`), and this build does
//! not decode that packet, so [`block_tag_members`] answers from the vanilla
//! census. A datapack that moves a block between `mineable/*` tags will mine at
//! the vanilla rate here. When `update_tags` is decoded, override this table at
//! [`block_tag_members`] — it is the single lookup every rule match goes
//! through.

use lodestone_model::{ItemStack, ToolBlocks, ToolMining, ToolPatch, ToolRule};

use crate::generated_tools as generated;

pub use generated::{BLOCK_TAG_COUNT, ITEM_TOOL_COUNT};

/// The block set a generated [`ToolRuleDef`] matches against — the static
/// mirror of [`lodestone_model::ToolBlocks`].
#[derive(Clone, Copy, Debug)]
pub enum ToolBlocksDef {
    /// A block tag, keyed into [`generated::BLOCK_TAGS`] by name.
    Tag(&'static str),
    /// An explicit block set as **sorted** `minecraft:block` registry ids.
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
    /// `Tool.defaultMiningSpeed`, used when no rule supplies a speed.
    pub default_mining_speed: f32,
    /// `Tool.damagePerBlock`.
    pub damage_per_block: u32,
    /// `Tool.canDestroyBlocksInCreative`.
    pub can_destroy_blocks_in_creative: bool,
}

/// The `minecraft:block` registry id of block-state `state_id`, or `None` when
/// the state is unknown to this version.
///
/// Block *state* ids and block *registry* ids are different id spaces — 32,366
/// states over 1,196 blocks — and the wire uses both: chunk palettes and
/// `block_update` carry state ids, while a `Holder<Block>` (a `block_event`
/// target, a tool rule's explicit block set) carries a registry id. This is the
/// bridge between them.
#[must_use]
pub fn block_registry_id(state_id: u32) -> Option<u16> {
    crate::generated_block_registry::STATE_BLOCK
        .get(state_id as usize)
        .copied()
}

/// The member blocks of `tag` (for example `minecraft:mineable/pickaxe`) as
/// **sorted** `minecraft:block` registry ids, or `None` if this version's census
/// has no such block tag.
///
/// The name is written without the leading `#`, exactly as the wire and
/// `TagKey.location()` write it.
#[must_use]
pub fn block_tag_members(tag: &str) -> Option<&'static [u16]> {
    generated::BLOCK_TAGS
        .binary_search_by_key(&tag, |&(name, _)| name)
        .ok()
        .map(|index| generated::BLOCK_TAGS[index].1)
}

/// The built-in `minecraft:tool` prototype of `item` (for example
/// `minecraft:diamond_pickaxe`), or `None` for an item that has none.
///
/// This is what a stack's component patch is a delta *against*; see the module
/// docs for why the wire alone is not enough.
#[must_use]
pub fn default_tool(item: &str) -> Option<&'static ToolDef> {
    generated::ITEM_TOOLS
        .binary_search_by_key(&item, |&(name, _)| name)
        .ok()
        .map(|index| &generated::ITEM_TOOLS[index].1)
}

/// Resolves the held item's break-time contribution for a block state: vanilla
/// `ItemStack.getDestroySpeed` and `Player.hasCorrectToolForDrops`.
///
/// `held` is the main-hand stack; `None` is the bare hand. Returns `None` only
/// when `state_id` is not a state this version knows.
///
/// The returned `correct_tool` is the **player's** flag, already folded with the
/// block's own `requiresCorrectToolForDrops` — see [`ToolMining::correct_tool`].
#[must_use]
pub fn mining(held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
    let requires_correct_tool = crate::hardness::hardness(state_id)?.requires_correct_tool;
    let block = block_registry_id(state_id)?;

    // The effective `minecraft:tool`, resolved exactly as
    // `ItemStack.get(DataComponents.TOOL)` does: the patch wins if it says
    // anything, otherwise the item's prototype.
    let patch = held.map_or(&ToolPatch::Inherited, |stack| &stack.components.tool);
    match patch {
        ToolPatch::Set(tool) => Some(evaluate(
            tool.rules.len(),
            |index| {
                let rule: &ToolRule = &tool.rules[index];
                (
                    model_rule_matches(rule, block),
                    rule.speed(),
                    rule.correct_for_drops,
                )
            },
            tool.default_mining_speed(),
            tool.damage_per_block,
            requires_correct_tool,
        )),
        ToolPatch::Removed => Some(bare_handed(requires_correct_tool)),
        ToolPatch::Inherited => {
            let Some(item) = held else {
                return Some(bare_handed(requires_correct_tool));
            };
            match default_tool(&item.item.to_string()) {
                Some(tool) => Some(evaluate(
                    tool.rules.len(),
                    |index| {
                        let rule = &tool.rules[index];
                        (
                            def_rule_matches(rule, block),
                            rule.speed,
                            rule.correct_for_drops,
                        )
                    },
                    tool.default_mining_speed,
                    tool.damage_per_block,
                    requires_correct_tool,
                )),
                None => Some(bare_handed(requires_correct_tool)),
            }
        }
    }
}

/// The contribution of an item with no `minecraft:tool` at all — a bare hand, a
/// block, a torch.
///
/// `Item.getDestroySpeed` returns `1.0F` when the component is absent and
/// `Item.isCorrectToolForDrops` returns `false`, which leaves
/// `Player.hasCorrectToolForDrops` as the plain negation of the block's own
/// requirement. That negation is the whole reason this seam exists rather than
/// callers reading `BlockHardness::requires_correct_tool` directly.
fn bare_handed(requires_correct_tool: bool) -> ToolMining {
    ToolMining {
        speed: 1.0,
        correct_tool: !requires_correct_tool,
        damage_per_block: 0,
    }
}

/// Vanilla `Tool.getMiningSpeed` + `Tool.isCorrectForDrops`, over any rule
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
        // `Player.hasCorrectToolForDrops`: a block that does not demand a
        // correct tool always drops, whatever the item says.
        correct_tool: !requires_correct_tool || correct.unwrap_or(false),
        damage_per_block,
    }
}

/// Whether a generated prototype rule covers `block` (a registry id).
fn def_rule_matches(rule: &ToolRuleDef, block: u16) -> bool {
    match rule.blocks {
        ToolBlocksDef::Tag(tag) => tag_contains(tag, block),
        ToolBlocksDef::Blocks(blocks) => blocks.binary_search(&block).is_ok(),
    }
}

/// Whether a wire-decoded rule covers `block` (a registry id).
///
/// The model carries a wire rule's explicit block set in wire order, not sorted,
/// so this is a linear scan — such sets are single-digit in practice (vanilla's
/// only one is `[minecraft:cobweb]`).
fn model_rule_matches(rule: &ToolRule, block: u16) -> bool {
    match &rule.blocks {
        ToolBlocks::Tag(tag) => tag_contains(&tag.to_string(), block),
        ToolBlocks::Blocks(blocks) => blocks.contains(&i32::from(block)),
    }
}

/// Whether block tag `tag` contains `block`. An unknown tag matches nothing —
/// see the module docs' "Known gap".
fn tag_contains(tag: &str, block: u16) -> bool {
    block_tag_members(tag).is_some_and(|members| members.binary_search(&block).is_ok())
}
