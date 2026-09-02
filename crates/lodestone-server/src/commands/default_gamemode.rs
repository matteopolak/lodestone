//! `/defaultgamemode` — the mechanism this command needed was simply a
//! store: nothing here tracked "the game mode a *new* player joins in" at
//! all, only the per-connection `game_mode` local a joined player already
//! has. [`crate::world_state::WorldStateHandle::default_game_mode`] is that
//! store, read by `crate::server::serve_connection_inner`'s join arm as the
//! fallback a brand-new player's saved data has no game mode to override.
//!
//! # What this does not do
//!
//! The real rule also force-resets every **already connected** player's mode
//! when the `forceGameMode` game rule is on. This crate models no such rule
//! (`crate::game_rules::GAME_RULES`) and has no cross-connection game-mode
//! push wired to this command, so `/defaultgamemode` only ever changes future
//! joins — a real, disclosed gap rather than a silent half-port.

use lodestone_command_mc::GameModeArg;

use super::registrar::Registrar;

/// The game-masters permission level.
const DEFAULT_GAMEMODE_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let cmd = registrar.literal(root, "defaultgamemode");
    registrar.require_level(cmd, DEFAULT_GAMEMODE_LEVEL);

    let (mode_node, mode_key) = registrar.arg(cmd, "gamemode", GameModeArg);
    registrar.exec(mode_node, move |ctx| {
        let mode = *ctx.get(mode_key);
        ctx.world.state.set_default_game_mode(mode);
        // Reuses `/gamemode`'s own English rendering rather than a second
        // table that could drift from it.
        ctx.send_success(format!(
            "The default game mode is now {}",
            super::gamemode::mode_name(mode)
        ));
        Ok(1)
    });
}
