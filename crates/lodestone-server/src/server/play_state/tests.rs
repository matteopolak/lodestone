//! Tests for teleport acknowledgement tracking.

use super::*;

#[test]
fn only_the_latest_teleport_acknowledgement_releases_movement() {
    let mut acknowledgements = TeleportAcknowledgements::after_initial(41);
    let replacement = acknowledgements.issue();

    assert_eq!(replacement, 42);
    assert!(
        !acknowledgements.accepts(41),
        "a late acknowledgement for the superseded join correction must stay pending"
    );
    assert!(
        acknowledgements.is_pending(),
        "a stale acknowledgement must not clear the newer correction"
    );
    assert!(acknowledgements.accepts(42));
    assert!(
        !acknowledgements.is_pending(),
        "the current acknowledgement must release the movement gate"
    );
    assert!(
        !acknowledgements.accepts(42),
        "a duplicate acknowledgement must not recreate an accepted state"
    );
}
