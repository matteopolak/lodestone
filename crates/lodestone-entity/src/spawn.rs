//! Spawning, despawning and mob-category rules.
//!
//! In multiplayer this is entirely server-authoritative, but a singleplayer
//! client runs an *integrated* server and needs the same rules, so this lives in
//! the version-free entity layer where both an integrated server (`impl-worldgen`)
//! and tests can drive it. Everything here is a **pure decision** over inputs the
//! caller supplies — no world handle, no RNG owned internally — so it is trivially
//! hermetic and the version/integrated-server layer keeps ownership of *when* to
//! call it and *where* the numbers come from.
//!
//! The two version-free pieces are:
//!
//! * [`MobCategory`] — the eight vanilla categories and their constants (per-chunk
//!   cap, friendliness, natural persistence, and the despawn distances). These are
//!   stable game rules, not protocol data, so they belong here rather than in a
//!   version crate.
//! * [`check_despawn`] — vanilla `Mob.checkDespawn` as a pure function returning a
//!   [`DespawnDecision`], plus [`mob_cap`] for the `NaturalSpawner` per-category
//!   cap formula.
//!
//! Per-mob *spawn placement* rules (light level, sky visibility, valid block
//! below, biome) are per-entity **data** that move between versions and need
//! more than a light/Y-band/solid-below triple to express (a per-species sky
//! requirement, a slime-chunk special case, …), so they live entirely in the
//! integrated server (`lodestone_server::natural_spawn::SpawnRule` and its
//! `SPAWN_RULES` table) rather than behind a seam in this crate. An earlier
//! `SpawnConditions`/`SpawnSample`/`SpawnEnvironment` seam attempted the
//! version-free version of this and was removed: it had no implementer and
//! the real placement rules were built independently in `lodestone-server`
//! because its shape could not express them.

/// Vanilla's eight spawn categories, with their game-rule constants baked in
/// from the 26.2 `MobCategory` enum.
///
/// The constants (per-chunk cap, friendliness, persistence, despawn distance)
/// are long-stable and identical across the versions this project targets, so
/// they are version-free. A version crate still owns the *mapping* from an
/// entity type id to its category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MobCategory {
    /// Hostile mobs (zombies, skeletons, …). Cap 70, despawn 128.
    Monster,
    /// Passive land animals (pigs, cows, …). Cap 10, naturally persistent.
    Creature,
    /// Ambient mobs (bats). Cap 15.
    Ambient,
    /// Axolotls. Cap 5.
    Axolotls,
    /// Underground water creatures (glow squid). Cap 5.
    UndergroundWaterCreature,
    /// Water creatures (squid, dolphins). Cap 5.
    WaterCreature,
    /// Water ambient (fish). Cap 20, despawn 64.
    WaterAmbient,
    /// Miscellaneous (item frames, boats, …). No cap; naturally persistent.
    Misc,
}

impl MobCategory {
    /// The categories that participate in natural spawning, in vanilla order —
    /// every category except [`Misc`](MobCategory::Misc), which never spawns
    /// naturally and has no cap. Moved here from
    /// `lodestone_server::mob_spawn`, to deduplicate it, so the one
    /// [`MobCategory`] the integrated server's spawn driver iterates lives
    /// beside the category it names.
    pub const SPAWNING: [MobCategory; 7] = [
        MobCategory::Monster,
        MobCategory::Creature,
        MobCategory::Ambient,
        MobCategory::Axolotls,
        MobCategory::UndergroundWaterCreature,
        MobCategory::WaterCreature,
        MobCategory::WaterAmbient,
    ];

    /// Maximum naturally-spawned instances per chunk before the cap formula
    /// applies. `Misc` returns `-1` (uncapped) exactly as vanilla does.
    #[must_use]
    pub const fn max_instances_per_chunk(self) -> i32 {
        match self {
            Self::Monster => 70,
            Self::Creature => 10,
            Self::Ambient => 15,
            Self::Axolotls | Self::UndergroundWaterCreature | Self::WaterCreature => 5,
            Self::WaterAmbient => 20,
            Self::Misc => -1,
        }
    }

    /// Whether the category is friendly (non-hostile). Drives peaceful-mode and
    /// some spawn rules.
    #[must_use]
    pub const fn is_friendly(self) -> bool {
        !matches!(self, Self::Monster)
    }

    /// Whether members are *naturally* persistent (never despawn from distance).
    /// Only `Creature` and `Misc` are, in vanilla.
    #[must_use]
    pub const fn is_persistent(self) -> bool {
        matches!(self, Self::Creature | Self::Misc)
    }

    /// Distance (blocks) beyond which a member despawns instantly.
    #[must_use]
    pub const fn despawn_distance(self) -> i32 {
        match self {
            Self::WaterAmbient => 64,
            _ => 128,
        }
    }

    /// Distance (blocks) within which a member never despawns and its idle timer
    /// resets. Vanilla returns a constant 32 for every category.
    #[must_use]
    pub const fn no_despawn_distance(self) -> i32 {
        32
    }
}

/// The `NaturalSpawner` magic number `17² = 289`: the number of chunks in the
/// 17×17 spawn-eligible area a full mob cap is defined over.
pub const MOB_CAP_CHUNK_AREA: i32 = 17 * 17;

/// The global per-category mob cap for a given number of spawnable chunks,
/// matching `NaturalSpawner.canSpawnForCategoryGlobal`:
/// `maxInstancesPerChunk * spawnableChunks / 289`.
///
/// Returns `None` for an uncapped category (`Misc`).
#[must_use]
pub fn mob_cap(category: MobCategory, spawnable_chunks: i32) -> Option<i32> {
    let max = category.max_instances_per_chunk();
    if max < 0 {
        return None;
    }
    Some(max * spawnable_chunks / MOB_CAP_CHUNK_AREA)
}

/// Whether another mob of `category` may spawn given the current live count,
/// matching vanilla's `count < cap` (uncapped categories always may).
#[must_use]
pub fn category_has_room(category: MobCategory, current_count: i32, spawnable_chunks: i32) -> bool {
    match mob_cap(category, spawnable_chunks) {
        Some(cap) => current_count < cap,
        None => true,
    }
}

/// The outcome of a despawn check for one mob on one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DespawnDecision {
    /// The mob should be removed from the world this tick.
    Discard,
    /// The mob stays; its `no_action_time` idle counter should be reset to 0
    /// (it is close to a player or is persistent).
    ResetNoActionTime,
    /// The mob stays and its idle counter is left unchanged (it keeps ageing).
    Keep,
}

/// The inputs `check_despawn` needs about the world this tick. Kept as a plain
/// struct so the integrated server fills it from wherever it likes and the check
/// stays a pure function.
#[derive(Debug, Clone, Copy)]
pub struct DespawnCtx {
    /// The mob's category.
    pub category: MobCategory,
    /// Whether the level difficulty is currently Peaceful.
    pub difficulty_peaceful: bool,
    /// Whether this entity type is allowed to exist in Peaceful
    /// (`EntityType.isAllowedInPeaceful`; e.g. a zombie is not, a wither is).
    pub allowed_in_peaceful: bool,
    /// Whether the mob has been marked `PersistenceRequired`.
    pub persistence_required: bool,
    /// Whether the mob requires custom persistence this tick (leashed, ridden).
    pub requires_custom_persistence: bool,
    /// `removeWhenFarAway`: base mobs return `true`; some (tamed, named) return
    /// `false` to opt out of distance despawning even without persistence.
    pub remove_when_far_away: bool,
    /// Squared distance to the nearest player, or `None` if no player exists.
    pub nearest_player_dist_sqr: Option<f64>,
    /// The mob's `noActionTime` idle-tick counter.
    pub no_action_time: u32,
    /// The value of `random.nextInt(800) == 0` for this tick (caller owns RNG).
    pub random_800_is_zero: bool,
}

/// Vanilla `Mob.checkDespawn`, expressed as a pure decision.
///
/// The ordering mirrors the decompiled reference: peaceful eviction first, then
/// the persistence short-circuit, then the two distance gates. The instant gate
/// (`dist > despawnDistance`) and the random gate (`idle > 600` and a 1/800 roll
/// beyond `noDespawnDistance`) both yield [`DespawnDecision::Discard`]; being
/// within `noDespawnDistance` yields [`DespawnDecision::ResetNoActionTime`].
///
/// The subtlety worth calling out: the two multiply/gate stages are **not**
/// folded — a mob 40 blocks away with a fresh idle counter is kept *and* not
/// reset, so its counter keeps climbing toward the 600 threshold. Collapsing the
/// gates would make far mobs either never despawn or despawn immediately.
#[must_use]
pub fn check_despawn(ctx: &DespawnCtx) -> DespawnDecision {
    if ctx.difficulty_peaceful && !ctx.allowed_in_peaceful {
        return DespawnDecision::Discard;
    }
    if ctx.persistence_required || ctx.requires_custom_persistence {
        return DespawnDecision::ResetNoActionTime;
    }
    let Some(dist_sqr) = ctx.nearest_player_dist_sqr else {
        // No player: vanilla's `getNearestPlayer` returns null and the whole
        // block is skipped, leaving the idle counter to keep climbing.
        return DespawnDecision::Keep;
    };

    let despawn = f64::from(ctx.category.despawn_distance());
    let despawn_sqr = despawn * despawn;
    if dist_sqr > despawn_sqr && ctx.remove_when_far_away {
        return DespawnDecision::Discard;
    }

    let no_despawn = f64::from(ctx.category.no_despawn_distance());
    let no_despawn_sqr = no_despawn * no_despawn;
    if ctx.no_action_time > 600
        && ctx.random_800_is_zero
        && dist_sqr > no_despawn_sqr
        && ctx.remove_when_far_away
    {
        DespawnDecision::Discard
    } else if dist_sqr < no_despawn_sqr {
        DespawnDecision::ResetNoActionTime
    } else {
        DespawnDecision::Keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_constants_match_vanilla() {
        assert_eq!(MobCategory::Monster.max_instances_per_chunk(), 70);
        assert_eq!(MobCategory::Creature.max_instances_per_chunk(), 10);
        assert_eq!(MobCategory::WaterAmbient.max_instances_per_chunk(), 20);
        assert_eq!(MobCategory::Misc.max_instances_per_chunk(), -1);
        assert!(!MobCategory::Monster.is_friendly());
        assert!(MobCategory::Creature.is_friendly());
        assert!(MobCategory::Creature.is_persistent());
        assert!(!MobCategory::Monster.is_persistent());
        assert_eq!(MobCategory::WaterAmbient.despawn_distance(), 64);
        assert_eq!(MobCategory::Monster.despawn_distance(), 128);
        assert_eq!(MobCategory::Monster.no_despawn_distance(), 32);
        // `SPAWNING` excludes `Misc` (never spawns naturally) and nothing else.
        assert!(!MobCategory::SPAWNING.contains(&MobCategory::Misc));
        assert_eq!(MobCategory::SPAWNING.len(), 7);
    }

    #[test]
    fn mob_cap_uses_vanilla_formula() {
        // 70 monsters/chunk over a full 289-chunk area = 70.
        assert_eq!(mob_cap(MobCategory::Monster, 289), Some(70));
        // Half the area rounds down like the integer division vanilla uses.
        assert_eq!(mob_cap(MobCategory::Monster, 144), Some(70 * 144 / 289));
        assert_eq!(mob_cap(MobCategory::Misc, 289), None);
        assert!(category_has_room(MobCategory::Monster, 69, 289));
        assert!(!category_has_room(MobCategory::Monster, 70, 289));
        assert!(category_has_room(MobCategory::Misc, 9999, 289));
    }

    fn base_ctx() -> DespawnCtx {
        DespawnCtx {
            category: MobCategory::Monster,
            difficulty_peaceful: false,
            allowed_in_peaceful: false,
            persistence_required: false,
            requires_custom_persistence: false,
            remove_when_far_away: true,
            nearest_player_dist_sqr: Some(50.0 * 50.0),
            no_action_time: 0,
            random_800_is_zero: false,
        }
    }

    #[test]
    fn peaceful_evicts_disallowed_monsters() {
        let ctx = DespawnCtx {
            difficulty_peaceful: true,
            ..base_ctx()
        };
        assert_eq!(check_despawn(&ctx), DespawnDecision::Discard);
        // A wither (allowed in peaceful) is not evicted.
        let ok = DespawnCtx {
            allowed_in_peaceful: true,
            ..ctx
        };
        assert_ne!(check_despawn(&ok), DespawnDecision::Discard);
    }

    #[test]
    fn persistence_keeps_and_resets_timer() {
        let ctx = DespawnCtx {
            persistence_required: true,
            nearest_player_dist_sqr: Some(9999.0 * 9999.0),
            ..base_ctx()
        };
        assert_eq!(check_despawn(&ctx), DespawnDecision::ResetNoActionTime);
    }

    #[test]
    fn instant_despawn_past_128() {
        let ctx = DespawnCtx {
            nearest_player_dist_sqr: Some(129.0 * 129.0),
            ..base_ctx()
        };
        assert_eq!(check_despawn(&ctx), DespawnDecision::Discard);
        // Just inside 128 with a fresh timer: kept, not reset (still ageing).
        let inside = DespawnCtx {
            nearest_player_dist_sqr: Some(100.0 * 100.0),
            ..base_ctx()
        };
        assert_eq!(check_despawn(&inside), DespawnDecision::Keep);
    }

    #[test]
    fn random_despawn_needs_idle_and_roll_and_distance() {
        // Idle long enough, rolled a 0, and beyond 32: despawns.
        let go = DespawnCtx {
            nearest_player_dist_sqr: Some(40.0 * 40.0),
            no_action_time: 601,
            random_800_is_zero: true,
            ..base_ctx()
        };
        assert_eq!(check_despawn(&go), DespawnDecision::Discard);
        // Same but the roll missed: kept.
        let miss = DespawnCtx {
            random_800_is_zero: false,
            ..go
        };
        assert_eq!(check_despawn(&miss), DespawnDecision::Keep);
        // Same but not idle long enough: kept.
        let fresh = DespawnCtx {
            no_action_time: 599,
            ..go
        };
        assert_eq!(check_despawn(&fresh), DespawnDecision::Keep);
    }

    #[test]
    fn within_32_resets_timer() {
        let ctx = DespawnCtx {
            nearest_player_dist_sqr: Some(20.0 * 20.0),
            no_action_time: 700,
            random_800_is_zero: true,
            ..base_ctx()
        };
        // Inside no-despawn radius always wins the reset branch.
        assert_eq!(check_despawn(&ctx), DespawnDecision::ResetNoActionTime);
    }

    #[test]
    fn no_player_keeps_mob() {
        let ctx = DespawnCtx {
            nearest_player_dist_sqr: None,
            ..base_ctx()
        };
        assert_eq!(check_despawn(&ctx), DespawnDecision::Keep);
    }

    #[test]
    fn remove_when_far_away_false_opts_out() {
        let ctx = DespawnCtx {
            nearest_player_dist_sqr: Some(200.0 * 200.0),
            remove_when_far_away: false,
            ..base_ctx()
        };
        // A tamed/named mob far away is neither discarded nor reset.
        assert_eq!(check_despawn(&ctx), DespawnDecision::Keep);
    }
}
