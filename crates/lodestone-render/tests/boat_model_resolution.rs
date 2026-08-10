//! Every one of 26.2's twenty boat entity types must resolve to a corpus rig.
//!
//! # The defect
//!
//! `entity::model_for_type` resolves an entity-type path against the
//! `entity_models` corpus, and the corpus names the four boat rigs by *class* —
//! `boat`, `chest_boat`, `raft`, `chest_raft` — because that is how vanilla builds
//! them (`BoatRenderer` picks its `ModelLayerLocation` from the boat's variant and
//! its geometry from `BoatModel`/`ChestBoatModel`/`RaftModel`/`ChestRaftModel`; the
//! wood species is a texture, not a mesh). The registry, meanwhile, has twenty
//! *types*. So `model_for_type("oak_boat")` returned `None`, and `resolve_animated`
//! skips an entity with no model — a placed boat was invisible, with the server
//! streaming it correctly the whole time.
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

use lodestone_render::entity::model_for_type;

/// The twenty boat types of 26.2 and the corpus rig each one draws with.
///
/// `(entity type path, corpus rig name)`. Nine wood species × (boat, chest boat),
/// plus the two bamboo rafts. The rig column is the jar census's class column
/// lowercased: `Boat` → `boat`, `ChestBoat` → `chest_boat`, `Raft` → `raft`,
/// `ChestRaft` → `chest_raft`.
const RIGS: &[(&str, &str)] = &[
    ("acacia_boat", "boat"),
    ("acacia_chest_boat", "chest_boat"),
    ("bamboo_chest_raft", "chest_raft"),
    ("bamboo_raft", "raft"),
    ("birch_boat", "boat"),
    ("birch_chest_boat", "chest_boat"),
    ("cherry_boat", "boat"),
    ("cherry_chest_boat", "chest_boat"),
    ("dark_oak_boat", "boat"),
    ("dark_oak_chest_boat", "chest_boat"),
    ("jungle_boat", "boat"),
    ("jungle_chest_boat", "chest_boat"),
    ("mangrove_boat", "boat"),
    ("mangrove_chest_boat", "chest_boat"),
    ("oak_boat", "boat"),
    ("oak_chest_boat", "chest_boat"),
    ("pale_oak_boat", "boat"),
    ("pale_oak_chest_boat", "chest_boat"),
    ("spruce_boat", "boat"),
    ("spruce_chest_boat", "chest_boat"),
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

    let missing: Vec<&str> = RIGS
        .iter()
        .map(|&(type_path, _)| type_path)
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
    for &(type_path, expected) in RIGS {
        match model_for_type(type_path) {
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

/// The three inputs a naive suffix rule gets wrong, called out individually so a
/// failure names the specific trap rather than appearing as one row of the table
/// above.
#[test]
fn the_chest_and_raft_traps_resolve_correctly() {
    // `oak_chest_boat` ends with `_boat` too: testing the short suffix first
    // draws every chest boat as a plain boat.
    assert_eq!(
        model_for_type("oak_chest_boat").map(|e| e.name),
        Some("chest_boat"),
        "a chest boat must not fall back to the plain boat rig"
    );
    // Neither bamboo raft has a `_boat` suffix anywhere.
    assert_eq!(
        model_for_type("bamboo_raft").map(|e| e.name),
        Some("raft"),
        "bamboo_raft carries no `_boat` suffix, so a `_boat`-only rule misses it"
    );
    assert_eq!(
        model_for_type("bamboo_chest_raft").map(|e| e.name),
        Some("chest_raft"),
        "bamboo_chest_raft is the second suffix-less case"
    );
}

/// The suffix rules must not shadow the corpus's own names.
///
/// `chest_boat` and `chest_raft` are corpus entries that also satisfy the
/// `_boat`/`_raft` suffix tests, so a resolver that consults the suffixes *before*
/// the corpus resolves the literal `"chest_boat"` to the plain `boat` rig — a
/// silent wrong-mesh substitution for any caller that passes a corpus name
/// straight through, which the `player_wide`/`player_slim` path already does.
#[test]
fn a_literal_corpus_rig_name_still_resolves_to_itself() {
    for name in ["boat", "chest_boat", "raft", "chest_raft"] {
        assert_eq!(
            model_for_type(name).map(|e| e.name),
            Some(name),
            "a literal corpus rig name must resolve to itself, not through the \
             boat suffix rules"
        );
    }
}

/// The negative control: the suffix rules must not hand a boat rig to something
/// that merely shares a word.
///
/// `chest_minecart` has no ported rig (the corpus has `minecart` only), and a
/// resolver matching on `contains("boat")`/`contains("chest")` rather than on the
/// suffix would be caught here. Without this arm, "return `chest_boat` for
/// anything with `chest` in it" would satisfy every assertion above.
#[test]
fn a_non_boat_type_gets_no_boat_rig() {
    for name in ["chest_minecart", "chest", "boater", "raft_of_ducks", "pig"] {
        let resolved = model_for_type(name).map(|e| e.name);
        assert!(
            !matches!(
                resolved,
                Some("boat" | "chest_boat" | "raft" | "chest_raft")
            ),
            "`{name}` is not a boat, but resolved to {resolved:?}"
        );
    }
}
