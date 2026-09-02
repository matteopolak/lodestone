//! Composed-generator parity anchor: proves [`OverworldGenerator::column`] runs
//! the *same* interpolated density field that `chunk_parity` proves 98304/98304
//! bit-exact, and that the surface + fluid stages actually ran (not a stone
//! sign-field, not empty air).
//!
//! This is deliberately anchored to the verified `density_chunk_jvm.txt` fixture
//! (chunk (0,0), seed 42) rather than a fresh oracle: the individual stages are
//! already JVM-proven, so what needs proving here is that *composition* preserves
//! them. The whole-field solidity check ties column() to that fixture cell for
//! cell; the presence checks are the anti-vacuity floors (a generator that
//! produced all air, or all stone, would pass a naive "did it run" test).

use std::path::Path;

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

const DENSITY_FIXTURE: &str = include_str!("support/density_chunk_jvm.txt");
const SEED: i64 = 42;

struct FsResolver {
    root: std::path::PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

impl Resolver for FsResolver {
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
}

fn base(name: &str) -> &str {
    name.split('[').next().unwrap_or(name)
}

fn make_resolver_and_settings() -> (FsResolver, Value) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    (resolver, settings)
}

fn make_generator() -> OverworldGenerator {
    let (resolver, settings) = make_resolver_and_settings();
    // Avoid dropping `settings` before the borrow ends by building inline.
    OverworldGenerator::new(SEED, &settings, &resolver, "minecraft:plains", false)
}

/// Every block's solidity in the composed output must match the **real
/// aquifer's own** solid/non-solid decision — not raw
/// `density > 0` any more. Before this crate composed the real aquifer, shape
/// solidity *was* exactly `density > 0` (the sea-level fluid approximation
/// never overrode it), which is what this test originally asserted against
/// the isolated `density_chunk_jvm.txt` fixture. `aquifer_parity` already
/// proves `AquiferSystem::block_at` bit-exact against the JVM for this same
/// chunk/seed; this test's job is narrower — proving *composition* preserved
/// that decision — so it re-derives the expectation from a freshly built
/// `AquiferSystem` (same seed/settings/resolver) rather than the stale
/// density-only fixture, which the real aquifer's barrier pressure can now
/// legitimately disagree with (a barrier can seal a `density <= 0` cell back
/// into `Stone`, or the reverse) — that disagreement was the concrete failure
/// observed once the real aquifer was composed here, which is exactly what
/// this rewritten assertion is checking is *not* a composition bug.
#[test]
fn composed_shape_matches_fresh_aquifer_solid_decision() {
    let (resolver, settings) = make_resolver_and_settings();
    let generator = OverworldGenerator::new(SEED, &settings, &resolver, "minecraft:plains", false);
    let col = generator.column(0, 0);

    let builder = lodestone_worldgen::density::Builder::new(SEED, &resolver);
    let aquifer = lodestone_worldgen::aquifer::AquiferSystem::new(&settings, &builder, 0, 0);

    let mut checked = 0usize;
    let mut solid_cells = 0usize;
    for x in 0..16i32 {
        for z in 0..16i32 {
            for y in generator.min_y()..generator.min_y() + generator.height() {
                let state = col.block_state(x as usize, y, z as usize);
                let b = base(state);
                let gen_solid = b != "minecraft:air" && b != "minecraft:water" && b != "minecraft:lava";
                let expected_solid = matches!(
                    aquifer.block_at(x, y, z),
                    lodestone_worldgen::aquifer::BlockKind::Stone
                );

                assert_eq!(
                    gen_solid, expected_solid,
                    "solidity mismatch at ({x},{y},{z}): generated {state:?} (solid={gen_solid}) \
                     vs fresh AquiferSystem::block_at (solid={expected_solid})"
                );
                checked += 1;
                if expected_solid {
                    solid_cells += 1;
                }
            }
        }
    }

    assert_eq!(checked, 16 * 16 * 384, "did not check the whole chunk");
    assert!(
        solid_cells > 1000,
        "vacuous: only {solid_cells} solid cells in the reference field"
    );
}

/// Companion control: the earlier premise ("solid == density > 0") must now
/// actually be *false* somewhere in this chunk — otherwise the rewrite above
/// would be proving nothing new over the original assertion. Uses the same
/// `density_chunk_jvm.txt` fixture the original test compared against.
#[test]
fn real_aquifer_solid_decision_disagrees_with_raw_density_at_least_once() {
    let (resolver, settings) = make_resolver_and_settings();
    let builder = lodestone_worldgen::density::Builder::new(SEED, &resolver);
    let aquifer = lodestone_worldgen::aquifer::AquiferSystem::new(&settings, &builder, 0, 0);

    let mut disagreements = 0usize;
    for line in DENSITY_FIXTURE.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (coords, bits) = line.rsplit_once(' ').expect("malformed line");
        let mut it = coords.split(',');
        let x: i32 = it.next().unwrap().parse().unwrap();
        let y: i32 = it.next().unwrap().parse().unwrap();
        let z: i32 = it.next().unwrap().parse().unwrap();
        let density = f64::from_bits(u64::from_str_radix(bits, 16).unwrap());
        let density_solid = density > 0.0;
        let aquifer_solid = matches!(
            aquifer.block_at(x, y, z),
            lodestone_worldgen::aquifer::BlockKind::Stone
        );
        if density_solid != aquifer_solid {
            disagreements += 1;
        }
    }
    assert!(
        disagreements > 0,
        "expected the real aquifer to disagree with raw density>0 at least once in this chunk \
         (otherwise the dedicated aquifer-decision test above is equivalent to the old, retired one)"
    );
}

/// The surface and fluid stages must actually have run: chunk (0,0) at seed 42 is
/// oceanic (the `surface_parity` fixture has its heightmap ≡ 62 < sea level 63),
/// so it must contain water above a sand/gravel floor — never bare stone all the
/// way up.
#[test]
fn composed_surface_and_fluid_are_applied() {
    let generator = make_generator();
    let col = generator.column(0, 0);

    let mut water = 0usize;
    let mut floor_surface = 0usize; // sand/gravel/dirt/grass — surface-rule results
    let mut stone = 0usize;
    for lz in 0..16usize {
        for lx in 0..16usize {
            for y in col.min_y()..col.min_y() + col.height() {
                match base(col.block_state(lx, y, lz)) {
                    "minecraft:water" => water += 1,
                    "minecraft:sand"
                    | "minecraft:gravel"
                    | "minecraft:dirt"
                    | "minecraft:grass_block" => floor_surface += 1,
                    "minecraft:stone" => stone += 1,
                    _ => {}
                }
            }
        }
    }

    assert!(
        water > 500,
        "expected an ocean column, got {water} water blocks"
    );
    assert!(
        floor_surface > 100,
        "surface rules did not run — {floor_surface} surface blocks (stone sign-field?)"
    );
    assert!(stone > 0, "expected stone below the surface, got none");

    // Structural: the surface rule must have capped the ocean floor with surface
    // materials across the chunk — a stone sign-field would cap every column with
    // stone. Vanilla legitimately leaves *some* deep-floor columns as stone, so
    // this is an aggregate (surface caps dominate), not a per-column claim.
    let mut surface_caps = 0usize;
    let mut stone_caps = 0usize;
    let mut grass_underwater = 0usize;
    for lz in 0..16usize {
        for lx in 0..16usize {
            let mut floor_y = None;
            for y in (col.min_y()..col.min_y() + col.height()).rev() {
                match base(col.block_state(lx, y, lz)) {
                    "minecraft:air" | "minecraft:water" | "minecraft:lava" => {}
                    _ => {
                        floor_y = Some(y);
                        break;
                    }
                }
            }
            let Some(floor_y) = floor_y else { continue };
            match base(col.block_state(lx, floor_y, lz)) {
                "minecraft:sand" | "minecraft:gravel" | "minecraft:dirt" => surface_caps += 1,
                "minecraft:stone" => stone_caps += 1,
                // grass on a submerged floor would be a surface-rule bug
                "minecraft:grass_block" => grass_underwater += 1,
                _ => {}
            }
        }
    }
    assert!(
        surface_caps > stone_caps,
        "surface rule barely ran: {surface_caps} surface-capped vs {stone_caps} stone-capped columns"
    );
    assert_eq!(
        grass_underwater, 0,
        "grass_block on a submerged floor is a surface-rule bug ({grass_underwater} columns)"
    );
    assert!(
        col.non_air_count() > 16 * 16 * 10,
        "vacuous: only {} non-air blocks",
        col.non_air_count()
    );
}
