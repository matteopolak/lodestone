//! The ender dragon fight (issue-tracked as "the ender dragon fight", child
//! of the boss-fights parent) — the phase state machine, end-crystal
//! healing, and the `EndDragonFight` controller (persisted defeated flag,
//! scan-on-load, boss-bar value, exit-portal geometry, four-crystal
//! respawn).
//!
//! # What it is
//!
//! Three pure, world-free modules ported from vanilla 26.2's decompiled
//! source under
//! `.cache/mc/26.2/src/net/minecraft/world/entity/boss/enderdragon/` and
//! `.../world/level/dimension/end/`:
//!
//! * [`phase`] — [`phase::PhaseManager`], the eleven-phase state machine
//!   (`EnderDragonPhase`/`EnderDragonPhaseManager`).
//! * [`crystal`] — the exact crystal-healing amount and interval
//!   (`EnderDragon.checkCrystals`).
//! * [`fight`] — [`fight::FightState`] and the free functions around it
//!   (`EnderDragonFight`, `DragonRespawnStage`, `EndPodiumFeature`'s exit-
//!   portal block geometry).
//!
//! # How it works
//!
//! Every function here is pure: given inputs, it returns a new state plus
//! (for `fight`) a list of world-side effects the **caller** performs. None
//! of the three modules touches a world, spawns an entity, or sends a
//! packet — see each module's own doc for exactly what it does not attempt,
//! and `docs/dragon-fight.md` for the full writeup including what a real
//! integration (spawning a live dragon/crystal pair, driving these functions
//! from [`crate::mobs::MobSim::tick`], and streaming the result) needs from
//! `crates/lodestone-server/src/protocol.rs` that this change could not add
//! itself (that file is held for a concurrent `MetadataField` edit — see
//! that doc for the exact variants and encode methods needed).
//!
//! # How to change it
//!
//! Each module cites the vanilla symbol it ports by name (never a line
//! number — the decompile gets re-extracted and lines move). When vanilla's
//! logic and this module's diverge, the divergence is named in a doc comment
//! at the point it happens, not left implicit — the biggest one is the
//! pathfinding substitution in `phase`'s own module doc, and the "no
//! obsidian pillars anywhere in this repo" scope note in `fight`'s.

pub mod crystal;
pub mod fight;
pub mod phase;
