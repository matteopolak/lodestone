//! Legacy particle-id → modern-key table for protocol 340: hermetic checks
//! over the hand-curated table in `src/particle_ids.rs`, plus an `#[ignore]`d
//! validity guard against the vendored `minecraft-data` sources.
//!
//! Unlike `tests/sound_ids.rs` or `tests/entity_types.rs`, this table is
//! **not** byte-for-byte regenerable from a single upstream file — see
//! `src/particle_ids.rs`'s module docs for why (a genuine cross-era rename,
//! derived by decompiling `.cache/mc/1.13.2/server.jar` under `container`,
//! not by transforming JSON). So this guard checks what a drift guard *can*
//! check without re-deriving the rename by hand each run:
//!
//! * every legacy id `0..=48` from `vendor/minecraft-data`'s
//!   `data/pc/1.12/particles.json` is accounted for by the table (either a
//!   real key or a documented `None`);
//! * the legacy id space is still exactly `0..=48` — a future `minecraft-data`
//!   bump changing that would silently invalidate every id above the change;
//! * every non-`None` key the table returns is one of the modern registry
//!   names in `data/pc/1.13/particles.json` — catching a typo or a stale key
//!   if the modern registry is ever regenerated, even though it cannot catch
//!   a *wrong but valid* rename (that needs the jar, not this file).
//!
//! Regenerating the *mapping itself* after a `minecraft-data` bump means
//! re-running the `container`/`vineflower` disassembly in `src/particle_ids.rs`'s
//! module docs, not editing this test.

use std::path::PathBuf;

use lodestone_v1_9::particle_ids::particle_key;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn legacy_source_path() -> PathBuf {
    manifest_dir().join("../../../vendor/minecraft-data/data/pc/1.12/particles.json")
}

fn modern_source_path() -> PathBuf {
    manifest_dir().join("../../../vendor/minecraft-data/data/pc/1.13/particles.json")
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed table (no vendored source needed)
// ---------------------------------------------------------------------------

#[test]
fn out_of_range_ids_are_none() {
    assert_eq!(particle_key(-1), None);
    assert_eq!(particle_key(49), None);
    assert_eq!(particle_key(i32::MAX), None);
}

#[test]
fn every_in_range_id_is_either_a_valid_key_or_a_documented_miss() {
    const UNMAPPED: [i32; 7] = [7, 8, 22, 28, 32, 39, 40];
    for id in 0..=48 {
        match particle_key(id) {
            Some(name) => {
                assert!(!UNMAPPED.contains(&id), "id {id} was expected unmapped but resolved to {name}");
                let _key: lodestone_model::ResourceKey =
                    name.parse().expect("resolved name is a valid resource key");
            }
            None => {
                assert!(UNMAPPED.contains(&id), "id {id} unexpectedly has no modern key");
            }
        }
    }
}

#[test]
fn known_verified_entries_resolve_as_derived() {
    // The explosion trio, the one case with three similarly-named modern
    // candidates and no forced elimination — settled by decompiling
    // `Explosion`'s own method. See `src/particle_ids.rs`.
    assert_eq!(particle_key(0), Some("minecraft:poof"));
    assert_eq!(particle_key(1), Some("minecraft:explosion"));
    assert_eq!(particle_key(2), Some("minecraft:explosion_emitter"));
    // The merged block-debris pair.
    assert_eq!(particle_key(37), Some("minecraft:block"));
    assert_eq!(particle_key(38), Some("minecraft:block"));
    assert_eq!(particle_key(36), Some("minecraft:item"));
}

// ---------------------------------------------------------------------------
// Validity guard (needs the vendored sources; #[ignore]d so the suite stays
// hermetic)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "reads the gitignored vendor/minecraft-data; run explicitly to verify"]
fn table_covers_the_legacy_id_space_and_targets_real_modern_keys() {
    let legacy_raw = std::fs::read_to_string(legacy_source_path())
        .expect("particles.json present under vendor/minecraft-data/data/pc/1.12");
    let legacy: serde_json::Value = serde_json::from_str(&legacy_raw).expect("parses");
    let legacy_entries = legacy.as_array().expect("array");

    let mut max_id = -1i64;
    for entry in legacy_entries {
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .expect("legacy entry has an integer id");
        max_id = max_id.max(id);
    }
    assert_eq!(
        legacy_entries.len() as i64,
        max_id + 1,
        "legacy 1.12 particle id space is no longer contiguous 0..=N — re-derive the table"
    );
    assert_eq!(
        max_id, 48,
        "legacy 1.12 particle id space changed size — re-derive the table for the new ids"
    );

    let modern_raw = std::fs::read_to_string(modern_source_path())
        .expect("particles.json present under vendor/minecraft-data/data/pc/1.13");
    let modern: serde_json::Value = serde_json::from_str(&modern_raw).expect("parses");
    let modern_names: std::collections::HashSet<&str> = modern
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .expect("modern entry has a name")
        })
        .collect();

    let mut checked = 0usize;
    for id in 0..=max_id as i32 {
        if let Some(name) = particle_key(id) {
            let bare = name.strip_prefix("minecraft:").unwrap_or(name);
            assert!(
                modern_names.contains(bare),
                "id {id} resolves to {name}, which is not in the 1.13 particle registry"
            );
            checked += 1;
        }
    }
    println!("=== PARTICLE-ID TABLE REPORT (protocol 340) ===");
    println!("legacy ids     : {}", max_id + 1);
    println!("mapped         : {checked}");
    println!("unmapped       : {}", (max_id + 1) as usize - checked);
    println!("=================================================");
}
