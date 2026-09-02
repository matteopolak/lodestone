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

use super::registrar::Registrar;

/// The game-masters permission level.
const KILL_LEVEL: u8 = 2;

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

    let (targets_node, targets_key) = registrar.arg(kill, "targets", EntityArg::players());
    registrar.exec(targets_node, move |ctx| {
        let selector = ctx.get(targets_key).clone();
        let targets = ctx.resolve(&selector)?;
        for target in &targets {
            ctx.effect(target.uuid, crate::commands::Effect::Kill);
        }
        if let [only] = targets.as_slice() {
            ctx.send_success(format!("Killed {}", only.username));
        } else {
            ctx.send_success(format!("Killed {} entities", targets.len()));
        }
        Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
    });
}
