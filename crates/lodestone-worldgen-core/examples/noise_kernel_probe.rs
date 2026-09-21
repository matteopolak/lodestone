//! Small release-only control for the noise kernel.
//!
//! This is intentionally separate from the production worldgen benchmark: it
//! keeps the measured loop to `PerlinNoise::get_value`, uses a fixed sequence
//! of fractional coordinates shaped like the field sampler's X/Z rows, and
//! prints a digest so the optimizer cannot discard the work.  It is useful for
//! comparing kernel layouts before running the much slower production session.

use std::hint::black_box;
use std::time::Instant;

use lodestone_worldgen_core::noise::{NormalNoise, PerlinNoise};
use lodestone_worldgen_core::rng::XoroshiroRandomSource;

const SAMPLES: usize = 1_048_576;
const ROUNDS: usize = 7;

fn coordinates() -> Vec<(f64, f64, f64)> {
    // Four-by-four X/Z rows and eight Y values mirror the production field
    // cell shape while crossing lattice boundaries at non-round fractions.
    (0..SAMPLES)
        .map(|i| {
            let cell = i / 128;
            let lane = i % 128;
            let x = f64::from((cell % 97) as i32 * 4) + f64::from((lane % 4) as u32) * 0.375;
            let y = -64.0 + f64::from((lane / 4 % 32) as u32) * 0.625;
            let z = f64::from((cell / 97 % 97) as i32 * 4) + f64::from((lane / 32) as u32) * 0.4375;
            (x, y, z)
        })
        .collect()
}

fn run_perlin(noise: &PerlinNoise, coordinates: &[(f64, f64, f64)]) -> (u128, u64) {
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let start = Instant::now();
    for _ in 0..ROUNDS {
        for &(x, y, z) in coordinates {
            let value = noise.get_value(black_box(x), black_box(y), black_box(z));
            digest ^= value.to_bits();
            digest = digest.wrapping_mul(0x1000_0000_01b3);
        }
    }
    (start.elapsed().as_nanos(), black_box(digest))
}

fn run_normal(noise: &NormalNoise, coordinates: &[(f64, f64, f64)]) -> (u128, u64) {
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let start = Instant::now();
    for _ in 0..ROUNDS {
        for &(x, y, z) in coordinates {
            let value = noise.get_value(black_box(x), black_box(y), black_box(z));
            digest ^= value.to_bits();
            digest = digest.wrapping_mul(0x1000_0000_01b3);
        }
    }
    (start.elapsed().as_nanos(), black_box(digest))
}

fn main() {
    let coordinates = coordinates();
    for octaves in [2usize, 4, 8, 16] {
        let amplitudes = vec![1.0; octaves];
        let mut rng = XoroshiroRandomSource::new(0x4f53_5441 + octaves as i64);
        let noise = PerlinNoise::create(&mut rng, -(octaves as i32 / 2), &amplitudes);
        let (nanos, digest) = run_perlin(&noise, &coordinates);
        let samples = (SAMPLES * ROUNDS) as f64;
        println!(
            "perlin_octaves={octaves} ns_per_sample={:.3} digest={digest:016x}",
            nanos as f64 / samples
        );
    }

    let amplitudes = [1.0; 8];
    let mut rng = XoroshiroRandomSource::new(0x4e4f_524d);
    let noise = NormalNoise::create(&mut rng, -4, &amplitudes);
    let (nanos, digest) = run_normal(&noise, &coordinates);
    let samples = (SAMPLES * ROUNDS) as f64;
    println!(
        "normal_octaves=8+8 ns_per_sample={:.3} digest={digest:016x}",
        nanos as f64 / samples
    );
}
