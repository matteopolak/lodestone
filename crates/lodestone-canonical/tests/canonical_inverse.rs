use lodestone_canonical::canonical::{self, CanonicalBlockState};
use lodestone_canonical::inverse::{self, InverseError};
use lodestone_data::block_states;

#[test]
fn inverse_image_has_one_representative_per_reachable_state() {
    let mut image = std::collections::BTreeSet::new();
    for old_id in 0..=255u8 {
        for meta in 0..16u8 {
            if let CanonicalBlockState::Resolved(state) = canonical::resolve(old_id, meta) {
                image.insert(state);
            }
        }
    }
    assert_eq!(image.len(), 1582, "the exact canonical image count drifted");
    for &state in &image {
        let packed = inverse::resolve(state).expect("every image state has a representative");
        let (old_id, meta) = ((packed >> 4) as u8, (packed & 0x0f) as u8);
        assert_eq!(canonical::resolve(old_id, meta), CanonicalBlockState::Resolved(state));
    }
}

#[test]
fn inverse_alias_control_chooses_the_minimum_packed_legacy_value() {
    let first = canonical::resolve(8, 0);
    assert_eq!(first, canonical::resolve(9, 0), "the control must be a real alias");
    let CanonicalBlockState::Resolved(state) = first else {
        panic!("alias control must resolve");
    };
    assert_eq!(inverse::resolve(state), Ok(128));
    assert_eq!(canonical::resolve(8, 0), CanonicalBlockState::Resolved(state));
}

#[test]
fn inverse_rejects_a_canonical_state_outside_the_exact_image() {
    let mut image = std::collections::BTreeSet::new();
    for old_id in 0..=255u8 {
        for meta in 0..16u8 {
            if let CanonicalBlockState::Resolved(state) = canonical::resolve(old_id, meta) {
                image.insert(state);
            }
        }
    }
    let unsupported = (0..block_states::STATE_COUNT)
        .find(|state| !image.contains(state))
        .expect("the pre-1.13 image must not cover the full modern registry");
    assert_eq!(
        inverse::resolve(unsupported),
        Err(InverseError::Unsupported { state: unsupported })
    );
}
