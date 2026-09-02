//! Per-block-state hardness/correct-tool table: hermetic checks over the
//! committed table, plus an `#[ignore]`d drift guard that regenerates it from
//! the committed JVM dump and asserts byte-for-byte equality (modelled on
//! `entity_dimensions.rs` and `collision_shapes.rs`). The generator lives here
//! so the checked-in table can never silently drift from the game data.
//!
//! # Data provenance
//!
//! `tests/support/hardness_jvm.txt` is an authoritative dump produced by
//! booting the real 26.2 server and reading vanilla's own "get destroy speed"
//! and "requires correct tool for drops" accessors for every one
//! of the 32,366 registered states (`HardnessOracle.java`, walking
//! vanilla's own block-state registry). `blocks.json` has no destroy-speed field at
//! all (it is block *properties* only) and `vendor/minecraft-data` was measured
//! stale/incomplete for 26.2 on the neighbouring collision-shape table (see
//! `src/collision_shapes.rs` module docs), so as with collision shapes and
//! entity dimensions, "boot the jar and ask it" is the only authoritative
//! source. It is committed as the external anchor (§ "an expected value must
//! originate outside the code under test"): the table is derived from it, so a
//! misread float or a transposed row fails the drift check rather than
//! silently shipping.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump from the server (writes to a scratch file; keep the `#` header
//!    when copying over the committed dump):
//!
//! ```text
//! CACHE="$(cd .cache/mc/26.2 && pwd)"
//! HERE="$(cd crates/protocol/v770/oracle-java && pwd)"
//! docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle -w /work eclipse-temurin:25-jdk bash -c '
//!   CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
//!   cp /oracle/HardnessOracle.java /work/ && javac -cp "$CP" -d /work /work/HardnessOracle.java
//!   java -cp "/work:$CP" HardnessOracle'
//! ```
//!
//!    then copy its stdout over `tests/support/hardness_jvm.txt` (keeping the
//!    `#` header).
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test hardness \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::block_states;
use lodestone_data::hardness;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/hardness.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/hardness_jvm.txt");

/// One authoritative row: global state id, block name, the raw f32 bits of
/// destroy speed, and requires-correct-tool-for-drops.
struct Row {
    id: usize,
    name: String,
    hardness_bits: u32,
    requires_tool: bool,
}

fn parse_dump(text: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: usize = tok.next().expect("id column").parse().expect("id is a usize");
        let name = tok.next().expect("name column").to_owned();
        let hardness_bits = u32::from_str_radix(tok.next().expect("hardness bits column"), 16)
            .expect("hardness bits are hex");
        let requires_tool_raw: u8 = tok
            .next()
            .expect("requires-correct-tool column")
            .parse()
            .expect("requires-correct-tool is 0 or 1");
        assert!(
            requires_tool_raw == 0 || requires_tool_raw == 1,
            "requires-correct-tool must be 0 or 1, got {requires_tool_raw} on {line:?}"
        );
        assert!(tok.next().is_none(), "unexpected trailing tokens on {line:?}");
        rows.push(Row {
            id,
            name,
            hardness_bits,
            requires_tool: requires_tool_raw == 1,
        });
    }
    rows.sort_by_key(|row| row.id);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.id, index, "dump ids are not a dense 0..N (gap at {index})");
    }
    rows
}

/// Renders the committed `hardness.rs` source from the parsed dump.
///
/// Deduplicates `(hardness, requires_correct_tool)` pairs the same way
/// `collision_shapes::generate` dedups shapes: distinct pairs are numbered in
/// ascending state-id order, independent of dump line order, so the table is
/// deterministic.
fn generate(rows: &[Row]) -> String {
    let count = rows.len();

    let mut entry_index: BTreeMap<(u32, bool), usize> = BTreeMap::new();
    let mut distinct: Vec<(u32, bool)> = Vec::new();
    let mut state_entry: Vec<usize> = Vec::with_capacity(count);
    for row in rows {
        let key = (row.hardness_bits, row.requires_tool);
        let idx = *entry_index.entry(key).or_insert_with(|| {
            distinct.push(key);
            distinct.len() - 1
        });
        state_entry.push(idx);
    }

    assert!(
        distinct.len() <= usize::from(u16::MAX) + 1,
        "more than u16::MAX distinct hardness entries"
    );

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test hardness -- --ignored`\n\
         // from tests/support/hardness_jvm.txt (a headless 26.2 server dump of\n\
         // BlockState.getDestroySpeed()/requiresCorrectToolForDrops(), protocol 776 /\n\
         // Minecraft 26.2). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see\n\
         // the test module docs).\n",
    );
    out.push_str(
        "//! Generated per-block-state hardness/correct-tool table for protocol 776\n\
         //! (Minecraft 26.2), indexed by global block-state id. Consumed by\n\
         //! [`crate::hardness`].\n\n",
    );

    let _ = writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).");
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// De-duplicated distinct `(hardness, requires_correct_tool)` pairs ({} of them),\n\
         /// indexed by entry index.",
        distinct.len()
    );
    let _ = writeln!(out, "pub static ENTRIES: [(f32, bool); {}] = [", distinct.len());
    for &(bits, requires_tool) in &distinct {
        // Round-trip through the exact f32 the game produced. Rust's `{:?}`
        // emits the shortest decimal that parses back to the same f32, so the
        // literal is human-readable *and* bit-exact.
        let hardness = f32::from_bits(bits);
        assert_eq!(
            hardness.to_bits(),
            bits,
            "hardness literal {hardness:?} does not round-trip"
        );
        let _ = writeln!(out, "    ({hardness:?}, {requires_tool}),");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state entry index into [`ENTRIES`], indexed by global block-state id."
    );
    let _ = writeln!(out, "pub static STATE_ENTRY: [u16; {count}] = [");
    for chunk in state_entry.chunks(16) {
        out.push_str("    ");
        for idx in chunk {
            let _ = write!(out, "{idx}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (anchored to the committed dump)
// ---------------------------------------------------------------------------

/// Finds the first state id whose block name matches `name`, via the
/// committed block-state table — robust to id shifts across data bumps.
fn first_id_named(name: &str) -> Option<u32> {
    (0..block_states::STATE_COUNT).find(|&id| block_states::block_name(id) == Some(name))
}

#[test]
fn committed_table_matches_the_committed_dump_bit_for_bit() {
    // The strongest check: every value in the shipped accessor equals the raw
    // f32/bool the real server produced. Non-vacuous by construction — it
    // iterates all 32,366 states and compares exact bits, so a single misread
    // float or flipped bool fails.
    let rows = parse_dump(DUMP);
    assert_eq!(
        rows.len(),
        hardness::STATE_COUNT as usize,
        "dump/table state count mismatch"
    );
    let mut checked = 0usize;
    for row in &rows {
        let entry = hardness::hardness(row.id as u32)
            .unwrap_or_else(|| panic!("id {} ({}) missing from table", row.id, row.name));
        assert_eq!(
            entry.hardness.to_bits(),
            row.hardness_bits,
            "hardness mismatch for {} (id {}): table {:?} vs server f32::from_bits(0x{:08x})",
            row.name,
            row.id,
            entry.hardness,
            row.hardness_bits
        );
        assert_eq!(
            entry.requires_correct_tool, row.requires_tool,
            "requires_correct_tool mismatch for {} (id {})",
            row.name, row.id
        );
        checked += 1;
    }
    assert_eq!(checked, 32_366, "expected 32,366 block states checked, got {checked}");
}

#[test]
fn count_matches_block_state_table() {
    assert_eq!(
        hardness::STATE_COUNT,
        block_states::STATE_COUNT,
        "hardness table must cover exactly the block-state id space"
    );
}

#[test]
fn out_of_range_ids_are_none() {
    assert_eq!(hardness::hardness(hardness::STATE_COUNT), None);
    assert_eq!(hardness::hardness(u32::MAX), None);
}

#[test]
fn every_id_resolves() {
    for id in 0..hardness::STATE_COUNT {
        assert!(hardness::hardness(id).is_some(), "id {id} did not resolve");
    }
}

#[test]
fn bedrock_is_unbreakable() {
    let id = first_id_named("minecraft:bedrock").expect("bedrock present");
    let entry = hardness::hardness(id).expect("bedrock resolves");
    assert_eq!(entry.hardness, -1.0, "bedrock (id {id}) must be unbreakable");
}

#[test]
fn obsidian_is_far_harder_than_dirt() {
    let obsidian_id = first_id_named("minecraft:obsidian").expect("obsidian present");
    let dirt_id = first_id_named("minecraft:dirt").expect("dirt present");
    let obsidian = hardness::hardness(obsidian_id).expect("obsidian resolves");
    let dirt = hardness::hardness(dirt_id).expect("dirt resolves");
    assert_eq!(obsidian.hardness, 50.0, "obsidian (id {obsidian_id}) hardness");
    assert_eq!(dirt.hardness, 0.5, "dirt (id {dirt_id}) hardness");
    assert!(
        obsidian.hardness > dirt.hardness * 50.0,
        "obsidian ({}) must be far harder than dirt ({})",
        obsidian.hardness,
        dirt.hardness
    );
}

#[test]
fn stone_requires_correct_tool_dirt_does_not() {
    let stone_id = first_id_named("minecraft:stone").expect("stone present");
    let dirt_id = first_id_named("minecraft:dirt").expect("dirt present");
    let stone = hardness::hardness(stone_id).expect("stone resolves");
    let dirt = hardness::hardness(dirt_id).expect("dirt resolves");
    assert!(
        stone.requires_correct_tool,
        "stone (id {stone_id}) must require the correct tool for drops"
    );
    assert!(
        !dirt.requires_correct_tool,
        "dirt (id {dirt_id}) must not require the correct tool for drops"
    );
}

// ---------------------------------------------------------------------------
// Drift guard (regenerates from the committed dump; `#[ignore]`d for parity
// with the other generated tables, though it needs no external artifact)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed table; run explicitly"]
fn committed_table_matches_dump() {
    let rows = parse_dump(DUMP);
    let generated = generate(&rows);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/hardness.rs is stale vs the JVM dump; regenerate with LODESTONE_REGEN=1"
    );
}
