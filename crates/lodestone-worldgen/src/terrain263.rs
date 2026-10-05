//! The 26.3 Overworld: noise fill, surface, carvers, biomes and placed-feature decoration, with
//! every product expressed in the canonical block-state census.
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
use lodestone_worldgen_core::engine::release26_3::climate::{BiomeSource, ChunkBiomes, ClimateCursor, ClimateTree};
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;
pub use lodestone_worldgen_feature_26_3::blocks::State;
use lodestone_worldgen_feature_26_3::env::Env;
use lodestone_worldgen_feature_26_3::level::{ChunkData, Level};
use lodestone_worldgen_feature_26_3::registry::{Decorator, Features};

use crate::frontend26_3::{FrontendError, parse_state_key};

/// The Overworld dimension's lowest block Y.
pub const MIN_Y: i32 = -64;
/// The Overworld dimension's block height.
pub const HEIGHT: i32 = 384;
/// Shaped chunks kept for reuse. A decoration window reads nine of them and consecutive windows
/// share six, so this is a working set, not a world cache.
const SHAPED_CAPACITY: usize = 384;

/// Why a 26.3 generator could not be built.
#[derive(Debug)]
pub enum Terrain263Error {
    Engine(String),
    State(FrontendError),
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

/// The compiled 26.3 Overworld for one seed.
pub struct Terrain263 {
    seed: i64,
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

    /// Compiles the Overworld preset `settings` (`overworld`, `large_biomes` or `amplified`) for
    /// `seed`. All three share the Overworld's biome parameter list and dimension shape.
    ///
    /// # Errors
    /// If a bundled document fails to compile or a state has no canonical counterpart.
    pub fn with_settings(seed: i64, settings: &str) -> Result<Self, Terrain263Error> {
        let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS)
            .with_surface_data(data::MATERIAL_RULE, data::MATERIAL_CONDITION, data::BIOME);
        let generator = TerrainGenerator::load(&res, settings, seed).map_err(|e| Terrain263Error::Engine(format!("{e:?}")))?;
        let points = data::CLIMATE_POINTS
            .iter()
            .find(|(name, _)| *name == "overworld")
            .ok_or_else(|| Terrain263Error::Engine("no overworld climate points".into()))?
            .1;
        let tree = ClimateTree::from_json(points, &generator.biomes).map_err(Terrain263Error::Engine)?;
        let possible = possible_biomes(points, &generator)?;
        let source = BiomeSource::MultiNoise(tree);
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
        })
    }

    /// The world seed.
    #[must_use]
    pub fn seed(&self) -> i64 {
        self.seed
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
        let mut fill = g.fill_chunk(cx, cz, &mut ctx);
        let mut cursor = ClimateCursor::default();
        let mut climate_ctx = Ctx::uncached();
        let mut chunk = {
            let mut zoomed = |qx: i32, qy: i32, qz: i32| g.biome_at_quart(&self.source, &mut cursor, qx, qy, qz, &mut climate_ctx);
            g.build_surface(&fill, cx, cz, MIN_Y, HEIGHT, &mut zoomed, &mut ctx)
        };
        g.carve_chunk(&self.carvers, &self.source, &mut cursor, cx, cz, &mut fill, &mut chunk, &mut ctx);
        let carved: Vec<State> = chunk.states.iter().map(|&s| self.engine_to_feature[s as usize]).collect();
        let biomes = g.chunk_biomes(&self.source, &mut cursor, cx, cz);
        Shaped { chunk: ChunkData::new(&self.env, MIN_Y, HEIGHT, carved), biomes }
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

    /// Like [`Self::decorate_source_states`], but hands each placed feature's writes to `each` as
    /// `(step, index within the step, writes)` in placement order.
    pub fn decorate_source_reports(
        &self,
        source: (i32, i32),
        window: &mut dyn FnMut(i32, i32) -> Option<Vec<State>>,
        each: &mut dyn FnMut(usize, usize, &[(i32, i32, i32, State)]),
    ) {
        let (sx, sz) = source;
        let mut shaped = Vec::with_capacity(9);
        let mut chunks = Vec::with_capacity(9);
        let mut present = BTreeSet::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let base = self.shaped(sx + dx, sz + dz);
                let data = match window(sx + dx, sz + dz) {
                    Some(states) => base.chunk.with_states(&self.env, MIN_Y, HEIGHT, states),
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
            MIN_Y,
            HEIGHT,
            self.generator.sea_level,
            chunks,
            Box::new(biome_source),
        );
        self.decorator.decorate(&mut level, sx, sz, &present, None, |report| each(report.step, report.index, &report.changed));
        debug_assert_eq!(level.out_of_window, 0, "decoration touched columns outside its window");
    }
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
