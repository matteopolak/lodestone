//! Biome sources (multi-noise overworld and nether, the end's island source)
//! against the real server's own (`BiomeSourceOracle263`): the chunk resolver over
//! bulk climate volumes and the point resolver, including the climate targets and
//! the search tree's tie-breaking and last-result hint.

use std::collections::BTreeMap;

use lodestone_worldgen_core::engine::release26_3::biome::BiomeId;
use lodestone_worldgen_core::engine::release26_3::climate::{BiomeSource, ClimateCursor, ClimateTree, EndBiomes};
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn long(&mut self, v: i64) {
        self.bytes(&v.to_le_bytes());
    }
    fn name(&mut self, s: &str) {
        self.bytes(s.as_bytes());
        self.bytes(&[0xFF]);
    }
}

struct Acc<'a> {
    g: &'a TerrainGenerator,
    targets: Fnv,
    biomes: Fnv,
    n: usize,
    top: BTreeMap<String, usize>,
}

impl<'a> Acc<'a> {
    fn new(g: &'a TerrainGenerator) -> Self {
        Self { g, targets: Fnv::new(), biomes: Fnv::new(), n: 0, top: BTreeMap::new() }
    }
    fn target(&mut self, t: &[i64; 6]) {
        for &v in t {
            self.targets.long(v);
        }
    }
    fn biome(&mut self, id: BiomeId) {
        let name = self.g.biomes.info(id).name.trim_start_matches("minecraft:").to_owned();
        self.biomes.name(&name);
        self.n += 1;
        *self.top.entry(name).or_default() += 1;
    }
    fn line(&self) -> String {
        let top: Vec<String> = self.top.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("targets {:x} biomes {:x} n {} top {}", self.targets.0, self.biomes.0, self.n, top.join(";"))
    }
}

fn setup(settings: &str, seed: i64) -> (TerrainGenerator, BiomeSource) {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS).with_surface_data(
        data::MATERIAL_RULE,
        data::MATERIAL_CONDITION,
        data::BIOME,
    );
    let g = TerrainGenerator::load(&res, settings, seed).expect("loads");
    let source = if settings == "end" {
        BiomeSource::End(EndBiomes::from_table(&g.biomes).unwrap())
    } else {
        let json = data::CLIMATE_POINTS.iter().find(|(n, _)| *n == settings).expect("points").1;
        BiomeSource::MultiNoise(ClimateTree::from_json(json, &g.biomes).unwrap())
    };
    (g, source)
}

fn check(settings: &str, seed: i64, fixture: &str) {
    let (g, source) = setup(settings, seed);
    let multi = matches!(source, BiomeSource::MultiNoise(_));
    // One cursor for the whole fixture: the reference keeps its last-result hint across
    // the queries of a run, and the fixture lines are in the order the oracle ran them.
    let mut cursor = ClimateCursor::default();
    let mut bad = Vec::new();
    for want in fixture.lines() {
        let f: Vec<&str> = want.split(' ').collect();
        let mut acc = Acc::new(&g);
        let prefix;
        if f[1] == "chunk" {
            let (cx, cz): (i32, i32) = (f[4].parse().unwrap(), f[5].parse().unwrap());
            prefix = format!("biomes chunk {settings} {seed} {cx} {cz}");
            if multi {
                // The oracle hashes the targets in x, z, y order over the grid.
                let climate = g.chunk_climate(cx, cz, &mut Ctx::new(&g.program));
                let qh = (g.height >> 2) as usize;
                for x in 0..4usize {
                    for z in 0..4usize {
                        for y in 0..qh {
                            acc.target(&climate[y + (x + z * 4) * qh]);
                        }
                    }
                }
            }
            let chunk = g.chunk_biomes(&source, &mut cursor, cx, cz);
            for section in 0..chunk.quarts_y / 4 {
                for x in 0..4 {
                    for y in 0..4 {
                        for z in 0..4 {
                            acc.biome(chunk.get(x, chunk.min_quart_y + section * 4 + y, z));
                        }
                    }
                }
            }
        } else {
            let (qx0, qy, qz0): (i32, i32, i32) = (f[4].parse().unwrap(), f[5].parse().unwrap(), f[6].parse().unwrap());
            prefix = format!("biomes scalar {settings} {seed} {qx0} {qy} {qz0}");
            let mut ctx = Ctx::uncached();
            for x in 0..16 {
                for z in 0..16 {
                    if multi {
                        acc.target(&g.climate_at(qx0 + x, qy, qz0 + z, &mut ctx));
                    }
                    acc.biome(g.biome_at_quart(&source, &mut cursor, qx0 + x, qy, qz0 + z, &mut ctx));
                }
            }
        }
        let got = format!("{prefix} {}", acc.line());
        if got != want {
            bad.push(format!("want {want}\ngot  {got}"));
        }
    }
    assert!(!fixture.is_empty());
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

macro_rules! case {
    ($test:ident, $file:literal, $settings:literal, $seed:literal) => {
        #[test]
        fn $test() {
            check($settings, $seed, include_str!(concat!("fixtures/release26_3/", $file)));
        }
    };
}

case!(overworld_chunks_42, "biomes-overworld-42.txt", "overworld", 42);
case!(overworld_chunks_negative_seed, "biomes-overworld--987654321.txt", "overworld", -987654321);
case!(nether_chunks_42, "biomes-nether-42.txt", "nether", 42);
case!(end_chunks_42, "biomes-end-42.txt", "end", 42);
case!(overworld_points_42, "biomes-scalar-overworld-42.txt", "overworld", 42);
case!(nether_points_42, "biomes-scalar-nether-42.txt", "nether", 42);
case!(end_points_42, "biomes-scalar-end-42.txt", "end", 42);

/// Control: a different seed must not reproduce the fixture.
#[test]
#[should_panic(expected = "want")]
fn control_wrong_seed_fails() {
    check("overworld", 43, include_str!("fixtures/release26_3/biomes-overworld-42.txt"));
}

/// Control for the search structure: brute force (first strict minimum in list
/// order) is not a substitute for the tree, because equidistant rows are resolved
/// by tree order and the last-result hint. Over a dense sweep the two disagree
/// somewhere, so a fixture match is evidence the port reproduces the tree.
#[test]
fn control_brute_force_differs_from_tree_somewhere() {
    let (g, _) = setup("overworld", 42);
    let json = data::CLIMATE_POINTS.iter().find(|(n, _)| *n == "overworld").unwrap().1;
    let rows: Vec<Vec<serde_json::Value>> = serde_json::from_str(json).unwrap();
    let tree = ClimateTree::from_json(json, &g.biomes).unwrap();
    let dist = |r: &Vec<serde_json::Value>, t: &[i64; 6]| -> i64 {
        let n = |i: usize| r[i].as_i64().unwrap();
        let mut d = n(12).pow(2);
        for k in 0..6 {
            let (lo, hi) = (n(2 * k), n(2 * k + 1));
            let a = t[k] - hi;
            let b = lo - t[k];
            d += (if a > 0 { a } else { b.max(0) }).pow(2);
        }
        d
    };
    let mut ctx = Ctx::uncached();
    let mut cursor = ClimateCursor::default();
    let mut differs = 0;
    for qz in -60..60 {
        for qx in -60..60 {
            let t = g.climate_at(qx * 5, 20, qz * 5, &mut ctx);
            let mut best = (i64::MAX, 0);
            for (i, r) in rows.iter().enumerate() {
                let d = dist(r, &t);
                if d < best.0 {
                    best = (d, i);
                }
            }
            let brute = g.biomes.id(rows[best.1][13].as_str().unwrap().trim_start_matches("minecraft:")).unwrap();
            if tree.search(&t, &mut cursor) != brute {
                differs += 1;
            }
        }
    }
    eprintln!("tree and brute force disagree on {differs} of 14400 targets");
    assert!(differs > 0, "no tie occurred in the sweep, so the control proves nothing");
}
