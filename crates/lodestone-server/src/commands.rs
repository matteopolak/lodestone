//! The server's **own** Brigadier command tree, and the execution model
//! `lodestone-command` deliberately left undefined (issue #48).
//!
//! # What it is
//!
//! [`ServerCommands`] owns a real [`CommandTree`] of built-in commands, plus the
//! side table of executors that turns a successful parse into an effect. It is
//! consulted by `crate::server`'s `ServerBound::ChatCommand` arm **before** the
//! host-installed [`CommandSink`](crate::CommandSink), and it answers `None` for
//! a root it does not own so plugin commands keep working untouched.
//!
//! # What was missing, precisely
//!
//! Two halves existed and neither was this one.
//!
//! * `crate::command` is a **seam**, not a dispatcher: `CommandSink` hands a
//!   whole command string to the host so `lodestone_ecs::commands` can dispatch
//!   *plugin* commands, because that registry structurally cannot live in this
//!   crate. With no host installed — a dedicated server, or any of this crate's
//!   own entry points — every command was refused. There were no built-ins to
//!   refuse *to*.
//! * `lodestone-command` is the **argument tree**: nodes, parsers, suggestions.
//!   Its own crate doc says so explicitly — "`executable` is a bare flag",
//!   "this crate has no `CommandSource` and no execution semantics", and names
//!   #48 as the issue expected to define them.
//!
//! So the gap was an execution model plus at least one command that reaches a
//! player. This module is both.
//!
//! # The execution model, and why it hangs off `NodeId`
//!
//! Brigadier attaches a `Command<S>` callback to a *node*; `CommandTree` has no
//! room for one, by design. So executors live in a `NodeId`-keyed side table
//! ([`ServerCommands::executors`]) and dispatch takes the **last** node of the
//! parsed path — which is Brigadier's own rule, since the deepest matched node
//! is the one whose callback runs.
//!
//! This is the same shape `lodestone_ecs::commands` already chose for plugin
//! commands, and matching it is deliberate: two dispatchers over one tree
//! library should not disagree about where a callback lives.
//!
//! An executor is a boxed closure rather than a `fn` pointer for one concrete
//! reason: `/gamerule` builds **one literal node per rule**, exactly as vanilla
//! does, so each executor must capture *which* rule it belongs to. A `fn`
//! pointer cannot, and the alternative — one executor that re-derives the rule
//! by walking the node path back to its parent literal — would put the tree's
//! shape and the executor's behaviour in two places that must agree.
//!
//! # Precedence, and the one thing it must not do
//!
//! [`ServerCommands::run`] returns:
//!
//! | outcome | meaning | `crate::server` does |
//! |---|---|---|
//! | `Some(response)` | a built-in root matched | send those lines; **do not** consult the host |
//! | `None` | nothing at the root matched | fall through to [`CommandDispatch`](crate::CommandDispatch) |
//!
//! The `None` case is keyed on [`ParseErrorKind::UnknownCommand`] specifically,
//! which the tree only produces when *no token matched at the root at all*. A
//! built-in that matched and then failed on its arguments returns
//! `Some(refusal)` — so `/gamerule nonsense` reports the parse error rather than
//! silently becoming a plugin's problem. Falling through on every error instead
//! would be the subtle wrong choice: a typo'd built-in would be answered by
//! whatever the host does with unknown input, which is usually
//! [`UNKNOWN_COMMAND`](crate::UNKNOWN_COMMAND) — the player would be told the
//! command does not exist when in fact only their argument was wrong.
//!
//! # Permissions
//!
//! Vanilla gates `/gamerule` at permission level 2. This crate has **no
//! operator model** and every connection it serves is treated as the
//! singleplayer owner — the same simplification `crate::server`'s
//! `apply_difficulty_change` already documents for `SET_GAME_RULE`, which is the
//! *same operation over a different transport*. Gating the command but not the
//! packet would be security theatre, so neither is gated and both say so.
//!
//! `CommandTree::require_permission` and `parse_filtered` exist and are the
//! hook to use when an op model lands; see `crate::command`'s note that this
//! layer cannot resolve a permission and never will.
//!
//! # How to change it
//!
//! * **Adding a built-in:** write a `register_*` function that builds its nodes
//!   and inserts executors, then call it from [`ServerCommands::new`]. Add a
//!   test that drives it through [`ServerCommands::run`] — not through the
//!   executor directly, which cannot see a tree that was never wired.
//! * **Do not add a `&mut World`-shaped parameter to [`CommandEffects`].** The
//!   reason is `crate::command`'s module doc, unchanged: this crate depends on
//!   neither `lodestone-ecs` nor the client vocabulary, and the browser bundle
//!   is the measured loser if it did. `lodestone-command` is safe to depend on
//!   because it is genuinely dependency-free — see its `Cargo.toml`, which has
//!   an empty `[dependencies]`.
//!
//! # Dependencies
//!
//! `lodestone-command` (the argument tree; zero dependencies of its own) and
//! `crate::game_rules` (what `/gamerule` actually mutates).

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_command::{
    BoolArgument, CommandTree, IntegerArgument, NodeId, ParseErrorKind, ParsedCommand, ParsedValue,
};

use crate::command::{CommandCaller, CommandResponse};
use crate::game_rules::{GAME_RULES, GameRuleValue, GameRulesHandle};

/// Everything a built-in command is allowed to touch.
///
/// One struct rather than a growing parameter list, and deliberately narrow: a
/// built-in reaches world state through a handle this crate already shares, so
/// adding a command does not widen `dispatch_play_packet`'s signature.
///
/// Borrowed, not owned, so the caller keeps whatever it already holds.
pub(crate) struct CommandEffects<'a> {
    /// Who typed it — the connection's own authenticated identity, built by
    /// `crate::server` from the login. See [`CommandCaller`] for why nothing in
    /// the command text can influence this.
    pub(crate) caller: &'a CommandCaller,
    /// The world's shared game rules (issue #327).
    pub(crate) rules: &'a GameRulesHandle,
}

/// A built-in command's behaviour: the parsed command in, lines out.
type Executor = Arc<dyn Fn(&CommandEffects<'_>, &ParsedCommand) -> CommandResponse + Send + Sync>;

/// The server's built-in command tree plus its executors.
///
/// Cheap to clone (the tree and the table are both behind one `Arc`), because
/// every connection task needs one and building the `/gamerule` subtree means
/// allocating ~120 nodes — once per server, not once per connection.
#[derive(Clone)]
pub struct ServerCommands {
    inner: Arc<ServerCommandsInner>,
}

struct ServerCommandsInner {
    tree: CommandTree,
    executors: HashMap<NodeId, Executor>,
}

impl std::fmt::Debug for ServerCommands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerCommands")
            .field("executors", &self.inner.executors.len())
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
        let mut tree = CommandTree::new();
        let mut executors: HashMap<NodeId, Executor> = HashMap::new();
        register_gamerule(&mut tree, &mut executors);
        Self { inner: Arc::new(ServerCommandsInner { tree, executors }) }
    }

    /// The underlying tree, for a caller that wants to render or send it
    /// (`COMMANDS`, packet id 16, whose encode is issue #46's half and does not
    /// exist in any family today).
    #[must_use]
    pub fn tree(&self) -> &CommandTree {
        &self.inner.tree
    }

    /// Completions for a partially-typed command, from the built-in tree.
    ///
    /// Exposed because the tree is the only thing that knows them, and a
    /// `COMMAND_SUGGESTIONS` responder will need exactly this. No production
    /// caller yet — stated rather than hidden, since an unconsumed method is
    /// how the last island started.
    #[must_use]
    pub fn suggest(&self, partial: &str) -> Vec<String> {
        self.inner.tree.suggest(partial)
    }

    /// Runs `command` (no leading `/`) if a built-in root matches it.
    ///
    /// `None` means no built-in root matched and the caller should fall through
    /// to the host sink — see this module's precedence table for why that is
    /// keyed on [`ParseErrorKind::UnknownCommand`] and not on "any error".
    #[must_use]
    pub(crate) fn run(
        &self,
        effects: &CommandEffects<'_>,
        command: &str,
    ) -> Option<CommandResponse> {
        let parsed = match self.inner.tree.parse(command) {
            Ok(parsed) => parsed,
            // Nothing at the root matched: not ours.
            Err(e) if e.kind == ParseErrorKind::UnknownCommand => return None,
            // A built-in matched and then failed. Ours, and the player is told
            // what actually went wrong.
            Err(e) => return Some(CommandResponse::refused(e.to_string())),
        };

        // Brigadier's own rule: the deepest matched node owns the callback.
        let Some(last) = parsed.nodes.last().copied() else {
            // Structurally unreachable — a successful parse entered at least
            // one node, or `parse` would have answered `UnknownCommand` above.
            // Refused rather than `None`, because falling through here would
            // hand the host a string the tree claimed to have handled.
            return Some(CommandResponse::refused(crate::command::UNKNOWN_COMMAND));
        };
        match self.inner.executors.get(&last) {
            Some(executor) => Some(executor(effects, &parsed)),
            // The node parsed and is executable but has no executor. That is a
            // registration bug in this module, not player error, so it must not
            // masquerade as "unknown command".
            None => Some(CommandResponse::refused(
                "That command is registered but has no behaviour — this is a server bug",
            )),
        }
    }
}

/// Builds `/gamerule`, one literal per rule, exactly as vanilla's
/// `GameRuleCommand` does.
///
/// # Why one literal per rule rather than one string argument
///
/// `/gamerule <name> <value>` with a single string argument would be a third of
/// the nodes and would be wrong in two ways that matter:
///
/// * **The value's type depends on the rule.** `random_tick_speed` takes an
///   integer with a declared range; `keep_inventory` takes a boolean. One
///   argument node cannot be both, so the type check would move into the
///   executor — after the tree already accepted the input, which is precisely
///   the layering vanilla avoids by giving each rule its own subtree with its
///   own `ArgumentType` (`GameRules.java`'s `registerBoolean`/`registerInteger`
///   each carry the `ArgumentType` the command node uses).
/// * **Suggestions.** A rule name is a closed set. One literal per rule means
///   `CommandTree::suggest` offers exactly the valid names, for free, and a
///   value slot offers `true`/`false` where that is what it takes.
fn register_gamerule(tree: &mut CommandTree, executors: &mut HashMap<NodeId, Executor>) {
    let root = tree.root();
    // Not executable: vanilla has no bare `/gamerule`, and leaving it
    // non-executable is what makes the tree answer `NotExecutable` (an
    // "incomplete command" the player is told about) rather than silently
    // succeeding.
    let gamerule = tree.add_literal(root, "gamerule");

    for spec in GAME_RULES {
        let rule = tree.add_literal(gamerule, spec.name);

        // `/gamerule <rule>` — query. Vanilla's `commands.gamerule.query`.
        tree.set_executable(rule, true);
        let name = spec.name;
        executors.insert(
            rule,
            Arc::new(move |effects: &CommandEffects<'_>, _parsed: &ParsedCommand| {
                let value = effects
                    .rules
                    .with(|rules| rules.get(name))
                    .expect("a rule built from GAME_RULES is always in GAME_RULES");
                CommandResponse::Ran {
                    feedback: vec![format!(
                        "Gamerule {name} is currently set to: {}",
                        value.serialize()
                    )],
                }
            }),
        );

        // `/gamerule <rule> <value>` — set. The argument's type and range come
        // from the rule's own spec, so the tree rejects a bad value before any
        // executor runs.
        let value_node = match spec.default {
            GameRuleValue::Bool(_) => {
                tree.add_argument(rule, "value", Arc::new(BoolArgument))
            }
            GameRuleValue::Int(_) => tree.add_argument(
                rule,
                "value",
                Arc::new(IntegerArgument::bounded(
                    spec.min.unwrap_or(i32::MIN),
                    spec.max.unwrap_or(i32::MAX),
                )),
            ),
        };
        tree.set_executable(value_node, true);
        executors.insert(
            value_node,
            Arc::new(move |effects: &CommandEffects<'_>, parsed: &ParsedCommand| {
                // Re-serialized from the *parsed* value rather than re-read
                // from the input text, so the string that reaches
                // `GameRules::set` is one the tree already type-checked. The
                // round trip through a string looks redundant and is not: it
                // keeps `GameRules::set` the single validating entry point for
                // both this command and the `SET_GAME_RULE` packet, so the two
                // transports cannot drift apart on what they accept.
                let raw = match parsed.argument("value") {
                    Some(ParsedValue::Bool(b)) => b.to_string(),
                    Some(ParsedValue::Integer(i)) => i.to_string(),
                    // Unreachable: this node's argument type produces only the
                    // two variants above.
                    _ => {
                        return CommandResponse::refused(
                            "That game rule's value could not be read — this is a server bug",
                        );
                    }
                };
                match effects.rules.with(|rules| rules.set(name, &raw)) {
                    Ok(value) => CommandResponse::Ran {
                        feedback: vec![format!(
                            "Gamerule {name} is now set to: {}",
                            value.serialize()
                        )],
                    },
                    // The tree already applied this rule's own type and range,
                    // so reaching here means the tree and the spec disagree.
                    // Reported rather than unwrapped: a panic here would take
                    // the caller's connection down (`CommandSink::run`'s own
                    // "must not panic" contract, for the same reason).
                    Err(e) => CommandResponse::refused(e.to_string()),
                }
            }),
        );
    }

    // Silences the unused-field warning until a built-in needs the caller.
    // `caller` is on `CommandEffects` deliberately: the identity is the one
    // thing `crate::server` can supply and a built-in cannot forge, and a
    // command that needs it (`/kill`, `/tp`) should not have to change this
    // struct's shape to get it.
    let _ = |effects: &CommandEffects<'_>| effects.caller;
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn caller() -> CommandCaller {
        CommandCaller::new(Uuid::from_u128(11), "tester")
    }

    fn run(commands: &ServerCommands, rules: &GameRulesHandle, text: &str) -> Option<CommandResponse> {
        let caller = caller();
        let effects = CommandEffects { caller: &caller, rules };
        commands.run(&effects, text)
    }

    /// The whole point of #48's server half: a command string produces a real
    /// change in world state, not a parse tree.
    #[test]
    fn gamerule_set_changes_the_shared_store_and_query_reads_it_back() {
        let commands = ServerCommands::new();
        let rules = GameRulesHandle::new();

        // Predicted exactly, not "changed": the default is 3
        // (`GameRules.java:74`), so a query before any set must read 3.
        assert_eq!(rules.random_tick_speed(), 3);
        let queried = run(&commands, &rules, "gamerule random_tick_speed")
            .expect("gamerule is a built-in root");
        assert_eq!(queried.lines(), ["Gamerule random_tick_speed is currently set to: 3"]);

        let set = run(&commands, &rules, "gamerule random_tick_speed 6")
            .expect("gamerule is a built-in root");
        assert!(set.is_ran(), "the set must run: {set:?}");
        assert_eq!(set.lines(), ["Gamerule random_tick_speed is now set to: 6"]);

        // The effect, read off the store rather than off the response — the
        // response could be right while the write went nowhere.
        assert_eq!(rules.random_tick_speed(), 6);
        assert!(rules.with(|r| r.is_set("random_tick_speed")));
    }

    /// A boolean rule takes a boolean, and the *tree* is what enforces it —
    /// `GameRules::set` never sees `"7"`.
    #[test]
    fn a_boolean_rule_rejects_a_non_boolean_at_the_tree_not_at_the_store() {
        let commands = ServerCommands::new();
        let rules = GameRulesHandle::new();

        let response =
            run(&commands, &rules, "gamerule keep_inventory 7").expect("gamerule root matched");
        assert!(!response.is_ran(), "a non-boolean must not run: {response:?}");
        // Not stored, so the reader still sees the vanilla default.
        assert!(!rules.with(|r| r.is_set("keep_inventory")));
        assert!(!rules.keep_inventory());

        // The control: the same command with a real boolean runs and is stored,
        // so the refusal above was about the value and not about the rule being
        // unreachable.
        let ok = run(&commands, &rules, "gamerule keep_inventory true").expect("root matched");
        assert!(ok.is_ran(), "{ok:?}");
        assert!(rules.keep_inventory());
    }

    /// An integer rule's declared range is enforced by the tree's own
    /// `IntegerArgument`, at the position vanilla enforces it.
    #[test]
    fn an_integer_rules_range_is_enforced_before_any_executor_runs() {
        let commands = ServerCommands::new();
        let rules = GameRulesHandle::new();

        let response = run(&commands, &rules, "gamerule random_tick_speed -1")
            .expect("gamerule root matched");
        assert!(!response.is_ran(), "{response:?}");
        assert!(!rules.with(|r| r.is_set("random_tick_speed")));
        assert_eq!(rules.random_tick_speed(), 3, "still the default");

        // `max_snow_accumulation_height` is `integer(..., 1, 0, 8)` — the one
        // rule with a real upper bound, so it separates "range enforced" from
        // "only the lower bound enforced".
        assert!(
            run(&commands, &rules, "gamerule max_snow_accumulation_height 8")
                .expect("root matched")
                .is_ran()
        );
        assert!(
            !run(&commands, &rules, "gamerule max_snow_accumulation_height 9")
                .expect("root matched")
                .is_ran()
        );
    }

    /// The precedence rule, and the reason it is not "fall through on any
    /// error": a root we do not own must fall through, and a root we *do* own
    /// must not.
    #[test]
    fn an_unknown_root_falls_through_but_a_bad_argument_to_a_known_root_does_not() {
        let commands = ServerCommands::new();
        let rules = GameRulesHandle::new();

        assert!(
            run(&commands, &rules, "warp spawn").is_none(),
            "an unknown root must fall through to the host sink"
        );
        assert!(
            run(&commands, &rules, "gamerule no_such_rule true").is_some(),
            "a known root with a bad argument must be answered here, not by the host"
        );
        assert!(
            run(&commands, &rules, "gamerule").is_some(),
            "a bare `/gamerule` is incomplete, not unknown"
        );
    }

    /// A bare `/gamerule` is not executable, matching vanilla, and the player is
    /// told it is incomplete rather than that it does not exist.
    #[test]
    fn a_bare_gamerule_is_incomplete_rather_than_unknown() {
        let commands = ServerCommands::new();
        let rules = GameRulesHandle::new();
        let response = run(&commands, &rules, "gamerule").expect("root matched");
        assert!(!response.is_ran());
        let line = &response.lines()[0];
        assert!(
            line.contains("incomplete"),
            "expected an incomplete-command message, got {line:?}"
        );
        assert_ne!(
            line, crate::command::UNKNOWN_COMMAND,
            "an incomplete built-in must not be reported as an unknown command"
        );
    }

    /// Every rule in [`GAME_RULES`] is reachable through the command, and every
    /// executable node has an executor.
    ///
    /// This is the registration gate: a rule added to the table but not to the
    /// tree, or a node marked executable with nothing behind it, is exactly the
    /// island class this module's own doc warns about, and neither is visible in
    /// any single-rule test above.
    #[test]
    fn every_rule_is_settable_through_the_command() {
        let commands = ServerCommands::new();
        let rules = GameRulesHandle::new();
        assert!(!GAME_RULES.is_empty());

        for spec in GAME_RULES {
            // A value that is legal for this rule: the opposite of its default
            // for a boolean, its own minimum for an integer.
            let value = match spec.default {
                GameRuleValue::Bool(b) => (!b).to_string(),
                GameRuleValue::Int(_) => spec.min.unwrap_or(0).to_string(),
            };
            let response = run(&commands, &rules, &format!("gamerule {} {value}", spec.name))
                .unwrap_or_else(|| panic!("`gamerule {}` must be a built-in", spec.name));
            assert!(
                response.is_ran(),
                "`gamerule {} {value}` must run, got {response:?}",
                spec.name
            );
            assert_eq!(
                rules
                    .with(|r| r.get(spec.name))
                    .expect("rule exists")
                    .serialize(),
                value,
                "`gamerule {}` reported success without storing the value",
                spec.name
            );
        }
        // Two rules' worth of writes must both survive — a store that kept only
        // the most recent write would pass every per-rule assertion above.
        assert_eq!(rules.with(|r| r.entries().len()), GAME_RULES.len());
    }

    /// Suggestions come from the tree, so the rule names are a closed set the
    /// player cannot mistype past.
    #[test]
    fn rule_names_and_boolean_values_are_suggested() {
        let commands = ServerCommands::new();
        let names = commands.suggest("gamerule random_tick");
        assert!(
            names.iter().any(|s| s == "random_tick_speed"),
            "expected random_tick_speed among {names:?}"
        );
        let values = commands.suggest("gamerule keep_inventory ");
        assert!(
            values.iter().any(|s| s == "true") && values.iter().any(|s| s == "false"),
            "expected true/false among {values:?}"
        );
    }
}
