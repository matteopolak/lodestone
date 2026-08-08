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
//! | the wire | [`ServerCommands::wire_tree`] → [`wire::project`] |
//!
//! [`Registrar::arg`] installs a node's parser **and** records its wire identity
//! in the same call, from one [`lodestone_command_mc::McArg`] value, so the
//! transmitted tree cannot drift from the executing one. The failure mode that
//! guards against is specific: a client that autocompletes something the server
//! then rejects.
//!
//! **Nothing sends the wire tree yet.** No protocol family in this workspace has a
//! `COMMANDS` (id 16) *encode* arm — that is a later unit. The projection exists
//! and is gated against a real 26.2 server's captured tree; autocomplete against
//! the server's own commands does not work end to end.
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
//! The *other* half of Brigadier — [`Registrar::modifier`] and the fork set — is
//! built now even though `/execute` is a later unit, and that is the reason a port
//! was chosen over a signature-driven macro. A function signature is a list; the
//! vanilla command set is a graph. With the modifier substrate in from day one,
//! `/execute` is purely additive and the commands built before it need no rework.
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

pub mod effect;
mod gamemode;
mod gamerule;
mod give;
pub mod registrar;
pub mod source;
pub mod wire;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lodestone_command::{CommandTree, NodeId, ParseErrorKind};
use lodestone_model::command_tree::CommandTree as WireCommandTree;

pub use effect::{DirectedEffect, Effect};
pub use registrar::{
    ArgKey, CommandOutcome, CommandResult, CommandWorld, Ctx, Registrar, WireDescriptor,
    level_filter, level_permission,
};
pub use source::{
    CommandSource, EntityAnchor, PlayerCandidate, SelectorError, SourceEntity, resolve_players,
};

use crate::command::CommandResponse;
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
        gamerule::register(&mut registrar);
        gamemode::register(&mut registrar);
        give::register(&mut registrar);
        Self::from_registrar(registrar)
    }

    /// Assemble from a [`Registrar`] the caller populated itself.
    ///
    /// The seam a gate uses to exercise the *substrate* rather than the shipped
    /// commands — the modifier/fork machinery has no production caller until
    /// `/execute` lands, and something has to drive it or it is an island of
    /// exactly the kind this module was. Also the shape a plugin-registered
    /// built-in would take.
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
        };
        Some(dispatcher.dispatch(world, source.clone(), &parsed))
    }
}
