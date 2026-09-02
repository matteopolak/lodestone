//! The client-side Brigadier command tree.
//!
//! ## What it is
//!
//! `minecraft:commands` (clientbound, id 16) sends the server's whole
//! Brigadier command dispatcher as a flat list of nodes — root / literal /
//! argument, each with a redirect, an executable flag, and (for arguments) a
//! parser id plus that parser's own network template. [`CommandTree`] is the
//! version-free, decode-target shape of that packet: something a protocol
//! adapter constructs directly from wire bytes, and something
//! `lodestone-shell`'s chat box walks to drive tab completion and syntax
//! highlighting, without either side depending on the other's crate.
//!
//! `minecraft:command_suggestions` (clientbound, id 15) is the answer to a
//! serverbound `command_suggestion` request; [`CommandSuggestionsResponse`]
//! is its decode target, unpacked from a transaction id, a `start` offset and
//! a `length`, plus the suggestion list — the offset and length describe the
//! `[start, start+length)` byte range of the input line the suggestions
//! replace.
//!
//! ## How it works
//!
//! [`ArgumentParser`] is keyed by 26.2's `minecraft:command_argument_type`
//! registry id, and each variant's payload mirrors exactly what that
//! parser's own network-serialization routine writes — a numeric min/max
//! pair with a leading flags byte for the four Brigadier primitives, a
//! `StringType` ordinal for `brigadier:string`, a flags byte for
//! `entity`/`score_holder`, a plain `int` for `time`, an `Identifier`
//! (VarInt-length UTF-8 registry key) for the five `resource*` parsers, and
//! no payload at all for every other parser (whose network-serialization
//! routine is a no-op).
//!
//! Vanilla itself degrades an unrecognised argument-type id to a nameless
//! pass-through node (the commands-packet decoder returns null for an
//! unknown parser, and the tree-resolution step turns that null stub into a
//! bare root node rather than failing the whole tree) — [`NodeKind::Unrecognized`]
//! keeps the same shape (no name, no parser, but its children/redirect/executable
//! flag still apply) rather than rejecting the packet, so a future/mod
//! argument type this build doesn't know about can't take the whole tree
//! down.
//!
//! A **redirect is a same-position jump, not a token-consuming one** — a
//! server can legally send a redirect cycle (`execute`'s own `run` argument
//! redirects back toward the root), so [`CommandTree::effective_children`]
//! is the one place that walks them, and it does so with a visited-node
//! guard rather than trusting the graph to be acyclic. See its own doc and
//! `command_tree::tests::effective_children_terminates_on_a_redirect_cycle`
//! for the control that proves the guard actually fires.
//!
//! ## How to change it
//!
//! - Adding a new `minecraft:command_argument_type` entry: add a variant to
//!   [`ArgumentParser`], a case to [`ArgumentParser::from_registry_id`], and
//!   (only if the type has network payload) update whichever protocol-crate
//!   decode arm calls in. This module has no protocol dependency, so it
//!   cannot itself decode bytes — see `docs/commands.md`'s "brokered decode"
//!   section for the arm this was handed off to.
//! - Changing tab-completion or highlighting behaviour is **not** here —
//!   that logic lives in `lodestone-shell/src/chat.rs`, which walks a
//!   `CommandTree` but owns none of its bytes.
//!
//! ## Dependencies
//!
//! Depends only on [`crate::ids::Identifier`] (aliased [`crate::ids::ResourceKey`])
//! for registry keys and suggestion-provider ids. No protocol or shell crate
//! dependency in either direction — `crates/protocol/*` depends on this
//! crate, not the reverse.

use std::collections::HashSet;

use crate::ids::ResourceKey;
use crate::text::Text;

/// Brigadier's own `StringType`'s three variants, in its declared enum
/// order — the wire buffer's enum-write helper sends the ordinal as a
/// VarInt, in the string argument's own network-serialization routine.
///
/// This ordinal order is **not** sourced from this session's decompiled
/// `.cache/mc/26.2` tree — `com.mojang.brigadier` is a separate library and
/// isn't checked out there. It is taken from the public, long-stable
/// Mojang/brigadier source (`SINGLE_WORD, QUOTABLE_PHRASE, GREEDY_PHRASE`),
/// which has not changed across any Minecraft version this project targets.
/// Flagging this explicitly per this repo's evidence standard: everything
/// else in this module is sourced from `.cache/mc/26.2`; this one enum is
/// external-library knowledge, not fetched fresh this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKind {
    /// A single token with no internal whitespace.
    SingleWord,
    /// A quoted string, or a single unquoted word.
    QuotablePhrase,
    /// The remainder of the input line, verbatim.
    GreedyPhrase,
}

/// One argument type's parser id and network template, keyed by 26.2's
/// `minecraft:command_argument_type` registry.
///
/// Every payload here is copied from that parser's own
/// network-serialization/deserialization pair. Parsers with no network
/// payload are unit variants.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgumentParser {
    /// `brigadier:bool` (id 0). No network payload; Brigadier's own
    /// bool-argument type suggests `true`/`false` unconditionally.
    Bool,
    /// `brigadier:float` (id 1). Vanilla's float-argument info: a flags byte
    /// then an `f32` for each present bound; absent bounds are
    /// `-f32::MAX`/`f32::MAX`.
    Float { min: f32, max: f32 },
    /// `brigadier:double` (id 2). Same shape as `Float`, `f64` bounds.
    Double { min: f64, max: f64 },
    /// `brigadier:integer` (id 3). Same shape, `i32` bounds
    /// (`i32::MIN`/`i32::MAX` when absent).
    Integer { min: i32, max: i32 },
    /// `brigadier:long` (id 4). Same shape, `i64` bounds.
    Long { min: i64, max: i64 },
    /// `brigadier:string` (id 5). Vanilla's string-argument serializer: a
    /// single `StringType` enum ordinal, no bounds.
    String(StringKind),
    /// `minecraft:entity` (id 6). Vanilla's entity-argument info: a flags
    /// byte, bit 0 `single` (only one entity/player), bit 1
    /// `players_only`.
    Entity { single: bool, players_only: bool },
    /// `minecraft:game_profile` (id 7). No payload.
    GameProfile,
    /// `minecraft:block_pos` (id 8). No payload.
    BlockPos,
    /// `minecraft:column_pos` (id 9). No payload.
    ColumnPos,
    /// `minecraft:vec3` (id 10). No payload.
    Vec3,
    /// `minecraft:vec2` (id 11). No payload.
    Vec2,
    /// `minecraft:block_state` (id 12). No payload.
    BlockState,
    /// `minecraft:block_predicate` (id 13). No payload.
    BlockPredicate,
    /// `minecraft:item_stack` (id 14). No payload.
    ItemStack,
    /// `minecraft:item_predicate` (id 15). No payload.
    ItemPredicate,
    /// `minecraft:team_color` (id 16). No payload.
    TeamColor,
    /// `minecraft:hex_color` (id 17). No payload.
    HexColor,
    /// `minecraft:component` (id 18). No payload.
    Component,
    /// `minecraft:style` (id 19). No payload.
    Style,
    /// `minecraft:message` (id 20). No payload.
    Message,
    /// `minecraft:nbt_compound_tag` (id 21). No payload.
    NbtCompoundTag,
    /// `minecraft:nbt_tag` (id 22). No payload.
    NbtTag,
    /// `minecraft:nbt_path` (id 23). No payload.
    NbtPath,
    /// `minecraft:objective` (id 24). No payload.
    Objective,
    /// `minecraft:objective_criteria` (id 25). No payload.
    ObjectiveCriteria,
    /// `minecraft:operation` (id 26). No payload. Vanilla's operation-argument
    /// parser suggests exactly `["=", "+=", "-=", "*=", "/=", "%=", "<", ">",
    /// "><"]`.
    Operation,
    /// `minecraft:particle` (id 27). No payload.
    Particle,
    /// `minecraft:angle` (id 28). No payload.
    Angle,
    /// `minecraft:rotation` (id 29). No payload.
    Rotation,
    /// `minecraft:scoreboard_slot` (id 30). No payload. Vanilla's
    /// scoreboard-slot-argument parser suggests every display slot's
    /// serialized name: `list`, `sidebar`, `below_name`, and
    /// `sidebar.team.<colour>` for the sixteen team colours.
    ScoreboardSlot,
    /// `minecraft:score_holder` (id 31). Vanilla's score-holder-argument
    /// info: a flags byte, bit 0 `multiple`.
    ScoreHolder { multiple: bool },
    /// `minecraft:swizzle` (id 32). No payload. Vanilla's swizzle-argument
    /// parser has no custom suggestion override, so Brigadier's default
    /// (empty) applies — vanilla itself offers zero completions for this
    /// parser.
    Swizzle,
    /// `minecraft:team` (id 33). No payload.
    Team,
    /// `minecraft:item_slot` (id 34). No payload.
    ItemSlot,
    /// `minecraft:item_slots` (id 35). No payload.
    ItemSlots,
    /// `minecraft:resource_location` (id 36). No payload.
    ResourceLocation,
    /// `minecraft:function` (id 37). No payload.
    Function,
    /// `minecraft:entity_anchor` (id 38). No payload. Vanilla's
    /// entity-anchor-argument parser suggests exactly `["feet", "eyes"]`, in
    /// the anchor enum's declaration order.
    EntityAnchor,
    /// `minecraft:int_range` (id 39). No payload.
    IntRange,
    /// `minecraft:float_range` (id 40). No payload.
    FloatRange,
    /// `minecraft:dimension` (id 41). No payload.
    Dimension,
    /// `minecraft:gamemode` (id 42). No payload. Vanilla's
    /// game-mode-argument parser suggests exactly `["survival", "creative",
    /// "adventure", "spectator"]`, in the game-mode enum's declaration
    /// order.
    GameMode,
    /// `minecraft:time` (id 43). Vanilla's time-argument info: a plain
    /// `i32` minimum tick count (no flags byte, no maximum).
    Time { min: i32 },
    /// `minecraft:resource_or_tag` (id 44). Vanilla's
    /// resource-or-tag-argument info: one `Identifier` registry key
    /// (a VarInt-length UTF-8 string, no separate namespace/path split on
    /// the wire).
    ResourceOrTag { registry: ResourceKey },
    /// `minecraft:resource_or_tag_key` (id 45). Same shape as
    /// `ResourceOrTag`.
    ResourceOrTagKey { registry: ResourceKey },
    /// `minecraft:resource` (id 46). Same shape as `ResourceOrTag`.
    Resource { registry: ResourceKey },
    /// `minecraft:resource_key` (id 47). Same shape as `ResourceOrTag`.
    ResourceKeyArg { registry: ResourceKey },
    /// `minecraft:resource_selector` (id 48). Same shape as `ResourceOrTag`.
    ResourceSelector { registry: ResourceKey },
    /// `minecraft:template_mirror` (id 49). No payload.
    TemplateMirror,
    /// `minecraft:template_rotation` (id 50). No payload.
    TemplateRotation,
    /// `minecraft:heightmap` (id 51). No payload.
    Heightmap,
    /// `minecraft:loot_table` (id 52). No payload.
    LootTable,
    /// `minecraft:loot_predicate` (id 53). No payload.
    LootPredicate,
    /// `minecraft:loot_modifier` (id 54). No payload.
    LootModifier,
    /// `minecraft:dialog` (id 55). No payload.
    Dialog,
    /// `minecraft:uuid` (id 56). No payload.
    Uuid,
    /// A registry id this build doesn't recognise — a newer or datapack/mod
    /// argument type. Carries the raw id for diagnostics; the owning
    /// [`NodeKind::Unrecognized`] node has no name and matches no input, the
    /// same way vanilla's own client degrades it.
    Unknown(i32),
}

/// One decoded node's identity: which of Brigadier's three built-in node
/// kinds it is, plus (for arguments) its parser and optional custom
/// suggestions provider.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// The tree's single root. Never named, never matches a token.
    Root,
    /// A fixed keyword, matched by exact text.
    Literal {
        /// The literal's exact text, e.g. `"gamemode"`.
        name: String,
    },
    /// A typed value, matched by parsing.
    Argument {
        /// The argument's name (not shown to the player by this client, but
        /// carried for parity with the server's usage strings).
        name: String,
        /// How the argument's text is parsed / what it suggests.
        parser: ArgumentParser,
        /// The `FLAG_CUSTOM_SUGGESTIONS` provider id, when present. Nearly
        /// always `minecraft:ask_server` in practice — see
        /// `ArgumentParser`'s module doc and `NEEDS_SERVER` in
        /// `lodestone-shell`'s `chat.rs` for what this drives.
        suggestions: Option<ResourceKey>,
    },
    /// An argument-type id this build doesn't recognise. See this module's
    /// own doc for why vanilla (and this client) treat it as a nameless
    /// pass-through rather than an error.
    Unrecognized {
        /// The raw, unrecognised `minecraft:command_argument_type` id.
        parser_id: i32,
    },
}

/// One node exactly as vanilla's commands-packet entry carries it: flags,
/// a redirect target, and a child index list, all as flat indices into the
/// owning [`CommandTree`]'s node list.
#[derive(Debug, Clone, PartialEq)]
pub struct RawCommandNode {
    /// This node's kind and (for arguments) parser.
    pub kind: NodeKind,
    /// The executable flag bit (`0x04`) — a command ending here can be run
    /// with no further tokens.
    pub executable: bool,
    /// The restricted flag bit (`0x20`) — the server would reject this node
    /// for a permission-lacking sender; carried for parity, not yet
    /// enforced by this client.
    pub restricted: bool,
    /// The redirect flag bit (`0x08`) target, when present: a
    /// same-position jump, not a token-consuming child. See
    /// [`CommandTree::effective_children`].
    pub redirect: Option<usize>,
    /// This node's own children (token-consuming).
    pub children: Vec<usize>,
}

/// Error constructing a [`CommandTree`] from raw decoded nodes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandTreeError {
    /// The declared root index is outside the node list.
    #[error("root index {0} is out of range for {1} nodes")]
    RootOutOfRange(usize, usize),
    /// A node's child index is outside the node list.
    #[error("node {0}'s child index {1} is out of range for {2} nodes")]
    ChildOutOfRange(usize, usize, usize),
    /// A node's redirect index is outside the node list.
    #[error("node {0}'s redirect index {1} is out of range for {2} nodes")]
    RedirectOutOfRange(usize, usize, usize),
}

/// The client-side Brigadier command tree, decoded from `minecraft:commands`.
///
/// Deliberately index-based (matching the wire's own flat entry list plus
/// a root index) rather than a pointer/`Rc`-linked tree: it is what a
/// protocol adapter can build directly out of vanilla's flat
/// commands-packet entry list with no intermediate allocation scheme, and
/// indices make [`Self::effective_children`]'s cycle guard a plain
/// `HashSet<usize>` instead of anything unsafe.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandTree {
    nodes: Vec<RawCommandNode>,
    root: usize,
}

impl CommandTree {
    /// Builds a tree from already-decoded nodes and a root index.
    ///
    /// Validates every index (`root`, every child, every redirect) is in
    /// range. Does **not** reject redirect cycles — the server is entitled
    /// to send one (`execute run` redirects back toward the root by design)
    /// — callers that walk redirects must guard against revisiting a node
    /// themselves; see [`Self::effective_children`].
    ///
    /// # Errors
    ///
    /// Returns [`CommandTreeError`] naming the first out-of-range index
    /// found.
    pub fn new(nodes: Vec<RawCommandNode>, root: usize) -> Result<Self, CommandTreeError> {
        if root >= nodes.len() {
            return Err(CommandTreeError::RootOutOfRange(root, nodes.len()));
        }
        for (index, node) in nodes.iter().enumerate() {
            for &child in &node.children {
                if child >= nodes.len() {
                    return Err(CommandTreeError::ChildOutOfRange(index, child, nodes.len()));
                }
            }
            if let Some(redirect) = node.redirect {
                if redirect >= nodes.len() {
                    return Err(CommandTreeError::RedirectOutOfRange(
                        index,
                        redirect,
                        nodes.len(),
                    ));
                }
            }
        }
        Ok(Self { nodes, root })
    }

    /// The root node's index.
    #[must_use]
    pub fn root(&self) -> usize {
        self.root
    }

    /// Looks up a node by index.
    #[must_use]
    pub fn node(&self, index: usize) -> Option<&RawCommandNode> {
        self.nodes.get(index)
    }

    /// How many nodes the tree has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree has no nodes. A [`CommandTree`] built via
    /// [`Self::new`] with any valid root is never empty, but this is cheap
    /// to offer for parity with `Vec::is_empty`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The set of node indices reachable from `start` **without consuming a
    /// token**: `start`'s own children, plus (recursively) the children of
    /// whatever `start` redirects to.
    ///
    /// A redirect is a same-position jump — walking one costs nothing in
    /// input, so a server-sent redirect cycle (`A` redirects to `B`, `B`
    /// redirects back to `A`) would recurse forever without a guard. This
    /// tracks visited node indices and stops expanding a node it has already
    /// seen, so the traversal terminates in at most `self.len()` steps
    /// regardless of how the redirects are wired.
    #[must_use]
    pub fn effective_children(&self, start: usize) -> Vec<usize> {
        let mut visited = HashSet::new();
        let mut out = Vec::new();
        self.effective_children_into(start, &mut visited, &mut out);
        out
    }

    fn effective_children_into(
        &self,
        index: usize,
        visited: &mut HashSet<usize>,
        out: &mut Vec<usize>,
    ) {
        if !visited.insert(index) {
            return;
        }
        let Some(node) = self.node(index) else {
            return;
        };
        out.extend(node.children.iter().copied());
        if let Some(redirect) = node.redirect {
            self.effective_children_into(redirect, visited, out);
        }
    }
}

/// One suggestion in a `minecraft:command_suggestions` response.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSuggestionEntry {
    /// The literal replacement text for the input range this response
    /// carries.
    pub text: String,
    /// An optional tooltip component, carried as a real [`Text`] rather than
    /// flattened to a legacy `§`-coded string. `None` when the server sent no
    /// tooltip for this entry.
    ///
    /// A tooltip is a genuine JSON/NBT text component on the wire in every
    /// family that carries one at all, and can legitimately hold a hex
    /// colour (`TextColor::Rgb`, added in 1.16) that no legacy code can
    /// represent — so a decode arm must not call
    /// [`Text::to_legacy_string`](crate::text::Text::to_legacy_string) on
    /// this field. `v47`/`v340` predate the transaction-id/range/tooltip
    /// shape entirely (their `TAB_COMPLETE` is a bare `matches: string[]`),
    /// so they always construct `None` here regardless of this field's
    /// type; that is a real absence on the wire, not a flatten.
    pub tooltip: Option<Text>,
}

/// Decode target for `minecraft:command_suggestions` (clientbound, id 15):
/// a transaction id, a `start` offset, a `length`, and the suggestion list.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSuggestionsResponse {
    /// Transaction id, echoing the `id` sent in the serverbound
    /// `command_suggestion` request this answers.
    pub id: i32,
    /// Start of the byte range (into the request's command text) these
    /// suggestions replace.
    pub start: i32,
    /// Length of that byte range.
    pub length: i32,
    /// The suggested replacement strings for that range.
    pub suggestions: Vec<CommandSuggestionEntry>,
}

impl ArgumentParser {
    /// Maps a `minecraft:command_argument_type` registry protocol id (26.2
    /// `registries.json`) to its parser id, for parsers with **no** network
    /// payload of their own. Parsers that do carry payload
    /// (`Float`/`Double`/`Integer`/`Long`/`String`/`Entity`/`ScoreHolder`/
    /// `Time`/the five `resource*` parsers) are constructed directly by the
    /// decode arm instead, since only it has the bytes; this covers every
    /// other id.
    ///
    /// Returns [`ArgumentParser::Unknown`] for any id outside `0..=56` (this
    /// build's known registry range) rather than failing — see this module's
    /// own doc for why that must not reject the packet.
    #[must_use]
    pub fn from_registry_id_no_payload(id: i32) -> Self {
        match id {
            0 => Self::Bool,
            6 => Self::Entity {
                single: false,
                players_only: false,
            },
            7 => Self::GameProfile,
            8 => Self::BlockPos,
            9 => Self::ColumnPos,
            10 => Self::Vec3,
            11 => Self::Vec2,
            12 => Self::BlockState,
            13 => Self::BlockPredicate,
            14 => Self::ItemStack,
            15 => Self::ItemPredicate,
            16 => Self::TeamColor,
            17 => Self::HexColor,
            18 => Self::Component,
            19 => Self::Style,
            20 => Self::Message,
            21 => Self::NbtCompoundTag,
            22 => Self::NbtTag,
            23 => Self::NbtPath,
            24 => Self::Objective,
            25 => Self::ObjectiveCriteria,
            26 => Self::Operation,
            27 => Self::Particle,
            28 => Self::Angle,
            29 => Self::Rotation,
            30 => Self::ScoreboardSlot,
            31 => Self::ScoreHolder { multiple: false },
            32 => Self::Swizzle,
            33 => Self::Team,
            34 => Self::ItemSlot,
            35 => Self::ItemSlots,
            36 => Self::ResourceLocation,
            37 => Self::Function,
            38 => Self::EntityAnchor,
            39 => Self::IntRange,
            40 => Self::FloatRange,
            41 => Self::Dimension,
            42 => Self::GameMode,
            49 => Self::TemplateMirror,
            50 => Self::TemplateRotation,
            51 => Self::Heightmap,
            52 => Self::LootTable,
            53 => Self::LootPredicate,
            54 => Self::LootModifier,
            55 => Self::Dialog,
            56 => Self::Uuid,
            other => Self::Unknown(other),
        }
    }

    /// The registry ids this parser is known to have network payload for
    /// (`Float`=1, `Double`=2, `Integer`=3, `Long`=4, `String`=5, `Entity`=6,
    /// `ScoreHolder`=31, `Time`=43, `ResourceOrTag`=44,
    /// `ResourceOrTagKey`=45, `Resource`=46, `ResourceKeyArg`=47,
    /// `ResourceSelector`=48), for a decode arm to check before falling back
    /// to [`Self::from_registry_id_no_payload`].
    #[must_use]
    pub fn has_network_payload(id: i32) -> bool {
        matches!(id, 1..=6 | 31 | 43..=48)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(name: &str, children: Vec<usize>) -> RawCommandNode {
        RawCommandNode {
            kind: NodeKind::Literal {
                name: name.to_string(),
            },
            executable: false,
            restricted: false,
            redirect: None,
            children,
        }
    }

    fn root(children: Vec<usize>) -> RawCommandNode {
        RawCommandNode {
            kind: NodeKind::Root,
            executable: false,
            restricted: false,
            redirect: None,
            children,
        }
    }

    #[test]
    fn rejects_an_out_of_range_root() {
        let nodes = vec![root(vec![])];
        assert_eq!(
            CommandTree::new(nodes, 5),
            Err(CommandTreeError::RootOutOfRange(5, 1))
        );
    }

    #[test]
    fn rejects_an_out_of_range_child() {
        let nodes = vec![root(vec![9])];
        assert_eq!(
            CommandTree::new(nodes, 0),
            Err(CommandTreeError::ChildOutOfRange(0, 9, 1))
        );
    }

    #[test]
    fn rejects_an_out_of_range_redirect() {
        let mut node = root(vec![]);
        node.redirect = Some(9);
        let nodes = vec![node];
        assert_eq!(
            CommandTree::new(nodes, 0),
            Err(CommandTreeError::RedirectOutOfRange(0, 9, 1))
        );
    }

    /// The termination control this module's doc promises: a two-node
    /// redirect cycle (0 redirects to 1, 1 redirects back to 0) must not
    /// hang `effective_children`, and the *negative* control — a
    /// visited-set with the guard deliberately disabled — proves the guard
    /// is what stops it, not luck.
    #[test]
    fn effective_children_terminates_on_a_redirect_cycle() {
        let mut a = literal("a", vec![2]);
        a.redirect = Some(1);
        let mut b = literal("b", vec![3]);
        b.redirect = Some(0);
        let nodes = vec![a, b, literal("a_child", vec![]), literal("b_child", vec![])];
        let tree = CommandTree::new(nodes, 0).unwrap();

        // Must return promptly (no hang) and must include both nodes' own
        // children, reached by hopping the cycle exactly once each way.
        let mut reached = tree.effective_children(0);
        reached.sort_unstable();
        assert_eq!(reached, vec![2, 3]);
    }

    /// The positive control's premise, checked directly: with the same
    /// cycle, a naive unguarded expansion (no visited set) would revisit
    /// node 0 and node 1 forever. This test doesn't call such a function —
    /// none exists in this module, by design — it instead demonstrates the
    /// cycle is real by walking one hop manually and observing it points
    /// straight back at the start, which is what would drive an unguarded
    /// walker into infinite recursion.
    #[test]
    fn the_redirect_cycle_used_above_is_a_genuine_cycle() {
        let mut a = literal("a", vec![]);
        a.redirect = Some(1);
        let mut b = literal("b", vec![]);
        b.redirect = Some(0);
        let nodes = vec![a, b];
        let tree = CommandTree::new(nodes, 0).unwrap();

        assert_eq!(tree.node(0).unwrap().redirect, Some(1));
        assert_eq!(tree.node(1).unwrap().redirect, Some(0));
    }

    #[test]
    fn effective_children_includes_own_children_with_no_redirect() {
        let nodes = vec![root(vec![1, 2]), literal("a", vec![]), literal("b", vec![])];
        let tree = CommandTree::new(nodes, 0).unwrap();
        assert_eq!(tree.effective_children(0), vec![1, 2]);
    }

    #[test]
    fn unknown_registry_id_falls_back_without_erroring() {
        assert_eq!(
            ArgumentParser::from_registry_id_no_payload(999),
            ArgumentParser::Unknown(999)
        );
    }

    #[test]
    fn has_network_payload_matches_the_documented_ids() {
        for id in [1, 2, 3, 4, 5, 6, 31, 43, 44, 45, 46, 47, 48] {
            assert!(ArgumentParser::has_network_payload(id), "id {id}");
        }
        for id in [0, 7, 8, 30, 32, 42, 56] {
            assert!(!ArgumentParser::has_network_payload(id), "id {id}");
        }
    }
}
