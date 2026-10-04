//! Noise settings: loading a settings document into a compiled terrain generator,
//! and filling a chunk's density and fluid substance.

use std::collections::HashMap;

use serde_json::Value;

use super::aquifer::{Aquifer, AquiferFunctions, Fluid, FluidPicker};
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
        Self { density: load(density), noise: load(noise), settings: load(settings) }
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
            aquifer_factory,
        };
        Ok((generator, extra_ids))
    }

    /// Fills one chunk column's terrain shape.
    pub fn fill_chunk(&self, chunk_x: i32, chunk_z: i32, ctx: &mut Ctx) -> ChunkFill {
        let (x0, z0) = (chunk_x * 16, chunk_z * 16);
        let volume = Volume::new([16, self.height, 16], [x0, self.min_y, z0], [1, 1, 1]);
        let mut density = vec![0.0f32; volume.len()];
        self.program.volume(ctx, self.router.final_density, &mut density, &volume);

        let picker = FluidPicker::new(self.sea_level, self.default_fluid);
        let mut aquifer = self
            .aquifer
            .map(|f| Aquifer::new(&self.program, ctx, f, self.aquifer_factory, &volume, picker));
        let mut substance = vec![Substance::Default; volume.len()];
        let mut fluid_updates = Vec::new();
        for z in 0..16 {
            for x in 0..16 {
                for y in (0..self.height).rev() {
                    let idx = volume.index(x, y, z);
                    let d = f64::from(density[idx]);
                    let (bx, by, bz) = (x0 + x, self.min_y + y, z0 + z);
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
                    substance[idx] = match result {
                        None => Substance::Default,
                        Some(Fluid::Air) => Substance::Fluid(Fluid::Air),
                        Some(f) => Substance::Fluid(f),
                    };
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
