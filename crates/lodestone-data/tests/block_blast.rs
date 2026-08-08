//! Per-block-type blast-resistance/flammability table: hermetic checks over the
//! committed table, plus an `#[ignore]`d drift guard that regenerates it from the
//! committed JVM dump and asserts byte-for-byte equality.
//!
//! Modelled on `hardness.rs` and `snow_support.rs`: generate-or-assert with
//! `LODESTONE_REGEN=1`, anchored to a committed JVM dump that is itself the
//! external expected-value source.
//!
//! # Data provenance
//!
//! `tests/support/blast_fire_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server (`oracle-java/BlastFireOracle.java`) and reading
//! four quantities per registered block:
//! `Block.getExplosionResistance()`, `FireBlock`'s private
//! `igniteOdds`/`burnOdds` maps (populated by `FireBlock.bootStrap()`, which
//! `Bootstrap.bootStrap()` calls at `Bootstrap.java:51`), and
//! `BlockState.ignitedByLava()`.
//!
//! `blocks.json` has none of these fields; the fire odds are not even a block
//! *property* (they are a side table keyed by `Block`), and
//! `vendor/minecraft-data` has no 26.x data at all. So "boot the jar and ask it"
//! is again the only authoritative source, exactly as for hardness and collision
//! shapes.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump (`just oracle-blast-fire`), keeping the `#` header when copying
//!    over the committed dump. Runtime is Apple `container`, per
//!    `docs/oracle-runtimes.md`.
//! 2. Regenerate the committed table (`just regen-blast-fire`):
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test block_blast \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```
//!
//! [`committed_values_match_the_dump`] is deliberately **not** `#[ignore]`d: it
//! compares the committed table's *values* against the dump through the public
//! API rather than the generated file's bytes, so a reflow of the generated
//! source cannot hide a wrong number and an ordinary `cargo test --workspace`
//! still catches drift.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_blast;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/block_blast.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/blast_fire_jvm.txt");

/// One authoritative row.
struct Row {
    id: usize,
    name: String,
    resistance_bits: u32,
    ignite_odds: u8,
    burn_odds: u8,
    ignited_by_lava: bool,
}

fn parse_dump(text: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok.next().expect("id column").parse().expect("id is a usize");
        let name = tok.next().expect("name column").to_owned();
        let resistance_bits = u32::from_str_radix(tok.next().expect("resistance bits column"), 16)
            .expect("resistance bits are hex");
        let ignite_odds: u8 = tok
            .next()
            .expect("ignite odds column")
            .parse()
            .expect("ignite odds fit a u8");
        let burn_odds: u8 = tok
            .next()
            .expect("burn odds column")
            .parse()
            .expect("burn odds fit a u8");
        let lava_raw: u8 = tok
            .next()
            .expect("ignitedByLava column")
            .parse()
            .expect("ignitedByLava is 0 or 1");
        assert!(lava_raw <= 1, "ignitedByLava must be 0 or 1, got {lava_raw} on {line:?}");
        assert!(tok.next().is_none(), "unexpected trailing tokens on {line:?}");
        rows.push(Row {
            id,
            name,
            resistance_bits,
            ignite_odds,
            burn_odds,
            ignited_by_lava: lava_raw == 1,
        });
    }
    rows.sort_by_key(|row| row.id);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.id, index, "dump ids are not a dense 0..N (gap at {index})");
    }
    rows
}

/// Renders the committed `block_blast.rs` source from the parsed dump.
///
/// Deduplicates the four-tuple the way `hardness.rs`'s generator does: distinct
/// tuples are numbered in ascending *registry id* order, independent of dump
/// line order, so the output is deterministic. The name index is emitted
/// separately, sorted by name, because the consumer looks up by name.
fn generate(rows: &[Row]) -> String {
    let count = rows.len();

    let mut entry_index: BTreeMap<(u32, u8, u8, bool), usize> = BTreeMap::new();
    let mut distinct: Vec<(u32, u8, u8, bool)> = Vec::new();
    let mut per_row: Vec<usize> = Vec::with_capacity(count);
    for row in rows {
        let key = (
            row.resistance_bits,
            row.ignite_odds,
            row.burn_odds,
            row.ignited_by_lava,
        );
        let index = *entry_index.entry(key).or_insert_with(|| {
            distinct.push(key);
            distinct.len() - 1
        });
        per_row.push(index);
    }
    assert!(
        u16::try_from(distinct.len()).is_ok(),
        "distinct entry count {} no longer fits a u16 index",
        distinct.len()
    );

    let mut by_name: Vec<(&str, usize)> = rows
        .iter()
        .zip(&per_row)
        .map(|(row, &entry)| (row.name.as_str(), entry))
        .collect();
    by_name.sort_by_key(|(name, _)| *name);
    for window in by_name.windows(2) {
        assert_ne!(window[0].0, window[1].0, "duplicate block name {}", window[0].0);
    }

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test block_blast -- --ignored`\n\
         // from tests/support/blast_fire_jvm.txt (a headless 26.2 server dump of\n\
         // Block.getExplosionResistance(), FireBlock's igniteOdds/burnOdds maps and\n\
         // BlockState.ignitedByLava(), protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see tests/block_blast.rs).\n\
         //! Generated per-block-type blast-resistance and flammability table for\n\
         //! protocol 776 (Minecraft 26.2). Consumed by [`crate::block_blast`].\n\n",
    );
    let _ = writeln!(
        out,
        "/// Number of blocks in the `minecraft:block` registry.\n\
         pub const BLOCK_COUNT: u32 = {count};\n"
    );
    let _ = writeln!(
        out,
        "/// De-duplicated distinct `(explosionResistanceBits, igniteOdds, burnOdds,\n\
         /// ignitedByLava)` tuples ({} of them). The resistance is raw `f32` bits —\n\
         /// rebuild it with [`f32::from_bits`], never with a decimal literal.\n\
         pub static ENTRIES: [(u32, u8, u8, bool); {}] = [",
        distinct.len(),
        distinct.len()
    );
    for &(bits, ignite, burn, lava) in &distinct {
        let _ = writeln!(
            out,
            "    (0x{bits:08x}, {ignite}, {burn}, {lava}), // {}",
            f32::from_bits(bits)
        );
    }
    out.push_str("];\n\n");
    let _ = writeln!(
        out,
        "/// Every block name, **sorted ascending**, paired with its index into\n\
         /// [`ENTRIES`]. Sorted rather than in registry order so the consumer can\n\
         /// binary-search a name straight out of a canonical state string.\n\
         pub static BY_NAME: [(&str, u16); {count}] = ["
    );
    for (name, entry) in &by_name {
        let _ = writeln!(out, "    ({name:?}, {entry}),");
    }
    out.push_str("];\n");
    out
}

/// The drift guard: regenerates the committed table from the dump and asserts
/// byte-for-byte equality, or rewrites it under `LODESTONE_REGEN=1`.
///
/// `#[ignore]`d because it writes a source file when regenerating; the
/// value-level check below runs on every ordinary `cargo test`.
#[test]
#[ignore = "regenerates a committed source file; run explicitly (just regen-blast-fire)"]
fn committed_table_matches_dump() {
    let rows = parse_dump(DUMP);
    let rendered = generate(&rows);
    let path = committed_path();
    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(&path, &rendered).expect("write generated table");
        println!("regenerated {} ({} bytes)", path.display(), rendered.len());
        return;
    }
    let committed = std::fs::read_to_string(&path).expect("read committed table");
    assert_eq!(
        committed, rendered,
        "src/generated/block_blast.rs has drifted from tests/support/blast_fire_jvm.txt — \
         run `just regen-blast-fire`"
    );
}

/// Every one of the 1,196 rows, read back through the public API and compared
/// against the dump. This is the non-`#[ignore]`d anchor: it does not care how
/// the generated file is formatted, only that the numbers it yields are the
/// JVM's.
#[test]
fn committed_values_match_the_dump() {
    let rows = parse_dump(DUMP);
    assert_eq!(
        rows.len(),
        block_blast::BLOCK_COUNT as usize,
        "dump row count and BLOCK_COUNT disagree"
    );
    for row in &rows {
        let got = block_blast::blast(&row.name)
            .unwrap_or_else(|| panic!("{} missing from the committed table", row.name));
        assert_eq!(
            got.explosion_resistance.to_bits(),
            row.resistance_bits,
            "{}: resistance",
            row.name
        );
        assert_eq!(got.ignite_odds, row.ignite_odds, "{}: ignite odds", row.name);
        assert_eq!(got.burn_odds, row.burn_odds, "{}: burn odds", row.name);
        assert_eq!(
            got.ignited_by_lava, row.ignited_by_lava,
            "{}: ignitedByLava",
            row.name
        );
    }
}

/// Population counts computed from the dump, asserted against the same counts
/// computed through the public API — the "how much?" question, so a table that
/// silently answered `INERT` for most names could not pass.
///
/// The four numbers are properties of 26.2's data, not of this port: 207
/// flammable blocks, 312 lava-ignitable, and both set differences non-empty.
#[test]
fn population_counts_match_the_dump() {
    let rows = parse_dump(DUMP);
    let dump_flammable = rows.iter().filter(|r| r.ignite_odds > 0).count();
    let dump_lava = rows.iter().filter(|r| r.ignited_by_lava).count();
    let dump_lava_only = rows
        .iter()
        .filter(|r| r.ignited_by_lava && r.ignite_odds == 0)
        .count();
    let dump_fire_only = rows
        .iter()
        .filter(|r| r.ignite_odds > 0 && !r.ignited_by_lava)
        .count();

    assert_eq!(dump_flammable, 207, "26.2 has 207 fire-flammable blocks");
    assert_eq!(dump_lava, 312, "26.2 has 312 lava-ignitable blocks");
    assert!(dump_lava_only > 0 && dump_fire_only > 0, "the two sets genuinely differ");

    let api_flammable = rows
        .iter()
        .filter(|r| block_blast::blast_or_inert(&r.name).ignite_odds > 0)
        .count();
    let api_lava = rows
        .iter()
        .filter(|r| block_blast::blast_or_inert(&r.name).ignited_by_lava)
        .count();
    assert_eq!(api_flammable, dump_flammable);
    assert_eq!(api_lava, dump_lava);
}

/// The odds tiers vanilla's `FireBlock` defines as named constants
/// (`IGNITE_INSTANT 60`/`EASY 30`/`MEDIUM 15`/`HARD 5`,
/// `BURN_INSTANT 100`/`EASY 60`/`MEDIUM 20`/`HARD 5`) are the *only* values that
/// may appear. A table that had picked up a stray value — or lost a tier — fails
/// here rather than producing a plausible-looking fire.
#[test]
fn odds_use_only_vanillas_named_tiers() {
    let rows = parse_dump(DUMP);
    let mut ignite: Vec<u8> = rows.iter().map(|r| r.ignite_odds).collect();
    ignite.sort_unstable();
    ignite.dedup();
    assert_eq!(ignite, vec![0, 5, 15, 30, 60], "ignite odds tiers");

    let mut burn: Vec<u8> = rows.iter().map(|r| r.burn_odds).collect();
    burn.sort_unstable();
    burn.dedup();
    assert_eq!(burn, vec![0, 5, 20, 60, 100], "burn odds tiers");
}

/// The dump's block names and their registry ids must be exactly
/// `BLOCK_REGISTRY_NAMES` — a cross-table join that proves the dump was taken
/// from the same 26.2 registry the rest of this crate is generated from, rather
/// than from a neighbouring version.
#[test]
fn dump_registry_order_matches_the_block_registry_table() {
    let rows = parse_dump(DUMP);
    for row in &rows {
        let expected = lodestone_data::block_states::block_type_name(row.id as u32)
            .unwrap_or_else(|| panic!("registry id {} out of range", row.id));
        assert_eq!(expected, row.name, "registry id {}", row.id);
    }
}
