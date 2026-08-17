//! `/op`, `/deop` and `/whitelist` — vanilla's `OpCommand`/`DeOpCommand`/
//! `WhitelistCommand`, the admin surface [`crate::access::AccessLists`]
//! never had one for.
//!
//! # What it is
//!
//! Before this module, nothing in this crate's command tree could reach
//! [`crate::access::AccessLists`] at all: `has_permission_level`, `deop` and
//! `whitelist_remove` had real, tested logic and zero production callers, and
//! so did `op`/`whitelist_add`/`ban`/`pardon`/`whitelisted` — an admin's only
//! way to grant operator status or manage the whitelist was to stop the
//! server and hand-edit `ops.json`/`whitelist.json` (see `crate::access`'s
//! own module doc). Meanwhile [`crate::commands::CommandWorld::access`]
//! (via `permission_level`/`command_permission_level`, already consulted at
//! every command dispatch through [`crate::commands::registrar::level_filter`])
//! was a *read* path that worked fine — the gap was entirely on the write
//! side.
//!
//! # Scoped to RCON, not chat
//!
//! [`crate::commands::CommandWorld::access`] is `Option`, and today exactly
//! one production caller passes `Some`: `crate::rcon::run_command_as`, via
//! [`crate::rcon::RconConfig::access`] — threaded from the same
//! [`crate::access::AccessHandle`] every accepted connection's join check
//! reads (`IntegratedServer::open_to_lan`'s `conn_access`), so an op granted
//! over RCON is real for the very next join. `crate::server::dispatch_play_packet`
//! (in-game chat) passes `None`, so these commands refuse there with "no
//! access list configured" rather than silently doing nothing — deliberate,
//! not an oversight: threading a fresh `&AccessHandle` parameter through
//! that function would widen this crate's single largest parameter list for
//! one command family, and vanilla's own admin workflow for exactly these
//! three commands is the dedicated-server console/RCON anyway, not chat.
//!
//! # Targets are `players()`, matching `/kill`
//!
//! Same narrowing [`crate::commands::kill`] already made, for the same
//! reason: [`crate::commands::CommandWorld`] only ever carries currently
//! *connected* [`crate::commands::PlayerCandidate`]s, so a player must be
//! online to be opped/deopped/whitelisted here. Vanilla's own
//! `GameProfileArgument` can additionally resolve an offline player from its
//! profile cache; this server keeps no such cache, so that half is a
//! disclosed narrowing rather than a silent one.
//!
//! # No disk persistence yet
//!
//! A mutation here changes the live, shared [`crate::access::AccessLists`]
//! immediately — the very next join check and the very next command's
//! permission gate both see it — but nothing calls
//! [`crate::access::AccessHandle::save`] afterward, so a server restart
//! reverts to whatever `ops.json`/`whitelist.json` said on disk. This is the
//! same "process-lifetime gate, not a persisted one" shape
//! `docs/dragon-fight.md` already discloses for `claim_dragon_fight_start`,
//! not a silent gap: `AccessLists::save`/`load` are real and tested, and
//! wiring one onto a save cadence is future work with its own trade-off
//! (save on every mutation vs. batching with the world's own autosave).

use lodestone_command_mc::EntityArg;

use crate::access::MAX_PERMISSION_LEVEL;

use super::registrar::Registrar;

/// `Commands.LEVEL_GAMEMASTERS + 1` — vanilla's own level for `/op`, `/deop`
/// and `/whitelist`.
const ADMIN_LEVEL: u8 = 3;

pub(super) fn register(registrar: &mut Registrar) {
    register_op(registrar);
    register_deop(registrar);
    register_whitelist(registrar);
}

fn register_op(registrar: &mut Registrar) {
    let root = registrar.root();
    let op = registrar.literal(root, "op");
    registrar.require_level(op, ADMIN_LEVEL);

    let (targets_node, targets_key) = registrar.arg(op, "targets", EntityArg::players());
    registrar.exec(targets_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let targets = ctx.resolve(&selector)?;
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        let mut opped = Vec::new();
        for target in &targets {
            // `OpCommand.opPlayer`'s own "nothing changed" refusal: report
            // it, don't just silently re-op.
            let already = access.with(|lists| lists.has_permission_level(target.uuid, MAX_PERMISSION_LEVEL));
            if already {
                continue;
            }
            access.with(|lists| lists.op(target.uuid, target.username.clone(), MAX_PERMISSION_LEVEL));
            opped.push(target.username.as_str());
        }
        match opped.as_slice() {
            [] => Err("Nothing changed. The player(s) are already operators".to_string()),
            [one] => {
                ctx.send_success(format!("Made {one} a server operator"));
                Ok(1)
            }
            many => {
                ctx.send_success(format!("Made {} players server operators", many.len()));
                Ok(i32::try_from(many.len()).unwrap_or(i32::MAX))
            }
        }
    });
}

fn register_deop(registrar: &mut Registrar) {
    let root = registrar.root();
    let deop = registrar.literal(root, "deop");
    registrar.require_level(deop, ADMIN_LEVEL);

    let (targets_node, targets_key) = registrar.arg(deop, "targets", EntityArg::players());
    registrar.exec(targets_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let targets = ctx.resolve(&selector)?;
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        let mut deopped = Vec::new();
        for target in &targets {
            // `AccessLists::deop`'s first production caller — previously
            // tested and dead, so an admin had no in-session way to revoke
            // operator status at all.
            if access.with(|lists| lists.deop(target.uuid)) {
                deopped.push(target.username.as_str());
            }
        }
        match deopped.as_slice() {
            [] => Err("Nothing changed. The player(s) are not operators".to_string()),
            [one] => {
                ctx.send_success(format!("Made {one} no longer a server operator"));
                Ok(1)
            }
            many => {
                ctx.send_success(format!("Made {} players no longer server operators", many.len()));
                Ok(i32::try_from(many.len()).unwrap_or(i32::MAX))
            }
        }
    });
}

fn register_whitelist(registrar: &mut Registrar) {
    let root = registrar.root();
    let whitelist = registrar.literal(root, "whitelist");
    registrar.require_level(whitelist, ADMIN_LEVEL);

    let on = registrar.literal(whitelist, "on");
    registrar.exec(on, |ctx| {
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        access.set_whitelist_enabled(true);
        ctx.send_success("Turned on the whitelist");
        Ok(1)
    });

    let off = registrar.literal(whitelist, "off");
    registrar.exec(off, |ctx| {
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        access.set_whitelist_enabled(false);
        ctx.send_success("Turned off the whitelist");
        Ok(1)
    });

    let list = registrar.literal(whitelist, "list");
    registrar.exec(list, |ctx| {
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        // `AccessLists::whitelisted`'s first production caller — previously
        // tested and dead. No name lookup exists for an arbitrary uuid
        // outside `PlayerCandidate`, so this reports uuids, not usernames —
        // a real, disclosed narrowing rather than vanilla's name list.
        let ids = access.with(|lists| lists.whitelisted());
        if ids.is_empty() {
            ctx.send_success("There are no whitelisted players");
        } else {
            let joined = ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            ctx.send_success(format!("There are {} whitelisted player(s): {joined}", ids.len()));
        }
        Ok(i32::try_from(ids.len()).unwrap_or(i32::MAX))
    });

    let add = registrar.literal(whitelist, "add");
    let (add_targets_node, add_targets_key) = registrar.arg(add, "targets", EntityArg::players());
    registrar.exec(add_targets_node, move |ctx| {
        let selector = ctx.get(add_targets_key).clone();
        let targets = ctx.resolve(&selector)?;
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        let mut added = Vec::new();
        for target in &targets {
            let already = access.with(|lists| lists.whitelisted().contains(&target.uuid));
            if already {
                continue;
            }
            access.with(|lists| lists.whitelist_add(target.uuid, target.username.clone()));
            added.push(target.username.as_str());
        }
        match added.as_slice() {
            [] => Err("Nothing changed. The player(s) are already whitelisted".to_string()),
            [one] => {
                ctx.send_success(format!("Added {one} to the whitelist"));
                Ok(1)
            }
            many => {
                ctx.send_success(format!("Added {} players to the whitelist", many.len()));
                Ok(i32::try_from(many.len()).unwrap_or(i32::MAX))
            }
        }
    });

    let remove = registrar.literal(whitelist, "remove");
    let (remove_targets_node, remove_targets_key) = registrar.arg(remove, "targets", EntityArg::players());
    registrar.exec(remove_targets_node, move |ctx| {
        let selector = ctx.get(remove_targets_key).clone();
        let targets = ctx.resolve(&selector)?;
        let Some(access) = ctx.world.access else {
            return Err("No access list is configured for this world".to_string());
        };
        let mut removed = Vec::new();
        for target in &targets {
            // `AccessLists::whitelist_remove`'s first production caller —
            // previously tested and dead, the other half of the gap this
            // module's doc names.
            if access.with(|lists| lists.whitelist_remove(target.uuid)) {
                removed.push(target.username.as_str());
            }
        }
        match removed.as_slice() {
            [] => Err("Nothing changed. The player(s) are not whitelisted".to_string()),
            [one] => {
                ctx.send_success(format!("Removed {one} from the whitelist"));
                Ok(1)
            }
            many => {
                ctx.send_success(format!("Removed {} players from the whitelist", many.len()));
                Ok(i32::try_from(many.len()).unwrap_or(i32::MAX))
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::access::{AccessHandle, AccessLists};
    use crate::commands::registrar::CommandWorld;
    use crate::commands::{CommandSource, PlayerCandidate, ServerCommands};
    use crate::game_rules::GameRulesHandle;
    use crate::world_state::WorldStateHandle;

    fn candidate(uuid: Uuid, name: &str) -> PlayerCandidate {
        PlayerCandidate {
            uuid,
            entity_id: 1,
            username: name.to_string(),
            position: lodestone_model::Vec3::new(0.0, 0.0, 0.0),
            rotation: lodestone_model::Rotation { yaw: 0.0, pitch: 0.0 },
            game_mode: lodestone_model::GameMode::Survival,
            xp_level: 0,
            xp_points: 0,
        }
    }

    fn console() -> CommandSource {
        CommandSource::console("Server", crate::commands::overworld_dimension(), 4)
    }

    /// The production path, end to end: `ServerCommands::run` (not the
    /// executor directly, which cannot see a tree that was never wired —
    /// this crate's own module doc names exactly that failure mode) grants
    /// operator status through `/op`, and the *same* `AccessHandle` reports
    /// it — proving this reaches `AccessLists::op`, not a copy.
    #[test]
    fn op_deop_and_whitelist_reach_the_real_access_list_through_run() {
        let access = AccessHandle::new(AccessLists::new());
        let target = Uuid::from_u128(42);
        let players = [candidate(target, "Notch")];
        let rules = GameRulesHandle::default();
        let state = WorldStateHandle::default();
        let world = CommandWorld {
            rules: &rules,
            players: &players,
            state: &state,
            mobs: None,
            border: None,
            access: Some(&access),
        };
        let commands = ServerCommands::new();

        assert_eq!(access.permission_level(target), 0, "control: not yet an op");
        let outcome = commands.run(&world, &console(), "op Notch").expect("/op is a built-in");
        assert!(outcome.response.lines().iter().any(|l| l.contains("Made Notch a server operator")));
        assert_eq!(access.permission_level(target), 4, "op reached the real AccessHandle");

        // A second `/op` on an already-opped player is vanilla's own
        // "nothing changed" refusal, not a silent no-op success.
        let outcome = commands.run(&world, &console(), "op Notch").expect("/op is a built-in");
        assert!(!outcome.response.is_ran(), "re-opping an op must refuse");

        let outcome = commands.run(&world, &console(), "deop Notch").expect("/deop is a built-in");
        assert!(outcome.response.lines().iter().any(|l| l.contains("no longer a server operator")));
        assert_eq!(access.permission_level(target), 0, "deop reached the real AccessHandle");

        // Neuter control: deopping an already-non-op must refuse, not
        // silently report success — proving the "nothing changed" branch
        // above is reachable rather than always skipped.
        let outcome = commands.run(&world, &console(), "deop Notch").expect("/deop is a built-in");
        assert!(!outcome.response.is_ran(), "re-deopping a non-op must refuse");

        assert!(!access.with(|lists| lists.whitelist_enabled()), "control: whitelist starts off");
        commands.run(&world, &console(), "whitelist on").expect("/whitelist on is a built-in");
        assert!(access.with(|lists| lists.whitelist_enabled()));

        commands.run(&world, &console(), "whitelist add Notch").expect("/whitelist add is a built-in");
        assert!(access.with(|lists| lists.whitelisted().contains(&target)));

        let outcome = commands.run(&world, &console(), "whitelist remove Notch").expect("built-in");
        assert!(outcome.response.lines().iter().any(|l| l.contains("Removed Notch from the whitelist")));
        assert!(!access.with(|lists| lists.whitelisted().contains(&target)), "remove reached the real list");
    }

    /// The scoping this module's own doc discloses: with no `access` handle
    /// (chat's own `dispatch_play_packet` call site), every command in this
    /// family refuses by name rather than silently doing nothing.
    #[test]
    fn no_access_handle_refuses_by_name_rather_than_silently_doing_nothing() {
        let rules = GameRulesHandle::default();
        let state = WorldStateHandle::default();
        let world = CommandWorld {
            rules: &rules,
            players: &[],
            state: &state,
            mobs: None,
            border: None,
            access: None,
        };
        let commands = ServerCommands::new();
        let outcome = commands.run(&world, &console(), "whitelist on").expect("/whitelist is a built-in");
        assert!(!outcome.response.is_ran());
        assert!(outcome.response.lines().iter().any(|l| l.contains("No access list is configured")));
    }
}
