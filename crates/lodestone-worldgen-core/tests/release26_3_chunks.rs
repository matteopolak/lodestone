//! Terrain-shape fill (final density, aquifer substance, fluid-update flags)
//! against the real server's own chunk fill (`ChunkOracle263`).

use lodestone_worldgen_core::engine::release26_3::aquifer::Fluid;
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, Substance, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;

fn fnv_bytes(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn line(settings: &str, seed: i64, cx: i32, cz: i32, g: &TerrainGenerator) -> String {
    let mut ctx = Ctx::new(&g.program);
    let fill = g.fill_chunk(cx, cz, &mut ctx);
    let mut dh = 0xcbf2_9ce4_8422_2325u64;
    for d in &fill.density {
        fnv_bytes(&mut dh, &d.to_bits().to_le_bytes());
    }
    // The oracle hashes substance codes in fill order: z, x, then y descending.
    let v = &fill.volume;
    let mut sh = 0xcbf2_9ce4_8422_2325u64;
    let mut counts = [0usize; 5];
    for z in 0..v.size[2] {
        for x in 0..v.size[0] {
            for y in (0..v.size[1]).rev() {
                let c = match fill.substance[v.index(x, y, z)] {
                    Substance::Default => 0,
                    Substance::Fluid(Fluid::Air) => 1,
                    Substance::Fluid(Fluid::Water) => 2,
                    Substance::Fluid(Fluid::Lava) => 3,
                };
                counts[c] += 1;
                fnv_bytes(&mut sh, &[b"DAWLX"[c]]);
            }
        }
    }
    let mut fh = 0xcbf2_9ce4_8422_2325u64;
    for i in &fill.fluid_updates {
        fnv_bytes(&mut fh, &i.to_le_bytes());
    }
    format!(
        "chunk {settings} {seed} {cx} {cz} density {dh:x} {} subst {sh:x} D{} A{} W{} L{} X{} sched {fh:x} {}",
        fill.density.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
        fill.fluid_updates.len()
    )
}

fn check(settings: &str, seed: i64, fixture: &str) {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS);
    let g = TerrainGenerator::load(&res, settings, seed).expect("loads");
    let mut bad = Vec::new();
    for want in fixture.lines() {
        let f: Vec<&str> = want.split(' ').collect();
        let got = line(settings, seed, f[3].parse().unwrap(), f[4].parse().unwrap(), &g);
        if got != want {
            bad.push(format!("want {want}\ngot  {got}"));
        }
    }
    assert!(!fixture.is_empty());
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

macro_rules! chunk_case {
    ($test:ident, $settings:literal, $seed:literal) => {
        #[test]
        fn $test() {
            check($settings, $seed, include_str!(concat!("fixtures/release26_3/chunk-", $settings, "-", $seed, ".txt")));
        }
    };
}

chunk_case!(overworld_42, "overworld", 42);
chunk_case!(overworld_negative_seed, "overworld", -987654321);
chunk_case!(amplified_42, "amplified", 42);
chunk_case!(large_biomes_42, "large_biomes", 42);
chunk_case!(nether_42, "nether", 42);
chunk_case!(end_42, "end", 42);
chunk_case!(caves_42, "caves", 42);
chunk_case!(floating_islands_42, "floating_islands", 42);

/// Control: a different seed must not reproduce the fixture.
#[test]
#[should_panic(expected = "want")]
fn control_wrong_seed_fails() {
    check("overworld", 43, include_str!("fixtures/release26_3/chunk-overworld-42.txt"));
}

fn column_line(settings: &str, seed: i64, x: i32, z: i32, g: &TerrainGenerator) -> String {
    let mut ctx = Ctx::new(&g.program);
    let cells = g.fill_column(x, z, &mut ctx);
    // Density is hashed from a second, identical fill: the column API returns substance only.
    let fill = {
        let v = lodestone_worldgen_core::engine::release26_3::volume::Volume::new([1, g.height, 1], [x, g.min_y, z], [1, 1, 1]);
        let mut d = vec![0.0f32; v.len()];
        let mut c = Ctx::new(&g.program);
        g.program.volume(&mut c, g.router.final_density, &mut d, &v);
        d
    };
    let mut dh = 0xcbf2_9ce4_8422_2325u64;
    for d in &fill {
        fnv_bytes(&mut dh, &d.to_bits().to_le_bytes());
    }
    let mut sh = 0xcbf2_9ce4_8422_2325u64;
    let mut counts = [0usize; 5];
    for cell in cells.iter().rev() {
        let c = match cell {
            Substance::Default => 0,
            Substance::Fluid(Fluid::Air) => 1,
            Substance::Fluid(Fluid::Water) => 2,
            Substance::Fluid(Fluid::Lava) => 3,
        };
        counts[c] += 1;
        fnv_bytes(&mut sh, &[b"DAWLX"[c]]);
    }
    format!(
        "column {settings} {seed} {x} {z} density {dh:x} {} subst {sh:x} D{} A{} W{} L{} X{}",
        fill.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4]
    )
}

fn check_columns(settings: &str, seed: i64, fixture: &str) {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS);
    let g = TerrainGenerator::load(&res, settings, seed).expect("loads");
    let mut bad = Vec::new();
    for want in fixture.lines() {
        let f: Vec<&str> = want.split(' ').collect();
        let got = column_line(settings, seed, f[3].parse().unwrap(), f[4].parse().unwrap(), &g);
        if got != want {
            bad.push(format!("want {want}\ngot  {got}"));
        }
    }
    assert!(!fixture.is_empty());
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

macro_rules! column_case {
    ($test:ident, $settings:literal, $seed:literal) => {
        #[test]
        fn $test() {
            check_columns($settings, $seed, include_str!(concat!("fixtures/release26_3/column-", $settings, "-", $seed, ".txt")));
        }
    };
}

column_case!(column_overworld_42, "overworld", 42);
column_case!(column_overworld_negative_seed, "overworld", -987654321);
column_case!(column_amplified_42, "amplified", 42);
column_case!(column_nether_42, "nether", 42);
column_case!(column_end_42, "end", 42);
column_case!(column_caves_42, "caves", 42);

/// Control: a different seed must not reproduce the column fixture.
#[test]
#[should_panic(expected = "want")]
fn column_control_wrong_seed_fails() {
    check_columns("overworld", 43, include_str!("fixtures/release26_3/column-overworld-42.txt"));
}

/// Single-thread throughput of the terrain-shape fill: best of 25 8x8 passes,
/// with a fresh context per chunk and with one reused context. Run with
/// `cargo test -p lodestone-worldgen-core --release --test release26_3_chunks -- --ignored --nocapture`.
#[test]
#[ignore = "timing"]
fn fill_throughput() {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS);
    for settings in ["overworld", "nether", "end"] {
        let g = TerrainGenerator::load(&res, settings, 42).expect("loads");
        let mut sink = 0usize;
        let mut best = [std::time::Duration::MAX; 2];
        for _ in 0..25 {
            let t = std::time::Instant::now();
            for cx in 0..8 {
                for cz in 0..8 {
                    let mut ctx = Ctx::new(&g.program);
                    sink += g.fill_chunk(cx, cz, &mut ctx).fluid_updates.len();
                }
            }
            best[0] = best[0].min(t.elapsed() / 64);
            let t = std::time::Instant::now();
            let mut ctx = Ctx::new(&g.program);
            for cx in 0..8 {
                for cz in 0..8 {
                    ctx.reset();
                    sink += g.fill_chunk(cx, cz, &mut ctx).fluid_updates.len();
                }
            }
            best[1] = best[1].min(t.elapsed() / 64);
        }
        println!("{settings}: fresh ctx {:?}/chunk, reused ctx {:?}/chunk (sink {sink})", best[0], best[1]);
    }
}
