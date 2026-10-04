//! Cave and canyon carving against the real server's fill, surface and carver
//! steps over the real biome source (`CarverOracle263`). Each chunk is checked
//! after surface building and again after carving, so a mismatch names its stage.

mod common;
use lodestone_worldgen_core::engine::release26_3::carver::CarverTable;
use lodestone_worldgen_core::engine::release26_3::climate::{BiomeSource, ClimateCursor, ClimateTree, EndBiomes};
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;

fn setup(settings: &str, seed: i64) -> (TerrainGenerator, BiomeSource, CarverTable) {
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
    (g, source, CarverTable::from_tables(data::CARVER).expect("carvers parse"))
}

fn check(settings: &str, seed: i64, fixture: &str, carve: bool) {
    let (g, source, carvers) = setup(settings, seed);
    let (min_y, height) = match settings {
        "overworld" => (-64, 384),
        "nether" => (0, 128),
        _ => (0, 256),
    };
    // One cursor for the run: the reference's source keeps its last-result hint across chunks.
    let mut cursor = ClimateCursor::default();
    let mut climate_ctx = Ctx::uncached();
    let mut bad = Vec::new();
    let lines: Vec<&str> = fixture.lines().collect();
    assert!(!lines.is_empty());
    for pair in lines.chunks(2) {
        let f: Vec<&str> = pair[0].split(' ').collect();
        let (cx, cz): (i32, i32) = (f[3].parse().unwrap(), f[4].parse().unwrap());
        let mut ctx = Ctx::new(&g.program);
        let mut fill = g.fill_chunk(cx, cz, &mut ctx);
        let mut chunk = {
            let mut zoomed = |qx: i32, qy: i32, qz: i32| g.biome_at_quart(&source, &mut cursor, qx, qy, qz, &mut climate_ctx);
            g.build_surface(&fill, cx, cz, min_y, height, &mut zoomed, &mut ctx)
        };
        let got_surface = format!("surface {}", common::chunk_report(&g, &chunk, settings, seed, cx, cz));
        if got_surface != pair[0] {
            bad.push(format!("want {}\ngot  {got_surface}", pair[0]));
        }
        if carve {
            g.carve_chunk(&carvers, &source, &mut cursor, cx, cz, &mut fill, &mut chunk, &mut ctx);
        }
        let got_carved = format!("carved {}", common::chunk_report(&g, &chunk, settings, seed, cx, cz));
        if got_carved != pair[1] {
            bad.push(format!("want {}\ngot  {got_carved}", pair[1]));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

macro_rules! carve_case {
    ($test:ident, $settings:literal, $seed:literal) => {
        #[test]
        fn $test() {
            check($settings, $seed, include_str!(concat!("fixtures/release26_3/carve-", $settings, "-", $seed, ".txt")), true);
        }
    };
}

carve_case!(overworld_42, "overworld", 42);
carve_case!(overworld_negative_seed, "overworld", -987654321);
/// Chunks picked because carving opens caves under grass over dirt, so the
/// top-material re-dress path runs (hundreds of positions across these chunks).
#[test]
fn overworld_redress_42() {
    check("overworld", 42, include_str!("fixtures/release26_3/carve-overworld-redress-42.txt"), true);
}
carve_case!(nether_42, "nether", 42);
carve_case!(end_42, "end", 42);

/// Control: without carving the overworld fixture's second lines must not match,
/// which proves the fixture chunks are actually carved.
#[test]
#[should_panic(expected = "want carved")]
fn control_uncarved_overworld_differs() {
    check("overworld", 42, include_str!("fixtures/release26_3/carve-overworld-42.txt"), false);
}

/// Control: a different seed must not reproduce the fixture.
#[test]
#[should_panic(expected = "want")]
fn control_wrong_seed_fails() {
    check("overworld", 43, include_str!("fixtures/release26_3/carve-overworld-42.txt"), true);
}
