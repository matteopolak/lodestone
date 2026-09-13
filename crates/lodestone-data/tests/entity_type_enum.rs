//! `EntityType` enum: hermetic checks over the committed table, plus an
//! `#[ignore]`d drift guard that regenerates it from the committed JVM dump
//! and asserts byte-for-byte equality — modelled on `tools.rs`'s
//! `generate_block_enum` and `block_enum.rs`'s gates. The generator lives
//! here so the checked-in enum can never silently drift from game data.
//!
//! # Data provenance
//!
//! `tests/support/entity_census_jvm.txt` is already a committed JVM dump for
//! this registry: `EntityCensusOracle.java` walks
//! vanilla's own entity-type registry on the real 26.2 server and writes one
//! `<id> <name> …` line per type, in registration order (its own header
//! documents `id` as "network entity-type registry id (dense 0..158)"). This
//! generator reads only the `id`/`name` columns; the remaining columns
//! (implementation class, living/mob flags, dimensions) are
//! `entity_census.rs`'s and `entity_dimensions.rs`'s concern, not this one's.
//! Row 100 is `minecraft:pig`, the same spot check `tests/entity_types.rs`
//! already pins against `registries.json` — two independently produced
//! artifacts agreeing.
//!
//! 158 entity types fit in a `u8`, unlike `Block` (1,196) and `Item` (1,537),
//! so this is the one registry in Stage 1 that uses `#[repr(u8)]`.
//!
//! # Refreshing after a version bump
//!
//! 1. Re-dump `tests/support/entity_census_jvm.txt` per the recipe in
//!    `tests/entity_census.rs`'s module docs.
//! 2. Regenerate the committed enum:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-data --test entity_type_enum \
//!     committed_enum_matches_dump -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use lodestone_data::entity_type::{CustomEntityTypeId, EntityType, EntityTypeKind, EntityTypeRef};
use lodestone_model::ResourceKey;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn committed_path() -> PathBuf {
    manifest_dir().join("src/generated/entity_type_enum.rs")
}

/// The committed JVM dump — an external anchor, not gitignored. Shared with
/// `tests/entity_census.rs`, but parsed independently here.
const DUMP: &str = include_str!("support/entity_census_jvm.txt");

// ---------------------------------------------------------------------------
// Dump parsing (id + name columns only)
// ---------------------------------------------------------------------------

fn parse_dump(text: &str) -> Vec<(u8, String)> {
    let mut rows: Vec<(u8, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let id: u8 = tok
            .next()
            .expect("id column")
            .parse()
            .expect("id fits u8");
        let name = tok.next().expect("name column").to_owned();
        // Ignore the remaining columns (impl class, living/mob flags, push
        // decls, dimension bits) — not this generator's concern.
        rows.push((id, name));
    }
    rows.sort_unstable_by_key(|(id, _)| *id);
    for (index, (id, _)) in rows.iter().enumerate() {
        assert_eq!(
            *id as usize, index,
            "dump ids are not a dense 0..N (gap at {index})"
        );
    }
    rows
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// The Rust enum variant name for a registry path: `ender_dragon` →
/// `EnderDragon`.
///
/// Callers must have already checked the path against
/// [`assert_path_is_variant_safe`]; this function only transforms.
fn variant_name(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for word in path.split('_') {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Fails the generator — loudly, naming the offender — if any entity-type
/// name cannot become a distinct, legal Rust variant. Mirrors `tools.rs`'s
/// `assert_path_is_variant_safe`. `docs/registry-types.md` already recorded
/// this registry as clean (158 paths, 158 distinct variants, all
/// `minecraft:`, none digit-leading); asserting it here is what keeps that
/// true after a version bump rather than a fact that happened to hold once.
fn assert_path_is_variant_safe(names: &[String]) -> Vec<(String, String)> {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut pairs = Vec::with_capacity(names.len());
    for name in names {
        let (namespace, path) = name
            .split_once(':')
            .unwrap_or_else(|| panic!("registry name {name:?} has no namespace"));
        assert_eq!(
            namespace, "minecraft",
            "the generated enum covers the built-in registry only; {name:?} is not `minecraft:`"
        );
        assert!(
            !path.is_empty()
                && path
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "entity type path {path:?} is outside [a-z0-9_] and has no obvious variant spelling"
        );
        assert!(
            !path.as_bytes()[0].is_ascii_digit(),
            "entity type path {path:?} starts with a digit, which is not a legal Rust variant"
        );
        let variant = variant_name(path);
        assert_ne!(
            variant, "Self",
            "entity type path {path:?} camel-cases to the reserved identifier `Self`"
        );
        if let Some(previous) = seen.insert(variant.clone(), path.to_owned()) {
            panic!(
                "entity type paths {previous:?} and {path:?} both camel-case to `{variant}`; \
                 the generator must be taught a disambiguation rather than silently aliasing \
                 them"
            );
        }
        pairs.push((variant, name.clone()));
    }
    pairs
}

/// Renders `src/generated/entity_type_enum.rs`: the `EntityType` enum whose
/// discriminant **is** the `minecraft:entity_type` registry id, plus the
/// name-lookup index table.
fn generate(rows: &[(u8, String)]) -> String {
    let count = rows.len();
    let names: Vec<String> = rows.iter().map(|(_, name)| name.clone()).collect();
    let variants = assert_path_is_variant_safe(&names);

    let mut by_name: Vec<u8> = (0..count as u8).collect();
    by_name.sort_unstable_by(|&a, &b| names[a as usize].cmp(&names[b as usize]));

    let mut out = String::new();
    out.push_str(
        "// @generated by `cargo test -p lodestone-data --test entity_type_enum -- --ignored`\n\
         // from tests/support/entity_census_jvm.txt (a headless 26.2 server dump of\n\
         // BuiltInRegistries.ENTITY_TYPE's registration order, protocol 776 / Minecraft\n\
         // 26.2). DO NOT EDIT BY HAND. Regenerate with LODESTONE_REGEN=1 (see the test\n\
         // module docs).\n",
    );
    out.push_str(
        "//! The generated `minecraft:entity_type` registry as a Rust enum.\n\
         //!\n\
         //! One variant per built-in entity type, in **registration** order, with the\n\
         //! discriminant written out explicitly so that `entity_type as u8` *is* the\n\
         //! registry id the wire carries — no lookup, no branch, no table. 158 entries fit\n\
         //! a `u8`, unlike `Block` and `Item`.\n\
         //!\n\
         //! The accessors live in [`crate::entity_type`]; this file is data only.\n\n",
    );

    let _ = writeln!(
        out,
        "/// A built-in entity type of Minecraft 26.2, one variant per\n\
         /// `minecraft:entity_type` registry entry.\n\
         ///\n\
         /// `EntityType as u8` is the registry id. Ordering is **registration** order, not\n\
         /// alphabetical.\n\
         ///\n\
         /// This enum is intentionally *not* `#[non_exhaustive]` and carries no `Custom`\n\
         /// variant: a match over it is exhaustive, so a version bump that adds an entity\n\
         /// type fails the compile of every incomplete match instead of falling into a\n\
         /// wildcard. Entity types a plugin adds are represented by\n\
         /// [`crate::entity_type::EntityTypeRef`], one level out.\n\
         #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\n\
         #[repr(u8)]\n\
         pub enum EntityType {{"
    );
    for (id, (variant, name)) in variants.iter().enumerate() {
        let _ = writeln!(out, "    /// `{name}`\n    {variant} = {id},");
    }
    out.push_str("}\n\n");

    let _ = writeln!(
        out,
        "/// Every [`EntityType`], indexed by its registry id — the safe inverse of\n\
         /// `entity_type as u8`, and the iteration order of the registry.\n\
         pub static TYPES_BY_REGISTRY_ID: [EntityType; {count}] = ["
    );
    for chunk in variants.chunks(4) {
        out.push_str("    ");
        for (variant, _) in chunk {
            let _ = write!(out, "EntityType::{variant}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n\n");

    let _ = writeln!(
        out,
        "/// Registry ids sorted by canonical name, for `O(log {count})` name lookup against\n\
         /// [`crate::generated_entity_types::ENTITY_TYPE_NAMES`].\n\
         ///\n\
         /// A permutation of `u8` rather than a `[(&str, EntityType)]` pairs table on\n\
         /// purpose: the pairs table would re-introduce {count} fat pointers and their\n\
         /// relocations for names that already exist once in rodata.\n\
         pub static REGISTRY_IDS_BY_NAME: [u8; {count}] = ["
    );
    for chunk in by_name.chunks(16) {
        out.push_str("    ");
        for id in chunk {
            let _ = write!(out, "{id}, ");
        }
        out.pop();
        out.push('\n');
    }
    out.push_str("];\n");
    out
}

// ---------------------------------------------------------------------------
// Hermetic tests over the committed enum (anchored to the committed dump,
// re-parsed independently of the generator)
// ---------------------------------------------------------------------------

#[test]
fn discriminant_is_the_registry_id_and_names_match_the_server_dump() {
    let rows = parse_dump(DUMP);
    assert_eq!(rows.len(), EntityType::COUNT as usize, "dump/enum type count");

    let mut mismatches = Vec::new();
    for (id, name) in &rows {
        match EntityType::from_registry_id(*id) {
            Some(entity_type) => {
                if entity_type.registry_id() != *id || entity_type.name() != name {
                    mismatches.push(format!(
                        "id {id} ({name}): enum gave {:?} = id {} name {}",
                        entity_type,
                        entity_type.registry_id(),
                        entity_type.name()
                    ));
                }
            }
            None => mismatches.push(format!("id {id} ({name}): enum has no such registry id")),
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} entity types disagree with the dump:\n{}",
        mismatches.len(),
        rows.len(),
        mismatches.join("\n")
    );
    assert_eq!(EntityType::from_registry_id(EntityType::COUNT), None);
    assert_eq!(EntityType::all().len(), rows.len());
    // The spot check `tests/entity_types.rs` already pins against
    // `registries.json`, restated here against the enum.
    assert_eq!(
        EntityType::from_registry_id(100).map(EntityType::name),
        Some("minecraft:pig")
    );
}

#[test]
fn name_order_is_also_path_order() {
    let names: Vec<&'static str> = EntityType::all().map(EntityType::name).collect();
    let paths: Vec<&'static str> = EntityType::all().map(EntityType::path).collect();
    let mut by_name: Vec<usize> = (0..names.len()).collect();
    let mut by_path = by_name.clone();
    by_name.sort_by_key(|&i| names[i]);
    by_path.sort_by_key(|&i| paths[i]);
    assert_eq!(
        by_name, by_path,
        "sorting entity types by namespaced name and by bare path give different orders; \
         `EntityType::from_name`'s bare-path search is no longer sound"
    );
}

#[test]
fn names_and_paths_round_trip_through_from_name() {
    let mut failures = Vec::new();
    for entity_type in EntityType::all() {
        if EntityType::from_name(entity_type.name()) != Some(entity_type) {
            failures.push(format!(
                "{}: namespaced form did not round-trip",
                entity_type.name()
            ));
        }
        if EntityType::from_name(entity_type.path()) != Some(entity_type) {
            failures.push(format!("{}: bare path did not round-trip", entity_type.path()));
        }
        if entity_type.name() != format!("minecraft:{}", entity_type.path()) {
            failures.push(format!("{}: name/path disagree", entity_type.name()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} round-trip failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn from_name_rejects_foreign_namespaces_and_unknown_paths() {
    assert_eq!(EntityType::from_name("minecraft:pig"), Some(EntityType::Pig));
    assert_eq!(EntityType::from_name("pig"), Some(EntityType::Pig));
    assert_eq!(EntityType::from_name("mypack:pig"), None);
    assert_eq!(EntityType::from_name("minecraft:not_an_entity"), None);
    assert_eq!(EntityType::from_name(""), None);
    assert_eq!(EntityType::from_name("minecraft:"), None);
}

#[test]
fn parsed_resource_keys_only_enter_the_fixed_registry_when_builtin() {
    let pig = ResourceKey::new("minecraft", "pig").expect("built-in key parses");
    let custom = ResourceKey::new("myplugin", "pig").expect("custom key parses");
    let unknown = ResourceKey::new("minecraft", "not_an_entity").expect("unknown key parses");

    assert_eq!(EntityType::from_resource_key(&pig), Some(EntityType::Pig));
    assert_eq!(EntityType::from_resource_key(&custom), None);
    assert_eq!(EntityType::from_resource_key(&unknown), None);
}

/// The representation claims, as numbers — mirrors `block_enum.rs`'s sibling
/// test. `EntityType` is the one Stage-1 registry small enough for `u8`.
#[test]
fn the_representation_is_the_size_it_claims() {
    assert_eq!(size_of::<EntityType>(), 1, "EntityType is a u8 discriminant");
    assert_eq!(
        size_of::<Option<EntityType>>(),
        1,
        "Option<EntityType> must use an unused discriminant as its niche"
    );
    assert_eq!(size_of::<EntityTypeRef>(), 4);
}

/// `EntityTypeRef` must cost the built-in path nothing and still carry a
/// custom id losslessly, including at the boundary where the two encodings
/// meet.
#[test]
fn entity_type_ref_separates_builtin_from_custom_without_aliasing() {
    for entity_type in EntityType::all() {
        let reference = EntityTypeRef::builtin(entity_type);
        assert_eq!(reference.kind(), EntityTypeKind::Builtin(entity_type));
        assert_eq!(reference.builtin_or_none(), Some(entity_type));
    }
    for index in [0u32, 1, 7, 1_000_000, u32::MAX - EntityType::COUNT as u32] {
        let id = CustomEntityTypeId::from_index(index);
        let reference = EntityTypeRef::custom(id);
        assert_eq!(reference.kind(), EntityTypeKind::Custom(id));
        assert_eq!(reference.builtin_or_none(), None);
        assert_eq!(
            reference.kind(),
            EntityTypeKind::Custom(CustomEntityTypeId::from_index(index)),
            "custom index {index} did not survive the round trip"
        );
    }
    assert_ne!(
        EntityTypeRef::builtin(
            EntityType::from_registry_id(EntityType::COUNT - 1).expect("last entity type")
        ),
        EntityTypeRef::custom(CustomEntityTypeId::from_index(0)),
        "the last built-in entity type and the first custom entity type collided"
    );
}

// ---------------------------------------------------------------------------
// Drift guard (regenerates from the committed dump; needs no external
// artifact beyond it)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "regenerates/verifies the committed enum; run explicitly"]
fn committed_enum_matches_dump() {
    let rows = parse_dump(DUMP);
    let generated = generate(&rows);

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        std::fs::write(committed_path(), &generated).expect("write entity type enum");
        eprintln!("regenerated {}", committed_path().display());
        return;
    }

    let committed =
        std::fs::read_to_string(committed_path()).expect("committed entity type enum present");
    assert_eq!(
        generated, committed,
        "src/generated/entity_type_enum.rs is stale vs the JVM dump; regenerate with \
         LODESTONE_REGEN=1"
    );
}
