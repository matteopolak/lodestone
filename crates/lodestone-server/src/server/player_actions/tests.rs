//! Tests for attack, swing, spectator and recipe-book handlers.

use super::*;
use lodestone_model::Vec3;
use uuid::Uuid;

#[test]
fn attack_records_a_main_hand_swing_for_remote_connections() {
    let registry = PlayerRegistry::new();
    let mut cursor = registry.swing_cursor();

    record_attack_swing(Some(&registry), 42);

    assert_eq!(
        registry.swings_since(&mut cursor),
        vec![crate::players::SwingEvent {
            entity_id: 42,
            hand: lodestone_model::Hand::Main,
        }],
        "an attack must reach the remote animation broadcast as a main-hand swing"
    );
}

#[test]
fn attack_has_no_singleplayer_broadcast_sink() {
    let registry = PlayerRegistry::new();
    let mut cursor = registry.swing_cursor();

    record_attack_swing(None, 42);

    assert!(
        registry.swings_since(&mut cursor).is_empty(),
        "the singleplayer path must not manufacture a remote swing event"
    );
}

#[test]
fn recipe_book_seen_accepts_only_advertised_display_ids() {
    let mut inventory = PlayerInventory::new();
    let valid = crate::crafting::recipe_book_entries()
        .first()
        .expect("the bundled recipe book has an entry")
        .id;
    assert!(inventory.recipe_book_entry_is_highlighted(valid));
    assert!(
        recipe_book_snapshot(&inventory)
            .into_iter()
            .find(|entry| entry.id == valid)
            .is_some_and(|entry| entry.highlight),
        "the join snapshot must expose an unacknowledged entry as highlighted"
    );

    let update = record_recipe_book_seen(&mut inventory, valid)
        .expect("an advertised display id must fold into a client update");
    assert!(
        !inventory.recipe_book_entry_is_highlighted(valid),
        "the validated packet must fold into the connection state"
    );
    assert!(
        !update.highlight,
        "the response must expose the cleared flag to the client read-model"
    );
    assert!(
        recipe_book_snapshot(&inventory)
            .into_iter()
            .find(|entry| entry.id == valid)
            .is_some_and(|entry| !entry.highlight),
        "the next snapshot must expose the acknowledgement to the client"
    );

    assert!(record_recipe_book_seen(&mut inventory, i32::MAX).is_none());
    assert!(
        inventory.recipe_book_entry_is_highlighted(i32::MAX),
        "an id absent from the advertised corpus must not manufacture seen state"
    );
}

/// `swing_action`'s two real inputs, against vanilla's own
/// `ClientboundAnimatePacket` constants (`SWING_MAIN_HAND = 0`,
/// `SWING_OFF_HAND = 3`) rather than the plausible-but-wrong `0`/`1`.
#[test]
fn swing_action_maps_hand_to_vanillas_animate_byte() {
    assert_eq!(swing_action(lodestone_model::Hand::Main), 0);
    assert_eq!(swing_action(lodestone_model::Hand::Off), 3);
}

/// The positive case: a spectator within range of a resolvable player
/// target gets the camera attached.
#[test]
fn spectator_action_resolves_a_nearby_player_target() {
    let registry = PlayerRegistry::new();
    let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(10.0, 64.0, 10.0));
    let result = apply_spectator_action(
        GameMode::Spectator,
        Some(target.entity_id()),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, Some(target.entity_id()));
}

/// **Control 1.** The identical setup, but not in spectator mode — the
/// feature is restricted to spectator mode.
#[test]
fn spectator_action_does_nothing_outside_spectator_mode() {
    let registry = PlayerRegistry::new();
    let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(10.0, 64.0, 10.0));
    let result = apply_spectator_action(
        GameMode::Survival,
        Some(target.entity_id()),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None, "survival mode must never attach a camera");
}

/// **Control 2.** A target far outside the interaction range must not
/// resolve, proving the range check is load-bearing rather than
/// decorative (a wrong implementation that ignores distance entirely
/// would pass every other case here).
#[test]
fn spectator_action_rejects_a_target_out_of_range() {
    let registry = PlayerRegistry::new();
    let target = registry.join("Target", Uuid::from_u128(1), Vec3::new(500.0, 64.0, 500.0));
    let result = apply_spectator_action(
        GameMode::Spectator,
        Some(target.entity_id()),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None, "a target 500+ blocks away must not resolve");
}

/// **Control 3.** No target on the wire (`OptionalInt` absent) must do
/// nothing, matching vanilla's own handler, which has no branch for it at
/// all.
#[test]
fn spectator_action_does_nothing_with_no_target() {
    let registry = PlayerRegistry::new();
    let result = apply_spectator_action(
        GameMode::Spectator,
        None,
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None);
}

/// **Control 4.** An unresolvable id (no mob, no player) must do nothing
/// rather than attach a camera to a fabricated position.
#[test]
fn spectator_action_rejects_an_unresolvable_target() {
    let registry = PlayerRegistry::new();
    let result = apply_spectator_action(
        GameMode::Spectator,
        Some(9999),
        Some((10.0, 64.0, 11.0)),
        &MobHandle::default(),
        Some(&registry),
    );
    assert_eq!(result, None);
}
