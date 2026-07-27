//! Version-free natural mob-spawn accounting and despawn logic.
//!
//! This is the *engine* half of singleplayer mob spawning: the mob-cap
//! arithmetic and the despawn state machine, both of which are pure vanilla
//! algorithm and belong in this version-free crate. The *data* half — which
//! entity type spawns in which biome, at what light level, from which registry
//! spawn list — is version and registry knowledge that must **not** live here,
//! exactly as the state-id→`PathType` classifier stays out of
//! [`ChunkWorld`](crate::ChunkWorld). It arrives through the
//! [`SpawnCandidateSource`] seam the caller injects.
//!
//! # Why the category table is hardcoded but the mapping is a seam
//!
//! [`MobCategory`]'s caps and despawn distances are stable vanilla constants —
//! `MONSTER` has been `70` per 289 chunks for many versions. Hardcoding them
//! mirrors `lodestone-physics` hardcoding `0.08` gravity: a stable vanilla
//! constant, not a per-connection knob. What genuinely moves between versions
//! is *which entity type is in which category* (and which categories exist at
//! all), so that mapping is the caller's job, surfaced as the category on each
//! [`SpawnCandidate`] rather than an entity-type table baked in here.
//!
//! # The two despawn gates must not be folded
//!
//! Vanilla `Mob.checkDespawn` has two independent distance gates. The subtle
//! part — and the one this project was warned about — is that a mob between the
//! immune radius (32) and the instant radius (128) is **kept but not reset**: it
//! keeps ageing toward the 600-tick random-despawn threshold. Folding the gates
//! (resetting the age timer whenever the mob survives the instant gate) makes a
//! mob at 40 blocks immortal, which is wrong and invisible in a short test. See
//! [`check_despawn`].

use lodestone_entity::pathfinding::MobShape;
use lodestone_model::Vec3;

/// Vanilla's `NaturalSpawner.MAGIC_NUMBER`: `17² = 289`. The per-category global
/// cap is `max_per_chunk * spawnable_chunks / MAGIC_NUMBER`, so a single player
/// with a full 8-chunk spawn radius (≈289 spawnable chunks) yields a cap equal
/// to the per-chunk maximum.
pub const MAGIC_NUMBER: i32 = 289;

/// Vanilla mob spawn categories (26.2 `MobCategory`).
///
/// Every category except [`Misc`](MobCategory::Misc) participates in natural
/// spawning; `Misc` (dropped items, projectiles, …) has no cap (`-1`) and is
/// filtered out. Caps and distances are the exact 26.2 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MobCategory {
    /// Hostile mobs (zombies, skeletons, …). Cap 70.
    Monster,
    /// Passive land animals (pigs, cows, …). Cap 10; persistent.
    Creature,
    /// Ambient mobs (bats). Cap 15.
    Ambient,
    /// Axolotls. Cap 5.
    Axolotls,
    /// Underground water creatures (glow squid). Cap 5.
    UndergroundWaterCreature,
    /// Water creatures (squid, dolphins). Cap 5.
    WaterCreature,
    /// Water ambient (fish). Cap 20; instant-despawn at 64, not 128.
    WaterAmbient,
    /// Non-spawning miscellany (items, projectiles). No cap.
    Misc,
}

impl MobCategory {
    /// The categories that participate in natural spawning, in vanilla order.
    /// `Misc` is excluded (it never spawns naturally).
    pub const SPAWNING: [MobCategory; 7] = [
        MobCategory::Monster,
        MobCategory::Creature,
        MobCategory::Ambient,
        MobCategory::Axolotls,
        MobCategory::UndergroundWaterCreature,
        MobCategory::WaterCreature,
        MobCategory::WaterAmbient,
    ];

    /// `getMaxInstancesPerChunk()`: the per-289-chunk cap numerator. `Misc` is
    /// `-1` (uncapped / non-spawning).
    #[must_use]
    pub const fn max_per_chunk(self) -> i32 {
        match self {
            MobCategory::Monster => 70,
            MobCategory::Creature => 10,
            MobCategory::Ambient => 15,
            MobCategory::Axolotls
            | MobCategory::UndergroundWaterCreature
            | MobCategory::WaterCreature => 5,
            MobCategory::WaterAmbient => 20,
            MobCategory::Misc => -1,
        }
    }

    /// `getDespawnDistance()`: beyond this many blocks a mob is despawned
    /// instantly (gate A). `64` for water-ambient, `128` for everything else.
    #[must_use]
    pub const fn despawn_distance(self) -> i32 {
        match self {
            MobCategory::WaterAmbient => 64,
            _ => 128,
        }
    }

    /// `getNoDespawnDistance()`: within this many blocks a mob is immune to the
    /// random far-despawn and its age timer is reset. Always `32`.
    #[must_use]
    pub const fn no_despawn_distance(self) -> i32 {
        32
    }

    /// Whether the category is friendly (spawns even with hostile spawning off).
    #[must_use]
    pub const fn is_friendly(self) -> bool {
        !matches!(self, MobCategory::Monster)
    }

    /// Whether the category is persistent by default (`Creature`, `Misc`):
    /// persistent mobs never naturally despawn.
    #[must_use]
    pub const fn is_persistent(self) -> bool {
        matches!(self, MobCategory::Creature | MobCategory::Misc)
    }
}

/// Per-cycle mob-cap accounting: how many spawnable chunks are in range and how
/// many mobs of each category are currently alive.
///
/// Rebuilt each spawn cycle from a census of live mobs, exactly like vanilla's
/// `NaturalSpawner.SpawnState`. The global cap is derived, never stored, so it
/// always tracks the current spawnable-chunk count.
#[derive(Debug, Clone)]
pub struct SpawnState {
    spawnable_chunks: i32,
    counts: [i32; 7],
}

impl SpawnState {
    /// A fresh accounting for `spawnable_chunks` chunks and zero mobs.
    #[must_use]
    pub fn new(spawnable_chunks: i32) -> Self {
        Self {
            spawnable_chunks: spawnable_chunks.max(0),
            counts: [0; 7],
        }
    }

    /// The index of a spawning category in [`MobCategory::SPAWNING`], or `None`
    /// for `Misc` (which is never counted for spawning).
    const fn spawning_index(category: MobCategory) -> Option<usize> {
        match category {
            MobCategory::Monster => Some(0),
            MobCategory::Creature => Some(1),
            MobCategory::Ambient => Some(2),
            MobCategory::Axolotls => Some(3),
            MobCategory::UndergroundWaterCreature => Some(4),
            MobCategory::WaterCreature => Some(5),
            MobCategory::WaterAmbient => Some(6),
            MobCategory::Misc => None,
        }
    }

    /// Records one live mob of `category`. `Misc` is ignored.
    pub fn record(&mut self, category: MobCategory) {
        if let Some(i) = Self::spawning_index(category) {
            self.counts[i] += 1;
        }
    }

    /// The current live count for `category` (`0` for `Misc`).
    #[must_use]
    pub fn count(&self, category: MobCategory) -> i32 {
        Self::spawning_index(category).map_or(0, |i| self.counts[i])
    }

    /// The global cap for `category`: `max_per_chunk * spawnable_chunks / 289`,
    /// using vanilla's integer division. `Misc` and any negative cap yield `0`.
    #[must_use]
    pub fn global_cap(&self, category: MobCategory) -> i32 {
        let max = category.max_per_chunk();
        if max < 0 {
            return 0;
        }
        max * self.spawnable_chunks / MAGIC_NUMBER
    }

    /// Whether another mob of `category` may spawn: `count < global_cap`, exactly
    /// vanilla's `canSpawnForCategoryGlobal`.
    #[must_use]
    pub fn can_spawn(&self, category: MobCategory) -> bool {
        self.count(category) < self.global_cap(category)
    }

    /// The spawnable-chunk count this accounting was built for.
    #[must_use]
    pub fn spawnable_chunks(&self) -> i32 {
        self.spawnable_chunks
    }
}

/// The outcome of a [`check_despawn`] evaluation for one mob.
///
/// The two fields are independent because vanilla's two gates are: a mob can be
/// discarded, have its age timer reset, or **neither** (the kept-but-ageing
/// middle band). `discard` and `reset_timer` are never both true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DespawnOutcome {
    /// The mob should be removed from the world.
    pub discard: bool,
    /// The mob's `no_action_time` age timer should be reset to zero (it is
    /// within the immune radius).
    pub reset_timer: bool,
}

impl DespawnOutcome {
    const KEEP: Self = Self {
        discard: false,
        reset_timer: false,
    };
    const DISCARD: Self = Self {
        discard: true,
        reset_timer: false,
    };
    const RESET: Self = Self {
        discard: false,
        reset_timer: true,
    };
}

/// Applies vanilla `Mob.checkDespawn`'s two distance gates for one non-persistent
/// mob, given the squared distance to the nearest player.
///
/// * **Gate A (instant):** beyond `despawn_distance` the mob is discarded.
/// * **Gate B (random far):** if it has been idle over `600` ticks, is beyond
///   the immune radius (`32`), and a `1/800` roll hits, it is discarded.
/// * **Immune reset:** if it is *within* the immune radius, its age timer is
///   reset.
/// * **Otherwise (the middle band, e.g. 40 blocks):** kept, and — crucially —
///   the timer is **not** reset, so it keeps ageing toward gate B.
///
/// `rng_hit_800` is the result of vanilla's `random.nextInt(800) == 0`, passed in
/// so the decision is pure and exactly testable; `remove_when_far_away` is the
/// mob's own `removeWhenFarAway` override (default `true` for despawnable mobs).
#[must_use]
pub fn check_despawn(
    category: MobCategory,
    dist_sqr_to_player: f64,
    no_action_time: i32,
    rng_hit_800: bool,
    remove_when_far_away: bool,
) -> DespawnOutcome {
    let despawn = f64::from(category.despawn_distance());
    if dist_sqr_to_player > despawn * despawn && remove_when_far_away {
        return DespawnOutcome::DISCARD;
    }
    let immune = f64::from(category.no_despawn_distance());
    let immune_sqr = immune * immune;
    if no_action_time > 600
        && rng_hit_800
        && dist_sqr_to_player > immune_sqr
        && remove_when_far_away
    {
        DespawnOutcome::DISCARD
    } else if dist_sqr_to_player < immune_sqr {
        DespawnOutcome::RESET
    } else {
        DespawnOutcome::KEEP
    }
}

/// A concrete mob the caller proposes to spawn at a position.
///
/// The version/terrain-dependent decisions — *which* mob type, at *what* valid
/// position, with what body — are made by the [`SpawnCandidateSource`]; this is
/// just the version-free result the spawn driver needs to instantiate a mob.
#[derive(Debug, Clone)]
pub struct SpawnCandidate {
    /// Where to place the mob (a validated, ground-supported position).
    pub pos: Vec3,
    /// The mob's collision body (drives path validity).
    pub shape: MobShape,
    /// Blocks per tick the follower advances (derived from movement speed).
    pub step_per_tick: f64,
    /// A\* open-set budget (`floor(follow_range * 16)`).
    pub visited_budget: i32,
}

/// The seam supplying the version/registry/terrain half of natural spawning.
///
/// Injected by the caller (the singleplayer shell) because "which mob spawns
/// here, and is this a legal spawn position" needs biome spawn lists, light
/// levels, and the entity registry — knowledge this version-free crate must not
/// embed. The driver only ever asks for a candidate *after* it has confirmed the
/// category is under its cap, so a source need not repeat the cap check.
pub trait SpawnCandidateSource {
    /// Propose a mob of `category` to spawn near chunk `(cx, cz)`, or `None` if
    /// nothing suitable can spawn there this cycle (wrong biome, too bright, no
    /// valid ground, or simply a declined random roll).
    fn candidate(&mut self, category: MobCategory, cx: i32, cz: i32) -> Option<SpawnCandidate>;
}

/// A tiny deterministic RNG (SplitMix64) for the spawn driver and its tests, so
/// spawning needs no `rand` dependency and is reproducible across seeds.
#[derive(Debug, Clone)]
pub struct SpawnRng(u64);

impl SpawnRng {
    /// Seeds the RNG.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A non-negative `i32` in `[0, bound)`, matching Java `nextInt(bound)`'s
    /// contract for the uses here (`bound > 0`).
    pub fn next_int(&mut self, bound: i32) -> i32 {
        if bound <= 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap formula is `max * chunks / 289` with vanilla integer truncation.
    /// A full single-player radius (289 chunks) yields caps equal to the
    /// per-chunk maxima; fewer chunks scale down and truncate toward zero.
    #[test]
    fn global_cap_matches_vanilla_formula() {
        let full = SpawnState::new(289);
        assert_eq!(full.global_cap(MobCategory::Monster), 70);
        assert_eq!(full.global_cap(MobCategory::Creature), 10);
        assert_eq!(full.global_cap(MobCategory::Ambient), 15);
        assert_eq!(full.global_cap(MobCategory::WaterAmbient), 20);
        assert_eq!(full.global_cap(MobCategory::WaterCreature), 5);

        // Double the chunks → double the cap.
        assert_eq!(SpawnState::new(578).global_cap(MobCategory::Monster), 140);

        // 100 chunks: 70*100/289 = 7000/289 = 24.2… → 24 (truncated).
        assert_eq!(SpawnState::new(100).global_cap(MobCategory::Monster), 24);
        // 10*100/289 = 3.46 → 3.
        assert_eq!(SpawnState::new(100).global_cap(MobCategory::Creature), 3);

        // Misc has no cap and never spawns.
        assert_eq!(full.global_cap(MobCategory::Misc), 0);
        assert!(!full.can_spawn(MobCategory::Misc));
    }

    /// `can_spawn` is `count < cap`: it stays true up to the cap, then blocks.
    #[test]
    fn can_spawn_gates_exactly_at_cap() {
        let mut state = SpawnState::new(289); // monster cap 70
        for _ in 0..69 {
            assert!(state.can_spawn(MobCategory::Monster));
            state.record(MobCategory::Monster);
        }
        assert!(state.can_spawn(MobCategory::Monster)); // count 69 < 70
        state.record(MobCategory::Monster);
        assert!(!state.can_spawn(MobCategory::Monster)); // count 70, not < 70
        assert_eq!(state.count(MobCategory::Monster), 70);
    }

    /// Gate A: beyond the instant-despawn distance the mob is discarded outright,
    /// regardless of its age timer.
    #[test]
    fn instant_despawn_beyond_128() {
        // 130 blocks → 130² = 16900 > 128² = 16384.
        let out = check_despawn(MobCategory::Monster, 130.0 * 130.0, 0, false, true);
        assert_eq!(out, DespawnOutcome::DISCARD);
        // Water-ambient uses 64, so 70 blocks despawns it but not a monster.
        assert!(check_despawn(MobCategory::WaterAmbient, 70.0 * 70.0, 0, false, true).discard);
        assert!(!check_despawn(MobCategory::Monster, 70.0 * 70.0, 0, false, true).discard);
    }

    /// The kept-but-not-reset middle band — the gate that must not be folded.
    /// A mob at 40 blocks is past the immune radius (32) but inside the instant
    /// radius (128): it is neither discarded nor reset, so it keeps ageing.
    #[test]
    fn middle_band_at_40_blocks_is_kept_not_reset() {
        let dist_sqr = 40.0 * 40.0; // 1600, between 32²=1024 and 128²=16384
        // Young timer, roll misses: kept, not reset.
        let out = check_despawn(MobCategory::Monster, dist_sqr, 100, false, true);
        assert_eq!(out, DespawnOutcome::KEEP);
        assert!(
            !out.reset_timer,
            "40-block mob must NOT have its age timer reset"
        );

        // Once it has aged past 600 and the 1/800 roll hits, gate B fires.
        let aged = check_despawn(MobCategory::Monster, dist_sqr, 601, true, true);
        assert!(
            aged.discard,
            "aged idle mob past 600 ticks should random-despawn"
        );

        // A mob at 20 blocks (inside the immune radius) is reset every check and
        // can therefore never reach gate B — the immortality the fold would give
        // the 40-block mob by mistake.
        let near = check_despawn(MobCategory::Monster, 20.0 * 20.0, 601, true, true);
        assert_eq!(near, DespawnOutcome::RESET);
        assert!(!near.discard);
    }

    /// Gate B needs *all* of: aged > 600, past the immune radius, and the roll.
    /// Missing any one keeps the mob.
    #[test]
    fn far_random_despawn_requires_all_conditions() {
        let dist_sqr = 40.0 * 40.0;
        // Aged and far but roll misses → kept.
        assert_eq!(
            check_despawn(MobCategory::Monster, dist_sqr, 601, false, true),
            DespawnOutcome::KEEP
        );
        // Roll hits and far but not aged → kept.
        assert_eq!(
            check_despawn(MobCategory::Monster, dist_sqr, 599, true, true),
            DespawnOutcome::KEEP
        );
        // removeWhenFarAway=false (e.g. a mob that refuses far-despawn) → kept
        // even fully aged past the instant gate.
        assert_eq!(
            check_despawn(MobCategory::Monster, 200.0 * 200.0, 601, true, false),
            DespawnOutcome::KEEP
        );
    }

    /// Category metadata matches the 26.2 table exactly.
    #[test]
    fn category_table_is_vanilla() {
        assert!(!MobCategory::Monster.is_friendly());
        assert!(MobCategory::Creature.is_friendly());
        assert!(MobCategory::Creature.is_persistent());
        assert!(!MobCategory::Monster.is_persistent());
        assert_eq!(MobCategory::WaterAmbient.despawn_distance(), 64);
        assert_eq!(MobCategory::Monster.despawn_distance(), 128);
        assert_eq!(MobCategory::Monster.no_despawn_distance(), 32);
        // Misc is excluded from the spawning set.
        assert!(!MobCategory::SPAWNING.contains(&MobCategory::Misc));
        assert_eq!(MobCategory::SPAWNING.len(), 7);
    }
}
