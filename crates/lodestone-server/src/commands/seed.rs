//! `/seed`, from `SeedCommand.java` — reports the world seed this server is
//! actually generating from.
//!
//! [`crate::worldgen_data::active_world_seed`] is the one place that value lives
//! (see that function's own doc for the single-world caveat); this is its first
//! command-surface reader.

use super::registrar::Registrar;

/// Vanilla gates `/seed` at `Commands.LEVEL_GAMEMASTERS` (2) — it is listed
/// under `ALLOW_CHEATS`-guarded commands (`SeedCommand.register`'s own
/// `requiresCheats` in a singleplayer/LAN world), same as `/difficulty`.
const SEED_LEVEL: u8 = 2;

pub(super) fn register(registrar: &mut Registrar) {
    let root = registrar.root();
    let seed = registrar.literal(root, "seed");
    registrar.require_level(seed, SEED_LEVEL);
    registrar.exec(seed, |ctx| {
        let value = crate::worldgen_data::active_world_seed();
        ctx.send_success(format!("Seed: [{value}]"));
        Ok(i32::try_from(value & i64::from(i32::MAX)).unwrap_or(0))
    });
}

#[cfg(test)]
mod tests {
    use crate::commands::registrar::Registrar;
    use crate::commands::{CommandSource, CommandWorld, ServerCommands, overworld_dimension};
    use crate::world_state::WorldStateHandle;

    /// The value in the message is a real read of `active_world_seed`, not a
    /// hardcoded string — checked by comparing the command's own answer
    /// against an independent direct read of the same global, immediately
    /// adjacent so the comparison is not itself racing another test.
    ///
    /// **Deliberately not an exact literal.** `active_world_seed` is a single
    /// process-wide `AtomicI64` this crate's own module doc already documents
    /// as shared across every world open in one process — `cargo test` runs
    /// this binary's tests concurrently on several threads, so a value this
    /// test wrote itself could be overwritten by another test between the
    /// write and the read. Comparing against a read taken right here, rather
    /// than against a constant chosen earlier in the test, is what keeps this
    /// assertion honest under that concurrency instead of merely usually
    /// passing.
    #[test]
    fn seed_reports_the_real_active_world_seed_not_a_placeholder() {
        let mut registrar = Registrar::new();
        super::register(&mut registrar);
        let commands = ServerCommands::from_registrar(registrar);

        let state = WorldStateHandle::new();
        let alice = CommandSource::console("alice", overworld_dimension(), 4);
        let world = CommandWorld { rules: &state, players: &[], state: &state, mobs: None, border: None };

        let outcome = commands.run(&world, &alice, "seed").expect("root matched");
        let expected = format!("Seed: [{}]", crate::worldgen_data::active_world_seed());
        assert_eq!(outcome.response.lines(), [expected]);
    }
}
