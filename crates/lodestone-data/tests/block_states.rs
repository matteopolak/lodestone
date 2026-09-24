//! Block-state table: hermetic consistency checks over the committed table, plus
//! an `#[ignore]`d drift guard that regenerates it from the Mojang blocks report
//! and asserts byte-for-byte equality (modelled on `xtask gen-packet-ids
//! --check`). The generator lives here so the checked-in table can never
//! silently drift from the game data.
//!
//! Regenerate the committed table after a data bump with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test block_states \
//!     committed_table_matches_report -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_model::BlockStateRegistry;
use lodestone_data::block::Block;
use lodestone_data::block_states::{self, BlockStateTable};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn report_path() -> PathBuf {
    manifest_dir().join("../../.cache/mc/26.2/generated/reports/blocks.json")
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/block_states.rs")
}

// ---------------------------------------------------------------------------
// Generator (shared by regen and the drift check)
// ---------------------------------------------------------------------------

/// A collected block state: `(id, block index, sorted property pairs)`.
type CollectedState = (usize, usize, Vec<(String, String)>);

/// The sorted `(name, value)` property pairs of one state, or empty for a block
/// with no properties.
fn props_of(state: &serde_json::Value) -> Vec<(String, String)> {
    let Some(object) = state.get("properties").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, String)> = object
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_str()
                    .expect("property value is a string")
                    .to_owned(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// Names in the order `STATES` uses for its block-index field.
fn alphabetical_block_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut names: Vec<&str> = names.into_iter().collect();
    names.sort_unstable();
    names
}

/// Renders the committed `block_states.rs` source from the parsed report.
///
/// Deterministic: block names are explicitly sorted before their positions
/// become state-table indices, and property sets are de-duplicated and indexed
/// in sorted order.
fn generate(doc: &serde_json::Value) -> String {
    let object = doc.as_object().expect("blocks.json is a JSON object");

    let block_names = alphabetical_block_names(object.keys().map(String::as_str));
    let mut registry_ids = Vec::with_capacity(block_names.len());
    for &name in &block_names {
        let block = Block::from_name(name).unwrap_or_else(|| {
            panic!("state report block `{name}` is absent from the generated block registry")
        });
        assert_eq!(
            block.name(),
            name,
            "state report block `{name}` mismatches the generated canonical block registry"
        );
        registry_ids.push(block.registry_id());
    }
    assert_eq!(
        block_names.len(),
        Block::COUNT as usize,
        "block-state report has {} block names; generated block registry has {}",
        block_names.len(),
        Block::COUNT
    );
    registry_ids.sort_unstable();
    assert_eq!(
        registry_ids,
        (0..Block::COUNT).collect::<Vec<_>>(),
        "block-state report names do not cover each generated registry id exactly once"
    );
    let block_index: BTreeMap<&str, usize> = block_names
        .iter()
        .enumerate()
        .map(|(index, &name)| (name, index))
        .collect();

    // Collect (id, block index, sorted properties) and the distinct property
    // sets. `distinct` is a BTreeMap so its key order — and hence the assigned
    // set indices — are deterministic.
    let mut distinct: BTreeMap<Vec<(String, String)>, ()> = BTreeMap::new();
    let mut collected: Vec<CollectedState> = Vec::new();
    let mut max_id = 0usize;
    for (name, info) in object {
        let bi = block_index[name.as_str()];
        for state in info["states"].as_array().expect("states is an array") {
            let id = state["id"].as_u64().expect("state id is an integer") as usize;
            let props = props_of(state);
            distinct.insert(props.clone(), ());
            collected.push((id, bi, props));
            max_id = max_id.max(id);
        }
    }

    let count = collected.len();
    assert_eq!(
        max_id + 1,
        count,
        "block-state ids are not dense: max id {max_id}, count {count}"
    );

    let set_index: BTreeMap<&Vec<(String, String)>, usize> = distinct
        .keys()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect();

    let mut states: Vec<Option<(usize, usize)>> = vec![None; count];
    for (id, bi, props) in &collected {
        let si = set_index[props];
        assert!(states[*id].is_none(), "duplicate block-state id {id}");
        states[*id] = Some((*bi, si));
    }
    let states: Vec<(usize, usize)> = states
        .into_iter()
        .map(|slot| slot.expect("every id in 0..count is present"))
        .collect();

    // --- emit -------------------------------------------------------------
    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test block_states -- --ignored`\n\
         // from .cache/mc/26.2/generated/reports/blocks.json (protocol 776 / Minecraft 26.2).\n\
         // DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n",
    );
    out.push_str("//! Generated block-state id table for protocol 776 (Minecraft 26.2).\n//!\n");
    out.push_str(
        "//! Raw rodata arrays consumed by [`crate::block_states`]; every string is a\n\
         //! `&'static str` and every entry is a small integer, so the whole table lives in\n\
         //! rodata with zero heap.\n\n",
    );

    let _ = writeln!(
        out,
        "/// Number of block states in the vanilla global palette (ids are `0..STATE_COUNT`)."
    );
    let _ = writeln!(out, "pub const STATE_COUNT: u32 = {count};\n");

    let _ = writeln!(
        out,
        "/// De-duplicated property sets (sorted `(name, value)` pairs), indexed by set index."
    );
    let _ = writeln!(
        out,
        "pub static PROPERTY_SETS: [&[(&str, &str)]; {}] = [",
        distinct.len()
    );
    for set in distinct.keys() {
        if set.is_empty() {
            out.push_str("    &[],\n");
        } else {
            out.push_str("    &[");
            for (index, (key, value)) in set.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "({key:?}, {value:?})");
            }
            out.push_str("],\n");
        }
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Per-state `(alphabetical block-name index, property-set index)`, indexed by\n\
         /// block-state id. Resolve the first field through\n\
         /// [`crate::generated_block_enum::REGISTRY_IDS_BY_NAME`] into\n\
         /// [`crate::generated_block_registry::BLOCK_REGISTRY_NAMES`]; it is not a\n\
         /// `minecraft:block` registration id."
    );
    let _ = writeln!(out, "pub static STATES: [(u16, u16); {count}] = [");
    for chunk in states.chunks(12) {
        out.push_str("    ");
        for (bi, si) in chunk {
            let _ = write!(out, "({bi}, {si}), ");
        }
        out.pop(); // trailing space
        out.push('\n');
    }
    out.push_str("];\n");

    out
}

fn one_block_report(name: &str) -> serde_json::Value {
    serde_json::json!({
        name: {
            "states": [{ "id": 0 }]
        }
    })
}

/// A name-keyed report may preserve insertion order. The reverse-order input
/// makes the alphabetical-index contract observable rather than inheriting the
/// JSON map implementation's current iteration behavior.
#[test]
fn generator_explicitly_sorts_block_names_before_assigning_state_indices() {
    assert_eq!(
        alphabetical_block_names(["minecraft:stone", "minecraft:air"]),
        ["minecraft:air", "minecraft:stone"],
        "report insertion order must not become a STATES block index"
    );
}

#[test]
#[should_panic(expected = "is absent from the generated block registry")]
fn generator_rejects_a_state_block_unknown_to_the_canonical_registry() {
    let _ = generate(&one_block_report("minecraft:not_a_block"));
}

#[test]
#[should_panic(expected = "block-state report has 1 block names; generated block registry has 1196")]
fn generator_requires_exact_canonical_block_coverage() {
    let _ = generate(&one_block_report("minecraft:air"));
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (no report needed)
// ---------------------------------------------------------------------------

#[test]
fn ids_are_contiguous_and_out_of_range_is_none() {
    let count = block_states::STATE_COUNT;
    assert!(count > 0, "committed table is empty; run the regen test");
    for id in 0..count {
        assert!(
            block_states::block_name(id).is_some(),
            "id {id} in 0..{count} did not resolve to a block name"
        );
        assert!(block_states::properties(id).is_some());
    }
    assert!(block_states::block_name(count).is_none());
    assert!(block_states::properties(count).is_none());
    assert!(block_states::block_name(u32::MAX).is_none());
}

#[test]
fn known_ids_resolve_to_the_right_blocks() {
    // These ids are cross-checked against the live 26.2 server's flat world in
    // `live_chunk` (bedrock=85, dirt=10, grass=9), closing the loop between the
    // static table and real wire data without a network round-trip here.
    assert_eq!(block_states::block_name(0), Some("minecraft:air"));
    assert_eq!(block_states::properties(0), Some(&[][..]));
    assert_eq!(block_states::block_name(1), Some("minecraft:stone"));
    assert_eq!(block_states::block_name(85), Some("minecraft:bedrock"));
    assert_eq!(block_states::block_name(10), Some("minecraft:dirt"));

    assert_eq!(block_states::block_name(9), Some("minecraft:grass_block"));
    assert_eq!(
        block_states::properties(9),
        Some(&[("snowy", "false")][..]),
        "id 9 is the default (non-snowy) grass block"
    );
    assert_eq!(block_states::properties(8), Some(&[("snowy", "true")][..]));
}

#[test]
fn typed_state_identity_is_total_and_extension_names_stay_at_the_parse_boundary() {
    let air = block_states::air_state();
    assert_eq!(air.name(), "minecraft:air");
    assert_eq!(air.properties(), &[]);
    assert_eq!(air.raw(), block_states::air_state_id());
    assert_eq!(block_states::StateId::from_state_str("minecraft:air"), Some(air));
    assert_eq!(
        block_states::StateId::from_state_str("lodestone:polished_test_stone"),
        None,
        "a namespaced extension must remain available to its owning registry rather than becoming a built-in state"
    );
}

#[test]
fn registry_trait_matches_the_static_accessors() {
    let table = BlockStateTable::new();
    assert_eq!(table.state_count(), block_states::STATE_COUNT);

    for id in [
        0u32,
        1,
        8,
        9,
        10,
        85,
        10780,
        block_states::STATE_COUNT.saturating_sub(1),
    ] {
        let resolved = table.resolve(id).expect("known id resolves");
        assert_eq!(
            resolved.block.to_string(),
            block_states::block_name(id).unwrap()
        );
        // The owned BTreeMap must carry exactly the static property pairs.
        let statics = block_states::properties(id).unwrap();
        assert_eq!(resolved.properties.len(), statics.len());
        for (key, value) in statics {
            assert_eq!(
                resolved.properties.get(*key).map(String::as_str),
                Some(*value)
            );
        }
    }
    assert!(table.resolve(block_states::STATE_COUNT).is_none());
}

// ---------------------------------------------------------------------------
// Drift guard + corpus report (requires the jar cache)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "requires .cache/mc/26.2 blocks report; regenerates and checks the committed table"]
fn committed_table_matches_report() {
    let raw = std::fs::read_to_string(report_path())
        .expect("blocks.json present under .cache/mc/26.2/generated/reports");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("blocks.json parses");
    let generated = generate(&doc);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/block_states.rs is stale vs blocks.json; regenerate with LODESTONE_REGEN=1"
    );

    // --- corpus report ----------------------------------------------------
    let object = doc.as_object().unwrap();
    let report_states: usize = object
        .values()
        .map(|b| b["states"].as_array().unwrap().len())
        .sum();
    let report_blocks = object.len();
    let mut max_id = 0u32;
    let mut distinct = std::collections::BTreeSet::new();
    for id in 0..block_states::STATE_COUNT {
        max_id = id;
        distinct.insert(block_states::properties(id).unwrap());
    }
    let table = BlockStateTable::new();

    println!("=== BLOCK-STATE TABLE REPORT ===");
    println!("blocks (report)          : {report_blocks}");
    println!(
        "states (report / table)  : {report_states} / {}",
        block_states::STATE_COUNT
    );
    println!(
        "max id + 1 == count      : {} + 1 == {} -> {}",
        max_id,
        block_states::STATE_COUNT,
        max_id + 1 == block_states::STATE_COUNT
    );
    println!("distinct property sets   : {}", distinct.len());
    println!("materialised heap (trait): {} bytes", table.heap_bytes());
    println!("================================");

    assert_eq!(report_states, block_states::STATE_COUNT as usize);
    assert_eq!(max_id + 1, block_states::STATE_COUNT);
}
