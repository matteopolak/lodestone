//! One-command entry point: prints the current chunk-for-chunk parity of
//! `lodestone-server`'s *actually served* generator against the committed
//! vanilla fixtures, per chunk and per stage.
//!
//! ```text
//! cargo run -p lodestone-worldgen-parity --bin compare
//! ```
//!
//! This is read-only and hermetic (no Docker, no network) — it just loads
//! `fixtures/composed_seed42.txt` and diffs it against
//! `lodestone_server::overworld_generator`. `tests/chunk_parity.rs` asserts
//! on the same numbers this prints; run this binary when you want to *see*
//! them (with the bounding box / per-section breakdown) rather than just
//! pass/fail.
use lodestone_worldgen_parity::{diff_field, parse_compact};

fn main() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest_dir}/fixtures/composed_seed42.txt");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    let fixtures = parse_compact(&text);

    for f in &fixtures {
        println!("=== chunk ({}, {}) seed {} ===", f.chunk_x, f.chunk_z, f.seed);
        let generator = lodestone_server::overworld_generator(f.seed);
        let generated = generator.column(f.chunk_x, f.chunk_z);

        println!("-- currently-composed subset (shape+fluid+biome+surface) vs. vanilla postsurface --");
        let report_surface = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
            |lx, y, lz| f.postsurface.get(lx, y, lz).to_string(),
        );
        print!("{}", report_surface.summary(8));

        println!("-- currently-composed subset vs. vanilla postcarve (the full non-feature/non-structure target) --");
        let report_full = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
            |lx, y, lz| f.postcarve.get(lx, y, lz).to_string(),
        );
        print!("{}", report_full.summary(8));

        println!(
            "-- currently-composed subset (no ore composition yet) vs. vanilla postfeatures — \
             the gap ore composition (#295's next increment) needs to close --"
        );
        let report_features = diff_field(
            f.min_y,
            f.height,
            |lx, y, lz| generated.block_state(lx as usize, y, lz as usize).to_string(),
            |lx, y, lz| f.postfeatures.get(lx, y, lz).to_string(),
        );
        print!("{}", report_features.summary(8));
        println!();
    }
}
