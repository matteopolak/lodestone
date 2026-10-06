//! 26.3 terrain for any dimension: noise fill, surface, carvers, biomes and placed-feature
//! decoration, with every product expressed in the canonical block-state census.
//!
//! [`Terrain263`] is the production generator for a 26.3 world. It owns the compiled engine
//! (`lodestone_worldgen_core::engine::release26_3`), the placed-feature decorator
//! (`lodestone_worldgen_feature_26_3`) and the two block-state bridges between the decorator's
//! state table and the canonical census. It holds no lifecycle policy: callers ask for a chunk's
//! shaped terrain ([`Terrain263::shaped`]) and for the writes one chunk's decoration makes over a
//! 3x3 window ([`Terrain263::decorate_source`]), and decide ordering and visibility themselves.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use lodestone_data::block_states::{STATE_COUNT, StateId};
use lodestone_worldgen_core::engine::release26_3::biome::{BiomeId, obfuscate_seed};
use lodestone_worldgen_core::engine::release26_3::carver::CarverTable;
use lodestone_worldgen_core::engine::release26_3::climate::{BiomeSource, ChunkBiomes, ClimateCursor, ClimateTree, EndBiomes};
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;
pub use lodestone_worldgen_feature_26_3::blocks::State;
use lodestone_worldgen_feature_26_3::env::Env;
pub use lodestone_worldgen_feature_26_3::feature::end::{EndSpike, spikes_for_seed as end_spikes_for_seed};
pub use lodestone_worldgen_feature_26_3::level::PlacedBlockEntity;
use lodestone_worldgen_feature_26_3::level::{ChunkData, Level};
use lodestone_worldgen_feature_26_3::registry::{Decorator, Features};

mod state_key;
mod structures;

pub use state_key::StateKeyError;

use state_key::parse_state_key;
use crate::structure::CodedLoot;

/// The source chunks whose decoration can write into a chunk, as offsets from it, in the order
/// their writes apply: x-major over the 3x3 neighbourhood. Overlapping writes resolve
/// last-write-wins, so a caller composing a chunk from its neighbours' decoration must keep this
/// order.
pub const DECORATION_SOURCE_OFFSETS: [(i32, i32); 9] =
    [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 0), (0, 1), (1, -1), (1, 0), (1, 1)];

/// Shaped chunks kept for reuse. A decoration window reads nine of them and consecutive windows
/// share six, so this is a working set, not a world cache.
const SHAPED_CAPACITY: usize = 384;

/// Why a 26.3 generator could not be built.
#[derive(Debug)]
pub enum Terrain263Error {
    Engine(String),
    State(StateKeyError),
    Features(String),
}

impl std::fmt::Display for Terrain263Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(e) => write!(f, "26.3 engine: {e}"),
            Self::State(e) => write!(f, "26.3 block state: {e}"),
            Self::Features(e) => write!(f, "26.3 features: {e}"),
        }
    }
}

impl std::error::Error for Terrain263Error {}

/// One chunk after noise fill, surface rules and carving, in the decorator's state space.
#[derive(Debug)]
pub struct Shaped {
    /// Blocks and heightmaps; carving updates the world-generation heightmaps, which freeze at
    /// this point.
    pub chunk: ChunkData,
    /// The chunk's raw 4x4x(height/4) biome cells.
    pub biomes: ChunkBiomes,
}

/// A decoration write: absolute position and the canonical state it leaves there.
pub type Write = (i32, i32, i32, StateId);

/// The compiled 26.3 generator of one dimension for one seed.
pub struct Terrain263 {
    seed: i64,
    /// The dimension's build range, which a chunk spans; the noise may cover less of it (the
    /// Nether's and the End's noise stops at 128).
    min_y: i32,
    height: i32,
    /// The sky light a column still being generated reads (zero in the Nether).
    sky_light: i32,
    generator: TerrainGenerator,
    source: BiomeSource,
    carvers: CarverTable,
    env: Env,
    decorator: Decorator,
    /// Engine state id -> decorator state.
    engine_to_feature: Vec<State>,
    /// Decorator state -> canonical state.
    canon_of_feature: Vec<StateId>,
    /// Canonical raw state id -> decorator state, `u16::MAX` when the 26.3 table lacks it.
    feature_of_canon: Vec<u16>,
    /// Decorator states with no canonical counterpart (a table mismatch, expected to be zero).
    unmapped_states: usize,
    shaped: Mutex<ShapedCache>,
    /// Structure registry and caches; `None` until [`Terrain263::with_structures`].
    structures: Option<structures::Structures263>,
    /// Names of the biomes the climate list can produce.
    possible_names: std::collections::HashSet<String>,
}

#[derive(Default)]
struct ShapedCache {
    entries: HashMap<(i32, i32), Arc<Shaped>>,
    order: VecDeque<(i32, i32)>,
}

impl std::fmt::Debug for Terrain263 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terrain263").field("seed", &self.seed).finish_non_exhaustive()
    }
}

impl Terrain263 {
    /// Compiles the 26.3 Overworld for `seed` from the bundled data.
    ///
    /// # Errors
    /// If a bundled document fails to compile or a state has no canonical counterpart.
    pub fn new(seed: i64) -> Result<Self, Terrain263Error> {
        Self::with_settings(seed, "overworld")
    }

    /// Compiles the noise settings `settings` for `seed`: an Overworld preset (`overworld`,
    /// `large_biomes` or `amplified`, which share the Overworld's biome list and build range),
    /// `nether` or `end`.
    ///
    /// # Errors
    /// If a bundled document fails to compile or a state has no canonical counterpart.
    pub fn with_settings(seed: i64, settings: &str) -> Result<Self, Terrain263Error> {
        Self::build(seed, settings, None)
    }

    /// The Overworld with one biome everywhere (`minecraft:plains`, ...), the single-biome world
    /// preset: the Overworld's noise and build range over a fixed biome source.
    ///
    /// # Errors
    /// If `biome` is unknown, or as [`Self::with_settings`].
    pub fn with_fixed_biome(seed: i64, biome: &str) -> Result<Self, Terrain263Error> {
        Self::build(seed, "overworld", Some(biome))
    }

    fn build(seed: i64, settings: &str, fixed: Option<&str>) -> Result<Self, Terrain263Error> {
        let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS)
            .with_surface_data(data::MATERIAL_RULE, data::MATERIAL_CONDITION, data::BIOME);
        let generator = TerrainGenerator::load(&res, settings, seed).map_err(|e| Terrain263Error::Engine(format!("{e:?}")))?;
        let climate = |name: &str| {
            data::CLIMATE_POINTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, points)| *points)
                .ok_or_else(|| Terrain263Error::Engine(format!("no {name} climate points")))
        };
        let ((min_y, height, sky_light), source, possible) = match (settings, fixed) {
            (_, Some(name)) => {
                let id = generator.biomes.id(name).ok_or_else(|| Terrain263Error::Engine(format!("unknown biome {name}")))?;
                ((-64, 384, 15), BiomeSource::Fixed(id), vec![id])
            }
            ("nether", None) => {
                let points = climate("nether")?;
                let tree = ClimateTree::from_json(points, &generator.biomes).map_err(Terrain263Error::Engine)?;
                ((0, 256, 0), BiomeSource::MultiNoise(tree), possible_biomes(points, &generator)?)
            }
            ("end", None) => {
                let biomes = EndBiomes::from_table(&generator.biomes).map_err(Terrain263Error::Engine)?;
                let possible = vec![biomes.end, biomes.highlands, biomes.midlands, biomes.small_islands, biomes.barrens];
                ((0, 256, 15), BiomeSource::End(biomes), possible)
            }
            (_, None) => {
                let points = climate("overworld")?;
                let tree = ClimateTree::from_json(points, &generator.biomes).map_err(Terrain263Error::Engine)?;
                ((-64, 384, 15), BiomeSource::MultiNoise(tree), possible_biomes(points, &generator)?)
            }
        };
        let possible_names = possible.iter().map(|&id| generator.biomes.info(id).name.clone()).collect();
        let carvers = CarverTable::from_tables(data::CARVER).map_err(|e| Terrain263Error::Engine(format!("{e:?}")))?;
        let env = Env::load();
        let features = Features::load(&env).map_err(Terrain263Error::Features)?;
        let decorator = Decorator::new(features, &generator.biomes, &possible).map_err(Terrain263Error::Features)?;

        let engine_to_feature = (0..generator.state_count())
            .map(|id| {
                env.blocks
                    .state_by_name(generator.state_key(id as _))
                    .map_err(|e| Terrain263Error::Engine(format!("engine state {id}: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut canon_of_feature = Vec::with_capacity(env.blocks.state_count());
        let mut feature_of_canon = vec![u16::MAX; STATE_COUNT as usize];
        let mut unmapped_states = 0;
        for state in 0..env.blocks.state_count() {
            let feature = state as State;
            match parse_state_key(&env.blocks.full_key(feature)) {
                Ok(canon) => {
                    canon_of_feature.push(canon);
                    let slot = &mut feature_of_canon[canon.raw() as usize];
                    if *slot == u16::MAX {
                        *slot = feature;
                    }
                }
                Err(_) => {
                    canon_of_feature.push(StateId::AIR);
                    unmapped_states += 1;
                }
            }
        }
        Ok(Self {
            seed,
            min_y,
            height,
            sky_light,
            generator,
            source,
            carvers,
            env,
            decorator,
            engine_to_feature,
            canon_of_feature,
            feature_of_canon,
            unmapped_states,
            shaped: Mutex::new(ShapedCache::default()),
            structures: None,
            possible_names,
        })
    }

    /// Enables structures: the registry is built from `resolver`'s structure sets, restricted to
    /// the biomes this Overworld can produce.
    #[must_use]
    pub fn with_structures(mut self, resolver: &dyn crate::density::Resolver) -> Self {
        self.structures = structures::Structures263::new(self.seed, resolver, &self.possible_names);
        self
    }

    /// The world seed.
    #[must_use]
    pub fn seed(&self) -> i64 {
        self.seed
    }

    /// The lowest block `y` of the dimension.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// The dimension's block height, which every chunk spans.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// The sea level the noise settings declare.
    #[must_use]
    pub fn sea_level(&self) -> i32 {
        self.generator.sea_level
    }

    /// The decorator's block table and tags.
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The compiled terrain engine (noise router, surface rules, biome table).
    #[must_use]
    pub fn generator(&self) -> &TerrainGenerator {
        &self.generator
    }

    /// How many decorator states have no canonical counterpart; zero for the bundled data.
    #[must_use]
    pub fn unmapped_states(&self) -> usize {
        self.unmapped_states
    }

    /// The canonical state a decorator state stands for.
    #[must_use]
    pub fn canonical(&self, state: State) -> StateId {
        self.canon_of_feature[state as usize]
    }

    /// The decorator state for a canonical one; air when the 26.3 table does not have it.
    #[must_use]
    pub fn feature_state(&self, state: StateId) -> State {
        match self.feature_of_canon[state.raw() as usize] {
            u16::MAX => self.env.known.air,
            s => s,
        }
    }

    /// The placed-feature decorator, for ordering diagnostics.
    #[must_use]
    pub fn decorator(&self) -> &Decorator {
        &self.decorator
    }

    /// The canonical name (`minecraft:plains`) of a biome.
    #[must_use]
    pub fn biome_name(&self, biome: BiomeId) -> &str {
        &self.generator.biomes.info(biome).name
    }

    /// Fill, surface, carvers and biomes for one chunk. Deterministic, cached.
    pub fn shaped(&self, cx: i32, cz: i32) -> Arc<Shaped> {
        if let Some(hit) = self.shaped.lock().expect("shaped cache lock poisoned").entries.get(&(cx, cz)) {
            return Arc::clone(hit);
        }
        let built = Arc::new(self.build_shaped(cx, cz));
        let mut cache = self.shaped.lock().expect("shaped cache lock poisoned");
        if cache.entries.insert((cx, cz), Arc::clone(&built)).is_none() {
            cache.order.push_back((cx, cz));
            while cache.order.len() > SHAPED_CAPACITY {
                if let Some(old) = cache.order.pop_front() {
                    cache.entries.remove(&old);
                }
            }
        }
        built
    }

    fn build_shaped(&self, cx: i32, cz: i32) -> Shaped {
        let g = &self.generator;
        let mut ctx = Ctx::new(&g.program);
        ctx.beardifier = self.beardifier_for(cx, cz);
        let mut fill = g.fill_chunk(cx, cz, &mut ctx);
        let mut cursor = ClimateCursor::default();
        let mut climate_ctx = Ctx::uncached();
        let mut chunk = {
            // The surface rules ask for the biome at every block they inspect; one quart cell
            // answers sixty-four of them, so each cell is sampled once.
            let mut memo: std::collections::HashMap<(i32, i32, i32), BiomeId> = std::collections::HashMap::new();
            let mut zoomed = |qx: i32, qy: i32, qz: i32| {
                *memo.entry((qx, qy, qz)).or_insert_with(|| g.biome_at_quart(&self.source, &mut cursor, qx, qy, qz, &mut climate_ctx))
            };
            g.build_surface(&fill, cx, cz, self.min_y, self.height, &mut zoomed, &mut ctx)
        };
        g.carve_chunk(&self.carvers, &self.source, &mut cursor, cx, cz, &mut fill, &mut chunk, &mut ctx);
        let carved: Vec<State> = chunk.states.iter().map(|&s| self.engine_to_feature[s as usize]).collect();
        let biomes = self.full_height_biomes(g.chunk_biomes(&self.source, &mut cursor, cx, cz), cx, cz, &mut cursor, &mut climate_ctx);
        Shaped { chunk: ChunkData::new(&self.env, self.min_y, self.height, carved), biomes }
    }

    /// Extends the noise range's biome cells to the dimension's whole height: a chunk stores a
    /// biome for every quart of its sections, and the quarts above a Nether or End noise range
    /// take the biome query's answer at their own height.
    fn full_height_biomes(&self, noise: ChunkBiomes, cx: i32, cz: i32, cursor: &mut ClimateCursor, ctx: &mut Ctx) -> ChunkBiomes {
        let (min_quart_y, quarts_y) = (self.min_y >> 2, self.height >> 2);
        if noise.min_quart_y == min_quart_y && noise.quarts_y == quarts_y {
            return noise;
        }
        let g = &self.generator;
        let mut ids = Vec::with_capacity((16 * quarts_y) as usize);
        for z in 0..4 {
            for x in 0..4 {
                for qy in min_quart_y..min_quart_y + quarts_y {
                    let inside = (noise.min_quart_y..noise.min_quart_y + noise.quarts_y).contains(&qy);
                    ids.push(if inside { noise.get(x, qy, z) } else { g.biome_at_quart(&self.source, cursor, cx * 4 + x, qy, cz * 4 + z, ctx) });
                }
            }
        }
        ChunkBiomes { min_quart_y, quarts_y, ids }
    }

    /// The terrain's estimated surface height at block `(x, z)`: the floor of the noise router's
    /// chunk-surface-level function, read without generating a chunk.
    #[must_use]
    pub fn preliminary_surface_level(&self, x: i32, z: i32) -> i32 {
        let program = &self.generator.program;
        program.value(&mut Ctx::new(program), self.generator.router.chunk_surface_level, x, 0, z).floor() as i32
    }

    /// The decoration of chunk `source` over the 3x3 window around it, as an ordered list of
    /// writes. `window` supplies a chunk's current blocks (decorator state layout, `y + (x + z *
    /// 16) * height`) when the caller holds a version of it other than the freshly shaped one;
    /// `None` reads the shaped terrain.
    ///
    /// Heightmaps are rebuilt from whatever blocks the window supplies, with the
    /// world-generation maps kept from the shaped chunk.
    pub fn decorate_source(&self, source: (i32, i32), window: &mut dyn FnMut(i32, i32) -> Option<Vec<State>>) -> Vec<Write> {
        self.decorate_source_states(source, window)
            .into_iter()
            .map(|(x, y, z, state)| (x, y, z, self.canon_of_feature[state as usize]))
            .collect()
    }

    /// [`Self::decorate_source`] with the writes left in the decorator's own state space, for
    /// callers that feed them straight back into [`Self::decorate_source`] windows.
    pub fn decorate_source_states(&self, source: (i32, i32), window: &mut dyn FnMut(i32, i32) -> Option<Vec<State>>) -> Vec<(i32, i32, i32, State)> {
        let mut writes = Vec::new();
        self.decorate_source_reports(source, window, &mut |_, _, changed| writes.extend_from_slice(changed));
        writes
    }

    /// [`Self::decorate_source_states`] plus the block entities and loot containers the source
    /// attached, in placement order.
    pub fn decorate_source_full(&self, source: (i32, i32), window: &mut dyn FnMut(i32, i32) -> Option<Vec<State>>) -> SourceDecoration {
        let mut writes = Vec::new();
        let (entities, loot) = self.decorate_source_reports(source, window, &mut |_, _, changed| writes.extend_from_slice(changed));
        SourceDecoration { writes, entities, loot }
    }

    /// Like [`Self::decorate_source_states`], but hands each placed feature's writes to `each` as
    /// `(step, index within the step, writes)` in placement order.
    pub fn decorate_source_reports(
        &self,
        source: (i32, i32),
        window: &mut dyn FnMut(i32, i32) -> Option<Vec<State>>,
        each: &mut dyn FnMut(usize, usize, &[(i32, i32, i32, State)]),
    ) -> (Vec<PlacedBlockEntity>, Vec<CodedLoot>) {
        let (sx, sz) = source;
        let mut shaped = Vec::with_capacity(9);
        let mut chunks = Vec::with_capacity(9);
        let mut present = BTreeSet::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let base = self.shaped(sx + dx, sz + dz);
                let data = match window(sx + dx, sz + dz) {
                    Some(states) => base.chunk.with_states(&self.env, self.min_y, self.height, states),
                    None => base.chunk.clone(),
                };
                present.extend(base.biomes.ids.iter().copied());
                chunks.push(data);
                shaped.push(base);
            }
        }
        let present: Vec<BiomeId> = present.into_iter().collect();
        let (min_quart, quarts_y) = (shaped[0].biomes.min_quart_y, shaped[0].biomes.quarts_y);
        let biome_source = {
            let shaped = shaped.clone();
            move |qx: i32, qy: i32, qz: i32| {
                let qx = qx.clamp((sx - 1) * 4, (sx + 2) * 4 - 1);
                let qz = qz.clamp((sz - 1) * 4, (sz + 2) * 4 - 1);
                let chunk = &shaped[(((qz >> 2) - (sz - 1)) * 3 + ((qx >> 2) - (sx - 1))) as usize];
                chunk.biomes.get(qx & 3, qy.clamp(min_quart, min_quart + quarts_y - 1), qz & 3)
            }
        };
        let mut level = Level::new(
            &self.env,
            self.seed,
            obfuscate_seed(self.seed),
            sx,
            sz,
            self.min_y,
            self.height,
            self.generator.sea_level,
            chunks,
            Box::new(biome_source),
        );
        level.gen_min_y = self.generator.min_y;
        level.gen_depth = self.generator.height;
        level.sky_light = self.sky_light;
        let mut loot = Vec::new();
        let mut place = |level: &Level<'_>, step: usize| -> Vec<(i32, i32, i32, State)> {
            let step = step as i32;
            if !self.has_structures_in_step(sx, sz, step) {
                return Vec::new();
            }
            let mut read = |x: i32, y: i32, z: i32| self.canonical(level.get(x, y, z));
            let (writes, mut placed_loot) = self.place_structures(sx, sz, step, &mut read);
            loot.append(&mut placed_loot);
            writes.into_iter().map(|(x, y, z, state)| (x, y, z, self.feature_state(state))).collect()
        };
        self.decorator.decorate_with_structures(&mut level, sx, sz, &present, None, &mut place, |report| each(report.step, report.index, &report.changed));
        debug_assert_eq!(level.out_of_window, 0, "decoration touched columns outside its window");
        (level.take_block_entities(), loot)
    }
}

/// One source chunk's decoration: its writes in the decorator's state space, the block entities
/// its features attached, and the loot containers its structure placement seeded. An entity or
/// container is only meaningful while the final block at its position is still its own block.
#[derive(Debug)]
pub struct SourceDecoration {
    pub writes: Vec<(i32, i32, i32, State)>,
    pub entities: Vec<PlacedBlockEntity>,
    pub loot: Vec<CodedLoot>,
}

/// The climate list's biomes, distinct, in the list's own order.
fn possible_biomes(points: &str, generator: &TerrainGenerator) -> Result<Vec<BiomeId>, Terrain263Error> {
    let rows: serde_json::Value = serde_json::from_str(points).map_err(|e| Terrain263Error::Engine(e.to_string()))?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in rows.as_array().ok_or_else(|| Terrain263Error::Engine("climate list is not an array".into()))? {
        let name = row.get(13).and_then(serde_json::Value::as_str).ok_or_else(|| Terrain263Error::Engine("climate row without a biome".into()))?;
        let id = generator.biomes.id(name).ok_or_else(|| Terrain263Error::Engine(format!("unknown biome {name}")))?;
        if seen.insert(id) {
            out.push(id);
        }
    }
    Ok(out)
}
