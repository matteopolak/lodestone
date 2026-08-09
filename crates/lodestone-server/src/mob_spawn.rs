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

use std::str::FromStr;

use lodestone_entity::AttributeMap;
use lodestone_entity::pathfinding::MobShape;
use lodestone_model::adapter::VersionAdapter;
use lodestone_model::{Identifier, Vec3};

/// Folds a version's **base** entity dimensions with the mob's resolved
/// attribute map into a pathfinding [`MobShape`] — the per-spawn consumer of the
/// dimension census.
///
/// The seam is split so this version-free crate never embeds a dimension table:
///
/// - **width / height** come from `adapter.entity_dimensions(entity_type_id)` —
///   the census, base geometry at scale 1 — then multiplied by the `SCALE`
///   attribute. The census is scale-1 by construction, so `SCALE` is folded here
///   (caller-side), never baked into the table.
/// - **`max_up_step`** comes from the *resolved* `STEP_HEIGHT` attribute
///   (post-modifier-fold), **not** the census geometry. Vanilla
///   `Entity.maxUpStep()` returns `getAttributeValue(STEP_HEIGHT)`
///   (`LivingEntity.java:3976`); sourcing it from static geometry would silently
///   disagree with the pathfinder the moment any modifier existed. The `as f32`
///   is that call site's `(float)` cast.
///
/// `entity_type_id` is the version's numeric network id (what `entity_dimensions`
/// is keyed by, and what an `add_entity` packet carries). Resolving a
/// [`ResourceKey`](lodestone_model::ResourceKey) such as `minecraft:zombie` to
/// that id is version knowledge and stays on the version-aware caller's side (it
/// asks the registry for the adapter), exactly like the rest of the version half
/// of [`SpawnCandidate`] — this crate only ever names the version-free
/// [`VersionAdapter`] trait.
///
/// Returns `None` when the adapter reports no census for `entity_type_id` (an
/// unknown type, or a version that has not homed a census); the caller chooses
/// the fallback rather than receiving a guessed box.
#[must_use]
pub fn resolve_mob_shape(
    adapter: &dyn VersionAdapter,
    entity_type_id: i32,
    attributes: &AttributeMap,
) -> Option<MobShape> {
    let base = adapter.entity_dimensions(entity_type_id)?;
    let scale = attribute_value(attributes, "minecraft:scale", 1.0) as f32;
    let step_height = attribute_value(attributes, "minecraft:step_height", 0.6) as f32;
    let mut shape = MobShape::land(base.width * scale, base.height * scale);
    shape.max_up_step = step_height;
    Some(shape)
}

/// Reads a computed attribute value by key, falling back to `fallback` only when
/// the key is not a registered attribute at all. A registered-but-absent
/// attribute already resolves to its registry default inside
/// [`AttributeMap::value`], so `scale`/`step_height` return 1.0 / 0.6 for a
/// default map without the fallback ever engaging.
fn attribute_value(attributes: &AttributeMap, key: &str, fallback: f64) -> f64 {
    Identifier::from_str(key)
        .ok()
        .and_then(|id| attributes.value(&id))
        .unwrap_or(fallback)
}

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

/// The entity types vanilla registers with `EntityType.Builder::notInPeaceful`,
/// by registry path — the **38** types that may not exist on `Peaceful`.
///
/// # Why this is a list and not `category == Monster`
///
/// Because the category is not the same question, and the difference is
/// asymmetric in the direction that matters. Every one of these 38 is
/// `MobCategory.MONSTER`, but **seven MONSTER types are not here**:
/// `piglin`, `shulker`, `ender_dragon`, `zombie_horse`, `zombie_nautilus`,
/// `camel_husk` and `sulfur_cube`. Vanilla's own gates are keyed on the flag,
/// never on the category — `Mob.checkDespawn`'s
/// `difficulty == PEACEFUL && !getType().isAllowedInPeaceful()`,
/// `SpawnPlacements.checkSpawnRules`' identical first guard, and
/// `EntityType.canSpawn`'s `isAllowedInPeaceful() || difficulty != PEACEFUL` —
/// so answering from the category would despawn a shulker the moment a player
/// switched to Peaceful, and vanilla keeps it.
///
/// # Provenance
///
/// Extracted from the pinned 26.2 decompile by splitting
/// `net.minecraft.world.entity.EntityTypes` on its `EntityTypeIds.` registrations
/// and keeping every block containing `notInPeaceful()`: 38 hits, all
/// `MobCategory.MONSTER`, zero of them ambiguous. `notInPeaceful` has exactly one
/// other occurrence in the whole tree — the builder method's own definition — so
/// the registration list is the complete set.
///
/// # How to change it
///
/// When the pinned version moves, re-run that extraction rather than editing
/// names here; `peaceful_forbids_exactly_the_notinpeaceful_registrations` below
/// pins the count and the seven MONSTER exceptions so a hand edit that drops one
/// fails loudly.
static NOT_ALLOWED_IN_PEACEFUL: [&str; 38] = [
    "blaze",
    "bogged",
    "breeze",
    "cave_spider",
    "creaking",
    "creeper",
    "drowned",
    "elder_guardian",
    "enderman",
    "endermite",
    "evoker",
    "ghast",
    "giant",
    "guardian",
    "hoglin",
    "husk",
    "illusioner",
    "magma_cube",
    "parched",
    "phantom",
    "piglin_brute",
    "pillager",
    "ravager",
    "silverfish",
    "skeleton",
    "slime",
    "spider",
    "stray",
    "vex",
    "vindicator",
    "warden",
    "witch",
    "wither",
    "wither_skeleton",
    "zoglin",
    "zombie",
    "zombie_villager",
    "zombified_piglin",
];

/// `EntityType.isAllowedInPeaceful` — whether an entity of this registry path may
/// exist while the world difficulty is `Peaceful`.
///
/// `path` is the namespace-less path (`"zombie"`, not `"minecraft:zombie"`).
/// Anything not in [`NOT_ALLOWED_IN_PEACEFUL`] answers `true`, which is vanilla's
/// own default (`EntityType.Builder`'s field starts at `true` and only
/// `notInPeaceful()` clears it), so an unmodelled or misspelled species is kept
/// rather than silently deleted.
#[must_use]
pub fn allowed_in_peaceful(path: &str) -> bool {
    !NOT_ALLOWED_IN_PEACEFUL.contains(&path)
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
/// The version/terrain-dependent decisions — *which* species, at *what* valid
/// position — are made by the [`SpawnCandidateSource`]; this is just the result
/// the spawn driver needs to instantiate a mob. The mob's **body** is not here on
/// purpose: it follows from the species, and
/// [`MobSim::spawn_species`](crate::MobSim::spawn_species) already resolves it
/// from the real 26.2 census along with the species' attributes and goal set. A
/// candidate carrying its own shape invited two answers to one question.
#[derive(Debug, Clone)]
pub struct SpawnCandidate {
    /// Where to place the mob (a validated, ground-supported position).
    pub pos: Vec3,
    /// Which species — a vanilla entity id such as `minecraft:sheep`.
    pub entity_type: lodestone_model::ResourceKey,
}

/// The seam supplying the registry/terrain half of natural spawning.
///
/// Injected by the caller because "which mob spawns here, and is this a legal
/// spawn position" needs biome spawn lists, light levels and the entity registry —
/// knowledge this module must not embed. [`crate::natural_spawn::NaturalSpawner`]
/// is the production implementer. The driver only ever asks *after* it has
/// confirmed the category is under its cap, so a source need not repeat the cap
/// check.
pub trait SpawnCandidateSource {
    /// Propose the mobs of `category` that spawn in chunk `(cx, cz)` this cycle,
    /// or an empty vector when nothing can (wrong biome, too bright, no valid
    /// ground, or simply a declined random roll).
    ///
    /// **A group, not a single mob**, because vanilla's
    /// `NaturalSpawner.spawnCategoryForChunk` is a cluster loop whose RNG draw
    /// order and count *is* the spawn rate. Returning one mob per call would let
    /// the driver interleave cap checks into the middle of a group's draws, which
    /// changes the stream and therefore the rates.
    fn cluster(&mut self, category: MobCategory, cx: i32, cz: i32) -> Vec<SpawnCandidate>;
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

    /// A uniform `f64` in `[0.0, 1.0)` with a 53-bit mantissa, matching Java
    /// `RandomSource.nextDouble()`'s contract — the roll
    /// [`crate::composter::Composter::insert`] asks its caller for (taking the
    /// top 53 bits of one draw and scaling by `2^-53` is equivalent to Java's
    /// `(next(26) << 27) + next(27)) / (1L << 53)`).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A uniform `f32` in `[0.0, 1.0)` with a 24-bit mantissa, matching Java
    /// `RandomSource.nextFloat()`'s contract (`nextInt(24) * 2^-24`) — the roll
    /// loot-table conditions and functions make (`random_chance`, binomial
    /// draws, `survives_explosion`). Takes the top 24 bits of one draw and
    /// scales by `2^-24`, which is distributionally identical to Java's 24-bit
    /// draw; only the exact stream differs (see `crate::loot`'s module doc for
    /// why that stream divergence is a follow-up, not a bug here).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
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

    // --- dimension-census fold (`resolve_mob_shape`) ---------------------

    use lodestone_entity::AttributeMap;
    use lodestone_model::action::ClientAction;
    use lodestone_model::adapter::{
        AdapterError, ConnectionState, Directive, EntityBaseDimensions, LoginProfile,
        ServerAddress, VersionAdapter, WorldSink,
    };

    /// A minimal [`VersionAdapter`] that answers `entity_dimensions` from a fixed
    /// table and panics on every other method — the fold only ever calls
    /// `entity_dimensions`, so a real adapter (which impls the whole trait) slots
    /// in unchanged. The table holds *real* census numbers so the fold is proven
    /// against the geometry the live server actually reports.
    #[derive(Debug)]
    struct CensusStub(std::collections::HashMap<i32, EntityBaseDimensions>);

    impl CensusStub {
        fn with(pairs: &[(i32, f32, f32)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|&(id, width, height)| (id, EntityBaseDimensions { width, height }))
                    .collect(),
            )
        }
    }

    impl VersionAdapter for CensusStub {
        fn protocol_version(&self) -> i32 {
            0
        }
        fn minecraft_versions(&self) -> &'static [&'static str] {
            &[]
        }
        fn supports(&self, _protocol: i32) -> bool {
            false
        }
        fn begin_login(
            &self,
            _profile: &LoginProfile,
            _server: &ServerAddress,
        ) -> Result<Vec<Directive>, AdapterError> {
            unimplemented!("census stub")
        }
        fn handle_packet(
            &self,
            _world: &mut dyn WorldSink,
            _state: ConnectionState,
            _packet_id: i32,
            _payload: &[u8],
        ) -> Result<Vec<Directive>, AdapterError> {
            unimplemented!("census stub")
        }
        fn encode_action(
            &self,
            _state: ConnectionState,
            _action: &ClientAction,
        ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
            unimplemented!("census stub")
        }
        fn entity_dimensions(&self, entity_type_id: i32) -> Option<EntityBaseDimensions> {
            self.0.get(&entity_type_id).copied()
        }
    }

    fn scale_key() -> Identifier {
        Identifier::from_str("minecraft:scale").unwrap()
    }

    fn step_key() -> Identifier {
        Identifier::from_str("minecraft:step_height").unwrap()
    }

    /// The census width/height reach the shape and `SCALE`/`STEP_HEIGHT` default
    /// through the attribute registry (1.0 / 0.6) when the map carries no
    /// override. Real zombie geometry: 0.6 × 1.95.
    #[test]
    fn resolve_folds_census_geometry_with_default_attributes() {
        let adapter = CensusStub::with(&[(151, 0.6, 1.95)]);
        let attrs = AttributeMap::new();
        let shape = resolve_mob_shape(&adapter, 151, &attrs).expect("known type");
        assert_eq!(shape.width, 0.6);
        assert_eq!(shape.height, 1.95);
        // STEP_HEIGHT comes from the attribute registry default (0.6), not the
        // MobShape::land literal — here they coincide, the next test separates
        // them.
        assert_eq!(shape.max_up_step, 0.6);
        // Cell extent a real zombie occupies: 1 wide, 2 tall.
        assert_eq!(shape.cell_width(), 1);
        assert_eq!(shape.cell_height(), 2);
    }

    /// `SCALE` multiplies width and height (the census is scale-1), so a baby /
    /// scaled mob resizes both axes.
    #[test]
    fn resolve_folds_scale_into_both_axes() {
        let adapter = CensusStub::with(&[(151, 0.6, 1.95)]);
        let mut attrs = AttributeMap::new();
        attrs.get_or_default(&scale_key()).set_base_value(2.0);
        let shape = resolve_mob_shape(&adapter, 151, &attrs).expect("known type");
        assert!((shape.width - 1.2).abs() < 1e-6, "width = {}", shape.width);
        assert!((shape.height - 3.9).abs() < 1e-6, "height = {}", shape.height);
    }

    /// `max_up_step` is sourced from the *attribute map*, not the census
    /// geometry: raising `STEP_HEIGHT` to 1.0 must change the shape even though
    /// the census (and `MobShape::land`'s 0.6 literal) never move. This is the
    /// silent-disagreement guard from the task.
    #[test]
    fn step_height_comes_from_attributes_not_census() {
        let adapter = CensusStub::with(&[(151, 0.6, 1.95)]);
        let mut attrs = AttributeMap::new();
        attrs.get_or_default(&step_key()).set_base_value(1.0);
        let shape = resolve_mob_shape(&adapter, 151, &attrs).expect("known type");
        assert_eq!(shape.max_up_step, 1.0);
        // Geometry is untouched by a step-height change.
        assert_eq!(shape.height, 1.95);
    }

    /// An unknown network id yields `None` (the census reports "unknown", never a
    /// guessed box) — the caller owns the fallback.
    #[test]
    fn resolve_unknown_type_is_none() {
        let adapter = CensusStub::with(&[(151, 0.6, 1.95)]);
        let attrs = AttributeMap::new();
        assert!(resolve_mob_shape(&adapter, 9999, &attrs).is_none());
    }

    /// The peaceful table, and the assertion that matters is the **discriminating
    /// pair**: a species that is `MobCategory.MONSTER` *and* allowed in peaceful.
    /// Without those seven rows, a table that simply answered "is it a monster"
    /// would pass every other line here.
    #[test]
    fn peaceful_forbids_exactly_the_notinpeaceful_registrations() {
        assert_eq!(
            NOT_ALLOWED_IN_PEACEFUL.len(),
            38,
            "the 26.2 decompile has 38 notInPeaceful registrations"
        );
        // Sorted, so a hand-added name lands where the extraction would have put
        // it and a duplicate is visible.
        let mut sorted = NOT_ALLOWED_IN_PEACEFUL;
        sorted.sort_unstable();
        assert_eq!(sorted, NOT_ALLOWED_IN_PEACEFUL, "the table must stay sorted");

        for forbidden in ["zombie", "slime", "magma_cube", "phantom", "warden", "wither"] {
            assert!(
                !allowed_in_peaceful(forbidden),
                "{forbidden} carries notInPeaceful() and must be forbidden on Peaceful"
            );
        }
        // **The discriminating rows.** All seven are `MobCategory.MONSTER` and none
        // calls `notInPeaceful()`, so vanilla keeps them on Peaceful. A
        // category-derived answer gets every one of these backwards.
        for kept in [
            "piglin",
            "shulker",
            "ender_dragon",
            "zombie_horse",
            "zombie_nautilus",
            "camel_husk",
            "sulfur_cube",
        ] {
            assert!(
                allowed_in_peaceful(kept),
                "{kept} is MobCategory.MONSTER but has no notInPeaceful(), so Peaceful \
                 must keep it — answering from the category is what this row exists to \
                 catch"
            );
        }
        // A passive animal and an unmodelled name both answer `true`, which is the
        // builder's own default.
        assert!(allowed_in_peaceful("sheep"));
        assert!(allowed_in_peaceful("not_a_real_species"));
    }
}
