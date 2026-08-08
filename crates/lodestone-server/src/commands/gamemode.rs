//! `/gamemode`, from `GameModeCommand.java` — not from memory.
//!
//! # The tree, as vanilla declares it
//!
//! ```text
//! literal("gamemode").requires(COMMANDS_GAMEMASTER)
//!   └─ argument("gamemode", GameModeArgument.gameMode())   [executable: self]
//!        └─ argument("target", EntityArgument.players())    [executable]
//! ```
//!
//! Confirmed against the captured 26.2 tree
//! (`crates/protocol/v770/tests/fixtures/command_tree_creative.hex`, nodes 23 /
//! 148 / 484): the root literal is non-executable and `FLAG_RESTRICTED`; the mode
//! node is `minecraft:gamemode` (registry id 42, no payload) and executable; the
//! target node is `minecraft:entity` with `single: false, players_only: true` and
//! executable. **One parser node, not four literals** — the four-literal shape is
//! from a much older version and is the obvious wrong guess.
//!
//! # Two executables on one path, and no `Option<T>`
//!
//! The optional trailing `<target>` is *two executable nodes*, not one node with
//! an optional parameter. Both attach to one body, and the shallow one supplies
//! the default (`Collections.singleton(getPlayerOrException())`) explicitly. The
//! wire tree must show both nodes executable, which is exactly what the fixture
//! shows and what the parity gate asserts — an `Option`-shaped design would
//! transmit one.

use lodestone_command_mc::{EntityArg, GameModeArg};
use lodestone_model::GameMode;

use super::effect::Effect;
use super::registrar::{Ctx, Registrar};
use super::source::PlayerCandidate;
use super::CommandResult;

/// `GameModeCommand.PERMISSION_CHECK` is `Permissions.COMMANDS_GAMEMASTER`,
/// which is level 2 in 26.2's numeric model (`Commands.LEVEL_GAMEMASTERS`).
const GAMEMODE_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let gamemode = registrar.literal(root, "gamemode");
    registrar.require_level(gamemode, GAMEMODE_LEVEL);

    let (mode_node, mode_key) = registrar.arg(gamemode, "gamemode", GameModeArg);
    // `/gamemode <mode>` — self. The default target is written out here rather
    // than being an absent parameter, so the two paths share a body without
    // either of them having to ask "was a target given?".
    registrar.exec(mode_node, move |ctx| {
        let mode = *ctx.get(mode_key);
        let me = ctx.resolve(&self_selector())?;
        set_mode(ctx, mode, &me)
    });

    let (target_node, target_key) = registrar.arg(mode_node, "target", EntityArg::players());
    registrar.exec(target_node, move |ctx| {
        let mode = *ctx.get(mode_key);
        let selector = ctx.get(target_key).clone();
        let targets = ctx.resolve(&selector)?;
        set_mode(ctx, mode, &targets)
    });
}

/// The `@s`-equivalent selector the no-target form resolves.
///
/// Built rather than parsed: `/gamemode creative` has no `@s` text to parse, and
/// re-parsing `"@s"` here would make the default depend on the grammar staying
/// able to express it.
fn self_selector() -> lodestone_command_mc::EntitySelector {
    lodestone_command_mc::EntitySelector {
        max_results: 1,
        includes_entities: true,
        current_entity: true,
        ..lodestone_command_mc::EntitySelector::default()
    }
}

/// `GameModeCommand.setMode` + `logGamemodeChange`.
///
/// The feedback split is vanilla's: changing your *own* mode says
/// `commands.gamemode.success.self`, changing someone else's says
/// `…success.other` to the caller **and** `gameMode.changed` to the target — the
/// second of which is the reason a directed effect queue exists at all.
fn set_mode(ctx: &mut Ctx<'_>, mode: GameMode, targets: &[PlayerCandidate]) -> CommandResult {
    let caller = ctx.source.uuid();
    let mut count = 0;
    for target in targets {
        ctx.effect(target.uuid, Effect::SetGameMode(mode));
        if Some(target.uuid) == caller {
            ctx.send_success(format!("Set own game mode to {}", mode_name(mode)));
        } else {
            // `sendCommandFeedback` gates only the *target*'s notification, not
            // the caller's confirmation (`logGamemodeChange`'s own `if`).
            //
            // Read by name rather than through a typed accessor because
            // `crate::game_rules` has none for this rule; `unwrap_or(true)`
            // matches the rule's vanilla default (`GameRules.java`).
            let feedback_on = ctx
                .world
                .rules
                .get_rule("send_command_feedback")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            if feedback_on {
                ctx.effect(
                    target.uuid,
                    Effect::Message(format!("Your game mode has been updated to {}", mode_name(mode))),
                );
            }
            ctx.send_success(format!(
                "Set {}'s game mode to {}",
                target.username,
                mode_name(mode)
            ));
        }
        count += 1;
    }
    Ok(count)
}

/// The English rendering of `gameMode.<name>`
/// (`assets/minecraft/lang/en_us.json`). This crate sends system chat as plain
/// text rather than a translatable component, so the language file's value has to
/// be applied here — the same trade [`crate::ChatLine::rendered`] documents.
#[must_use]
pub fn mode_name(mode: GameMode) -> &'static str {
    match mode {
        GameMode::Survival => "Survival Mode",
        GameMode::Creative => "Creative Mode",
        GameMode::Adventure => "Adventure Mode",
        GameMode::Spectator => "Spectator Mode",
    }
}
