//! Noise settings: loading a settings document into a compiled terrain generator,
//! and filling a chunk's density and fluid substance.

use std::collections::HashMap;

use serde_json::Value;

use super::aquifer::{Aquifer, AquiferFunctions, Fluid, FluidPicker};
use super::biome::BiomeTable;
use super::material::{self, MaterialResources, MaterialSystem, StateTable, SurfaceNoises, SURFACE_NOISE_NAMES};
use super::compile::{Compiler, RandomConfig};
use super::sampler::{Ctx, Program, SId};
use super::tree::{Resources, Tree, TreeError};
use super::volume::Volume;
use crate::rng::{PositionalRandomFactory, RandomSource};

/// Parsed registry documents, keyed by resource path without the namespace.
#[derive(Default)]
pub struct ResourceSet {
    density: HashMap<String, Value>,
    noise: HashMap<String, Value>,
    settings: HashMap<String, Value>,
    material_rules: HashMap<String, Value>,
    material_conditions: HashMap<String, Value>,
    biomes: BiomeTable,
}

impl ResourceSet {
    /// Builds a set from `(name, json)` tables.
    ///
    /// # Panics
    /// If a bundled document is not valid JSON, which is a build-data defect.
    pub fn from_tables(
        density: &[(&str, &str)],
        noise: &[(&str, &str)],
        settings: &[(&str, &str)],
    ) -> Self {
        fn load(t: &[(&str, &str)]) -> HashMap<String, Value> {
            t.iter()
                .map(|(n, j)| ((*n).to_owned(), serde_json::from_str(j).expect("bundled worldgen JSON parses")))
                .collect()
        }
        Self {
            density: load(density),
            noise: load(noise),
            settings: load(settings),
            material_rules: HashMap::new(),
            material_conditions: HashMap::new(),
            biomes: BiomeTable::default(),
        }
    }

    /// Adds the surface-rule registries and the biome climate table.
    ///
    /// # Panics
    /// If a bundled document is not valid JSON, which is a build-data defect.
    #[must_use]
    pub fn with_surface_data(
        mut self,
        material_rules: &[(&str, &str)],
        material_conditions: &[(&str, &str)],
        biomes: &[(&str, &str)],
    ) -> Self {
        fn load(t: &[(&str, &str)]) -> HashMap<String, Value> {
            t.iter()
                .map(|(n, j)| ((*n).to_owned(), serde_json::from_str(j).expect("bundled worldgen JSON parses")))
                .collect()
        }
        self.material_rules = load(material_rules);
        self.material_conditions = load(material_conditions);
        self.biomes = BiomeTable::from_tables(biomes);
        self
    }

    pub fn settings(&self, name: &str) -> Option<&Value> {
        self.settings.get(strip(name))
    }
}

fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

impl Resources for ResourceSet {
    fn density_function(&self, name: &str) -> Option<&Value> {
        self.density.get(strip(name))
    }
    fn noise(&self, name: &str) -> Option<&Value> {
        self.noise.get(strip(name))
    }
}

impl MaterialResources for ResourceSet {
    fn material_rule(&self, name: &str) -> Option<&Value> {
        self.material_rules.get(strip(name))
    }
    fn material_condition(&self, name: &str) -> Option<&Value> {
        self.material_conditions.get(strip(name))
    }
}

#[derive(Debug)]
pub enum SettingsError {
    Missing(String),
    Malformed(&'static str),
    Tree(TreeError),
}

impl From<TreeError> for SettingsError {
    fn from(e: TreeError) -> Self {
        Self::Tree(e)
    }
}

/// The eight router roots the terrain stage reads.
#[derive(Clone, Copy, Debug)]
pub struct Router {
    pub temperature: SId,
    pub vegetation: SId,
    pub continents: SId,
    pub erosion: SId,
    pub depth: SId,
    pub ridges: SId,
    pub chunk_surface_level: SId,
    pub final_density: SId,
}

/// A compiled noise-settings document bound to a world seed.
pub struct TerrainGenerator {
    pub program: Program,
    pub router: Router,
    pub aquifer: Option<AquiferFunctions>,
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
    pub default_fluid: Fluid,
    pub default_block: String,
    pub biomes: BiomeTable,
    pub(crate) zoom_seed: i64,
    pub(crate) material: Option<MaterialSystem>,
    aquifer_factory: crate::rng::AnyPositionalFactory,
}

fn fluid_of(name: &str) -> Option<Fluid> {
    match strip(name) {
        "air" => Some(Fluid::Air),
        "water" => Some(Fluid::Water),
        "lava" => Some(Fluid::Lava),
        _ => None,
    }
}

impl TerrainGenerator {
    pub fn load(res: &ResourceSet, settings: &str, seed: i64) -> Result<Self, SettingsError> {
        Self::load_with(res, settings, seed, &[]).map(|(g, _)| g)
    }

    /// Like [`load`](Self::load), additionally compiling the named registry
    /// density functions, returned in order. Used by oracle comparisons.
    pub fn load_with(
        res: &ResourceSet,
        settings: &str,
        seed: i64,
        extra: &[&str],
    ) -> Result<(Self, Vec<SId>), SettingsError> {
        let doc = res.settings(settings).ok_or_else(|| SettingsError::Missing(settings.to_owned()))?;
        let legacy = doc.get("legacy_random_source").and_then(Value::as_bool).unwrap_or(false);
        let noise = doc.get("noise").ok_or(SettingsError::Malformed("noise"))?;
        let min_y = noise.get("min_y").and_then(Value::as_i64).ok_or(SettingsError::Malformed("noise.min_y"))? as i32;
        let height = noise.get("height").and_then(Value::as_i64).ok_or(SettingsError::Malformed("noise.height"))? as i32;
        let sea_level = doc.get("sea_level").and_then(Value::as_i64).ok_or(SettingsError::Malformed("sea_level"))? as i32;
        let default_fluid = doc
            .get("default_fluid")
            .and_then(|v| v.as_str().or_else(|| v.get("Name").and_then(Value::as_str)))
            .and_then(fluid_of)
            .ok_or(SettingsError::Malformed("default_fluid"))?;
        let default_block = doc
            .get("default_block")
            .and_then(|v| v.as_str().or_else(|| v.get("Name").and_then(Value::as_str)))
            .ok_or(SettingsError::Malformed("default_block"))?
            .to_owned();
        let router_doc = doc.get("noise_router").ok_or(SettingsError::Malformed("noise_router"))?;

        let mut tree = Tree::new();
        let extra_nodes = extra
            .iter()
            .map(|n| tree.reference(n, res))
            .collect::<Result<Vec<_>, _>>()?;
        let mut parse = |v: &Value| tree.parse(v, res);
        let field = |name: &'static str| router_doc.get(name).ok_or(SettingsError::Malformed(name));
        let roots = [
            parse(field("temperature")?)?,
            parse(field("vegetation")?)?,
            parse(field("continents")?)?,
            parse(field("erosion")?)?,
            parse(field("depth")?)?,
            parse(field("ridges")?)?,
            parse(field("chunk_surface_level")?)?,
            parse(field("final_density")?)?,
        ];
        let aq_roots = match doc.get("aquifers") {
            Some(a) => {
                let f = |name: &'static str| a.get(name).ok_or(SettingsError::Malformed(name));
                Some([
                    parse(f("barrier")?)?,
                    parse(f("fluid_level_floodedness")?)?,
                    parse(f("fluid_level_spread")?)?,
                    parse(f("lava")?)?,
                    parse(f("exclusion")?)?,
                    parse(f("surface_level")?)?,
                ])
            }
            None => None,
        };

        let mut states = StateTable::default();
        // Terrain-only resource sets carry no surface registries; the material
        // system is then absent and only the shape fill is available.
        let parsed = match doc.get("material_rule").filter(|_| !res.material_rules.is_empty()) {
            Some(v) => {
                for name in SURFACE_NOISE_NAMES {
                    tree.load_noise(name, res)?;
                }
                Some(material::parse_rule(v, res, &res.biomes, &mut tree, &mut states)?)
            }
            None => None,
        };
        let default_state_key = material::state_key(doc.get("default_block").ok_or(SettingsError::Malformed("default_block"))?)?;

        let mut compiler = Compiler::new(tree, RandomConfig { seed, legacy });
        let r: Vec<SId> = roots.iter().map(|&n| compiler.sampler(n)).collect();
        let aquifer = aq_roots.map(|a| {
            let s: Vec<SId> = a.iter().map(|&n| compiler.sampler(n)).collect();
            AquiferFunctions {
                barrier: s[0],
                fluid_level_floodedness: s[1],
                fluid_level_spread: s[2],
                lava: s[3],
                exclusion: s[4],
                surface_level: s[5],
            }
        });
        let extra_ids: Vec<SId> = extra_nodes.iter().map(|&n| compiler.sampler(n)).collect();
        let factory = compiler.factory();
        let material = match parsed {
            Some(parsed) => {
                let mut noise_for = |name: &str| {
                    let id = compiler.tree.name_id(name);
                    compiler.named_noise(id)
                };
                let noise_map: Vec<usize> = parsed.noise_names.iter().map(|n| noise_for(n)).collect();
                let n: Vec<usize> = SURFACE_NOISE_NAMES.iter().map(|n| noise_for(n)).collect();
                let noises = SurfaceNoises {
                    surface: n[0],
                    surface_secondary: n[1],
                    clay_bands_offset: n[2],
                    badlands_pillar: n[3],
                    badlands_pillar_roof: n[4],
                    badlands_surface: n[5],
                    iceberg_pillar: n[6],
                    iceberg_pillar_roof: n[7],
                    iceberg_surface: n[8],
                };
                let ore_functions: Vec<[SId; 3]> = parsed
                    .ore_nodes
                    .iter()
                    .map(|[a, b, c]| [compiler.sampler(*a), compiler.sampler(*b), compiler.sampler(*c)])
                    .collect();
                let randoms = parsed
                    .random_names
                    .iter()
                    .map(|name| factory.from_hash_of(name).fork_positional())
                    .collect();
                let clay_bands = material::generate_bands(&mut factory.from_hash_of("minecraft:clay_bands"), &mut states);
                let default_block = states.intern(&default_state_key);
                let air = states.intern("minecraft:air");
                let water = states.intern("minecraft:water");
                let lava = states.intern("minecraft:lava");
                let snow_block = states.intern("minecraft:snow_block");
                let packed_ice = states.intern("minecraft:packed_ice");
                Some(MaterialSystem {
                    rule: parsed.rule.expect("parse_rule yields a rule"),
                    states,
                    noise_map,
                    noise_slots: parsed.noise_slots,
                    randoms,
                    ore_functions,
                    noises,
                    noise_random: factory,
                    clay_bands,
                    ore_random: factory.from_hash_of("minecraft:ore").fork_positional(),
                    default_block,
                    air,
                    water,
                    lava,
                    snow_block,
                    packed_ice,
                    sea_level,
                    preliminary_surface: r[6],
                    eroded_badlands: res.biomes.id("eroded_badlands"),
                    frozen_ocean: res.biomes.id("frozen_ocean"),
                    deep_frozen_ocean: res.biomes.id("deep_frozen_ocean"),
                })
            }
            None => None,
        };
        let aquifer_factory = factory.from_hash_of("minecraft:aquifer").fork_positional();
        let program = compiler.finish();
        let generator = Self {
            program,
            router: Router {
                temperature: r[0],
                vegetation: r[1],
                continents: r[2],
                erosion: r[3],
                depth: r[4],
                ridges: r[5],
                chunk_surface_level: r[6],
                final_density: r[7],
            },
            aquifer,
            min_y,
            height,
            sea_level,
            default_fluid,
            default_block,
            biomes: res.biomes.clone(),
            zoom_seed: super::biome::obfuscate_seed(seed),
            material,
            aquifer_factory,
        };
        Ok((generator, extra_ids))
    }

    /// The engine state id of the dimension's default (solid) block.
    pub fn default_block_state_id(&self) -> Option<super::material::StateId> {
        self.material.as_ref().map(|m| m.default_block)
    }

    /// How many distinct block states the material rules can produce; valid
    /// state ids are `0..state_count()`.
    pub fn state_count(&self) -> usize {
        self.material.as_ref().map_or(0, |m| m.states.len())
    }

    /// The data-written key (`name[sorted=props]`) of a block state a surface
    /// build produced; empty when the generator has no material rules.
    pub fn state_key(&self, id: super::material::StateId) -> &str {
        self.material.as_ref().map_or("", |m| m.states.key(id))
    }

    /// Fills one chunk's terrain shape.
    pub fn fill_chunk(&self, chunk_x: i32, chunk_z: i32, ctx: &mut Ctx) -> ChunkFill {
        let volume = Volume::new([16, self.height, 16], [chunk_x * 16, self.min_y, chunk_z * 16], [1, 1, 1]);
        self.fill_volume(volume, ctx)
    }

    /// The terrain shape of a single block column, as used for base-height
    /// queries. Cells are indexed from the bottom of the world upwards. The
    /// aquifer is built over this one-column volume, so its results can differ
    /// from the same column inside a chunk fill.
    pub fn fill_column(&self, block_x: i32, block_z: i32, ctx: &mut Ctx) -> Vec<Substance> {
        let volume = Volume::new([1, self.height, 1], [block_x, self.min_y, block_z], [1, 1, 1]);
        self.fill_volume(volume, ctx).substance
    }

    fn fill_volume(&self, volume: Volume, ctx: &mut Ctx) -> ChunkFill {
        let mut density = vec![0.0f32; volume.len()];
        self.program.volume(ctx, self.router.final_density, &mut density, &volume);

        let picker = FluidPicker::new(self.sea_level, self.default_fluid);
        let mut aquifer = self
            .aquifer
            .map(|f| Aquifer::new(&self.program, ctx, f, self.aquifer_factory, &volume, picker));
        let mut substance = vec![Substance::Default; volume.len()];
        let mut fluid_updates = Vec::new();
        for z in 0..volume.size[2] {
            for x in 0..volume.size[0] {
                for y in (0..volume.size[1]).rev() {
                    let idx = volume.index(x, y, z);
                    let d = f64::from(density[idx]);
                    let (bx, by, bz) = (volume.block_x(x), volume.block_y(y), volume.block_z(z));
                    let result = match aquifer.as_mut() {
                        Some(a) => {
                            let r = a.compute_substance(&self.program, ctx, bx, by, bz, d);
                            if a.should_schedule_fluid_update() {
                                fluid_updates.push(idx as u32);
                            }
                            r
                        }
                        None => (d <= 0.0).then(|| picker.compute(bx, by, bz).at(by)),
                    };
                    substance[idx] = result.map_or(Substance::Default, Substance::Fluid);
                }
            }
        }
        ChunkFill { volume, density, substance, fluid_updates }
    }
}

/// What a cell holds before surface rules: the default block, or a fluid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Substance {
    Default,
    Fluid(Fluid),
}

#[derive(Debug)]
pub struct ChunkFill {
    pub volume: Volume,
    pub density: Vec<f32>,
    pub substance: Vec<Substance>,
    /// Indices into the volume whose fluid needs an update scheduled.
    pub fluid_updates: Vec<u32>,
}

impl std::fmt::Debug for ResourceSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceSet").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for TerrainGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerrainGenerator").finish_non_exhaustive()
    }
}
