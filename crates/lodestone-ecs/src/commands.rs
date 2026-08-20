//! Plugin command registration and dispatch: a
//! [`CommandRegistry`] resource a plugin populates in its `Plugin::build`, an
//! argument tree per command, per-node permission gating, and tab completion.
//!
//! ## What it is
//!
//! A third-party `bevy_app::Plugin` builds a [`PluginCommand`] — root literal,
//! description, aliases, a permission gating the whole tree, per-node
//! permissions, argument types, and a handler per executable node — and hands
//! it to [`CommandRegistry::register`]. [`dispatch`] then resolves an input
//! string against the registry, gates it through
//! [`crate::permissions::Permissions`], and runs the handler with `&mut World`.
//! [`suggest`] is the tab-completion half.
//!
//! ```no_run
//! use std::sync::Arc;
//! use bevy_app::{App, Plugin};
//! use lodestone_command::IntegerArgument;
//! use lodestone_ecs::commands::{CommandOutcome, CommandRegistry, PluginCommand, PluginCommandsPlugin};
//!
//! struct MyPlugin;
//!
//! impl Plugin for MyPlugin {
//!     fn build(&self, app: &mut App) {
//!         app.add_plugins(PluginCommandsPlugin);
//!
//!         let mut command = PluginCommand::new("myplugin");
//!         command.description("does a thing");
//!         command.alias("mp");
//!         command.permission("myplugin.use");
//!
//!         let root = command.root();
//!         let admin = command.literal(root, "admin");
//!         command.require_permission(admin, "myplugin.admin");
//!
//!         let amount = command.argument(admin, "amount", Arc::new(IntegerArgument::bounded(1, 64)));
//!         command.on_execute(amount, |invocation| {
//!             let n = invocation.integer("amount").unwrap_or(0);
//!             CommandOutcome::Success(n)
//!         });
//!
//!         app.world_mut()
//!             .resource_mut::<CommandRegistry>()
//!             .register(command)
//!             .expect("root literal `myplugin` is not already taken");
//!     }
//! }
//! ```
//!
//! ## Why the registry is here and not in `lodestone-server`
//!
//! The instinct that the registry should be "server-side, since that is where
//! command *execution* semantics live" is right about semantics and wrong about
//! the crate, for a reason that instinct could not have known:
//!
//! - **`lodestone-server` deliberately does not depend on `lodestone-ecs`**, and
//!   says so in its own manifest ("Deliberately NOT `lodestone-ecs`, despite
//!   `docs/server-ecs.md`'s title"). It links neither bevy nor this crate.
//! - **There is no plugin API on the server at all.** Every plugin seam in this
//!   workspace is a `bevy_app::Plugin` added to `lodestone_app::client_app()`
//!   (`docs/plugin-registration.md`). A registry inside `lodestone-server`
//!   would be unreachable by every plugin that can currently exist.
//!
//! So the registry lives where the plugin API lives. It is a plain
//! `Resource` with no client-specific state, so a future server `App` — or a
//! future `/execute`-context dispatcher — inserts the same resource and calls
//! the same [`dispatch`]. Nothing here knows or cares which side it is on.
//!
//! ## Wire reachability and remaining boundary
//!
//! Native integrated singleplayer now installs a shell-owned `CommandSink` on
//! its local duplex connection. `CHAT_COMMAND` reaches the server's built-ins
//! first; an unknown direct root calls [`dispatch`] as the authenticated player,
//! while a terminal `/execute ... run <plugin>` carries its rewritten entity,
//! position, rotation, dimension, anchor and permission level into
//! [`CommandSource::contextual`]. The v770 wire gate drives both forms through
//! a real client/server pair and verifies `store result` and `store success`.
//!
//! This is intentionally **not** a general network-plugin host. Open-to-LAN
//! peers do not receive the client registry, and RCON, console and command-block
//! paths remain built-in-only. Those boundaries avoid handing an arbitrary
//! remote caller the shell's ECS handle.
//!
//! Clientbound command-tree encoding remains absent: no protocol family emits
//! `COMMANDS` (clientbound id 16). [`command_tree_for`] is ready for that arm
//! and applies the same permission pruning vanilla's `fillUsableCommands` does.
//!
//! ## How to change it
//!
//! - **A handler is stored per [`lodestone_command::NodeId`] in a side table on
//!   [`RegisteredCommand`], not on the tree node.** `lodestone-command` has no
//!   execution model on purpose (a future `/execute`-context dispatcher will want
//!   to define one differently), so keep handlers out of it. The cost is that
//!   `on_execute` must set the
//!   node's `executable` flag *and* insert into the table; [`PluginCommand::on_execute`]
//!   does both and is the only way to do either.
//! - **Dispatch resolves the handler by walking the parsed path backwards.** A
//!   parse can legitimately end on a node with no handler (an intermediate
//!   literal marked executable by nothing); the nearest ancestor with a handler
//!   wins, which is how `/myplugin admin` falls back to `admin`'s handler when
//!   `reload` was not typed. If you change this, note that
//!   `lodestone_command::CommandTree::parse` already rejects a path ending on a
//!   non-`executable` node, so the backwards walk only ever skips nodes that
//!   were executable-but-handlerless — which is a registration bug, and
//!   [`CommandDispatchError::NoHandler`] reports it rather than silently
//!   succeeding.
//! - **Aliases are rewritten to the canonical root before parsing**, because the
//!   tree contains only the canonical literal. This is what Bukkit does. If you
//!   add a second rewriting rule, do it in [`canonicalize`] — a second site is
//!   how alias and permission resolution start disagreeing about which command
//!   is running.
//! - **Adding a resolution step to permission gating means editing
//!   `crate::permissions`, not here.** This module only supplies the
//!   [`lodestone_command::PermissionFilter`] closure.
//!
//! ## Configuration
//!
//! None. [`PluginCommandsPlugin`] inserts [`CommandRegistry`],
//! [`crate::permissions::Permissions`] and [`PlayerDirectory`], and registers
//! the one system that keeps the directory fresh.
//!
//! ## Dependencies
//!
//! `lodestone-command` for the tree, parser and suggester (this is that crate's
//! first consumer — see its crate doc, which used to declare itself an island).
//! `crate::permissions` for resolution. `lodestone-game`'s tab list, read
//! through [`crate::session::SessionTabList`], for live player-name
//! suggestions.

use std::collections::HashMap;
use std::sync::Arc;

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{IntoScheduleConfigs, Res, Resource};
use bevy_ecs::world::World;
use lodestone_command::{
    ArgumentType, ChoicesArgument, CommandTree, NodeId, ParseError, ParseErrorKind, ParsedCommand,
    ParsedValue,
};
use lodestone_model::{ResourceKey, Rotation, Vec3};
use parking_lot::RwLock;

use crate::permissions::{PermissionSubject, Permissions};
use crate::schedules::GameTick;
use crate::sets::TickSet;

/// Who is running a command.
///
/// # `/execute` context
///
/// Direct roots retain `execution: None`, preserving the original identity-only
/// API. A server host may set [`Self::execution`] for a terminal `/execute ...
/// run`; registry permission resolution intentionally continues to use
/// [`Self::subject`], which the host derives from the rewritten executor
/// identity.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSource {
    pub subject: PermissionSubject,
    /// Display name, for messages back to the sender.
    pub name: String,
    /// The rewritten server context, absent for a direct plugin root.
    pub execution: Option<CommandExecutionContext>,
}

/// The entity identity in a contextual plugin invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntity {
    pub uuid: uuid::Uuid,
    pub entity_id: i32,
    pub username: String,
}

/// Which point local coordinates resolve from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAnchor {
    Feet,
    Eyes,
}

/// Value-only `/execute` context exposed to a plugin handler.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandExecutionContext {
    pub entity: Option<CommandEntity>,
    pub position: Vec3,
    pub rotation: Rotation,
    pub dimension: ResourceKey,
    pub anchor: CommandAnchor,
    pub permission_level: u8,
}

impl CommandSource {
    pub fn player(id: uuid::Uuid, name: impl Into<String>) -> Self {
        Self {
            subject: PermissionSubject::Player(id),
            name: name.into(),
            execution: None,
        }
    }

    /// The server console — holds every permission (see
    /// [`PermissionSubject::Console`]).
    pub fn console() -> Self {
        Self {
            subject: PermissionSubject::Console,
            name: "Console".to_string(),
            execution: None,
        }
    }

    /// A host-provided source for a terminal `/execute ... run`.
    #[must_use]
    pub fn contextual(
        subject: PermissionSubject,
        name: impl Into<String>,
        execution: CommandExecutionContext,
    ) -> Self {
        Self { subject, name: name.into(), execution: Some(execution) }
    }
}

/// What a handler reports back. Mirrors Brigadier's `int` result, which vanilla
/// uses for `/execute store` and command-block success counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Succeeded, with Brigadier's integer result.
    Success(i32),
    /// Failed for a reason the sender should be told. Not an *error* in the
    /// dispatch sense — the command ran and decided it could not do the thing.
    Failure(String),
}

impl CommandOutcome {
    /// Brigadier's conventional "did one thing successfully".
    pub fn ok() -> Self {
        Self::Success(1)
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }
}

/// Everything a handler gets: the world, who ran it, and the parsed arguments.
pub struct CommandInvocation<'w> {
    pub world: &'w mut World,
    pub source: CommandSource,
    /// The full parsed path and its arguments.
    pub parsed: ParsedCommand,
    /// The canonical input (alias already rewritten, leading `/` stripped).
    pub input: String,
}

impl std::fmt::Debug for CommandInvocation<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `world` is deliberately omitted: `World` is not `Debug`, and printing
        // it would be useless anyway.
        f.debug_struct("CommandInvocation")
            .field("source", &self.source)
            .field("parsed", &self.parsed)
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl CommandInvocation<'_> {
    /// A named argument, as parsed.
    pub fn argument(&self, name: &str) -> Option<&ParsedValue> {
        self.parsed.argument(name)
    }

    pub fn integer(&self, name: &str) -> Option<i32> {
        match self.parsed.argument(name)? {
            ParsedValue::Integer(v) => Some(*v),
            _ => None,
        }
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        match self.parsed.argument(name)? {
            ParsedValue::String(v) | ParsedValue::Custom(v) => Some(v.as_str()),
            _ => None,
        }
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.parsed.argument(name)? {
            ParsedValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn double(&self, name: &str) -> Option<f64> {
        match self.parsed.argument(name)? {
            ParsedValue::Double(v) => Some(*v),
            ParsedValue::Float(v) => Some(f64::from(*v)),
            _ => None,
        }
    }
}

/// A handler for one executable node.
pub type CommandHandler = Arc<dyn Fn(&mut CommandInvocation<'_>) -> CommandOutcome + Send + Sync>;

/// A command under construction, before [`CommandRegistry::register`] freezes
/// it.
///
/// The builder is arena-shaped rather than chained, mirroring
/// `lodestone_command::CommandTree` underneath: you hold [`NodeId`]s and hang
/// children off them. Brigadier's fluent `then(literal(..).then(..))` style does
/// not translate to Rust without either a macro or pervasive `Box<dyn>`
/// gymnastics, and the arena form is what lets a plugin build a tree in a loop.
pub struct PluginCommand {
    name: String,
    description: String,
    aliases: Vec<String>,
    tree: CommandTree,
    /// The `NodeId` of the root literal (the command name itself), not the
    /// tree's synthetic root.
    root_literal: NodeId,
    handlers: HashMap<NodeId, CommandHandler>,
}

impl std::fmt::Debug for PluginCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("aliases", &self.aliases)
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl PluginCommand {
    /// Start a command whose root literal is `name`.
    ///
    /// # Panics
    ///
    /// If `name` contains a space. A literal with a space can never be matched —
    /// `lodestone_command`'s tokenizer splits on exactly `' '` — so this is a
    /// programming error rather than a runtime condition, and failing at
    /// construction is far cheaper to diagnose than a command that silently
    /// never matches.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            !name.contains(' '),
            "command name must not contain a space: {name:?}"
        );
        let mut tree = CommandTree::new();
        let tree_root = tree.root();
        let root_literal = tree.add_literal(tree_root, &name.to_lowercase());
        Self {
            name,
            description: String::new(),
            aliases: Vec::new(),
            tree,
            root_literal,
            handlers: HashMap::new(),
        }
    }

    /// The root literal node — the command name. Hang subcommands off this.
    pub fn root(&self) -> NodeId {
        self.root_literal
    }

    pub fn description(&mut self, description: impl Into<String>) -> &mut Self {
        self.description = description.into();
        self
    }

    /// An alternative name. Rewritten to the canonical root before parsing, so
    /// the tree never contains the alias — Bukkit's own behaviour.
    pub fn alias(&mut self, alias: impl Into<String>) -> &mut Self {
        self.aliases.push(alias.into().to_lowercase());
        self
    }

    /// The permission gating the **whole** command — "a permission node
    /// gating the whole tree". Applied to the root literal, so subtree pruning
    /// makes the entire command invisible and unusable in one step.
    pub fn permission(&mut self, permission: impl Into<String>) -> &mut Self {
        self.tree.require_permission(self.root_literal, permission);
        self
    }

    /// Add a literal subcommand.
    pub fn literal(&mut self, parent: NodeId, name: &str) -> NodeId {
        self.tree.add_literal(parent, name)
    }

    /// Add an argument slot.
    pub fn argument(
        &mut self,
        parent: NodeId,
        name: &str,
        argument_type: Arc<dyn ArgumentType>,
    ) -> NodeId {
        self.tree.add_argument(parent, name, argument_type)
    }

    /// Gate one node, and everything under it.
    pub fn require_permission(&mut self, node: NodeId, permission: impl Into<String>) -> &mut Self {
        self.tree.require_permission(node, permission);
        self
    }

    /// Redirect this node to continue from `target`'s children — Brigadier's
    /// `redirect`, e.g. the `/execute … run <command>` shape.
    pub fn redirect(&mut self, node: NodeId, target: NodeId) -> &mut Self {
        self.tree.set_redirect(node, target);
        self
    }

    /// Make `node` executable and attach its handler.
    ///
    /// The **only** way to do either: a node marked executable with no handler
    /// parses successfully and then fails dispatch with
    /// [`CommandDispatchError::NoHandler`], and a handler on a non-executable
    /// node is never reached. Coupling them here makes both impossible.
    pub fn on_execute<F>(&mut self, node: NodeId, handler: F) -> &mut Self
    where
        F: Fn(&mut CommandInvocation<'_>) -> CommandOutcome + Send + Sync + 'static,
    {
        self.tree.set_executable(node, true);
        self.handlers.insert(node, Arc::new(handler));
        self
    }
}

/// A frozen, registered command.
pub struct RegisteredCommand {
    pub name: String,
    pub description: String,
    pub aliases: Vec<String>,
    pub tree: CommandTree,
    handlers: HashMap<NodeId, CommandHandler>,
}

impl RegisteredCommand {
    /// The permission gating the whole command, if any.
    pub fn permission(&self) -> Option<&str> {
        self.tree.get(self.root_literal()).permission()
    }

    /// The root literal's id. Recomputed rather than stored because
    /// `CommandTree::new` always makes it node 1 (node 0 is the synthetic
    /// root) — asserted by `root_literal_is_always_node_one`, so this is a
    /// checked invariant rather than an assumption.
    fn root_literal(&self) -> NodeId {
        *self
            .tree
            .get(self.tree.root())
            .children()
            .first()
            .expect("a registered command always has exactly one root literal")
    }
}

impl std::fmt::Debug for RegisteredCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("aliases", &self.aliases)
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

/// Why a registration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRegisterError {
    /// The root literal, or one of the aliases, is already taken.
    ///
    /// Refused rather than silently overwritten: two plugins claiming
    /// `/warp` and the second winning is the kind of conflict that presents as
    /// "my plugin stopped working" with nothing in any log.
    NameTaken { name: String },
    /// No node in the tree has a handler, so the command could never do
    /// anything. Caught at registration because it is always a mistake and is
    /// otherwise invisible until someone types the command.
    NoHandlers { name: String },
}

impl std::fmt::Display for CommandRegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameTaken { name } => write!(f, "command name or alias '{name}' is already registered"),
            Self::NoHandlers { name } => write!(f, "command '{name}' has no executable node with a handler"),
        }
    }
}

impl std::error::Error for CommandRegisterError {}

/// Why a dispatch failed before or instead of running a handler.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandDispatchError {
    /// The input was empty once the leading `/` was stripped.
    Empty,
    /// No registered command owns that root literal. Vanilla's "Unknown or
    /// incomplete command".
    UnknownCommand { name: String },
    /// The command was found and the input did not parse. Carries the
    /// underlying error, which is [`ParseErrorKind::NoPermission`] when the
    /// reason was a permission gate.
    Parse(ParseError),
    /// A node parsed as executable but had no handler — a registration bug that
    /// [`CommandRegistry::register`] would normally have caught, reachable only
    /// if `set_executable` was called outside [`PluginCommand::on_execute`].
    NoHandler { name: String },
    /// [`CommandRegistry`] or [`Permissions`] is missing from the world.
    ///
    /// A hard error rather than an ungated fallback **on purpose**: a missing
    /// permission resource must never mean "allow everything". That failure
    /// mode is silent, security-shaped, and would only be noticed by someone
    /// who did not have the permission they just used.
    NotInstalled { missing: &'static str },
}

impl CommandDispatchError {
    /// The message to show the sender, in vanilla's register.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "Unknown or incomplete command".to_string(),
            Self::UnknownCommand { .. } => "Unknown or incomplete command".to_string(),
            Self::Parse(error) => error.kind.to_string(),
            Self::NoHandler { name } => format!("Command '{name}' is misconfigured"),
            Self::NotInstalled { missing } => format!("Commands are unavailable ({missing} missing)"),
        }
    }

    /// Was this a permission refusal? Distinguishes a permission gate from
    /// every other parse failure, for a caller that wants to log or count them.
    pub fn is_permission_denied(&self) -> bool {
        matches!(
            self,
            Self::Parse(ParseError {
                kind: ParseErrorKind::NoPermission { .. },
                ..
            })
        )
    }
}

impl std::fmt::Display for CommandDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl std::error::Error for CommandDispatchError {}

/// The registry a plugin populates.
#[derive(Resource, Default)]
pub struct CommandRegistry {
    /// Canonical name (lower-cased) → command.
    commands: HashMap<String, Arc<RegisteredCommand>>,
    /// Alias (lower-cased) → canonical name. Kept separate from `commands` so
    /// an alias can never shadow a real command's own entry.
    aliases: HashMap<String, String>,
    /// Registration order, so listings are stable rather than hash-ordered.
    order: Vec<String>,
}

impl std::fmt::Debug for CommandRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandRegistry")
            .field("commands", &self.order)
            .field("aliases", &self.aliases)
            .finish()
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Freeze and store a command.
    pub fn register(&mut self, command: PluginCommand) -> Result<(), CommandRegisterError> {
        let canonical = command.name.to_lowercase();

        if command.handlers.is_empty() {
            return Err(CommandRegisterError::NoHandlers {
                name: command.name.clone(),
            });
        }
        if self.is_taken(&canonical) {
            return Err(CommandRegisterError::NameTaken { name: canonical });
        }
        for alias in &command.aliases {
            if self.is_taken(alias) {
                return Err(CommandRegisterError::NameTaken { name: alias.clone() });
            }
        }

        for alias in &command.aliases {
            self.aliases.insert(alias.clone(), canonical.clone());
        }
        self.order.push(canonical.clone());
        self.commands.insert(
            canonical,
            Arc::new(RegisteredCommand {
                name: command.name,
                description: command.description,
                aliases: command.aliases,
                tree: command.tree,
                handlers: command.handlers,
            }),
        );
        Ok(())
    }

    fn is_taken(&self, name: &str) -> bool {
        self.commands.contains_key(name) || self.aliases.contains_key(name)
    }

    /// Look up by canonical name or alias.
    pub fn get(&self, name: &str) -> Option<&Arc<RegisteredCommand>> {
        let name = name.to_lowercase();
        match self.commands.get(&name) {
            Some(command) => Some(command),
            None => self.aliases.get(&name).and_then(|c| self.commands.get(c)),
        }
    }

    /// Canonical names, in registration order.
    pub fn names(&self) -> &[String] {
        &self.order
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Live player names, for [`player_argument`]'s suggestions.
///
/// An `Arc<RwLock<Vec<String>>>` rather than a plain `Vec` because the
/// [`lodestone_command::SuggestionProvider`] inside an argument type is built
/// once, at plugin-build time, and must keep seeing fresh data afterwards —
/// it cannot borrow the `World` at suggestion time. The system
/// [`sync_player_directory`] refreshes it once per tick from
/// [`crate::session::SessionTabList`].
///
/// The lock is held for exactly one `clone()` of a small `Vec` on each side, so
/// it never spans a frame — the discipline `docs/world-unification.md`
/// describes.
#[derive(Resource, Clone, Default)]
pub struct PlayerDirectory(pub Arc<RwLock<Vec<String>>>);

impl PlayerDirectory {
    pub fn names(&self) -> Vec<String> {
        self.0.read().clone()
    }

    pub fn set(&self, names: Vec<String>) {
        *self.0.write() = names;
    }
}

impl std::fmt::Debug for PlayerDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PlayerDirectory").field(&self.names()).finish()
    }
}

/// An argument that parses a player name and suggests whoever is online — a
/// "player name" argument shape.
///
/// **Lenient**, not strict: vanilla accepts an offline player's name in most
/// commands, and the suggestion list is only who happens to be in the tab list
/// right now. A strict version would reject valid input the moment a player
/// logged out mid-typing.
pub fn player_argument(directory: &PlayerDirectory) -> Arc<dyn ArgumentType> {
    let names = directory.0.clone();
    Arc::new(ChoicesArgument::lenient(Arc::new(move || {
        names.read().clone()
    })))
}

/// An argument over a fixed, closed set — a "block id" argument shape.
///
/// **Strict**: a value outside the set fails at parse rather than reaching the
/// handler. For a closed registry that is what you want; a typo'd block id
/// arriving at the handler as a `String` would look like a handler bug.
pub fn choice_argument(choices: impl IntoIterator<Item = impl Into<String>>) -> Arc<dyn ArgumentType> {
    let choices: Vec<String> = choices.into_iter().map(Into::into).collect();
    Arc::new(ChoicesArgument::fixed(choices, true))
}

/// Strip a leading `/`, then rewrite an alias to its canonical root.
///
/// The single place input is rewritten (see the module doc's "how to change
/// it"). Returns the canonical input and the resolved command, or `None` if no
/// command owns the first token.
///
/// # Only *leading* whitespace is trimmed, and that is load-bearing
///
/// A trailing space is **significant to suggestion**: it is the entire signal
/// that the current token is finished and the *next* one is being completed.
/// `"warp "` must suggest `warp`'s children; `"warp"` must suggest command names
/// beginning `warp`. An earlier version of this function called `.trim()`, which
/// collapsed the two — every tree-level completion silently returned the command
/// name back to the caller instead of its subcommands, and only the two
/// suggestion tests in `tests/plugin_command_registry.rs` caught it. Parsing
/// tolerates a trailing space either way, so `dispatch` is unaffected and does
/// not need it trimmed.
fn canonicalize(registry: &CommandRegistry, input: &str) -> Option<(Arc<RegisteredCommand>, String)> {
    let input = input.strip_prefix('/').unwrap_or(input).trim_start();
    if input.is_empty() {
        return None;
    }
    let (head, rest) = match input.split_once(' ') {
        Some((head, rest)) => (head, Some(rest)),
        None => (input, None),
    };
    let command = registry.get(head)?.clone();
    let canonical_head = command.name.to_lowercase();
    let canonical = match rest {
        Some(rest) => format!("{canonical_head} {rest}"),
        None => canonical_head,
    };
    Some((command, canonical))
}

/// Resolve, gate and run one command against the registry.
///
/// The shell's local `CommandSink` adapter calls this after the server's
/// `CHAT_COMMAND` arm resolves a plugin terminal (directly or through
/// `/execute ... run`). The server stays ECS-free; this crate only receives the
/// version-free source and command string across that host seam.
pub fn dispatch(
    world: &mut World,
    source: &CommandSource,
    input: &str,
) -> Result<CommandOutcome, CommandDispatchError> {
    // Resolve everything that needs to read the world *before* taking `&mut
    // World` for the handler. Two shared borrows coexist fine; the block ends
    // before the handler runs, so a handler is free to mutate anything —
    // including the registry.
    let (command, canonical, parsed) = {
        let registry = world
            .get_resource::<CommandRegistry>()
            .ok_or(CommandDispatchError::NotInstalled {
                missing: "CommandRegistry",
            })?;
        let permissions = world
            .get_resource::<Permissions>()
            .ok_or(CommandDispatchError::NotInstalled {
                missing: "Permissions",
            })?;

        let trimmed = input.strip_prefix('/').unwrap_or(input).trim();
        if trimmed.is_empty() {
            return Err(CommandDispatchError::Empty);
        }
        let head = trimmed.split_once(' ').map_or(trimmed, |(h, _)| h).to_string();

        let (command, canonical) = canonicalize(registry, input)
            .ok_or(CommandDispatchError::UnknownCommand { name: head })?;

        let subject = source.subject;
        let filter = move |node: &str| permissions.has(subject, node);
        let parsed = command
            .tree
            .parse_filtered(&canonical, &filter)
            .map_err(CommandDispatchError::Parse)?;

        (command, canonical, parsed)
    };

    // The deepest node on the parsed path that has a handler. See the module
    // doc for why this walks backwards rather than requiring the last node.
    let handler = parsed
        .nodes
        .iter()
        .rev()
        .find_map(|id| command.handlers.get(id))
        .cloned()
        .ok_or_else(|| CommandDispatchError::NoHandler {
            name: command.name.clone(),
        })?;

    let mut invocation = CommandInvocation {
        world,
        source: source.clone(),
        parsed,
        input: canonical,
    };
    Ok(handler(&mut invocation))
}

/// Tab completions for a partially-typed command, gated per node (the
/// permission-gate's suggestion half).
///
/// Takes `&World` rather than `&mut World`: suggesting never runs a handler.
/// A node the subject cannot use is **silently** absent, together with its
/// subtree — see `lodestone_command::filter` for why this differs from
/// [`dispatch`]'s loud refusal.
pub fn suggest(world: &World, source: &CommandSource, input: &str) -> Vec<String> {
    let Some(registry) = world.get_resource::<CommandRegistry>() else {
        return Vec::new();
    };
    let Some(permissions) = world.get_resource::<Permissions>() else {
        return Vec::new();
    };

    let stripped = input.strip_prefix('/').unwrap_or(input);
    let subject = source.subject;
    let filter = move |node: &str| permissions.has(subject, node);

    // Still on the first token: complete command names themselves, skipping any
    // command whose own root-literal permission the subject lacks.
    if !stripped.contains(' ') {
        let partial = stripped.to_lowercase();
        let mut out: Vec<String> = registry
            .names()
            .iter()
            .filter(|name| name.starts_with(&partial))
            .filter(|name| {
                registry.get(name).is_some_and(|command| {
                    command.permission().is_none_or(|node| filter(node))
                })
            })
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        return out;
    }

    let Some((command, canonical)) = canonicalize(registry, stripped) else {
        return Vec::new();
    };
    command.tree.suggest_filtered(&canonical, &filter)
}

/// The permission-pruned tree for one subject, ready for a future clientbound
/// `COMMANDS` encoder.
///
/// Returns the node ids reachable for `source`, in vanilla's
/// `fillUsableCommands` order (depth-first, children in insertion order,
/// skipping a denied node and its whole subtree). Not a `CommandTree` copy:
/// building one would need to renumber every redirect, and the encoder needs a
/// flat list with a root index anyway.
///
/// **Nothing calls this yet** — no protocol family encodes `COMMANDS`. It exists
/// so the arm that does has the pruning already correct rather than
/// reimplementing it, and it is exercised by the gate.
pub fn command_tree_for(command: &RegisteredCommand, allows: &dyn Fn(&str) -> bool) -> Vec<NodeId> {
    fn walk(
        tree: &CommandTree,
        node: NodeId,
        allows: &dyn Fn(&str) -> bool,
        out: &mut Vec<NodeId>,
    ) {
        for &child in tree.get(node).children() {
            if let Some(permission) = tree.get(child).permission() {
                if !allows(permission) {
                    continue;
                }
            }
            out.push(child);
            walk(tree, child, allows, out);
        }
    }
    let mut out = Vec::new();
    walk(&command.tree, command.tree.root(), allows, &mut out);
    out
}

/// `TickSet::Send`: refresh [`PlayerDirectory`] from the session tab list.
///
/// Runs unconditionally but cheaply — it rebuilds a `Vec<String>` of however
/// many players are listed, once per tick. If that ever shows up in a profile,
/// the fix is to fold it off the tab-list *events* rather than to poll, which
/// needs an `ingest`-side arm and is a bigger change than it looks.
pub fn sync_player_directory(
    directory: Res<PlayerDirectory>,
    tab_lists: bevy_ecs::prelude::Query<&crate::session::SessionTabList>,
) {
    let mut names: Vec<String> = Vec::new();
    for list in &tab_lists {
        names.extend(list.0.iter().map(|entry| entry.profile.name.clone()));
    }
    names.sort();
    names.dedup();
    directory.set(names);
}

/// Installs the registry, the permission resource, the player directory and the
/// directory-refresh system.
///
/// Inserts [`Permissions`] as well as [`CommandRegistry`] because [`dispatch`]
/// hard-errors without it (see [`CommandDispatchError::NotInstalled`]): a plugin
/// that adds this plugin and registers a command must not then be told commands
/// are unavailable.
#[derive(Debug, Default)]
pub struct PluginCommandsPlugin;

impl Plugin for PluginCommandsPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<CommandRegistry>();
        app.init_resource::<Permissions>();
        app.init_resource::<PlayerDirectory>();
        app.add_systems(GameTick, sync_player_directory.in_set(TickSet::Send));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_command::{IntegerArgument, StringArgument};

    /// `RegisteredCommand::root_literal` assumes the synthetic root has exactly
    /// one child and that it is the command's own literal. Asserted by *name*
    /// rather than by id, so it stays meaningful if the arena numbering ever
    /// changes.
    #[test]
    fn the_root_literal_is_the_command_name() {
        let mut command = PluginCommand::new("test");
        let root = command.root();
        command.permission("test.use");
        command.on_execute(root, |_| CommandOutcome::ok());
        let mut registry = CommandRegistry::new();
        registry.register(command).unwrap();

        let registered = registry.get("test").unwrap();
        assert_eq!(registered.tree.get(registered.tree.root()).children().len(), 1);
        assert_eq!(
            registered.tree.get(registered.root_literal()).name(),
            Some("test")
        );
        // And `permission()` reads that same node, which is what makes a
        // command-level permission gate the whole tree.
        assert_eq!(registered.permission(), Some("test.use"));
    }

    #[test]
    fn a_command_with_no_handler_is_refused_at_registration() {
        let command = PluginCommand::new("empty");
        let mut registry = CommandRegistry::new();
        assert_eq!(
            registry.register(command),
            Err(CommandRegisterError::NoHandlers {
                name: "empty".to_string()
            })
        );
    }

    #[test]
    fn a_duplicate_name_or_alias_is_refused() {
        let mut registry = CommandRegistry::new();

        let mut first = PluginCommand::new("warp");
        let root = first.root();
        first.alias("w");
        first.on_execute(root, |_| CommandOutcome::ok());
        registry.register(first).unwrap();

        let mut same_name = PluginCommand::new("warp");
        let root = same_name.root();
        same_name.on_execute(root, |_| CommandOutcome::ok());
        assert!(matches!(
            registry.register(same_name),
            Err(CommandRegisterError::NameTaken { .. })
        ));

        // And an alias colliding with the *other* command's alias.
        let mut same_alias = PluginCommand::new("warps");
        let root = same_alias.root();
        same_alias.alias("w");
        same_alias.on_execute(root, |_| CommandOutcome::ok());
        assert_eq!(
            registry.register(same_alias),
            Err(CommandRegisterError::NameTaken {
                name: "w".to_string()
            })
        );
    }

    #[test]
    fn canonicalize_rewrites_an_alias_and_strips_the_slash() {
        let mut registry = CommandRegistry::new();
        let mut command = PluginCommand::new("myplugin");
        let root = command.root();
        command.alias("mp");
        let sub = command.literal(root, "reload");
        command.on_execute(sub, |_| CommandOutcome::ok());
        registry.register(command).unwrap();

        let (resolved, canonical) = canonicalize(&registry, "/mp reload").unwrap();
        assert_eq!(resolved.name, "myplugin");
        assert_eq!(canonical, "myplugin reload");

        assert!(canonicalize(&registry, "/nope").is_none());
    }

    /// A greedy string reaches the handler intact, which is the argument type
    /// most likely to be broken by tokenizing twice.
    #[test]
    fn a_greedy_string_argument_survives_canonicalization() {
        let mut registry = CommandRegistry::new();
        let mut command = PluginCommand::new("say");
        let root = command.root();
        let message = command.argument(root, "message", Arc::new(StringArgument::greedy()));
        command.on_execute(message, |_| CommandOutcome::ok());
        registry.register(command).unwrap();

        let (_, canonical) = canonicalize(&registry, "/say hello there world").unwrap();
        assert_eq!(canonical, "say hello there world");
        let parsed = registry
            .get("say")
            .unwrap()
            .tree
            .parse(&canonical)
            .expect("greedy string should parse");
        assert_eq!(
            parsed.argument("message"),
            Some(&ParsedValue::String("hello there world".to_string()))
        );
    }

    #[test]
    fn an_integer_argument_bound_is_enforced() {
        let mut registry = CommandRegistry::new();
        let mut command = PluginCommand::new("give");
        let root = command.root();
        let amount = command.argument(root, "amount", Arc::new(IntegerArgument::bounded(1, 64)));
        command.on_execute(amount, |_| CommandOutcome::ok());
        registry.register(command).unwrap();

        let tree = &registry.get("give").unwrap().tree;
        assert!(tree.parse("give 32").is_ok());
        assert!(tree.parse("give 65").is_err());
        assert!(tree.parse("give 0").is_err());
    }

    /// The strict choice argument rejects a value outside the set, and the
    /// lenient one accepts it — the distinction `choice_argument`'s doc calls
    /// silent if got wrong.
    #[test]
    fn strict_choices_reject_an_unknown_value_and_lenient_ones_accept_it() {
        let strict = choice_argument(["stone", "dirt"]);
        let directory = PlayerDirectory::default();
        directory.set(vec!["Alice".to_string()]);
        let lenient = player_argument(&directory);

        let mut tree = CommandTree::new();
        let root = tree.root();
        let a = tree.add_literal(root, "strict");
        let arg = tree.add_argument(a, "block", strict);
        tree.set_executable(arg, true);
        let b = tree.add_literal(root, "lenient");
        let arg2 = tree.add_argument(b, "who", lenient);
        tree.set_executable(arg2, true);

        assert!(tree.parse("strict stone").is_ok());
        assert!(tree.parse("strict stnoe").is_err(), "strict must reject");
        assert!(tree.parse("lenient Alice").is_ok());
        assert!(
            tree.parse("lenient Bob").is_ok(),
            "lenient must accept an offline name"
        );
    }

    /// Suggestions for a dynamic provider really do follow the shared cell,
    /// which is the whole reason `PlayerDirectory` is an `Arc<RwLock<_>>`
    /// rather than a `Vec` copied at build time.
    #[test]
    fn player_suggestions_follow_the_directory_after_the_argument_was_built() {
        let directory = PlayerDirectory::default();
        let argument = player_argument(&directory);
        assert!(argument.suggest("").is_empty());

        directory.set(vec!["Alice".to_string(), "Bob".to_string()]);
        assert_eq!(argument.suggest(""), vec!["Alice", "Bob"]);
    }
}
