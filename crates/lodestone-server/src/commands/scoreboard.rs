//! `/scoreboard`, from `ScoreboardCommand.java` (issue #48's remainder — the
//! part of `/execute store`/`if score` that needed a real store to exist
//! before either could be built; see `crate::commands::execute`'s module doc
//! for the conditional half this unlocks).
//!
//! # What is built
//!
//! `objectives add/remove/list`, and `players set/add/remove/get/reset/
//! list/operation`. See `crate::commands::scoreboard_store`'s module doc for
//! the store itself and what "criteria" means here (nothing — every score
//! moves only because a command asked it to).
//!
//! # What is not built
//!
//! * **`objectives setdisplay`** and every display-slot concept
//!   (`minecraft:scoreboard_slot`). Nothing in this crate renders a sidebar
//!   or a below-name tag, so a stored display slot would be write-only —
//!   the same honest omission `crate::commands::scoreboard_store`'s module
//!   doc names for criteria.
//! * **`players enable`** (trigger criteria) — meaningless with no criteria
//!   semantics modelled at all.
//! * **Selector/`*` holders in `get`.** Vanilla's `get` target is
//!   `score_holder` *single*; this uses [`ScoreHolderArg::single`], which
//!   still accepts a selector or `*` grammatically — resolved down to
//!   "exactly one holder" the same way `players operation`'s source is, and
//!   refused otherwise (`*`/a multi-match selector has no single score to
//!   report).
//!
//! # `targets`/`source` resolution
//!
//! [`ScoreHolderInput::All`] resolves to every name
//! [`ScoreboardHandle::tracked_holders`] currently knows of;
//! [`ScoreHolderInput::Selector`] resolves through the ordinary roster
//! resolver ([`Ctx::resolve`]) to online players' usernames;
//! [`ScoreHolderInput::Name`] is used literally, which is what makes a "fake
//! player" counter name (`TIMER`, `KILLS_TOTAL`, …) work at all — it never
//! needs to resolve to anything, unlike every other `<targets>` argument
//! this crate has.

use lodestone_command::IntegerArgument;
use lodestone_command_mc::{ObjectiveArg, ObjectiveCriteriaArg, OperationArg, ScoreHolderArg, ScoreHolderInput};

use super::registrar::{ArgKey, Ctx, Registrar};
use super::scoreboard_store::ScoreboardError;
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const SCOREBOARD_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let scoreboard = registrar.literal(root, "scoreboard");
    registrar.require_level(scoreboard, SCOREBOARD_LEVEL);

    register_objectives(registrar, scoreboard);
    register_players(registrar, scoreboard);
}

// ---- objectives -------------------------------------------------------------

fn register_objectives(registrar: &mut Registrar, scoreboard: lodestone_command::NodeId) {
    let objectives = registrar.literal(scoreboard, "objectives");

    let list = registrar.literal(objectives, "list");
    registrar.exec(list, |ctx| {
        let all = ctx.world.state.scoreboard().objectives();
        if all.is_empty() {
            ctx.send_success("There are no scoreboard objectives");
        } else {
            let names = all.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", ");
            ctx.send_success(format!("There are {} objective(s): {names}", all.len()));
        }
        Ok(i32::try_from(all.len()).unwrap_or(i32::MAX))
    });

    let add = registrar.literal(objectives, "add");
    let (name_node, name_key) = registrar.arg(add, "objective", ObjectiveArg);
    let (criteria_node, criteria_key) = registrar.arg(name_node, "criteria", ObjectiveCriteriaArg);
    // No `<displayName>` — vanilla's is `ComponentArgument.textComponent()`
    // (JSON text), which this crate has no textual parser for (see this
    // module's doc). Defaulting the display name to the objective's own
    // name is vanilla's own fallback (`ObjectiveCommand`'s no-display-name
    // overload uses `PlainTextContents.LiteralContents(name)`).
    registrar.exec(criteria_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        let criteria = ctx.get(criteria_key).clone();
        add_objective(ctx, &name, &criteria, &name.clone())
    });

    let remove = registrar.literal(objectives, "remove");
    let (remove_node, remove_key) = registrar.arg(remove, "objective", ObjectiveArg);
    registrar.exec(remove_node, move |ctx| {
        let name = ctx.get(remove_key).clone();
        match ctx.world.state.scoreboard().remove_objective(&name) {
            Ok(()) => {
                ctx.send_success(format!("Removed objective {name}"));
                Ok(1)
            }
            Err(e) => Err(e.to_string()),
        }
    });
}

fn add_objective(ctx: &mut Ctx<'_>, name: &str, criteria: &str, display_name: &str) -> CommandResult {
    match ctx.world.state.scoreboard().add_objective(name, criteria, display_name) {
        Ok(()) => {
            ctx.send_success(format!("Created new objective {name}"));
            Ok(1)
        }
        Err(e) => Err(e.to_string()),
    }
}

// ---- players ----------------------------------------------------------------

fn register_players(registrar: &mut Registrar, scoreboard: lodestone_command::NodeId) {
    let players = registrar.literal(scoreboard, "players");

    // `/scoreboard players list [<target>]`.
    let list = registrar.literal(players, "list");
    registrar.exec(list, |ctx| {
        let holders = ctx.world.state.scoreboard().tracked_holders();
        if holders.is_empty() {
            ctx.send_success("There are no tracked players");
        } else {
            ctx.send_success(format!(
                "There are {} tracked player(s): {}",
                holders.len(),
                holders.join(", ")
            ));
        }
        Ok(i32::try_from(holders.len()).unwrap_or(i32::MAX))
    });
    let (list_target_node, list_target_key) = registrar.arg(list, "target", ScoreHolderArg::single());
    registrar.exec(list_target_node, move |ctx| {
        let holder = resolve_single(ctx, list_target_key)?;
        let scores = ctx.world.state.scoreboard().scores_for(&holder);
        if scores.is_empty() {
            ctx.send_success(format!("{holder} has no scores to show"));
        } else {
            let rendered = scores
                .iter()
                .map(|(objective, value)| format!("{objective}: {value}"))
                .collect::<Vec<_>>()
                .join(", ");
            ctx.send_success(format!("{holder} has {} score(s): {rendered}", scores.len()));
        }
        Ok(i32::try_from(scores.len()).unwrap_or(i32::MAX))
    });

    // `/scoreboard players get <target> <objective>`.
    let get = registrar.literal(players, "get");
    let (get_target_node, get_target_key) = registrar.arg(get, "target", ScoreHolderArg::single());
    let (get_obj_node, get_obj_key) = registrar.arg(get_target_node, "objective", ObjectiveArg);
    registrar.exec(get_obj_node, move |ctx| {
        let holder = resolve_single(ctx, get_target_key)?;
        let objective = ctx.get(get_obj_key).clone();
        match ctx.world.state.scoreboard().get_score(&holder, &objective) {
            Ok(value) => {
                ctx.send_success(format!("{holder} has {value} {objective}"));
                Ok(value)
            }
            Err(e) => Err(e.to_string()),
        }
    });

    register_mutation(registrar, players, "set", i32::MIN, i32::MAX, |scoreboard, holder, objective, value| {
        scoreboard.set_score(holder, objective, value)
    });
    register_mutation(registrar, players, "add", 0, i32::MAX, |scoreboard, holder, objective, value| {
        scoreboard.add_score(holder, objective, value)
    });
    register_mutation(registrar, players, "remove", 0, i32::MAX, |scoreboard, holder, objective, value| {
        scoreboard.remove_score(holder, objective, value)
    });

    register_reset(registrar, players);
    register_operation(registrar, players);
}

/// `set`/`add`/`remove` share one shape: `<targets> <objective> <score>`,
/// differing only in the store method they call and the score's legal
/// range. `apply` is a plain `fn` pointer (not a closure) so it can be
/// `Copy`'d into the executor without borrowing anything.
fn register_mutation(
    registrar: &mut Registrar,
    players: lodestone_command::NodeId,
    literal: &str,
    min: i32,
    max: i32,
    apply: fn(&super::scoreboard_store::ScoreboardHandle, &str, &str, i32) -> Result<i32, ScoreboardError>,
) {
    let node = registrar.literal(players, literal);
    let (targets_node, targets_key) = registrar.arg(node, "targets", ScoreHolderArg::multiple());
    let (obj_node, obj_key) = registrar.arg(targets_node, "objective", ObjectiveArg);
    let (score_node, score_key) = registrar.arg(obj_node, "score", IntegerArgument::bounded(min, max));
    registrar.exec(score_node, move |ctx| {
        let holders = resolve_many(ctx, targets_key)?;
        let objective = ctx.get(obj_key).clone();
        let amount = *ctx.get(score_key);
        let mut last = 0;
        for holder in &holders {
            last = apply(ctx.world.state.scoreboard(), holder, &objective, amount).map_err(|e| e.to_string())?;
        }
        if let [only] = holders.as_slice() {
            ctx.send_success(format!("Set {objective} for {only} to {last}"));
        } else {
            ctx.send_success(format!("Changed {objective} for {} players", holders.len()));
        }
        Ok(last)
    });
}

fn register_reset(registrar: &mut Registrar, players: lodestone_command::NodeId) {
    let reset = registrar.literal(players, "reset");
    let (targets_node, targets_key) = registrar.arg(reset, "targets", ScoreHolderArg::multiple());
    registrar.exec(targets_node, move |ctx| {
        let holders = resolve_many(ctx, targets_key)?;
        for holder in &holders {
            ctx.world.state.scoreboard().reset_all(holder);
        }
        ctx.send_success(format!("Reset scores for {} player(s)", holders.len()));
        Ok(i32::try_from(holders.len()).unwrap_or(i32::MAX))
    });
    let (obj_node, obj_key) = registrar.arg(targets_node, "objective", ObjectiveArg);
    registrar.exec(obj_node, move |ctx| {
        let holders = resolve_many(ctx, targets_key)?;
        let objective = ctx.get(obj_key).clone();
        let mut count = 0;
        for holder in &holders {
            if ctx.world.state.scoreboard().reset_score(holder, &objective) {
                count += 1;
            }
        }
        ctx.send_success(format!("Reset {objective} for {count} player(s)"));
        Ok(count)
    });
}

fn register_operation(registrar: &mut Registrar, players: lodestone_command::NodeId) {
    let operation = registrar.literal(players, "operation");
    let (targets_node, targets_key) = registrar.arg(operation, "targets", ScoreHolderArg::multiple());
    let (target_obj_node, target_obj_key) = registrar.arg(targets_node, "targetObjective", ObjectiveArg);
    let (op_node, op_key) = registrar.arg(target_obj_node, "operation", OperationArg);
    let (source_node, source_key) = registrar.arg(op_node, "source", ScoreHolderArg::single());
    let (source_obj_node, source_obj_key) = registrar.arg(source_node, "sourceObjective", ObjectiveArg);
    registrar.exec(source_obj_node, move |ctx| {
        let targets = resolve_many(ctx, targets_key)?;
        let target_objective = ctx.get(target_obj_key).clone();
        let op = *ctx.get(op_key);
        let source = resolve_single(ctx, source_key)?;
        let source_objective = ctx.get(source_obj_key).clone();
        let mut last = 0;
        for target in &targets {
            last = ctx
                .world
                .state
                .scoreboard()
                .operation(target, &target_objective, op, &source, &source_objective)
                .map_err(|e| e.to_string())?;
        }
        ctx.send_success(format!("Updated {target_objective} for {} player(s)", targets.len()));
        Ok(last)
    });
}

// ---- resolution ---------------------------------------------------------

/// A [`ScoreHolderInput`] to every holder name it names. `All` is every
/// tracked holder (see this module's doc); `Selector` resolves through the
/// ordinary roster resolver; `Name` is used literally.
///
/// `pub(super)` rather than private: `crate::commands::execute`'s `if
/// score`/`unless score` needs the identical resolution for its own
/// `<target>`/`<source>` arguments, and a second copy is exactly the kind of
/// thing that drifts.
pub(super) fn resolve_many(ctx: &Ctx<'_>, key: ArgKey<ScoreHolderInput>) -> Result<Vec<String>, String> {
    match ctx.get(key).clone() {
        ScoreHolderInput::All => Ok(ctx.world.state.scoreboard().tracked_holders()),
        ScoreHolderInput::Name(name) => Ok(vec![name]),
        ScoreHolderInput::Selector(selector) => {
            Ok(ctx.resolve(&selector)?.into_iter().map(|c| c.username).collect())
        }
    }
}

/// [`resolve_many`], refused unless it names exactly one holder — `get`'s
/// target and `operation`'s source both need exactly one score to read.
pub(super) fn resolve_single(ctx: &Ctx<'_>, key: ArgKey<ScoreHolderInput>) -> Result<String, String> {
    let holders = resolve_many(ctx, key)?;
    match holders.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err("No player was found".to_string()),
        _ => Err("Only one score holder is allowed here".to_string()),
    }
}
