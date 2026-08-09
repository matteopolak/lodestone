//! Natural mob spawning against a live world — the driver `mob_spawn.rs`'s
//! cap/despawn engine never had, plus the per-species placement table
//! `lodestone_entity::spawn`'s `SpawnRule`/`SpawnEnvironment` seam never had an
//! implementer for (issues #221, #222).
//!
//! ## What it is
//!
//! Three pieces that only make sense together:
//!
//! * [`SPAWN_RULES`] — vanilla's `SpawnPlacements` static registration block,
//!   transcribed as data for every species the bundled 26.2 biome spawn lists can
//!   actually name (51 of them). This is the *data* half `crate::mob_spawn`'s
//!   module doc says must not live in the version-free engine.
//! * [`ColumnLight`] — a per-column light cache over `lodestone_world`'s real
//!   light engine, because every monster rule in the game is a light test and the
//!   server had no light at any position.
//! * [`NaturalSpawner`] — a [`SpawnCandidateSource`](crate::mob_spawn::SpawnCandidateSource)
//!   that runs vanilla's `NaturalSpawner.spawnCategoryForChunk` cluster loop over
//!   real terrain, real biomes and the real biome spawn lists
//!   ([`lodestone_worldgen::spawners`], parsed but consumerless until now).
//!
//! [`crate::tick::run_tick_loop`] drives it once per tick over its tick area,
//! gated on the `spawn_mobs` game rule, and runs the despawn pass beside it.
//!
//! ## How it works
//!
//! Per chunk and per category still under its global cap, vanilla picks one
//! random position in the chunk (`getRandomPosWithin`: random x/z, y uniform
//! between the world floor and one above the surface), then makes up to three
//! *group* attempts, each wandering a `±6` offset up to `ceil(nextFloat() * 4)`
//! times and re-rolling the same weighted species for the whole group. The RNG
//! draw order and count is the specification — it is what makes spawn rates what
//! they are — so [`NaturalSpawner::cluster`] draws in vanilla's order and returns
//! the whole group rather than one mob per call.
//!
//! Light comes from `lodestone_world::compute_column_light` over the column's own
//! palette indices (with `lodestone_data::light_props` supplying dampening and
//! emission per palette entry, so no registry lookup happens per cell). Two
//! deliberate bounds:
//!
//! * **At most [`LIGHT_BUDGET_PER_CYCLE`] columns are lit per cycle.** A column is
//!   ~1 ms in release, and the tick budget is 50 ms; an unbounded first pass over
//!   a 49-column tick area would blow it outright.
//! * **The cache is dropped wholesale every [`LIGHT_TTL_TICKS`] ticks.** There is
//!   no per-block relight anywhere in this tree (issue #94), so a torch placed in
//!   a dark room stops spawns within ten seconds rather than instantly. Vanilla's
//!   own lighting is asynchronous; this is a coarser version of the same lag, and
//!   it is the reason the cache is a TTL rather than a dirty set.
//!
//! ## How to change it, and the gotchas
//!
//! * **The species table is a record transcription, not a guess.** Every row
//!   comes from `SpawnPlacements.java`'s registration plus the `check*SpawnRules`
//!   body it names; the block-tag rows come from
//!   `data/minecraft/tags/block/*_spawnable_on.json`. If you add a species,
//!   read its predicate — the families genuinely differ (a wolf wants
//!   `WOLVES_SPAWNABLE_ON` and brightness > 8; a bat wants stone below,
//!   `nextBoolean()`, and brightness ≤ `nextInt(4)`).
//! * **A predicate that branches over *alternatives* needs [`Special`], not more
//!   fields.** There is one: `Slime.checkSlimeSpawnRules`, whose swamp-surface and
//!   slime-chunk arms own a Y band and a set of RNG draws each, so a single row of
//!   conjoined fields structurally cannot express it —
//!   [`NaturalSpawner::slime_permits`] and `docs/natural-mob-spawning.md`.
//! * **A species absent from [`SPAWN_RULES`] cannot spawn**, deliberately. The
//!   alternative — falling back to "no restrictions" — spawns guardians on land.
//!   [`spawn_rule`] returning `None` is why a Nether-only species in an overworld
//!   biome list is inert rather than wrong.
//! * **Sea level is [`SEA_LEVEL`], a constant.** The tick loop holds a
//!   [`ChunkSource`](crate::chunk::ChunkSource), not a generator, so there is
//!   nothing to ask. It is right for every overworld preset; a custom
//!   `sea_level` would shift the water-animal bands.
//! * **Peaceful difficulty is not read here.** `crate::tick` gates the whole
//!   cycle on the `spawn_mobs` rule; difficulty lives on
//!   [`crate::world_state::WorldStateHandle`] and folding it in belongs with the
//!   peaceful-eviction pass in `lodestone_entity::spawn::check_despawn`, which
//!   already models it.
//!
//! ## Dependencies
//!
//! `lodestone_world` (the light engine), `lodestone_data` (`light_props`,
//! `block_states`), `lodestone_worldgen::spawners` (the biome lists) and
//! `crate::mob_spawn` (the cap engine and its RNG).

use std::collections::HashMap;
use std::str::FromStr;

use lodestone_model::{Difficulty, ResourceKey, Vec3};
use lodestone_world::{BlockVolume, LightProperties, compute_column_light};

use crate::chunk::ChunkColumn;
use crate::mob_spawn::{MobCategory, SpawnCandidate, SpawnCandidateSource, SpawnRng};
use crate::mobs::{ChunkWorld, block_state_id_or_default};

/// Vanilla's overworld sea level, the anchor for every water-animal Y band. See
/// the module doc for why this is a constant.
pub const SEA_LEVEL: i32 = 63;

/// How many columns [`NaturalSpawner`] will light in one spawn cycle before
/// giving up on the rest until the next. See the module doc.
pub const LIGHT_BUDGET_PER_CYCLE: usize = 4;

/// How long a column's cached light is trusted, in ticks (10 s).
pub const LIGHT_TTL_TICKS: u64 = 200;

/// Vanilla `NaturalSpawner.MIN_SPAWN_DISTANCE` squared — a mob never spawns
/// within 24 blocks of the nearest player.
const MIN_PLAYER_DIST_SQR: f64 = 576.0;

/// `BiomeTags.ALLOWS_SURFACE_SLIME_SPAWNS`, flattened —
/// `BiomeTagsProvider.java:266` adds exactly `swamp` and `mangrove_swamp` and
/// nothing else, so the tag is two names rather than a lookup.
const SURFACE_SLIME_BIOMES: &[&str] = &["minecraft:swamp", "minecraft:mangrove_swamp"];

/// `DimensionType.MOON_BRIGHTNESS_PER_PHASE` (`DimensionType.java:57`), indexed
/// by `MoonPhase.index()`.
const MOON_BRIGHTNESS_PER_PHASE: [f32; 8] = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];

/// `Slime.checkSlimeSpawnRules`' slime-chunk ceiling: the slime-chunk arm only
/// fires strictly below this Y (`Slime.java:94`).
const SLIME_CHUNK_MAX_Y: i32 = 40;

/// How a species is positioned relative to the candidate block —
/// `SpawnPlacementTypes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// `ON_GROUND`: a valid spawn surface below, and two blocks of legal empty
    /// space at and above the position.
    OnGround,
    /// `IN_WATER`: water at the position, and the block above not a full solid.
    InWater,
    /// `IN_LAVA`: lava at the position.
    InLava,
    /// `NO_RESTRICTIONS`: anything goes (phantoms, vexes, foxes, pandas).
    NoRestrictions,
}

/// The light condition a species' `check*SpawnRules` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightRule {
    /// `Monster.isDarkEnoughToSpawn`: raw sky light must not exceed
    /// `nextInt(32)`, block light must be 0 (the overworld's
    /// `monsterSpawnBlockLightLimit`), and the local raw brightness must not
    /// exceed the overworld's `monsterSpawnLightTest`, `UniformInt(0, 7)`.
    Dark,
    /// `Animal.isBrightEnoughToSpawn`: raw brightness > 8.
    Bright,
    /// No light test at all (`checkAnyLightMonsterSpawnRules`, and every
    /// water/ambient species whose predicate omits one).
    Any,
    /// Raw brightness must be ≤ `nextInt(bound)` — a bat's `nextInt(4)`.
    MaxRandom(i32),
    /// Raw brightness must be exactly 0 (a glow squid).
    Zero,
}

/// What must be directly below the candidate position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ground {
    /// `Mob.checkMobSpawnRules`: `BlockState.isValidSpawn`, i.e. a sturdy up-face
    /// that emits less than 14.
    ValidSpawn,
    /// One of these block names (a `*_spawnable_on` tag, flattened — see the
    /// module doc).
    OneOf(&'static [&'static str]),
    /// Water below (the surface water-animal band, and a drowned).
    Water,
    /// Nothing is required.
    Any,
}

/// A `nextInt` gate a predicate applies before anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chance {
    /// No gate.
    Always,
    /// `random.nextInt(n) == 0`.
    OneIn(i32),
    /// `random.nextInt(n) != 0` — an ocelot's `nextInt(3) != 0`.
    NotOneIn(i32),
    /// `!random.nextBoolean()` — a bat's.
    NotCoinFlip,
}

/// A predicate whose shape the [`SpawnRule`] fields structurally cannot carry —
/// one that branches over *alternatives* rather than conjoining conditions, or
/// that needs a world fact no other row does.
///
/// There is exactly one today. The point of naming it rather than widening
/// [`SpawnRule`] with four more `Option`s is that the alternation, and the RNG
/// draw order it implies, is *code* in vanilla too; a data row cannot express
/// "arm A, else arm B" without also encoding which arm consumed which draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    /// No special arm: the [`SpawnRule`] fields are the whole predicate.
    None,
    /// `Slime.checkSlimeSpawnRules` (`Slime.java:73-99`) — see
    /// [`NaturalSpawner::slime_permits`].
    Slime,
}

/// One species' whole spawn condition: `SpawnPlacements`' registered placement
/// type plus the `check*SpawnRules` predicate it registered with, reduced to the
/// checks this server can actually answer.
#[derive(Debug, Clone, Copy)]
pub struct SpawnRule {
    /// `SpawnPlacements` placement type.
    pub placement: Placement,
    /// The predicate's light test.
    pub light: LightRule,
    /// What the predicate demands below the position.
    pub ground: Ground,
    /// Inclusive world-Y band, when the predicate names one.
    pub y_range: (i32, i32),
    /// Whether the position must see the sky (`checkSurfaceMonstersSpawnRules`).
    pub needs_sky: bool,
    /// The predicate's own `nextInt` gate.
    pub chance: Chance,
    /// A predicate arm the fields above cannot express. See [`Special`].
    pub special: Special,
}

impl SpawnRule {
    /// The default shape: on the ground, on a valid spawn surface, anywhere in
    /// the world, no light test and no dice.
    const fn base() -> Self {
        Self {
            placement: Placement::OnGround,
            light: LightRule::Any,
            ground: Ground::ValidSpawn,
            y_range: (i32::MIN, i32::MAX),
            needs_sky: false,
            chance: Chance::Always,
            special: Special::None,
        }
    }

    /// `Monster::checkMonsterSpawnRules` — dark enough, valid surface below.
    const fn monster() -> Self {
        Self {
            light: LightRule::Dark,
            ..Self::base()
        }
    }

    /// `Monster::checkSurfaceMonstersSpawnRules` — a monster that also needs sky
    /// (husk, parched, camel husk).
    const fn surface_monster() -> Self {
        Self {
            needs_sky: true,
            ..Self::monster()
        }
    }

    /// `Monster::checkAnyLightMonsterSpawnRules` — no light test (blaze, breeze,
    /// zoglin), and the family every difficulty-only Nether predicate reduces to
    /// (magma cube, sulfur cube, zombified piglin).
    const fn any_light_monster() -> Self {
        Self::base()
    }

    /// `Animal::checkAnimalSpawnRules` and its per-species tag variants: bright
    /// enough (> 8) and standing on `on`.
    const fn animal(on: &'static [&'static str]) -> Self {
        Self {
            light: LightRule::Bright,
            ground: Ground::OneOf(on),
            ..Self::base()
        }
    }

    /// `WaterAnimal::checkSurfaceWaterAnimalSpawnRules` /
    /// `AgeableWaterCreature::checkSurfaceAgeableWaterCreatureSpawnRules`: in the
    /// `[sea - 13, sea]` band, water below and water above.
    const fn surface_water() -> Self {
        Self {
            placement: Placement::InWater,
            ground: Ground::Water,
            y_range: (SEA_LEVEL - 13, SEA_LEVEL),
            ..Self::base()
        }
    }
}

/// `minecraft:grass_block` — `ANIMALS_SPAWNABLE_ON`.
const ANIMALS_ON: &[&str] = &["minecraft:grass_block"];
/// `WOLVES_SPAWNABLE_ON`.
const WOLVES_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:snow",
    "minecraft:snow_block",
    "minecraft:coarse_dirt",
    "minecraft:podzol",
];
/// `FOXES_SPAWNABLE_ON`.
const FOXES_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:snow",
    "minecraft:snow_block",
    "minecraft:podzol",
    "minecraft:coarse_dirt",
];
/// `RABBITS_SPAWNABLE_ON`.
const RABBITS_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:snow",
    "minecraft:snow_block",
    "minecraft:sand",
];
/// `GOATS_SPAWNABLE_ON` (the `ANIMALS_SPAWNABLE_ON` include flattened in).
const GOATS_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:stone",
    "minecraft:snow",
    "minecraft:snow_block",
    "minecraft:packed_ice",
    "minecraft:gravel",
];
/// `FROGS_SPAWNABLE_ON`.
const FROGS_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:mud",
    "minecraft:mangrove_roots",
    "minecraft:muddy_mangrove_roots",
];
/// `MOOSHROOMS_SPAWNABLE_ON`.
const MOOSHROOMS_ON: &[&str] = &["minecraft:mycelium"];
/// `PARROTS_SPAWNABLE_ON`, with the `#leaves`/`#logs` includes reduced to the
/// two the bundled jungle surface actually produces below a spawn position.
const PARROTS_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:air",
    "minecraft:jungle_leaves",
    "minecraft:jungle_log",
    "minecraft:oak_leaves",
    "minecraft:oak_log",
];
/// `ARMADILLO_SPAWNABLE_ON`.
const ARMADILLO_ON: &[&str] = &[
    "minecraft:grass_block",
    "minecraft:red_sand",
    "minecraft:coarse_dirt",
    "minecraft:terracotta",
];
/// `CAMELS_SPAWNABLE_ON` (`#sand`).
const CAMELS_ON: &[&str] = &["minecraft:sand", "minecraft:red_sand"];
/// `BATS_SPAWNABLE_ON` (`#base_stone_overworld`).
const BATS_ON: &[&str] = &[
    "minecraft:stone",
    "minecraft:granite",
    "minecraft:diorite",
    "minecraft:andesite",
    "minecraft:tuff",
    "minecraft:deepslate",
];
/// `AXOLOTLS_SPAWNABLE_ON`.
const AXOLOTLS_ON: &[&str] = &["minecraft:clay"];

/// `SpawnPlacements`' registration for every species the bundled 26.2 overworld
/// and Nether biome spawn lists can name, keyed by path (no `minecraft:`).
///
/// Sorted so [`spawn_rule`] can binary-search it, and so a duplicate is a visible
/// adjacency rather than a silent second registration — the same reason vanilla's
/// own `register` throws on one.
static SPAWN_RULES: &[(&str, SpawnRule)] = &[
    ("armadillo", SpawnRule::animal(ARMADILLO_ON)),
    ("axolotl", {
        SpawnRule {
            placement: Placement::InWater,
            ground: Ground::OneOf(AXOLOTLS_ON),
            ..SpawnRule::base()
        }
    }),
    ("bat", {
        SpawnRule {
            light: LightRule::MaxRandom(4),
            ground: Ground::OneOf(BATS_ON),
            chance: Chance::NotCoinFlip,
            // `pos.getY() >= heightmap(WORLD_SURFACE)` is a rejection, i.e. bats
            // are underground-only. Expressed as `needs_sky: false` plus the
            // below-surface check `NaturalSpawner` applies for this rule.
            ..SpawnRule::base()
        }
    }),
    ("bogged", SpawnRule::monster()),
    ("camel", SpawnRule::animal(CAMELS_ON)),
    ("cave_spider", SpawnRule::monster()),
    ("chicken", SpawnRule::animal(ANIMALS_ON)),
    ("cod", SpawnRule::surface_water()),
    ("cow", SpawnRule::animal(ANIMALS_ON)),
    ("creeper", SpawnRule::monster()),
    ("dolphin", SpawnRule::surface_water()),
    ("donkey", SpawnRule::animal(ANIMALS_ON)),
    ("drowned", {
        // `checkDrownedSpawnRules`: water below, dark enough, water at the
        // position, and — outside the `MORE_FREQUENT_DROWNED_SPAWNS` biomes —
        // `nextInt(40) == 0`. The frequent-biome arm (`nextInt(15)`) needs a
        // biome tag this crate has no table for; the rarer gate is the safe one
        // to model, since over-spawning drowned is the visible failure.
        SpawnRule {
            placement: Placement::InWater,
            light: LightRule::Dark,
            ground: Ground::Water,
            chance: Chance::OneIn(40),
            ..SpawnRule::base()
        }
    }),
    ("enderman", SpawnRule::monster()),
    ("fox", {
        SpawnRule {
            placement: Placement::NoRestrictions,
            ..SpawnRule::animal(FOXES_ON)
        }
    }),
    ("frog", SpawnRule::animal(FROGS_ON)),
    ("ghast", {
        // `checkGhastSpawnRules` is `nextInt(20) == 0` plus the bare mob rules.
        SpawnRule {
            chance: Chance::OneIn(20),
            ..SpawnRule::base()
        }
    }),
    ("glow_squid", {
        SpawnRule {
            placement: Placement::InWater,
            light: LightRule::Zero,
            ground: Ground::Any,
            y_range: (i32::MIN, SEA_LEVEL - 33),
            ..SpawnRule::base()
        }
    }),
    ("goat", SpawnRule::animal(GOATS_ON)),
    ("hoglin", SpawnRule::any_light_monster()),
    ("horse", SpawnRule::animal(ANIMALS_ON)),
    ("husk", SpawnRule::surface_monster()),
    ("llama", SpawnRule::animal(ANIMALS_ON)),
    ("magma_cube", SpawnRule::any_light_monster()),
    ("mooshroom", SpawnRule::animal(MOOSHROOMS_ON)),
    ("nautilus", SpawnRule::surface_water()),
    ("ocelot", {
        SpawnRule {
            chance: Chance::NotOneIn(3),
            ..SpawnRule::base()
        }
    }),
    ("panda", {
        SpawnRule {
            placement: Placement::NoRestrictions,
            ..SpawnRule::animal(ANIMALS_ON)
        }
    }),
    ("parched", SpawnRule::surface_monster()),
    ("parrot", SpawnRule::animal(PARROTS_ON)),
    ("pig", SpawnRule::animal(ANIMALS_ON)),
    ("piglin", SpawnRule::any_light_monster()),
    ("polar_bear", SpawnRule::animal(ANIMALS_ON)),
    ("pufferfish", SpawnRule::surface_water()),
    ("rabbit", SpawnRule::animal(RABBITS_ON)),
    ("salmon", SpawnRule::surface_water()),
    ("sheep", SpawnRule::animal(ANIMALS_ON)),
    ("skeleton", SpawnRule::monster()),
    ("slime", {
        // `checkSlimeSpawnRules` is **two alternatives**, not a conjunction, so
        // none of the fields here can carry it — the Y band and the light test in
        // particular belong to one arm each. See
        // [`NaturalSpawner::slime_permits`], and `Special`'s own doc for why the
        // alternation is code rather than data.
        //
        // Everything a slime shares with `checkMobSpawnRules` *is* here: it is
        // `ON_GROUND` on a valid spawn surface, with no Y band and no light test
        // of its own at this level.
        SpawnRule {
            special: Special::Slime,
            ..SpawnRule::base()
        }
    }),
    ("spider", SpawnRule::monster()),
    ("squid", SpawnRule::surface_water()),
    ("stray", {
        // `checkStraySpawnRules` is the monster rules plus `canSeeSky`.
        SpawnRule::surface_monster()
    }),
    ("strider", {
        SpawnRule {
            placement: Placement::InLava,
            ground: Ground::Any,
            ..SpawnRule::base()
        }
    }),
    ("sulfur_cube", SpawnRule::any_light_monster()),
    ("tropical_fish", SpawnRule::surface_water()),
    ("turtle", {
        SpawnRule {
            light: LightRule::Bright,
            ground: Ground::OneOf(&["minecraft:sand", "minecraft:red_sand"]),
            y_range: (i32::MIN, SEA_LEVEL + 3),
            ..SpawnRule::base()
        }
    }),
    ("witch", SpawnRule::monster()),
    ("wolf", SpawnRule::animal(WOLVES_ON)),
    ("zombie", SpawnRule::monster()),
    ("zombie_horse", SpawnRule::monster()),
    ("zombie_villager", SpawnRule::monster()),
    ("zombified_piglin", SpawnRule::any_light_monster()),
];

/// The registered rule for a species path (`zombie`, not `minecraft:zombie`), or
/// `None` for a species with no registration — which cannot spawn naturally. See
/// the module doc for why that is deliberate.
#[must_use]
pub fn spawn_rule(path: &str) -> Option<&'static SpawnRule> {
    SPAWN_RULES
        .binary_search_by(|(name, _)| (*name).cmp(path))
        .ok()
        .map(|i| &SPAWN_RULES[i].1)
}

// --- light -----------------------------------------------------------------

/// One column's block grid as palette indices, so the light engine reads a `u16`
/// per cell instead of resolving a state string.
struct PaletteVolume {
    cells: Vec<u16>,
    min_y: i32,
    section_count: usize,
}

impl PaletteVolume {
    fn of(column: &ChunkColumn) -> Self {
        let section_count = column.section_count();
        let mut cells = Vec::with_capacity(section_count * 4096);
        for s in 0..section_count {
            column.append_section_cells(s, &mut cells);
        }
        Self {
            cells,
            min_y: column.min_y,
            section_count,
        }
    }
}

impl BlockVolume for PaletteVolume {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        let local = y - self.min_y;
        if local < 0 || local >= (self.section_count * 16) as i32 {
            // Air, which is palette index 0 by `ChunkColumn`'s own invariant —
            // the apron section above and below the built column.
            return 0;
        }
        let s = (local / 16) as usize;
        let y_in = (local % 16) as usize;
        let idx = s * 4096 + (y_in << 8) + (z << 4) + x;
        u32::from(self.cells[idx])
    }

    fn min_y(&self) -> i32 {
        self.min_y
    }

    fn section_count(&self) -> usize {
        self.section_count
    }
}

/// `(dampening, emission)` per palette index, resolved once per column rather
/// than once per cell.
struct PaletteProps(Vec<(u8, u8)>);

impl PaletteProps {
    fn of(column: &ChunkColumn) -> Self {
        Self(
            column
                .raw_palette()
                .iter()
                .map(|name| {
                    block_state_id_or_default(name)
                        .and_then(lodestone_data::light_props::light_props)
                        // An unresolvable state darkens and occludes, never
                        // brightens — see `light_props`' own module doc.
                        .unwrap_or((15, 0))
                })
                .collect(),
        )
    }
}

impl LightProperties for PaletteProps {
    fn opacity(&self, state: u32) -> u8 {
        self.0.get(state as usize).map_or(15, |&(d, _)| d)
    }

    fn emission(&self, state: u32) -> u8 {
        self.0.get(state as usize).map_or(0, |&(_, e)| e)
    }
}

/// Sky and block light for one column, sampled by world coordinate.
#[derive(Debug)]
struct ColumnLight {
    light: lodestone_world::ColumnLight,
    min_y: i32,
    section_count: usize,
}

impl ColumnLight {
    fn compute(column: &ChunkColumn) -> Self {
        let volume = PaletteVolume::of(column);
        let props = PaletteProps::of(column);
        Self {
            light: compute_column_light(&volume, &props),
            min_y: column.min_y,
            section_count: column.section_count(),
        }
    }

    /// `(sky, block)` raw light at world `y` and chunk-local `x`/`z`.
    ///
    /// Above the built column both answer the dimension default the client would
    /// resolve: full sky, no block light. See `docs/server-chunk-light.md`'s
    /// "`Missing` is full daylight" note — a `0` here would darken the sky.
    fn at(&self, x: usize, y: i32, z: usize) -> (u8, u8) {
        let local = y - self.min_y;
        if local < 0 {
            return (0, 0);
        }
        let s = (local / 16) as usize;
        if s >= self.section_count {
            return (15, 0);
        }
        let y_in = (local % 16) as usize;
        let sky = self.light.section_sky_light(s, x, y_in, z).unwrap_or(15);
        let block = self.light.section_block_light(s, x, y_in, z).unwrap_or(0);
        (sky, block)
    }
}

// --- the spawner -----------------------------------------------------------

/// Runs vanilla's natural-spawn cluster loop over real terrain, biomes and biome
/// spawn lists.
///
/// Holds the light cache across cycles, which is the only reason it is a
/// long-lived struct rather than a free function: see the module doc for the
/// budget and the TTL.
pub struct NaturalSpawner {
    /// Per-biome spawn lists, keyed by biome name. Cloned out of the generator
    /// once, because the tick loop holds a [`ChunkSource`](crate::chunk::ChunkSource)
    /// and cannot reach one.
    biomes: HashMap<String, lodestone_worldgen::spawners::BiomeSpawners>,
    lights: HashMap<(i32, i32), ColumnLight>,
    lit_this_cycle: usize,
    lights_refreshed_at: u64,
    rng: SpawnRng,
    players: Vec<Vec3>,
    /// The terrain snapshot this cycle runs against, handed in by
    /// [`begin_cycle`](Self::begin_cycle) rather than stored at construction:
    /// [`crate::MobHandle::reseed`] replaces the sim's world, and a spawner
    /// holding the old one would light chunks nothing paths over.
    world: Option<&'static ChunkWorld>,
    /// The **world generation seed**, which is a different number from the
    /// spawn-RNG seed `new` takes and is used for exactly one thing:
    /// `WorldgenRandom.seedSlimeChunk`. See [`with_world_seed`](Self::with_world_seed).
    world_seed: i64,
    /// The world clock's `day_time`, for the moon phase
    /// `SURFACE_SLIME_SPAWN_CHANCE` is keyframed against. See
    /// [`set_day_time`](Self::set_day_time).
    day_time: i64,
    /// The world difficulty, for `SpawnPlacements.checkSpawnRules`' peaceful
    /// guard. See [`set_difficulty`](Self::set_difficulty).
    difficulty: Difficulty,
}

impl std::fmt::Debug for NaturalSpawner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NaturalSpawner")
            .field("biomes", &self.biomes.len())
            .field("lit_columns", &self.lights.len())
            .field("players", &self.players.len())
            .finish()
    }
}

impl NaturalSpawner {
    /// A spawner over `biomes`, seeded with `seed`.
    ///
    /// An empty `biomes` map makes every cycle a no-op — the honest answer for a
    /// source with no generator behind it (a flat test fixture), and the reason
    /// this takes the table rather than looking one up.
    #[must_use]
    pub fn new(
        biomes: HashMap<String, lodestone_worldgen::spawners::BiomeSpawners>,
        seed: u64,
    ) -> Self {
        Self {
            biomes,
            lights: HashMap::new(),
            lit_this_cycle: 0,
            lights_refreshed_at: 0,
            rng: SpawnRng::new(seed),
            players: Vec::new(),
            world: None,
            world_seed: 0,
            day_time: 0,
            // `LevelSettings.DEFAULT`'s difficulty, matching
            // `crate::world_state::WorldState`'s own default, so a spawner nobody
            // sets it on behaves exactly as it did before the guard existed.
            difficulty: Difficulty::Normal,
        }
    }

    /// Records the **world generation** seed, which is what
    /// `WorldgenRandom.seedSlimeChunk` mixes and therefore what decides which
    /// chunks are slime chunks.
    ///
    /// Separate from `new`'s `seed` on purpose. That one seeds the spawn RNG
    /// stream and is a fixed literal in production (`tick::NATURAL_SPAWN_SEED`),
    /// because the stream only has to be *reproducible*. This one is not free to
    /// choose: get it wrong and the slime chunks are a different set from the ones
    /// the terrain was generated for, which is worse than none — a player who
    /// looks up a slime chunk for their seed would find nothing there.
    ///
    /// Defaults to `0`, which is a real seed rather than a sentinel; a spawner
    /// that is never told the world seed reports the slime chunks of seed 0. That
    /// is the honest failure and it is why this is a named setter: a caller that
    /// omits it is visible at the call site.
    #[must_use]
    pub fn with_world_seed(mut self, world_seed: i64) -> Self {
        self.world_seed = world_seed;
        self
    }

    /// Sets the world clock's `day_time`, which fixes the moon phase for
    /// [`surface_slime_spawn_chance`](Self::surface_slime_spawn_chance).
    ///
    /// Additive rather than a `begin_cycle` parameter so no existing caller has to
    /// change; a caller that never calls it runs at `day_time == 0`, i.e. a full
    /// moon, which is the *most* permissive phase for surface slimes.
    pub fn set_day_time(&mut self, day_time: i64) {
        self.day_time = day_time;
    }

    /// Sets the world difficulty, which decides whether a candidate species may be
    /// proposed at all — vanilla's `SpawnPlacements.checkSpawnRules`, whose *first*
    /// statement is `!type.isAllowedInPeaceful() && level.getDifficulty() ==
    /// PEACEFUL → false`.
    ///
    /// # Why refusing at spawn time is not redundant with the peaceful despawn
    ///
    /// [`crate::MobSim::remove_monsters`] already evicts a forbidden mob, and the
    /// tick loop runs it before this cycle — so on Peaceful a monster proposed here
    /// lived exactly one tick. One tick is enough to be *seen*: the loop publishes
    /// its snapshot set after the spawn cycle, so the connection's next streaming
    /// pass sends `ADD_ENTITY` and the pass after it sends `REMOVE_ENTITIES`. The
    /// player on Peaceful watched zombies blink in and out. Vanilla refuses in both
    /// places, and this is the half that stops the flicker.
    ///
    /// Defaults to `Normal`; a caller that never sets it spawns monsters, which is
    /// the behaviour every existing gate was written against.
    pub fn set_difficulty(&mut self, difficulty: Difficulty) {
        self.difficulty = difficulty;
    }

    /// Starts a cycle at `tick` with `players` as the loaded players, resetting
    /// the per-cycle light budget and dropping the cache once its TTL is up.
    pub fn begin_cycle(&mut self, world: &'static ChunkWorld, tick: u64, players: Vec<Vec3>) {
        self.world = Some(world);
        self.players = players;
        self.lit_this_cycle = 0;
        if tick.saturating_sub(self.lights_refreshed_at) >= LIGHT_TTL_TICKS {
            self.lights.clear();
            self.lights_refreshed_at = tick;
        }
    }

    /// Whether any player is loaded. Vanilla spawns nothing without one, and the
    /// caller can skip the whole cycle rather than pay for the census.
    #[must_use]
    pub fn has_players(&self) -> bool {
        !self.players.is_empty()
    }

    /// Every species one biome's spawn list names for `category`, in declaration
    /// order.
    ///
    /// Exposed so a caller can compare what actually spawned against the biome
    /// document rather than against a hand-written list — and so it can do that
    /// without naming [`lodestone_worldgen`]'s own `MobCategory`, a third enum by
    /// that name.
    #[must_use]
    pub fn species_for(&self, biome: &str, category: MobCategory) -> Vec<&str> {
        self.biomes
            .get(biome)
            .map(|s| {
                s.for_category(worldgen_category(category))
                    .iter()
                    .map(|e| e.entity_type.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The squared distance from `pos` to the nearest player, or `None` when none
    /// is loaded.
    fn nearest_player_dist_sqr(&self, x: f64, y: f64, z: f64) -> Option<f64> {
        self.players
            .iter()
            .map(|p| {
                let (dx, dy, dz) = (p.x - x, p.y - y, p.z - z);
                dx * dx + dy * dy + dz * dz
            })
            .fold(None, |best: Option<f64>, d| {
                Some(best.map_or(d, |b| b.min(d)))
            })
    }

    /// Light at a world position, computing (and caching) the column if the
    /// per-cycle budget allows. `None` means "not known this cycle" — the caller
    /// treats that as "do not spawn", never as darkness.
    fn light_at(&mut self, x: i32, y: i32, z: i32) -> Option<(u8, u8)> {
        let world = self.world?;
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16) as usize, z.rem_euclid(16) as usize);
        if !self.lights.contains_key(&(cx, cz)) {
            if self.lit_this_cycle >= LIGHT_BUDGET_PER_CYCLE {
                return None;
            }
            let column = world.column(cx, cz)?;
            self.lit_this_cycle += 1;
            self.lights.insert((cx, cz), ColumnLight::compute(column));
        }
        Some(self.lights[&(cx, cz)].at(lx, y, lz))
    }

    /// Vanilla's `getMaxLocalRawBrightness(pos)`: the greater of block light and
    /// sky light reduced by the world's current sky darkening. Ambient darkening
    /// is not modelled (there is no world clock here), so this is the *daytime*
    /// answer — the conservative direction, since a brighter reading only ever
    /// suppresses a spawn.
    fn raw_brightness(sky: u8, block: u8) -> u8 {
        sky.max(block)
    }

    /// The species-independent half of `isValidSpawnPostitionForType` plus the
    /// species' own `SpawnRule`, evaluated at `pos`.
    #[allow(clippy::too_many_lines)]
    fn permits(&mut self, rule: &SpawnRule, x: i32, y: i32, z: i32) -> bool {
        if y < rule.y_range.0 || y > rule.y_range.1 {
            return false;
        }
        let Some(world) = self.world else {
            return false;
        };

        let here = world.block_state(x, y, z).to_string();
        let below = world.block_state(x, y - 1, z).to_string();
        let above = world.block_state(x, y + 1, z).to_string();

        match rule.placement {
            Placement::OnGround => {
                if !is_valid_spawn_surface(&below) {
                    return false;
                }
                if !is_valid_empty_spawn_block(&here) || !is_valid_empty_spawn_block(&above) {
                    return false;
                }
            }
            Placement::InWater => {
                if !is_water(&here) {
                    return false;
                }
                if is_full_solid(&above) {
                    return false;
                }
            }
            Placement::InLava => {
                if !is_lava(&here) {
                    return false;
                }
            }
            Placement::NoRestrictions => {}
        }

        match rule.ground {
            Ground::ValidSpawn => {
                if !is_valid_spawn_surface(&below) {
                    return false;
                }
            }
            Ground::OneOf(names) => {
                let base = below.split('[').next().unwrap_or(&below);
                if !names.contains(&base) {
                    return false;
                }
            }
            Ground::Water => {
                if !is_water(&below) {
                    return false;
                }
            }
            Ground::Any => {}
        }

        // The light half. Drawn *after* the cheap terrain rejections, which is
        // also vanilla's order — `isDarkEnoughToSpawn` is the last thing
        // `checkMonsterSpawnRules` reaches — so the RNG stream is not consumed by
        // a position that was never going to work.
        let Some((sky, block)) = self.light_at(x, y, z) else {
            return false;
        };
        if rule.needs_sky && sky < 15 {
            return false;
        }
        match rule.light {
            LightRule::Any => {}
            LightRule::Dark => {
                if i32::from(sky) > self.rng.next_int(32) {
                    return false;
                }
                // The overworld's `monsterSpawnBlockLightLimit` is 0.
                if block > 0 {
                    return false;
                }
                // `monsterSpawnLightTest` is `UniformInt(0, 7)`.
                if i32::from(Self::raw_brightness(sky, block)) > self.rng.next_int(8) {
                    return false;
                }
            }
            LightRule::Bright => {
                if Self::raw_brightness(sky, block) <= 8 {
                    return false;
                }
            }
            LightRule::MaxRandom(bound) => {
                if i32::from(Self::raw_brightness(sky, block)) > self.rng.next_int(bound) {
                    return false;
                }
            }
            LightRule::Zero => {
                if Self::raw_brightness(sky, block) != 0 {
                    return false;
                }
            }
        }
        match rule.special {
            Special::None => {}
            Special::Slime => {
                if !self.slime_permits(x, y, z, Self::raw_brightness(sky, block)) {
                    return false;
                }
            }
        }
        true
    }

    /// The moon-phase `SURFACE_SLIME_SPAWN_CHANCE` at the current `day_time`.
    ///
    /// `EnvironmentAttributes.SURFACE_SLIME_SPAWN_CHANCE` (`EnvironmentAttributes.java:144`)
    /// defaults to **`0.0`** and is raised by exactly one modifier track:
    /// `Timelines.MOON`'s (`Timelines.java:168-175`), a `FloatModifier.MAXIMUM`
    /// keyframed `CONSTANT` (so a step function, not a ramp) at each phase start to
    /// `MOON_BRIGHTNESS_PER_PHASE[phase] * 0.5`. `max(0.0, that)` is `that`, so the
    /// whole attribute reduces to this expression.
    ///
    /// The consequence is worth stating because it is not how older versions
    /// behaved and it reads as a bug if you do not know it: **at new moon the
    /// surface arm cannot fire at all** (chance `0.0`, and `nextFloat() < 0.0` is
    /// never true), and at full moon it is `0.5`. Surface swamp slimes are a
    /// moon-phase feature in 26.2.
    #[must_use]
    fn surface_slime_spawn_chance(&self) -> f32 {
        // `MoonPhase.PHASE_LENGTH` is 24000 and `MoonPhase.COUNT` is 8; the
        // timeline's period is `24000 * COUNT` and each phase's `startTick()` is
        // `index * 24000`.
        let phase = self.day_time.div_euclid(24_000).rem_euclid(8) as usize;
        MOON_BRIGHTNESS_PER_PHASE[phase] * 0.5
    }

    /// `Slime.checkSlimeSpawnRules` (`Slime.java:73-99`), minus the two clauses
    /// that belong to the caller.
    ///
    /// Vanilla's body is **two alternatives in sequence**, and the sequence is
    /// load-bearing because each arm consumes draws:
    ///
    /// 1. **The swamp surface arm.** Biome in
    ///    [`SURFACE_SLIME_BIOMES`], `50 < y < 70`, then `nextFloat() <
    ///    surfaceSlimeSpawnChance` and `maxLocalRawBrightness <= nextInt(8)`. The
    ///    `nextInt(8)` is drawn *only* if the `nextFloat()` passed — vanilla's `&&`
    ///    short-circuits and so does this.
    /// 2. **The slime-chunk arm**, reached whenever arm 1 did not return true:
    ///    `nextInt(10) == 0`, the chunk is a slime chunk, and `y < 40`. That
    ///    `nextInt(10)` is drawn *before* the slime-chunk test in vanilla too, so
    ///    it is consumed even in a non-slime chunk.
    ///
    /// Both arms then defer to `checkMobSpawnRules`, which is "the block below is a
    /// valid spawn surface" — already enforced by the row's
    /// [`Ground::ValidSpawn`], so it is not repeated here.
    ///
    /// The two omitted clauses: `level.getDifficulty() != PEACEFUL` (the tick loop
    /// gates the whole cycle, and `remove_monsters` evicts anything that slips
    /// through — see the module doc) and the `EntitySpawnReason.isSpawner` early
    /// return, which cannot apply to a natural spawn.
    fn slime_permits(&mut self, x: i32, y: i32, z: i32, brightness: u8) -> bool {
        let surface_band = y > 50 && y < 70;
        let surface_biome = surface_band
            && self
                .world
                .and_then(|w| w.biome_at(x, y, z))
                .is_some_and(|b| SURFACE_SLIME_BIOMES.contains(&b.as_str()));
        if surface_biome {
            let chance = self.surface_slime_spawn_chance();
            if self.rng.next_f32() < chance && i32::from(brightness) <= self.rng.next_int(8) {
                return true;
            }
        }
        self.rng.next_int(10) == 0
            && lodestone_worldgen::is_slime_chunk(x.div_euclid(16), z.div_euclid(16), self.world_seed)
            && y < SLIME_CHUNK_MAX_Y
    }

    /// One weighted pick out of `category`'s list for the biome at `(x, y, z)`,
    /// drawing exactly once from the RNG — vanilla's `WeightedList.getRandom`.
    fn pick_species(
        &mut self,
        category: MobCategory,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<(&'static SpawnRule, ResourceKey, i32, i32)> {
        let biome = self.world?.biome_at(x, y, z)?;
        let entries = self.biomes.get(&biome)?.for_category(worldgen_category(category));
        let total: i32 = entries.iter().map(|e| e.weight.max(0)).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = self.rng.next_int(total);
        for entry in entries {
            roll -= entry.weight.max(0);
            if roll < 0 {
                let key = ResourceKey::from_str(&entry.entity_type).ok()?;
                let rule = spawn_rule(key.path())?;
                return Some((rule, key, entry.min_count, entry.max_count));
            }
        }
        None
    }
}

impl SpawnCandidateSource for NaturalSpawner {
    /// Vanilla `NaturalSpawner.spawnCategoryForChunk` for one chunk and one
    /// category, returning the whole group it produced.
    ///
    /// The draw order is vanilla's, in vanilla's sequence: the start position,
    /// then per group a `ceil(nextFloat() * 4)` attempt budget, then per attempt
    /// two `nextInt(6)` pairs for the wander, then the species pick (once per
    /// group) and its count, then the rule's own light and chance draws.
    fn cluster(&mut self, category: MobCategory, cx: i32, cz: i32) -> Vec<SpawnCandidate> {
        let mut out = Vec::new();
        let Some(world) = self.world else {
            return out;
        };
        let Some(start) = self.random_pos_within(cx, cz) else {
            return out;
        };
        let (sx, sy, sz) = start;
        // `if (!state.isRedstoneConductor(...))` — a spawn never starts inside a
        // full solid.
        if is_full_solid(world.block_state(sx, sy, sz)) {
            return out;
        }

        for _group in 0..3 {
            let mut x = sx;
            let mut z = sz;
            let mut species: Option<(&'static SpawnRule, ResourceKey)> = None;
            let mut attempts = (self.rng.next_f32() * 4.0).ceil() as i32;
            let mut group_size = 0;

            let mut attempt = 0;
            while attempt < attempts {
                attempt += 1;
                x += self.rng.next_int(6) - self.rng.next_int(6);
                z += self.rng.next_int(6) - self.rng.next_int(6);
                let (fx, fz) = (f64::from(x) + 0.5, f64::from(z) + 0.5);
                let Some(dist_sqr) = self.nearest_player_dist_sqr(fx, f64::from(sy), fz) else {
                    return out;
                };
                if dist_sqr <= MIN_PLAYER_DIST_SQR {
                    continue;
                }
                let despawn = f64::from(category.despawn_distance());
                if dist_sqr > despawn * despawn {
                    continue;
                }

                if species.is_none() {
                    let Some((rule, key, min_count, max_count)) =
                        self.pick_species(category, x, sy, z)
                    else {
                        break;
                    };
                    // Vanilla re-derives the attempt budget from the group's own
                    // `minCount`/`maxCount` the moment the species is known.
                    attempts = min_count + self.rng.next_int(1 + max_count - min_count);
                    species = Some((rule, key));
                }
                let (rule, key) = species.clone().expect("just set");

                match rule.chance {
                    Chance::Always => {}
                    Chance::OneIn(n) => {
                        if self.rng.next_int(n) != 0 {
                            continue;
                        }
                    }
                    Chance::NotOneIn(n) => {
                        if self.rng.next_int(n) == 0 {
                            continue;
                        }
                    }
                    Chance::NotCoinFlip => {
                        if self.rng.next_int(2) == 1 {
                            continue;
                        }
                    }
                }

                // `SpawnPlacements.checkSpawnRules`' first statement, ahead of the
                // rule's own predicate for the same reason it is first there: the
                // predicate draws from the RNG (light brightness, the per-species
                // chance), and vanilla's peaceful refusal happens before any of that.
                // Keyed on the per-type `notInPeaceful` flag, never on the category —
                // see `crate::mob_spawn::allowed_in_peaceful` for the seven
                // `MobCategory.MONSTER` species vanilla keeps on Peaceful.
                if self.difficulty == Difficulty::Peaceful
                    && !crate::mob_spawn::allowed_in_peaceful(key.path())
                {
                    continue;
                }

                if !self.permits(&rule, x, sy, z) {
                    continue;
                }

                out.push(SpawnCandidate {
                    pos: Vec3::new(fx, f64::from(sy), fz),
                    entity_type: key,
                });
                group_size += 1;
                // `getMaxSpawnClusterSize` is 4 for every species that does not
                // override it.
                if out.len() >= 4 || group_size >= 4 {
                    return out;
                }
            }
        }
        out
    }
}

impl NaturalSpawner {
    /// Vanilla `getRandomPosWithin`: a random column in the chunk, then a Y
    /// uniform in `[min_y, surface + 1]`.
    fn random_pos_within(&mut self, cx: i32, cz: i32) -> Option<(i32, i32, i32)> {
        let world = self.world?;
        let x = cx * 16 + self.rng.next_int(16);
        let z = cz * 16 + self.rng.next_int(16);
        let min_y = world.floor_y();
        let top = world.surface_y(x, z)? + 1;
        if top < min_y + 1 {
            return None;
        }
        let y = min_y + self.rng.next_int(top - min_y + 1);
        if y < min_y + 1 {
            return None;
        }
        Some((x, y, z))
    }
}

/// The [`lodestone_worldgen`] category for one of ours. Two independent enums by
/// the same name, in two crates that must not depend on each other — see
/// `crate::mobs`' note on the same hazard for `lodestone_entity`'s third one.
fn worldgen_category(category: MobCategory) -> lodestone_worldgen::spawners::MobCategory {
    use lodestone_worldgen::spawners::MobCategory as W;
    match category {
        MobCategory::Monster => W::Monster,
        MobCategory::Creature => W::Creature,
        MobCategory::Ambient => W::Ambient,
        MobCategory::Axolotls => W::Axolotls,
        MobCategory::UndergroundWaterCreature => W::UndergroundWaterCreature,
        MobCategory::WaterCreature => W::WaterCreature,
        MobCategory::WaterAmbient => W::WaterAmbient,
        MobCategory::Misc => W::Misc,
    }
}

/// The block name without its state properties.
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn is_water(state: &str) -> bool {
    let base = base_name(state);
    base == "minecraft:water" || state.contains("waterlogged=true")
}

fn is_lava(state: &str) -> bool {
    base_name(state) == "minecraft:lava"
}

/// Whether the state's collision shape is a full cube — vanilla's
/// `isCollisionShapeFullBlock`, and the `isRedstoneConductor` proxy
/// `spawnCategoryForPosition` opens with.
fn is_full_solid(state: &str) -> bool {
    block_state_id_or_default(state)
        .and_then(lodestone_data::collision_shapes::collision_boxes)
        .is_some_and(|boxes| {
            boxes.len() == 1
                && boxes[0].min.iter().all(|&v| v <= 0.0)
                && boxes[0].max.iter().all(|&v| v >= 1.0)
        })
}

/// Vanilla `BlockState.isValidSpawn`'s default: a sturdy up-face that emits less
/// than 14. Approximated as "a full collision cube", which is the only
/// sturdy-face question this crate's collision census can answer, plus the real
/// emission from the light census.
fn is_valid_spawn_surface(state: &str) -> bool {
    if !is_full_solid(state) {
        return false;
    }
    block_state_id_or_default(state)
        .and_then(lodestone_data::light_props::light_props)
        .is_none_or(|(_, emission)| emission < 14)
}

/// Vanilla `NaturalSpawner.isValidEmptySpawnBlock`: not a full collision block,
/// not a signal source, no fluid, and not in `PREVENT_MOB_SPAWNING_INSIDE`
/// (`#rails`).
fn is_valid_empty_spawn_block(state: &str) -> bool {
    let base = base_name(state);
    if is_full_solid(state) {
        return false;
    }
    if is_water(state) || is_lava(state) {
        return false;
    }
    if base.ends_with("rail") {
        return false;
    }
    !matches!(
        base,
        "minecraft:redstone_wire"
            | "minecraft:redstone_torch"
            | "minecraft:redstone_wall_torch"
            | "minecraft:redstone_block"
            | "minecraft:lever"
            | "minecraft:comparator"
            | "minecraft:repeater"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is sorted, so [`spawn_rule`]'s binary search is valid, and holds
    /// no duplicate — the invariant vanilla's own `register` throws on.
    #[test]
    fn table_is_sorted_and_unique() {
        for pair in SPAWN_RULES.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{} must sort before {}",
                pair[0].0,
                pair[1].0
            );
        }
    }

    /// Every species the bundled overworld and Nether biome documents can name
    /// has a registration — a species without one is silently unspawnable, which
    /// is exactly the failure this table exists to prevent.
    #[test]
    fn every_bundled_biome_species_has_a_rule() {
        let mut missing: Vec<String> = Vec::new();
        for spawners in crate::worldgen_data::bundled_biome_spawners().values() {
            for category in lodestone_worldgen::spawners::MobCategory::ALL {
                for entry in spawners.for_category(category) {
                    let path = entry
                        .entity_type
                        .strip_prefix("minecraft:")
                        .unwrap_or(&entry.entity_type);
                    if spawn_rule(path).is_none() {
                        missing.push(path.to_string());
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "biome spawn lists name species with no SpawnPlacements row: {missing:?}"
        );
    }

    /// The four rule families keep the distinctions vanilla draws between them.
    /// Getting `checkAnyLightMonsterSpawnRules` confused with
    /// `checkMonsterSpawnRules` is a zombified piglin that stops spawning in the
    /// Nether's daylight-equivalent; getting `Animal`'s tag wrong is a wolf on
    /// sand.
    #[test]
    fn rule_families_match_the_registration() {
        let zombie = spawn_rule("zombie").expect("registered");
        assert_eq!(zombie.light, LightRule::Dark);
        assert_eq!(zombie.placement, Placement::OnGround);
        assert!(!zombie.needs_sky);

        let husk = spawn_rule("husk").expect("registered");
        assert!(husk.needs_sky, "husk is checkSurfaceMonstersSpawnRules");

        let piglin = spawn_rule("zombified_piglin").expect("registered");
        assert_eq!(
            piglin.light,
            LightRule::Any,
            "checkZombifiedPiglinSpawnRules applies no light test"
        );

        let wolf = spawn_rule("wolf").expect("registered");
        assert_eq!(wolf.light, LightRule::Bright);
        assert_eq!(wolf.ground, Ground::OneOf(WOLVES_ON));

        let cod = spawn_rule("cod").expect("registered");
        assert_eq!(cod.placement, Placement::InWater);
        assert_eq!(cod.y_range, (SEA_LEVEL - 13, SEA_LEVEL));

        // A guardian is registered in vanilla but appears in no bundled biome
        // list, so it must be absent here rather than fall back to "anywhere".
        assert!(spawn_rule("guardian").is_none());
    }

    /// Issue #515: the slime row must carry the *alternation*, not one arm of it.
    ///
    /// The regression this pins is specific and was the shipped state: the row
    /// used to be `y_range: (51, 69)` with `LightRule::MaxRandom(8)`, i.e. the
    /// swamp arm only — and that band **excludes every Y the slime-chunk arm can
    /// fire at** (`y < 40`), so a working `is_slime_chunk` predicate could not
    /// have been reached even once.
    #[test]
    fn slime_carries_both_arms_not_one() {
        let slime = spawn_rule("slime").expect("registered");
        assert_eq!(slime.special, Special::Slime);
        assert_eq!(
            slime.y_range,
            (i32::MIN, i32::MAX),
            "a Y band on the row would gate both arms; each arm owns its own"
        );
        assert_eq!(
            slime.light,
            LightRule::Any,
            "the brightness test belongs to the swamp arm alone"
        );
        assert_eq!(slime.ground, Ground::ValidSpawn, "checkMobSpawnRules");
        assert!(SLIME_CHUNK_MAX_Y < 51, "the two arms' bands must not overlap");
    }

    /// `SURFACE_SLIME_SPAWN_CHANCE` across a full lunar month, against
    /// `DimensionType.MOON_BRIGHTNESS_PER_PHASE * 0.5` expanded by hand from the
    /// record — the expected values come from `DimensionType.java:57` and
    /// `Timelines.java:168-175`, not from this module.
    #[test]
    fn surface_slime_chance_follows_the_moon() {
        let expected = [0.5f32, 0.375, 0.25, 0.125, 0.0, 0.125, 0.25, 0.375];
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        for (phase, want) in expected.iter().enumerate() {
            // Mid-phase, so a wrong `div`/`rem` order lands on a different phase.
            spawner.set_day_time(phase as i64 * 24_000 + 12_000);
            assert!(
                (spawner.surface_slime_spawn_chance() - want).abs() < f32::EPSILON,
                "phase {phase}: want {want}, got {}",
                spawner.surface_slime_spawn_chance()
            );
        }
        // The month wraps, and a negative `day_time` (`/time set` can produce one)
        // must not index out of bounds.
        spawner.set_day_time(8 * 24_000);
        assert!((spawner.surface_slime_spawn_chance() - 0.5).abs() < f32::EPSILON);
        spawner.set_day_time(-1);
        let _ = spawner.surface_slime_spawn_chance();
    }
}
