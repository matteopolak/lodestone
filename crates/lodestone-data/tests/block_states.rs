//! Block-state identities: hermetic checks over the compiled tables and a
//! read-only drift guard for the three offline-generated block identity files.
//! Generate reviewed source with `tools/identity_staging.py --scope base
//! --rust-output <private-directory>`; the drift guard never rewrites identities.
//!
//! ```text
//! cargo test -p lodestone-data --test block_states \
//!     committed_table_matches_report -- --ignored --nocapture
//! ```

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
fn canonical_defaults_match_official_numeric_witnesses() {
    assert_eq!(Block::GrassBlock.default_state().raw(), 9);
    assert_eq!(Block::OakLog.default_state().raw(), 137);
    assert_eq!(Block::Water.default_state().raw(), 86);
    assert_eq!(block_states::state_id("minecraft:oak_log[axis=x]"), Some(136));
    assert_eq!(block_states::state_id("minecraft:oak_log"), Some(137));
    assert_eq!(block_states::state_id("minecraft:oak_log[axis=z]"), Some(138));
    assert_eq!(block_states::state_id("oak_log"), None);
    assert_eq!(block_states::state_id("oak_log[axis=y]"), None);
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
#[ignore = "requires both official report caches and Python; checks identity files without writing"]
fn committed_table_matches_report() {
    let output = std::process::Command::new("python3")
        .arg(manifest_dir().join("tools/identity_staging.py"))
        .args(["--scope", "base", "--runtime-check"])
        .output()
        .expect("run the offline identity drift check");
    assert!(
        output.status.success(),
        "offline identity check failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = std::fs::read_to_string(report_path())
        .expect("blocks.json present under .cache/mc/26.2/generated/reports");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("blocks.json parses");
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
