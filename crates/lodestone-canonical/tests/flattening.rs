//! Generator + drift guard + ambiguous-case regression tests for the
//! `id:meta` -> modern block-state table (`src/flattening.rs`,
//! `src/generated/flattening.rs`). Modelled directly on
//! `crates/versions/26.2/tests/hardness.rs`'s generate-or-assert pattern.
//!
//! # Data provenance
//!
//! `tests/support/flattening_1_13_2_jvm.txt` is an authoritative dump
//! produced by reflectively reading a private static lookup array inside
//! one class of the real 1.13.2 server jar's own world-upgrade (old-save
//! conversion) flattening step — see `oracle-java/FlatteningOracle.java`'s
//! module docs for how that class was located (obfuscated names are
//! jar-build-specific and meaningless, so it was found by grepping raw class
//! bytes for distinctive pre-Flattening block names and decompiling the
//! surviving candidate), what it is and is not authoritative for, and
//! `docs/protocol-340-flattening-table.md` for the full enumeration of
//! ambiguous cases this generator encodes.
//!
//! # Refreshing after the source jar changes
//!
//! 1. Re-dump (pure JDK, no Docker/live server needed — the lookup's static
//!    initializer runs on class-load, with no server bootstrap required):
//!
//! ```text
//! JAR=.cache/mc/1.13.2/server.jar
//! javac -cp "$JAR" -d /tmp/flatten-oracle-out \
//!     crates/lodestone-canonical/oracle-java/FlatteningOracle.java
//! java -cp "/tmp/flatten-oracle-out:$JAR" FlatteningOracle
//! ```
//!
//!    then copy stdout over `tests/support/flattening_1_13_2_jvm.txt`
//!    (keeping the `#` header, and updating the SHA-256 line to match the new
//!    jar). Note: if the jar changes, the class will almost certainly be
//!    obfuscated to a different short name than the current one — obfuscated
//!    names are jar-build-specific, not stable across builds. Rediscover it
//!    with the grep-then-decompile method documented in
//!    `FlatteningOracle.java`'s class doc before touching anything else.
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-canonical --test flattening \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_canonical::flattening::{self, LegacyBlockState};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/flattening.rs")
}

/// The committed JVM dump — an external anchor, not gitignored.
const DUMP: &str = include_str!("support/flattening_1_13_2_jvm.txt");

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PropPair(String, String);

#[derive(Debug, Clone)]
enum DumpSlot {
    Undefined,
    Resolved { name: String, properties: Vec<PropPair> },
}

/// Old id `140` (flower pot): every one of its 16 metas collapses to the same
/// placeholder `potted_cactus` in vanilla's own table — the contained plant
/// is a block-entity field, not derivable from meta at all. Verified against
/// the dump (see `docs/protocol-340-flattening-table.md`).
const FLOWER_POT_OLD_ID: u8 = 140;

/// Old id `175` (double plant), metas `8..=11`: the upper half of a double
/// plant. Vanilla's own table returns a single fixed species (`peony`) for
/// all four upper-half metas — plausible-looking but wrong, since the real
/// species is read from the paired lower-half block at conversion time, not
/// stored in the upper half's own meta. No mechanical sentinel marks this
/// one (unlike skulls' `%%FILTER_ME%%`), so it is hardcoded here from the
/// verified dump content rather than detected structurally.
const DOUBLE_PLANT_OLD_ID: u8 = 175;
const DOUBLE_PLANT_UPPER_METAS: std::ops::RangeInclusive<u8> = 8..=11;

/// Vanilla's own internal placeholder for "could not resolve" (skulls: type
/// and rotation are block-entity fields). Detected mechanically wherever it
/// appears in the dump, rather than hardcoded by id.
const FILTER_ME_SENTINEL: &str = "%%FILTER_ME%%";

fn parse_dump(text: &str) -> Vec<DumpSlot> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let n_str = cols.next().expect("index column");
        let _n: usize = n_str.parse().expect("index is a usize");
        let Some(name) = cols.next() else {
            rows.push(DumpSlot::Undefined);
            continue;
        };
        let mut properties = Vec::new();
        if let Some(props_str) = cols.next() {
            for kv in props_str.split(',') {
                let (k, v) = kv.split_once('=').unwrap_or_else(|| {
                    panic!("malformed property {kv:?} on line {line:?}")
                });
                properties.push(PropPair(k.to_owned(), v.to_owned()));
            }
        }
        assert!(cols.next().is_none(), "unexpected trailing column on {line:?}");
        rows.push(DumpSlot::Resolved {
            name: name.to_owned(),
            properties,
        });
    }
    rows
}

/// A slot's classification for table-generation purposes — mirrors
/// `flattening::LegacyBlockState` minus `OutOfBounds` (handled separately by
/// `flattening::lookup`, never stored in the generated table).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Classified {
    NoTableEntry,
    RequiresContext,
    Resolved { name: String, properties: Vec<PropPair> },
}

fn classify(index: usize, slot: &DumpSlot) -> Classified {
    let old_id = (index / 16) as u8;
    let meta = (index % 16) as u8;
    match slot {
        DumpSlot::Undefined => Classified::NoTableEntry,
        DumpSlot::Resolved { name, properties } => {
            if name == FILTER_ME_SENTINEL {
                return Classified::RequiresContext;
            }
            if old_id == FLOWER_POT_OLD_ID {
                return Classified::RequiresContext;
            }
            if old_id == DOUBLE_PLANT_OLD_ID && DOUBLE_PLANT_UPPER_METAS.contains(&meta) {
                return Classified::RequiresContext;
            }
            Classified::Resolved {
                name: name.clone(),
                properties: properties.clone(),
            }
        }
    }
}

/// Renders the committed `src/generated/flattening.rs` source from the
/// parsed dump.
fn generate(rows: &[DumpSlot]) -> String {
    let classified: Vec<Classified> = rows
        .iter()
        .enumerate()
        .map(|(i, s)| classify(i, s))
        .collect();

    // De-duplicate distinct (name, properties) pairs, numbered in ascending
    // slot order (same scheme as v26-2's hardness/collision-shape tables).
    let mut entry_index: BTreeMap<(String, Vec<PropPair>), usize> = BTreeMap::new();
    let mut distinct: Vec<(String, Vec<PropPair>)> = Vec::new();
    let mut slot_entry: Vec<Option<usize>> = Vec::with_capacity(classified.len());
    for c in &classified {
        match c {
            Classified::Resolved { name, properties } => {
                let key = (name.clone(), properties.clone());
                let idx = *entry_index.entry(key.clone()).or_insert_with(|| {
                    distinct.push(key);
                    distinct.len() - 1
                });
                slot_entry.push(Some(idx));
            }
            _ => slot_entry.push(None),
        }
    }

    // Flatten all distinct entries' properties into one shared array,
    // recording each entry's (start, len) into it.
    let mut properties_flat: Vec<PropPair> = Vec::new();
    let mut entry_offsets: Vec<(usize, usize)> = Vec::with_capacity(distinct.len());
    for (_, props) in &distinct {
        let start = properties_flat.len();
        properties_flat.extend(props.iter().cloned());
        entry_offsets.push((start, props.len()));
    }

    let no_table_entry_count = classified
        .iter()
        .filter(|c| matches!(c, Classified::NoTableEntry))
        .count();
    let requires_context_count = classified
        .iter()
        .filter(|c| matches!(c, Classified::RequiresContext))
        .count();
    let resolved_slot_count = classified
        .iter()
        .filter(|c| matches!(c, Classified::Resolved { .. }))
        .count();

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-canonical --test flattening -- --ignored`\n\
         // from tests/support/flattening_1_13_2_jvm.txt (a reflective dump of the real\n\
         // 1.13.2 server jar's own world-upgrade flattening step, protocol 340 /\n\
         // Minecraft 1.12.2 -> block state). DO NOT EDIT BY HAND. Regenerate with\n\
         // LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    let _ = writeln!(
        out,
        "//! Generated pre-Flattening `id:meta` -> block-state table for protocol 340\n\
         //! (Minecraft 1.12.2). Consumed by [`crate::flattening`].\n\
         //!\n\
         //! {} of {} slots resolve to a single modern state ({} distinct states); {}\n\
         //! have no table entry at all (vanilla itself falls back to air, this table\n\
         //! does not); {} require block-entity/neighbor data this table cannot supply.",
        resolved_slot_count,
        classified.len(),
        distinct.len(),
        no_table_entry_count,
        requires_context_count,
    );
    out.push('\n');

    let _ = writeln!(
        out,
        "/// Number of `(old_block_id, meta)` slots covered by [`SLOTS`] (see\n\
         /// `crate::flattening`'s module docs for the one slot this excludes)."
    );
    let _ = writeln!(out, "pub const SLOT_COUNT: usize = {};\n", classified.len());

    let _ = writeln!(out, "/// A single classified slot. See `crate::flattening::LegacyBlockState`.");
    let _ = writeln!(out, "#[derive(Clone, Copy, Debug, PartialEq, Eq)]");
    let _ = writeln!(out, "pub enum Slot {{");
    let _ = writeln!(out, "    NoTableEntry,");
    let _ = writeln!(out, "    RequiresContext,");
    let _ = writeln!(out, "    Resolved(u16),");
    let _ = writeln!(out, "}}\n");

    let _ = writeln!(
        out,
        "/// One resolved `(name, properties-range-into-PROPERTIES)` entry, {} of them,\n\
         /// de-duplicated across all resolved slots.",
        distinct.len()
    );
    let _ = writeln!(out, "pub struct ResolvedEntry {{");
    let _ = writeln!(out, "    pub name: &'static str,");
    let _ = writeln!(out, "    pub properties_start: u16,");
    let _ = writeln!(out, "    pub properties_len: u8,");
    let _ = writeln!(out, "}}\n");

    let _ = writeln!(out, "pub static RESOLVED: [ResolvedEntry; {}] = [", distinct.len());
    for (i, (name, _)) in distinct.iter().enumerate() {
        let (start, len) = entry_offsets[i];
        let _ = writeln!(
            out,
            "    ResolvedEntry {{ name: {name:?}, properties_start: {start}, properties_len: {len} }},"
        );
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Flat, shared `(key, value)` property storage referenced by [`ResolvedEntry`]\n\
         /// via `properties_start`/`properties_len`, {} pairs total.",
        properties_flat.len()
    );
    let _ = writeln!(
        out,
        "pub static PROPERTIES: [(&str, &str); {}] = [",
        properties_flat.len()
    );
    for PropPair(k, v) in &properties_flat {
        let _ = writeln!(out, "    ({k:?}, {v:?}),");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-slot classification, indexed by `old_block_id * 16 + meta`."
    );
    let _ = writeln!(out, "pub static SLOTS: [Slot; SLOT_COUNT] = [");
    for (i, c) in classified.iter().enumerate() {
        let rendered = match c {
            Classified::NoTableEntry => "Slot::NoTableEntry".to_owned(),
            Classified::RequiresContext => "Slot::RequiresContext".to_owned(),
            Classified::Resolved { .. } => {
                format!("Slot::Resolved({})", slot_entry[i].expect("resolved slot has an entry index"))
            }
        };
        let _ = writeln!(out, "    {rendered}, // {i}");
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (anchored to the committed dump)
// ---------------------------------------------------------------------------

#[test]
fn committed_table_matches_the_committed_dump() {
    let rows = parse_dump(DUMP);
    assert_eq!(rows.len(), flattening::SLOT_COUNT, "dump/table slot count mismatch");

    for (index, slot) in rows.iter().enumerate() {
        let old_id = (index / 16) as u8;
        let meta = (index % 16) as u8;
        let looked_up = flattening::lookup(old_id, meta);
        let expected = classify(index, slot);
        match (&expected, looked_up) {
            (Classified::NoTableEntry, LegacyBlockState::NoTableEntry) => {}
            (Classified::RequiresContext, LegacyBlockState::RequiresAdditionalContext) => {}
            (Classified::Resolved { name, properties }, LegacyBlockState::Resolved(resolved)) => {
                assert_eq!(resolved.name, name, "name mismatch at old_id={old_id} meta={meta}");
                let mut got: Vec<(String, String)> = resolved
                    .properties
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect();
                got.sort();
                let mut want: Vec<(String, String)> =
                    properties.iter().map(|p| (p.0.clone(), p.1.clone())).collect();
                want.sort();
                assert_eq!(got, want, "properties mismatch at old_id={old_id} meta={meta}");
            }
            (expected, got) => panic!(
                "slot {index} (old_id={old_id} meta={meta}) classification mismatch: dump says {expected:?}, table says {got:?}"
            ),
        }
    }
}

#[test]
fn out_of_bounds_slot_is_reported_distinctly() {
    assert_eq!(flattening::lookup(255, 15), LegacyBlockState::OutOfBounds);
    // Adjacent, in-bounds structure_block metas are ordinary resolved slots.
    assert!(matches!(flattening::lookup(255, 3), LegacyBlockState::Resolved(_)));
}

#[test]
fn flower_pot_contents_require_additional_context() {
    for meta in 0..16u8 {
        assert_eq!(
            flattening::lookup(FLOWER_POT_OLD_ID, meta),
            LegacyBlockState::RequiresAdditionalContext,
            "flower pot meta {meta} should require additional context"
        );
    }
}

#[test]
fn skull_type_and_rotation_require_additional_context() {
    // Old id 144, metas 0-5 and 8-13 are the defined skull-type slots (6, 7,
    // 14, 15 are simply undefined -- five skull types times {nodrop false,
    // true}); all of them hit vanilla's own `%%FILTER_ME%%` placeholder.
    for meta in [0u8, 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13] {
        assert_eq!(
            flattening::lookup(144, meta),
            LegacyBlockState::RequiresAdditionalContext,
            "skull meta {meta} should require additional context"
        );
    }
}

#[test]
fn double_plant_upper_half_requires_additional_context() {
    for meta in DOUBLE_PLANT_UPPER_METAS {
        assert_eq!(
            flattening::lookup(DOUBLE_PLANT_OLD_ID, meta),
            LegacyBlockState::RequiresAdditionalContext,
            "double plant upper-half meta {meta} should require additional context"
        );
    }
    // The lower half (metas 0-5) IS resolvable from meta alone.
    for meta in 0u8..6 {
        assert!(
            matches!(flattening::lookup(DOUBLE_PLANT_OLD_ID, meta), LegacyBlockState::Resolved(_)),
            "double plant lower-half meta {meta} should resolve"
        );
    }
}

#[test]
fn undefined_metadata_is_not_silently_treated_as_air() {
    // Old id 0 (air) only ever had meta 0 in real 1.12.2 data; metas 1-15 are
    // simply never assigned in vanilla's own table. A caller must not get
    // back "air" for these, because that would be indistinguishable from a
    // real air block.
    for meta in 1u8..16 {
        assert_eq!(
            flattening::lookup(0, meta),
            LegacyBlockState::NoTableEntry,
            "air id with meta {meta} should have no table entry, not silently resolve"
        );
    }
    assert!(matches!(flattening::lookup(0, 0), LegacyBlockState::Resolved(_)));
}

#[test]
fn stone_variants_resolve_to_distinct_states() {
    let stone = flattening::lookup(1, 0);
    let granite = flattening::lookup(1, 1);
    assert!(matches!(stone, LegacyBlockState::Resolved(_)));
    assert!(matches!(granite, LegacyBlockState::Resolved(_)));
    let (LegacyBlockState::Resolved(stone), LegacyBlockState::Resolved(granite)) = (stone, granite) else {
        unreachable!()
    };
    assert_eq!(stone.name, "minecraft:stone");
    assert_eq!(granite.name, "minecraft:granite");
    assert_ne!(stone.name, granite.name);
}

#[test]
fn oak_stairs_metadata_encodes_orientation_not_identity() {
    // Old id 53 (oak_stairs): all 8 defined metas resolve to the SAME block
    // name with DIFFERENT `facing`/`half` properties -- metadata here is
    // orientation, not a family split, unlike stone's metadata above.
    let mut names = std::collections::HashSet::new();
    for meta in 0u8..8 {
        let LegacyBlockState::Resolved(resolved) = flattening::lookup(53, meta) else {
            panic!("oak_stairs meta {meta} should resolve");
        };
        names.insert(resolved.name);
        assert!(
            !resolved.properties.is_empty(),
            "oak_stairs meta {meta} should carry orientation properties"
        );
    }
    assert_eq!(names.len(), 1, "all oak_stairs metas should share one block name");
}

// ---------------------------------------------------------------------------
// Drift guard (regenerates from the committed dump)
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
        "src/generated/flattening.rs is stale vs the JVM dump; regenerate with LODESTONE_REGEN=1"
    );
}
