//! `/gamerule` — one literal per rule, exactly as `GameRuleCommand` does.
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
//!   the layering vanilla avoids by giving each rule its own subtree with its
//!   own `ArgumentType` (`GameRules.java`'s `registerBoolean`/`registerInteger`
//!   each carry the `ArgumentType` the command node uses).
//! * **Suggestions.** A rule name is a closed set. One literal per rule means
//!   `CommandTree::suggest` offers exactly the valid names, for free, and a value
//!   slot offers `true`/`false` where that is what it takes.
//!
//! # One known, deliberate divergence from the captured vanilla tree
//!
//! Vanilla registers **two** literals per rule — `keep_inventory` *and*
//! `minecraft:keep_inventory` — which the captured 26.2 tree confirms
//! (`command_tree_creative.hex` has both, 2 × 58 rule literals). This registers
//! only the unprefixed form. That is a gap in *tree parity* for `/gamerule` and
//! nothing else: both literals lead to identical subtrees, so no behaviour
//! differs for anyone typing the ordinary spelling, and closing it is one line in
//! the loop below whenever `/gamerule`'s own parity gate is written. Recorded here
//! rather than discovered later.

use lodestone_command::{BoolArgument, IntegerArgument};

use super::registrar::Registrar;
use crate::game_rules::{GAME_RULES, GameRuleValue};

/// Vanilla gates `/gamerule` at `Commands.LEVEL_GAMEMASTERS` (2).
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
        let rule = registrar.literal(gamerule, spec.name);
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
