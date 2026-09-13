//! The serverbound cargo-fuzz target must select from real packet ids rather
//! than spending its input budget on the unknown-id fallback.

use lodestone_fuzz::Family;

#[test]
fn every_v770_connection_state_has_distinct_serverbound_entries() {
    let mut total = 0;
    for state in Family::STATES {
        let entries = Family::V770.serverbound_entries(state);
        assert!(!entries.is_empty(), "no serverbound entries for {state:?}");

        let mut ids = entries.iter().map(|(_, id)| *id).collect::<Vec<_>>();
        ids.sort_unstable();
        assert!(ids.windows(2).all(|pair| pair[0] != pair[1]));
        assert!(entries.iter().all(|(name, _)| !name.is_empty()));
        total += entries.len();
    }

    assert!(total > Family::STATES.len());
}
