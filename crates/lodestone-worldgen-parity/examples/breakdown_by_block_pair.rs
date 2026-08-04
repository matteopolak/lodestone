//! `cargo run -p lodestone-worldgen-parity --example breakdown_by_block_pair`
//!
//! Finer-grained than `compare`'s bounding-box/section report: groups every
//! *real* (base-block-id-differing) mismatch by `(expected, got)` pair and
//! prints the biggest offenders, e.g. `("minecraft:terracotta",
//! "minecraft:stone") x1720` — this is what actually said "chunk
//! (-120,-120) is the badlands-exclusion gap" and "chunk (0,0)'s carve gap is
//! mostly water-where-vanilla-carved-a-flooded-cave-and-we-didn't," rather
//! than a percentage. Kept as a standing diagnostic, not a test: there is no
//! fixed "correct" set of pairs to assert against, since which pairs show up
//! is exactly what changes as more phases (#295, #406) get composed.
use lodestone_worldgen_parity::{diff_field, parse_compact};
use std::collections::BTreeMap;

fn main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest_dir}/fixtures/composed_seed42.txt");
    let text = std::fs::read_to_string(&path).unwrap();
    let fixtures = parse_compact(&text);
    for f in &fixtures {
        let generator = lodestone_server::overworld_generator(f.seed);
        let generated = generator.column(f.chunk_x, f.chunk_z);
        for (label, field) in [("surface", &f.postsurface), ("carve", &f.postcarve)] {
            let report = diff_field(
                f.min_y, f.height,
                |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
                |lx, y, lz| field.get(lx, y, lz).to_string(),
            );
            let mut base_only_same = 0;
            let mut fluid_level_only = 0;
            let mut real = 0;
            let mut by_pair: BTreeMap<(String,String), usize> = BTreeMap::new();
            for m in &report.mismatches {
                let eb = m.expected.split('[').next().unwrap();
                let gb = m.got.split('[').next().unwrap();
                if eb == gb {
                    base_only_same += 1;
                    if eb == "minecraft:water" || eb == "minecraft:lava" {
                        fluid_level_only += 1;
                    }
                } else {
                    real += 1;
                    *by_pair.entry((eb.to_string(), gb.to_string())).or_insert(0) += 1;
                }
            }
            println!("chunk ({},{}) stage {label}: total_mismatch={} base_id_same_but_props_differ={} (of which fluid-level-only={}) base_id_differs={}",
                f.chunk_x, f.chunk_z, report.mismatches.len(), base_only_same, fluid_level_only, real);
            println!("  top base-id-differ pairs:");
            let mut v: Vec<_> = by_pair.into_iter().collect();
            v.sort_by_key(|(_,c)| std::cmp::Reverse(*c));
            for (pair, count) in v.iter().take(10) {
                println!("    {:?} x{}", pair, count);
            }
        }
    }
}
