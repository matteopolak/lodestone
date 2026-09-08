//! Server-owned plugin command registration and tick-owner dispatch.
//!
//! ## What it is
//!
//! [`ServerCommandRegistry`] is the server-side home for plugin command trees.
//! It owns registration, aliases, parsing, suggestions and the permission
//! filter used by both execution and completion. [`ServerCommandQueue`] is the
//! bounded hand-back seam for network-facing producers: producers enqueue a
//! value-only request, and the primary tick owner drains it and sends the
//! response without sharing a server world lock with a connection task.
//!
//! This module deliberately does not depend on the client ECS. A handler gets
//! a value-only [`ServerCommandInvocation`]; the eventual server plugin host
//! can add world effects from the tick-owned side of that boundary without
//! making connection tasks borrow or lock the world.
//!
//! ## How it works
//!
//! A [`ServerPluginCommand`] builds one `lodestone-command` tree and keeps its
//! handlers in a side table keyed by [`lodestone_command::NodeId`]. Registration
//! freezes the command and rejects duplicate names or aliases. Unregistration
//! removes a canonical root and all aliases; reload replaces one atomically.
//! Dispatch
//! canonicalises an alias, filters the parse through [`ServerPermissions`],
//! then invokes the deepest handler on the parsed path. Suggestions use the
//! same permission policy but silently prune denied nodes.
//!
//! The queue is intentionally separate from [`crate::CommandDispatch`]. That
//! existing trait is synchronous and is used by connection tasks; this queue
//! provides the non-blocking request path needed before a dedicated server can
//! install a tick-owned command sink. Pure value-only handlers can still use
//! [`ServerCommandRegistry::into_dispatch`] as a production adapter today.
//!
//! ## How to change it
//!
//! Keep registration and dispatch on the same tree. Do not add a world or ECS
//! handle to [`ServerCommandInvocation`] or [`ServerCommandRequest`]. If a
//! future server sink needs a response, enqueue a request and let its tick
//! owner call [`ServerCommandQueue::drain`] rather than waiting while holding
//! a connection-side lock. Extend [`ServerPermissions`] only through its
//! single `allows` path so execution and suggestions cannot disagree.
//! [`ServerCommandQueue::drain_async`] uses the existing server scheduler when
//! a response-producing handler should run off-tick; its result still returns
//! only through the scheduler's tick-owned hand-back.
//!
//! ## Configuration
//!
//! [`ServerCommandQueue::with_capacity`] controls the bounded request queue;
//! there is no environment configuration. The default permission for an
//! undeclared node is operator-only, while declarations can choose any of
//! [`ServerPermissionDefault`]'s four values.
//!
//! ## Dependencies
//!
//! `lodestone-command` supplies the version-free tree, parser and suggester;
//! `bevy_ecs` supplies the optional `Resource` marker for server insertion;
//! [`crate::CommandCaller`] carries the authenticated caller identity and
//! effective operator level.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;

use bevy_ecs::resource::Resource;
use lodestone_command::{ArgumentType, CommandTree, NodeId, ParseError, ParsedCommand};
use uuid::Uuid;

use crate::CommandCaller;

/// The value-only caller context a server command receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommandSource {
    pub caller: CommandCaller,
}

impl ServerCommandSource {
    #[must_use]
    pub fn new(caller: CommandCaller) -> Self {
        Self { caller }
    }
}

/// The value-only invocation handed to a registered handler.
pub struct ServerCommandInvocation {
    pub source: ServerCommandSource,
    pub parsed: ParsedCommand,
    /// The canonical input, with a leading slash removed and aliases rewritten.
    pub input: String,
}

impl fmt::Debug for ServerCommandInvocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCommandInvocation")
            .field("source", &self.source)
            .field("parsed", &self.parsed)
            .field("input", &self.input)
            .finish()
    }
}

/// What a server plugin handler reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCommandOutcome {
    Ran { feedback: Vec<String>, result: i32 },
    Refused { message: String },
}

impl ServerCommandOutcome {
    #[must_use]
    pub fn ran(result: i32) -> Self {
        Self::Ran { feedback: Vec::new(), result }
    }

    #[must_use]
    pub fn refused(message: impl Into<String>) -> Self {
        Self::Refused { message: message.into() }
    }
}

/// A handler for one executable command node.
pub type ServerCommandHandler = Arc<dyn Fn(&ServerCommandInvocation) -> ServerCommandOutcome + Send + Sync>;

/// A value-only adapter for the existing server command seam.
///
/// This is safe for handlers that only produce a response. A handler that must
/// mutate server state belongs behind [`ServerCommandQueue`] and must run from
/// the primary tick owner instead of using this synchronous adapter.
pub struct ServerCommandSink {
    registry: Arc<ServerCommandRegistry>,
    permissions: Arc<ServerPermissions>,
}

/// The server command owner shared by connection-facing dispatch and command
/// tree projection. Keeping both views over the same registry prevents a
/// suggestion from advertising a root or branch that execution would refuse.
#[derive(Clone)]
pub struct ServerCommandOwner {
    registry: Arc<ServerCommandRegistry>,
    permissions: Arc<ServerPermissions>,
    dispatch: crate::CommandDispatch,
}

impl fmt::Debug for ServerCommandOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCommandOwner")
            .field("registry", &self.registry)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

impl ServerCommandOwner {
    #[must_use]
    pub fn new(registry: Arc<ServerCommandRegistry>, permissions: Arc<ServerPermissions>) -> Self {
        let dispatch = crate::CommandDispatch::installed(Arc::new(ServerCommandSink {
            registry: Arc::clone(&registry),
            permissions: Arc::clone(&permissions),
        }));
        Self { registry, permissions, dispatch }
    }

    #[must_use]
    pub fn dispatch(&self) -> crate::CommandDispatch {
        self.dispatch.clone()
    }

    #[must_use]
    pub fn suggest(&self, caller: &CommandCaller, input: &str) -> Vec<String> {
        self.registry.suggest(&ServerCommandSource::new(caller.clone()), &self.permissions, input)
    }

    #[must_use]
    pub fn registry(&self) -> &Arc<ServerCommandRegistry> {
        &self.registry
    }
}

impl fmt::Debug for ServerCommandSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCommandSink").finish_non_exhaustive()
    }
}

impl crate::CommandSink for ServerCommandSink {
    fn run(&self, caller: &CommandCaller, command: &str) -> crate::CommandResponse {
        let source = ServerCommandSource::new(caller.clone());
        match self.registry.dispatch(&source, &self.permissions, command) {
            Ok(ServerCommandOutcome::Ran { feedback, .. }) => crate::CommandResponse::Ran { feedback },
            Ok(ServerCommandOutcome::Refused { message }) => crate::CommandResponse::refused(message),
            Err(ServerCommandDispatchError::Empty | ServerCommandDispatchError::UnknownCommand { .. }) => {
                crate::CommandResponse::refused(crate::UNKNOWN_COMMAND)
            }
            Err(error) => crate::CommandResponse::refused(error.to_string()),
        }
    }
}

/// A command under construction.
pub struct ServerPluginCommand {
    name: String,
    aliases: Vec<String>,
    description: String,
    tree: CommandTree,
    root_literal: NodeId,
    handlers: HashMap<NodeId, ServerCommandHandler>,
}

impl fmt::Debug for ServerPluginCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerPluginCommand")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("description", &self.description)
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl ServerPluginCommand {
    /// Start a command whose root literal is `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(!name.trim().is_empty(), "command name must not be empty");
        assert!(!name.contains(' '), "command name must not contain spaces: {name:?}");
        let mut tree = CommandTree::new();
        let root_literal = tree.add_literal(tree.root(), &name.to_lowercase());
        Self {
            name,
            aliases: Vec::new(),
            description: String::new(),
            tree,
            root_literal,
            handlers: HashMap::new(),
        }
    }

    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root_literal
    }

    pub fn description(&mut self, description: impl Into<String>) -> &mut Self {
        self.description = description.into();
        self
    }

    pub fn alias(&mut self, alias: impl Into<String>) -> &mut Self {
        let alias = alias.into();
        assert!(!alias.trim().is_empty(), "command alias must not be empty");
        assert!(!alias.contains(' '), "command alias must not contain spaces: {alias:?}");
        self.aliases.push(alias.to_lowercase());
        self
    }

    pub fn permission(&mut self, permission: impl Into<String>) -> &mut Self {
        self.tree.require_permission(self.root_literal, permission);
        self
    }

    pub fn literal(&mut self, parent: NodeId, name: &str) -> NodeId {
        self.tree.add_literal(parent, name)
    }

    pub fn argument(&mut self, parent: NodeId, name: &str, ty: Arc<dyn ArgumentType>) -> NodeId {
        self.tree.add_argument(parent, name, ty)
    }

    pub fn require_permission(&mut self, node: NodeId, permission: impl Into<String>) -> &mut Self {
        self.tree.require_permission(node, permission);
        self
    }

    pub fn on_execute<F>(&mut self, node: NodeId, handler: F) -> &mut Self
    where
        F: Fn(&ServerCommandInvocation) -> ServerCommandOutcome + Send + Sync + 'static,
    {
        self.tree.set_executable(node, true);
        self.handlers.insert(node, Arc::new(handler));
        self
    }
}

/// Why a server command registration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCommandRegisterError {
    NameTaken { name: String },
    NoHandlers { name: String },
}

impl fmt::Display for ServerCommandRegisterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameTaken { name } => write!(f, "command name or alias '{name}' is already registered"),
            Self::NoHandlers { name } => write!(f, "command '{name}' has no executable handler"),
        }
    }
}

impl std::error::Error for ServerCommandRegisterError {}

/// Why command dispatch did not invoke a handler.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerCommandDispatchError {
    Empty,
    UnknownCommand { name: String },
    Parse(ParseError),
    NoHandler { name: String },
    AsyncQueueFull,
    AsyncShutdown,
}

impl fmt::Display for ServerCommandDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("unknown or incomplete command"),
            Self::UnknownCommand { .. } => f.write_str("unknown or incomplete command"),
            Self::Parse(error) => error.fmt(f),
            Self::NoHandler { name } => write!(f, "command '{name}' has no executable handler"),
            Self::AsyncQueueFull => f.write_str("command completion queue is full"),
            Self::AsyncShutdown => f.write_str("command completion queue is shut down"),
        }
    }
}

impl std::error::Error for ServerCommandDispatchError {}

/// The default applied to a declared node when no explicit grant matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerPermissionDefault {
    True,
    False,
    #[default]
    Op,
    NotOp,
}

impl ServerPermissionDefault {
    fn allows(self, level: u8) -> bool {
        match self {
            Self::True => true,
            Self::False => false,
            Self::Op => level >= 1,
            Self::NotOp => level == 0,
        }
    }
}

/// A server-owned grant and declaration store.
#[derive(Debug, Default, Resource)]
pub struct ServerPermissions {
    defaults: BTreeMap<String, ServerPermissionDefault>,
    grants: BTreeMap<Uuid, BTreeMap<String, bool>>,
}

impl ServerPermissions {
    pub fn declare(&mut self, node: impl Into<String>, default: ServerPermissionDefault) {
        self.defaults.insert(normalize_node(&node.into()), default);
    }

    pub fn allow(&mut self, player: Uuid, node: impl Into<String>) {
        self.grant(player, node, true);
    }

    pub fn deny(&mut self, player: Uuid, node: impl Into<String>) {
        self.grant(player, node, false);
    }

    fn grant(&mut self, player: Uuid, node: impl Into<String>, allowed: bool) {
        self.grants.entry(player).or_default().insert(normalize_node(&node.into()), allowed);
    }

    /// Resolve one node for an authenticated caller. Exact grants outrank
    /// wildcards; among wildcards, the most specific dotted prefix wins.
    #[must_use]
    pub fn allows(&self, caller: &CommandCaller, node: &str) -> bool {
        let node = normalize_node(node);
        let grant = self.grants.get(&caller.uuid).and_then(|entries| {
            entries
                .iter()
                .filter_map(|(key, value)| wildcard_specificity(key, &node).map(|score| (score, *value)))
                .max_by_key(|(score, _)| *score)
        });
        grant.map_or_else(
            || self.defaults.get(&node).copied().unwrap_or_default().allows(caller.permission_level),
            |(_, allowed)| allowed,
        )
    }
}

fn normalize_node(node: &str) -> String {
    node.trim().to_lowercase()
}

fn wildcard_specificity(key: &str, node: &str) -> Option<u32> {
    if key == node {
        return Some(u32::MAX);
    }
    if key == "*" {
        return Some(0);
    }
    let prefix = key.strip_suffix(".*")?;
    (node == prefix || node.starts_with(&format!("{prefix}."))).then(|| prefix.split('.').count() as u32)
}

/// The server-owned registry that plugins populate.
#[derive(Debug, Default, Resource)]
pub struct ServerCommandRegistry {
    commands: BTreeMap<String, Arc<RegisteredServerCommand>>,
    aliases: BTreeMap<String, String>,
    order: Vec<String>,
}

struct RegisteredServerCommand {
    name: String,
    aliases: Vec<String>,
    description: String,
    tree: CommandTree,
    root_literal: NodeId,
    handlers: HashMap<NodeId, ServerCommandHandler>,
}

impl fmt::Debug for RegisteredServerCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredServerCommand")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("description", &self.description)
            .field("nodes", &self.tree.len())
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl ServerCommandRegistry {
    pub fn register(&mut self, command: ServerPluginCommand) -> Result<(), ServerCommandRegisterError> {
        let canonical = normalize_node(&command.name);
        if command.handlers.is_empty() {
            return Err(ServerCommandRegisterError::NoHandlers { name: command.name });
        }
        if self.commands.contains_key(&canonical) || self.aliases.contains_key(&canonical) {
            return Err(ServerCommandRegisterError::NameTaken { name: canonical });
        }
        for alias in &command.aliases {
            if alias == &canonical {
                return Err(ServerCommandRegisterError::NameTaken { name: alias.clone() });
            }
            if self.commands.contains_key(alias) || self.aliases.contains_key(alias) {
                return Err(ServerCommandRegisterError::NameTaken { name: alias.clone() });
            }
        }
        for alias in &command.aliases {
            self.aliases.insert(alias.clone(), canonical.clone());
        }
        self.order.push(canonical.clone());
        self.commands.insert(canonical, Arc::new(RegisteredServerCommand {
            name: normalize_node(&command.name),
            aliases: command.aliases,
            description: command.description,
            tree: command.tree,
            root_literal: command.root_literal,
            handlers: command.handlers,
        }));
        Ok(())
    }

    /// Remove a command and all of its aliases.
    ///
    /// Passing either the canonical name or an alias removes the same command.
    /// The command's handlers are dropped with the registry entry, so a stale
    /// dispatch cannot continue invoking a reloaded plugin.
    pub fn unregister(&mut self, name: &str) -> bool {
        let canonical = normalize_node(name);
        let canonical = self.aliases.get(&canonical).cloned().unwrap_or(canonical);
        self.detach(&canonical).is_some()
    }

    /// Replace a command atomically, retaining its position in suggestion
    /// order. A failed registration leaves the previous command untouched.
    ///
    /// If `command` has no matching existing name this is equivalent to
    /// [`Self::register`]. This makes directory reloads one operation for a
    /// host: readers never observe a half-unregistered command.
    pub fn reload(&mut self, command: ServerPluginCommand) -> Result<(), ServerCommandRegisterError> {
        let requested = normalize_node(&command.name);
        let old_name = self.aliases.get(&requested).cloned().unwrap_or(requested);
        let old = self.detach(&old_name);
        let result = self.register(command);
        match result {
            Ok(()) => {
                if let Some((_, position)) = old {
                    let current = self.order.len() - 1;
                    let name = self.order.remove(current);
                    self.order.insert(position.min(self.order.len()), name);
                }
                Ok(())
            }
            Err(error) => {
                if let Some((registered, position)) = old {
                    self.restore(old_name, registered, position);
                }
                Err(error)
            }
        }
    }

    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.order
    }

    /// Install this registry into the existing command seam for handlers that
    /// do not need a world borrow. The returned dispatch is cloneable and can
    /// be passed to the server connection or RCON configuration.
    #[must_use]
    pub fn into_dispatch(self: Arc<Self>, permissions: Arc<ServerPermissions>) -> crate::CommandDispatch {
        ServerCommandOwner::new(self, permissions).dispatch()
    }

    /// Build a cloneable owner for both command execution and suggestions.
    #[must_use]
    pub fn into_owner(self: Arc<Self>, permissions: Arc<ServerPermissions>) -> ServerCommandOwner {
        ServerCommandOwner::new(self, permissions)
    }

    fn get(&self, name: &str) -> Option<&Arc<RegisteredServerCommand>> {
        let name = normalize_node(name);
        self.commands.get(&name).or_else(|| self.aliases.get(&name).and_then(|canonical| self.commands.get(canonical)))
    }

    fn detach(&mut self, canonical: &str) -> Option<(Arc<RegisteredServerCommand>, usize)> {
        let command = self.commands.remove(canonical)?;
        self.aliases.retain(|_, target| target != canonical);
        let position = self
            .order
            .iter()
            .position(|name| name == canonical)
            .expect("every registered command has one suggestion-order entry");
        self.order.remove(position);
        Some((command, position))
    }

    fn restore(&mut self, canonical: String, command: Arc<RegisteredServerCommand>, position: usize) {
        for alias in &command.aliases {
            self.aliases.insert(alias.clone(), canonical.clone());
        }
        self.commands.insert(canonical.clone(), command);
        self.order.insert(position.min(self.order.len()), canonical);
    }

    pub fn dispatch(
        &self,
        source: &ServerCommandSource,
        permissions: &ServerPermissions,
        input: &str,
    ) -> Result<ServerCommandOutcome, ServerCommandDispatchError> {
        let stripped = input.strip_prefix('/').unwrap_or(input).trim_start();
        if stripped.trim().is_empty() {
            return Err(ServerCommandDispatchError::Empty);
        }
        let (head, rest) = stripped.split_once(' ').map_or((stripped, None), |(head, rest)| (head, Some(rest)));
        let command = self.get(head).ok_or_else(|| ServerCommandDispatchError::UnknownCommand { name: head.to_string() })?.clone();
        let canonical = rest.map_or_else(|| command.name.clone(), |tail| format!("{} {tail}", command.name));
        let filter = |node: &str| permissions.allows(&source.caller, node);
        let parsed = command.tree.parse_filtered(&canonical, &filter).map_err(ServerCommandDispatchError::Parse)?;
        let handler = parsed.nodes.iter().rev().find_map(|id| command.handlers.get(id)).cloned().ok_or_else(|| ServerCommandDispatchError::NoHandler { name: command.name.clone() })?;
        let invocation = ServerCommandInvocation { source: source.clone(), parsed, input: canonical };
        Ok(handler(&invocation))
    }

    /// Return permission-filtered suggestions for one caller.
    #[must_use]
    pub fn suggest(&self, source: &ServerCommandSource, permissions: &ServerPermissions, input: &str) -> Vec<String> {
        let stripped = input.strip_prefix('/').unwrap_or(input);
        let filter = |node: &str| permissions.allows(&source.caller, node);
        if !stripped.contains(' ') {
            let partial = normalize_node(stripped);
            return self.order.iter().filter(|name| name.starts_with(&partial)).filter_map(|name| {
                let command = self.commands.get(name)?;
                let permission = command.tree.get(command.root_literal).permission();
                permission.is_none_or(|node| filter(node)).then_some(name.clone())
            }).collect();
        }
        let (head, tail) = stripped.split_once(' ').expect("contains a space");
        let Some(command) = self.get(head) else { return Vec::new() };
        let canonical = format!("{} {tail}", command.name);
        command.tree.suggest_filtered(&canonical, &filter)
    }
}

/// A queued value-only command request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommandRequest {
    pub source: ServerCommandSource,
    pub input: String,
}

/// A non-blocking response ticket. The tick owner sends the response while
/// draining; a producer may poll this ticket but cannot wait on the world.
pub struct ServerCommandTicket {
    receiver: Receiver<Result<ServerCommandOutcome, ServerCommandDispatchError>>,
}

impl fmt::Debug for ServerCommandTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCommandTicket").finish_non_exhaustive()
    }
}

impl ServerCommandTicket {
    pub fn try_recv(&self) -> Result<Option<Result<ServerCommandOutcome, ServerCommandDispatchError>>, ServerCommandQueueError> {
        match self.receiver.try_recv() {
            Ok(response) => Ok(Some(response)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ServerCommandQueueError::Closed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerCommandQueueError {
    Full,
    Closed,
}

/// The bounded queue drained by the primary server tick owner.
pub struct ServerCommandQueue {
    receiver: Receiver<QueuedCommand>,
    accepting: Arc<AtomicBool>,
}

impl fmt::Debug for ServerCommandQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCommandQueue")
            .field("accepting", &self.accepting.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ServerCommandQueueHandle {
    sender: SyncSender<QueuedCommand>,
    accepting: Arc<AtomicBool>,
}

impl fmt::Debug for ServerCommandQueueHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerCommandQueueHandle").finish_non_exhaustive()
    }
}

struct QueuedCommand {
    request: ServerCommandRequest,
    reply: SyncSender<Result<ServerCommandOutcome, ServerCommandDispatchError>>,
}

impl ServerCommandQueue {
    pub fn with_capacity(capacity: usize) -> (Self, ServerCommandQueueHandle) {
        assert!(capacity > 0, "server command queue capacity must be positive");
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let accepting = Arc::new(AtomicBool::new(true));
        (
            Self { receiver, accepting: Arc::clone(&accepting) },
            ServerCommandQueueHandle { sender, accepting },
        )
    }

    pub fn drain(&mut self, registry: &ServerCommandRegistry, permissions: &ServerPermissions, max: usize) -> usize {
        let mut drained = 0;
        while drained < max {
            let Ok(queued) = self.receiver.try_recv() else { break };
            let response = registry.dispatch(&queued.request.source, permissions, &queued.request.input);
            let _ = queued.reply.try_send(response);
            drained += 1;
        }
        drained
    }

    /// Dispatch queued value-only commands through the server's bounded async
    /// scheduler. The worker receives no world; the response sender is called
    /// only by the scheduler's tick-owner hand-back. If the scheduler cannot
    /// reserve capacity, the request receives a named refusal rather than
    /// waiting or being silently dropped.
    pub fn drain_async(
        &mut self,
        scheduler: &mut crate::ecs::ServerTaskScheduler,
        registry: Arc<ServerCommandRegistry>,
        permissions: Arc<ServerPermissions>,
        max: usize,
    ) -> usize {
        let mut drained = 0;
        while drained < max {
            let Ok(queued) = self.receiver.try_recv() else { break };
            let refusal_reply = queued.reply;
            let source = queued.request.source;
            let input = queued.request.input;
            // Keep one sender for the scheduler hand-back and retain the
            // original for an immediate refusal if enqueueing that hand-back
            // fails. Both paths must be able to report exactly once.
            let hand_back_reply = refusal_reply.clone();
            let result = scheduler.spawn_with_handback(
                {
                    let registry = Arc::clone(&registry);
                    let permissions = Arc::clone(&permissions);
                    move || registry.dispatch(&source, &permissions, &input)
                },
                move |result, _world| {
                    let _ = hand_back_reply.try_send(result);
                },
            );
            if let Err(error) = result {
                let refusal = match error {
                    crate::ecs::ServerAsyncTaskError::Full => ServerCommandDispatchError::AsyncQueueFull,
                    crate::ecs::ServerAsyncTaskError::Shutdown => ServerCommandDispatchError::AsyncShutdown,
                };
                let _ = refusal_reply.try_send(Err(refusal));
            }
            drained += 1;
        }
        drained
    }

    pub fn shutdown(&mut self) {
        self.accepting.store(false, Ordering::Release);
    }
}

impl Drop for ServerCommandQueue {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
    }
}

impl ServerCommandQueueHandle {
    pub fn try_enqueue(&self, request: ServerCommandRequest) -> Result<ServerCommandTicket, ServerCommandQueueError> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(ServerCommandQueueError::Closed);
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        match self.sender.try_send(QueuedCommand { request, reply }) {
            Ok(()) => Ok(ServerCommandTicket { receiver }),
            Err(TrySendError::Full(_)) => Err(ServerCommandQueueError::Full),
            Err(TrySendError::Disconnected(_)) => Err(ServerCommandQueueError::Closed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_command::IntegerArgument;

    fn source(level: u8) -> ServerCommandSource {
        ServerCommandSource::new(CommandCaller::with_permission_level(Uuid::from_u128(7), "tester", level))
    }

    fn command() -> ServerPluginCommand {
        let mut command = ServerPluginCommand::new("tools");
        command.alias("t").permission("tools.use");
        let root = command.root();
        let count = command.argument(root, "count", Arc::new(IntegerArgument::bounded(1, 9)));
        command.require_permission(count, "tools.count");
        command.on_execute(count, |invocation| {
            ServerCommandOutcome::ran(invocation.parsed.argument("count").is_some() as i32)
        });
        command
    }

    #[test]
    fn dispatch_uses_alias_and_argument_handler() {
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        let mut permissions = ServerPermissions::default();
        permissions.declare("tools.use", ServerPermissionDefault::True);
        permissions.declare("tools.count", ServerPermissionDefault::True);
        assert_eq!(registry.dispatch(&source(0), &permissions, "/t 4").unwrap(), ServerCommandOutcome::ran(1));
    }

    #[test]
    fn denied_nodes_are_refused_and_hidden_from_suggestions() {
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        let mut permissions = ServerPermissions::default();
        permissions.declare("tools.use", ServerPermissionDefault::True);
        permissions.declare("tools.count", ServerPermissionDefault::False);
        let error = registry.dispatch(&source(0), &permissions, "tools 4").unwrap_err();
        assert!(matches!(error, ServerCommandDispatchError::Parse(ParseError { kind: lodestone_command::ParseErrorKind::NoPermission { .. }, .. })));
        assert!(registry.suggest(&source(0), &permissions, "tools ").is_empty());
    }

    #[test]
    fn wildcard_and_operator_defaults_are_fail_closed_for_players() {
        let mut permissions = ServerPermissions::default();
        let player = CommandCaller::with_permission_level(Uuid::from_u128(9), "player", 0);
        let operator = CommandCaller::with_permission_level(Uuid::from_u128(10), "operator", 2);
        assert!(!permissions.allows(&player, "plugin.use"));
        assert!(permissions.allows(&operator, "plugin.use"));
        permissions.allow(player.uuid, "plugin.*");
        assert!(permissions.allows(&player, "plugin.use"));
        permissions.deny(player.uuid, "plugin.admin");
        assert!(!permissions.allows(&player, "plugin.admin"));
    }

    #[test]
    fn queue_is_bounded_and_only_drain_runs_dispatch() {
        let (mut queue, handle) = ServerCommandQueue::with_capacity(1);
        let request = |input: &str| ServerCommandRequest { source: source(0), input: input.to_string() };
        let first = handle.try_enqueue(request("tools 1")).unwrap();
        assert!(matches!(handle.try_enqueue(request("tools 2")), Err(ServerCommandQueueError::Full)));
        assert_eq!(first.try_recv().unwrap(), None);
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        let mut permissions = ServerPermissions::default();
        permissions.declare("tools.use", ServerPermissionDefault::True);
        permissions.declare("tools.count", ServerPermissionDefault::True);
        assert_eq!(queue.drain(&registry, &permissions, 1), 1);
        assert_eq!(first.try_recv().unwrap(), Some(Ok(ServerCommandOutcome::ran(1))));
    }

    #[test]
    fn async_queue_completion_returns_only_after_a_tick_owner_hand_back() {
        let (mut queue, handle) = ServerCommandQueue::with_capacity(1);
        let ticket = handle
            .try_enqueue(ServerCommandRequest { source: source(0), input: "tools 1".to_string() })
            .unwrap();
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        let mut permissions = ServerPermissions::default();
        permissions.declare("tools.use", ServerPermissionDefault::True);
        permissions.declare("tools.count", ServerPermissionDefault::True);
        let mut world = crate::ecs::ServerApp::bootstrap().into_world();
        let registry = Arc::new(registry);
        let permissions = Arc::new(permissions);
        assert_eq!(
            queue.drain_async(
                &mut world.resource_mut::<crate::ecs::ServerTaskScheduler>(),
                Arc::clone(&registry),
                Arc::clone(&permissions),
                1,
            ),
            1
        );

        for _ in 0..100 {
            world.run_schedule(crate::ecs::GameTick);
            if let Some(Ok(outcome)) = ticket.try_recv().unwrap() {
                assert_eq!(outcome, ServerCommandOutcome::ran(1));
                return;
            }
            std::thread::yield_now();
        }
        panic!("async command completion never reached the tick owner");
    }

    #[test]
    fn async_queue_reports_scheduler_saturation_without_waiting() {
        let (mut queue, handle) = ServerCommandQueue::with_capacity(1);
        let ticket = handle
            .try_enqueue(ServerCommandRequest { source: source(0), input: "tools 1".to_string() })
            .unwrap();
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        let mut permissions = ServerPermissions::default();
        permissions.declare("tools.use", ServerPermissionDefault::True);
        permissions.declare("tools.count", ServerPermissionDefault::True);
        let mut world = crate::ecs::ServerApp::bootstrap().into_world();
        world.insert_resource(crate::ecs::ServerTaskScheduler::with_async_hand_back_capacity(1));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_release = Arc::clone(&release);
        world
            .resource_mut::<crate::ecs::ServerTaskScheduler>()
            .spawn_with_handback(
                move || {
                    worker_release.wait();
                    1_u8
                },
                |_, _| {},
            )
            .unwrap();
        let result = queue.drain_async(
            &mut world.resource_mut::<crate::ecs::ServerTaskScheduler>(),
            Arc::new(registry),
            Arc::new(permissions),
            1,
        );
        assert_eq!(result, 1);
        assert!(matches!(
            ticket.try_recv().unwrap(),
            Some(Err(ServerCommandDispatchError::AsyncQueueFull))
        ));
        release.wait();
    }

    #[test]
    fn registry_has_a_command_dispatch_consumer() {
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        let mut permissions = ServerPermissions::default();
        permissions.declare("tools.use", ServerPermissionDefault::True);
        permissions.declare("tools.count", ServerPermissionDefault::True);
        let dispatch = Arc::new(registry).into_dispatch(Arc::new(permissions));
        assert!(dispatch.is_installed());
        assert!(dispatch.run(&source(0).caller, "t 2").is_ran());
    }

    #[test]
    fn unregister_and_reload_are_atomic_and_alias_aware() {
        let mut registry = ServerCommandRegistry::default();
        registry.register(command()).unwrap();
        assert!(registry.unregister("t"), "an alias must remove its canonical command");
        assert!(matches!(
            registry.dispatch(&source(0), &ServerPermissions::default(), "tools 2"),
            Err(ServerCommandDispatchError::UnknownCommand { .. })
        ));

        registry.register(command()).unwrap();
        let mut replacement = ServerPluginCommand::new("tools");
        replacement.alias("replacement");
        replacement.on_execute(|_| ServerCommandOutcome::ran(9));
        registry.reload(replacement).unwrap();
        assert_eq!(registry.names(), &["tools".to_owned()]);
        assert_eq!(
            registry
                .dispatch(&source(0), &ServerPermissions::default(), "replacement")
                .unwrap(),
            ServerCommandOutcome::ran(9)
        );

        let invalid = ServerPluginCommand::new("tools");
        let error = registry.reload(invalid).unwrap_err();
        assert!(matches!(error, ServerCommandRegisterError::NoHandlers { .. }));
        assert_eq!(
            registry
                .dispatch(&source(0), &ServerPermissions::default(), "tools")
                .unwrap(),
            ServerCommandOutcome::ran(9),
            "a failed reload must restore the previous command"
        );
    }
}
