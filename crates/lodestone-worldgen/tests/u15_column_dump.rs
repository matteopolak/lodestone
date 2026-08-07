//! U15's byte-identity harness: dump whole generated columns to a file so two
//! *independently built* checkouts can be compared with `cmp`.
//!
//! # What it is
//!
//! `docs/plans/worldgen-rewrite.md`'s cutover gate requires that a
//! parity-preserving performance change leave the served bytes untouched. An
//! in-process test cannot show that, because the "before" implementation does
//! not exist in the same binary — so the arms are two checkouts (an isolated
//! `git worktree --detach` at the pre-change sha, and the working tree), each
//! running **this same file**, whose md5 is checked to be identical on both
//! arms before the comparison is believed.
//!
//! # How it works
//!
//! `#[ignore]`d, because it writes a file and is driven by a shell harness
//! rather than by `cargo test`. Run it as:
//!
//! ```text
//! LODESTONE_U15_DUMP=/tmp/arm.bin \
//!   cargo test -p lodestone-worldgen --test u15_column_dump -- --ignored --nocapture
//! ```
//!
//! The dump is the **wire-facing** product — `GeneratedColumn::into_raw`'s
//! `(min_y, height, palette, blocks, biomes)` — not an internal structure. That
//! matters: palette *order* reaches the wire (see `interner.rs`'s note on
//! `RandomState` iteration order having shipped a bug here once), so a change
//! that permuted the palette while placing identical blocks would be caught by
//! this and missed by a block-set comparison.
//!
//! The scene is deliberately a 3×3 patch per seed rather than a single column:
//! only an interior chunk has all nine of its ore/vegetation sources really
//! computed, and the ore driver's cross-chunk spill is exactly what a change to
//! the ore engine could break. Five seeds, because "one seed out of five failing
//! is a failure" is the plan's rollback rule and a one-seed gate cannot express
//! it.
//!
//! # How to change it
//!
//! Adding a seed or widening the patch is free and strictly better. **Do not**
//! make the dump lossy (e.g. hashing it here) — `cmp` on the raw bytes is what
//! makes a mismatch localisable to a column, and the harness prints the first
//! differing offset. The non-degeneracy figures it prints are part of the
//! evidence, not decoration: a dump of pure air would compare equal under any
//! change at all.

use std::io::Write;

/// Seeds the two arms both sweep. Seed 42 is the one the checked-in JVM density
/// dump anchors (`overworld_gen.rs`), so at least one arm coordinate is known to
/// be JVM-verified terrain rather than an arbitrary column.
const SEEDS: [i64; 5] = [42, 7, 1337, -5, 123_456_789];

/// Chunk patch per seed. `-1..=1` so `(0, 0)` is a fully interior column.
const PATCH: std::ops::RangeInclusive<i32> = -1..=1;

#[test]
#[ignore = "two-arm byte-identity dumper; driven by U15's shell harness, not by cargo test"]
fn dump_columns() {
    let path = std::env::var("LODESTONE_U15_DUMP")
        .expect("set LODESTONE_U15_DUMP to the output path for this arm");
    let mut out = Vec::<u8>::new();
    let mut columns = 0usize;
    let mut distinct_bytes = std::collections::HashSet::new();
    let mut distinct_states = std::collections::HashSet::new();
    let mut non_air_total = 0u64;

    for seed in SEEDS {
        let generator = lodestone_server::overworld_generator(seed);
        for cz in PATCH {
            for cx in PATCH {
                let column = generator.column(cx, cz);
                let non_air = column.non_air_count();
                non_air_total += non_air as u64;
                let (min_y, height, palette, blocks, biomes) = column.into_raw();

                // A self-describing record per column, so `cmp`'s byte offset is
                // interpretable and a truncated arm cannot compare equal to a
                // prefix of the other.
                out.extend_from_slice(b"COL:");
                out.extend_from_slice(&seed.to_le_bytes());
                out.extend_from_slice(&cx.to_le_bytes());
                out.extend_from_slice(&cz.to_le_bytes());
                out.extend_from_slice(&min_y.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
                for state in &palette {
                    distinct_states.insert(state.clone());
                    out.extend_from_slice(&(state.len() as u32).to_le_bytes());
                    out.extend_from_slice(state.as_bytes());
                }
                out.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
                for b in &blocks {
                    let bytes = b.to_le_bytes();
                    distinct_bytes.insert(bytes[0]);
                    distinct_bytes.insert(bytes[1]);
                    out.extend_from_slice(&bytes);
                }
                for biome in &biomes {
                    out.extend_from_slice(&(biome.len() as u32).to_le_bytes());
                    out.extend_from_slice(biome.as_bytes());
                }
                columns += 1;
            }
        }
    }

    // Non-degeneracy, asserted here rather than left to the shell: a dump that
    // is mostly one value compares equal under almost any change, so these
    // floors are what make the `cmp` result mean something. The numbers are
    // deliberately floors, not equalities — they must not become a second parity
    // gate that a legitimate terrain change has to be talked past.
    assert_eq!(columns, SEEDS.len() * 9, "the sweep did not cover every seed/chunk");
    assert!(non_air_total > 0, "the whole sweep generated only air");
    assert!(
        distinct_states.len() >= 20,
        "only {} distinct block states across the whole dump — too uniform to detect a \
         reordering or a misplacement",
        distinct_states.len()
    );
    assert!(
        distinct_bytes.len() >= 20,
        "only {} distinct byte values in the block arrays — the dump is too uniform for \
         `cmp` to be meaningful",
        distinct_bytes.len()
    );

    let mut f = std::fs::File::create(&path).expect("create dump");
    f.write_all(&out).expect("write dump");
    println!(
        "U15 dump: {columns} columns, {} bytes, {} distinct block states, \
         {} distinct block-array byte values, {non_air_total} non-air blocks -> {path}",
        out.len(),
        distinct_states.len(),
        distinct_bytes.len(),
    );
}
