//! Hermetic byte-exact tests for the serverbound action encoders `move` and
//! `swing`.
//!
//! Expected payloads are built from the wire specification using
//! `f64::to_be_bytes` / `f32::to_be_bytes` (authoritative IEEE-754 big-endian),
//! never from the adapter's own encoder, so a symmetric bug cannot pass.
//! `move_player_pos_rot` and `swing` layouts are verified against 26.2's
//! `ServerboundMovePlayerPacket.PosRot` and `ServerboundSwingPacket`.

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{ClientAction, ConnectionState, Hand, Rotation, Vec3, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_v770::packets::game::{MovePlayerPosRot, Swing};

const CTX: Ctx = Ctx { version: 776 };

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    reader.ensure_empty().expect("no trailing bytes");
    value
}

fn move_action(on_ground: bool) -> ClientAction {
    ClientAction::Move {
        pos: Vec3 {
            x: 1.5,
            y: 64.0,
            z: -2.5,
        },
        rotation: Rotation {
            yaw: 90.0,
            pitch: -45.0,
        },
        on_ground,
    }
}

/// Hand-built `move_player_pos_rot` body for [`move_action`] with the given
/// flags byte.
fn move_golden(flags: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1.5_f64.to_be_bytes());
    bytes.extend_from_slice(&64.0_f64.to_be_bytes());
    bytes.extend_from_slice(&(-2.5_f64).to_be_bytes());
    bytes.extend_from_slice(&90.0_f32.to_be_bytes());
    bytes.extend_from_slice(&(-45.0_f32).to_be_bytes());
    bytes.push(flags);
    bytes
}

#[test]
fn encode_action_move_on_ground_is_byte_exact() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &move_action(true))
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((play::serverbound::MOVE_PLAYER_POS_ROT, move_golden(0x01)))
    );
}

#[test]
fn encode_action_move_airborne_clears_on_ground_bit() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &move_action(false))
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((play::serverbound::MOVE_PLAYER_POS_ROT, move_golden(0x00)))
    );
}

#[test]
fn move_body_round_trips_from_golden_bytes() {
    // Symmetric guard: decode the hand-built vector and confirm the fields, so
    // the encoder and decoder are both pinned to the wire, not to each other.
    let body: MovePlayerPosRot = decode(&move_golden(0x01));
    assert_eq!(body.x, 1.5);
    assert_eq!(body.y, 64.0);
    assert_eq!(body.z, -2.5);
    assert_eq!(body.yaw, 90.0);
    assert_eq!(body.pitch, -45.0);
    assert_eq!(body.flags, 0x01);
}

#[test]
fn encode_action_swing_main_hand_is_single_zero_byte() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SwingArm { hand: Hand::Main },
        )
        .expect("encode swing");
    assert_eq!(encoded, Some((play::serverbound::SWING, vec![0x00])));
}

#[test]
fn encode_action_swing_off_hand_is_single_one_byte() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SwingArm { hand: Hand::Off },
        )
        .expect("encode swing");
    assert_eq!(encoded, Some((play::serverbound::SWING, vec![0x01])));
    let body: Swing = decode(&[0x01]);
    assert_eq!(body.hand, 1);
}

#[test]
fn move_and_swing_are_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(ConnectionState::Configuration, &move_action(true))
            .expect("encode"),
        None,
        "movement is a play-state action only"
    );
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::SwingArm { hand: Hand::Main }
            )
            .expect("encode"),
        None,
        "swing is a play-state action only"
    );
}
