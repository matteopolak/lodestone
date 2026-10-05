//! Shared harness: builds the 3x3 terrain neighbourhood with the 26.3 engine over the
//! synthetic quart-resolution biome layout of `scripts/worldgen-oracle-26-3` (`pick` below is
//! mirrored in `DecorationOracle263`) and decorates the centre chunk.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::OnceLock;

use lodestone_worldgen_core::engine::release26_3::biome::{BiomeId, obfuscate_seed};
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;
use lodestone_worldgen_feature_26_3::blocks::State;
use lodestone_worldgen_feature_26_3::env::Env;
use lodestone_worldgen_feature_26_3::level::{ChunkData, Level};
use lodestone_worldgen_feature_26_3::registry::{Decorator, Features};

pub const SURFACE_BIOMES: &str = include_str!("../../../../scripts/worldgen-oracle-26-3/surface-biomes.txt");

pub fn env() -> &'static Env {
    static ENV: OnceLock<Env> = OnceLock::new();
    ENV.get_or_init(Env::load)
}

pub fn pick(qx: i32, qy: i32, qz: i32, n: usize) -> usize {
    let mut h = (qx >> 1).wrapping_mul(73_856_093) ^ (qy >> 3).wrapping_mul(19_349_663) ^ (qz >> 1).wrapping_mul(83_492_791);
    h ^= ((h as u32) >> 13) as i32;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= ((h as u32) >> 15) as i32;
    h.rem_euclid(n as i32) as usize
}

pub fn fnv_bytes(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

pub fn generator(settings: &str, seed: i64) -> TerrainGenerator {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS).with_surface_data(
        data::MATERIAL_RULE,
        data::MATERIAL_CONDITION,
        data::BIOME,
    );
    TerrainGenerator::load(&res, settings, seed).expect("loads")
}

pub struct World {
    pub g: TerrainGenerator,
    pub ids: Vec<BiomeId>,
    pub min_y: i32,
    pub height: i32,
    pub seed: i64,
    states: HashMap<u32, State>,
}

impl World {
    pub fn new(settings: &str, seed: i64, layout: &str) -> Self {
        let g = generator(settings, seed);
        let ids = layout.lines().map(str::trim).filter(|l| !l.is_empty()).map(|n| g.biomes.id(n).expect("biome known")).collect();
        let (min_y, height) = match settings {
            "overworld" => (-64, 384),
            "nether" => (0, 128),
            _ => (0, 256),
        };
        Self { g, ids, min_y, height, seed, states: HashMap::new() }
    }

    fn state(&mut self, engine: u32) -> State {
        if let Some(s) = self.states.get(&engine) {
            return *s;
        }
        let s = env().blocks.state_by_name(self.g.state_key(engine as _)).expect("engine state resolves");
        self.states.insert(engine, s);
        s
    }

    /// The nine chunks around `(cx, cz)` (z then x), terrain only.
    pub fn neighbourhood(&mut self, cx: i32, cz: i32) -> Vec<ChunkData> {
        let mut out = Vec::new();
        let ids = self.ids.clone();
        for dz in -1..=1 {
            for dx in -1..=1 {
                let (px, pz) = (cx + dx, cz + dz);
                let mut ctx = Ctx::new(&self.g.program);
                let fill = self.g.fill_chunk(px, pz, &mut ctx);
                let mut source = |qx: i32, qy: i32, qz: i32| ids[pick(qx, qy, qz, ids.len())];
                let chunk = self.g.build_surface(&fill, px, pz, self.min_y, self.height, &mut source, &mut ctx);
                let states: Vec<State> = chunk.states.iter().map(|&s| self.state(s as u32)).collect();
                out.push(ChunkData::new(env(), self.min_y, self.height, states));
            }
        }
        out
    }

    /// The biomes stored in the sections of the nine chunks.
    pub fn present(&self, cx: i32, cz: i32) -> Vec<BiomeId> {
        let mut seen = std::collections::BTreeSet::new();
        for qz in (cz - 1) * 4..(cz + 2) * 4 {
            for qx in (cx - 1) * 4..(cx + 2) * 4 {
                for qy in self.min_y / 4..(self.min_y + self.height) / 4 {
                    seen.insert(self.ids[pick(qx, qy, qz, self.ids.len())]);
                }
            }
        }
        seen.into_iter().collect()
    }

    pub fn level(&mut self, cx: i32, cz: i32) -> Level<'static> {
        let chunks = self.neighbourhood(cx, cz);
        let ids = self.ids.clone();
        let sea = 63;
        Level::new(
            env(),
            self.seed,
            obfuscate_seed(self.seed),
            cx,
            cz,
            self.min_y,
            self.height,
            sea,
            chunks,
            Box::new(move |qx, qy, qz| ids[pick(qx, qy, qz, ids.len())]),
        )
    }

    pub fn decorator(&self) -> Decorator {
        let features = Features::load(env()).expect("features load");
        Decorator::new(features, &self.g.biomes, &self.ids).expect("decorator")
    }
}

/// One oracle-format line per executed feature, plus the nine chunk hashes.
pub fn run_chunk(world: &mut World, decorator: &Decorator, cx: i32, cz: i32, only: Option<&[&str]>) -> Vec<String> {
    let mut level = world.level(cx, cz);
    let present = world.present(cx, cz);
    let mut lines = vec![format!("chunk {} {} {} {}", "overworld", world.seed, cx, cz)];
    let blocks = &env().blocks;
    decorator.decorate(&mut level, cx, cz, &present, only, |r| {
        let mut changed = r.changed;
        // The oracle hashes chunk by chunk (z then x), within a chunk z, x, y.
        changed.sort_by_key(|&(x, y, z, _)| (z >> 4, x >> 4, z, x, y));
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &(x, y, z, s) in &changed {
            for v in [x, y, z] {
                fnv_bytes(&mut h, &v.to_le_bytes());
            }
            h = (h ^ blocks.key_hash(s)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        lines.push(format!(
            "f {} {} minecraft:{} draws {} changed {} hash {h:x}",
            r.step,
            r.index,
            decorator.features.placed[r.placed].name.trim_start_matches("placed_feature/"),
            r.draws,
            changed.len()
        ));
    });
    for dz in 0..3 {
        for dx in 0..3 {
            let mut h = 0xcbf2_9ce4_8422_2325u64;
            for &s in level.chunk_states(dx, dz) {
                h = (h ^ blocks.key_hash(s)).wrapping_mul(0x0000_0100_0000_01b3);
            }
            lines.push(format!("final {} {} {h:x}", cx + dx - 1, cz + dz - 1));
        }
    }
    assert_eq!(level.out_of_window, 0, "decoration touched columns outside the window");
    lines
}

/// Compares fixture text against produced lines, reporting the first differing feature.
pub fn compare(want: &str, got: &[String]) {
    let want: Vec<&str> = want.lines().filter(|l| !l.starts_with('#')).collect();
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        assert_eq!(w, g, "line {i} differs\n  want {w}\n  got  {g}");
    }
    assert_eq!(want.len(), got.len(), "line count");
}

/// Runs every chunk of an oracle fixture through the decorator (only the listed feature types
/// execute, as on the oracle side) and compares line by line.
pub fn check(seed: i64, fixture: &str, only: &[&str]) {
    let mut world = World::new("overworld", seed, SURFACE_BIOMES);
    let decorator = world.decorator();
    let mut checked = 0;
    let mut chunks: Vec<(i32, i32)> = Vec::new();
    for l in fixture.lines() {
        if let Some(rest) = l.strip_prefix("chunk ") {
            let f: Vec<&str> = rest.split(' ').collect();
            chunks.push((f[2].parse().unwrap(), f[3].parse().unwrap()));
        }
    }
    let mut sections: Vec<String> = Vec::new();
    for l in fixture.lines() {
        if l.starts_with("chunk ") {
            sections.push(String::new());
        }
        if let Some(s) = sections.last_mut() {
            s.push_str(l);
            s.push('\n');
        }
    }
    for ((cx, cz), want) in chunks.iter().zip(&sections) {
        let got = run_chunk(&mut world, &decorator, *cx, *cz, Some(only));
        compare(want, &got);
        checked += 1;
    }
    assert!(checked > 0);
}
