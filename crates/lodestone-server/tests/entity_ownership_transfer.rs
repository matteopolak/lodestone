//! Acceptance gates for the moving-entity ownership barrier.

use lodestone_server::entity_handoff::{
    EntityHandoffError, EntityHandoffToken, EntityOwnershipHandoff,
};
use lodestone_server::tick_region::TickOwner;

fn token(entity_id: i32, serial: usize, epoch: u64) -> EntityHandoffToken {
    EntityHandoffToken::new(
        entity_id,
        serial,
        epoch,
        TickOwner::Chunk { cx: -1, cz: 0 },
        TickOwner::Chunk { cx: 0, cz: 0 },
    )
    .expect("the fixture names a real cross-owner transfer")
}

#[test]
fn source_stop_is_a_barrier_before_destination_start() {
    let mut handoff = EntityOwnershipHandoff::default();
    let transfer = token(9, 4, 1);

    assert_eq!(
        handoff.start_destination(transfer),
        Err(EntityHandoffError::DestinationBeforeSource)
    );
    handoff.stop_source(transfer).expect("source stop");
    handoff
        .start_destination(transfer)
        .expect("destination start after source stop");
    assert_eq!(handoff.pending(), 0);
}

#[test]
fn stale_duplicate_and_concurrent_transfers_fail_without_losing_newer_state() {
    let mut handoff = EntityOwnershipHandoff::default();
    let first = token(9, 4, 7);
    handoff.stop_source(first).expect("first source stop");
    assert_eq!(
        handoff.stop_source(first),
        Err(EntityHandoffError::DuplicateSourceStop)
    );
    assert_eq!(
        handoff.stop_source(token(9, 4, 8)),
        Err(EntityHandoffError::ConcurrentTransfer)
    );
    handoff
        .start_destination(first)
        .expect("first destination start");
    assert_eq!(
        handoff.start_destination(first),
        Err(EntityHandoffError::DuplicateDestinationStart)
    );
    assert_eq!(
        handoff.stop_source(token(9, 4, 6)),
        Err(EntityHandoffError::StaleEpoch)
    );

    let newer = token(9, 4, 8);
    handoff.stop_source(newer).expect("newer source stop");
    assert_eq!(handoff.pending(), 1);
    handoff
        .start_destination(newer)
        .expect("newer destination start");
    assert_eq!(handoff.pending(), 0);
}

#[test]
fn mismatched_destination_does_not_consume_the_active_route() {
    let mut handoff = EntityOwnershipHandoff::default();
    let transfer = token(11, 2, 3);
    handoff.stop_source(transfer).expect("source stop");
    let mismatched = EntityHandoffToken {
        destination: TickOwner::Chunk { cx: 1, cz: 0 },
        ..transfer
    };
    assert_eq!(
        handoff.start_destination(mismatched),
        Err(EntityHandoffError::MismatchedDestination)
    );
    assert_eq!(handoff.pending(), 1);
    handoff
        .start_destination(transfer)
        .expect("the matching destination still owns the route");
}

#[test]
fn forgetting_an_entity_releases_pending_state() {
    let mut handoff = EntityOwnershipHandoff::default();
    handoff.stop_source(token(17, 0, 11)).expect("source stop");
    handoff.forget_entity(17);
    assert_eq!(handoff.pending(), 0);
    handoff
        .stop_source(token(17, 0, 1))
        .expect("a removed id can start fresh state");
}
