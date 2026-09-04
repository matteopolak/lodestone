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
//! vanilla's own "get explosion resistance" accessor, its own fire-block
//! class's own private
//! ignite-odds/burn-odds maps (populated by its own bootstrap step, which
//! vanilla's own bootstrap step calls), and its own "ignited by lava" field.
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

use lodestone_data::{block::Block, block_blast, block_states};

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
/// line order, so the output is deterministic. The generated lookup table is
/// indexed by the canonical `Block` registry id, so it does not repeat names.
fn generate(rows: &[Row]) -> String {
    let count = rows.len();
    let registry_count = usize::from(Block::COUNT);
    assert_eq!(
        count, registry_count,
        "registry ids must be exactly 0..BLOCK_COUNT: got {count} rows for {registry_count} blocks"
    );

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

    let mut entry_by_registry_id = vec![None; registry_count];
    for (row, &entry) in rows.iter().zip(&per_row) {
        assert!(
            row.id < registry_count,
            "registry ids must be exactly 0..BLOCK_COUNT: id {} is outside 0..{registry_count}",
            row.id
        );
        assert!(
            entry_by_registry_id[row.id].replace(entry).is_none(),
            "registry ids must be exactly 0..BLOCK_COUNT: duplicate id {}",
            row.id
        );
        let block = Block::from_name(&row.name)
            .unwrap_or_else(|| panic!("{} is not a built-in block", row.name));
        assert_eq!(
            usize::from(block.registry_id()),
            row.id,
            "registry ids must be exactly 0..BLOCK_COUNT: {} has registry id {} in the canonical block table, not {}",
            row.name,
            block.registry_id(),
            row.id
        );
    }
    let entry_by_registry_id: Vec<usize> = entry_by_registry_id
        .into_iter()
        .enumerate()
        .map(|(id, entry)| {
            entry.unwrap_or_else(|| {
                panic!("registry ids must be exactly 0..BLOCK_COUNT: missing id {id}")
            })
        })
        .collect();

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test block_blast -- --ignored`\n\
         // from tests/support/blast_fire_jvm.txt (a headless 26.2 server dump of\n\
         // each block's own explosion resistance, the fire block's own ignite/burn\n\
         // odds maps, and each block state's own lava-ignition flag, protocol 776 /\n\
         // Minecraft 26.2).\n\
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
        "/// Entry index into [`ENTRIES`], indexed by `minecraft:block` registry id.\n\
         /// The canonical names live only in `generated_block_registry`.\n\
         pub static ENTRY_BY_REGISTRY_ID: [u16; {count}] = ["
    );
    for entry in &entry_by_registry_id {
        let _ = writeln!(out, "    {entry},");
    }
    out.push_str("];\n\n");

    // ---- the flat per-block-state resistance table (the ray-walk hot path) ----
    let mut value_index: BTreeMap<u32, usize> = BTreeMap::new();
    let mut values: Vec<u32> = Vec::new();
    let mut intern = |bits: u32| -> usize {
        *value_index.entry(bits).or_insert_with(|| {
            values.push(bits);
            values.len() - 1
        })
    };
    // `EMPTY_RESISTANCE` is interned first so it is index 0 and the array reads
    // as "mostly zero means mostly air" at a glance.
    let empty_index = intern(EMPTY_RESISTANCE_BITS);
    assert_eq!(empty_index, 0);

    let state_count = block_states::STATE_COUNT;
    let mut per_state: Vec<usize> = Vec::with_capacity(state_count as usize);
    for id in 0..state_count {
        let name = block_states::block_name(id).expect("every state id has a block");
        let waterlogged = block_states::properties(id)
            .expect("every state id has properties")
            .iter()
            .any(|(key, value)| *key == "waterlogged" && *value == "true");
        let block = Block::from_name(name)
            .unwrap_or_else(|| panic!("{name} is not a built-in block"));
        let entry = entry_by_registry_id[usize::from(block.registry_id())];
        let block_bits = distinct[entry].0;
        let is_air = matches!(
            name,
            "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
        );
        let index = if is_air && !waterlogged {
            empty_index
        } else {
            let block = f32::from_bits(block_bits);
            let effective = if waterlogged {
                block.max(FLUID_RESISTANCE)
            } else {
                block
            };
            intern(effective.to_bits())
        };
        per_state.push(index);
    }
    assert!(
        u16::try_from(values.len()).is_ok(),
        "distinct resistance count {} no longer fits a u16 index",
        values.len()
    );

    let _ = writeln!(
        out,
        "/// Number of block states (ids are `0..STATE_COUNT`).\n\
         pub const STATE_COUNT: u32 = {state_count};\n"
    );
    let _ = writeln!(
        out,
        "/// The sentinel [`RESISTANCE_VALUES`] entry standing for vanilla's\n\
         /// `Optional.empty()` — a cell that is air and holds no fluid, which a blast\n\
         /// ray pays no resistance term for. A quiet NaN, so it can never collide with\n\
         /// a real resistance.\n\
         pub const EMPTY_RESISTANCE: u32 = 0x{EMPTY_RESISTANCE_BITS:08x};\n"
    );
    let _ = writeln!(
        out,
        "/// De-duplicated `max(block, fluid)` explosion resistances as raw `f32`\n\
         /// bits ({} of them), including [`EMPTY_RESISTANCE`] at index 0.\n\
         pub static RESISTANCE_VALUES: [u32; {}] = [",
        values.len(),
        values.len()
    );
    for &bits in &values {
        if bits == EMPTY_RESISTANCE_BITS {
            let _ = writeln!(out, "    0x{bits:08x}, // Optional.empty()");
        } else {
            let _ = writeln!(out, "    0x{bits:08x}, // {}", f32::from_bits(bits));
        }
    }
    out.push_str("];\n\n");
    let _ = writeln!(
        out,
        "/// Per-block-state index into [`RESISTANCE_VALUES`], indexed by global\n\
         /// block-state id. The explosion ray walk's innermost lookup: one bounds-checked\n\
         /// index, no strings, with the fluid `max` already folded in.\n\
         pub static STATE_RESISTANCE_ENTRY: [u16; {state_count}] = ["
    );
    for chunk in per_state.chunks(32) {
        out.push_str("    ");
        for (offset, index) in chunk.iter().enumerate() {
            if offset > 0 {
                out.push(' ');
            }
            let _ = write!(out, "{index},");
        }
        out.push('\n');
    }
    out.push_str("];\n");
    out
}

/// The quiet NaN standing for `Optional.empty()`. All-ones is never a real
/// resistance, and `f32::from_bits` on it is a NaN rather than a plausible
/// number, so a consumer that forgot the sentinel check fails loudly.
const EMPTY_RESISTANCE_BITS: u32 = 0xFFFF_FFFF;

/// The resistance both vanilla fluids report, folded into the flat table.
const FLUID_RESISTANCE: f32 = 100.0;

#[test]
fn generator_emits_registry_indexed_entries_without_duplicate_names() {
    let rendered = generate(&parse_dump(DUMP));

    assert!(rendered.contains("pub static ENTRY_BY_REGISTRY_ID: [u16; 1196]"));
    assert!(!rendered.contains("BY_NAME"));
    assert!(!rendered.contains("\"minecraft:"));
}

#[test]
#[should_panic(expected = "registry ids must be exactly 0..BLOCK_COUNT")]
fn generator_rejects_duplicate_registry_ids() {
    let mut rows = parse_dump(DUMP);
    rows[1].id = rows[0].id;

    generate(&rows);
}

#[test]
#[should_panic(expected = "registry ids must be exactly 0..BLOCK_COUNT")]
fn generator_rejects_missing_registry_ids() {
    let mut rows = parse_dump(DUMP);
    rows.remove(1);

    generate(&rows);
}

#[test]
#[should_panic(expected = "registry ids must be exactly 0..BLOCK_COUNT")]
fn generator_rejects_out_of_range_registry_ids() {
    let mut rows = parse_dump(DUMP);
    rows[1].id = rows.len();

    generate(&rows);
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

/// The odds tiers vanilla's own fire block defines as named constants
/// (ignite: instant 60 / easy 30 / medium 15 / hard 5,
/// burn: instant 100 / easy 60 / medium 20 / hard 5) are the *only* values that
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
