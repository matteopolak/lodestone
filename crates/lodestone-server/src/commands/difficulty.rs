//! `/difficulty`, from `DifficultyCommand.java`.
//!
//! # One literal per difficulty, not a dedicated argument type
//!
//! Vanilla's own tree uses a `DifficultyArgument`, but this server's
//! [`lodestone_model::command_tree::ArgumentParser`] has no wire entry for it —
//! the closed four-value set is exactly [`crate::commands::gamerule`]'s own
//! reasoning for one literal per value, applied here instead of inventing a
//! parser this server cannot transmit correctly.
//!
//! # The locked-difficulty refusal
//!
//! [`crate::world_state::WorldStateHandle::set_difficulty`] returns `false` for
//! a locked world (`MinecraftServer.setDifficulty`'s own guard) — reported as a
//! refusal here rather than silently doing nothing, which is the failure this
//! server's own `docs/world-state.md` names as the one that looks like it works.

use lodestone_model::Difficulty;

use super::registrar::Registrar;
use super::CommandResult;

/// `Commands.LEVEL_GAMEMASTERS`.
const DIFFICULTY_LEVEL: u8 = 2;

const DIFFICULTIES: [(&str, Difficulty); 4] = [
    ("peaceful", Difficulty::Peaceful),
    ("easy", Difficulty::Easy),
    ("normal", Difficulty::Normal),
    ("hard", Difficulty::Hard),
];

fn difficulty_name(difficulty: Difficulty) -> &'static str {
    DIFFICULTIES
        .iter()
        .find(|(_, d)| *d == difficulty)
        .map_or("unknown", |(name, _)| name)
}

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let difficulty = registrar.literal(root, "difficulty");
    registrar.require_level(difficulty, DIFFICULTY_LEVEL);

    // Bare `/difficulty` — query, `commands.difficulty.query`.
    registrar.exec(difficulty, |ctx| {
        let (current, _) = ctx.world.state.difficulty();
        ctx.send_success(format!("The difficulty is {}", difficulty_name(current)));
        Ok(1)
    });

    for (name, value) in DIFFICULTIES {
        let literal = registrar.literal(difficulty, name);
        registrar.exec(literal, move |ctx| set(ctx, value));
    }
}

fn set(ctx: &mut super::registrar::Ctx<'_>, difficulty: Difficulty) -> CommandResult {
    if ctx.world.state.set_difficulty(difficulty) {
        ctx.send_success(format!("The difficulty has been set to {}", difficulty_name(difficulty)));
        Ok(1)
    } else {
        Err("The difficulty has been locked and cannot be changed".to_string())
    }
}
