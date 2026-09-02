//! Selection tests for 1.16.5's `LocalPlayer.sendPosition`.

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{ClientAction, ConnectionState, Rotation, Vec3, VersionAdapter};
use lodestone_v1_14::packet_ids::play;
use lodestone_v1_14::packets::game::{
    ServerboundFlying, ServerboundLook, ServerboundPosition, ServerboundPositionLook,
};
use lodestone_v1_14::V735Adapter;

const BASE_POS: Vec3 = Vec3 { x: 100.0, y: 64.0, z: -200.0 };
const BASE_ROT: Rotation = Rotation { yaw: 5.0, pitch: -10.0 };
const CTX: Ctx = Ctx { version: 754 };

fn action(pos: Vec3, rotation: Rotation, on_ground: bool) -> ClientAction {
    ClientAction::Move { pos, rotation, on_ground, horizontal_collision: false }
}

fn encode(adapter: &V735Adapter, action: ClientAction) -> Option<(i32, Vec<u8>)> {
    adapter
        .encode_action(ConnectionState::Play, &action)
        .expect("movement encoding")
}

fn decode<T: Decode>(bytes: &[u8]) -> T {
    let mut reader = Reader::new(bytes);
    let packet = T::decode(&mut reader, CTX).expect("packet body decodes");
    assert!(reader.is_empty(), "packet body has no trailing bytes");
    packet
}

#[test]
fn vanilla_1_16_selects_position_look_position_and_look_by_dirty_axes() {
    let adapter = V735Adapter::new();
    let (id, body) = encode(&adapter, action(BASE_POS, BASE_ROT, true)).expect("position/look");
    assert_eq!(id, play::serverbound::POSITION_LOOK);
    assert_eq!(
        decode::<ServerboundPositionLook>(&body),
        ServerboundPositionLook {
            x: BASE_POS.x,
            y: BASE_POS.y,
            z: BASE_POS.z,
            yaw: BASE_ROT.yaw,
            pitch: BASE_ROT.pitch,
            on_ground: true,
        }
    );
    let moved = Vec3 { x: BASE_POS.x + 1.0, ..BASE_POS };
    let (id, body) = encode(&adapter, action(moved, BASE_ROT, false)).expect("position");
    assert_eq!(id, play::serverbound::POSITION);
    assert_eq!(
        decode::<ServerboundPosition>(&body),
        ServerboundPosition { x: moved.x, y: moved.y, z: moved.z, on_ground: false }
    );
    let turned = Rotation { yaw: BASE_ROT.yaw + 1.0, ..BASE_ROT };
    let (id, body) = encode(&adapter, action(moved, turned, true)).expect("look");
    assert_eq!(id, play::serverbound::LOOK);
    assert_eq!(
        decode::<ServerboundLook>(&body),
        ServerboundLook { yaw: turned.yaw, pitch: turned.pitch, on_ground: true }
    );
}

#[test]
fn vanilla_1_16_is_quiet_when_idle_but_reports_an_on_ground_transition() {
    let adapter = V735Adapter::new();
    encode(&adapter, action(BASE_POS, BASE_ROT, true));
    assert_eq!(encode(&adapter, action(BASE_POS, BASE_ROT, true)), None);
    let (id, body) = encode(&adapter, action(BASE_POS, BASE_ROT, false)).expect("on-ground change");
    assert_eq!(id, play::serverbound::FLYING);
    assert_eq!(body, vec![0]);
    assert_eq!(decode::<ServerboundFlying>(&body), ServerboundFlying { on_ground: false });
}

#[test]
fn vanilla_1_16_forces_position_on_the_20th_idle_tick() {
    let adapter = V735Adapter::new();
    encode(&adapter, action(BASE_POS, BASE_ROT, true));
    for _ in 0..19 {
        assert_eq!(encode(&adapter, action(BASE_POS, BASE_ROT, true)), None);
    }
    assert_eq!(
        encode(&adapter, action(BASE_POS, BASE_ROT, true)).map(|(id, _)| id),
        Some(play::serverbound::POSITION)
    );
}

#[test]
fn vanilla_1_16_ignores_sub_threshold_position_jitter() {
    let adapter = V735Adapter::new();
    encode(&adapter, action(BASE_POS, BASE_ROT, true));
    let jitter = Vec3 { x: BASE_POS.x + 0.02, ..BASE_POS };
    assert_eq!(encode(&adapter, action(jitter, BASE_ROT, true)), None);
}

#[test]
fn vanilla_1_16_uses_a_strict_nine_e_minus_four_position_boundary() {
    let adapter = V735Adapter::new();
    let zero = Vec3::default();
    let rotation = Rotation::default();
    assert_eq!(encode(&adapter, action(zero, rotation, false)), None);

    let exact = Vec3 { x: 0.03, ..zero };
    assert_eq!(exact.x * exact.x, 9e-4);
    assert_eq!(encode(&adapter, action(exact, rotation, false)), None);

    let above = Vec3 { x: 0.030_000_1, ..zero };
    let (id, body) = encode(&adapter, action(above, rotation, false)).expect("position above boundary");
    assert_eq!(id, play::serverbound::POSITION);
    assert_eq!(
        decode::<ServerboundPosition>(&body),
        ServerboundPosition { x: above.x, y: above.y, z: above.z, on_ground: false }
    );
}

#[test]
fn fresh_1_16_adapter_starts_from_a_zero_baseline_and_clones_share_it() {
    let adapter = V735Adapter::new();
    let zero = Vec3::default();
    let rotation = Rotation::default();
    assert_eq!(encode(&adapter, action(zero, rotation, false)), None);

    let moved = Vec3 { x: 1.0, ..zero };
    assert_eq!(
        encode(&adapter, action(moved, rotation, false)).map(|(id, _)| id),
        Some(play::serverbound::POSITION)
    );
    let clone = adapter.clone();
    assert_eq!(encode(&clone, action(moved, rotation, false)), None);
}
