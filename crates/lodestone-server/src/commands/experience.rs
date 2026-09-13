//! `/experience` (`/xp`).
//!
//! # `set` zeroes first, `add` does not
//!
//! [`crate::experience::PlayerExperience`] has no direct level/points setter —
//! only [`give_points`](crate::experience::PlayerExperience::give_points) (a
//! points *delta*, which correctly re-derives level and progress through the
//! carry algorithm) and [`take_levels`](crate::experience::PlayerExperience::take_levels)
//! (a level delta downward, the same operation an enchanting-table hit
//! performs on the player who paid the levels).
//! `/xp set` is applied by zeroing the target's experience
//! ([`crate::experience::PlayerExperience::respawn`], which resets to
//! `Default`) and then applying the requested *absolute* value from that known
//! zero — `give_points(amount)` for points, `take_levels(-amount)` for levels
//! (subtracting a negative level delta is exactly "gain `amount` levels", the
//! same trick `/xp add … levels` uses below). This is an approximation of the
//! real level/points setters, which do not necessarily zero progress first;
//! documented rather than silent, since no test here can currently observe
//! the difference.
//!
//! # Targets are `players()`; see [`crate::commands::kill`]'s module doc for why

use lodestone_command::IntegerArgument;
use lodestone_command_mc::EntityArg;

use super::registrar::{Ctx, Registrar};
use super::CommandResult;
use crate::commands::Effect;

/// The game-masters permission level.
const XP_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    for name in ["experience", "xp"] {
        let xp = registrar.literal(root, name);
        registrar.require_level(xp, XP_LEVEL);
        register_add(registrar, xp);
        register_set(registrar, xp);
        register_query(registrar, xp);
    }
}

fn register_add(registrar: &mut Registrar, xp: lodestone_command::NodeId) {
    let add = registrar.literal(xp, "add");
    let (targets_node, targets_key) = registrar.arg(add, "targets", EntityArg::players());
    let (amount_node, amount_key) =
        registrar.arg(targets_node, "amount", IntegerArgument::bounded(0, i32::MAX));
    // Default (no unit literal) is points — vanilla's own default overload.
    registrar.exec(amount_node, move |ctx| {
        apply(ctx, targets_key, Effect::GiveExperience { levels: false, amount: *ctx.get(amount_key) })
    });
    let levels = registrar.literal(amount_node, "levels");
    registrar.exec(levels, move |ctx| {
        apply(ctx, targets_key, Effect::GiveExperience { levels: true, amount: *ctx.get(amount_key) })
    });
    let points = registrar.literal(amount_node, "points");
    registrar.exec(points, move |ctx| {
        apply(ctx, targets_key, Effect::GiveExperience { levels: false, amount: *ctx.get(amount_key) })
    });
}

fn register_set(registrar: &mut Registrar, xp: lodestone_command::NodeId) {
    let set = registrar.literal(xp, "set");
    let (targets_node, targets_key) = registrar.arg(set, "targets", EntityArg::players());
    let (amount_node, amount_key) =
        registrar.arg(targets_node, "amount", IntegerArgument::bounded(0, i32::MAX));
    registrar.exec(amount_node, move |ctx| {
        apply(ctx, targets_key, Effect::SetExperience { levels: false, amount: *ctx.get(amount_key) })
    });
    let levels = registrar.literal(amount_node, "levels");
    registrar.exec(levels, move |ctx| {
        apply(ctx, targets_key, Effect::SetExperience { levels: true, amount: *ctx.get(amount_key) })
    });
    let points = registrar.literal(amount_node, "points");
    registrar.exec(points, move |ctx| {
        apply(ctx, targets_key, Effect::SetExperience { levels: false, amount: *ctx.get(amount_key) })
    });
}

fn register_query(registrar: &mut Registrar, xp: lodestone_command::NodeId) {
    let query_root = registrar.literal(xp, "query");
    let (targets_node, targets_key) = registrar.arg(query_root, "targets", EntityArg::player());
    let levels = registrar.literal(targets_node, "levels");
    registrar.exec(levels, move |ctx| query(ctx, targets_key, false));
    let points = registrar.literal(targets_node, "points");
    registrar.exec(points, move |ctx| query(ctx, targets_key, true));
}

fn apply(
    ctx: &mut Ctx<'_>,
    targets_key: super::registrar::ArgKey<lodestone_command_mc::EntitySelector>,
    effect: Effect,
) -> CommandResult {
    let selector = ctx.get(targets_key).clone();
    let targets = ctx.resolve(&selector)?;
    for target in &targets {
        ctx.effect(target.uuid, effect.clone());
    }
    if let [only] = targets.as_slice() {
        ctx.send_success(format!("Granted experience to {}", only.username));
    } else {
        ctx.send_success(format!("Granted experience to {} players", targets.len()));
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}

fn query(
    ctx: &mut Ctx<'_>,
    targets_key: super::registrar::ArgKey<lodestone_command_mc::EntitySelector>,
    points: bool,
) -> CommandResult {
    let selector = ctx.get(targets_key).clone();
    let targets = ctx.resolve(&selector)?;
    let Some(target) = targets.first() else {
        return Err("No player was found".to_string());
    };
    // `PlayerCandidate::xp_level`/`xp_points` — the same producer/mirror split
    // `game_mode` already has, republished by `crate::server::republish_experience`
    // at every site that sends the client's set-experience packet to the
    // target's own connection. `points` here is the query formula
    // (`floor(experienceProgress * xp_needed_for_next_level)`, see
    // `crate::experience::PlayerExperience::query_points`'s own doc), not the
    // lifetime total.
    let result = if points { target.xp_points } else { target.xp_level };
    let unit = if points { "experience points" } else { "experience levels" };
    // The real message shape: `"%s has %s <points|levels>"`, and its return
    // value is the queried number.
    ctx.send_success(format!("{} has {result} {unit}", target.username));
    Ok(result)
}
