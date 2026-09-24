//! `/gamerule` — one literal per rule, exactly as the real command does.
//!
//! # Why one literal per rule rather than one string argument
//!
//! `/gamerule <name> <value>` with a single string argument would be a third of
//! the nodes and would be wrong in two ways that matter:
//!
//! * **The value's type depends on the rule.** `random_tick_speed` takes an
//!   integer with a declared range; `keep_inventory` takes a boolean. One
//!   argument node cannot be both, so the type check would move into the
//!   executor — after the tree already accepted the input, which is precisely
//!   the layering the real command avoids by giving each rule its own
//!   subtree with its own argument type, declared alongside the rule itself.
//! * **Suggestions.** A rule name is a closed set. One literal per rule means
//!   `CommandTree::suggest` offers exactly the valid names, for free, and a value
//!   slot offers `true`/`false` where that is what it takes.
//!
//! # Two literals per rule, matching the real command
//!
//! The real registration walks each rule and builds its argument subtree
//! **twice** — once against the bare name (e.g. `keep_inventory`) and once
//! against the namespaced form (`minecraft:keep_inventory`) — each an
//! independently-built, structurally identical subtree hung off the same
//! `/gamerule` root. Neither call site reads the literal's own text back:
//! the query/set handlers both report through the rule's bare id, regardless
//! of which literal a player typed. [`register`] below mirrors that shape
//! exactly: [`register_rule_literal`] is called once per name, and the
//! *set/query* closures it builds always capture `spec.name` (never the
//! literal string), matching the real command's own indifference to which
//! spelling triggered them.
//!
//! `crates/protocol/v770/tests/builtin_command_parity.rs`'s
//! `gamerule_has_every_rule_subtree_right_and_every_literal_too` is the parity
//! gate this satisfies — see that test for the one remaining, deliberate
//! divergence (`max_minecart_speed`, gated behind a feature flag this crate has
//! no concept of).

use lodestone_command::{BoolArgument, IntegerArgument};

use super::registrar::Registrar;
use crate::game_rules::{GAME_RULES, GameRuleValue};

/// The real rule gates `/gamerule` at the game-masters permission level (2).
const GAMERULE_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    // Not executable: vanilla has no bare `/gamerule`, and leaving it
    // non-executable is what makes the tree answer `NotExecutable` (an
    // "incomplete command" the player is told about) rather than silently
    // succeeding.
    let gamerule = registrar.literal(root, "gamerule");
    registrar.require_level(gamerule, GAMERULE_LEVEL);

    for spec in GAME_RULES {
        // Bare (`keep_inventory`) and namespaced (`minecraft:keep_inventory`) —
        // see this module's own doc for why both are independently built rather
        // than one redirecting to the other.
        register_rule_literal(registrar, gamerule, spec, spec.name);
        register_rule_literal(registrar, gamerule, spec, &format!("minecraft:{}", spec.name));
    }
}

/// One rule's full subtree (query + `value` argument), hung off `literal_name`
/// — either `spec.name` or its `minecraft:`-prefixed alias. Every executor
/// closure built here captures `spec.name`, **not** `literal_name`: the real
/// query/set handlers report through the rule's own bare id regardless of
/// which of the two literals a player actually typed, and
/// `GameRules::get_rule`/`set_rule` are keyed on the bare name.
fn register_rule_literal(
    registrar: &mut Registrar,
    gamerule: lodestone_command::NodeId,
    spec: &'static crate::game_rules::GameRuleSpec,
    literal_name: &str,
) {
    let rule = registrar.literal(gamerule, literal_name);
    let name = spec.name;

    // `/gamerule <rule>` — query. Vanilla's `commands.gamerule.query`.
    registrar.exec(rule, move |ctx| {
        let value = ctx
            .world
            .rules
            .get_rule(name)
            .expect("a rule built from GAME_RULES is always in GAME_RULES");
        ctx.send_success(format!(
            "Gamerule {name} is currently set to: {}",
            value.serialize()
        ));
        Ok(1)
    });

    // `/gamerule <rule> <value>` — set. The argument's type and range come
    // from the rule's own spec, so the tree rejects a bad value before any
    // executor runs.
    match spec.default {
        GameRuleValue::Bool(_) => {
            let (node, key) = registrar.arg(rule, "value", BoolArgument);
            registrar.exec(node, move |ctx| set(ctx, name, ctx.get(key).to_string()));
        }
        GameRuleValue::Int(_) => {
            let (node, key) = registrar.arg(
                rule,
                "value",
                IntegerArgument::bounded(
                    spec.min.unwrap_or(i32::MIN),
                    spec.max.unwrap_or(i32::MAX),
                ),
            );
            registrar.exec(node, move |ctx| set(ctx, name, ctx.get(key).to_string()));
        }
    }
}

/// Re-serialized from the *parsed* value rather than re-read from the input text,
/// so the string that reaches `GameRules::set` is one the tree already
/// type-checked. The round trip through a string looks redundant and is not: it
/// keeps `GameRules::set` the single validating entry point for both this command
/// and the `SET_GAME_RULE` packet, so the two transports cannot drift apart on
/// what they accept.
fn set(ctx: &mut super::registrar::Ctx<'_>, name: &str, raw: String) -> super::CommandResult {
    match ctx.world.rules.set_rule(name, &raw) {
        Ok(value) => {
            ctx.send_success(format!("Gamerule {name} is now set to: {}", value.serialize()));
            Ok(1)
        }
        // The tree already applied this rule's own type and range, so reaching
        // here means the tree and the spec disagree. Reported rather than
        // unwrapped: a panic here would take the caller's connection down.
        Err(e) => Err(e.to_string()),
    }
}
