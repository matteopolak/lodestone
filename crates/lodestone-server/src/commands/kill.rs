//! `/kill`.
//!
//! # Targets are any entity
//!
//! `<targets>` resolves through [`Ctx::resolve_targets`], so `@e` reaches every
//! entity the mob simulation owns. A player is killed by a directed
//! [`crate::commands::Effect`]; anything else by `MobSim::kill_entity`.
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

/// Kills a non-player entity. A killed item frame drops nothing and its framed
/// map forgets the marker it showed; the removal reaches clients through the
/// next entity-stream diff.
fn kill_world_entity(ctx: &Ctx<'_>, uuid: Uuid) {
    let Some(mobs) = ctx.world.mobs else { return };
    if let Some(broken) = mobs.with(|sim| sim.kill_entity(uuid)).and_then(|kill| kill.frame) {
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
            ctx.send_success(format!("Killed {}", only.display_name()));
        } else {
            ctx.send_success(format!("Killed {} entities", targets.len()));
        }
        Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
    });
}
