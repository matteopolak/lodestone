//! The entity-type tables, checked against the wire transcript they came from
//! and cross-checked against `minecraft-data`.
//!
//! # Why the generated tables are not simply trusted
//!
//! Protocol 5 numbers **objects and mobs in two separate id spaces**, and they
//! overlap: 50 is a creeper in one and primed TNT in the other, 63 is an ender
//! dragon in one and a fireball in the other, 66 a witch and a wither skull.
//! Reading an id through the wrong table therefore names a real, wrong entity
//! rather than failing, which no round-trip and no schema check can catch.
//!
//! So the tables come from a wire transcript
//! (`captures/entity_types_1_7_10.txt`): each name was summoned on a real
//! server through its own command interface and the id read off the spawn
//! packet that followed. The tests here assert that the generated tables still
//! agree with that transcript, and separately that they agree with
//! `minecraft-data`'s independent 1.7 dataset — two sources that were not
//! derived from each other, and neither of them from this crate.
//!
//! The `minecraft-data` cross-check reads `vendor/` **at run time**, never
//! through `include_str!`: `vendor/` is git-ignored, so a compile-time read
//! makes the test binary unbuildable wherever it is absent, which is exactly
//! what a CI checkout is. When it is missing the cross-check reports what it
//! skipped rather than passing silently.
//!
//! # Regenerating
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.7.10
//! # re-record the transcript, then:
//! LODESTONE_REGEN=1 cargo test -p lodestone-v1-7 --test entity_types -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use lodestone_v1_7::entity_types;

/// How a transcript row classifies its subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Space {
    /// Spawned through the living-entity packet, so the id is a mob id.
    Living,
    /// Spawned through the object packet, so the id is an object id.
    Object,
    /// Has a spawn packet of its own that carries no type field at all.
    Untyped,
    /// The server refused the name, which is evidence about the era rather
    /// than a gap in the transcript.
    Refused,
}

/// One transcript row.
#[derive(Debug, Clone)]
struct Row {
    name: String,
    space: Space,
    id: Option<i32>,
}

fn transcript_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("captures")
        .join("entity_types_1_7_10.txt")
}

fn read_transcript() -> Vec<Row> {
    let path = transcript_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The name is everything before the space keyword, not the first
        // whitespace-separated token: a refused row records the name exactly
        // as it was typed, and some of those contain a space.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let keyword_at = tokens
            .iter()
            .position(|token| {
                matches!(*token, "living" | "object" | "untyped" | "refused")
            })
            .unwrap_or_else(|| panic!("transcript row {line:?} names no id space"));
        assert!(keyword_at > 0, "transcript row {line:?} has no name");
        let name = tokens[..keyword_at].join(" ");
        let space = match tokens[keyword_at] {
            "living" => Space::Living,
            "object" => Space::Object,
            "untyped" => Space::Untyped,
            "refused" => Space::Refused,
            other => unreachable!("the position scan already accepted {other:?}"),
        };
        let id = match space {
            Space::Living | Space::Object => Some(
                tokens
                    .get(keyword_at + 1)
                    .unwrap_or_else(|| panic!("typed transcript row {name:?} has no id"))
                    .parse()
                    .unwrap_or_else(|err| {
                        panic!("transcript row {name:?} has a non-integer id: {err}")
                    }),
            ),
            Space::Untyped | Space::Refused => None,
        };
        rows.push(Row { name, space, id });
    }
    rows
}

/// The two tables the crate ships, as `(id, name)` maps.
fn shipped_tables() -> (BTreeMap<i32, String>, BTreeMap<i32, String>) {
    let mut mobs = BTreeMap::new();
    let mut objects = BTreeMap::new();
    // Walked rather than read out of the statics directly, so the test
    // exercises the same public lookups production uses. The upper bound
    // covers every id either space uses with room to spare.
    for id in 0..256 {
        if let Some(name) = entity_types::mob_type_name(id) {
            mobs.insert(id, name.to_owned());
        }
        if let Some(name) = entity_types::object_type_name(id) {
            objects.insert(id, name.to_owned());
        }
    }
    (mobs, objects)
}

/// Groups a transcript's typed rows by id, per space.
///
/// One id can carry **several** names here, and that is a fact about the era
/// rather than a recording artefact: every minecart variant shares one entity
/// type, with the variant living in entity metadata. So the tables are checked
/// for id coverage and for naming inside the recorded set, not against a
/// one-name-per-id map that the wire does not have.
fn transcript_by_id(rows: &[Row], space: Space) -> BTreeMap<i32, Vec<String>> {
    let mut out: BTreeMap<i32, Vec<String>> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.space == space) {
        if let Some(id) = row.id {
            out.entry(id).or_default().push(row.name.clone());
        }
    }
    out
}

#[test]
fn the_shipped_tables_cover_exactly_the_ids_the_wire_produced() {
    let rows = read_transcript();
    let (mobs, objects) = shipped_tables();
    let expected_mobs = transcript_by_id(&rows, Space::Living);
    let expected_objects = transcript_by_id(&rows, Space::Object);

    assert!(
        !expected_mobs.is_empty() && !expected_objects.is_empty(),
        "the transcript produced no rows in one of the two spaces, so this test would pass \
         vacuously: {} mob id(s), {} object id(s)",
        expected_mobs.len(),
        expected_objects.len()
    );
    assert_eq!(
        mobs.keys().copied().collect::<Vec<_>>(),
        expected_mobs.keys().copied().collect::<Vec<_>>(),
        "the shipped mob table's ids no longer match the transcript's"
    );
    assert_eq!(
        objects.keys().copied().collect::<Vec<_>>(),
        expected_objects.keys().copied().collect::<Vec<_>>(),
        "the shipped object table's ids no longer match the transcript's"
    );

    for (id, name) in &mobs {
        assert!(
            expected_mobs[id].contains(name),
            "mob {id} ships as {name} but the wire recorded {:?} for it",
            expected_mobs[id]
        );
    }
    for (id, name) in &objects {
        assert!(
            expected_objects[id].contains(name),
            "object {id} ships as {name} but the wire recorded {:?} for it",
            expected_objects[id]
        );
    }
}

/// The one id in this era that several names share, kept as its own test so
/// the fact is asserted rather than merely tolerated by the check above.
#[test]
fn every_minecart_variant_shares_one_object_type() {
    let rows = read_transcript();
    let carts: Vec<&Row> = rows
        .iter()
        .filter(|row| {
            row.space == Space::Object && row.name.starts_with("minecraft:minecart")
        })
        .collect();
    assert!(
        carts.len() >= 7,
        "the transcript recorded only {} minecart variant(s), too few to establish the shared \
         type",
        carts.len()
    );
    let ids: std::collections::BTreeSet<Option<i32>> =
        carts.iter().map(|row| row.id).collect();
    assert_eq!(
        ids.len(),
        1,
        "the minecart variants no longer share one object type: {ids:?}. If that is real, the \
         adapter needs a per-variant path; if it is a recording error, the transcript is wrong."
    );
    // And the shipped name for that shared id must be the base one, not one
    // arbitrary variant: a chest minecart reported as a TNT minecart is worse
    // than one reported as an unspecified minecart.
    let shared = carts[0].id.expect("a typed row has an id");
    assert_eq!(
        lodestone_v1_7::entity_types::object_type_name(shared),
        Some("minecraft:minecart"),
        "object {shared} is shared by every variant, so it must ship under the base name"
    );
}

/// The property that makes the two-table split load-bearing: reading an id
/// through the wrong space names a real, wrong entity.
#[test]
fn the_two_id_spaces_genuinely_collide() {
    let (mobs, objects) = shipped_tables();
    let collisions: Vec<i32> = mobs
        .keys()
        .filter(|id| objects.contains_key(id))
        .copied()
        .collect();
    assert!(
        collisions.len() >= 3,
        "only {} colliding id(s) ({collisions:?}) -- if the spaces stopped overlapping, the \
         separate tables would be bookkeeping rather than a correctness requirement",
        collisions.len()
    );
    for id in collisions {
        assert_ne!(
            mobs[&id], objects[&id],
            "id {id} names the same thing in both spaces, so the collision is harmless there"
        );
    }
}

/// A refused row is evidence about the era, so it must not have quietly
/// become a table entry under some other spelling.
#[test]
fn a_refused_name_is_absent_from_both_tables() {
    let rows = read_transcript();
    let refused: Vec<&Row> = rows
        .iter()
        .filter(|row| row.space == Space::Refused)
        .collect();
    assert!(
        !refused.is_empty(),
        "the transcript records no refusal, so this test proves nothing -- a real recording \
         against this era refuses several names that later versions have"
    );
    let (mobs, objects) = shipped_tables();
    for row in refused {
        let snake = row.name.to_lowercase().replace(' ', "_");
        let candidate = format!("minecraft:{snake}");
        assert!(
            !mobs.values().any(|name| *name == candidate)
                && !objects.values().any(|name| *name == candidate),
            "{:?} was refused by the server but {candidate} is in a shipped table",
            row.name
        );
    }
}

// ---------------------------------------------------------------------------
// Cross-check against minecraft-data's independent dataset.
// ---------------------------------------------------------------------------

/// Runtime path to `minecraft-data`'s own 1.7 entity list.
///
/// Read at run time, never through `include_str!` — see the module docs.
fn minecraft_data_entities() -> Option<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("vendor/minecraft-data/data/pc/1.7/entities.json");
    std::fs::read_to_string(path).ok()
}

#[test]
fn minecraft_data_agrees_with_the_wire_transcript_on_every_id_it_carries() {
    let Some(text) = minecraft_data_entities() else {
        eprintln!(
            "skipped: vendor/minecraft-data/data/pc/1.7/entities.json is absent, so the \
             independent cross-check did not run. The transcript-versus-table tests above are \
             unaffected; only the second source is missing."
        );
        return;
    };

    // Parsed with a deliberately narrow scan rather than a schema: the file is
    // a flat array of objects and only three of their fields matter here.
    let mut dataset: Vec<(i32, String, String)> = Vec::new();
    for chunk in text.split('{').skip(1) {
        let field = |key: &str| -> Option<String> {
            let needle = format!("\"{key}\"");
            let at = chunk.find(&needle)? + needle.len();
            let rest = chunk[at..].trim_start().strip_prefix(':')?.trim_start();
            if let Some(quoted) = rest.strip_prefix('"') {
                Some(quoted[..quoted.find('"')?].to_owned())
            } else {
                let end = rest
                    .find(|c: char| !c.is_ascii_digit() && c != '-')
                    .unwrap_or(rest.len());
                Some(rest[..end].to_owned())
            }
        };
        let (Some(id), Some(name), Some(kind)) = (field("id"), field("name"), field("type")) else {
            continue;
        };
        let Ok(id) = id.parse::<i32>() else { continue };
        dataset.push((id, name, kind));
    }
    assert!(
        dataset.len() > 20,
        "the narrow scan extracted only {} row(s) from minecraft-data, so it is broken rather \
         than the tables being wrong",
        dataset.len()
    );

    let rows = read_transcript();
    let by_name: BTreeMap<String, &Row> = rows
        .iter()
        .filter(|row| row.id.is_some())
        .map(|row| {
            (
                row.name
                    .strip_prefix("minecraft:")
                    .unwrap_or(&row.name)
                    .to_owned(),
                row,
            )
        })
        .collect();

    // The dataset gives some names **several** ids -- it lists three rows for
    // the rideable minecart (10, 11, 12) and two for a falling block (70, 74),
    // where a real server of this era only ever uses one of each. So a name is
    // compared as a set: the wire's id must be among the dataset's ids for
    // that name. Demanding equality would fail on the dataset's own
    // duplication rather than on a misread, and that duplication is precisely
    // why this source is a cross-check and the wire is the authority.
    let mut dataset_ids: BTreeMap<(String, String), Vec<i32>> = BTreeMap::new();
    for (id, name, kind) in &dataset {
        dataset_ids
            .entry((camel_to_snake(name), kind.clone()))
            .or_default()
            .push(*id);
    }

    let mut compared = 0usize;
    let mut multi_id_names = Vec::new();
    let mut disagreements = String::new();
    for ((snake, kind), ids) in &dataset_ids {
        let Some(row) = by_name.get(snake) else {
            continue;
        };
        // Only rows in the matching space are comparable: the dataset's own
        // `type` field says which id space the number belongs to.
        if !matches!(
            (kind.as_str(), row.space),
            ("mob", Space::Living) | ("object", Space::Object)
        ) {
            continue;
        }
        compared += 1;
        if ids.len() > 1 {
            multi_id_names.push((snake.clone(), ids.clone()));
        }
        if !row.id.is_some_and(|id| ids.contains(&id)) {
            let _ = writeln!(
                disagreements,
                "  {snake}: the wire said {:?}, minecraft-data offers {ids:?}",
                row.id
            );
        }
    }

    assert!(
        compared >= 20,
        "only {compared} name(s) were comparable between the two sources, so this cross-check is \
         too thin to mean anything -- most likely the name-shape conversion stopped matching"
    );
    assert!(
        disagreements.is_empty(),
        "the wire transcript and minecraft-data disagree on {compared} compared name(s). The \
         wire is the authority; a disagreement means one of them was misread.\n{disagreements}"
    );
    // Reported rather than asserted away: the duplication is a property of the
    // dataset a later reader should know about, and its disappearance would
    // mean the dataset changed under us.
    assert!(
        !multi_id_names.is_empty(),
        "minecraft-data no longer gives any comparable name more than one id. That contradicts \
         what this cross-check was written against, so re-derive rather than relaxing it."
    );
    eprintln!(
        "cross-checked {compared} name(s) against minecraft-data; {} of them carry several ids \
         there and matched on set membership: {multi_id_names:?}",
        multi_id_names.len()
    );
}

/// Converts this era's CamelCase entity name to the snake_case the tables use.
///
/// Acronym-aware, because a naive per-capital split turns a name like `TNT`
/// into `t_n_t`; the transcript's own spelling is what this has to reproduce.
fn camel_to_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let previous_lower = i > 0 && chars[i - 1].is_ascii_lowercase();
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let previous_upper = i > 0 && chars[i - 1].is_ascii_uppercase();
            if i > 0 && (previous_lower || (previous_upper && next_lower)) {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn the_name_shape_conversion_handles_the_acronym_case() {
    // The case that motivated the acronym rule: a naive split gives `t_n_t`.
    assert_eq!(camel_to_snake("PrimedTnt"), "primed_tnt");
    assert_eq!(camel_to_snake("EntityHorse"), "entity_horse");
    assert_eq!(camel_to_snake("Creeper"), "creeper");
    assert_eq!(camel_to_snake("XPOrb"), "xp_orb");
}

// ---------------------------------------------------------------------------
// Regenerator.
// ---------------------------------------------------------------------------

/// Rewrites `src/generated/entity_types.rs` from the committed transcript.
///
/// Runs only under `LODESTONE_REGEN=1`, and otherwise reports that it did
/// nothing rather than passing silently — a generator that quietly no-ops is
/// how a "regenerated" table ends up being the old one.
///
/// The one judgement it makes is the shared-type case: several names map to
/// one object id, and the table ships the shortest of them, which is the base
/// name rather than an arbitrary variant. Everything else is a transcription.
#[test]
fn regenerate_the_generated_tables() {
    if std::env::var("LODESTONE_REGEN").as_deref() != Ok("1") {
        eprintln!(
            "skipped: set LODESTONE_REGEN=1 to rewrite src/generated/entity_types.rs from \
             tests/captures/entity_types_1_7_10.txt"
        );
        return;
    }

    let rows = read_transcript();
    let mobs = pick_names(&transcript_by_id(&rows, Space::Living));
    let objects = pick_names(&transcript_by_id(&rows, Space::Object));
    assert!(
        !mobs.is_empty() && !objects.is_empty(),
        "the transcript yielded no rows in one space; refusing to write an empty table"
    );

    let mut out = String::new();
    out.push_str(
        "// @generated by `LODESTONE_REGEN=1 cargo test -p lodestone-v1-7 --test entity_types`\n\
         // from tests/captures/entity_types_1_7_10.txt, a transcript of a real\n\
         // Minecraft 1.7.10 (protocol 5) server's own wire. DO NOT EDIT BY HAND.\n",
    );
    out.push_str(
        "//! Generated entity-type id->name tables for protocol 5 (Minecraft 1.7.6-1.7.10).\n\
         //!\n\
         //! Protocol 5 numbers mobs and objects in two **separate** id spaces, so these\n\
         //! are two independent tables and an id means nothing without knowing which\n\
         //! spawn packet carried it. Consumed by [`crate::entity_types`].\n\
         //!\n\
         //! Unlike the 1.8 era's equivalent, these tables are generated from a wire\n\
         //! transcript rather than from `minecraft-data`, and the dataset is the\n\
         //! cross-check rather than the source. `tests/entity_types.rs` records what\n\
         //! that comparison found.\n",
    );
    write_table(
        &mut out,
        "MOB",
        "mob",
        "/// Every row was read off a real server's wire. `minecraft-data`'s own 1.7\n\
         /// mob list agrees with all of them, id for id.\n",
        &mobs,
    );
    write_table(
        &mut out,
        "OBJECT",
        "object",
        "/// Every row was read off a real server's wire.\n\
         ///\n\
         /// Id 10 is shared by every minecart variant: the wire carries the same\n\
         /// object type for a rideable, chest, furnace, TNT, hopper, spawner and\n\
         /// command-block minecart, and the variant travels in entity metadata. The\n\
         /// base name owns the row so a lookup cannot claim a specificity the wire\n\
         /// did not carry.\n\
         ///\n\
         /// The fishing bobber (object 90 in `minecraft-data`) is deliberately\n\
         /// absent: this era's command interface refuses its name, so no wire\n\
         /// evidence for it exists and the dataset alone is not authority enough.\n",
        &objects,
    );

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("generated")
        .join("entity_types.rs");
    std::fs::write(&path, out).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    eprintln!(
        "wrote {} mob and {} object row(s) to {}",
        mobs.len(),
        objects.len(),
        path.display()
    );
}

/// Collapses each id's recorded names to the one the table ships.
///
/// The shortest name wins, which for the shared minecart type is the base
/// `minecraft:minecart` rather than whichever variant happened to sort first.
fn pick_names(by_id: &BTreeMap<i32, Vec<String>>) -> Vec<(i32, String)> {
    by_id
        .iter()
        .map(|(id, names)| {
            let mut sorted = names.clone();
            sorted.sort_by_key(|name| (name.len(), name.clone()));
            (*id, sorted[0].clone())
        })
        .collect()
}

fn write_table(out: &mut String, prefix: &str, label: &str, note: &str, rows: &[(i32, String)]) {
    let _ = write!(
        out,
        "\n/// Number of {label} type entries.\npub const {prefix}_TYPES_COUNT: usize = {};\n",
        rows.len()
    );
    out.push_str("\n/// `(type id, canonical identifier)` pairs, sorted by id for binary search.\n");
    out.push_str("///\n");
    out.push_str(note);
    let _ = write!(
        out,
        "pub static {prefix}_TYPES: [(i32, &str); {}] = [\n",
        rows.len()
    );
    for (id, name) in rows {
        let _ = writeln!(out, "    ({id}, \"{name}\"),");
    }
    out.push_str("];\n");
}
