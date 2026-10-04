//! Exact 26.2 state light inputs, compact generation, and bounded captured witnesses.
//!
//! The ignored drift guard requires `LODESTONE_LIGHT_PROPS_DUMP` to point at a
//! complete `LightPropertiesOracle` capture. It checks its pinned SHA-256 and
//! joins every ID, name, and property set to the official 26.2 `blocks.json`
//! report and the canonical table before generating. Capture instructions and
//! the scalar solver boundary are in `docs/block-light-inputs.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use lodestone_data::block_states::{self, StateId};
use lodestone_data::light_props;

const EXPECTED_STATES: usize = 32_366;
const EXPECTED_BLOCKS: usize = 1_196;
const CAPTURE_HEADER: &str = "# LightPropertiesOracle 26.2 protocol=776";
const CAPTURE_SHA256: &str = "6ec5b31daeab2cbdab643ba2f696ebd042aca0683a6e10b92c219ae12e9f8d2b";
const WITNESSES: &str = include_str!("support/light_props_witnesses.txt");

#[derive(Debug)]
struct Row {
    id: usize,
    name: String,
    dampening: u8,
    emission: u8,
    properties: Vec<(String, String)>,
}

fn parse_row(line: &str) -> Result<Row, String> {
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!("expected five state columns: {line:?}"));
    }
    let id = fields[0].parse::<usize>().map_err(|_| "invalid state id")?;
    let dampening = fields[2].parse::<u8>().map_err(|_| "invalid dampening")?;
    let emission = fields[3].parse::<u8>().map_err(|_| "invalid emission")?;
    if dampening > 15 || emission > 15 {
        return Err(format!("state {id}: light input outside 0..=15"));
    }
    let mut properties = Vec::new();
    if fields[4] != "-" {
        for property in fields[4].split(',') {
            let (key, value) = property.split_once('=').ok_or("invalid property")?;
            if key.is_empty() || value.is_empty() || value.contains('=') {
                return Err(format!("state {id}: invalid property {property:?}"));
            }
            if properties.last().is_some_and(|(previous, _): &(String, String)| {
                previous.as_str() >= key
            }) {
                return Err(format!("state {id}: properties must have unique sorted keys"));
            }
            properties.push((key.to_owned(), value.to_owned()));
        }
    }
    Ok(Row {
        id,
        name: fields[1].to_owned(),
        dampening,
        emission,
        properties,
    })
}

fn parse_dump(text: &str, counts: (usize, usize)) -> Result<Vec<Row>, String> {
    if text.lines().next() != Some(CAPTURE_HEADER) {
        return Err("capture must identify Minecraft 26.2 / protocol 776".to_owned());
    }
    let census = format!("C {} {}", counts.0, counts.1);
    let trailer = format!("E {} {}", counts.0, counts.1);
    let mut started = false;
    let mut completed = false;
    let mut rows = Vec::new();
    let mut signatures = BTreeSet::new();
    let mut names = BTreeSet::new();
    for line in text.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
        if completed {
            return Err("data after completion trailer".to_owned());
        }
        if !started {
            if line != census {
                return Err(format!("expected census {census:?}, got {line:?}"));
            }
            started = true;
            continue;
        }
        if line == trailer {
            completed = true;
            continue;
        }
        let row = parse_row(line)?;
        if row.id != rows.len() {
            return Err(format!("state {}: expected dense ID {}", row.id, rows.len()));
        }
        if !signatures.insert((row.name.clone(), row.properties.clone())) {
            return Err(format!("duplicate semantic state at {}", row.id));
        }
        names.insert(row.name.clone());
        rows.push(row);
    }
    if !completed || rows.len() != counts.0 || names.len() != counts.1 {
        return Err(format!("incomplete census: {} states / {} blocks", rows.len(), names.len()));
    }
    Ok(rows)
}

fn check_canonical_identity(row: &Row) -> Result<(), String> {
    let id = u32::try_from(row.id).map_err(|_| "state id exceeds u32")?;
    if block_states::block_name(id) != Some(row.name.as_str()) {
        return Err(format!("state {}: canonical block name disagrees", row.id));
    }
    let properties: Vec<_> = block_states::properties(id)
        .ok_or("state absent from canonical table")?
        .iter()
        .map(|&(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    if row.properties != properties {
        return Err(format!("state {}: canonical properties disagree", row.id));
    }
    Ok(())
}

fn check_report_identity(rows: &[Row], report: &serde_json::Value) {
    let blocks = report.as_object().expect("official blocks report is an object");
    assert_eq!(blocks.len(), EXPECTED_BLOCKS);
    let mut states = BTreeMap::new();
    for (name, block) in blocks {
        for state in block["states"].as_array().expect("block states are an array") {
            let id = usize::try_from(state["id"].as_u64().expect("report state id")).unwrap();
            let properties: Vec<(String, String)> = state.get("properties")
                .map(|value| {
                    let mut properties: Vec<_> = value.as_object().expect("report properties")
                        .iter()
                        .map(|(key, value)| (key.clone(), value.as_str().unwrap().to_owned()))
                        .collect();
                    properties.sort_unstable();
                    properties
                })
                .unwrap_or_default();
            assert!(states.insert(id, (name, properties)).is_none(), "duplicate report ID {id}");
        }
    }
    assert_eq!(states.len(), EXPECTED_STATES);
    for row in rows {
        let (name, properties) = states.get(&row.id).expect("capture ID in official report");
        assert_eq!(*name, &row.name, "state {}: report block name", row.id);
        assert_eq!(properties, &row.properties, "state {}: report properties", row.id);
        check_canonical_identity(row).unwrap();
    }
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn capture_digest(path: &Path) -> String {
    let output = Command::new("shasum").arg("-a").arg("256").arg(path)
        .output().expect("shasum is required for the ignored capture verification");
    assert!(output.status.success(), "shasum failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().split_whitespace()
        .next().expect("SHA-256 output").to_owned()
}

fn generate(rows: &[Row]) -> String {
    let mut entry_index = BTreeMap::new();
    let mut distinct = Vec::new();
    let mut state_entry = Vec::with_capacity(rows.len());
    for row in rows {
        let key = (row.dampening, row.emission);
        let index = *entry_index.entry(key).or_insert_with(|| {
            distinct.push(key);
            distinct.len() - 1
        });
        state_entry.push(index);
    }
    assert!(distinct.len() <= usize::from(u8::MAX) + 1);
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test light_props -- --ignored`\n\
         // from a complete LightPropertiesOracle 26.2 server-state capture.\n",
    );
    writeln!(out, "// Capture SHA-256: {CAPTURE_SHA256}").unwrap();
    out.push_str(
        "// DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see tests/light_props.rs).\n\
         //! Generated per-block-state light table for protocol 776 (Minecraft 26.2),\n\
         //! indexed by canonical block-state id. Consumed by [`crate::light_props`].\n\n",
    );
    writeln!(out, "/// Number of block states (ids are `0..STATE_COUNT`).").unwrap();
    writeln!(out, "pub const STATE_COUNT: u32 = {};\n", rows.len()).unwrap();
    writeln!(out,
        "/// De-duplicated distinct `(dampening, emission)` pairs ({} of them),\n\
         /// indexed by entry index. Both values are `0..=15`.", distinct.len()).unwrap();
    writeln!(out, "pub static ENTRIES: [(u8, u8); {}] = [", distinct.len()).unwrap();
    for (dampening, emission) in distinct {
        writeln!(out, "    ({dampening}, {emission}),").unwrap();
    }
    out.push_str("];\n\n");
    out.push_str("/// Per-state entry index into [`ENTRIES`], indexed by canonical block-state id.\n");
    writeln!(out, "pub static STATE_ENTRY: [u8; {}] = [", rows.len()).unwrap();
    for chunk in state_entry.chunks(32) {
        out.push_str("    ");
        for index in chunk {
            write!(out, "{index}, ").unwrap();
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");
    out
}

#[test]
fn census_and_total_lookup_match_canonical_states() {
    assert_eq!(light_props::STATE_COUNT, 35_723);
    assert_eq!(light_props::STATE_COUNT, block_states::STATE_COUNT);
    for raw in 0..light_props::STATE_COUNT {
        let id = StateId::new(raw).unwrap();
        let (dampening, emission) = light_props::light_props(id);
        assert!(dampening <= 15 && emission <= 15, "state {raw}");
        assert_eq!(light_props::dampening(id), dampening);
        assert_eq!(light_props::emission(id), emission);
    }
    assert!(StateId::new(light_props::STATE_COUNT).is_none());
    assert!(StateId::new(u32::MAX).is_none());
}

#[test]
fn captured_state_witnesses_match_exact_inputs() {
    assert!(WITNESSES.lines().any(|line| line == format!("# capture-sha256 {CAPTURE_SHA256}")));
    let mut seen = BTreeSet::new();
    for line in WITNESSES.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let row = parse_row(line).unwrap();
        assert!(seen.insert(row.id), "duplicate witness {}", row.id);
        check_canonical_identity(&row).unwrap();
        let id = StateId::new(row.id as u32).unwrap();
        assert_eq!(light_props::light_props(id), (row.dampening, row.emission),
            "{} {:?} (state {})", row.name, row.properties, row.id);
    }
    assert_eq!(seen.len(), 40);
}

#[test]
fn capture_controls_reject_truncation_wrong_ids_and_semantic_duplicates() {
    let control = format!("{CAPTURE_HEADER}\nC 2 2\n0 minecraft:air 0 0 -\n1 minecraft:stone 15 0 -\nE 2 2\n");
    assert!(parse_dump(&control, (2, 2)).is_ok());
    assert!(parse_dump(&control.replace("E 2 2\n", ""), (2, 2)).is_err());
    assert!(parse_dump(&control.replace("1 minecraft:stone", "2 minecraft:stone"), (2, 2)).is_err());
    assert!(parse_dump(&control.replace("1 minecraft:stone", "1 minecraft:air"), (2, 2)).is_err());
    assert!(parse_dump(&control.replace("26.2 protocol=776", "26.3 protocol=777"), (2, 2)).is_err());
    let wrong_identity = parse_row("0 minecraft:stone 15 0 -").unwrap();
    assert!(check_canonical_identity(&wrong_identity).is_err());
    let wrong_properties = parse_row("0 minecraft:air 0 0 lit=true").unwrap();
    assert!(check_canonical_identity(&wrong_properties).is_err());
}

#[test]
#[ignore = "requires a complete pinned JVM capture and the official 26.2 blocks report"]
fn committed_table_matches_source() {
    include!("support/base-only-generation.rs");
    assert_eq!(block_states::STATE_COUNT as usize, EXPECTED_STATES);
    let path = PathBuf::from(std::env::var_os("LODESTONE_LIGHT_PROPS_DUMP")
        .expect("set LODESTONE_LIGHT_PROPS_DUMP to the complete 26.2 capture"));
    let capture = std::fs::read_to_string(&path).expect("read light-properties capture");
    let rows = parse_dump(&capture, (EXPECTED_STATES, EXPECTED_BLOCKS))
        .expect("complete 26.2 light-properties capture");
    assert_eq!(capture_digest(&path), CAPTURE_SHA256, "capture provenance changed");
    let report_path = manifest_dir().join("../../.cache/mc/26.2/generated/reports/blocks.json");
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report_path).expect("official 26.2 blocks report"),
    ).expect("valid official report");
    check_report_identity(&rows, &report);
    let generated = generate(&rows);
    let committed_path = manifest_dir().join("src/generated/light_props.rs");
    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(&committed_path, generated).expect("write generated light table");
        eprintln!("regenerated {}", committed_path.display());
    } else {
        assert_eq!(generated, std::fs::read_to_string(committed_path).unwrap(),
            "light table is stale; regenerate with LODESTONE_REGEN=1");
    }
}
