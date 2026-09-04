//! A mechanical brake on new string-typed registry columns in generated tables.
//!
//! # Why this exists as a check and not as a convention
//!
//! The rule it enforces — *a generated column that names a registry entry should
//! hold a typed id, not a `&'static str`* — is the kind of rule this repo has
//! measured itself violating while the rule sat written down in a doc comment in
//! the very crate it governed. So it is spelled as an allowlist that a new
//! column must be added to before the crate's tests pass.
//!
//! # What it does and does not see
//!
//! It scans `src/generated/*.rs` for `pub static` arrays whose element type
//! mentions `&str`, and requires each to appear in [`ALLOWED`] with a
//! [`Kind`] and a reason. It is deliberately narrow:
//!
//! * It sees **generated columns only**. A hand-written `String` field on a
//!   struct is not covered — that needs real parsing, and a hand-rolled Rust
//!   lexer in this repo has been wrong about lifetimes three times. The
//!   generated tables are where 62% of the debt lives, so this is where the
//!   lever is.
//! * It is a **two-way** diff, not a count. A column that disappears fails just
//!   as loudly as one that appears, so the allowlist cannot rot into a stale
//!   snapshot of a list that has since grown — the failure mode a parity gate in
//!   this repo shipped for weeks.
//! * It **fails if it finds nothing**. "Scanned no files" and "found no
//!   violations" must never share a verdict.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Why a `&str` column is allowed to stay a `&str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The registry's own canonical name column — exactly one per registry, and
    /// the right and only place for these strings to live. A typed id resolves
    /// *through* it.
    CanonicalNames,
    /// A genuinely open string space with no registry behind it: block property
    /// keys and values, translation keys.
    OpenStringSpace,
    /// A column that names an entry of some registry as a string. **These are
    /// the debt.** Each one should become a typed id; the note says which
    /// registry it should key on.
    CrossReference,
    /// A second copy of a registry's names, in a different order. Should resolve
    /// through the canonical column plus a permutation instead.
    DuplicateNames,
}

use Kind::{CanonicalNames, CrossReference, DuplicateNames, OpenStringSpace};

/// Every `&str`-typed generated column, with its verdict.
///
/// Adding a generated string column means adding a row here, which is the point:
/// classifying it is a deliberate act. `CrossReference` and `DuplicateNames`
/// rows are the migration queue, in table-size order.
const ALLOWED: &[(&str, &str, Kind, &str)] = &[
    ("attribute_types.rs", "ATTRIBUTE_NAMES", CanonicalNames, "the minecraft:attribute registry"),
    ("block_blast.rs", "BY_NAME", CrossReference, "block name -> index; should be a permutation of block registry ids, as block_enum's REGISTRY_IDS_BY_NAME already is"),
    ("block_entity_types.rs", "TYPE_NAMES", CanonicalNames, "the minecraft:block_entity_type registry"),
    ("block_registry.rs", "BLOCK_REGISTRY_NAMES", CanonicalNames, "the minecraft:block registry, in registration order; Block::name reads this"),
    ("block_states.rs", "BLOCK_NAMES", DuplicateNames, "a second, name-sorted copy of the block names; should become a permutation over BLOCK_REGISTRY_NAMES"),
    ("block_states.rs", "PROPERTY_SETS", OpenStringSpace, "block property keys and values are not a registry"),
    ("damage_types.rs", "DAMAGE_TYPE_NAMES", CanonicalNames, "the minecraft:damage_type registry"),
    ("damage_types.rs", "DAMAGE_TYPE_MESSAGE_IDS", OpenStringSpace, "translation keys, not registry entries"),
    ("damage_types.rs", "DAMAGE_TYPE_TAG_NAMES", CanonicalNames, "the damage-type tag registry"),
    ("data_component_types.rs", "DATA_COMPONENT_TYPE_NAMES", CanonicalNames, "the minecraft:data_component_type registry"),
    ("entity_types.rs", "ENTITY_TYPE_NAMES", CanonicalNames, "the minecraft:entity_type registry"),
    ("items.rs", "ITEM_NAMES", CanonicalNames, "the minecraft:item registry"),
    ("menus.rs", "MENU_NAMES", CanonicalNames, "the minecraft:menu registry"),
    ("mob_effects.rs", "MOB_EFFECT_NAMES", CanonicalNames, "the minecraft:mob_effect registry"),
    ("particle_types.rs", "PARTICLE_TYPE_NAMES", CanonicalNames, "the minecraft:particle_type registry"),
    ("potion_effect_keys.rs", "POTION_EFFECT_KEYS", CrossReference, "vanilla's own potion-name accessor collapses every long_/strong_ variant of one potion onto the same key (e.g. swiftness/long_swiftness/strong_swiftness -> \"swiftness\"); keys into the same minecraft:potion path space POTION_NAMES already carries, just many-to-one"),
    ("potions.rs", "POTION_NAMES", CanonicalNames, "the minecraft:potion registry"),
    ("sound_events.rs", "SOUND_EVENT_NAMES", CanonicalNames, "the minecraft:sound_event registry"),
    ("tools.rs", "BLOCK_TAGS", CanonicalNames, "the block-tag registry; its members are already typed u16 block ids"),
    ("tools.rs", "ITEM_TOOLS", CrossReference, "keyed by item name; should key on a minecraft:item registry id once Item is generated"),
];

fn generated_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/generated")
}

/// Every `(file, symbol)` in `src/generated` whose `pub static` array element
/// type mentions `&str`.
fn scan() -> BTreeMap<(String, String), String> {
    let dir = generated_dir();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()));

    let mut files = 0usize;
    let mut found = BTreeMap::new();
    for entry in entries {
        let path = entry.expect("readable dir entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        files += 1;
        let file = path
            .file_name()
            .expect("named file")
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("pub static ") else {
                continue;
            };
            let Some((symbol, declaration)) = rest.split_once(':') else {
                continue;
            };
            // The element type is everything up to the initialiser.
            let element = declaration.split(" = ").next().unwrap_or(declaration);
            if element.contains("&str") {
                found.insert(
                    (file.clone(), symbol.trim().to_owned()),
                    element.trim().to_owned(),
                );
            }
        }
    }

    // "Scanned nothing" must not read as "found nothing". The crate has 28
    // generated modules; a floor well below that still catches a path that
    // silently stopped resolving.
    assert!(
        files >= 20,
        "scanned only {files} files under {} — the scan did not run, which is not a pass",
        dir.display()
    );
    assert!(
        found.len() >= 15,
        "found only {} string columns across {files} files — the pattern stopped matching",
        found.len()
    );
    found
}

#[test]
fn every_generated_string_column_is_classified() {
    let found = scan();
    let allowed: BTreeMap<(String, String), (Kind, &str)> = ALLOWED
        .iter()
        .map(|&(file, symbol, kind, note)| {
            ((file.to_owned(), symbol.to_owned()), (kind, note))
        })
        .collect();
    assert_eq!(
        allowed.len(),
        ALLOWED.len(),
        "ALLOWED has a duplicate (file, symbol) row"
    );

    let unclassified: Vec<String> = found
        .iter()
        .filter(|(key, _)| !allowed.contains_key(*key))
        .map(|((file, symbol), element)| format!("  {file}: {symbol}: {element}"))
        .collect();
    assert!(
        unclassified.is_empty(),
        "{} new string-typed generated column(s) with no classification:\n{}\n\n\
         A generated column that names a registry entry should hold a typed id — a `Block`, \
         an item id — not a `&'static str`. If this one genuinely must stay a string, add it \
         to ALLOWED in tests/generated_string_columns.rs with a Kind and a reason.",
        unclassified.len(),
        unclassified.join("\n")
    );

    // The other direction: a stale allowlist is how a parity gate goes quietly
    // vacuous, so a row naming a column that no longer exists fails too.
    let stale: Vec<String> = allowed
        .keys()
        .filter(|key| !found.contains_key(*key))
        .map(|(file, symbol)| format!("  {file}: {symbol}"))
        .collect();
    assert!(
        stale.is_empty(),
        "{} ALLOWED row(s) name a column that no longer exists — delete them:\n{}",
        stale.len(),
        stale.join("\n")
    );

    // Report the migration queue rather than staying silent about it.
    let debt: Vec<&(&str, &str, Kind, &str)> = ALLOWED
        .iter()
        .filter(|(_, _, kind, _)| matches!(kind, CrossReference | DuplicateNames))
        .collect();
    eprintln!(
        "classified {} generated string columns; {} still carry registry entries as strings:",
        found.len(),
        debt.len()
    );
    for (file, symbol, kind, note) in &debt {
        eprintln!("  {file}: {symbol} ({kind:?}) — {note}");
    }
    assert_eq!(
        debt.len(),
        4,
        "the count of untyped registry columns changed; if it went down, update this number \
         and delete the ALLOWED row — it is meant to reach zero"
    );
}
