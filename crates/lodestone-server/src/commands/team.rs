//! `/team`, from `TeamCommand.java` (issue #48's remainder — explicitly
//! *not* unlocked by `/scoreboard` landing: vanilla keeps teams and the
//! scoreboard as two separate `Scoreboard`-owned tables, and this crate now
//! does too, behind `crate::commands::team_store`).
//!
//! # What is built
//!
//! `list [<team>]`, `add <team> [<displayName>]`, `remove <team>`, `empty
//! <team>`, `join <team> [<members>]`, `leave <members>`, and `modify <team>
//! <option> <value>` for every option vanilla's own `TeamCommand.java`
//! registers: `displayName`, `color`, `friendlyfire`,
//! `seeFriendlyInvisibles`, `nametagVisibility`, `deathMessageVisibility`,
//! `collisionRule`, `prefix`, `suffix`.
//!
//! `<members>` reuses [`ScoreHolderArg`]/[`super::scoreboard::resolve_many`]
//! rather than [`EntityArg`] — the identical grammar `/scoreboard players`
//! uses, and vanilla's own `TeamCommand` registers it the same way
//! (`ScoreHolderArgument.greedyScoreHolder()`), so a selector, `*`, or a bare
//! "fake player" name all mean the same thing there and here.
//!
//! # What is not built, and why
//!
//! `displayName`/`prefix`/`suffix` accept plain text
//! ([`lodestone_command::StringArgument::greedy`]), not vanilla's JSON text
//! component — this crate has no textual component parser anywhere (the same
//! honest omission `crate::commands::scoreboard`'s module doc names for
//! `/scoreboard objectives add`'s `displayName`). Nothing in this crate
//! renders a nametag, a below-name line, or a `/team list` colour, so
//! `friendlyfire`/`seeFriendlyInvisibles`/`collisionRule` are stored and
//! reported back but not yet consulted by the mob/combat simulation — see
//! `crate::commands::team_store`'s own module doc.

use lodestone_command::{IntegerArgument, StringArgument};
use lodestone_command_mc::{ScoreHolderArg, TeamArg, TeamColorArg};

use super::registrar::{ArgKey, Ctx, Registrar};
use super::scoreboard::resolve_many;
use super::team_store::{CollisionRule, TeamError, Visibility};
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`, same as `/scoreboard`.
const TEAM_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let team = registrar.literal(root, "team");
    registrar.require_level(team, TEAM_LEVEL);

    register_list(registrar, team);
    register_add(registrar, team);
    register_remove(registrar, team);
    register_empty(registrar, team);
    register_join(registrar, team);
    register_leave(registrar, team);
    register_modify(registrar, team);
}

// ---- list ---------------------------------------------------------------

fn register_list(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let list = registrar.literal(team, "list");
    registrar.exec(list, |ctx| {
        let all = ctx.world.state.team().teams();
        if all.is_empty() {
            ctx.send_success("There are no teams");
        } else {
            let names = all.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ");
            ctx.send_success(format!("There are {} team(s): {names}", all.len()));
        }
        Ok(i32::try_from(all.len()).unwrap_or(i32::MAX))
    });

    let (name_node, name_key) = registrar.arg(list, "team", TeamArg);
    registrar.exec(name_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        let team = get_team(ctx, &name)?;
        let count = team.members.len();
        if team.members.is_empty() {
            ctx.send_success(format!("Team {name} has no members"));
        } else {
            ctx.send_success(format!(
                "Team {name} has {count} member(s): {}",
                team.members.join(", ")
            ));
        }
        // Every `/team modify`-able field, echoed back — the read side that
        // makes each of them a real, production-reachable value rather than
        // write-only storage. `friendlyfire`/`seeFriendlyInvisibles`/
        // `collisionRule` are still not *enforced* by the combat/mob
        // simulation (see this module's own doc), but that is a documented
        // reduction; reading them back here is not the same gap as never
        // reading them at all.
        ctx.send_success(describe_team_options(&team));
        Ok(i32::try_from(count).unwrap_or(i32::MAX))
    });
}

/// `/team list <team>`'s second feedback line — every configurable field
/// vanilla's own `PlayerTeam` carries, in one place so `add`/`modify`'s own
/// confirmation lines do not need to duplicate this formatting.
fn describe_team_options(team: &super::team_store::Team) -> String {
    let color = team.color.map_or_else(|| "reset".to_string(), |c| c.name());
    format!(
        "{name} displays as \"{display}\", colour {color}, prefix {prefix:?}, suffix {suffix:?}, \
         friendlyFire={ff}, seeFriendlyInvisibles={sfi}, nametagVisibility={ntv}, \
         deathMessageVisibility={dmv}, collisionRule={cr}",
        name = team.name,
        display = team.display_name,
        prefix = team.prefix,
        suffix = team.suffix,
        ff = team.friendly_fire,
        sfi = team.see_friendly_invisibles,
        ntv = team.nametag_visibility.wire_name(),
        dmv = team.death_message_visibility.wire_name(),
        cr = team.collision_rule.wire_name(),
    )
}

// ---- add / remove ---------------------------------------------------------

fn register_add(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let add = registrar.literal(team, "add");
    let (name_node, name_key) = registrar.arg(add, "team", TeamArg);
    registrar.exec(name_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        add_team(ctx, &name, &name.clone())
    });

    let (display_node, display_key) = registrar.arg(name_node, "displayName", StringArgument::greedy());
    registrar.exec(display_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        let display = ctx.get(display_key).clone();
        add_team(ctx, &name, &display)
    });
}

fn add_team(ctx: &mut Ctx<'_>, name: &str, display: &str) -> CommandResult {
    match ctx.world.state.team().add_team(name, display) {
        Ok(()) => {
            ctx.send_success(format!("Created team {name}"));
            Ok(1)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn register_remove(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let remove = registrar.literal(team, "remove");
    let (name_node, name_key) = registrar.arg(remove, "team", TeamArg);
    registrar.exec(name_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        match ctx.world.state.team().remove_team(&name) {
            Ok(()) => {
                ctx.send_success(format!("Removed team {name}"));
                Ok(1)
            }
            Err(e) => Err(e.to_string()),
        }
    });
}

// ---- empty ----------------------------------------------------------------

fn register_empty(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let empty = registrar.literal(team, "empty");
    let (name_node, name_key) = registrar.arg(empty, "team", TeamArg);
    registrar.exec(name_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        match ctx.world.state.team().empty(&name) {
            Ok(count) => {
                ctx.send_success(format!("Removed {count} member(s) from team {name}"));
                Ok(i32::try_from(count).unwrap_or(i32::MAX))
            }
            Err(e) => Err(e.to_string()),
        }
    });
}

// ---- join / leave -----------------------------------------------------------

fn register_join(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let join = registrar.literal(team, "join");
    let (name_node, name_key) = registrar.arg(join, "team", TeamArg);
    // `join <team>` with no `<members>` — vanilla defaults to the caller
    // (`TeamCommand`'s no-members overload resolves `context.getSource()
    // .getEntityOrException()`'s own name).
    registrar.exec(name_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        let holder = ctx.source.name.clone();
        join_one(ctx, &name, &[holder])
    });

    let (members_node, members_key) = registrar.arg(name_node, "members", ScoreHolderArg::multiple());
    registrar.exec(members_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        let holders = resolve_many(ctx, members_key)?;
        join_one(ctx, &name, &holders)
    });
}

fn join_one(ctx: &mut Ctx<'_>, name: &str, holders: &[String]) -> CommandResult {
    for holder in holders {
        ctx.world.state.team().join(name, holder).map_err(|e| e.to_string())?;
    }
    ctx.send_success(format!("Added {} member(s) to team {name}", holders.len()));
    Ok(i32::try_from(holders.len()).unwrap_or(i32::MAX))
}

fn register_leave(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let leave = registrar.literal(team, "leave");
    let (members_node, members_key) = registrar.arg(leave, "members", ScoreHolderArg::multiple());
    registrar.exec(members_node, move |ctx| {
        let holders = resolve_many(ctx, members_key)?;
        let mut removed = 0;
        for holder in &holders {
            if ctx.world.state.team().leave(holder) {
                removed += 1;
            }
        }
        ctx.send_success(format!("Removed {removed} member(s) from their team(s)"));
        Ok(removed)
    });
}

// ---- modify -----------------------------------------------------------------

fn register_modify(registrar: &mut Registrar, team: lodestone_command::NodeId) {
    let modify = registrar.literal(team, "modify");
    let (name_node, name_key) = registrar.arg(modify, "team", TeamArg);

    register_text_option(registrar, name_node, name_key, "displayName", |t, v| t.display_name = v);
    register_text_option(registrar, name_node, name_key, "prefix", |t, v| t.prefix = v);
    register_text_option(registrar, name_node, name_key, "suffix", |t, v| t.suffix = v);

    register_bool_option(registrar, name_node, name_key, "friendlyfire", |t, v| t.friendly_fire = v);
    register_bool_option(registrar, name_node, name_key, "seeFriendlyInvisibles", |t, v| {
        t.see_friendly_invisibles = v;
    });

    let color = registrar.literal(name_node, "color");
    let (color_value_node, color_value_key) = registrar.arg(color, "value", TeamColorArg);
    registrar.exec(color_value_node, move |ctx| {
        let name = ctx.get(name_key).clone();
        let value = *ctx.get(color_value_key);
        apply_modify(ctx, &name, move |t| t.color = value)
    });

    register_visibility_option(registrar, name_node, name_key, "nametagVisibility", |t, v| {
        t.nametag_visibility = v;
    });
    register_visibility_option(registrar, name_node, name_key, "deathMessageVisibility", |t, v| {
        t.death_message_visibility = v;
    });
    register_collision_option(registrar, name_node, name_key);
}

fn register_text_option(
    registrar: &mut Registrar,
    team_node: lodestone_command::NodeId,
    team_key: ArgKey<String>,
    literal: &str,
    apply: fn(&mut super::team_store::Team, String),
) {
    let option = registrar.literal(team_node, literal);
    let (value_node, value_key) = registrar.arg(option, "value", StringArgument::greedy());
    registrar.exec(value_node, move |ctx| {
        let name = ctx.get(team_key).clone();
        let value = ctx.get(value_key).clone();
        apply_modify(ctx, &name, move |t| apply(t, value))
    });
}

fn register_bool_option(
    registrar: &mut Registrar,
    team_node: lodestone_command::NodeId,
    team_key: ArgKey<String>,
    literal: &str,
    apply: fn(&mut super::team_store::Team, bool),
) {
    let option = registrar.literal(team_node, literal);
    let (value_node, value_key) = registrar.arg(option, "value", lodestone_command::BoolArgument);
    registrar.exec(value_node, move |ctx| {
        let name = ctx.get(team_key).clone();
        let value = *ctx.get(value_key);
        apply_modify(ctx, &name, move |t| apply(t, value))
    });
}

/// `nametagVisibility`/`deathMessageVisibility` — vanilla registers the four
/// [`Visibility`] tokens as literal children rather than a generic argument
/// type (`TeamCommand.addTeamOptions`'s own shape for these two), so this
/// does the same instead of inventing a wire type nothing else needs.
fn register_visibility_option(
    registrar: &mut Registrar,
    team_node: lodestone_command::NodeId,
    team_key: ArgKey<String>,
    literal: &'static str,
    apply: fn(&mut super::team_store::Team, Visibility),
) {
    let option = registrar.literal(team_node, literal);
    for (token, value) in [
        ("always", Visibility::Always),
        ("never", Visibility::Never),
        ("hideForOtherTeams", Visibility::HideForOtherTeams),
        ("hideForOwnTeam", Visibility::HideForOwnTeam),
    ] {
        let value_node = registrar.literal(option, token);
        registrar.exec(value_node, move |ctx| {
            let name = ctx.get(team_key).clone();
            apply_named(ctx, &name, literal, value.wire_name(), move |t| apply(t, value))
        });
    }
}

fn register_collision_option(
    registrar: &mut Registrar,
    team_node: lodestone_command::NodeId,
    team_key: ArgKey<String>,
) {
    let option = registrar.literal(team_node, "collisionRule");
    for (token, value) in [
        ("always", CollisionRule::Always),
        ("never", CollisionRule::Never),
        ("pushOwnTeam", CollisionRule::PushOwnTeam),
        ("pushOtherTeams", CollisionRule::PushOtherTeams),
    ] {
        let value_node = registrar.literal(option, token);
        registrar.exec(value_node, move |ctx| {
            let name = ctx.get(team_key).clone();
            apply_named(ctx, &name, "collisionRule", value.wire_name(), move |t| t.collision_rule = value)
        });
    }
}

fn apply_modify(
    ctx: &mut Ctx<'_>,
    name: &str,
    f: impl FnOnce(&mut super::team_store::Team),
) -> CommandResult {
    match ctx.world.state.team().modify(name, f) {
        Ok(()) => {
            ctx.send_success(format!("Updated team {name}"));
            Ok(1)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// [`apply_modify`], with a confirmation message naming the option and the
/// value it was set to — used by the three option kinds registered as
/// literal tokens (`nametagVisibility`/`deathMessageVisibility`/
/// `collisionRule`), where the value is already a `&'static str` and a
/// generic "Updated team X" would throw that away for no reason.
fn apply_named(
    ctx: &mut Ctx<'_>,
    name: &str,
    option: &str,
    value: &str,
    f: impl FnOnce(&mut super::team_store::Team),
) -> CommandResult {
    match ctx.world.state.team().modify(name, f) {
        Ok(()) => {
            ctx.send_success(format!("Set {option} for team {name} to {value}"));
            Ok(1)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn get_team(ctx: &Ctx<'_>, name: &str) -> Result<super::team_store::Team, String> {
    ctx.world.state.team().team(name).ok_or_else(|| TeamError::Unknown(name.to_string()).to_string())
}
