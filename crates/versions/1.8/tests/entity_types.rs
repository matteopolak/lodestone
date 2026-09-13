//! Entity-type id→name tables for protocol 47: hermetic checks over the
//! committed tables, plus an `#[ignore]`d drift guard that regenerates them from
//! the vendored `minecraft-data` project and asserts byte-for-byte equality
//! (modelled on the v26-2 entity-type table and the packet-id generator).
//!
//! Unlike the modern crates, protocol 47 predates Mojang's data generator, so
//! there is no authoritative `registries.json`. The oracle is the
//! community-maintained `minecraft-data` project
//! (`vendor/minecraft-data/data/pc/1.8/entities.json`, gitignored like the rest
//! of the vendored data). It records **two** id spaces — `type: "mob"` and
//! `type: "object"` — which map to `spawn_entity_living` and `spawn_entity`
//! respectively.
//!
//! **Judgement call, recorded rather than buried:** `minecraft-data`'s 1.8
//! `name` fields are the game's *legacy internal* CamelCase names, which are
//! neither the wire strings nor the modern resource keys. We snake_case them
//! verbatim (`PigZombie` → `pig_zombie`, `LavaSlime` → `lava_slime`, `Ozelot`
//! → `ozelot`), so the identifiers describe what 1.8 actually spawns without
//! pretending its ids map onto the modern registry. Several ids also share a
//! name (`MinecartRideable` at 10/11/12, `FallingSand` at 70/74); the table is
//! keyed by id so this is faithful, not lossy.
//!
//! Regenerate the committed tables after a data bump with:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-8 --test entity_types \
//!     committed_tables_match_source -- --ignored --nocapture
//! ```

use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_v1_8::entity_types::{self, MOB_TYPE_COUNT, OBJECT_TYPE_COUNT};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Vendored community data (gitignored local artifact).
fn source_path() -> PathBuf {
    manifest_dir().join("../../../vendor/minecraft-data/data/pc/1.8/entities.json")
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/entity_types.rs")
}

// ---------------------------------------------------------------------------
// Generator (shared by regen and the drift check)
// ---------------------------------------------------------------------------

/// Converts a `minecraft-data` legacy name to a canonical lowercase path:
/// spaces become underscores and CamelCase word boundaries gain an underscore,
/// e.g. `PigZombie` → `pig_zombie`, `Fishing Float` → `fishing_float`.
fn snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let mut prev_lower_or_digit = false;
    for ch in name.chars() {
        if ch == ' ' || ch == '-' {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            prev_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() {
            if prev_lower_or_digit {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower_or_digit = false;
        } else {
            out.push(ch);
            prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

/// Parses `entities.json` into a sorted `(id, identifier)` table for one
/// `type` category (`"mob"` or `"object"`).
fn parse_category(doc: &serde_json::Value, category: &str) -> Vec<(i32, String)> {
    let entries = doc.as_array().expect("entities.json is an array");
    let mut pairs: Vec<(i32, String)> = entries
        .iter()
        .filter(|entry| entry.get("type").and_then(serde_json::Value::as_str) == Some(category))
        .map(|entry| {
            let id = entry
                .get("id")
                .and_then(serde_json::Value::as_i64)
                .expect("entity entry has an integer id") as i32;
            let name = entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .expect("entity entry has a name");
            (id, format!("minecraft:{}", snake_case(name)))
        })
        .collect();
    pairs.sort_by_key(|(id, _)| *id);
    // Distinct ids within a category are required for a keyed lookup; a
    // collision would mean two spawns are indistinguishable on the wire.
    for window in pairs.windows(2) {
        assert_ne!(
            window[0].0, window[1].0,
            "duplicate {category} id {} in entities.json",
            window[0].0
        );
    }
    pairs
}

fn render_table(out: &mut String, const_name: &str, count_name: &str, pairs: &[(i32, String)]) {
    let _ = writeln!(
        out,
        "/// Number of {} type entries.",
        const_name.to_lowercase()
    );
    let _ = writeln!(out, "pub const {count_name}: usize = {};\n", pairs.len());
    let _ = writeln!(
        out,
        "/// `(type id, canonical identifier)` pairs, sorted by id for binary search."
    );
    let _ = writeln!(
        out,
        "pub static {const_name}: [(i32, &str); {}] = [",
        pairs.len()
    );
    for (id, name) in pairs {
        let _ = writeln!(out, "    ({id}, \"{name}\"),");
    }
    out.push_str("];\n\n");
}

fn generate(doc: &serde_json::Value) -> String {
    let mobs = parse_category(doc, "mob");
    let objects = parse_category(doc, "object");

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-v1-8 --test entity_types -- --ignored`\n\
         // from vendor/minecraft-data/data/pc/1.8/entities.json (protocol 47 /\n\
         // Minecraft 1.8.x). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1\n\
         // (see the test module docs).\n",
    );
    out.push_str(
        "//! Generated entity-type id->name tables for protocol 47 (Minecraft 1.8.x).\n//!\n",
    );
    out.push_str(
        "//! Maps the two 1.8 numeric entity-type id spaces (mob and object) to their\n\
         //! canonical `minecraft:*` identifiers. Consumed by [`crate::entity_types`].\n\n",
    );

    render_table(&mut out, "MOB_TYPES", "MOB_TYPE_COUNT", &mobs);
    render_table(&mut out, "OBJECT_TYPES", "OBJECT_TYPE_COUNT", &objects);

    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed tables (no source needed)
// ---------------------------------------------------------------------------

#[test]
fn tables_are_non_empty_and_out_of_range_is_none() {
    assert!(
        entity_types::mob_type_name(90).is_some(),
        "mob table populated"
    );
    assert!(
        entity_types::object_type_name(1).is_some(),
        "object table populated"
    );
    assert_eq!(entity_types::mob_type_name(-1), None);
    assert_eq!(entity_types::mob_type_name(i32::MAX), None);
    assert_eq!(entity_types::object_type_name(-1), None);
    assert_eq!(entity_types::object_type_name(i32::MAX), None);
    // The two id spaces are genuinely distinct: id 50 is a creeper as a mob but
    // primed TNT as an object. A single shared table would collapse them.
    assert_eq!(entity_types::mob_type_name(50), Some("minecraft:creeper"));
    assert_eq!(
        entity_types::object_type_name(50),
        Some("minecraft:primed_tnt")
    );
}

#[test]
fn known_ids_resolve_to_well_formed_identifiers() {
    // Spot ids the live seam relies on; a transposed table fails instantly.
    assert_eq!(entity_types::mob_type_name(90), Some("minecraft:pig"));
    assert_eq!(entity_types::mob_type_name(54), Some("minecraft:zombie"));
    // The recorded 1.8-vs-modern naming divergence is intentional.
    assert_eq!(
        entity_types::mob_type_name(57),
        Some("minecraft:pig_zombie")
    );
    assert_eq!(entity_types::object_type_name(60), Some("minecraft:arrow"));
    assert_eq!(entity_types::object_type_name(1), Some("minecraft:boat"));
    assert_eq!(entity_types::PLAYER, "minecraft:player");
}

#[test]
fn identifiers_have_lowercase_paths() {
    for id in 0..256 {
        for name in [
            entity_types::mob_type_name(id),
            entity_types::object_type_name(id),
        ]
        .into_iter()
        .flatten()
        {
            let path = name
                .strip_prefix("minecraft:")
                .expect("namespaced identifier");
            assert!(!path.is_empty(), "id {id} has an empty path");
            assert!(
                path.chars().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '_' | '.' | '-' | '/')),
                "id {id} identifier {name:?} has an invalid path character"
            );
            // Every identifier must parse as a real ResourceKey.
            let _key: lodestone_model::ResourceKey = name.parse().expect("valid resource key");
        }
    }
}

// ---------------------------------------------------------------------------
// Drift guard (needs the vendored source; #[ignore]d so the suite stays hermetic)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "reads the gitignored vendor/minecraft-data; run explicitly to regen/verify"]
fn committed_tables_match_source() {
    let raw = std::fs::read_to_string(source_path())
        .expect("entities.json present under vendor/minecraft-data/data/pc/1.8");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("entities.json parses");
    let generated = generate(&doc);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed tables");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed tables present");
    assert_eq!(
        generated, committed,
        "src/generated/entity_types.rs is stale vs entities.json; regenerate with LODESTONE_REGEN=1"
    );

    // Whole-corpus cross-check through the public accessor.
    let mobs = parse_category(&doc, "mob");
    let objects = parse_category(&doc, "object");
    assert_eq!(mobs.len(), MOB_TYPE_COUNT, "mob count mismatch");
    assert_eq!(objects.len(), OBJECT_TYPE_COUNT, "object count mismatch");
    let mut mismatches = 0usize;
    for (id, name) in &mobs {
        if entity_types::mob_type_name(*id) != Some(name.as_str()) {
            mismatches += 1;
        }
    }
    for (id, name) in &objects {
        if entity_types::object_type_name(*id) != Some(name.as_str()) {
            mismatches += 1;
        }
    }
    println!("=== ENTITY-TYPE TABLE REPORT (protocol 47) ===");
    println!("mob types    : {MOB_TYPE_COUNT}");
    println!("object types : {OBJECT_TYPE_COUNT}");
    println!("mismatches   : {mismatches}");
    println!("==============================================");
    assert_eq!(
        mismatches, 0,
        "committed tables disagree with entities.json"
    );
}
