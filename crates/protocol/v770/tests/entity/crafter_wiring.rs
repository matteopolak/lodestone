//! Hermetic wiring test for `CONTAINER_SLOT_STATE_CHANGED`, the auto-crafting
//! `Crafter` block entity's own remainder: a crafter's per-slot enable/
//! disable toggle. `ServerboundContainerSlotStateChangedPacket`'s wire layout
//! is a VarInt `slotId`, a VarInt `containerId`, then a plain boolean
//! `newState` (`crates/protocol/v770/src/packets/game.rs`'s
//! `ContainerSlotStateChanged` struct documents the same layout).
//!
//! `slot_id` and `container_id` are two adjacent VarInts of the same type —
//! exactly the shape CLAUDE.md's evidence standard warns transposes without a
//! trace — so every case below uses pairwise-distinct values for the two.

use lodestone_core::State;
use lodestone_server::ServerBound;
use lodestone_server::ServerProtocol;
use lodestone_v770::V770ServerProtocol;
use lodestone_v770::packet_ids::play;

/// `slot_id` 4, `container_id` 11 — pairwise-distinct, so a transposition of
/// the two VarInts (both would still be in-range, valid-looking values)
/// cannot survive this assertion.
#[test]
fn decode_container_slot_state_changed_disabling_a_slot() {
    let proto = V770ServerProtocol;
    let body = vec![
        0x04, // slot_id = 4 (VarInt)
        0x0B, // container_id = 11 (VarInt)
        0x00, // new_state = false (disable)
    ];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::CONTAINER_SLOT_STATE_CHANGED, &body),
        ServerBound::ContainerSlotStateChanged {
            window_id: 11,
            slot_id: 4,
            new_state: false,
        },
    );
}

/// The mirror case — different `slot_id`/`container_id` values than the
/// disable case above, and `new_state = true` — so a decoder that got the
/// boolean's byte value backwards (`0x01` read as "disable") or that reused
/// the previous test's field values by accident cannot pass both.
#[test]
fn decode_container_slot_state_changed_enabling_a_slot() {
    let proto = V770ServerProtocol;
    let body = vec![
        0x07, // slot_id = 7 (VarInt)
        0x02, // container_id = 2 (VarInt)
        0x01, // new_state = true (enable)
    ];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::CONTAINER_SLOT_STATE_CHANGED, &body),
        ServerBound::ContainerSlotStateChanged {
            window_id: 2,
            slot_id: 7,
            new_state: true,
        },
    );
}

/// **Control**: a short/malformed frame must not construct a variant with
/// truncated or zeroed fields.
#[test]
fn decode_container_slot_state_changed_rejects_a_short_frame() {
    let proto = V770ServerProtocol;
    let short = vec![0x04];
    assert_eq!(
        proto.decode(State::Play, play::serverbound::CONTAINER_SLOT_STATE_CHANGED, &short),
        ServerBound::Ignored,
    );
}
