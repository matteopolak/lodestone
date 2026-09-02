//! Every one of 26.2's twenty boat entity types must resolve to a corpus rig.
//!
//! # The defect
//!
//! `entity::model_for_type` resolves an [`EntityType`] against the
//! `entity_models` corpus, and the corpus names the four boat rigs by *class* —
//! `boat`, `chest_boat`, `raft`, `chest_raft` — because that is how vanilla builds
//! them (`BoatRenderer` picks its `ModelLayerLocation` from the boat's variant and
//! its geometry from `BoatModel`/`ChestBoatModel`/`RaftModel`/`ChestRaftModel`; the
//! wood species is a texture, not a mesh). The registry, meanwhile, has twenty
//! *types*. So `model_for_type(EntityType::OakBoat)` returned `None`, and
//! `resolve_animated` skips an entity with no model — a placed boat was invisible,
//! with the server streaming it correctly the whole time.
//!
//! # Why a plain oak boat is the wrong input to test with
//!
//! `oak_boat` passes under a naive `strip_suffix("_boat")` rule, which is exactly
//! the rule that gets both remaining cases wrong. The discriminating inputs are:
//!
//! * **a chest boat** — `oak_chest_boat` also ends with `_boat`, so a rule that
//!   tests the short suffix first draws every chest boat as a plain boat; and
//! * **`bamboo_raft` / `bamboo_chest_raft`** — neither carries a `_boat` suffix at
//!   all, so a `_boat`-only rule misses two of the twenty entirely.
//!
//! Both are asserted below, and [`RIGS`]'s whole table is checked as a collection
//! rather than with an `assert!` inside the loop, so a neuter reports every arm it
//! broke instead of aborting on the first.
//!
//! # Where the expectation comes from
//!
//! Not from the suffix rule under test. [`RIGS`] pairs each type with the vanilla
//! **class** the jar census recorded for it — `lodestone-data`'s generated
//! `entity_census` carries `Boat` / `ChestBoat` / `Raft` / `ChestRaft` per type in
//! its provenance column, dumped from the real registry — and the four class names
//! map one-to-one onto the four corpus rigs. The table is then checked against
//! `lodestone_data::entity_types` so it cannot drift into fiction: a species added
//! or renamed in the registry fails the count arm rather than passing silently.
//!
//! # Non-entity-type rig names (`"boat"`, `"chest_boat"`, ...) and garbage input
//!
//! Those cases exercise `entity::canonical_model_name`, the crate-private `&str`
//! boundary `model_for_type` no longer is — see that function's own doc and
//! `lodestone-render/src/entity.rs`'s `mod tests` (`a_literal_corpus_rig_name_...`,
//! `a_non_boat_type_gets_no_boat_rig`) for where that coverage moved. This file
//! now covers only real, registered boat entity types, which is what
//! [`EntityType`] can express.

use lodestone_data::entity_type::EntityType;
use lodestone_render::entity::model_for_type;

/// The twenty boat types of 26.2 and the corpus rig each one draws with.
///
/// `(entity type, corpus rig name)`. Nine wood species × (boat, chest boat),
/// plus the two bamboo rafts. The rig column is the jar census's class column
/// lowercased: `Boat` → `boat`, `ChestBoat` → `chest_boat`, `Raft` → `raft`,
/// `ChestRaft` → `chest_raft`.
const RIGS: &[(EntityType, &str)] = &[
    (EntityType::AcaciaBoat, "boat"),
    (EntityType::AcaciaChestBoat, "chest_boat"),
    (EntityType::BambooChestRaft, "chest_raft"),
    (EntityType::BambooRaft, "raft"),
    (EntityType::BirchBoat, "boat"),
    (EntityType::BirchChestBoat, "chest_boat"),
    (EntityType::CherryBoat, "boat"),
    (EntityType::CherryChestBoat, "chest_boat"),
    (EntityType::DarkOakBoat, "boat"),
    (EntityType::DarkOakChestBoat, "chest_boat"),
    (EntityType::JungleBoat, "boat"),
    (EntityType::JungleChestBoat, "chest_boat"),
    (EntityType::MangroveBoat, "boat"),
    (EntityType::MangroveChestBoat, "chest_boat"),
    (EntityType::OakBoat, "boat"),
    (EntityType::OakChestBoat, "chest_boat"),
    (EntityType::PaleOakBoat, "boat"),
    (EntityType::PaleOakChestBoat, "chest_boat"),
    (EntityType::SpruceBoat, "boat"),
    (EntityType::SpruceChestBoat, "chest_boat"),
];

/// [`RIGS`] is a claim about the registry, so it is checked against the registry
/// rather than trusted: every name must be a real 26.2 entity type, and the
/// registry must contain no boat-shaped type the table omits.
///
/// The second half is what catches a *new* wood species: it counts registry
/// entries whose class the census would call a boat by the only signal available
/// from the name list alone — containing `boat` or `raft` — and requires the total
/// to be exactly twenty. That is a deliberately wider net than
/// `canonical_model_name`'s rule (it would also catch a hypothetical
/// `boat_of_holding`), which is the point: the table must be a superset failure,
/// not a silent one.
#[test]
fn the_twenty_boat_types_are_exactly_what_the_registry_holds() {
    let registry: Vec<&str> = (0..lodestone_data::entity_types::TYPE_COUNT as i32)
        .filter_map(lodestone_data::entity_types::entity_type_name)
        .map(|id| id.strip_prefix("minecraft:").unwrap_or(id))
        .collect();
    assert!(
        registry.len() > 100,
        "the registry read returned only {} types, so nothing below is measuring \
         anything",
        registry.len()
    );

    // Every `RIGS` entry is already a real `EntityType` variant (the compiler
    // enforced that), but this crosschecks it against `lodestone-data`'s
    // *other* generated table (`entity_types`, not `entity_type_enum`), so the
    // two tables cannot silently drift apart from each other.
    let missing: Vec<&str> = RIGS
        .iter()
        .map(|&(entity_type, _)| entity_type.path())
        .filter(|type_path| !registry.contains(type_path))
        .collect();
    assert!(
        missing.is_empty(),
        "these table entries are not 26.2 entity types at all, so the table is \
         fiction: {missing:?}"
    );

    let boat_shaped: Vec<&str> = registry
        .iter()
        .copied()
        .filter(|path| path.contains("boat") || path.contains("raft"))
        .collect();
    assert_eq!(
        boat_shaped.len(),
        20,
        "the registry's boat-shaped types are {boat_shaped:?}; the table covers \
         {} of them. A new wood species needs a row here (and nothing else — \
         canonical_model_name derives its rig from the suffix).",
        RIGS.len()
    );
    assert_eq!(RIGS.len(), 20, "the table lost or gained a row");
}

/// The gate: every boat type resolves, and to the *right* rig.
///
/// Mismatches are collected and asserted on the collection, so neutering the
/// alias reports all twenty rather than only whichever sorts first.
#[test]
fn every_boat_type_resolves_to_its_class_rig() {
    let mut wrong = Vec::new();
    for &(entity_type, expected) in RIGS {
        let type_path = entity_type.path();
        match model_for_type(entity_type) {
            None => wrong.push(format!("{type_path}: no model at all (invisible)")),
            Some(entry) if entry.name != expected => {
                wrong.push(format!("{type_path}: got {}, want {expected}", entry.name));
            }
            Some(_) => {}
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} boat types resolve wrongly: {wrong:#?}",
        wrong.len(),
        RIGS.len()
    );
}
