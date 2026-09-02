//! The server's **own** Brigadier command tree, and the execution model
//! `lodestone-command` deliberately left undefined (issue #48).
//!
//! # What it is
//!
//! [`ServerCommands`] owns one [`CommandTree`] of built-in commands plus the side
//! tables that turn a successful parse into effects: executors, source-set
//! *modifiers*, the fork set, and each argument node's wire descriptor. One entry
//! point — [`ServerCommands::run`] — serves in-game chat, RCON and any future
//! command block.
//!
//! # What this replaced, and why it mattered
//!
//! The previous version of this module was an **island**. `mod commands;` was
//! declared and its tests were green, but `ServerCommands` had **zero references
//! anywhere outside this file**. Its own module doc claimed `crate::server`'s
//! `ChatCommand` arm consulted it; that claim was stale. The arm actually called a
//! hand-rolled `parse_gamemode_command` string split and then fell through to the
//! host [`CommandDispatch`](crate::CommandDispatch) sink — and since every real
//! constructor passes `CommandDispatch::none()`, **`/gamerule` typed by a player
//! did nothing at all.** RCON was worse: `rcon.rs` called the host sink only, so it
//! bypassed the built-ins entirely.
//!
//! That is the defect class `CLAUDE.md` calls the island, in its purest form: a
//! subsystem individually built, individually tested, reaching zero pixels because
//! nothing called it. The fix is not more tests on this module — a crate's own
//! suite is a closed loop — it is the single [`ServerCommands::run`] call site in
//! `dispatch_play_packet` and the one in `rcon.rs`.
//!
//! # One tree, three consumers
//!
//! | consumer | entry point |
//! |---|---|
//! | execution | [`ServerCommands::run`] → `parse_filtered` → the executor table |
//! | suggestion | [`ServerCommands::suggest`] → `suggest_filtered` |
//! | the wire | [`ServerCommands::wire_tree_for`] → [`wire::project_filtered`] |
//!
//! [`Registrar::arg`] installs a node's parser **and** records its wire identity
//! in the same call, from one [`lodestone_command_mc::McArg`] value, so the
//! transmitted tree cannot drift from the executing one. The failure mode that
//! guards against is specific: a client that autocompletes something the server
//! then rejects.
//!
//! **The wire tree is sent at join.** `crate::server`'s Play handoff calls
//! [`ServerCommands::wire_tree_for`] with the connection's resolved permission
//! level and hands the result to
//! [`ServerProtocol::encode_commands`](crate::ServerProtocol::encode_commands),
//! at vanilla's own position in the sequence — `PlayerList.placeNewPlayer` sends
//! it from `sendPlayerPermissionLevel`, after the abilities packet and before
//! `sendLevelInfo`. A protocol family with no `encode_commands` override sends
//! nothing, so the legacy families degrade silently rather than breaking.
//!
//! # The execution model
//!
//! Brigadier attaches a `Command<S>` to a node; [`CommandTree`] has no room for
//! one, by design. So executors live in a `NodeId`-keyed side table and dispatch
//! takes the **last** node of the parsed path — Brigadier's own rule, since the
//! deepest matched node owns the callback. `lodestone_ecs::commands` already chose
//! this shape for plugin commands and matching it is deliberate: two dispatchers
//! over one tree library should not disagree about where a callback lives.
//!
//! The *other* half of Brigadier — [`Registrar::modifier`] and the fork set — was
//! built ahead of `/execute` (`crate::commands::execute`, which now uses it), and
//! that is the reason a port was chosen over a signature-driven macro. A function
//! signature is a list; the vanilla command set is a graph. With the modifier
//! substrate in from day one, `/execute` landed purely additively and the
//! commands built before it needed no rework.
//!
//! # Effects, and why executors do not act directly
//!
//! See [`effect`]. The short version: a player's game mode is a **local variable**
//! in `dispatch_play_packet`, so a shared `Arc` executor physically cannot write
//! it. Executors emit typed [`Effect`]s; own-connection effects are applied inline
//! by the `ChatCommand` arm through `proto`, and cross-player effects travel a
//! directed per-uuid outbox on the shared [`crate::PlayerRegistry`].
//!
//! # Permissions are real now
//!
//! Every built-in root is gated at its vanilla level via
//! [`Registrar::require_level`], resolved against
//! [`crate::AccessLists::permission_level`] (0–4, matching 26.2's
//! `HasCommandLevel`). RCON's caller is level 4. `lodestone-command`'s permission
//! seam is a dotted *string* because that crate cannot know what a permission is,
//! so a level is encoded as `lodestone.level.N` and read back by
//! [`registrar::level_filter`] — the mapping lives in exactly those two places,
//! and an unrecognised permission string fails **closed**.
//!
//! # Precedence, and the one thing it must not do
//!
//! | outcome | meaning | `crate::server` does |
//! |---|---|---|
//! | `Some(outcome)` | a built-in root matched | send its lines, apply its effects; **do not** consult the host |
//! | `None` | nothing at the root matched | fall through to [`CommandDispatch`](crate::CommandDispatch) |
//!
//! `None` is keyed on [`ParseErrorKind::UnknownCommand`] specifically, which the
//! tree only produces when *no token matched at the root at all*. A built-in that
//! matched and then failed on its arguments answers with a refusal — so `/gamerule
//! nonsense` reports the parse error rather than silently becoming a plugin's
//! problem. Falling through on every error would tell the player the command does
//! not exist when only their argument was wrong.
//!
//! # How to change it
//!
//! * **Adding a built-in:** write a `register` function in its own submodule, add
//!   a call to [`ServerCommands::new`], and add two gates — a wire-parity gate
//!   against the captured fixture (`crates/protocol/v770/tests/builtin_command_parity.rs`)
//!   and one execution test driven through [`ServerCommands::run`], not through the
//!   executor directly, which cannot see a tree that was never wired.
//! * **Do not add a `&mut World`-shaped field to [`CommandWorld`].** The reason is
//!   [`crate::command`]'s module doc, unchanged.
//!
//! # Dependencies
//!
//! `lodestone-command` (the tree, zero dependencies of its own),
//! `lodestone-command-mc` (the Minecraft argument types), `lodestone-model` (the
//! version-free `ArgumentParser`), and [`crate::game_rules`].

/// `/op`/`/deop`/`/whitelist`. `cfg`-gated with `crate::access` (native
/// only) and `CommandWorld::access`, the field these built-ins read/write —
/// a browser singleplayer world has no access lists and no RCON console to
/// reach them through.
#[cfg(not(target_arch = "wasm32"))]
mod access_commands;
mod block_commands;
mod chat_commands;
mod clear;
mod default_gamemode;
mod difficulty;
pub mod effect;
/// `/effect give` and `/effect clear` — the producer that makes
/// [`crate::mob_effects`] reachable from a running game.
mod effect_command;
mod execute;
mod experience;
/// `/function` and `/reload` (issue #48's remainder — datapack functions and
/// function tags). See [`function_store`] for the loader itself.
mod function;
pub mod function_store;
mod gamemode;
mod gamerule;
mod give;
mod help;
mod kill;
mod nbt_data;
pub mod nbt_storage;
pub mod registrar;
mod scoreboard;
pub mod scoreboard_store;
mod seed;
pub mod source;
mod stopwatch;
pub mod stopwatch_store;
mod summon;
mod team;
pub mod team_store;
mod teleport;
mod time;
mod weather;
pub mod wire;
mod world_spawn_commands;
mod worldborder;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lodestone_command::{CommandTree, NodeId, ParseErrorKind};
use lodestone_model::command_tree::{
    CommandSuggestionEntry as WireCommandSuggestionEntry,
    CommandSuggestionsResponse as WireCommandSuggestionsResponse, CommandTree as WireCommandTree,
};

pub use effect::{DirectedEffect, Effect};
pub use registrar::{
    ArgKey, CommandOutcome, CommandResult, CommandWorld, Ctx, Registrar, WireDescriptor,
    level_filter, level_permission,
};
pub use source::{
    CommandSource, EntityAnchor, PlayerCandidate, SelectorError, SourceEntity, resolve_players,
};

use crate::command::{CommandCaller, CommandDispatch, CommandResponse};
use registrar::{Dispatcher, TreeId};

/// `minecraft:overworld`, the only dimension this server hosts.
///
/// A [`CommandSource`] needs one and there is nothing to read it from yet, so it
/// is named here rather than defaulted inside `CommandSource` — a source with a
/// silently-invented dimension is the kind of thing that looks right until the
/// Nether exists.
///
/// # Panics
///
/// Never: the input is a literal that satisfies `ResourceKey`'s character set.
#[must_use]
pub fn overworld_dimension() -> lodestone_model::ids::ResourceKey {
    "minecraft:overworld".parse().expect("`minecraft:overworld` is a valid resource key")
}

/// The server's built-in command tree plus everything hung off it.
///
/// Cheap to clone (one `Arc`), because every connection task needs one and
/// building the `/gamerule` subtree alone means allocating ~120 nodes — once per
/// server, not once per connection.
#[derive(Clone)]
pub struct ServerCommands {
    inner: Arc<Inner>,
}

struct Inner {
    tree: CommandTree,
    tree_id: TreeId,
    executors: HashMap<NodeId, registrar::ExecutorEntry>,
    modifiers: HashMap<NodeId, registrar::ModifierEntry>,
    forks: HashSet<NodeId>,
    wire: HashMap<NodeId, WireDescriptor>,
    /// Exactly the keys of `wire`, precomputed because [`Ctx`] needs the set on
    /// every argument read and building it per execution would be silly.
    argument_nodes: HashSet<NodeId>,
}

impl std::fmt::Debug for ServerCommands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerCommands")
            .field("nodes", &self.inner.tree.len())
            .field("executors", &self.inner.executors.len())
            .field("modifiers", &self.inner.modifiers.len())
            .finish()
    }
}

impl Default for ServerCommands {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerCommands {
    /// The tree with every built-in registered.
    #[must_use]
    pub fn new() -> Self {
        let mut registrar = Registrar::new();
        #[cfg(not(target_arch = "wasm32"))]
        access_commands::register(&mut registrar);
        gamerule::register(&mut registrar);
        gamemode::register(&mut registrar);
        give::register(&mut registrar);
        effect_command::register(&mut registrar);
        time::register(&mut registrar);
        difficulty::register(&mut registrar);
        seed::register(&mut registrar);
        world_spawn_commands::register(&mut registrar);
        kill::register(&mut registrar);
        experience::register(&mut registrar);
        clear::register(&mut registrar);
        block_commands::register(&mut registrar);
        chat_commands::register(&mut registrar);
        nbt_data::register(&mut registrar);
        teleport::register(&mut registrar);
        summon::register(&mut registrar);
        stopwatch::register(&mut registrar);
        weather::register(&mut registrar);
        default_gamemode::register(&mut registrar);
        help::register(&mut registrar);
        execute::register(&mut registrar);
        worldborder::register(&mut registrar);
        scoreboard::register(&mut registrar);
        team::register(&mut registrar);
        function::register(&mut registrar);
        Self::from_registrar(registrar)
    }

    /// Assemble from a [`Registrar`] the caller populated itself.
    ///
    /// The seam a gate uses to exercise the *substrate* rather than the shipped
    /// commands. The modifier/fork machinery had no production caller before
    /// `/execute` (`crate::commands::execute`) landed and used it — before
    /// that, something had to drive it here or it was an island of exactly
    /// the kind this module was. Also the shape a plugin-registered built-in
    /// would take.
    #[must_use]
    pub fn from_registrar(registrar: Registrar) -> Self {
        let parts = registrar.finish();
        let argument_nodes = parts.wire.keys().copied().collect();
        Self {
            inner: Arc::new(Inner {
                tree: parts.tree,
                tree_id: parts.tree_id,
                executors: parts.executors,
                modifiers: parts.modifiers,
                forks: parts.forks,
                wire: parts.wire,
                argument_nodes,
            }),
        }
    }

    /// The underlying tree, for a caller that wants to walk it.
    #[must_use]
    pub fn tree(&self) -> &CommandTree {
        &self.inner.tree
    }

    /// The version-free projection of the tree, for `minecraft:commands`.
    ///
    /// # Panics
    ///
    /// Never, for a tree built by [`Registrar`] — see [`wire::project`]'s own
    /// note. The `expect` is there so a future builder that *can* produce an
    /// inconsistent graph fails at its own gate rather than silently transmitting
    /// it.
    #[must_use]
    pub fn wire_tree(&self) -> WireCommandTree {
        wire::project(&self.inner.tree, &self.inner.wire)
            .expect("a Registrar-built tree always projects to a consistent index graph")
    }

    /// The projection a player at permission `level` is allowed to see — what the
    /// join sequence actually sends.
    ///
    /// Not [`Self::wire_tree`] with a filter bolted on afterwards: pruning has to
    /// happen *during* the walk, because dropping a node renumbers every index
    /// after it. See [`wire::project_filtered`] for the two vanilla behaviours it
    /// reproduces and why a denied node takes its subtree with it.
    ///
    /// # Panics
    ///
    /// Never, for a [`Registrar`]-built tree — same terms as [`Self::wire_tree`].
    #[must_use]
    pub fn wire_tree_for(&self, level: u8) -> WireCommandTree {
        wire::project_filtered(&self.inner.tree, &self.inner.wire, &level_filter(level))
            .expect("a Registrar-built tree always projects to a consistent index graph")
    }

    /// Completions for a partially-typed command, filtered to what `level` may
    /// see.
    ///
    /// Gated *silently*: a node this level cannot use is simply absent, which is
    /// what vanilla achieves by never sending it. See
    /// `lodestone_command::filter` for why suggestion and execution differ here.
    #[must_use]
    pub fn suggest(&self, partial: &str, level: u8) -> Vec<String> {
        self.inner.tree.suggest_filtered(partial, &level_filter(level))
    }

    /// Builds a full `minecraft:command_suggestions` response for a
    /// `ServerBound::CommandSuggestion` request's raw wire input.
    ///
    /// `raw` is the **whole typed line, including the leading `/`** — the wire
    /// format `ServerBound::CommandSuggestion`'s own doc describes — and
    /// exactly one leading `/` is stripped here before consulting
    /// [`Self::suggest`], mirroring vanilla's
    /// `ServerGamePacketListenerImpl.handleCustomCommandSuggestions`, which
    /// skips exactly one leading `/` off its `StringReader` before parsing.
    ///
    /// `start`/`length` name the byte range of `raw` the suggestions replace —
    /// the token currently being typed, i.e. everything after the last space
    /// (or after the slash, if there is none) — derived from the same
    /// token-boundary rule [`lodestone_command::CommandTree::suggest_filtered`]
    /// applies internally (`crates/lodestone-command/src/suggest.rs`), offset
    /// back by the one byte the leading slash occupies in `raw` but not in the
    /// stripped text handed to `suggest`.
    ///
    /// Split out from the connection loop specifically so this arithmetic is
    /// unit-testable without a live connection — see this module's tests.
    #[must_use]
    pub fn suggest_response(
        &self,
        id: i32,
        raw: &str,
        level: u8,
    ) -> WireCommandSuggestionsResponse {
        let stripped = raw.strip_prefix('/').unwrap_or(raw);
        let token_start = stripped.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let start = (raw.len() - stripped.len()) + token_start;
        let suggestions = self
            .suggest(stripped, level)
            .into_iter()
            .map(|text| WireCommandSuggestionEntry { text, tooltip: None })
            .collect();
        WireCommandSuggestionsResponse {
            id,
            start: start as i32,
            length: (raw.len() - start) as i32,
            suggestions,
        }
    }

    /// Runs `command` (no leading `/`) if a built-in root matches it.
    ///
    /// `None` means no built-in root matched and the caller should fall through
    /// to the host sink — see this module's precedence table for why that is keyed
    /// on [`ParseErrorKind::UnknownCommand`] and not on "any error".
    #[must_use]
    pub fn run(
        &self,
        world: &CommandWorld<'_>,
        source: &CommandSource,
        command: &str,
    ) -> Option<CommandOutcome> {
        self.run_inner(world, source, command, None, None)
    }

    /// Run a built-in command, allowing only a terminal `/execute ... run`
    /// whose built-in root is unknown to call the host's contextual seam.
    ///
    /// Direct roots keep [`Self::run`]'s existing caller/permission behaviour;
    /// this method is intentionally opt-in so RCON and command blocks retain
    /// their current built-in-only policy until a real host supplies a safe
    /// contextual adapter for those ingress paths.
    #[must_use]
    pub fn run_with_contextual_dispatch(
        &self,
        world: &CommandWorld<'_>,
        source: &CommandSource,
        command: &str,
        dispatch: &CommandDispatch,
        caller: &CommandCaller,
    ) -> Option<CommandOutcome> {
        self.run_inner(world, source, command, Some(dispatch), Some(caller))
    }

    fn run_inner(
        &self,
        world: &CommandWorld<'_>,
        source: &CommandSource,
        command: &str,
        contextual_dispatch: Option<&CommandDispatch>,
        contextual_caller: Option<&CommandCaller>,
    ) -> Option<CommandOutcome> {
        let filter = level_filter(source.permission_level);
        let parsed = match self.inner.tree.parse_filtered(command, &filter) {
            Ok(parsed) => parsed,
            // Nothing at the root matched: not ours.
            Err(e) if e.kind == ParseErrorKind::UnknownCommand => return None,
            // A built-in matched and then failed. Ours, and the player is told
            // what actually went wrong.
            Err(e) => {
                return Some(CommandOutcome {
                    response: CommandResponse::refused(e.to_string()),
                    effects: Vec::new(),
                });
            }
        };
        let dispatcher = Dispatcher {
            tree: &self.inner.tree,
            tree_id: self.inner.tree_id,
            executors: &self.inner.executors,
            modifiers: &self.inner.modifiers,
            forks: &self.inner.forks,
            argument_nodes: &self.inner.argument_nodes,
            commands: self,
        };
        Some(dispatcher.dispatch(
            world,
            source.clone(),
            &parsed,
            contextual_dispatch,
            contextual_caller,
        ))
    }
}

#[cfg(test)]
mod suggest_response_tests {
    use super::*;

    /// A discriminating input, not the plausible round number: `start`/`length`
    /// land somewhere other than 0 or 1, and the candidate set has exactly one
    /// member — `/gamemode`'s argument-type suggester
    /// (`lodestone_command_mc::GameModeArg`) returns all four mode names
    /// unconditionally, so only `suggest_filtered`'s own prefix filter proves
    /// this reached the right node and used the right partial.
    #[test]
    fn a_partial_argument_token_gets_the_right_byte_range_and_the_filtered_candidate() {
        let commands = ServerCommands::new();
        let raw = "/gamemode surviv";
        let response = commands.suggest_response(42, raw, 4);
        assert_eq!(response.id, 42, "the transaction id must echo verbatim");
        // "/gamemode " is 10 bytes (1 slash + 8 + 1 space); "surviv" starts there.
        assert_eq!(response.start, 10);
        assert_eq!(response.length, 6, "the length of the partial token \"surviv\", not of the whole line");
        assert_eq!(
            response.suggestions.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["survival"],
            "\"surviv\" must filter out creative/adventure/spectator"
        );
        assert!(
            response.suggestions.iter().all(|e| e.tooltip.is_none()),
            "this server never attaches a suggestion tooltip"
        );
    }

    /// The permission control for the test above: `/gamemode` requires level 2
    /// (`gamemode::GAMEMODE_LEVEL`), so a level-0 source must get no
    /// suggestions at all for the identical input — proving the byte range
    /// above came from real permission-gated tree traversal, not from string
    /// splitting alone. Mirrors `wire::project_filtered`'s own denied-subtree
    /// behaviour on the suggestion axis.
    #[test]
    fn an_unprivileged_source_gets_no_suggestions_under_a_gated_root() {
        let commands = ServerCommands::new();
        let response = commands.suggest_response(1, "/gamemode surviv", 0);
        assert!(
            response.suggestions.is_empty(),
            "a level-0 source must not see /gamemode's argument suggestions: {:?}",
            response.suggestions
        );
        // The control: the identical call at a sufficient level is non-empty,
        // so the emptiness above is the permission gate firing and not some
        // other reason (a typo in the literal, an empty tree, ...).
        let privileged = commands.suggest_response(1, "/gamemode surviv", 4);
        assert!(!privileged.suggestions.is_empty());
    }

    /// No space in the input at all: the whole stripped text is the partial
    /// token, `start` is exactly `1` (right after the slash) by construction,
    /// and two *different* built-in roots share the "gam" prefix
    /// (`gamemode`, `gamerule` — `defaultgamemode` does not, it starts with
    /// `d`), so this also exercises multi-candidate, sorted output rather than
    /// a single-item list that a broken sort could pass by accident.
    #[test]
    fn a_root_level_partial_with_no_space_matches_every_prefixed_literal() {
        let commands = ServerCommands::new();
        let response = commands.suggest_response(7, "/gam", 4);
        assert_eq!(response.start, 1);
        assert_eq!(response.length, 3, "the length of \"gam\"");
        assert_eq!(
            response.suggestions.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["gamemode", "gamerule"],
            "case-insensitively sorted, per CommandTree::suggest_filtered's own doc"
        );
    }
}
