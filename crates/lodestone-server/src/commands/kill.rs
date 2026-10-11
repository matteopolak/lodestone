//! `/kill`.
//!
//! # Targets are `players()`, not the real "any entity" selector
//!
//! The real `<targets>` argument matches any entity, not just players. This
//! server's [`crate::commands::CommandWorld`] only ever carries
//! [`crate::commands::PlayerCandidate`]s (see [`crate::commands::source`]'s
//! module doc for why: entity resolution needs a world this crate deliberately
//! does not depend on), so `players()` is the same narrowing `/gamemode` and
//! `/give` already made for the same reason.
//!
//! # Delivery: [`crate::commands::Effect::Kill`] through the ordinary directed
//! path
//!
//! Unlike `SetBlock`/`Broadcast`, a kill genuinely can target *any* connected
//! player, not just the caller — so this is an ordinary per-uuid
//! [`crate::commands::Effect`], applied wherever a `GiveItems`/`SetGameMode`
//! effect already is (the issuing connection inline for itself, the target's own
//! connection via the registry for anyone else).

use lodestone_command_mc::EntityArg;
use uuid::Uuid;

use super::registrar::{Ctx, Registrar};
use super::source::{CommandTarget, SelectorError};

/// The game-masters permission level.
const KILL_LEVEL: u8 = 2;

/// Removes a non-player entity at once. A killed item frame drops nothing and
/// its framed map forgets the marker it showed; the removal reaches clients
/// through the next entity-stream diff.
fn kill_world_entity(ctx: &Ctx<'_>, uuid: Uuid) {
    let Some(mobs) = ctx.world.mobs else { return };
    if let Some(broken) = mobs.with(|sim| sim.kill_frame_by_uuid(uuid)) {
        crate::item_frame::forget_map(ctx.world.state.maps(), broken.map);
    }
}

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let kill = registrar.literal(root, "kill");
    registrar.require_level(kill, KILL_LEVEL);

    // Bare `/kill` — self. The real console-refusal for a non-player source.
    registrar.exec(kill, |ctx| {
        let Some(uuid) = ctx.source.uuid() else {
            return Err("That command can only be used by a player".to_string());
        };
        ctx.effect(uuid, crate::commands::Effect::Kill);
        ctx.send_success(format!("Killed {}", ctx.source.name));
        Ok(1)
    });

    let (targets_node, targets_key) = registrar.arg(kill, "targets", EntityArg::entities());
    registrar.exec(targets_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let targets = ctx.resolve_targets(&selector).map_err(|error| match error {
            SelectorError::NoPlayersFound if selector.includes_entities => "No entity was found".to_string(),
            other => other.to_string(),
        })?;
        for target in &targets {
            match target {
                CommandTarget::Player(player) => ctx.effect(player.uuid, crate::commands::Effect::Kill),
                CommandTarget::World(entity) => kill_world_entity(ctx, entity.uuid),
            }
        }
        if let [only] = targets.as_slice() {
            ctx.send_success(format!(
                "Killed {}",
                match only {
                    CommandTarget::Player(player) => player.username.as_str(),
                    CommandTarget::World(entity) => entity.entity_type.as_str(),
                }
            ));
        } else {
            ctx.send_success(format!("Killed {} entities", targets.len()));
        }
        Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
    });
}
