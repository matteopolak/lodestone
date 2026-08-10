//! **The wire shape of the two animation packets the integrated server had no
//! encoder for at all**: `hurt_animation` (the camera damage tilt and the red
//! hurt flash) and `entity_event` byte 3 (`LivingEntity.die`'s fall-over).
//!
//! # Where the expected values come from
//!
//! `tests/entity_events.rs` builds both payloads **by hand** from the packet
//! specs — `hurt_animation_emits_yaw` writes `var_i32` then a big-endian `f32`,
//! and `entity_event_emits_id_and_status` writes `i32::to_be_bytes` then a raw
//! status byte — and asserts the adapter decodes each into the matching
//! `ClientEvent`. Both fixtures predate these encoders and were written from the
//! decode side, so checking encoder bytes against the same construction is two
//! independent transcriptions agreeing rather than a self-round-trip.
//!
//! # The error each gate is actually pointed at
//!
//! The two packets have the **same two logical fields in the same order at
//! different widths**, which is the trap: `ClientboundHurtAnimationPacket.write`
//! is `writeVarInt` + `writeFloat` while `ClientboundEntityEventPacket.write` is
//! `writeInt` + `writeByte`. Porting either from its field list rather than from
//! `write` yields something that looks right and desynchronises the stream. So
//! both gates assert the **payload length** as well as the bytes, because that is
//! the thing a width mistake moves and a field-order mistake does not.
//!
//! Fixture values are chosen so no wrong hypothesis lands on the right answer:
//! the entity ids are above 127 so their VarInt and fixed-width encodings differ
//! in length, and the hurt yaw is off-axis and non-round so a hardcoded `0.0`, a
//! truncation to an integer, or a degrees/radians mix-up all fail.

use lodestone_core::{Reader, Writer};
use lodestone_server::entity_event;
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;

fn var_i32(v: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(v);
    w.as_slice().to_vec()
}

/// `hurt_animation` is a VarInt id then an IEEE-754 `f32` yaw, in that order.
///
/// The yaw is `23.130102`, which is
/// `HurtDirection::from_source`'s answer for a hit at `(3, ·, 4)` on a victim at
/// the origin facing 30° — deliberately **off-axis**, because a frontal hit
/// yields `0.0` and `0.0` is exactly what a hardcoded placeholder would also
/// produce. A gate whose only fixture is a frontal hit cannot tell a wired yaw
/// from an ignored one.
///
/// `300` for the id rather than a small number: its VarInt is two bytes, so a
/// mistaken fixed-width `i32` (the shape [`entity_event`] genuinely wants, one
/// method away in the same impl) fails on length rather than surviving as a
/// plausible payload.
#[test]
fn hurt_animation_encodes_a_varint_id_then_a_float_yaw() {
    let proto = V770ServerProtocol::default();
    let yaw = 23.130_102_f32;

    let mut expected = var_i32(300);
    expected.extend_from_slice(&yaw.to_be_bytes());
    // Two bytes of VarInt plus four of float. Stated as a number rather than
    // `expected.len()` so the assertion below is not checking a value against
    // itself.
    assert_eq!(expected.len(), 6, "the fixture itself must be 2 + 4 bytes");

    match proto.encode_hurt_animation(300, yaw) {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                play::clientbound::HURT_ANIMATION,
                "the damage tilt must go out as hurt_animation"
            );
            assert_eq!(
                payload.len(),
                6,
                "a fixed-width i32 id would make this 8 bytes — that is the \
                 transcription mistake this packet's sibling invites"
            );
            assert_eq!(
                payload, expected,
                "payload must be VarInt 300 then f32 23.130102, the byte string \
                 entity_events.rs decodes into EntityHurtAnimation"
            );

            // Field by field, so a failure names which one moved.
            let mut r = Reader::new(&payload);
            assert_eq!(r.var_i32().expect("entity id"), 300);
            let decoded = r.f32().expect("yaw");
            assert!(
                (decoded - yaw).abs() < f32::EPSILON,
                "yaw must survive as {yaw}, got {decoded} — a rounded or zeroed \
                 value here is the tilt pointing the wrong way"
            );
            assert!(
                r.ensure_empty().is_ok(),
                "no trailing bytes: our own adapter's decode rejects them"
            );
        }
        other => panic!("expected a Send directive, got {other:?}"),
    }
}

/// `entity_event` is a **fixed-width big-endian `i32`** id then one status byte —
/// not a VarInt id, which is the whole reason this gate exists next to the one
/// above.
///
/// `DEATH` is read from the named constant rather than written as `3`, so the
/// number is checked in one place (`entity_event::DEATH`, transcribed from
/// `EntityEvent.DEATH`) instead of once per call site.
#[test]
fn entity_event_encodes_a_fixed_width_id_then_the_status_byte() {
    let proto = V770ServerProtocol::default();

    let mut expected = 300i32.to_be_bytes().to_vec();
    expected.push(entity_event::DEATH);
    assert_eq!(expected.len(), 5, "the fixture itself must be 4 + 1 bytes");
    // The discriminating fact, asserted rather than assumed: a VarInt 300 is
    // shorter than the fixed-width form, so the two encodings cannot coincide at
    // this id.
    assert_ne!(
        var_i32(300).len(),
        4,
        "300 was chosen because its VarInt is not 4 bytes; at a smaller id the \
         width mistake would be invisible"
    );

    match proto.encode_entity_event(300, entity_event::DEATH) {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                play::clientbound::ENTITY_EVENT,
                "the death animation must go out as entity_event"
            );
            assert_eq!(
                payload.len(),
                5,
                "a VarInt id would make this 3 bytes — the mistake the sibling \
                 encoder above invites in the other direction"
            );
            assert_eq!(
                payload, expected,
                "payload must be i32 300 big-endian then byte 3, the byte string \
                 entity_events.rs decodes into EntityStatus"
            );

            let mut r = Reader::new(&payload);
            assert_eq!(r.i32().expect("entity id"), 300);
            assert_eq!(r.u8().expect("status"), 3);
            assert!(r.ensure_empty().is_ok(), "no trailing bytes");
        }
        other => panic!("expected a Send directive, got {other:?}"),
    }
}

/// The three taming/breeding statuses this crate also names, each round-tripping
/// through the same encoder.
///
/// Not a duplicate of the gate above: it pins that the *status byte is carried
/// through* rather than fixed, which a single-value gate cannot distinguish from
/// a hardcoded `3`. The values are checked against the constants, which is where
/// the transcription from `EntityEvent` lives.
#[test]
fn every_named_status_byte_reaches_the_wire_unchanged() {
    let proto = V770ServerProtocol::default();
    // Collected rather than asserted inside the loop, so a failure reports every
    // wrong arm instead of aborting on the first.
    let mut wrong: Vec<(u8, u8)> = Vec::new();
    for event in [
        entity_event::DEATH,
        entity_event::TAMING_FAILED,
        entity_event::TAMING_SUCCEEDED,
        entity_event::IN_LOVE_HEARTS,
    ] {
        match proto.encode_entity_event(7, event) {
            ServerDirective::Send { payload, .. } => {
                let mut r = Reader::new(&payload);
                assert_eq!(r.i32().expect("entity id"), 7);
                let got = r.u8().expect("status");
                if got != event {
                    wrong.push((event, got));
                }
            }
            other => panic!("expected a Send directive, got {other:?}"),
        }
    }
    assert!(
        wrong.is_empty(),
        "these statuses did not survive the encoder as (expected, got): {wrong:?}"
    );
    // And the numbers themselves, against `EntityEvent`'s declarations.
    assert_eq!(
        (
            entity_event::DEATH,
            entity_event::TAMING_FAILED,
            entity_event::TAMING_SUCCEEDED,
            entity_event::IN_LOVE_HEARTS
        ),
        (3, 6, 7, 18),
        "EntityEvent.DEATH/TAMING_FAILED/TAMING_SUCCEEDED/IN_LOVE_HEARTS. Note \
         IN_LOVE_HEARTS is 18, not LOVE_HEARTS (12) which is the villager's"
    );
}
