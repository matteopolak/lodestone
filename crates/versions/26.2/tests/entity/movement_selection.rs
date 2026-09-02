//! Hermetic byte-exact tests for vanilla's movement-packet **selection
//! rule**: which of `move_player_pos`, `move_player_pos_rot`,
//! `move_player_rot`, `move_player_status_only` (or nothing at all) a tick's
//! `ClientAction::Move` produces.
//!
//! This is deliberately *not* a per-packet round-trip test — a round-trip
//! passes even when the encoder is self-consistently wrong. These tests pin
//! the exact sequence of wire bytes a stateful adapter instance produces
//! across several consecutive `Move` actions, mirroring vanilla's own
//! client-side local-player position-send tick (confirmed against the
//! decompiled 26.2 client source) against
//! vanilla's own serverbound move-player packet's four nested packet
//! classes. Expected
//! bodies are built by hand from `f64::to_be_bytes` / `f32::to_be_bytes`,
//! never from the adapter's own encoder.
//!
//! The rule (see `vanilla's own local player's own send position`): position is "dirty" when the
//! squared distance from the last **sent** position exceeds `(2e-4)²`, or
//! every 20 ticks regardless of movement (a periodic forced update,
//! `positionReminder >= 20`); rotation is dirty on *any* nonzero yaw/pitch
//! delta from the last sent rotation. Both dirty sends `PosRot`; position
//! only sends `Pos`; rotation only sends `Rot`; neither, but on-ground or
//! horizontal-collision changed since the last tick, sends `StatusOnly`;
//! otherwise nothing is sent — a deliberate `None`, not a bug. A single
//! adapter instance is reused across each test's sequence of calls, exactly
//! as one adapter is reused across a connection's lifetime.

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{ClientAction, ConnectionState, Rotation, Vec3, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_v26_2::packets::game::{MovePlayerPos, MovePlayerRot, MovePlayerStatusOnly};

const CTX: Ctx = Ctx { version: 776 };

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let value = T::decode(&mut reader, CTX).expect("decode");
    reader.ensure_empty().expect("no trailing bytes");
    value
}

fn move_action(
    pos: Vec3,
    rotation: Rotation,
    on_ground: bool,
    horizontal_collision: bool,
) -> ClientAction {
    ClientAction::Move {
        pos,
        rotation,
        on_ground,
        horizontal_collision,
    }
}

/// Hand-built `move_player_pos` body: `f64` x, y, z, then a flags byte.
fn pos_golden(x: f64, y: f64, z: f64, flags: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&x.to_be_bytes());
    bytes.extend_from_slice(&y.to_be_bytes());
    bytes.extend_from_slice(&z.to_be_bytes());
    bytes.push(flags);
    bytes
}

/// Hand-built `move_player_rot` body: `f32` yaw, pitch, then a flags byte.
fn rot_golden(yaw: f32, pitch: f32, flags: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&yaw.to_be_bytes());
    bytes.extend_from_slice(&pitch.to_be_bytes());
    bytes.push(flags);
    bytes
}

/// Hand-built `move_player_status_only` body: just a flags byte.
fn status_only_golden(flags: u8) -> Vec<u8> {
    vec![flags]
}

const BASE_POS: Vec3 = Vec3 {
    x: 100.0,
    y: 64.0,
    z: -200.0,
};
const BASE_ROT: Rotation = Rotation {
    yaw: 0.0,
    pitch: 0.0,
};

/// The very first `Move` on a fresh adapter always reads as maximally dirty
/// (vanilla's `LocalPlayer` fields zero-initialize identically), so it always
/// sends `PosRot`. Establishing that baseline here, rather than relying on
/// `serverbound_actions.rs`, keeps every test in this file self-contained.
fn establish_baseline(adapter: &V770Adapter) {
    adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, true, false),
        )
        .expect("encode initial move")
        .expect("initial move always sends pos_rot");
}

#[test]
fn position_only_change_sends_pos_packet() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    let next_pos = Vec3 {
        x: BASE_POS.x + 1.0,
        y: BASE_POS.y,
        z: BASE_POS.z,
    };
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(next_pos, BASE_ROT, true, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::MOVE_PLAYER_POS,
            pos_golden(next_pos.x, next_pos.y, next_pos.z, 0x01)
        ))
    );

    let body: MovePlayerPos = decode(&pos_golden(next_pos.x, next_pos.y, next_pos.z, 0x01));
    assert_eq!(body.x, next_pos.x);
    assert_eq!(body.y, next_pos.y);
    assert_eq!(body.z, next_pos.z);
    assert_eq!(body.flags, 0x01);
}

#[test]
fn rotation_only_change_sends_rot_packet() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    let next_rot = Rotation {
        yaw: 45.0,
        pitch: -10.0,
    };
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, next_rot, true, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::MOVE_PLAYER_ROT,
            rot_golden(next_rot.yaw, next_rot.pitch, 0x01)
        ))
    );

    let body: MovePlayerRot = decode(&rot_golden(next_rot.yaw, next_rot.pitch, 0x01));
    assert_eq!(body.yaw, next_rot.yaw);
    assert_eq!(body.pitch, next_rot.pitch);
    assert_eq!(body.flags, 0x01);
}

#[test]
fn both_position_and_rotation_change_sends_pos_rot_packet() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    let next_pos = Vec3 {
        x: BASE_POS.x + 2.0,
        y: BASE_POS.y,
        z: BASE_POS.z - 3.0,
    };
    let next_rot = Rotation {
        yaw: 90.0,
        pitch: 12.0,
    };
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(next_pos, next_rot, false, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded.map(|(id, _)| id),
        Some(play::serverbound::MOVE_PLAYER_POS_ROT)
    );
}

#[test]
fn negligible_delta_sends_nothing() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    // Well under the (2e-4)^2 = 4e-8 squared-distance threshold, and rotation
    // and status both unchanged: vanilla sends nothing this tick.
    let jittered_pos = Vec3 {
        x: BASE_POS.x + 1e-6,
        y: BASE_POS.y,
        z: BASE_POS.z,
    };
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(jittered_pos, BASE_ROT, true, false),
        )
        .expect("encode move");
    assert_eq!(encoded, None, "sub-epsilon jitter must not be sent at all");
}

#[test]
fn on_ground_change_alone_sends_status_only_packet() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    // Same position and rotation as the baseline, but on_ground flips from
    // true to false: only the status changed, so `StatusOnly` is sent with
    // the on-ground bit cleared.
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, false, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::MOVE_PLAYER_STATUS_ONLY,
            status_only_golden(0x00)
        ))
    );

    let body: MovePlayerStatusOnly = decode(&status_only_golden(0x00));
    assert_eq!(body.flags, 0x00);
}

#[test]
fn horizontal_collision_change_alone_sends_status_only_packet() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    // Same position, rotation, and on_ground as the baseline, but
    // horizontal_collision flips from false to true.
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, true, true),
        )
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::MOVE_PLAYER_STATUS_ONLY,
            status_only_golden(0x03)
        ))
    );
}

#[test]
fn nothing_changed_at_all_sends_nothing() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, true, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded, None,
        "an exact repeat of the last-sent pose and status must send nothing"
    );
}

#[test]
fn periodic_reminder_forces_a_position_send_every_20_ticks_even_when_idle() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    // 19 further idle ticks (identical pose/status to the baseline): every
    // one of these must send nothing, since nothing changed and the
    // reminder counter (currently at 0 after the baseline call, which reset
    // it on send) hasn't yet reached 20.
    for _ in 0..19 {
        let encoded = adapter
            .encode_action(
                ConnectionState::Play,
                &move_action(BASE_POS, BASE_ROT, true, false),
            )
            .expect("encode move");
        assert_eq!(
            encoded, None,
            "idle ticks before the 20-tick mark send nothing"
        );
    }

    // The 20th tick since the baseline forces a full position resend
    // (`positionReminder >= 20`) even though nothing actually moved. Since
    // rotation is still unchanged, this is a `Pos` packet, not `PosRot`.
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, true, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::MOVE_PLAYER_POS,
            pos_golden(BASE_POS.x, BASE_POS.y, BASE_POS.z, 0x01)
        )),
        "the 20th idle tick must force a periodic position resend"
    );

    // And the reminder resets: the next idle tick goes quiet again.
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, true, false),
        )
        .expect("encode move");
    assert_eq!(
        encoded, None,
        "the reminder resets after firing, so the next tick is idle again"
    );
}

#[test]
fn move_is_not_selected_outside_play_state() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Configuration,
            &move_action(BASE_POS, BASE_ROT, true, false),
        )
        .expect("encode");
    assert_eq!(encoded, None, "movement is a play-state action only");
}
