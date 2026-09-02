//! Nether parity against a **real Mojang server's own world**.
//!
//! # Where the expected values come from
//!
//! Not from us, and not from a round trip. `support/nether_vanilla_oracle.txt` is
//! extracted from `.cache/mc/survival/world/dimensions/minecraft/the_nether/
//! region/*.mca` — four region files a vanilla 26.2 server wrote, seed
//! **−195764831** (read out of `world/data/minecraft/world_gen_settings.dat`,
//! which is where 26.2 keeps the seed; `level.dat` has none). Two independent
//! products of that server are compared here:
//!
//! 1. **The stored per-quart biome containers** of all 1,116 chunks whose
//!    `Status` has reached `minecraft:biomes`. This is the decisive gate on the
//!    entire Nether noise stack, because in this dimension the biome *is* the
//!    noise: `noise_settings/nether.json` zeroes continentalness, erosion, depth
//!    and weirdness, so a biome is a pure function of two `NormalNoise`s that only
//!    exist if `legacy_random_source`, vanilla's own legacy-Nether-biome-noise
//!    constructor and
//!    the `seed + 0` / `seed + 1` seeding are all right at once. Get any one of
//!    them wrong and the map is a different map.
//! 2. **The bedrock floor and roof masks** of eight `minecraft:full` chunks —
//!    `vertical_gradient`'s per-position `nextFloat()` off the *surface* system's
//!    positional factory, which under legacy init is a `LegacyPositionalFactory`
//!    rather than the xoroshiro one the Overworld uses. Bedrock is the one product
//!    of the surface rules that later stages cannot touch: it is absent from
//!    `#minecraft:nether_carver_replaceables`, so no carver may replace it, and no
//!    Nether decoration feature places or removes it. So this comparison is exact
//!    without needing the decoration steps this generator does not run.
//!
//! # What is deliberately *not* compared
//!
//! Whole columns. Nether decoration (`glowstone_extra`, `patch_fire`,
//! `nether_wart`, the crimson/warped vegetation, basalt pillars, ores) and the
//! fortress/bastion structures are not composed yet, so a block-for-block sweep
//! would measure their absence rather than this generator's correctness. See
//! `docs/worldgen-nether.md` for what that leaves.
//!
//! # The 1,328 chunks that are excluded, and why that is not cherry-picking
//!
//! The oracle world stores 2,444 Nether chunks. 1,328 of them sit at
//! `minecraft:structure_starts`, a step *before* `fillBiomesFromNoise`, and their
//! biome container is still the registry placeholder — all 64 cells
//! `minecraft:plains`, in the Nether. The extractor asserts that (rather than
//! filtering on the name) and drops them, because comparing against a value
//! vanilla has not computed yet would be comparing against nothing.
//!
//! # The world-species limit
//!
//! **`minecraft:warped_forest` does not occur in this world at all.** Its
//! parameter row is the only one with a non-zero `offset` on the humidity side
//! (`0, 5000, offset 3750`), and this seed's 1,116 generated chunks never sample
//! there. A gate expecting one would be wrong about the world, not about the
//! generator — so `the_census_matches_the_oracle_world` pins **0** for it
//! explicitly rather than leaving it unmentioned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::nether::NetherGenerator;
use serde_json::Value;

const ORACLE: &str = include_str!("support/nether_vanilla_oracle.txt");

/// A [`Resolver`] over `crates/lodestone-server/assets/worldgen/` — the same
/// bundle the integrated server embeds, and the *Nether's* rows of it. A
/// single-biome fixture table would make every biome comparison trivially agree,
/// which is the "world" species of vacuous test.
struct NetherAssets {
    root: PathBuf,
}

impl NetherAssets {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    fn try_read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display())),
            Err(_) => Value::Null,
        }
    }
}

impl Resolver for NetherAssets {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }
    fn biome_parameters(&self) -> Value {
        self.read("biome_parameters", "nether")
    }
    fn biome_document(&self, id: &str) -> Value {
        self.try_read("biome", id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.try_read("configured_carver", id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.try_read("tags/block", id)
    }
}

const SEED: i64 = -195_764_831;

fn assets_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen")
}

fn settings() -> Value {
    let path = assets_root().join("noise_settings/nether.json");
    serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
    )
    .expect("nether.json")
}

/// The oracle fixture, parsed: the legend, the per-chunk biome grids, and the
/// per-chunk bedrock masks.
struct Oracle {
    legend: Vec<String>,
    biomes: BTreeMap<(i32, i32), [usize; 16]>,
    /// `(floor_mask, roof_mask)` per `lz * 16 + lx`, 5 bits each.
    bedrock: BTreeMap<(i32, i32), Vec<(u8, u8)>>,
    seed: i64,
}

fn oracle() -> Oracle {
    let mut legend: Vec<String> = Vec::new();
    let mut biomes = BTreeMap::new();
    let mut bedrock = BTreeMap::new();
    let mut seed = 0i64;
    for line in ORACLE.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("seed") => seed = parts.next().unwrap().parse().unwrap(),
            Some("legend") => {
                let index: usize = parts.next().unwrap().parse().unwrap();
                assert_eq!(index, legend.len(), "legend must be dense and in order");
                legend.push(parts.next().unwrap().to_string());
            }
            Some("biomes") => {
                let cx: i32 = parts.next().unwrap().parse().unwrap();
                let cz: i32 = parts.next().unwrap().parse().unwrap();
                let grid = parts.next().unwrap().as_bytes();
                assert_eq!(grid.len(), 16);
                biomes.insert(
                    (cx, cz),
                    std::array::from_fn(|i| (grid[i] - b'a') as usize),
                );
            }
            Some("bedrock") => {
                let cx: i32 = parts.next().unwrap().parse().unwrap();
                let cz: i32 = parts.next().unwrap().parse().unwrap();
                let hex = parts.next().unwrap();
                assert_eq!(hex.len(), 256 * 4);
                let masks = (0..256)
                    .map(|i| {
                        let floor = u8::from_str_radix(&hex[i * 4..i * 4 + 2], 16).unwrap();
                        let roof = u8::from_str_radix(&hex[i * 4 + 2..i * 4 + 4], 16).unwrap();
                        (floor, roof)
                    })
                    .collect();
                bedrock.insert((cx, cz), masks);
            }
            _ => {}
        }
    }
    assert!(!legend.is_empty() && !biomes.is_empty() && !bedrock.is_empty());
    Oracle {
        legend,
        biomes,
        bedrock,
        seed,
    }
}

/// The gate. Every one of the 1,116 biome-bearing chunks, every one of their 16
/// horizontal quarts, element-wise against what the vanilla server wrote — no
/// tolerance, no sampling, no aggregate.
///
/// # The one quart that differs, and why no implementation can fix it
///
/// **17,855 of 17,856 agree.** The one that does not is chunk (−20, −21) quart
/// (0, 1), and it is an **exact fitness tie**: the climate target there is
/// `temperature 2000, humidity 1457`, and `nether_wastes` (a degenerate point at
/// `0, 0`) and `crimson_forest` (at `4000, 0`) are both `2000² + 1457² =
/// 6,122,849` away. 2000 is the exact midpoint of 0 and 4000.
///
/// At an exact tie vanilla's answer is **a function of the previous query on the
/// same thread**, not of the target. Vanilla's own R-tree search seeds the descent with
/// its own last-result field — a `ThreadLocal<Leaf>` — and its own subtree search compares
/// with a strict `minDistance > childDistance`, so a tied candidate never displaces
/// the incumbent. The incumbent is whatever the
/// previous sampled position resolved to, and that `ThreadLocal` **persists across
/// chunks**: the neighbouring quart at (−80, −84) really is `crimson_forest`.
///
/// So vanilla's own answer at a tie depends on its chunk *and* quart iteration
/// order, which is exactly what a demand-ordered generator cannot have — and must
/// not have, since `columns_are_byte_identical_regardless_of_order_or_generator_instance`
/// is the stronger requirement. `BiomeTable::nearest_row_seeded` exists for this
/// (it takes the previous candidate), and threading it here would trade a
/// 0.0056% divergence for order-dependent output. That is the wrong trade.
///
/// This test therefore **classifies** rather than tolerates: a disagreement is
/// admissible only if the two biomes' fitnesses are *equal*, which is a derived
/// condition on the data rather than a threshold, and the admissible count and
/// position are pinned so that any change to either is a failure.
#[test]
fn nether_biomes_match_the_vanilla_oracle_world() {
    let o = oracle();
    assert_eq!(o.seed, SEED, "the fixture's seed must be the one we generate at");
    let resolver = NetherAssets { root: assets_root() };
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);

    // The independent view of the same table the generator searches, so a
    // disagreement can be classified without asking the generator about itself.
    let table = lodestone_worldgen::biome::parse_table(&resolver.biome_parameters());
    let sampler = lodestone_worldgen::biome::ClimateSampler::new(
        &settings,
        &lodestone_worldgen::density::Builder::with_algorithm(
            SEED,
            lodestone_worldgen::rng::Algorithm::Legacy,
            &resolver,
        ),
    );
    let fitness_of = |name: &str, target: &[i64; 7]| -> i64 {
        table
            .iter()
            .find(|p| p.biome == name)
            .unwrap_or_else(|| panic!("{name} is not in the Nether parameter table"))
            .fitness(target)
    };

    let mut checked = 0usize;
    let mut real: Vec<String> = Vec::new();
    let mut ties: Vec<(i32, i32, usize)> = Vec::new();
    for (&(cx, cz), expected) in &o.biomes {
        let got = generator.biome_quarts(cx, cz);
        for i in 0..16 {
            checked += 1;
            let want = &o.legend[expected[i]];
            if &got[i] == want {
                continue;
            }
            let qx = cx * 4 + (i % 4) as i32;
            let qz = cz * 4 + (i / 4) as i32;
            let target = sampler.target(qx * 4, 0, qz * 4);
            let ours = fitness_of(&got[i], &target);
            let theirs = fitness_of(want, &target);
            if ours == theirs {
                ties.push((cx, cz, i));
            } else if real.len() < 20 {
                real.push(format!(
                    "chunk ({cx},{cz}) quart ({},{}) target {target:?}: vanilla {want} \
                     (fitness {theirs}), ours {} (fitness {ours})",
                    i % 4,
                    i / 4,
                    got[i]
                ));
            }
        }
    }
    assert_eq!(checked, o.biomes.len() * 16);
    assert_eq!(
        o.biomes.len(),
        1116,
        "the oracle world has 1,116 biome-bearing Nether chunks; a different \
         count means the fixture was rebuilt against a different world"
    );
    assert!(
        real.is_empty(),
        "{} of {checked} Nether quarts disagree with the vanilla oracle world at a \
         strictly worse fitness -- these are real defects, not tie-breaks.\n  {}",
        real.len(),
        real.join("\n  ")
    );
    assert_eq!(
        ties,
        vec![(-20, -21, 4)],
        "exactly one quart in the oracle world sits on an exact fitness tie \
         (chunk (-20,-21), quart (0,1) = index 4); a different set means either the \
         climate values moved or the search's tie-break did"
    );
}

/// The census, pinned per name — including **`warped_forest` = 0**, which is a
/// property of this world rather than of the generator (see the module doc).
///
/// It is a second, coarser view of the same data as the test above and would be
/// redundant if that one were the only thing that could fail: it is here because
/// a wholesale swap of two biome names would leave the exhaustive comparison's
/// failure list enormous and unreadable, and this reduces the same defect to four
/// numbers.
#[test]
fn the_census_matches_the_oracle_world() {
    let o = oracle();
    let resolver = NetherAssets { root: assets_root() };
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);

    let mut contains: BTreeMap<String, usize> = BTreeMap::new();
    for &(cx, cz) in o.biomes.keys() {
        let quarts = generator.biome_quarts(cx, cz);
        let mut distinct: Vec<&str> = quarts.iter().map(String::as_str).collect();
        distinct.sort_unstable();
        distinct.dedup();
        for name in distinct {
            *contains.entry(name.to_string()).or_default() += 1;
        }
    }
    let got: Vec<(&str, usize)> = contains.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    assert_eq!(
        got,
        vec![
            ("minecraft:basalt_deltas", 172),
            ("minecraft:crimson_forest", 327),
            ("minecraft:nether_wastes", 487),
            ("minecraft:soul_sand_valley", 255),
        ],
        "chunks-containing census over the oracle world's 1,116 Nether chunks"
    );
    assert_eq!(
        contains.get("minecraft:warped_forest"),
        None,
        "this world contains no warped forest -- a world-species limit, not a bug"
    );
}

/// The Nether's climate router has `y_scale: 0.0` on both live channels and
/// literal `0.0` on the other four, so a biome cannot depend on `y`. This is what
/// licenses [`NetherGenerator::biome_quarts`] sampling at `quartY = 0` and
/// `NetherColumn` carrying 16 biomes instead of 128.
///
/// **The oracle world independently agrees**: the extractor asserts that every
/// section of every one of the 1,116 chunks stores the same 4×4 grid, and it does.
/// This test is the same claim made against the code rather than the data, so a
/// future router change that introduces a real depth channel fails here rather
/// than silently truncating.
#[test]
fn nether_biomes_do_not_vary_with_y() {
    let resolver = NetherAssets { root: assets_root() };
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);
    let sampler = lodestone_worldgen::biome::ClimateSampler::new(
        &settings,
        &lodestone_worldgen::density::Builder::with_algorithm(
            SEED,
            lodestone_worldgen::rng::Algorithm::Legacy,
            &resolver,
        ),
    );
    // A spread of real columns, not the origin only: the origin's climate could
    // be y-invariant by coincidence.
    for &(x, z) in &[(0, 0), (137, -244), (-1000, 3000), (48, 48)] {
        let at_zero = sampler.target(x, 0, z);
        for y in [1, 31, 32, 64, 120, 127] {
            assert_eq!(
                sampler.target(x, y, z),
                at_zero,
                "nether climate at ({x},{y},{z}) differs from y=0"
            );
        }
    }
    // And the generator's own answer agrees with the sampler's, so the two are
    // not separately-correct-but-different things.
    let quarts = generator.biome_quarts(0, 0);
    assert_eq!(quarts.len(), 16);
}

/// Bedrock floor and roof, exact, against eight `full` chunks the vanilla server
/// wrote. See the module doc for why bedrock specifically is comparable without
/// the decoration steps.
///
/// This is the gate on the *surface* half of legacy init: `vertical_gradient`
/// draws `factory.at(x, y, z).next_float()` from
/// `master.fromHashOf(random_name).forkPositional()`, and under
/// `legacy_random_source` that whole chain is the LCG. With the xoroshiro factory
/// the y = 0 and y = 127 rows still come out solid bedrock (those are the
/// gradient's saturated ends) and **every position between them is wrong** — which
/// is exactly why this compares the full 5-bit masks rather than "is there
/// bedrock at the bottom".
#[test]
fn nether_bedrock_shell_matches_the_vanilla_oracle_world() {
    let o = oracle();
    let resolver = NetherAssets { root: assets_root() };
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);

    let mut checked = 0usize;
    let mut wrong = 0usize;
    let mut first: Vec<String> = Vec::new();
    // Anti-vacuity floors on the fixture itself: if the masks were all-zero or
    // all-ones the comparison would pass for a generator that never places
    // bedrock, or one that fills the shell solid.
    let mut interior_bedrock = 0usize;
    let mut interior_not_bedrock = 0usize;

    for (&(cx, cz), masks) in &o.bedrock {
        for lz in 0..16usize {
            for lx in 0..16usize {
                let (floor, roof) = masks[lz * 16 + lx];
                for (i, y) in (0..5).enumerate() {
                    let want = floor & (1 << i) != 0;
                    let got = generator_bedrock(&generator, cx, cz, lx, y, lz);
                    checked += 1;
                    if (1..4).contains(&y) {
                        if want {
                            interior_bedrock += 1;
                        } else {
                            interior_not_bedrock += 1;
                        }
                    }
                    if want != got && first.len() < 20 {
                        first.push(format!(
                            "({},{y},{}) floor: vanilla {want}, ours {got}",
                            cx * 16 + lx as i32,
                            cz * 16 + lz as i32
                        ));
                    }
                    if want != got {
                        wrong += 1;
                    }
                }
                for (i, y) in (123..128).enumerate() {
                    let want = roof & (1 << i) != 0;
                    let got = generator_bedrock(&generator, cx, cz, lx, y, lz);
                    checked += 1;
                    if (124..127).contains(&y) {
                        if want {
                            interior_bedrock += 1;
                        } else {
                            interior_not_bedrock += 1;
                        }
                    }
                    if want != got && first.len() < 20 {
                        first.push(format!(
                            "({},{y},{}) roof: vanilla {want}, ours {got}",
                            cx * 16 + lx as i32,
                            cz * 16 + lz as i32
                        ));
                    }
                    if want != got {
                        wrong += 1;
                    }
                }
            }
        }
    }

    assert_eq!(checked, o.bedrock.len() * 256 * 10);
    assert!(
        interior_bedrock > 0 && interior_not_bedrock > 0,
        "the oracle masks must be mixed inside the gradient band \
         (bedrock {interior_bedrock}, not-bedrock {interior_not_bedrock}); \
         an all-or-nothing fixture would make this comparison vacuous"
    );
    assert_eq!(
        wrong, 0,
        "{wrong} of {checked} bedrock-shell positions disagree with the vanilla \
         oracle world.\n  {}",
        first.join("\n  ")
    );
}

/// Memoised per chunk so the bedrock test generates each of the eight columns
/// once rather than 2,560 times.
fn generator_bedrock(
    generator: &NetherGenerator,
    cx: i32,
    cz: i32,
    lx: usize,
    y: i32,
    lz: usize,
) -> bool {
    use std::cell::RefCell;
    thread_local! {
        static CACHE: RefCell<Option<((i32, i32), lodestone_worldgen::nether::NetherColumn)>> =
            const { RefCell::new(None) };
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let fresh = match cache.as_ref() {
            Some((key, _)) if *key == (cx, cz) => false,
            _ => true,
        };
        if fresh {
            *cache = Some(((cx, cz), generator.column(cx, cz)));
        }
        let (_, column) = cache.as_ref().unwrap();
        column.block_state(lx, y, lz) == "minecraft:bedrock"
    })
}

/// The anti-island floor: a generated Nether column has to contain real terrain,
/// a lava sea at the documented level and air above it. Every bound here is read
/// out of `nether.json` rather than written down, so a settings change moves the
/// expectation with it.
#[test]
fn a_nether_column_is_real_terrain_not_a_uniform_field() {
    let resolver = NetherAssets { root: assets_root() };
    let settings = settings();
    let generator = NetherGenerator::new(SEED, &settings, &resolver);
    let column = generator.column(0, 0);

    let sea_level = settings["sea_level"].as_i64().unwrap() as i32;
    let min_y = settings["noise"]["min_y"].as_i64().unwrap() as i32;
    let height = settings["noise"]["height"].as_i64().unwrap() as i32;
    assert_eq!((column.min_y(), column.height()), (min_y, height));

    let cells = (16 * 16 * height) as usize;
    let non_air = column.non_air_count();
    assert!(
        non_air > cells / 10 && non_air < cells,
        "{non_air} of {cells} non-air: an all-air or fully solid column is not terrain"
    );

    let mut lava = 0usize;
    let mut above_sea_lava = 0usize;
    for lz in 0..16 {
        for lx in 0..16 {
            for y in min_y..min_y + height {
                if column.block_state(lx, y, lz).starts_with("minecraft:lava") {
                    lava += 1;
                    if y >= sea_level {
                        above_sea_lava += 1;
                    }
                }
            }
        }
    }
    assert!(lava > 0, "the Nether's global fluid picker must produce lava");
    // Fill lava is strictly below `sea_level`; the carver's own lava is at
    // y <= min_gen_y + 31, i.e. 31, which is also below it. So nothing may place
    // lava at or above sea level in a column with no decoration.
    assert_eq!(
        above_sea_lava, 0,
        "lava at or above sea_level {sea_level} means the fluid picker's level is wrong"
    );
    // Bedrock shell, from the surface rules' two hardcoded vertical gradients.
    assert_eq!(column.block_state(0, min_y, 0), "minecraft:bedrock");
    assert_eq!(
        column.block_state(0, min_y + height - 1, 0),
        "minecraft:bedrock"
    );
}

/// `column` must be a pure function of `(seed, cx, cz)`: two independently
/// constructed generators, and the same generator asked out of order, produce
/// byte-identical palettes and block arrays.
///
/// Not a theoretical property — the Overworld path shipped a bug where a
/// `HashMap` iteration order made the same terrain come out with a permuted
/// palette, so "same blocks" is not the same claim as "same bytes". The join
/// scheduler is view-first and re-sortable, so nothing may depend on column order.
#[test]
fn columns_are_byte_identical_regardless_of_order_or_generator_instance() {
    let resolver = NetherAssets { root: assets_root() };
    let settings = settings();

    let forward = {
        let generator = NetherGenerator::new(SEED, &settings, &resolver);
        [(0, 0), (1, 0), (0, 1)].map(|(cx, cz)| generator.column(cx, cz).into_raw())
    };
    let reverse = {
        let generator = NetherGenerator::new(SEED, &settings, &resolver);
        let mut out = [(0, 1), (1, 0), (0, 0)].map(|(cx, cz)| generator.column(cx, cz).into_raw());
        out.reverse();
        out
    };
    for (i, (a, b)) in forward.iter().zip(reverse.iter()).enumerate() {
        assert_eq!(a.2, b.2, "palette differs for column {i}");
        assert_eq!(a.3, b.3, "blocks differ for column {i}");
        assert_eq!(a.4, b.4, "biomes differ for column {i}");
    }
}
