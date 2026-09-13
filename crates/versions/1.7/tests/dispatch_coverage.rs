//! Proves protocol 5's `play::clientbound` dispatch table is internally
//! consistent, and that the check is not vacuous.
//!
//! `V5Adapter::play_dispatch_table` already asserts this at construction time
//! (a `.expect(...)` on `Table::build`), so a broken table would fail every
//! call into the play state — but that only demonstrates the table is *built
//! somewhere*. This test rebuilds it from the same public pieces so the claim
//! is explicit, and then removes one `IGNORED` entry and requires construction
//! to fail. The negative control is the point: without it, a table that had
//! stopped catching anything would still pass the happy path.

use lodestone_core::dispatch::{DispatchError, Table};
use lodestone_v1_7::adapter::{CLIENTBOUND, IGNORED, PROTOCOL};
use lodestone_v1_7::packet_ids::play;

#[test]
fn the_play_dispatch_table_accounts_for_every_declared_id() {
    let table = Table::build(PROTOCOL, play::clientbound::ENTRIES, CLIENTBOUND, IGNORED)
        .expect("every play::clientbound id must have a handler or an IGNORED entry");

    // Falsifiable rather than tautological: every handled id dispatches to
    // something, and the table holds no entry for an ignored id, since
    // `Table::get` answers `None` for those by construction.
    assert_eq!(table.len(), CLIENTBOUND.len());
    assert_eq!(
        CLIENTBOUND.len() + IGNORED.len(),
        play::clientbound::ENTRIES.len(),
        "the two lists must partition the declared ids exactly -- an id in both would inflate \
         the sum and one in neither would shrink it"
    );
}

#[test]
fn dropping_an_ignored_entry_must_fail_construction() {
    // `minecraft:entity` is a real packet a live server sends constantly and
    // this family deliberately does not translate. Removing its exemption from
    // a local copy must make the id unaccounted for -- the silent catch-all
    // arm, reborn as a build error, actually firing rather than merely being
    // present in the source.
    const DROPPED: &str = "minecraft:entity";
    let expected_id = play::clientbound::ENTRIES
        .iter()
        .find(|(name, _)| *name == DROPPED)
        .map(|(_, id)| *id)
        .expect("protocol 5 declares an entity packet");

    let mut thinned: Vec<_> = IGNORED
        .iter()
        .copied()
        .filter(|entry| entry.name != DROPPED)
        .collect();
    assert_eq!(
        thinned.len(),
        IGNORED.len() - 1,
        "the filter must remove exactly one entry, or the control proves nothing"
    );
    thinned.sort_by_key(|entry| entry.name);

    let err = Table::build(PROTOCOL, play::clientbound::ENTRIES, CLIENTBOUND, &thinned)
        .expect_err("dropping an IGNORED entry must fail construction");

    assert_eq!(
        err,
        DispatchError::UnlistedId {
            name: DROPPED,
            id: expected_id,
        }
    );
}

/// The table is keyed by name against the generated id table, so a handler
/// bound to a name that protocol 5 does not carry is a silent no-op rather
/// than an error — unless construction rejects it, which is what this pins.
#[test]
fn binding_a_handler_to_a_name_this_protocol_lacks_must_fail_construction() {
    let mut entries: Vec<(&str, i32)> = play::clientbound::ENTRIES.to_vec();
    let removed = entries
        .iter()
        .position(|(name, _)| *name == "minecraft:chat")
        .expect("protocol 5 declares a chat packet");
    entries.remove(removed);

    let err = Table::build(PROTOCOL, &entries, CLIENTBOUND, IGNORED)
        .expect_err("a handler for a name absent from ENTRIES must fail construction");
    assert!(
        format!("{err}").contains("minecraft:chat"),
        "the failure must name the offending packet, not merely refuse: {err}"
    );
}
