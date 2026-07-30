//! The `VersionAdapter::entity_facts` seam: proves the version-owned entity
//! census — 158 real types dumped from a headless 26.2 server — actually reaches
//! a version-free consumer **through the trait object**, keyed by the
//! `ResourceKey` that is the only entity identity surviving ingest.
//!
//! Every adapter here is bound as `&dyn VersionAdapter` before it is called.
//! That is the whole point: `tests/entity_census.rs` already covers the concrete
//! table row-for-row, so a test calling `V770Adapter::entity_facts` directly
//! would prove nothing new. What can silently break is the *seam* — a missing
//! `impl` override leaves the trait's `None` default in place and the shell sees
//! "this version knows no entity types" while the table sits right there. That is
//! the island shape, and for this particular seam the `None` default is
//! **indistinguishable from a correct answer**: `entity_pushes_players` maps it to
//! `false`, which is also the right answer for a dropped item. A dead seam would
//! therefore produce no push at all and look plausible. Only asserting that a
//! zombie comes back `true` can see it.
//!
//! # Both keyings must agree
//!
//! `entity_dimensions` (network id) and `entity_facts` (resource key) read the
//! same census two ways. [`both_keyings_agree_on_every_type`] walks all 158 types
//! through both and asserts identical dimensions, so the split-identifier lookup
//! `entity_facts` uses cannot drift off by one against the id-indexed table.

use lodestone_model::{ResourceKey, VersionAdapter};
use lodestone_v770::{entity_types, V770Adapter};

/// Binds the concrete adapter behind the trait object, so every assertion below
/// travels the same dynamic-dispatch path a version-free consumer uses after
/// `lodestone_registry::adapter_for_protocol`.
fn seam() -> Box<dyn VersionAdapter> {
    Box::new(V770Adapter::new())
}

fn key(name: &str) -> ResourceKey {
    name.parse().unwrap_or_else(|_| panic!("{name} parses as a resource key"))
}

#[test]
fn seam_returns_real_facts_not_the_trait_default() {
    let adapter = seam();

    // A zombie is the load-bearing assertion: `true` cannot be produced by the
    // trait default, by an absent census, or by a table stuck at `false`.
    let zombie = adapter
        .entity_facts(&key("minecraft:zombie"))
        .expect("zombie resolves through the trait object");
    assert!(
        zombie.pushes_players,
        "a zombie must push the player through the seam — a `false` here is what a \
         dead seam looks like, and it is invisible in every other assertion"
    );
    assert_eq!(
        (zombie.dimensions.width, zombie.dimensions.height),
        (0.6, 1.95),
        "zombie base hitbox through the seam"
    );

    // And the real dimensions actually arrive, rather than the player's box the
    // producer used as a stand-in before this seam existed.
    let creeper = adapter
        .entity_facts(&key("minecraft:creeper"))
        .expect("creeper resolves");
    assert_eq!((creeper.dimensions.width, creeper.dimensions.height), (0.6, 1.7));
    let cow = adapter.entity_facts(&key("minecraft:cow")).expect("cow resolves");
    assert_eq!((cow.dimensions.width, cow.dimensions.height), (0.9, 1.4));
    assert_ne!(
        (cow.dimensions.width, cow.dimensions.height),
        (0.6, 1.8),
        "a cow must not be reported with the player's box"
    );
}

#[test]
fn the_briefed_control_cases_come_back_through_the_seam() {
    // The four cases the entity-push work is specified against: a dropped item,
    // an arrow and a boat are not pushers; a zombie is. All four via dynamic
    // dispatch, since that is the path `lodestone-shell` takes.
    let adapter = seam();
    for name in ["minecraft:item", "minecraft:arrow", "minecraft:oak_boat"] {
        let facts = adapter
            .entity_facts(&key(name))
            .unwrap_or_else(|| panic!("{name} resolves through the seam"));
        assert!(!facts.pushes_players, "{name} must not push the player");
    }
    assert!(
        adapter
            .entity_facts(&key("minecraft:zombie"))
            .expect("zombie resolves")
            .pushes_players,
        "zombie must push the player"
    );
}

#[test]
fn an_unknown_type_misses_rather_than_guessing() {
    // Default-deny at the seam boundary: an unrecognised path and a foreign
    // namespace both miss. Neither may resolve to a guessed box or a permissive
    // `pushes_players`.
    let adapter = seam();
    assert!(adapter.entity_facts(&key("minecraft:not_a_real_entity")).is_none());
    assert!(
        adapter.entity_facts(&key("someplugin:custom_mob")).is_none(),
        "a plugin-namespaced type is a miss, not a type to reason about"
    );
    // A vanilla *path* under a foreign namespace must not resolve either.
    assert!(adapter.entity_facts(&key("someplugin:zombie")).is_none());
}

#[test]
fn both_keyings_agree_on_every_type() {
    // `entity_dimensions` (wire id) and `entity_facts` (resource key) are the same
    // census read two ways; the seam docs promise they agree. Walk all 158.
    let adapter = seam();
    let mut checked = 0usize;
    for id in 0..i32::try_from(entity_types::TYPE_COUNT).unwrap() {
        let name = entity_types::entity_type_name(id).expect("id within TYPE_COUNT");
        let by_id = adapter
            .entity_dimensions(id)
            .unwrap_or_else(|| panic!("{name} (id {id}) resolves by id"));
        let by_key = adapter
            .entity_facts(&key(name))
            .unwrap_or_else(|| panic!("{name} (id {id}) resolves by key"));
        assert_eq!(
            by_key.dimensions, by_id,
            "{name} (id {id}) disagrees between the two keyings"
        );
        checked += 1;
    }
    assert_eq!(checked, 158, "expected 158 types cross-checked, got {checked}");
}

#[test]
fn the_census_reaches_the_seam_for_a_mixed_population() {
    // Guards against a seam that resolves *some* types: a table sliced short, or a
    // linear scan that stops early, would still pass a zombie-only assertion.
    let adapter = seam();
    let pushers = (0..i32::try_from(entity_types::TYPE_COUNT).unwrap())
        .filter(|&id| {
            let name = entity_types::entity_type_name(id).expect("id within TYPE_COUNT");
            adapter
                .entity_facts(&key(name))
                .is_some_and(|facts| facts.pushes_players)
        })
        .count();
    assert_eq!(
        pushers, 90,
        "the seam must surface all 90 pushers of the 158-type census"
    );
}
