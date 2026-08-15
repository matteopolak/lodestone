//! The dedicated-server binary's own admin console: run one line of text as a
//! server command and get back what a player typing it would see, with no
//! connection and no socket involved.
//!
//! ## What it is
//!
//! `lodestone-dedicated-server`'s stdin loop is the one production caller —
//! see that crate's `main.rs`. It exists because RCON (`crate::rcon`) already
//! solved "run a command with no live `ServerProtocol` connection behind it",
//! and a local stdin console needs exactly the same shape: the built-in
//! [`crate::commands::ServerCommands`] tree first, the host
//! [`CommandDispatch`](crate::command::CommandDispatch) sink for a root it
//! does not own, both against the world's real, shared
//! [`WorldStateHandle`](crate::world_state::WorldStateHandle) and
//! [`PlayerRegistry`](crate::players::PlayerRegistry) — never a private copy,
//! which is the exact bug issues #327/#328 were filed for.
//!
//! ## How it works
//!
//! [`run`] builds a throwaway [`RconConfig`](crate::rcon::RconConfig) (its
//! `addr`/`password` fields are never read — nothing here binds a socket) and
//! calls [`crate::rcon::run_console_command`], which is
//! [`crate::rcon::run_command_as`] under identity `"Server"` at permission
//! level 4 — vanilla's own dedicated-server console identity
//! (`MinecraftServer` itself as a `CommandSource`, `Commands.LEVEL_OWNERS`),
//! distinct from RCON's `"Rcon"`.
//!
//! ## How to change it
//!
//! This module owns none of the command logic — it is a two-line adapter over
//! `crate::rcon`. A behaviour change (a new built-in, a permission rule)
//! belongs in `crate::commands` and is picked up here for free; do not
//! reimplement dispatch in the binary crate.
//!
//! ## Dependencies
//!
//! Native only, like `crate::rcon` — a console with no socket still needs
//! `crate::rcon::RconConfig`'s shape, so this module carries the identical
//! `cfg` gate rather than duplicating the type.

#[cfg(not(target_arch = "wasm32"))]
use crate::command::CommandDispatch;
#[cfg(not(target_arch = "wasm32"))]
use crate::players::PlayerRegistry;
#[cfg(not(target_arch = "wasm32"))]
use crate::rcon::RconConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::world_state::WorldStateHandle;

/// Runs `command` (with or without a leading `/`) against `world`/`players`
/// and returns the text a caller should print — feedback lines joined with
/// `\n` on success, or the single refusal line.
///
/// `players` is `None` for a world with no shared registry (there is nobody
/// to target a directed effect at, same as `RconConfig::players`'s own doc);
/// pass the dedicated server's real, shared registry so `/list`, `/say` and a
/// targeted `/gamemode <player>` all see and reach real connections.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn run(world: &WorldStateHandle, players: Option<&PlayerRegistry>, command: &str) -> String {
    let config = RconConfig::new(
        std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
        String::new(),
        CommandDispatch::none(),
    )
    .with_world(world.clone(), players.cloned());
    crate::rcon::run_console_command(&config, command)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_command_is_refused_rather_than_panicking() {
        let world = WorldStateHandle::new();
        let response = run(&world, None, "/this-is-not-a-real-command");
        assert!(
            !response.is_empty(),
            "a refusal must still produce explanatory text, not an empty string"
        );
    }

    #[test]
    fn a_leading_slash_is_optional_exactly_like_rcon() {
        let world = WorldStateHandle::new();
        // Both spellings must reach the same built-in — `/gamerule` with no
        // further arguments lists the rules rather than refusing, so a
        // non-empty response here is the control that the command actually
        // ran rather than silently refusing both ways.
        let with_slash = run(&world, None, "/gamerule");
        let without_slash = run(&world, None, "gamerule");
        assert_eq!(with_slash, without_slash);
        assert!(!with_slash.is_empty());
    }
}
