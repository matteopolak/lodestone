//! `/say`, `/me` and `/msg` (`/tell`/`/w`).
//!
//! # `/say` and `/me` are self-targeted [`Effect::Broadcast`](crate::commands::Effect::Broadcast)
//!
//! Same delivery shape as [`crate::commands::block_commands`]'s `SetBlock`: the
//! *content* has nothing to do with the caller specifically, but reaching every
//! connection needs the player registry, which only the issuing connection's own
//! `ChatCommand` arm has in scope. See that arm's handling of `Broadcast` in
//! `crate::server`, and `crate::rcon`'s module doc for why RCON's `/say`/`/me`
//! goes through a *separate* path that calls the registry directly instead.
//!
//! # `/msg` needs no new [`Effect`](crate::commands::Effect) variant at all
//!
//! A private message is exactly one existing thing: [`Effect::Message`] aimed at
//! one resolved target, which is already how `/gamemode <target>`'s
//! `gameMode.changed` notification and `/effect give`'s recipient text are
//! delivered.

use lodestone_command::StringArgument;
use lodestone_command_mc::EntityArg;

use super::registrar::Registrar;
use super::CommandResult;
use crate::commands::Effect;

/// Vanilla gates `/say` at `Commands.LEVEL_GAMEMASTERS` (2). `/me` and `/msg`
/// are ungated (level 0) — anyone connected may emote or whisper.
const SAY_LEVEL: u8 = 2;
const ME_LEVEL: u8 = 0;
const MSG_LEVEL: u8 = 0;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();

    let say = registrar.literal(root, "say");
    registrar.require_level(say, SAY_LEVEL);
    let (say_message, say_message_key) = registrar.arg(say, "message", StringArgument::greedy());
    registrar.exec(say_message, move |ctx| {
        let message = ctx.get(say_message_key).clone();
        ctx.effect(
            ctx.source.uuid().unwrap_or_default(),
            Effect::Broadcast { sender: "Server".to_string(), message },
        );
        Ok(1)
    });

    let me = registrar.literal(root, "me");
    registrar.require_level(me, ME_LEVEL);
    let (me_action, me_action_key) = registrar.arg(me, "action", StringArgument::greedy());
    registrar.exec(me_action, move |ctx| {
        let action = ctx.get(me_action_key).clone();
        let name = ctx.source.name.clone();
        ctx.effect(
            ctx.source.uuid().unwrap_or_default(),
            Effect::Broadcast { sender: name, message: format!("* {action}") },
        );
        Ok(1)
    });

    for name in ["msg", "tell", "w"] {
        let msg = registrar.literal(root, name);
        registrar.require_level(msg, MSG_LEVEL);
        let (targets_node, targets_key) = registrar.arg(msg, "targets", EntityArg::players());
        let (message_node, message_key) =
            registrar.arg(targets_node, "message", StringArgument::greedy());
        registrar.exec(message_node, move |ctx| whisper(ctx, targets_key, message_key));
    }
}

fn whisper(
    ctx: &mut super::registrar::Ctx<'_>,
    targets_key: super::registrar::ArgKey<lodestone_command_mc::EntitySelector>,
    message_key: super::registrar::ArgKey<String>,
) -> CommandResult {
    let selector = ctx.get(targets_key).clone();
    let message = ctx.get(message_key).clone();
    let sender = ctx.source.name.clone();
    let targets = ctx.resolve(&selector)?;
    for target in &targets {
        ctx.effect(target.uuid, Effect::Message(format!("{sender} whispers to you: {message}")));
    }
    if let [only] = targets.as_slice() {
        ctx.send_success(format!("You whisper to {}: {message}", only.username));
    } else {
        ctx.send_success(format!("You whisper to {} players: {message}", targets.len()));
    }
    Ok(i32::try_from(targets.len()).unwrap_or(i32::MAX))
}
