use lodestone_core::{Decode, Encode};
use lodestone_model::BlockPos;
use lodestone_v26_3::packets::{
    AcceptTeleportation, AddTransientBlock, DeltaPath, DeltaStep, EntityPositionSync,
    MoveEntityPos, MoveEntityPosRot, MoveEntityRot, OpenSignEditor, PositionPath, PositionStep,
    PostEffects, Punch, SignTextSlot, SignUpdate, SwingAnimation, SwingKind, WireHand,
    ParticleDistribution, ParticleSpawn, WireStateId,
    decode_body, encode_body,
};

fn hex(value: &str) -> Vec<u8> {
    value.as_bytes().chunks_exact(2).map(|pair| {
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
    }).collect()
}

fn assert_body<T: Decode + Encode + PartialEq + std::fmt::Debug>(body: &str, value: T) {
    let bytes = hex(body);
    assert_eq!(decode_body::<T>(&bytes).unwrap(), value);
    assert_eq!(encode_body(&value).unwrap(), bytes);
    let mut extended = bytes.clone();
    extended.push(0);
    assert!(decode_body::<T>(&extended).is_err());
    if !bytes.is_empty() {
        assert!(decode_body::<T>(&bytes[..bytes.len() - 1]).is_err());
    }
}

fn release_fixture(name: &str) -> String {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/new_packet_wire_controls_26_3.json",
    )).unwrap();
    fixtures["cases"][name]["body_hex"].as_str().unwrap().to_owned()
}

#[test]
fn post_effects_are_an_ordered_identifier_list() {
    assert_body(&release_fixture("post_effects_ordered"),
        PostEffects { effects: vec!["minecraft:spider".into(), "minecraft:creeper".into()] });
    assert_body(&release_fixture("post_effects_empty"), PostEffects { effects: Vec::new() });
}

#[test]
fn transient_block_position_uses_signed_packed_coordinates() {
    assert_body("fffffa4000075fd1ee0a", AddTransientBlock {
        pos: BlockPos { x: -23, y: -47, z: 117 },
        state: WireStateId::new(1390).unwrap(),
    });
}

#[test]
fn swing_carries_hand_kind_and_varint_duration() {
    assert_body(&release_fixture("swing"), SwingAnimation {
        entity_id: 291, hand: WireHand::Off, kind: SwingKind::Stab, duration_ticks: 129,
    });
    assert_body(&release_fixture("signed_swing"), SwingAnimation {
        entity_id: 291, hand: WireHand::Off, kind: SwingKind::Stab, duration_ticks: -3,
    });
}

#[test]
fn transient_block_fixture_retains_the_wire_identity_before_selected_translation() {
    assert_body(&release_fixture("transient_block"), AddTransientBlock {
        pos: BlockPos { x: -23, y: -47, z: 117 },
        state: WireStateId::new(12514).unwrap(),
    });
}

#[test]
fn punch_has_no_hand_field() {
    assert_body("", Punch);
    assert!(decode_body::<Punch>(&[1]).is_err());
}

#[test]
fn teleport_acknowledgement_carries_the_accepted_pose() {
    assert_body("a302c0372000000000004050e00000000000405d700000000000422e0000c1440000",
        AcceptTeleportation {
            id: 291, pos: [-23.125, 67.5, 117.75], yaw: 43.5, pitch: -12.25,
        });
    assert!(decode_body::<AcceptTeleportation>(&[0xa3, 2]).is_err());
}

#[test]
fn relative_linear_position_puts_properties_before_delta() {
    assert_body("a302010201fffe0005", MoveEntityPos {
        entity_id: 291, on_ground: true, path: DeltaPath::Linear([513, -2, 5]),
    });
    assert!(decode_body::<MoveEntityPos>(&hex("a3020201fffe000501")).is_err());
}

#[test]
fn relative_position_steps_have_individual_times() {
    assert_body("a3020581010201fffe0005030001fffc0007", MoveEntityPos {
        entity_id: 291, on_ground: true,
        path: DeltaPath::Stepped(vec![
            DeltaStep { ticks: 129, delta: [513, -2, 5] },
            DeltaStep { ticks: 3, delta: [1, -4, 7] },
        ]),
    });
}

#[test]
fn relative_position_rotation_keeps_angles_after_the_path() {
    assert_body("a302000201fffe00053f81", MoveEntityPosRot {
        entity_id: 291, on_ground: false, path: DeltaPath::Linear([513, -2, 5]),
        yaw: 63, pitch: 129,
    });
}

#[test]
fn rotation_only_puts_ground_before_angles() {
    assert_body("a302013f81", MoveEntityRot {
        entity_id: 291, on_ground: true, yaw: 63, pitch: 129,
    });
}

#[test]
fn particle_fields_preserve_vector_speed_and_varint_count() {
    assert_body("0100c0372000000000004050e00000000000405d7000000000003e000000bf4000003fc000003e800000bf0000003fa00000810102",
        ParticleSpawn {
            override_limiter: true, always_show: false, pos: [-23.125, 67.5, 117.75],
            offset: [0.125, -0.75, 1.5], speed: [0.25, -0.5, 1.25], count: 129,
            distribution: ParticleDistribution::AlternativeWithSpeed,
        });
}

#[test]
fn absolute_position_sync_has_a_path_discriminant() {
    assert_body("a30200c0372000000000004050e00000000000405d700000000000422e0000c144000001",
        EntityPositionSync {
            entity_id: 291, path: PositionPath::Linear([-23.125, 67.5, 117.75]),
            yaw: 43.5, pitch: -12.25, on_ground: true,
        });
}

#[test]
fn absolute_position_steps_have_individual_times() {
    assert_body("0101c0372000000000004050e00000000000405d7000000000008101",
        PositionPath::Stepped(vec![PositionStep { pos: [-23.125, 67.5, 117.75], ticks: 129 }]));
}

#[test]
fn sign_editor_uses_a_side_discriminant() {
    assert_body("fffffa4000075fd101", OpenSignEditor {
        pos: BlockPos { x: -23, y: -47, z: 117 }, slot: SignTextSlot::Front,
    });
}

#[test]
fn sign_update_places_the_side_after_four_unprefixed_lines() {
    assert_body("fffffa4000075fd10141026263000364656601", SignUpdate {
        pos: BlockPos { x: -23, y: -47, z: 117 },
        lines: ["A".into(), "bc".into(), "".into(), "def".into()], slot: SignTextSlot::Front,
    });
    assert!(decode_body::<SignUpdate>(&hex("fffffa4000075fd10101410262630003646566")).is_err());
}

#[test]
fn path_counts_are_checked_before_allocation() {
    assert!(decode_body::<MoveEntityPos>(&hex("a302feffffff07")).is_err());
    assert!(decode_body::<PositionPath>(&hex("01ffffffff07")).is_err());
    assert!(decode_body::<PostEffects>(&hex("ffffffff07")).is_err());
}
