//! `/help`, from `HelpCommand.java` — the root listing only.
//!
//! # What this leaves out
//!
//! Vanilla's `/help <command>` prints that command's own usage strings, built
//! from every executable path through its subtree
//! (`Commands.getStructure(...)`'s recursive walk). No such per-command usage
//! text exists here — a [`super::registrar::Registrar`]-built tree tracks
//! parsers and executors, not English descriptions — so this registers only
//! the bare `/help`, which lists the root command names
//! [`super::registrar::Ctx::root_command_names`] already derives from the tree
//! itself.

use super::registrar::Registrar;

/// Vanilla's `/help` is available to everyone.
const HELP_LEVEL: u8 = 0;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let help = registrar.literal(root, "help");
    registrar.require_level(help, HELP_LEVEL);
    registrar.exec(help, |ctx| {
        let names = ctx.root_command_names();
        ctx.send_success(format!("Available commands: {}", names.join(", ")));
        Ok(i32::try_from(names.len()).unwrap_or(i32::MAX))
    });
}
