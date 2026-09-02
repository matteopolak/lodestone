//! Hermetic wiring tests for issue #616's `SET_BEACON` remainder: the
//! serverbound `ServerboundSetBeaconPacket` decode, and the two clientbound
//! packets (`update_mob_effect`/`remove_mob_effect`) a beacon's periodic
//! effect application needs to actually put a buff icon on a client's HUD.
//!
//! Serverbound bytes are hand-built from the wire spec
//! (`vanilla's own mob effect's own stream codec` is a direct, 0-based registry VarInt — verified
//! against the committed `MOB_EFFECT_NAMES` table, not assumed). Clientbound
//! bytes go through the real, independently written
//! [`V770Adapter::handle_packet`] — the same "two independent
//! implementations of one spec" shape `container_encoders.rs` and
//! `book_content_wiring.rs` already use, chosen for the same reason their own
//! doc comments give: `decode(encode(x)) == x` alone proves nothing
//! (CLAUDE.md's evidence standard).

use lodestone_core::State;
use lodestone_model::{ClientEvent, ConnectionState, Directive, ResourceKey, VersionAdapter};
use lodestone_server::{ServerBound, ServerDirective, ServerProtocol};
use lodestone_v770::V770Adapter;
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

fn handle(id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, id, payload)
        .expect("handle packet")
}

fn key(s: &str) -> ResourceKey {
    s.parse().expect("valid key")
}

// ---------------------------------------------------------------------
// Serverbound: `SET_BEACON`
// ---------------------------------------------------------------------

/// Both powers set: primary `minecraft:speed` (registry id `0`), secondary
/// `minecraft:regeneration` (registry id `9`) — pairwise-distinct ids, so a
/// primary/secondary transposition cannot survive.
#[test]
fn decode_set_beacon_with_both_powers_selected() {
    let proto = V770ServerProtocol;
    let body = vec![
        0x01, 0x00, // primary present, id 0 (speed)
        0x01, 0x09, // secondary present, id 9 (regeneration)
    ];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::SET_BEACON, &body),
        ServerBound::SetBeacon {
            primary: Some("minecraft:speed".to_owned()),
            secondary: Some("minecraft:regeneration".to_owned()),
        },
    );
}

/// A primary with no secondary — the far more common submission (levels
/// 1–3), and the case that most needs its own coverage: a decoder that
/// always read a second `Optional` unconditionally would still pass the
/// case above (which happens to have one) and only fail here.
#[test]
fn decode_set_beacon_with_only_a_primary() {
    let proto = V770ServerProtocol;
    let body = vec![
        0x01, 0x04, // primary present, id 4 (strength)
        0x00, // secondary absent
    ];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::SET_BEACON, &body),
        ServerBound::SetBeacon {
            primary: Some("minecraft:strength".to_owned()),
            secondary: None,
        },
    );
}

/// Clearing both powers (both bools `false`) — the third reachable shape,
/// distinct from "no packet at all".
#[test]
fn decode_set_beacon_clearing_both_powers() {
    let proto = V770ServerProtocol;
    let body = vec![0x00, 0x00];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::SET_BEACON, &body),
        ServerBound::SetBeacon { primary: None, secondary: None },
    );
}

/// **Control**: a short/malformed frame must not construct a variant with
/// truncated or zeroed fields.
#[test]
fn decode_set_beacon_rejects_a_short_frame() {
    let proto = V770ServerProtocol;
    let short = vec![0x01];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::SET_BEACON, &short),
        ServerBound::Ignored,
    );
}

// ---------------------------------------------------------------------
// Clientbound: `update_mob_effect` / `remove_mob_effect`
// ---------------------------------------------------------------------

/// A beacon-shaped application (ambient, visible, icon shown, no blend) with
/// every numeric field distinct from every other (entity id, amplifier,
/// duration all pairwise-different) so a field transposition cannot survive,
/// round-tripped through the real, independently written client decoder.
#[test]
fn update_mob_effect_reaches_a_client_as_the_right_entity_and_effect() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } =
        proto.encode_update_mob_effect(11, "minecraft:regeneration", 1, 340, true, true, true, false)
    else {
        panic!("expected a Send directive");
    };

    let events = handle(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let Directive::Emit(ClientEvent::MobEffectApplied {
        entity_id,
        effect,
        amplifier,
        duration_ticks,
        ambient,
        visible,
        show_icon,
        blend,
    }) = &events[0]
    else {
        panic!("expected MobEffectApplied, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 11);
    assert_eq!(*effect, key("minecraft:regeneration"));
    assert_eq!(*amplifier, 1);
    assert_eq!(*duration_ticks, 340);
    assert!(ambient);
    assert!(visible);
    assert!(show_icon);
    assert!(!blend);
}

/// **Control** for the four-bit flag byte: every flag set to a *different*
/// combination than the case above (three `true`s moved, one dropped) must
/// decode to exactly that different combination — the discriminating case
/// for a stuck-at-one-value or transposed-bit encoder.
#[test]
fn update_mob_effect_flag_bits_are_not_stuck_or_transposed() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } =
        proto.encode_update_mob_effect(4, "minecraft:speed", 0, 20, false, false, false, true)
    else {
        panic!("expected a Send directive");
    };
    let events = handle(packet_id, &payload);
    let Directive::Emit(ClientEvent::MobEffectApplied {
        ambient, visible, show_icon, blend, ..
    }) = &events[0]
    else {
        panic!("expected MobEffectApplied, got {:?}", events[0]);
    };
    assert!(!ambient);
    assert!(!visible);
    assert!(!show_icon);
    assert!(blend);
}

#[test]
fn remove_mob_effect_reaches_a_client_as_the_right_entity_and_effect() {
    let proto = V770ServerProtocol;
    let ServerDirective::Send { packet_id, payload } = proto.encode_remove_mob_effect(22, "minecraft:strength")
    else {
        panic!("expected a Send directive");
    };
    let events = handle(packet_id, &payload);
    assert_eq!(events.len(), 1);
    let Directive::Emit(ClientEvent::MobEffectRemoved { entity_id, effect }) = &events[0] else {
        panic!("expected MobEffectRemoved, got {:?}", events[0]);
    };
    assert_eq!(*entity_id, 22);
    assert_eq!(*effect, key("minecraft:strength"));
}
