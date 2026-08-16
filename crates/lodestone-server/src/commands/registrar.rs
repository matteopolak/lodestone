//! The typed builder: [`ArgKey`], [`Registrar`], [`Ctx`], and the executor /
//! modifier tables the dispatch walk reads.
//!
//! # Typed keys, and what they buy over Brigadier
//!
//! Brigadier reads an argument back with `getArgument("gamemode", GameType.class)`
//! — a string and a class, neither checked against the tree. An [`ArgKey<T>`]
//! exists **only** as the second half of the [`Registrar::arg`] call that created
//! its node, so there is no string to typo and no type to get wrong: a handler
//! cannot name an argument the tree does not declare, because the only way to
//! obtain a name is to have declared it.
//!
//! # The three residual runtime panics, named
//!
//! This is not a total design and pretending otherwise would be worse than the
//! panics. Each is a *programming* error in a `register_*` function, not
//! anything a player can cause, and each fires on the **first execution** of the
//! command — which is why one execution test per command is the stated bar:
//!
//! | panic | cause |
//! |---|---|
//! | `key from another tree` | an [`ArgKey`] built by a different [`Registrar`] |
//! | `key above its own depth` | reading an argument that is *deeper* on the path than the node currently executing (an [`Registrar::exec`] on the `<item>` node reading the `<count>` key) |
//! | `wrong Value type` | an [`McArg`] whose `Value` is not what its `parse` puts in the `ParsedValue` |
//!
//! The first two are structurally impossible to hit from a correct
//! registration; the third is the one real seam, and it is documented on
//! [`McArg`] itself.
//!
//! # Why the wire descriptor is recorded by `arg` and not separately
//!
//! One `CommandTree` has three consumers — execution, suggestion, and the wire
//! projection — and the failure mode when the third disagrees with the first is
//! a client that autocompletes what the server rejects. [`Registrar::arg`] takes
//! an [`McArg`], which *is* both halves, and records `a.wire()` in the same call
//! that installs the parser. There is no second place to state a node's wire
//! identity, so there is nothing to keep in sync.

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lodestone_command::{CommandTree, NodeId, ParsedCommand};
use lodestone_command_mc::McArg;
use lodestone_model::command_tree::ArgumentParser;
use lodestone_model::ids::ResourceKey;

use super::effect::{DirectedEffect, Effect};
use super::source::{CommandSource, PlayerCandidate, SelectorError};
use crate::game_rules::GameRulesHandle;

/// Identifies which [`Registrar`]'s arena an [`ArgKey`] indexes into.
///
/// A [`NodeId`] is meaningless outside its own tree, and this server builds
/// exactly one tree in production — but a test builds several, and an `ArgKey`
/// leaking between them would silently read whatever node happened to share the
/// index. An `AtomicU32` counter rather than a pointer so the id is `Copy` and
/// `Debug` and survives the tree being moved into an `Arc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeId(u32);

static NEXT_TREE_ID: AtomicU32 = AtomicU32::new(0);

impl TreeId {
    fn fresh() -> Self {
        Self(NEXT_TREE_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// A handle to one argument node, carrying the type of its value.
///
/// `PhantomData<fn() -> T>` rather than `PhantomData<T>` so the key is `Send`,
/// `Sync` and `Copy` regardless of `T` — a key is captured by an executor
/// closure, which must be `Send + Sync`, and `PhantomData<T>` would leak `T`'s
/// auto traits onto it.
pub struct ArgKey<T> {
    node: NodeId,
    tree: TreeId,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for ArgKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ArgKey<T> {}

impl<T> std::fmt::Debug for ArgKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArgKey")
            .field("node", &self.node)
            .field("tree", &self.tree)
            .field("type", &std::any::type_name::<T>())
            .finish()
    }
}

impl<T> ArgKey<T> {
    /// The node this key names, for a caller that needs to attach something else
    /// to it (an `exec`, a child argument).
    #[must_use]
    pub fn node(self) -> NodeId {
        self.node
    }
}

/// What an executor returns: the vanilla integer result, or a refusal the player
/// is shown.
///
/// `Result` rather than a bespoke enum because the two halves really are success
/// and failure here, and because `?` inside an executor is worth having. The
/// integer is vanilla's own return value — the number of things affected, which
/// `/execute store result` reads and which chained commands compose on.
pub type CommandResult = Result<i32, String>;

/// One executor, as stored in the side table. Aliased publicly (as
/// [`ExecutorEntry`]) because [`super::ServerCommands`] holds the table.
pub type ExecutorEntry = Arc<dyn Fn(&mut Ctx<'_>) -> CommandResult + Send + Sync>;
type Executor = ExecutorEntry;

/// A modifier rewrites the set of sources the rest of the command runs for —
/// Brigadier's `RedirectModifier`, and the whole of `/execute`'s machinery.
///
/// `as`/`at`/`positioned` return exactly one source (a *rewrite*); `as @a`
/// returns many (a **fork**). Both are this one signature; [`Registrar::modifier`]'s
/// `forks` flag is what distinguishes them, and it changes error handling rather
/// than the return type — a failure inside a fork is swallowed so the other
/// branches still run, which is `CommandDispatcher::execute`'s
/// `forkedStopControl` behaviour.
pub type ModifierEntry = Arc<
    dyn Fn(&mut Ctx<'_>, Vec<CommandSource>, &ParsedCommand) -> Result<Vec<CommandSource>, String>
        + Send
        + Sync,
>;
type Modifier = ModifierEntry;

/// One argument node's transmitted identity.
#[derive(Debug, Clone, PartialEq)]
pub struct WireDescriptor {
    pub parser: ArgumentParser,
    pub suggestions: Option<ResourceKey>,
}

/// Everything a built-in command may read about the world.
///
/// Borrowed, so the caller keeps whatever it already holds, and deliberately
/// narrow: adding a command must not widen `dispatch_play_packet`'s signature.
///
/// **Do not add a `&mut World`-shaped field.** The reason is
/// [`crate::command`]'s module doc, unchanged — this crate depends on neither
/// `lodestone-ecs` nor the client vocabulary, and the browser bundle is the
/// measured loser if it did.
pub struct CommandWorld<'a> {
    /// The world's shared game rules (issue #327), behind [`RuleStore`].
    ///
    /// `dyn RuleStore + Sync` because a `CommandWorld` is held across an `await`
    /// inside `serve_play`'s connection task, which `tokio::spawn` requires to be
    /// `Send` — and a `&dyn Trait` is only `Send` when the trait is `Sync`. Caught
    /// by the compiler at three `integrated.rs` spawn sites, not here.
    pub rules: &'a (dyn RuleStore + Sync),
    /// Every connected player, flattened out of the registry so resolution
    /// happens outside its lock. Never empty on the production path — see the
    /// `ChatCommand` arm in `crate::server`, which synthesises the caller's own
    /// entry when there is no registry, so `@s` works in singleplayer.
    pub players: &'a [PlayerCandidate],
    /// The world's difficulty, clock and spawn — `/time`, `/difficulty` and
    /// `/setworldspawn`'s read/write surface.
    ///
    /// A concrete `&WorldStateHandle` rather than a second trait object: unlike
    /// [`Self::rules`] (which a test may back with a bare [`crate::game_rules::GameRulesHandle`]
    /// that has no clock or difficulty at all), every one of these three commands
    /// needs the same production store, and `WorldStateHandle` already implements
    /// [`RuleStore`] too — see [`super::overworld_dimension`]'s neighbours for the
    /// production wiring, which passes one handle for both fields.
    pub state: &'a crate::world_state::WorldStateHandle,
    /// `/summon`'s synchronous single-mob spawn entry point — `MobHandle::with`
    /// plus `MobSim::spawn_species`, both already `pub` on the mob simulation's
    /// own handle, so this crate needs no new API there at all.
    ///
    /// `Option` because not every caller has one: RCON's `run_command` builds a
    /// [`CommandWorld`] with no chunk source or player registry either (see that
    /// module's own doc for the up-to-date roster of what RCON can and cannot
    /// run), and handing `/summon` a `MobHandle::default()` there would spawn
    /// into a throwaway sim nothing ticks or streams — an island, not a
    /// degradation. `None` is the honest answer for that caller; a live
    /// connection's `ChatCommand` arm always has the real handle in scope and
    /// passes `Some`.
    pub mobs: Option<&'a crate::mobs::MobHandle>,
    /// `/worldborder`'s read/write surface (issue #580) — the same
    /// [`crate::border::BorderFeed`] `crate::tick::run_tick_loop_with_weather`
    /// now ticks and every production connection reads for its join
    /// broadcast and enforcement. `Option` for the same reason [`Self::mobs`]
    /// is: RCON and this module's own test helper build a [`CommandWorld`]
    /// with no live border to reach, and a default feed nothing ticks would
    /// be an island rather than a degradation. A live connection's
    /// `ChatCommand` arm always has the real handle and passes `Some`.
    pub border: Option<&'a crate::border::BorderFeed>,
    /// `/op`/`/deop`/`/whitelist`'s read/write surface (`crate::access`) —
    /// previously nothing in this crate's command tree could reach
    /// [`crate::access::AccessLists`] at all, so an admin's only way to
    /// grant/revoke operator status or manage the whitelist was to stop the
    /// server and hand-edit `ops.json`/`whitelist.json`. `Option` for the
    /// same reason [`Self::mobs`]/[`Self::border`] are: RCON
    /// ([`crate::rcon::RconConfig::access`]) is today's one production
    /// `Some`, matching vanilla's own admin surface (the dedicated-server
    /// console/RCON, not in-game chat) — see `crate::commands::access_commands`'s
    /// own module doc for the rest of that scoping.
    ///
    /// `cfg`-gated with `crate::access` itself (native only, like
    /// `crate::rcon`, whose `AccessHandle` this field carries) — a browser
    /// singleplayer world has no filesystem-backed access lists and no RCON
    /// to grant them through.
    #[cfg(not(target_arch = "wasm32"))]
    pub access: Option<&'a crate::access::AccessHandle>,
}

/// Read/write access to the world's game rules, abstracted over *which* store.
///
/// This exists because of a real split that the previous island hid: the
/// production path keeps its rules inside
/// [`crate::world_state::WorldStateHandle`], while [`GameRulesHandle`] is a
/// standalone handle used by tests and by `run_tick_loop`. The old
/// `ServerCommands` took a `&GameRulesHandle` — so even if it *had* been wired
/// in, `/gamerule` would have written a store nothing else read.
///
/// One trait, two implementors, and the production wiring passes the
/// `WorldStateHandle` it already holds.
pub trait RuleStore {
    /// The rule's current value, or `None` for a name not in `GAME_RULES`.
    fn get_rule(&self, name: &str) -> Option<crate::game_rules::GameRuleValue>;

    /// Set the rule from its wire spelling, validating type and range.
    ///
    /// # Errors
    ///
    /// [`crate::game_rules::GameRuleError`] for an unknown rule or an
    /// unparseable/out-of-range value. The tree has already type-checked the
    /// value, so an error here means the tree and the spec disagree.
    fn set_rule(
        &self,
        name: &str,
        raw: &str,
    ) -> Result<crate::game_rules::GameRuleValue, crate::game_rules::GameRuleError>;
}

impl RuleStore for GameRulesHandle {
    fn get_rule(&self, name: &str) -> Option<crate::game_rules::GameRuleValue> {
        self.with(|rules| rules.get(name))
    }

    fn set_rule(
        &self,
        name: &str,
        raw: &str,
    ) -> Result<crate::game_rules::GameRuleValue, crate::game_rules::GameRuleError> {
        self.with(|rules| rules.set(name, raw))
    }
}

impl RuleStore for crate::world_state::WorldStateHandle {
    fn get_rule(&self, name: &str) -> Option<crate::game_rules::GameRuleValue> {
        self.rules().get(name)
    }

    fn set_rule(
        &self,
        name: &str,
        raw: &str,
    ) -> Result<crate::game_rules::GameRuleValue, crate::game_rules::GameRuleError> {
        crate::world_state::WorldStateHandle::set_rule(self, name, raw)
    }
}

/// One execution's context: the parsed command, the source it is running for,
/// and the effects it has asked for so far.
pub struct Ctx<'a> {
    tree: &'a CommandTree,
    tree_id: TreeId,
    parsed: &'a ParsedCommand,
    /// How many nodes of `parsed.nodes` are in scope. An argument parsed at a
    /// node beyond this is not visible: a modifier attached halfway down a path
    /// must not be able to read an argument that had not been parsed when it
    /// ran.
    depth: usize,
    /// The source this invocation is running for. `pub` because `/execute`'s
    /// modifiers exist to rewrite it.
    pub source: CommandSource,
    pub world: &'a CommandWorld<'a>,
    /// Which nodes are argument nodes, from the registrar's own wire table.
    ///
    /// `lodestone_command::Node` keeps `as_literal` crate-private, so there is no
    /// public way to ask the tree whether a node is an argument. The registrar
    /// already knows — it records a [`WireDescriptor`] for every argument node
    /// and none for a literal — so this borrows that set rather than widening
    /// `lodestone-command`'s API for one caller.
    argument_nodes: &'a HashSet<NodeId>,
    feedback: Vec<String>,
    effects: Vec<DirectedEffect>,
}

impl<'a> Ctx<'a> {
    /// Read an argument's value.
    ///
    /// # Panics
    ///
    /// On a key from another tree, or a key naming a node deeper on the path
    /// than the node currently executing, or an [`McArg`] whose `Value` type
    /// disagrees with what its parser produced. See this module's doc for why
    /// all three are registration bugs that fire on first execution.
    #[must_use]
    pub fn get<T: std::any::Any>(&self, key: ArgKey<T>) -> &T {
        assert!(
            key.tree == self.tree_id,
            "argument key {key:?} belongs to a different command tree than the one executing"
        );
        // `parsed.arguments` is in parse order, which is the order the *argument*
        // nodes appear on `parsed.nodes` — literals contribute no value. So the
        // key's index among the argument nodes of the in-scope prefix is its
        // index into the value list.
        //
        // The **last**, not first, matching occurrence: `/execute`'s redirect
        // cycle (every non-forking modifier redirects back into `execute`'s own
        // children) means the *same* `NodeId` — e.g. `positioned`'s `pos`
        // argument — can appear more than once in one parsed path
        // (`execute positioned 1 2 3 positioned ~5 ~ ~ run …`). Every modifier
        // is invoked with `depth` set to exactly its own occurrence's position
        // (`index + 1` in `Dispatcher::dispatch`'s walk), so "the value for
        // this key within the in-scope prefix" must mean the *closest*
        // occurrence to that depth, not the first one ever parsed — otherwise
        // a second `positioned` silently re-reads the first one's value.
        // Measured: without this, `execute positioned 1.0 2.0 11.0 positioned
        // ~5 ~0 ~-4 run …` resolved to `(1, 2, 11)` — the second hop's
        // coordinates were parsed correctly but never read.
        let index = self
            .parsed
            .nodes
            .iter()
            .take(self.depth)
            .filter(|node| self.argument_nodes.contains(node))
            .enumerate()
            .filter(|(_, node)| **node == key.node)
            .map(|(i, _)| i)
            .last()
            .unwrap_or_else(|| {
                panic!(
                    "argument key {key:?} names a node that is not on the executing path within \
                     depth {} — reading an argument deeper than the node that owns this executor",
                    self.depth
                )
            });
        let (_, value) = self
            .parsed
            .arguments
            .get(index)
            .expect("the tree's argument nodes and the parse's argument values are one sequence");
        value.downcast_ref::<T>().unwrap_or_else(|| {
            panic!(
                "argument {key:?} parsed as {value:?}, which is not the McArg::Value type declared \
                 for its argument type"
            )
        })
    }

    /// Show `line` to the command's caller (`source.sendSuccess`).
    pub fn send_success(&mut self, line: impl Into<String>) {
        self.feedback.push(line.into());
    }

    /// Ask for `effect` to be applied to `target`.
    pub fn effect(&mut self, target: uuid::Uuid, effect: Effect) {
        self.effects.push(DirectedEffect::new(target, effect));
    }

    /// Every top-level command literal this source's permission level may see —
    /// `/help`'s listing.
    ///
    /// Reuses [`lodestone_command::CommandTree::suggest_filtered`] at an empty
    /// prefix rather than a hand-maintained name list: the tree is the one
    /// source of truth for "what is registered", and a second list here is
    /// exactly the parity hazard this crate's own module doc warns about for a
    /// wire tree — a name added to one and not the other.
    #[must_use]
    pub fn root_command_names(&self) -> Vec<String> {
        let mut names = self.tree.suggest_filtered("", &level_filter(self.source.permission_level));
        names.sort();
        names
    }

    /// Resolve an entity selector against the roster, from this source.
    ///
    /// # Errors
    ///
    /// [`SelectorError`], which the caller normally propagates with `?` after
    /// `to_string()`.
    pub fn resolve(
        &self,
        selector: &lodestone_command_mc::EntitySelector,
    ) -> Result<Vec<PlayerCandidate>, SelectorError> {
        super::source::resolve_players(
            selector,
            &self.source,
            self.world.players,
            &super::source::no_shuffle,
        )
    }
}

/// Builds a [`super::ServerCommands`]' tree, executors, modifiers and wire
/// descriptors together.
pub struct Registrar {
    tree: CommandTree,
    tree_id: TreeId,
    executors: HashMap<NodeId, Executor>,
    modifiers: HashMap<NodeId, Modifier>,
    forks: HashSet<NodeId>,
    wire: HashMap<NodeId, WireDescriptor>,
}

/// The permission-node prefix a required *level* is recorded under.
///
/// `lodestone-command`'s permission seam is a dotted string, because that crate
/// cannot know what a permission is (see its `filter` module). 26.2's model is a
/// numeric level 0–4. Encoding the level as `lodestone.level.N` keeps the seam
/// unchanged and keeps the mapping in exactly two places — here, and
/// [`super::level_filter`] which reads it back.
pub const LEVEL_PERMISSION_PREFIX: &str = "lodestone.level.";

/// The permission node standing for "holds at least level `level`".
#[must_use]
pub fn level_permission(level: u8) -> String {
    format!("{LEVEL_PERMISSION_PREFIX}{level}")
}

impl Registrar {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tree: CommandTree::new(),
            tree_id: TreeId::fresh(),
            executors: HashMap::new(),
            modifiers: HashMap::new(),
            forks: HashSet::new(),
            wire: HashMap::new(),
        }
    }

    #[must_use]
    pub fn root(&self) -> NodeId {
        self.tree.root()
    }

    /// A literal child — `Commands.literal(name)`.
    pub fn literal(&mut self, parent: NodeId, name: &str) -> NodeId {
        self.tree.add_literal(parent, name)
    }

    /// An argument child, with its wire identity recorded in the same call —
    /// `Commands.argument(name, type)`.
    ///
    /// Returns the node (to hang children off) and the typed key (to read the
    /// value in an executor). Both, rather than one, because both are needed and
    /// deriving either from the other would mean a lookup that could fail.
    pub fn arg<A: McArg>(
        &mut self,
        parent: NodeId,
        name: &str,
        argument: A,
    ) -> (NodeId, ArgKey<A::Value>) {
        let descriptor =
            WireDescriptor { parser: argument.wire(), suggestions: argument.suggestion_provider() };
        let node = self.tree.add_argument(parent, name, Arc::new(argument));
        self.wire.insert(node, descriptor);
        (node, ArgKey { node, tree: self.tree_id, _marker: PhantomData })
    }

    /// Attach an executor and mark the node executable — Brigadier's
    /// `.executes(…)`, whose two halves are one call here because a node marked
    /// executable with nothing behind it is a registration bug the tree cannot
    /// see.
    pub fn exec(
        &mut self,
        node: NodeId,
        executor: impl Fn(&mut Ctx<'_>) -> CommandResult + Send + Sync + 'static,
    ) {
        self.tree.set_executable(node, true);
        self.executors.insert(node, Arc::new(executor));
    }

    /// Attach a source-set modifier — Brigadier's `.fork(…)`/`.redirect(…, mod)`.
    ///
    /// `forks` is `true` for the many-out case (`execute as @a`), which changes
    /// error handling: a failure in one branch is swallowed so the others run.
    pub fn modifier(
        &mut self,
        node: NodeId,
        forks: bool,
        modifier: impl Fn(&mut Ctx<'_>, Vec<CommandSource>, &ParsedCommand) -> Result<Vec<CommandSource>, String>
        + Send
        + Sync
        + 'static,
    ) {
        self.modifiers.insert(node, Arc::new(modifier));
        if forks {
            self.forks.insert(node);
        }
    }

    /// Continue parsing from `target`'s children — Brigadier's `redirect`.
    pub fn redirect(&mut self, node: NodeId, target: NodeId) {
        self.tree.set_redirect(node, target);
    }

    /// Gate `node` and its whole subtree on permission level `level` —
    /// `Commands.hasPermission(LEVEL_GAMEMASTERS)`.
    pub fn require_level(&mut self, node: NodeId, level: u8) {
        self.tree.require_permission(node, level_permission(level));
    }

    /// Consume the registrar.
    #[must_use]
    pub fn finish(self) -> RegistrarParts {
        RegistrarParts {
            tree: self.tree,
            tree_id: self.tree_id,
            executors: self.executors,
            modifiers: self.modifiers,
            forks: self.forks,
            wire: self.wire,
        }
    }
}

impl Default for Registrar {
    fn default() -> Self {
        Self::new()
    }
}

/// [`Registrar::finish`]'s output, assembled into [`super::ServerCommands`].
pub struct RegistrarParts {
    pub tree: CommandTree,
    pub tree_id: TreeId,
    pub executors: HashMap<NodeId, Executor>,
    pub modifiers: HashMap<NodeId, Modifier>,
    pub forks: HashSet<NodeId>,
    pub wire: HashMap<NodeId, WireDescriptor>,
}

/// The outcome of one dispatch.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutcome {
    /// What the caller is told.
    pub response: crate::command::CommandResponse,
    /// What must be applied, and to whom.
    pub effects: Vec<DirectedEffect>,
}

pub(super) struct Dispatcher<'a> {
    pub(super) tree: &'a CommandTree,
    pub(super) tree_id: TreeId,
    pub(super) executors: &'a HashMap<NodeId, Executor>,
    pub(super) modifiers: &'a HashMap<NodeId, Modifier>,
    pub(super) forks: &'a HashSet<NodeId>,
    pub(super) argument_nodes: &'a HashSet<NodeId>,
}

impl Dispatcher<'_> {
    /// Brigadier's `CommandDispatcher::execute`, restated: thread the source set
    /// through every modifier on the path, then run the **deepest** executor once
    /// per surviving source.
    ///
    /// The two behaviours worth stating because they are not symmetric:
    ///
    /// * A modifier that fails **aborts** the whole command, unless it is a fork,
    ///   in which case the failing branch is dropped and the rest continue.
    /// * An executor that fails aborts, unless the path forked, in which case its
    ///   refusal is recorded and the remaining sources still run. `execute as @a
    ///   run give @s …` must not stop at the first player whose inventory is
    ///   full.
    ///
    /// # A node's own modifier is skipped when *that node* is the terminal one
    ///
    /// This is `/execute`'s `if`/`unless`: vanilla attaches **both**
    /// `.fork(execute, modifier)` *and* `.executes(numericConditionalHandler)`
    /// to the same condition node (`ExecuteCommand::addConditional`), and real
    /// Brigadier's `ContextChain` only ever invokes one of the two — the fork
    /// modifier fires exclusively when the chain **continues** to a further
    /// stage (`execute if entity @a run …`), and a terminal match instead runs
    /// the node's own `command` callback directly against the *original*,
    /// unfiltered context (`execute if entity @a` alone). A plain "apply every
    /// modifier found along the path" walk cannot see that distinction — it
    /// would filter the source (or drop it entirely when the condition fails)
    /// *before* the terminal executor got a chance to report its own
    /// pass/fail message, silently turning a failed `unless`/`if` into an
    /// empty success instead of `commands.execute.conditional.fail`. So: when
    /// a node is both the parsed path's last node *and* carries its own
    /// executor, its modifier is not applied here at all — the executor runs
    /// against the single incoming source unchanged, exactly as `execute if
    /// entity <nothing>`'s own handler expects to see it.
    pub(super) fn dispatch(
        &self,
        world: &CommandWorld<'_>,
        source: CommandSource,
        parsed: &ParsedCommand,
    ) -> CommandOutcome {
        let mut sources = vec![source];
        let mut forked = false;
        let mut feedback: Vec<String> = Vec::new();
        let mut effects: Vec<DirectedEffect> = Vec::new();
        let last_index = parsed.nodes.len().wrapping_sub(1);

        for (index, node) in parsed.nodes.iter().enumerate() {
            if index == last_index && self.executors.contains_key(node) {
                continue;
            }
            let Some(modifier) = self.modifiers.get(node) else { continue };
            let node_forks = self.forks.contains(node);
            forked |= node_forks;
            let mut next: Vec<CommandSource> = Vec::new();
            // One `Ctx` per input source: a modifier reads `ctx.source`, so
            // handing it the whole set at once with somebody else's source in
            // scope would be the wrong context for all but the first.
            for input in std::mem::take(&mut sources) {
                let mut ctx = self.context(world, parsed, index + 1, input.clone());
                match modifier(&mut ctx, vec![input], parsed) {
                    Ok(produced) => {
                        feedback.append(&mut ctx.feedback);
                        effects.append(&mut ctx.effects);
                        next.extend(produced);
                    }
                    Err(message) => {
                        if node_forks || forked {
                            feedback.push(message);
                        } else {
                            return refused(message);
                        }
                    }
                }
            }
            sources = next;
        }

        let Some(&last) = parsed.nodes.last() else {
            // Structurally unreachable: a successful parse entered at least one
            // node. Refused rather than passed through, because falling through
            // would hand the host a string this tree claimed.
            return refused(crate::command::UNKNOWN_COMMAND);
        };
        let Some(executor) = self.executors.get(&last) else {
            // Executable with nothing behind it: a bug in a `register_*`
            // function, not player error, so it must not read as "unknown
            // command".
            return refused("That command is registered but has no behaviour — this is a server bug");
        };

        let mut ran = 0;
        let mut failures: Vec<String> = Vec::new();
        for input in sources {
            let mut ctx = self.context(world, parsed, parsed.nodes.len(), input);
            match executor(&mut ctx) {
                Ok(count) => {
                    ran += count;
                    feedback.append(&mut ctx.feedback);
                    effects.append(&mut ctx.effects);
                }
                Err(message) => {
                    // A forked path keeps going; an unforked one is a single
                    // invocation and its failure is *the* answer.
                    if forked {
                        failures.push(message);
                    } else {
                        return refused(message);
                    }
                }
            }
        }
        let _ = ran;
        feedback.extend(failures);
        CommandOutcome {
            response: crate::command::CommandResponse::Ran { feedback },
            effects,
        }
    }

    fn context<'c>(
        &'c self,
        world: &'c CommandWorld<'c>,
        parsed: &'c ParsedCommand,
        depth: usize,
        source: CommandSource,
    ) -> Ctx<'c> {
        Ctx {
            tree: self.tree,
            tree_id: self.tree_id,
            parsed,
            depth,
            source,
            world,
            feedback: Vec::new(),
            effects: Vec::new(),
            argument_nodes: self.argument_nodes,
        }
    }
}

fn refused(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome {
        response: crate::command::CommandResponse::refused(message),
        effects: Vec::new(),
    }
}

/// A [`lodestone_command::PermissionFilter`] that reads
/// [`level_permission`] back into a numeric level.
///
/// A node carrying any *other* permission string is denied: an unrecognised
/// permission must fail closed, the same direction
/// [`crate::CommandDispatch::none`] fails.
#[must_use]
pub fn level_filter(level: u8) -> impl Fn(&str) -> bool {
    move |permission: &str| {
        permission
            .strip_prefix(LEVEL_PERMISSION_PREFIX)
            .and_then(|rest| rest.parse::<u8>().ok())
            .is_some_and(|required| level >= required)
    }
}

/// The error a selector failure becomes for a player.
impl From<SelectorError> for String {
    fn from(error: SelectorError) -> Self {
        error.to_string()
    }
}
