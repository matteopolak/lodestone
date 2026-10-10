//! Natural mob spawning against a live world. This module combines the
//! cap/despawn engine in `mob_spawn.rs` with the per-species placement rules
//! exposed by `lodestone_entity::spawn`.
//!
//! ## What it is
//!
//! Three pieces that only make sense together:
//!
//! * [`SPAWN_RULES`] — the static placement table, transcribed as data for every
//!   species the bundled biome spawn lists can
//!   actually name (`every_bundled_biome_species_has_a_rule` keeps the table
//!   complete). This is the *data* half `crate::mob_spawn`'s
//!   module doc says must not live in the version-free engine.
//! * [`ColumnLight`] — a per-column light cache over `lodestone_world`'s real
//!   light engine, because every monster rule in the game is a light test and the
//!   server had no light at any position.
//! * [`NaturalSpawner`] — a [`SpawnCandidateSource`](crate::mob_spawn::SpawnCandidateSource)
//!   that runs the category cluster loop over
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
//!   no per-block relight anywhere in this tree, so a torch placed in
//!   a dark room stops spawns within ten seconds rather than instantly. Vanilla's
//!   own lighting is asynchronous; this is a coarser version of the same lag, and
//!   it is the reason the cache is a TTL rather than a dirty set.
//!
//! ## How to change it, and the gotchas
//!
//! * **The species table is a record transcription, not a guess.** Every row
//!   comes from vanilla's own spawn-placement registration plus the `check*SpawnRules`
//!   body it names; the block-tag rows come from
//!   `data/minecraft/tags/block/*_spawnable_on.json`. If you add a species,
//!   read its predicate — the families genuinely differ (a wolf wants
//!   `WOLVES_SPAWNABLE_ON` and brightness > 8; a bat wants stone below,
//!   `nextBoolean()`, and brightness ≤ `nextInt(4)`).
//! * **A predicate that branches over *alternatives* needs [`Special`], not more
//!   fields.** There is one: Slime's check slime spawn rules, whose swamp-surface and
//!   slime-chunk arms own a Y band and a set of RNG draws each, so a single row of
//!   conjoined fields structurally cannot express it —
//!   [`NaturalSpawner::slime_permits`] and `docs/natural-mob-spawning.md`.
//! * **A species absent from [`SPAWN_RULES`] cannot spawn**, deliberately. The
//!   alternative — falling back to "no restrictions" — spawns guardians on land.
//!   [`spawn_rule`] returning `None` is why a Nether-only species in an overworld
//!   biome list is inert rather than wrong.
//! * **Sea level is the dimension's default.** Rule bands are offsets from
//!   [`Dimension::sea_level`]; a custom generator `sea_level` would shift the
//!   water-animal bands.
//! * **Environment inputs come from the tick owner.** Difficulty refuses
//!   forbidden species before placement; dimension and weather determine the
//!   monster light thresholds. Animal brightness remains un-darkened skylight.
//!
//! ## Dependencies
//!
//! `lodestone_world` (the light engine), `lodestone_data` (`light_props`,
//! `block_states`), `lodestone_worldgen::spawners` (the biome lists) and
//! `crate::mob_spawn` (the cap engine and its RNG).

use std::collections::HashMap;
use std::str::FromStr;

use lodestone_data::block_states::StateId;
use lodestone_data::block::Block;
use lodestone_model::{Difficulty, ResourceKey, Vec3};
use lodestone_world::{BlockVolume, LightProperties, compute_column_light};

use crate::chunk::{ChunkColumn, ChunkGenerationStage};
use crate::dimension::Dimension;
use crate::generation_population::PlacementDecision;
use crate::mob_spawn::{MobCategory, SpawnCandidate, SpawnCandidateSource, SpawnRng};
use crate::mobs::ChunkWorld;

/// How many columns [`NaturalSpawner`] will light in one spawn cycle before
/// giving up on the rest until the next. See the module doc.
pub const LIGHT_BUDGET_PER_CYCLE: usize = 4;

/// How long a column's cached light is trusted, in ticks (10 s).
pub const LIGHT_TTL_TICKS: u64 = 200;

/// Vanilla `NaturalSpawner.MIN_SPAWN_DISTANCE` squared — a mob never spawns
/// within 24 blocks of the nearest player.
const MIN_PLAYER_DIST_SQR: f64 = 576.0;

/// The biome tags's allows surface slime spawns, flattened —
/// vanilla's own biome-tag provider adds exactly `swamp` and `mangrove_swamp` and
/// nothing else, so the tag is two names rather than a lookup.
const SURFACE_SLIME_BIOMES: &[&str] = &["minecraft:swamp", "minecraft:mangrove_swamp"];

/// Vanilla's own `DimensionType.MOON_BRIGHTNESS_PER_PHASE`, indexed
/// by `MoonPhase.index()`.
const MOON_BRIGHTNESS_PER_PHASE: [f32; 8] = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75];

/// Vanilla's own slime spawn-rules check's slime-chunk ceiling: the slime-chunk arm only
/// fires strictly below this Y.
const SLIME_CHUNK_MAX_Y: i32 = 40;

/// How a species is positioned relative to the candidate block —
/// The spawn placement types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// `ON_GROUND`: a valid spawn surface below, and two blocks of legal empty
    /// space at and above the position.
    OnGround,
    /// `IN_WATER`: water at the position, and the block above not a full solid.
    /// The surface water-animal rows additionally require water above, via
    /// [`SpawnRule::water_above`].
    InWater,
    /// `IN_LAVA`: lava at the position.
    InLava,
    /// `NO_RESTRICTIONS`: anything goes (phantoms, vexes, foxes, pandas).
    NoRestrictions,
}

/// The light condition a species' `check*SpawnRules` applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightRule {
    /// Monster's is dark enough to spawn: raw sky light must not exceed
    /// `nextInt(32)`, block light must be 0 (the overworld's
    /// `monsterSpawnBlockLightLimit`), and the local raw brightness must not
    /// exceed the overworld's `monsterSpawnLightTest, the uniform int(0, 7)`.
    Dark,
    /// Animal's is bright enough to spawn: raw brightness > 8.
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
    /// `Mob.checkMobSpawnRules`: BlockState's is valid spawn, i.e. a sturdy up-face
    /// that emits less than 14.
    ValidSpawn,
    /// One of these generated block types (a `*_spawnable_on` tag, flattened —
    /// see the module doc).
    OneOf(&'static [Block]),
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
/// There are three today. The point of naming it rather than widening
/// [`SpawnRule`] with four more `Option`s is that the alternation, and the RNG
/// draw order it implies, is *code* in vanilla too; a data row cannot express
/// "arm A, else arm B" without also encoding which arm consumed which draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    /// No special arm: the [`SpawnRule`] fields are the whole predicate.
    None,
    /// Vanilla's own slime spawn-rules check — see
    /// [`NaturalSpawner::slime_permits`].
    Slime,
    /// The drowned predicate: a biome-dependent dice gate, with a depth gate
    /// outside the river biomes. See [`NaturalSpawner::drowned_permits`].
    Drowned,
    /// The tropical fish predicate: the surface band applies everywhere except
    /// the biomes that allow it at any height.
    TropicalFish,
}

/// One species' whole spawn condition: the spawn placements' registered placement
/// type plus the `check*SpawnRules` predicate it registered with, reduced to the
/// checks this server can actually answer.
#[derive(Debug, Clone, Copy)]
pub struct SpawnRule {
    /// The spawn placements placement type.
    pub placement: Placement,
    /// The predicate's light test.
    pub light: LightRule,
    /// What the predicate demands below the position.
    pub ground: Ground,
    /// Inclusive Y band relative to the dimension's sea level, when the
    /// predicate names one; `None` leaves that side open.
    pub sea_band: (Option<i32>, Option<i32>),
    /// Whether the position must see the sky (`checkSurfaceMonstersSpawnRules`).
    pub needs_sky: bool,
    /// The predicate's own `nextInt` gate.
    pub chance: Chance,
    /// Whether the block directly above the position must be a plain water
    /// block (the surface water-animal predicates), beyond the placement type's
    /// own "not a redstone conductor" test.
    pub water_above: bool,
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
            sea_band: (None, None),
            needs_sky: false,
            chance: Chance::Always,
            water_above: false,
            special: Special::None,
        }
    }

    /// Monster's check monster spawn rules — dark enough, valid surface below.
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

    /// Monster's check any light monster spawn rules — no light test (blaze, breeze,
    /// zoglin), and the family every difficulty-only Nether predicate reduces to
    /// (magma cube, sulfur cube, zombified piglin).
    const fn any_light_monster() -> Self {
        Self::base()
    }

    /// Animal's check animal spawn rules and its per-species tag variants: bright
    /// enough (> 8) and standing on `on`.
    const fn animal(on: &'static [Block]) -> Self {
        Self {
            light: LightRule::Bright,
            ground: Ground::OneOf(on),
            ..Self::base()
        }
    }

    /// The surface water-animal and ageable-water-creature predicates: in the
    /// `[sea - 13, sea]` band, water below and a plain water block above.
    const fn surface_water() -> Self {
        Self {
            placement: Placement::InWater,
            ground: Ground::Water,
            sea_band: (Some(-13), Some(0)),
            water_above: true,
            ..Self::base()
        }
    }
}

/// `minecraft:grass_block` — `ANIMALS_SPAWNABLE_ON`.
const ANIMALS_ON: &[Block] = &[Block::GrassBlock];
/// `WOLVES_SPAWNABLE_ON`.
const WOLVES_ON: &[Block] = &[
    Block::GrassBlock,
    Block::Snow,
    Block::SnowBlock,
    Block::CoarseDirt,
    Block::Podzol,
];
/// `FOXES_SPAWNABLE_ON`.
const FOXES_ON: &[Block] = &[
    Block::GrassBlock,
    Block::Snow,
    Block::SnowBlock,
    Block::Podzol,
    Block::CoarseDirt,
];
/// `RABBITS_SPAWNABLE_ON`.
const RABBITS_ON: &[Block] = &[
    Block::GrassBlock,
    Block::Snow,
    Block::SnowBlock,
    Block::Sand,
];
/// `GOATS_SPAWNABLE_ON` (the `ANIMALS_SPAWNABLE_ON` include flattened in).
const GOATS_ON: &[Block] = &[
    Block::GrassBlock,
    Block::Stone,
    Block::Snow,
    Block::SnowBlock,
    Block::PackedIce,
    Block::Gravel,
];
/// `FROGS_SPAWNABLE_ON`.
const FROGS_ON: &[Block] = &[
    Block::GrassBlock,
    Block::Mud,
    Block::MangroveRoots,
    Block::MuddyMangroveRoots,
];
/// `MOOSHROOMS_SPAWNABLE_ON`.
const MOOSHROOMS_ON: &[Block] = &[Block::Mycelium];
/// `PARROTS_SPAWNABLE_ON`, with the `#leaves`/`#logs` includes reduced to the
/// two the bundled jungle surface actually produces below a spawn position.
const PARROTS_ON: &[Block] = &[
    Block::GrassBlock,
    Block::Air,
    Block::JungleLeaves,
    Block::JungleLog,
    Block::OakLeaves,
    Block::OakLog,
];
/// `ARMADILLO_SPAWNABLE_ON`.
const ARMADILLO_ON: &[Block] = &[
    Block::GrassBlock,
    Block::RedSand,
    Block::CoarseDirt,
    Block::Terracotta,
];
/// `CAMELS_SPAWNABLE_ON` (`#sand`).
const CAMELS_ON: &[Block] = &[Block::Sand, Block::RedSand];
/// `BATS_SPAWNABLE_ON` (`#base_stone_overworld`).
const BATS_ON: &[Block] = &[
    Block::Stone,
    Block::Granite,
    Block::Diorite,
    Block::Andesite,
    Block::Tuff,
    Block::Deepslate,
];
/// `AXOLOTLS_SPAWNABLE_ON`.
const AXOLOTLS_ON: &[Block] = &[Block::Clay];

/// The spawn placements' registration for every species the bundled overworld
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
        // Water below, dark enough, water at the position; then the dice and
        // depth gates of `Special::Drowned`.
        SpawnRule {
            placement: Placement::InWater,
            light: LightRule::Dark,
            ground: Ground::Water,
            special: Special::Drowned,
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
            sea_band: (None, Some(-33)),
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
    ("nautilus", {
        // A deeper band than the surface animals: `[sea - 25, sea - 5]`.
        SpawnRule {
            sea_band: (Some(-25), Some(-5)),
            ..SpawnRule::surface_water()
        }
    }),
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
    ("tropical_fish", {
        SpawnRule {
            sea_band: (None, None),
            special: Special::TropicalFish,
            ..SpawnRule::surface_water()
        }
    }),
    ("turtle", {
        SpawnRule {
            light: LightRule::Bright,
            ground: Ground::OneOf(&[Block::Sand, Block::RedSand]),
            sea_band: (None, Some(3)),
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
struct PaletteProps {
    states: Vec<(u8, u8)>,
    has_skylight: bool,
}

impl PaletteProps {
    fn of(column: &ChunkColumn, dimension: Dimension) -> Self {
        Self {
            states: column
                .palette()
                .iter()
                .map(|&state| lodestone_data::light_props::light_props(state))
                .collect(),
            has_skylight: dimension.has_skylight(),
        }
    }
}

impl LightProperties for PaletteProps {
    fn has_skylight(&self) -> bool {
        self.has_skylight
    }

    fn opacity(&self, state: u32) -> u8 {
        self.states.get(state as usize).map_or(15, |&(d, _)| d)
    }

    fn emission(&self, state: u32) -> u8 {
        self.states.get(state as usize).map_or(0, |&(_, e)| e)
    }
}

/// Sky and block light for one column, sampled by world coordinate.
#[derive(Debug)]
struct ColumnLight {
    light: lodestone_world::ColumnLight,
    min_y: i32,
    section_count: usize,
    has_skylight: bool,
}

impl ColumnLight {
    fn compute(column: &ChunkColumn, dimension: Dimension) -> Self {
        let volume = PaletteVolume::of(column);
        let props = PaletteProps::of(column, dimension);
        Self {
            light: compute_column_light(&volume, &props),
            min_y: column.min_y,
            section_count: column.section_count(),
            has_skylight: dimension.has_skylight(),
        }
    }

    /// `(sky, block)` raw light at world `y` and chunk-local `x`/`z`.
    ///
    /// Above the built column, sky-bearing dimensions have full sky light;
    /// dimensions without a sky source have zero. Block light is zero there.
    fn at(&self, x: usize, y: i32, z: usize) -> (u8, u8) {
        let local = y - self.min_y;
        if local < 0 {
            return (0, 0);
        }
        let s = (local / 16) as usize;
        if s >= self.section_count {
            return (if self.has_skylight { 15 } else { 0 }, 0);
        }
        let y_in = (local % 16) as usize;
        let sky = if self.has_skylight {
            self.light.section_sky_light(s, x, y_in, z).unwrap_or(15)
        } else {
            0
        };
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
    /// [`crate::MobHandle::replace_world`] replaces the sim's world, and a spawner
    /// holding the old one would light chunks nothing paths over.
    ///
    /// **`Arc`, not the `&'static` this used to be.** The old lifetime came from
    /// `MobHandle`'s deliberate `Box::leak`, which is sound only because that
    /// snapshot is built **once** and never moves. The spawn area now *follows the
    /// player* (`crate::tick_area::FollowArea`), so this view is replaced every
    /// time the area moves — and leaking one 49-column snapshot per chunk boundary
    /// crossed would leak roughly 31 KiB per column for the life of the process.
    /// Refcounting it costs one atomic increment per accessor call and bounds the
    /// memory at one live view.
    world: Option<std::sync::Arc<ChunkWorld>>,
    /// The **world generation seed**, which is a different number from the
    /// spawn-RNG seed `new` takes and is used for exactly one thing:
    /// WorldgenRandom's seed slime chunk. See [`with_world_seed`](Self::with_world_seed).
    world_seed: i64,
    /// The world clock for sky darkening and the surface-slime moon phase.
    day_time: i64,
    /// The world difficulty for the species-specific peaceful guard.
    difficulty: Difficulty,
    dimension: Dimension,
    rain_level: f32,
    thunder_level: f32,
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
            // The level settings's default's difficulty, matching
            // `crate::world_state::WorldState`'s own default, so a spawner nobody
            // sets it on behaves exactly as it did before the guard existed.
            difficulty: Difficulty::Normal,
            dimension: Dimension::Overworld,
            rain_level: 0.0,
            thunder_level: 0.0,
        }
    }

    /// Records the **world generation** seed, which is what
    /// WorldgenRandom's seed slime chunk mixes and therefore what decides which
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
    /// proposed at all — vanilla's spawn placements's check spawn rules, whose *first*
    /// statement is `!type.is_allowed_in_peaceful() && level.get_difficulty() ==
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

    /// Supplies the tick owner's dimension and current weather intensities.
    /// Changing dimension invalidates light sampled with a different sky source.
    pub fn set_environment(&mut self, dimension: Dimension, rain_level: f32, thunder_level: f32) {
        if self.dimension != dimension {
            self.lights.clear();
            self.dimension = dimension;
        }
        self.rain_level = rain_level.clamp(0.0, 1.0);
        self.thunder_level = thunder_level.clamp(0.0, 1.0);
    }

    /// Starts a cycle at `tick` with `players` as the loaded players, resetting
    /// the per-cycle light budget and dropping the cache once its TTL is up.
    pub fn begin_cycle(
        &mut self,
        world: std::sync::Arc<ChunkWorld>,
        tick: u64,
        players: Vec<Vec3>,
    ) {
        self.start_cycle(tick, players);
        self.use_world(world);
    }

    pub(crate) fn start_cycle(&mut self, tick: u64, players: Vec<Vec3>) {
        self.world = None;
        self.players = players;
        self.lit_this_cycle = 0;
        if tick.saturating_sub(self.lights_refreshed_at) >= LIGHT_TTL_TICKS {
            self.lights.clear();
            self.lights_refreshed_at = tick;
        }
    }

    /// Changes the terrain view without resetting this cycle's shared light budget.
    pub(crate) fn use_world(&mut self, world: std::sync::Arc<ChunkWorld>) {
        self.world = Some(world);
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
                    .filter_map(|e| e.entity_type.builtin_or_none().map(|id| id.name()))
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
        let world = self.world.clone()?;
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16) as usize, z.rem_euclid(16) as usize);
        if !self.lights.contains_key(&(cx, cz)) {
            if self.lit_this_cycle >= LIGHT_BUDGET_PER_CYCLE {
                return None;
            }
            let column = world.column(cx, cz)?;
            self.lit_this_cycle += 1;
            self.lights.insert((cx, cz), ColumnLight::compute(column, self.dimension));
        }
        Some(self.lights[&(cx, cz)].at(lx, y, lz))
    }

    /// Un-darkened light, used by animals and glow squid regardless of the clock.
    fn raw_brightness(sky: u8, block: u8) -> u8 {
        sky.max(block)
    }

    fn effective_thunder(&self) -> f32 {
        if self.dimension == Dimension::Overworld {
            self.rain_level * self.thunder_level
        } else {
            0.0
        }
    }

    fn sky_darkening(&self) -> u8 {
        sky_darkening_for(self.dimension, self.day_time, self.rain_level, self.effective_thunder())
    }

    fn local_brightness(&self, sky: u8, block: u8) -> u8 {
        sky.saturating_sub(self.sky_darkening()).max(block)
    }

    fn monster_brightness(&self, sky: u8, block: u8) -> u8 {
        let darkening = if self.effective_thunder() > 0.9 {
            10
        } else {
            self.sky_darkening()
        };
        sky.saturating_sub(darkening).max(block)
    }

    /// The species-independent half of `isValidSpawnPostitionForType` plus the
    /// species' own `SpawnRule`, evaluated at `pos`.
    fn permits(&mut self, rule: &SpawnRule, x: i32, y: i32, z: i32) -> bool {
        self.placement_permits(rule, x, y, z).unwrap_or(false)
    }

    #[allow(clippy::too_many_lines)]
    fn placement_permits(&mut self, rule: &SpawnRule, x: i32, y: i32, z: i32) -> Option<bool> {
        let sea_level = self.dimension.sea_level();
        let (low, high) = rule.sea_band;
        if low.is_some_and(|low| y < sea_level + low) || high.is_some_and(|high| y > sea_level + high) {
            return Some(false);
        }
        let world = self.world.clone()?;
        let column = world.column(x.div_euclid(16), z.div_euclid(16))?;
        if column.generation_stage() < ChunkGenerationStage::Full {
            return None;
        }

        let here = world.block_state_id(x, y, z);
        let below = world.block_state_id(x, y - 1, z);
        let above = world.block_state_id(x, y + 1, z);

        match rule.placement {
            Placement::OnGround => {
                if !is_valid_spawn_surface_id(below) {
                    return Some(false);
                }
                if !is_valid_empty_spawn_block_id(here) || !is_valid_empty_spawn_block_id(above) {
                    return Some(false);
                }
            }
            Placement::InWater => {
                if !is_water_id(here) {
                    return Some(false);
                }
                if is_full_solid_id(above) {
                    return Some(false);
                }
                if rule.water_above && above.block() != Block::Water {
                    return Some(false);
                }
            }
            Placement::InLava => {
                if !is_lava_id(here) {
                    return Some(false);
                }
            }
            Placement::NoRestrictions => {}
        }

        match rule.ground {
            Ground::ValidSpawn => {
                if !is_valid_spawn_surface_id(below) {
                    return Some(false);
                }
            }
            Ground::OneOf(blocks) => {
                if !blocks.contains(&below.block()) {
                    return Some(false);
                }
            }
            Ground::Water => {
                if !is_water_id(below) {
                    return Some(false);
                }
            }
            Ground::Any => {}
        }

        // Terrain failures precede light draws so invalid positions do not
        // consume the placement RNG stream.
        let (sky, block) = self.light_at(x, y, z)?;
        if rule.needs_sky && sky < 15 {
            return Some(false);
        }
        match rule.light {
            LightRule::Any => {}
            LightRule::Dark => {
                if i32::from(sky) > self.rng.next_int(32) {
                    return Some(false);
                }
                let block_limit = if self.dimension == Dimension::Nether {
                    15
                } else {
                    0
                };
                if block > block_limit {
                    return Some(false);
                }
                let threshold = match self.dimension {
                    Dimension::Overworld => self.rng.next_int(8),
                    Dimension::Nether => 7,
                    Dimension::End => 15,
                };
                if i32::from(self.monster_brightness(sky, block)) > threshold {
                    return Some(false);
                }
            }
            LightRule::Bright => {
                if Self::raw_brightness(sky, block) <= 8 {
                    return Some(false);
                }
            }
            LightRule::MaxRandom(bound) => {
                if i32::from(self.local_brightness(sky, block)) > self.rng.next_int(bound) {
                    return Some(false);
                }
            }
            LightRule::Zero => {
                if Self::raw_brightness(sky, block) != 0 {
                    return Some(false);
                }
            }
        }
        match rule.special {
            Special::None => {}
            Special::Slime => {
                if !self.slime_permits(x, y, z, self.local_brightness(sky, block)) {
                    return Some(false);
                }
            }
            Special::Drowned => {
                if !self.drowned_permits(x, y, z) {
                    return Some(false);
                }
            }
            Special::TropicalFish => {
                let any_height = self
                    .world
                    .as_ref()
                    .and_then(|w| w.biome_at(x, y, z))
                    .is_some_and(|b| b == "minecraft:lush_caves");
                if !any_height && !(sea_level - 13..=sea_level).contains(&y) {
                    return Some(false);
                }
            }
        }
        Some(true)
    }

    /// The moon-phase `SURFACE_SLIME_SPAWN_CHANCE` at the current `day_time`.
    ///
    /// Vanilla's own environment attributes's surface slime spawn chance
    /// defaults to **`0.0`** and is raised by exactly one modifier track:
    /// vanilla's own moon timeline, a float modifier's maximum
    /// keyframed `CONSTANT` (so a step function, not a ramp) at each phase start to
    /// `MOON_BRIGHTNESS_PER_PHASE[phase] * 0.5`. `max(0.0, that)` is `that`, so the
    /// whole attribute reduces to this expression.
    ///
    /// The consequence is worth stating because it is not how older versions
    /// behaved and it reads as a bug if you do not know it: **at new moon the
    /// surface arm cannot fire at all** (chance `0.0`, and `nextFloat() < 0.0` is
    /// never true), and at full moon it is `0.5`. Surface swamp slimes are a
    /// moon-phase feature in this release.
    #[must_use]
    fn surface_slime_spawn_chance(&self) -> f32 {
        // `MoonPhase.PHASE_LENGTH` is 24000 and `MoonPhase.COUNT` is 8; the
        // timeline's period is `24000 * COUNT` and each phase's start tick is
        // `index * 24000`.
        let phase = self.day_time.div_euclid(24_000).rem_euclid(8) as usize;
        MOON_BRIGHTNESS_PER_PHASE[phase] * 0.5
    }

    /// Vanilla's own slime spawn-rules check, minus the two clauses
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
    /// The two omitted clauses: `level.get_difficulty() != PEACEFUL` (the tick loop
    /// gates the whole cycle, and `remove_monsters` evicts anything that slips
    /// through — see the module doc) and the entity spawn reason's is spawner early
    /// return, which cannot apply to a natural spawn.
    fn slime_permits(&mut self, x: i32, y: i32, z: i32, brightness: u8) -> bool {
        let surface_band = y > 50 && y < 70;
        let surface_biome = surface_band
            && self
                .world
                .as_ref()
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

    /// The drowned dice and depth gates: one in 15 in the river biomes, one in 40
    /// and strictly below `sea level - 5` everywhere else.
    fn drowned_permits(&mut self, x: i32, y: i32, z: i32) -> bool {
        let river = self
            .world
            .as_ref()
            .and_then(|w| w.biome_at(x, y, z))
            .is_some_and(|b| is_river_biome(&b));
        if river {
            self.rng.next_int(15) == 0
        } else {
            self.rng.next_int(40) == 0 && y < self.dimension.sea_level() - 5
        }
    }

    /// One weighted pick out of `category`'s list for the biome at `(x, y, z)`,
    /// drawing exactly once from the RNG — vanilla's WeightedList's get random.
    fn pick_species(
        &mut self,
        category: MobCategory,
        x: i32,
        y: i32,
        z: i32,
    ) -> Option<(&'static SpawnRule, ResourceKey, i32, i32)> {
        let biome = self.world.clone()?.biome_at(x, y, z)?;
        // Rivers skip 98 percent of the water-ambient picks.
        if category == MobCategory::WaterAmbient && is_river_biome(&biome) && self.rng.next_f32() < 0.98 {
            return None;
        }
        let entries = self.biomes.get(&biome)?.for_category(worldgen_category(category));
        let total: i32 = entries.iter().map(|e| e.weight.max(0)).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = self.rng.next_int(total);
        for entry in entries {
            roll -= entry.weight.max(0);
            if roll < 0 {
                let entity_type = entry.entity_type.builtin_or_none()?;
                let key = ResourceKey::from_str(entity_type.name()).ok()?;
                let rule = spawn_rule(entity_type.path())?;
                return Some((rule, key, entry.min_count, entry.max_count));
            }
        }
        None
    }

    /// Constants consume no count draw; distinct bounds consume one inclusive draw.
    fn group_attempt_count(&mut self, min_count: i32, max_count: i32) -> i32 {
        if max_count > min_count {
            min_count + self.rng.next_int(max_count - min_count + 1)
        } else {
            min_count
        }
    }

    /// Applies placement rules without losing candidates whose terrain or light
    /// is unavailable. Generation population has no player-distance or cap gate.
    pub fn classify_generation_spawn(
        &mut self,
        candidate: &lodestone_worldgen::spawn_stage::GenerationSpawn,
    ) -> PlacementDecision {
        let Some(entity_type) = candidate.entity_type.builtin_or_none() else {
            return PlacementDecision::Rejected;
        };
        let Ok(key) = ResourceKey::from_str(entity_type.name()) else {
            return PlacementDecision::Rejected;
        };
        let Some(rule) = spawn_rule(entity_type.path()) else {
            return PlacementDecision::Rejected;
        };
        if self.difficulty == Difficulty::Peaceful
            && !crate::mob_spawn::allowed_in_peaceful(key.path())
        {
            return PlacementDecision::Rejected;
        }
        match self.placement_permits(rule, candidate.x, candidate.y, candidate.z) {
            None => PlacementDecision::Deferred,
            Some(false) => PlacementDecision::Rejected,
            Some(true) => PlacementDecision::Accepted(SpawnCandidate {
                pos: Vec3::new(
                    f64::from(candidate.x) + 0.5,
                    f64::from(candidate.y),
                    f64::from(candidate.z) + 0.5,
                ),
                entity_type: key,
            }),
        }
    }
}

impl SpawnCandidateSource for NaturalSpawner {
    /// Vanilla NaturalSpawner's spawn category for chunk for one chunk and one
    /// category, returning the whole group it produced.
    ///
    /// The draw order is vanilla's, in vanilla's sequence: the start position,
    /// then per group a `ceil(nextFloat() * 4)` attempt budget, then per attempt
    /// two `nextInt(6)` pairs for the wander, then the species pick (once per
    /// group) and its count, then the rule's own light and chance draws.
    fn cluster(&mut self, category: MobCategory, cx: i32, cz: i32) -> Vec<SpawnCandidate> {
        let mut out = Vec::new();
        let Some(world) = self.world.clone() else {
            return out;
        };
        let Some(start) = self.random_pos_within(cx, cz) else {
            return out;
        };
        let (sx, sy, sz) = start;
        // `if (!state.is_redstone_conductor(...))` — a spawn never starts inside a
        // full solid.
        if is_full_solid_id(world.block_state_id(sx, sy, sz)) {
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
                    // The selected species replaces the provisional group budget.
                    attempts = self.group_attempt_count(min_count, max_count);
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

                // The spawn placements's check spawn rules' first statement, ahead of the
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
        let world = self.world.clone()?;
        if world.column(cx, cz)?.generation_stage() < ChunkGenerationStage::Full {
            return None;
        }
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

/// The river biome tag: `river` and `frozen_river`.
fn is_river_biome(biome: &str) -> bool {
    matches!(biome, "minecraft:river" | "minecraft:frozen_river")
}

fn is_water_id(state: StateId) -> bool {
    state.block() == Block::Water
        || state
            .properties()
            .iter()
            .any(|&(key, value)| key == "waterlogged" && value == "true")
}

fn is_lava_id(state: StateId) -> bool {
    state.block() == Block::Lava
}

fn is_full_solid_id(state: StateId) -> bool {
    let boxes = lodestone_data::collision_shapes::collision_boxes(state);
    boxes.len() == 1
        && boxes[0].min.iter().all(|&v| v <= 0.0)
        && boxes[0].max.iter().all(|&v| v >= 1.0)
}

fn is_valid_spawn_surface_id(state: StateId) -> bool {
    is_full_solid_id(state)
        && lodestone_data::light_props::light_props(state).1 < 14
}

fn is_valid_empty_spawn_block_id(state: StateId) -> bool {
    if is_full_solid_id(state) || is_water_id(state) || is_lava_id(state) {
        return false;
    }
    !matches!(
        state.block(),
        Block::Rail
            | Block::PoweredRail
            | Block::DetectorRail
            | Block::ActivatorRail
            | Block::RedstoneWire
            | Block::RedstoneTorch
            | Block::Lever
            | Block::RedstoneWallTorch
            | Block::RedstoneBlock
            | Block::Comparator
            | Block::Repeater
    )
}

/// How much the sky's light is reduced: 0 at full day to 11 at night, from the
/// clock and the weather (`thunder` is the effective thunder level).
pub(crate) fn sky_darkening_for(dimension: Dimension, day_time: i64, rain_level: f32, thunder: f32) -> u8 {
    if dimension != Dimension::Overworld {
        return if dimension == Dimension::Nether { 11 } else { 0 };
    }
    let tick = day_time.rem_euclid(24_000) as f32;
    let night_factor = 0.26666668;
    let factor = if (133.0..=11_867.0).contains(&tick) {
        1.0
    } else if tick < 13_670.0 && tick > 11_867.0 {
        1.0 + (night_factor - 1.0) * ((tick - 11_867.0) / 1_803.0)
    } else if (13_670.0..=22_330.0).contains(&tick) {
        night_factor
    } else {
        let dawn_tick = if tick < 133.0 { tick + 24_000.0 } else { tick };
        night_factor + (1.0 - night_factor) * ((dawn_tick - 22_330.0) / 1_803.0)
    };
    let mut level = 15.0 * factor;
    let rain = rain_level - thunder;
    level += rain * 0.3125 * (4.0 - level);
    level += thunder * 0.52734375 * (4.0 - level);
    (15.0 - level) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_count_constants_and_uniforms_preserve_rng_suffix() {
        // Standard seed-zero SplitMix64 words are E220A8397B1DCDAF,
        // 6E789E6AA1B965F4 and 06C45D188009454F. Inclusive-bound arithmetic
        // gives 2 + word1 % 4 = 5, 4 + word2 % 3 = 4, word3 % 1009 = 569.
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        let counts = [(4, 4), (2, 5), (1, 1), (4, 6)];
        let actual = counts.map(|(min, max)| spawner.group_attempt_count(min, max));
        assert_eq!(actual, [4, 5, 1, 4]);
        let suffix = spawner.rng.next_int(1009);
        assert_eq!(suffix, 569);

        // The old unconditional draw consumes four words instead of two.
        // Words four/five are F88BB8A8724C81EC and 1B39896A51A8749B.
        let mut always_draw = SpawnRng::new(0);
        let wrong = counts.map(|(min, max)| min + always_draw.next_int(max - min + 1));
        assert_eq!(wrong, [4, 2, 1, 5]);
        assert_ne!(wrong, actual);
        let wrong_suffix = always_draw.next_int(1009);
        assert_eq!(wrong_suffix, 792);
        assert_ne!(wrong_suffix, suffix);
    }

    fn cow_candidate(cx: i32) -> lodestone_worldgen::spawn_stage::GenerationSpawn {
        lodestone_worldgen::spawn_stage::GenerationSpawn {
            entity_type: lodestone_data::entity_type::EntityType::Cow.into(),
            x: cx * 16 + 3,
            y: 64,
            z: 7,
        }
    }

    fn grass_column() -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 80);
        column.set_block_id(3, 63, 7, Block::GrassBlock.default_state());
        column
    }

    #[test]
    fn spawn_sky_darkening_matches_day_track_and_weather_values() {
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        for (time, darkening) in [
            (133, 0), (6_000, 0), (11_867, 0), (12_800, 5),
            (13_670, 11), (18_000, 11), (22_330, 11), (23_200, 5),
            (24_133, 0), (-6_000, 11),
        ] {
            spawner.set_day_time(time);
            assert_eq!(spawner.sky_darkening(), darkening, "time={time}");
        }
        spawner.set_day_time(6_000);
        spawner.set_environment(Dimension::Overworld, 1.0, 0.0);
        assert_eq!(spawner.sky_darkening(), 3);
        assert_eq!(spawner.monster_brightness(15, 0), 12);
        spawner.set_environment(Dimension::Overworld, 1.0, 1.0);
        assert_eq!(spawner.sky_darkening(), 5);
        assert_eq!(spawner.monster_brightness(15, 0), 5);
        assert_eq!(spawner.monster_brightness(15, 8), 8);
        spawner.set_environment(Dimension::Overworld, 1.0, 0.9);
        assert_eq!(spawner.monster_brightness(15, 0), 10);
        spawner.set_environment(Dimension::Overworld, 0.5, 1.0);
        assert_eq!(spawner.monster_brightness(15, 0), 13);
        spawner.set_environment(Dimension::Nether, 1.0, 1.0);
        assert_eq!(spawner.sky_darkening(), 11);
        assert_eq!(spawner.effective_thunder(), 0.0);
        spawner.set_environment(Dimension::End, 1.0, 1.0);
        assert_eq!(spawner.sky_darkening(), 0);
    }

    #[test]
    fn spawn_light_controls_distinguish_night_monsters_from_animal_brightness() {
        let world = std::sync::Arc::new(ChunkWorld::from_columns([((0, 0), grass_column())]));
        let monster = spawn_rule("zombie").expect("monster rule");
        let animal = spawn_rule("cow").expect("animal rule");
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        spawner.begin_cycle(world, 1, Vec::new());
        spawner.set_day_time(6_000);
        assert_eq!(spawner.light_at(3, 64, 7), Some((15, 0)));
        assert_eq!(spawner.local_brightness(15, 0), 15);
        assert!(!spawner.permits(monster, 3, 64, 7));
        assert!(spawner.permits(animal, 3, 64, 7));
        spawner.set_day_time(18_000);
        spawner.rng = SpawnRng::new(0);
        assert_eq!(spawner.local_brightness(15, 0), 4);
        // Seed zero's first two standard SplitMix64 words end in AF and F4:
        // raw sky compares with 15, and the final threshold is 4.
        assert!(spawner.permits(monster, 3, 64, 7));
        assert!(spawner.permits(animal, 3, 64, 7));
        spawner.set_environment(Dimension::Overworld, 1.0, 1.0);
        assert!(spawner.permits(animal, 3, 64, 7));
    }

    #[test]
    fn spawn_dimension_light_controls_keep_sky_and_monster_limits_distinct() {
        let world = std::sync::Arc::new(ChunkWorld::from_columns([((0, 0), grass_column())]));
        let monster = spawn_rule("enderman").expect("monster rule");
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        spawner.begin_cycle(world, 1, Vec::new());
        assert_eq!(spawner.light_at(3, 64, 7), Some((15, 0)));
        assert!(!spawner.permits(monster, 3, 64, 7));
        spawner.set_environment(Dimension::End, 0.0, 0.0);
        spawner.rng = SpawnRng::new(0);
        assert_eq!(spawner.light_at(3, 64, 7), Some((15, 0)));
        assert!(spawner.permits(monster, 3, 64, 7));
        spawner.set_environment(Dimension::Nether, 0.0, 0.0);
        assert_eq!(spawner.light_at(3, 64, 7), Some((0, 0)));
        assert_eq!(spawner.light_at(3, 80, 7), Some((0, 0)));
        assert!(spawner.permits(monster, 3, 64, 7));
        spawner.set_environment(Dimension::Overworld, 0.0, 0.0);
        assert_eq!(spawner.light_at(3, 64, 7), Some((15, 0)));
    }

    #[test]
    fn spawn_dimension_block_light_controls_accept_dim_nether_emission_only() {
        let mut column = grass_column();
        column.set_block_id(3, 63, 7, Block::MagmaBlock.default_state());
        let world = std::sync::Arc::new(ChunkWorld::from_columns([((0, 0), column)]));
        let monster = spawn_rule("enderman").expect("monster rule");
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        spawner.set_environment(Dimension::Nether, 0.0, 0.0);
        spawner.begin_cycle(world, 1, Vec::new());
        assert_eq!(spawner.light_at(3, 64, 7), Some((0, 2)));
        assert!(spawner.permits(monster, 3, 64, 7));
        spawner.set_environment(Dimension::End, 0.0, 0.0);
        spawner.rng = SpawnRng::new(0);
        assert_eq!(spawner.light_at(3, 64, 7), Some((15, 2)));
        assert!(!spawner.permits(monster, 3, 64, 7));
    }

    #[test]
    fn generation_light_budget_defers_fifth_column_then_accepts_it() {
        let world = std::sync::Arc::new(ChunkWorld::from_columns(
            (0..5).map(|cx| ((cx, 0), grass_column())),
        ));
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        spawner.begin_cycle(std::sync::Arc::clone(&world), 1, Vec::new());
        for cx in 0..4 {
            let PlacementDecision::Accepted(candidate) =
                spawner.classify_generation_spawn(&cow_candidate(cx))
            else {
                panic!("grass and open daylight must accept column {cx}");
            };
            assert_eq!(candidate.pos, Vec3::new(f64::from(cx * 16) + 3.5, 64.0, 7.5));
        }
        assert!(matches!(
            spawner.classify_generation_spawn(&cow_candidate(4)),
            PlacementDecision::Deferred,
        ));
        spawner.begin_cycle(world, 2, Vec::new());
        assert!(matches!(
            spawner.classify_generation_spawn(&cow_candidate(4)),
            PlacementDecision::Accepted(_),
        ));
    }

    #[test]
    fn generation_missing_terrain_defers_but_solid_headroom_rejects() {
        let mut column = grass_column();
        column.set_block_id(3, 65, 7, Block::Stone.default_state());
        let world = std::sync::Arc::new(ChunkWorld::from_columns([((0, 0), column)]));
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        spawner.begin_cycle(world, 1, Vec::new());
        assert!(matches!(
            spawner.classify_generation_spawn(&cow_candidate(0)),
            PlacementDecision::Rejected,
        ));
        assert!(matches!(
            spawner.classify_generation_spawn(&cow_candidate(1)),
            PlacementDecision::Deferred,
        ));
        spawner.use_world(std::sync::Arc::new(ChunkWorld::from_columns([((1, 0), grass_column())])));
        assert!(matches!(
            spawner.classify_generation_spawn(&cow_candidate(1)),
            PlacementDecision::Accepted(_),
        ));
    }

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
                        .builtin_or_none()
                        .expect("bundled biome entity type is built-in")
                        .path();
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
        assert_eq!(cod.sea_band, (Some(-13), Some(0)));

        // A guardian appears in no bundled biome list, so it must be absent here
        // rather than fall back to "anywhere".
        assert!(spawn_rule("guardian").is_none());
    }

    /// The slime row carries both arms of its alternation. Its Y range is
    /// unbounded because the swamp arm and the slime-chunk arm apply their own
    /// ranges; a shared band would exclude one arm from every candidate.
    #[test]
    fn slime_carries_both_arms_not_one() {
        let slime = spawn_rule("slime").expect("registered");
        assert_eq!(slime.special, Special::Slime);
        assert_eq!(
            slime.sea_band,
            (None, None),
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
    /// record — the expected values come from vanilla's own dimension-type
    /// constant and moon timeline, not from this module.
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

    /// A column of water at block (3, 7), seabed at y 19, water through y 62 and
    /// open air above, in `biome`.
    fn water_column(biome: &str) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 80);
        for qy in 0..column.biome_y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    column.set_biome_cell(qx, qy, qz, biome);
                }
            }
        }
        column.set_block_id(3, 19, 7, Block::Stone.default_state());
        for y in 20..=62 {
            column.set_block_id(3, y, 7, Block::Water.default_state());
        }
        column
    }

    fn water_spawner(column: ChunkColumn) -> NaturalSpawner {
        let world = std::sync::Arc::new(ChunkWorld::from_columns([((0, 0), column)]));
        let mut spawner = NaturalSpawner::new(HashMap::new(), 0);
        spawner.begin_cycle(world, 1, Vec::new());
        spawner
    }

    /// The surface water animals need a plain water block directly above, not
    /// merely "no full solid". Expected values are the reference rule: fluid
    /// below, water block above, y in `[63 - 13, 63]`.
    #[test]
    fn surface_water_animals_need_water_above_and_the_surface_band() {
        let cod = spawn_rule("cod").expect("registered");
        let mut spawner = water_spawner(water_column("minecraft:ocean"));
        assert!(spawner.permits(cod, 3, 60, 7), "water below, water above, inside the band");
        assert!(spawner.permits(cod, 3, 50, 7), "the band's lower edge is 63 - 13");
        assert!(!spawner.permits(cod, 3, 49, 7), "one below the band");
        // y = 62 is the top water cell: air above it. Open air is not a full solid,
        // so a bare "above is not solid" test would accept it.
        assert!(!spawner.permits(cod, 3, 62, 7), "air above the position is not water");
        assert!(spawner.permits(spawn_rule("dolphin").expect("registered"), 3, 60, 7));
    }

    /// The nautilus lives deeper: `[63 - 25, 63 - 5]`, water below and above.
    #[test]
    fn nautilus_uses_its_own_depth_band() {
        let nautilus = spawn_rule("nautilus").expect("registered");
        let mut spawner = water_spawner(water_column("minecraft:ocean"));
        assert!(spawner.permits(nautilus, 3, 38, 7), "lower edge: 63 - 25");
        assert!(!spawner.permits(nautilus, 3, 37, 7), "one below the band");
        assert!(spawner.permits(nautilus, 3, 58, 7), "upper edge: 63 - 5");
        assert!(!spawner.permits(nautilus, 3, 59, 7), "one above the band");
        let cod = spawn_rule("cod").expect("registered");
        assert!(!spawner.permits(cod, 3, 40, 7), "a cod would not take the nautilus's depth");
    }

    /// Tropical fish ignore the height band only in the biomes that allow it.
    #[test]
    fn tropical_fish_ignore_the_band_only_in_lush_caves() {
        let fish = spawn_rule("tropical_fish").expect("registered");
        assert!(!water_spawner(water_column("minecraft:ocean")).permits(fish, 3, 30, 7));
        assert!(water_spawner(water_column("minecraft:ocean")).permits(fish, 3, 60, 7));
        assert!(water_spawner(water_column("minecraft:lush_caves")).permits(fish, 3, 30, 7));
    }

    /// Drowned: one in 15 in rivers with no depth gate; one in 40 and strictly
    /// below `63 - 5` elsewhere. Seeds 31 and 29 are SplitMix64 streams whose
    /// first word is `0 mod 15` (and not `0 mod 40`), and `0 mod 40` (and not
    /// `0 mod 15`), respectively: the words were computed outside this crate.
    #[test]
    fn drowned_dice_and_depth_depend_on_the_biome() {
        let permits = |biome: &str, seed: u64, y: i32| {
            let mut spawner = water_spawner(water_column(biome));
            spawner.rng = SpawnRng::new(seed);
            spawner.drowned_permits(3, y, 7)
        };
        // River: the 1/15 roll, at a depth that the ocean gate would refuse.
        assert!(permits("minecraft:river", 31, 60), "15-roll hit, shallow, river");
        assert!(permits("minecraft:frozen_river", 31, 60));
        assert!(!permits("minecraft:river", 29, 60), "a 40-roll hit is not a 15-roll hit");
        // Elsewhere: the 1/40 roll, and only below 58.
        assert!(!permits("minecraft:ocean", 31, 40), "a 15-roll hit is not a 40-roll hit");
        assert!(permits("minecraft:ocean", 29, 57), "40-roll hit, below sea level - 5");
        assert!(!permits("minecraft:ocean", 29, 58), "58 is not strictly below 58");
    }

    /// Rivers skip 98 percent of water-ambient picks. The first float draw of
    /// SplitMix64 seed 0 is word `E220A8397B1DCDAF` shifted right 40 over 2^24,
    /// about 0.883 (below 0.98: skipped); seed 44's is about 0.9815 (kept). The
    /// ocean control with the same seed 0 shows the skip is the river's.
    #[test]
    fn rivers_skip_most_water_ambient_picks() {
        let spawners = crate::worldgen_data::bundled_biome_spawners().clone();
        let pick = |biome: &str, seed: u64| {
            let world = std::sync::Arc::new(ChunkWorld::from_columns([((0, 0), water_column(biome))]));
            let mut spawner = NaturalSpawner::new(spawners.clone(), seed);
            spawner.begin_cycle(world, 1, Vec::new());
            spawner.rng = SpawnRng::new(seed);
            spawner.pick_species(MobCategory::WaterAmbient, 3, 60, 7).is_some()
        };
        assert!(pick("minecraft:ocean", 0), "control: the ocean list yields a fish for seed 0");
        assert!(!pick("minecraft:river", 0), "a river skips it");
        assert!(pick("minecraft:river", 44), "and keeps the roll at or above 0.98");
    }
}
