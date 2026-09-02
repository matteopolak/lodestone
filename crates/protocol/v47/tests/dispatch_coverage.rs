//! Proves protocol 47's `play::clientbound` dispatch table is internally
//! consistent, and that the check is not vacuous.
//!
//! `V47Adapter::play_dispatch_table` already asserts this at construction
//! time (a `.expect(...)` on `Table::build`), so a broken table would fail
//! every call to `handle_play` -- but that only demonstrates the table is
//! *built somewhere*. This test rebuilds it directly from the same public
//! pieces (`packet_ids::play::clientbound::ENTRIES`, `adapter::CLIENTBOUND`,
//! `adapter::IGNORED`) so the assertion is explicit, plus a negative control
//! that removes one `IGNORED` entry and requires construction to fail --
//! proving the detector itself still works rather than trusting the happy
//! path alone (`docs/testing-policy.md`'s bar for a construction-time check
//! like this one).

use lodestone_core::dispatch::{DispatchError, Table};
use lodestone_v47::adapter::{CLIENTBOUND, IGNORED, PROTOCOL};
use lodestone_v47::packet_ids::play;

#[test]
fn play_dispatch_table_builds_for_every_entries_id() {
    let table = Table::build(PROTOCOL, play::clientbound::ENTRIES, CLIENTBOUND, IGNORED)
        .expect("every play::clientbound id must have a handler or an IGNORED entry");

    // A real, falsifiable claim: every handled id actually dispatches to
    // something, and the table has no entries for ids that are on the
    // IGNORED list (Table::get returns None for those by construction).
    assert_eq!(table.len(), CLIENTBOUND.len());
}

#[test]
fn negative_control_dropping_an_ignored_entry_fails_construction() {
    // `minecraft:update_time` (id 3) has no handler in CLIENTBOUND and is
    // deliberately on IGNORED ("v770 has this; backport"). Drop it from a
    // local copy of the list and require Table::build to notice the id is
    // now unaccounted for -- the `_ =>` island, reborn as a build error,
    // actually firing rather than merely being present in the source.
    let mut without_update_time: Vec<_> = IGNORED
        .iter()
        .copied()
        .filter(|entry| entry.name != "minecraft:update_time")
        .collect();
    assert_eq!(
        without_update_time.len(),
        IGNORED.len() - 1,
        "the filter must actually remove exactly one entry"
    );
    without_update_time.sort_by_key(|entry| entry.name);

    let err = Table::build(
        PROTOCOL,
        play::clientbound::ENTRIES,
        CLIENTBOUND,
        &without_update_time,
    )
    .expect_err("dropping update_time's IGNORED entry must fail construction");

    assert_eq!(
        err,
        DispatchError::UnlistedId {
            name: "minecraft:update_time",
            id: 3,
        }
    );
}
