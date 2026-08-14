//! Damage-type registry table: the generator, the drift guard, and hermetic
//! checks whose expected values come from the committed jar dump rather than
//! from the table under test.
//!
//! # Data provenance
//!
//! `tests/support/damage_types_jar.txt` is vanilla 26.2's own datapack JSON,
//! **verbatim**, extracted from the real server jar
//! (`.cache/mc/26.2/versions/26.2/server-26.2.jar`). Damage types differ from
//! every other table in this crate in a way that matters: hardness, collision
//! shapes and entity dimensions have *no* datapack representation, so they need
//! a registry-walking JVM oracle (`oracle-java/HardnessOracle.java` and
//! friends). Damage types **are** data files. Reading them directly is strictly
//! more authoritative than booting a server and asking a program to describe
//! them, and it needs no JVM, no Docker and no `container`.
//!
//! Two traps, both confirmed by measurement rather than assumed:
//!
//! * The **outer** `.cache/mc/26.2/server.jar` is a *bundler*:
//!   `unzip -l | grep damage_type` returns **zero** hits against it. Searching
//!   the wrong jar looks exactly like "this version has no damage-type data".
//! * **Seven of the 34 tag files reference other tags** (`"#minecraft:is_explosion"`),
//!   so membership is a transitive closure. `bypasses_shield` lists 12 entries
//!   and resolves to **30**, because one of them is `#minecraft:bypasses_armor`.
//!   A flat reader is wrong for exactly those seven tags and right for the other
//!   27, which is the worst possible failure shape.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-extract the dump (this is what `just regen-damage-types` runs first):
//!
//! ```text
//! python3 scripts/extract-damage-types.py \
//!     .cache/mc/26.2/versions/26.2/server-26.2.jar \
//!     crates/lodestone-data/tests/support/damage_types_jar.txt
//! ```
//!
//! 2. Regenerate the committed table:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test damage_types \
//!     committed_table_matches_dump -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::damage_types::{
    ALL_DAMAGE_TYPE_TAGS, DAMAGE_TYPE_COUNT, DAMAGE_TYPE_TAG_COUNT, DamageEffects, DamageScaling,
    DamageType, DamageTypeTag, DeathMessageType,
};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/damage_types.rs")
}

/// The committed jar dump — the external anchor, not gitignored.
const DUMP: &str = include_str!("support/damage_types_jar.txt");

/// `bypasses_cooldown` is declared at `DamageTypeTags.BYPASSES_COOLDOWN` and gates the
/// i-frame window in `LivingEntity.hurtServer`, but ships **no data file**. It is
/// carried as a real, empty tag; see `assert`s in
/// [`bypasses_cooldown_is_a_real_tag_with_no_members`].
const CODE_ONLY_TAGS: [&str; 1] = ["bypasses_cooldown"];

// ---------------------------------------------------------------------------
// Dump parsing (verbatim JSON entries, `>>> <relative path>` delimited)
// ---------------------------------------------------------------------------

/// One damage type exactly as the datapack states it.
#[derive(Debug, Clone, PartialEq)]
struct RawType {
    name: String,
    message_id: String,
    scaling: String,
    exhaustion: f32,
    /// `optionalFieldOf("effects", HURT)` — `None` means the key is absent.
    effects: Option<String>,
    /// `optionalFieldOf("death_message_type", DEFAULT)`.
    death_message_type: Option<String>,
}

fn parse_dump(text: &str) -> (Vec<RawType>, BTreeMap<String, Vec<String>>) {
    let mut types = Vec::new();
    let mut tags: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let mut current: Option<String> = None;
    let mut body = String::new();

    // `>>> damage_type/x.json` / `>>> tags/damage_type/y.json` headers separate
    // verbatim JSON bodies. Comments only appear before the first header.
    let flush = |path: &str, body: &str, types: &mut Vec<RawType>,
                 tags: &mut BTreeMap<String, Vec<String>>| {
        let json: serde_json::Value =
            serde_json::from_str(body).unwrap_or_else(|e| panic!("{path} is not valid JSON: {e}"));
        if let Some(rest) = path.strip_prefix("tags/damage_type/") {
            let name = rest
                .strip_suffix(".json")
                .expect("tag entry ends in .json")
                .to_owned();
            let values = json
                .get("values")
                .and_then(|v| v.as_array())
                .unwrap_or_else(|| panic!("{path} has no `values` array"))
                .iter()
                .map(|v| {
                    v.as_str()
                        .unwrap_or_else(|| {
                            panic!("{path} has a non-string tag entry: object entries \
                                    (`{{\"id\":…,\"required\":false}}`) are not modelled, and \
                                    none exist in 26.2 -- if one appears, the closure resolver \
                                    below must learn about it")
                        })
                        .to_owned()
                })
                .collect();
            assert!(
                tags.insert(name.clone(), values).is_none(),
                "duplicate tag entry {name}"
            );
        } else if let Some(rest) = path.strip_prefix("damage_type/") {
            let name = rest
                .strip_suffix(".json")
                .expect("type entry ends in .json")
                .to_owned();
            let field = |key: &str| -> Option<String> {
                json.get(key)
                    .map(|v| v.as_str().unwrap_or_else(|| panic!("{path}/{key} is not a string")))
                    .map(str::to_owned)
            };
            types.push(RawType {
                message_id: field("message_id")
                    .unwrap_or_else(|| panic!("{path} has no message_id")),
                scaling: field("scaling").unwrap_or_else(|| panic!("{path} has no scaling")),
                exhaustion: json
                    .get("exhaustion")
                    .and_then(|v| v.as_f64())
                    .unwrap_or_else(|| panic!("{path} has no exhaustion"))
                    as f32,
                effects: field("effects"),
                death_message_type: field("death_message_type"),
                name,
            });
        } else {
            panic!("unexpected dump entry path {path:?}");
        }
    };

    for line in text.lines() {
        if let Some(path) = line.strip_prefix(">>> ") {
            if let Some(prev) = current.take() {
                flush(&prev, &body, &mut types, &mut tags);
            }
            current = Some(path.trim().to_owned());
            body.clear();
        } else if current.is_some() {
            body.push_str(line);
            body.push('\n');
        } else {
            assert!(
                line.is_empty() || line.starts_with('#'),
                "unexpected preamble line before the first entry: {line:?}"
            );
        }
    }
    if let Some(prev) = current.take() {
        flush(&prev, &body, &mut types, &mut tags);
    }

    types.sort_by(|a, b| a.name.cmp(&b.name));
    (types, tags)
}

/// Every tag name in table order: the data-file tags plus the code-only ones,
/// sorted. The bit index of a tag is its position here.
fn tag_order(tags: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut names: BTreeSet<String> = tags.keys().cloned().collect();
    for extra in CODE_ONLY_TAGS {
        names.insert(extra.to_owned());
    }
    names.into_iter().collect()
}

/// Resolves one tag's membership to its transitive closure, inlining
/// `#minecraft:other_tag` references exactly as vanilla's `TagLoader` does.
///
/// Cycle-safe via `seen`: vanilla rejects cyclic tags at load time, and this
/// panics rather than looping if one ever appears.
fn resolve(
    tag: &str,
    tags: &BTreeMap<String, Vec<String>>,
    seen: &mut Vec<String>,
) -> BTreeSet<String> {
    assert!(
        !seen.iter().any(|s| s == tag),
        "cyclic damage_type tag reference: {seen:?} -> {tag}"
    );
    seen.push(tag.to_owned());
    let mut out = BTreeSet::new();
    // A code-only tag (no data file) resolves to the empty set.
    for entry in tags.get(tag).map(Vec::as_slice).unwrap_or(&[]) {
        if let Some(referenced) = entry.strip_prefix('#') {
            let path = referenced
                .strip_prefix("minecraft:")
                .unwrap_or(referenced)
                .to_owned();
            assert!(
                tags.contains_key(&path),
                "{tag} references unknown tag #{referenced}"
            );
            out.extend(resolve(&path, tags, seen));
        } else {
            out.insert(
                entry
                    .strip_prefix("minecraft:")
                    .unwrap_or(entry)
                    .to_owned(),
            );
        }
    }
    seen.pop();
    out
}

fn scaling_index(name: &str) -> u8 {
    match name {
        "never" => 0,
        "when_caused_by_living_non_player" => 1,
        "always" => 2,
        other => panic!("unknown DamageScaling {other:?}"),
    }
}

fn effects_index(name: Option<&str>) -> u8 {
    // `DamageType.DIRECT_CODEC`: optionalFieldOf("effects", DamageEffects.HURT).
    match name.unwrap_or("hurt") {
        "hurt" => 0,
        "thorns" => 1,
        "drowning" => 2,
        "burning" => 3,
        "poking" => 4,
        "freezing" => 5,
        other => panic!("unknown DamageEffects {other:?}"),
    }
}

fn death_message_index(name: Option<&str>) -> u8 {
    // `DamageType.DIRECT_CODEC`: optionalFieldOf("death_message_type", DEFAULT).
    match name.unwrap_or("default") {
        "default" => 0,
        "fall_variants" => 1,
        "intentional_game_design" => 2,
        other => panic!("unknown DeathMessageType {other:?}"),
    }
}

/// Renders the committed `generated/damage_types.rs` source from the dump.
fn generate(types: &[RawType], tags: &BTreeMap<String, Vec<String>>) -> String {
    let order = tag_order(tags);
    assert!(
        order.len() <= 64,
        "more than 64 damage_type tags: the u64 mask no longer fits ({} tags)",
        order.len()
    );

    let resolved: Vec<BTreeSet<String>> = order
        .iter()
        .map(|tag| resolve(tag, tags, &mut Vec::new()))
        .collect();

    // Every tag member must be a real damage type -- a typo'd or removed type
    // in a tag file would otherwise vanish silently into an unset bit.
    let known: BTreeSet<&str> = types.iter().map(|t| t.name.as_str()).collect();
    for (tag, members) in order.iter().zip(&resolved) {
        for member in members {
            assert!(
                known.contains(member.as_str()),
                "tag {tag} lists unknown damage type {member}"
            );
        }
    }

    let masks: Vec<u64> = types
        .iter()
        .map(|ty| {
            let mut mask = 0u64;
            for (bit, members) in resolved.iter().enumerate() {
                if members.contains(&ty.name) {
                    mask |= 1u64 << bit;
                }
            }
            mask
        })
        .collect();

    let count = types.len();
    let tag_count = order.len();

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test damage_types -- --ignored`\n\
         // from tests/support/damage_types_jar.txt (vanilla 26.2's own datapack JSON,\n\
         // verbatim, out of .cache/mc/26.2/versions/26.2/server-26.2.jar). DO NOT EDIT BY\n\
         // HAND. Regenerate with LODESTONE_REGEN=1 (see the test module docs).\n\
         //\n\
         // Tag masks are the RESOLVED TRANSITIVE CLOSURE: seven vanilla tag files\n\
         // reference other tags (\"#minecraft:is_explosion\"), so a flat read of the JSON\n\
         // under-reports membership for exactly those seven.\n",
    );
    out.push_str(
        "//! Generated `minecraft:damage_type` registry table for Minecraft 26.2,\n\
         //! indexed by alphabetical table position. Consumed by\n\
         //! [`crate::damage_types`].\n\
         //!\n\
         //! These indices are **not** network registry ids: `minecraft:damage_type` is a\n\
         //! purely data-driven registry with no default protocol id, assigned per\n\
         //! connection by registry-sync order.\n\n",
    );

    let _ = writeln!(out, "/// Number of damage types in the registry.");
    let _ = writeln!(out, "pub const DAMAGE_TYPE_COUNT: usize = {count};\n");
    let _ = writeln!(
        out,
        "/// Number of damage-type tags ({} with data files, plus {} code-only).",
        tag_count - CODE_ONLY_TAGS.len(),
        CODE_ONLY_TAGS.len()
    );
    let _ = writeln!(out, "pub const DAMAGE_TYPE_TAG_COUNT: usize = {tag_count};\n");

    let _ = writeln!(
        out,
        "/// Damage type path names (no `minecraft:` namespace), alphabetical."
    );
    let _ = writeln!(out, "pub static DAMAGE_TYPE_NAMES: [&str; {count}] = [");
    for ty in types {
        let _ = writeln!(out, "    {:?},", ty.name);
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// `message_id` per type -- the death-message key stem, which is often NOT the\n\
         /// type name (`mob_attack` -> `\"mob\"`, `bad_respawn_point` -> `\"badRespawnPoint\"`)."
    );
    let _ = writeln!(out, "pub static DAMAGE_TYPE_MESSAGE_IDS: [&str; {count}] = [");
    for ty in types {
        let _ = writeln!(out, "    {:?},", ty.message_id);
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Raw `f32` bit patterns of `exhaustion`, so the text round-trip loses no\n\
         /// precision (same convention as the hardness table)."
    );
    let _ = writeln!(
        out,
        "pub static DAMAGE_TYPE_EXHAUSTION_BITS: [u32; {count}] = ["
    );
    for ty in types {
        let _ = writeln!(
            out,
            "    0x{:08x}, // {} = {:?}",
            ty.exhaustion.to_bits(),
            ty.name,
            ty.exhaustion
        );
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// `DamageScaling` discriminant per type (0 never, 1 when_caused_by_living_non_player,\n\
         /// 2 always)."
    );
    let _ = writeln!(out, "pub static DAMAGE_TYPE_SCALING: [u8; {count}] = [");
    for ty in types {
        let _ = writeln!(
            out,
            "    {}, // {} = {}",
            scaling_index(&ty.scaling),
            ty.name,
            ty.scaling
        );
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// `DamageEffects` discriminant per type (0 hurt, 1 thorns, 2 drowning, 3 burning,\n\
         /// 4 poking, 5 freezing). Absent in the JSON means `hurt`, not missing."
    );
    let _ = writeln!(out, "pub static DAMAGE_TYPE_EFFECTS: [u8; {count}] = [");
    for ty in types {
        let _ = writeln!(
            out,
            "    {}, // {} = {}",
            effects_index(ty.effects.as_deref()),
            ty.name,
            ty.effects.as_deref().unwrap_or("hurt (default)")
        );
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// `DeathMessageType` discriminant per type (0 default, 1 fall_variants,\n\
         /// 2 intentional_game_design)."
    );
    let _ = writeln!(out, "pub static DAMAGE_TYPE_DEATH_MESSAGE: [u8; {count}] = [");
    for ty in types {
        let _ = writeln!(
            out,
            "    {}, // {} = {}",
            death_message_index(ty.death_message_type.as_deref()),
            ty.name,
            ty.death_message_type.as_deref().unwrap_or("default (default)")
        );
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Tag path names (no namespace), alphabetical. Position = bit index in\n\
         /// [`DAMAGE_TYPE_TAG_MASKS`]."
    );
    let _ = writeln!(
        out,
        "pub static DAMAGE_TYPE_TAG_NAMES: [&str; {tag_count}] = ["
    );
    for (bit, tag) in order.iter().enumerate() {
        let n = resolved[bit].len();
        let note = if tags.contains_key(tag) {
            ""
        } else {
            " (code-only: no data file)"
        };
        let _ = writeln!(out, "    {tag:?}, // bit {bit}, {n} members{note}");
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Resolved tag membership per damage type: bit `i` set = member of\n\
         /// `DAMAGE_TYPE_TAG_NAMES[i]`."
    );
    let _ = writeln!(out, "pub static DAMAGE_TYPE_TAG_MASKS: [u64; {count}] = [");
    for (ty, mask) in types.iter().zip(&masks) {
        let mut names: Vec<&str> = Vec::new();
        for (bit, tag) in order.iter().enumerate() {
            if mask & (1u64 << bit) != 0 {
                names.push(tag);
            }
        }
        let _ = writeln!(
            out,
            "    0x{:010x}, // {}: {}",
            mask,
            ty.name,
            if names.is_empty() {
                "(no tags)".to_owned()
            } else {
                names.join(" ")
            }
        );
    }
    out.push_str("];\n");

    out
}

// ---------------------------------------------------------------------------
// The drift guard / regen path
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates the committed table from the jar dump; run explicitly"]
fn committed_table_matches_dump() {
    let (types, tags) = parse_dump(DUMP);
    let generated = generate(&types, &tags);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write committed table");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed = std::fs::read_to_string(committed_path()).expect("committed table present");
    assert_eq!(
        generated, committed,
        "src/generated/damage_types.rs is stale vs the jar dump; regenerate with LODESTONE_REGEN=1"
    );
}

// ---------------------------------------------------------------------------
// Hermetic checks: expected values come from the committed dump, never from the
// table under test.
// ---------------------------------------------------------------------------

#[test]
fn every_field_of_every_type_matches_the_committed_dump() {
    let (types, _tags) = parse_dump(DUMP);
    assert_eq!(
        types.len(),
        DAMAGE_TYPE_COUNT,
        "dump/table damage-type count mismatch"
    );

    let mut checked = 0usize;
    for raw in &types {
        let ty = DamageType::from_name(&raw.name)
            .unwrap_or_else(|| panic!("{} missing from the table", raw.name));
        assert_eq!(ty.name(), raw.name);
        assert_eq!(ty.message_id(), raw.message_id, "{} message_id", raw.name);
        assert_eq!(
            ty.exhaustion().to_bits(),
            raw.exhaustion.to_bits(),
            "{} exhaustion",
            raw.name
        );
        assert_eq!(ty.scaling().name(), raw.scaling, "{} scaling", raw.name);
        assert_eq!(
            ty.effects().name(),
            raw.effects.as_deref().unwrap_or("hurt"),
            "{} effects",
            raw.name
        );
        assert_eq!(
            ty.death_message_type().name(),
            raw.death_message_type.as_deref().unwrap_or("default"),
            "{} death_message_type",
            raw.name
        );
        checked += 1;
    }
    assert_eq!(checked, DAMAGE_TYPE_COUNT, "not every type was checked");
}

#[test]
fn every_tag_membership_matches_the_closure_of_the_committed_dump() {
    let (_types, tags) = parse_dump(DUMP);
    let order = tag_order(&tags);
    assert_eq!(
        order.len(),
        DAMAGE_TYPE_TAG_COUNT,
        "dump/table tag count mismatch"
    );

    // The enum's ordering IS the bit layout; a variant in the wrong place would
    // silently shift every membership bit.
    for (bit, name) in order.iter().enumerate() {
        assert_eq!(
            ALL_DAMAGE_TYPE_TAGS[bit].name(),
            name.as_str(),
            "tag bit {bit} is {:?} in the table but {name:?} in the dump",
            ALL_DAMAGE_TYPE_TAGS[bit].name()
        );
        assert_eq!(
            ALL_DAMAGE_TYPE_TAGS[bit] as usize, bit,
            "DamageTypeTag discriminant does not match its table position"
        );
    }

    let mut asserted = 0usize;
    for (bit, tag_name) in order.iter().enumerate() {
        let expected = resolve(tag_name, &tags, &mut Vec::new());
        let tag = ALL_DAMAGE_TYPE_TAGS[bit];
        for ty in DamageType::ALL {
            let want = expected.contains(ty.name());
            assert_eq!(
                ty.is_in(tag),
                want,
                "{} in #{tag_name}: table says {}, dump closure says {want}",
                ty.name(),
                ty.is_in(tag)
            );
            asserted += 1;
        }
    }
    assert_eq!(
        asserted,
        DAMAGE_TYPE_COUNT * DAMAGE_TYPE_TAG_COUNT,
        "not every (type, tag) pair was asserted"
    );
}

/// The closure is the one interpretive step between the datapack and the table,
/// so it gets its own check with numbers taken from the raw JSON by hand.
///
/// `bypasses_shield.json` lists **12** entries, one of which is
/// `#minecraft:bypasses_armor`; `bypasses_armor.json` lists **19**. The two
/// direct sets are disjoint (asserted below rather than assumed), so the closure
/// must be 11 + 19 = **30** members where a flat non-resolving reader reports
/// **11**. This asserts the measurement lands on the right one of those two.
///
/// Note the arithmetic here was wrong on the first attempt — 31, from spuriously
/// counting the `#` reference as a member — and the test caught it. The
/// disjointness assertion exists so the prediction is *derived* from the dump
/// rather than restated.
#[test]
fn tag_closure_resolves_references_rather_than_reading_flat() {
    let (_types, tags) = parse_dump(DUMP);

    let flat_entries = tags
        .get("bypasses_shield")
        .expect("bypasses_shield tag present");
    assert_eq!(
        flat_entries.len(),
        12,
        "the dump's bypasses_shield entry count changed; re-derive this test's numbers"
    );
    assert!(
        flat_entries.iter().any(|e| e == "#minecraft:bypasses_armor"),
        "bypasses_shield no longer references #minecraft:bypasses_armor"
    );
    // The 11 non-reference entries, and the 19 inherited ones.
    let direct: BTreeSet<String> = flat_entries
        .iter()
        .filter(|e| !e.starts_with('#'))
        .map(|e| e.strip_prefix("minecraft:").unwrap_or(e).to_owned())
        .collect();
    assert_eq!(direct.len(), 11, "11 direct entries + 1 tag reference = 12");
    let flat_wrong = direct.len();

    let inherited = resolve("bypasses_armor", &tags, &mut Vec::new());
    assert_eq!(inherited.len(), 19, "bypasses_armor member count changed");
    assert!(
        direct.is_disjoint(&inherited),
        "the two sets overlap, so 11 + 19 is no longer the right prediction: {:?}",
        direct.intersection(&inherited).collect::<Vec<_>>()
    );

    let armor_members = DamageTypeTag::BypassesArmor.members().count();
    assert_eq!(armor_members, 19, "bypasses_armor member count changed");

    let resolved = DamageTypeTag::BypassesShield.members().count();
    assert_eq!(
        resolved,
        direct.len() + inherited.len(),
        "bypasses_shield must resolve to its 11 own + the 19 it inherits"
    );
    assert_eq!(resolved, 30, "derived prediction, restated as a constant");
    assert!(
        resolved > flat_wrong + 15,
        "the resolved and flat hypotheses must differ substantially for this to be a real check \
         (resolved {resolved}, flat {flat_wrong})"
    );

    // Membership only reachable through the reference: `generic` is in
    // bypasses_armor and is NOT listed in bypasses_shield.json.
    let generic = DamageType::from_name("generic").expect("generic present");
    assert!(!flat_entries.iter().any(|e| e == "minecraft:generic"));
    assert!(
        generic.is_in(DamageTypeTag::BypassesShield),
        "closure-only membership missing: generic reaches bypasses_shield via #bypasses_armor"
    );
}

/// A permanent **negative control** for the closure resolver: the same
/// assertion the test above makes, run against a deliberately broken resolver
/// that ignores `#tag` references. It must fail. Without this, "the closure is
/// resolved" rests on the resolver and the expectation sharing one author.
#[test]
fn a_flat_non_resolving_reader_fails_the_closure_assertion() {
    let (_types, tags) = parse_dump(DUMP);

    // Broken resolver: drops every `#tag` reference instead of inlining it.
    fn resolve_flat(tag: &str, tags: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
        tags.get(tag)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter(|e| !e.starts_with('#'))
            .map(|e| e.strip_prefix("minecraft:").unwrap_or(e).to_owned())
            .collect()
    }

    let broken = resolve_flat("bypasses_shield", &tags);
    let correct = resolve("bypasses_shield", &tags, &mut Vec::new());

    assert_ne!(
        broken.len(),
        correct.len(),
        "the control is vacuous: flat and resolved readings agree, so this suite could \
         not detect an unresolved closure at all"
    );
    assert_eq!(broken.len(), 11, "flat reading drops the one #tag reference");
    assert_eq!(correct.len(), 30);
    assert!(
        !broken.contains("generic"),
        "the broken reader must miss closure-only membership -- that is what makes the \
         real assertion above non-vacuous"
    );
}

/// `bypasses_cooldown` exists in code and not in data. Asserted, not assumed:
/// if a future version ships the file, this fails rather than continuing to
/// report that nothing bypasses the i-frame window.
#[test]
fn bypasses_cooldown_is_a_real_tag_with_no_members() {
    let (_types, tags) = parse_dump(DUMP);
    assert!(
        !tags.contains_key("bypasses_cooldown"),
        "vanilla 26.2 ships no bypasses_cooldown data file; a version that adds one must \
         update the CODE_ONLY_TAGS list and the docs claiming the tag is empty"
    );
    assert_eq!(
        DamageTypeTag::BypassesCooldown.members().count(),
        0,
        "bypasses_cooldown has no data file, so no type can be in it"
    );

    // Control proving the detector works: the mechanism that would report
    // membership does report it for a tag that has members.
    assert_eq!(
        DamageTypeTag::BypassesArmor.members().count(),
        19,
        "the membership query must be capable of returning members, or the emptiness \
         above measures nothing"
    );
}

/// Values hand-read out of the datapack JSON, spot-checking the fields the
/// combat/loot/fall consumers actually read.
#[test]
fn hand_read_datapack_values_survive_the_table() {
    let fall = DamageType::from_name("minecraft:fall").expect("fall");
    assert_eq!(fall.message_id(), "fall");
    assert_eq!(fall.exhaustion(), 0.0);
    assert_eq!(fall.scaling(), DamageScaling::WhenCausedByLivingNonPlayer);
    assert_eq!(fall.death_message_type(), DeathMessageType::FallVariants);
    assert!(fall.is_in(DamageTypeTag::IsFall));
    assert!(fall.is_in(DamageTypeTag::BypassesArmor));
    assert!(fall.is_in(DamageTypeTag::NoKnockback));

    // message_id is not the type name.
    let mob = DamageType::from_name("mob_attack").expect("mob_attack");
    assert_eq!(mob.message_id(), "mob");
    assert_eq!(mob.exhaustion(), 0.1);
    assert_eq!(mob.effects(), DamageEffects::Hurt);
    let bad_bed = DamageType::from_name("bad_respawn_point").expect("bad_respawn_point");
    assert_eq!(bad_bed.message_id(), "badRespawnPoint");
    assert_eq!(bad_bed.scaling(), DamageScaling::Always);
    assert_eq!(
        bad_bed.death_message_type(),
        DeathMessageType::IntentionalGameDesign
    );

    // effects defaults vs explicit.
    assert_eq!(
        DamageType::from_name("lava").expect("lava").effects(),
        DamageEffects::Burning
    );
    assert_eq!(
        DamageType::from_name("drown").expect("drown").effects(),
        DamageEffects::Drowning
    );
    assert_eq!(
        DamageType::from_name("freeze").expect("freeze").effects(),
        DamageEffects::Freezing
    );
    assert_eq!(
        DamageType::from_name("sweet_berry_bush")
            .expect("sweet_berry_bush")
            .effects(),
        DamageEffects::Poking
    );

    // out_of_world is the "kill everything" type: bypasses armour, resistance
    // and invulnerability.
    let void = DamageType::from_name("out_of_world").expect("out_of_world");
    assert!(void.is_in(DamageTypeTag::BypassesArmor));
    assert!(void.is_in(DamageTypeTag::BypassesResistance));
    assert!(void.is_in(DamageTypeTag::BypassesInvulnerability));

    // starve is the only bypasses_effects member, sonic_boom the only
    // bypasses_enchantments one.
    assert_eq!(
        DamageTypeTag::BypassesEffects
            .members()
            .map(DamageType::name)
            .collect::<Vec<_>>(),
        vec!["starve"]
    );
    assert_eq!(
        DamageTypeTag::BypassesEnchantments
            .members()
            .map(DamageType::name)
            .collect::<Vec<_>>(),
        vec!["sonic_boom"]
    );
}

/// The trap CLAUDE.md records from a lost debugging session: `minecraft:generic`
/// is itself `bypasses_armor`-tagged, so it reduces nothing and is the wrong
/// type to test armour with. `mob_attack` is reducible. Pinned here so the next
/// person writing a damage oracle reads it from the table rather than
/// rediscovering it.
#[test]
fn generic_bypasses_armor_but_mob_attack_does_not() {
    let generic = DamageType::from_name("generic").expect("generic");
    let mob_attack = DamageType::from_name("mob_attack").expect("mob_attack");
    assert!(
        generic.is_in(DamageTypeTag::BypassesArmor),
        "minecraft:generic IS bypasses_armor-tagged -- it is the wrong damage type for \
         testing armour reduction"
    );
    assert!(
        !mob_attack.is_in(DamageTypeTag::BypassesArmor),
        "minecraft:mob_attack is a reducible type -- use it for armour tests"
    );
}

#[test]
fn an_unknown_type_name_is_a_miss_not_a_default() {
    assert!(DamageType::from_name("minecraft:not_a_damage_type").is_none());
    assert!(DamageType::from_name("").is_none());
    // Namespaced and bare forms both resolve, to the same type.
    assert_eq!(
        DamageType::from_name("minecraft:lava"),
        DamageType::from_name("lava")
    );
}
