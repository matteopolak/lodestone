//! Where singleplayer worlds live on disk.
//!
//! # What it is
//!
//! The shell's answer to "which directory is this world?", and the reason
//! singleplayer now survives quitting at all. Persistence itself landed in
//! issue [#437](https://github.com/matteopolak/lodestone/issues/437) and reached
//! **zero players**, because `net.rs` opened every session through
//! `IntegratedServer::open_in_memory_with_mobs` — the non-persistent
//! constructor — and the persistent one needs a `world_dir` the shell had no
//! concept of. Issue
//! [#468](https://github.com/matteopolak/lodestone/issues/468).
//!
//! # The product decision, stated rather than smuggled in
//!
//! Issue #468 names two materially different readings, and **this module
//! implements the first**:
//!
//! 1. **One implicit default world** — every singleplayer session opens
//!    [`default_world_dir`], and it persists. *This is what ships.*
//! 2. **A save list** — create/select/delete, each world its own directory with
//!    its own name and seed, which is what vanilla does.
//!
//! (1) was chosen because it is strictly better than an ephemeral world, does
//! not preclude (2), and is not blocked on the UI work (2) needs. It is a
//! visible product decision and not a wiring detail, so it is written down
//! here, in the commit, and in `docs/world-save-load.md` rather than being left
//! for a player to infer.
//!
//! **(2) is not nearly free, despite the seed screen existing.** The
//! `CreateWorld` screen ([`crate::menu::create_world`]) does collect a name and
//! a seed, and is reachable — so the *input* half is done. The *listing* half
//! is not: [`crate::menu::world_select`] renders exactly one hardcoded
//! `BUNDLED_WORLD` row, its Edit and Delete buttons are deliberately disabled
//! against vanilla's `LevelSummary.canEdit`/`canDelete`, and its pixel gates pin
//! a single row's label and geometry. Turning that into a real list means a
//! `LevelSummary` equivalent, directory enumeration, delete confirmation, and
//! new gates for all of it. That is a feature, not this issue.
//!
//! ## The wart this reading has, honestly
//!
//! With one implicit world, **"Create New World" cannot create a second one.**
//! Pressing it with a typed seed opens the existing world instead, because the
//! stored seed of an existing world always wins over a requested one (see
//! `lodestone_server::region_source::resolve_world_seed` for why that rule is
//! the only safe one). The typed seed therefore takes effect only on the very
//! first launch, when no world exists yet.
//!
//! That is a real gap a player can notice, and the alternative is worse: making
//! Create overwrite the directory would silently destroy the world they had
//! been building. **Nothing here deletes a world**, and (2) is what fixes it
//! properly.
//!
//! # How to change it
//!
//! To land (2), keep [`saves_dir`] as the root and give each world its own
//! subdirectory beneath it — the layout below is already the vanilla one, so
//! nothing here has to move. What changes is that
//! [`crate::app`]'s `begin_singleplayer` stops calling [`default_world_dir`]
//! and instead passes a directory the world-select screen chose.
//!
//! # Configuration
//!
//! Rooted at [`crate::menu::servers::data_dir`], so the `LODESTONE_DATA_DIR`
//! environment variable relocates saves along with `options.json`,
//! `servers.json` and `hidden_players.json`. **Tests must set it** rather than
//! writing into the real user data directory — see
//! `tests/singleplayer_persistence.rs`.
//!
//! On macOS that yields
//! `~/Library/Application Support/lodestone/saves/world`.
//!
//! # Dependencies
//!
//! [`crate::menu::servers::data_dir`] for the platform directory, and nothing
//! else — deliberately no version family, so
//! `cargo check -p lodestone-shell --no-default-features` still holds.

use std::path::PathBuf;

/// The directory name every world folder sits under, matching vanilla's own
/// `saves/` (`LevelStorageSource.createDefault` roots at `<game dir>/saves`).
pub const SAVES_DIR: &str = "saves";

/// The folder name of the one implicit world reading (1) gives us.
///
/// `"world"` rather than `"New World"` deliberately: it is the name a
/// *dedicated* server uses (`level-name=world` in `server.properties`), it needs
/// no filename sanitising, and it matches every real world folder this repo
/// already reads under `.cache/mc/*/world`.
pub const DEFAULT_WORLD_NAME: &str = "world";

/// The root every world folder lives under.
#[must_use]
pub fn saves_dir() -> PathBuf {
    crate::menu::servers::data_dir().join(SAVES_DIR)
}

/// The one world singleplayer opens, per this module's reading (1).
///
/// Nothing is created here — [`lodestone_server::region_source::RegionChunkSource::new`]
/// creates the region directory and
/// [`lodestone_server::region_source::resolve_world_seed`] creates the settings
/// file, both at world-open time, so a shell that never plays singleplayer
/// writes nothing.
#[must_use]
pub fn default_world_dir() -> PathBuf {
    saves_dir().join(DEFAULT_WORLD_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_world_sits_under_the_data_dir() {
        let world = default_world_dir();
        assert!(
            world.starts_with(crate::menu::servers::data_dir()),
            "worlds must share the `LODESTONE_DATA_DIR` root that options.json \
             and servers.json use, or a relocated data dir would strand them: {world:?}"
        );
        assert!(world.ends_with("saves/world"), "{world:?}");
    }

    #[test]
    fn the_default_world_is_inside_the_saves_root() {
        // Reading (2) grows siblings of this path; if it ever stops being a
        // child of `saves_dir()`, enumeration for a world list would miss it.
        assert!(default_world_dir().starts_with(saves_dir()));
    }
}
