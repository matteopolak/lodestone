//! Bit-for-bit parity of vanilla's own math-helper trig table and value helpers against the JVM.
//!
//! The carvers lean entirely on vanilla's own math-helper sin/cos (a 65536-entry float lookup
//! table) for tunnel/canyon walking, and on its own random-between[-inclusive] for
//! sampling `FloatProvider`/`HeightProvider` config fields. If any table entry
//! or the index arithmetic is off by a bit, cave shapes drift and every
//! downstream carve diverges — so this is a load-bearing primitive that must be
//! proven before the geometry on top of it.
//!
//! Oracle: `scripts/worldgen-oracle/MthOracle.java` reads the game's own `SIN`
//! field via reflection and calls the real math-helper sin/cos/random-between. We
//! compare every one of the 65536 table entries, plus `sin`/`cos` over a dense
//! sweep of the exact `double` inputs the oracle used, plus the RNG-driven
//! helpers — element-wise, naming the first divergent index on failure.

use std::collections::HashMap;

use lodestone_worldgen::math;
use lodestone_worldgen::rng::{LegacyRandomSource, RandomSource};

fn load() -> HashMap<String, String> {
    let text = include_str!("support/mth_jvm.txt");
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(' ').expect("key value");
        m.insert(k.to_string(), v.to_string());
    }
    m
}

fn f32_bits(hex: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(hex, 16).expect("hex f32"))
}
fn f64_bits(hex: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(hex, 16).expect("hex f64"))
}

#[test]
fn mth_sin_table_matches_jvm_bit_for_bit() {
    let m = load();
    let len: usize = m["sin.len"].parse().unwrap();
    assert_eq!(len, 65536, "table length");

    // Rebuild the table the same way vanilla does and diff every entry.
    let mut checked = 0usize;
    for i in 0..len {
        let want = f32_bits(&m[&format!("sin.{i}")]);
        // Reproduce SIN[i] = (float)Math.sin(i / SIN_SCALE).
        let got = ((f64::from(i as u32) / 10_430.378_350_470_453).sin()) as f32;
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "SIN[{i}] mismatch: got {got} ({:08x}) want {want} ({:08x})",
            got.to_bits(),
            want.to_bits()
        );
        checked += 1;
    }
    assert_eq!(checked, 65536, "must have checked the whole table");
}

#[test]
fn mth_sin_cos_match_jvm_over_dense_sweep() {
    let m = load();
    let n: usize = m["msamples.len"].parse().unwrap();
    assert!(n > 4000, "expected a dense sweep, got {n}");

    let mut checked = 0usize;
    for idx in 0..n {
        let d = f64_bits(&m[&format!("din.{idx}")]);
        let want_sin = f32_bits(&m[&format!("msin.{idx}")]);
        let want_cos = f32_bits(&m[&format!("mcos.{idx}")]);
        let got_sin = math::sin(d);
        let got_cos = math::cos(d);
        assert_eq!(
            got_sin.to_bits(),
            want_sin.to_bits(),
            "sin({d}) [idx {idx}] got {got_sin} want {want_sin}"
        );
        assert_eq!(
            got_cos.to_bits(),
            want_cos.to_bits(),
            "cos({d}) [idx {idx}] got {got_cos} want {want_cos}"
        );
        checked += 1;
    }
    assert_eq!(checked, n, "must have checked every sampled angle");
}

#[test]
fn mth_random_helpers_match_jvm() {
    let m = load();
    let seeds: [i64; 4] = [0, 42, 1_234_567_890_123, -8_823_894_646];
    let mut checked = 0usize;
    for seed in seeds {
        let mut r = LegacyRandomSource::new(seed);
        for i in 0..8 {
            let want = f32_bits(&m[&format!("rb[{seed}].{i}")]);
            let got = math::random_between(&mut r, 0.75, 1.4);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "randomBetween seed {seed} #{i}"
            );
            checked += 1;
        }
        for i in 0..8 {
            let want: i32 = m[&format!("rbi[{seed}].{i}")].parse().unwrap();
            let got = math::random_between_inclusive(&mut r, 10, 67);
            assert_eq!(got, want, "randomBetweenInclusive seed {seed} #{i}");
            checked += 1;
        }
        for i in 0..4 {
            let want = f32_bits(&m[&format!("abs[{seed}].{i}")]);
            let got = math::abs_f32(r.next_float() - 0.5);
            assert_eq!(got.to_bits(), want.to_bits(), "abs seed {seed} #{i}");
            checked += 1;
        }
    }
    assert!(
        checked >= 80,
        "must have checked all helper draws, got {checked}"
    );
}
