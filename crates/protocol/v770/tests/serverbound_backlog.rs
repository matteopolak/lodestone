//! Hermetic byte-exact tests for the remaining serverbound backlog encoders:
//! `player_loaded`, `seen_advancements`, `command_suggestion`, `paddle_boat`,
//! and `move_vehicle`.
//!
//! Expected payloads are built from the wire specification with an
//! independent VarInt encoder (never the adapter's own codec), so a
//! symmetric bug cannot pass. Layouts are verified against 26.2's
//! `ServerboundPlayerLoadedPacket` (`StreamCodec.unit`, empty body),
//! `ServerboundSeenAdvancementsPacket` (VarInt `Action` ordinal, plus a
//! conditional identifier only for `OPENED_TAB`), `ServerboundCommandSuggestionPacket`
//! (VarInt id + UTF-8 string), `ServerboundPaddleBoatPacket` (two plain
//! booleans) and `ServerboundMoveVehiclePacket` (`Vec3` + yaw/pitch + a
//! single trailing boolean, no horizontal-collision bit unlike player
//! movement).
//!
//! None of these five actions currently have a live call site elsewhere in
//! the workspace (no vehicle-riding controller, no boat-paddle input
//! handling, no advancement-tab UI, no tab-completion feature) except
//! `player_loaded`, which closes a documented gap: several tests under
//! `lodestone-client` note the client cannot yet short-circuit the server's
//! ~60-tick post-join movement-validation window because the action didn't
//! exist. These tests exist so a future caller (`ClientHandle::send_action`)
//! has byte-exact protocol coverage before it's wired to anything.

use lodestone_model::{ClientAction, ConnectionState, ResourceKey, Rotation, Vec3, VersionAdapter};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

#[test]
fn player_loaded_is_an_empty_body() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(ConnectionState::Play, &ClientAction::PlayerLoaded)
        .expect("encode player loaded");
    assert_eq!(
        encoded,
        Some((play::serverbound::PLAYER_LOADED, Vec::new()))
    );
}

#[test]
fn player_loaded_is_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(ConnectionState::Configuration, &ClientAction::PlayerLoaded)
            .expect("encode"),
        None,
        "player_loaded is a play-state action only"
    );
}

#[test]
fn seen_advancements_opened_tab_carries_the_identifier() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SeenAdvancements {
                tab: Some(ResourceKey::new("minecraft", "story/root").expect("valid identifier")),
            },
        )
        .expect("encode seen advancements");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(0)); // Action.OPENED_TAB
    let id = "minecraft:story/root";
    want.extend_from_slice(&varint(id.len() as i32));
    want.extend_from_slice(id.as_bytes());
    assert_eq!(encoded, Some((play::serverbound::SEEN_ADVANCEMENTS, want)));
}

#[test]
fn seen_advancements_closed_screen_has_no_identifier() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::SeenAdvancements { tab: None },
        )
        .expect("encode seen advancements");
    assert_eq!(
        encoded,
        Some((
            play::serverbound::SEEN_ADVANCEMENTS,
            varint(1) // Action.CLOSED_SCREEN, nothing follows
        ))
    );
}

#[test]
fn command_suggestion_is_id_then_utf8_string() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::CommandSuggestion {
                id: 7,
                command: "/give @s diamond".to_owned(),
            },
        )
        .expect("encode command suggestion");
    let mut want = Vec::new();
    want.extend_from_slice(&varint(7));
    let cmd = "/give @s diamond";
    want.extend_from_slice(&varint(cmd.len() as i32));
    want.extend_from_slice(cmd.as_bytes());
    assert_eq!(encoded, Some((play::serverbound::COMMAND_SUGGESTION, want)));
}

#[test]
fn paddle_boat_is_two_plain_booleans() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::PaddleBoat {
                left: true,
                right: false,
            },
        )
        .expect("encode paddle boat");
    assert_eq!(
        encoded,
        Some((play::serverbound::PADDLE_BOAT, vec![1, 0]))
    );
}

#[test]
fn move_vehicle_is_byte_exact() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::MoveVehicle {
                pos: Vec3 {
                    x: 10.0,
                    y: 65.5,
                    z: -3.25,
                },
                rotation: Rotation {
                    yaw: 180.0,
                    pitch: 5.0,
                },
                on_ground: true,
            },
        )
        .expect("encode move vehicle");
    let mut want = Vec::new();
    want.extend_from_slice(&10.0_f64.to_be_bytes());
    want.extend_from_slice(&65.5_f64.to_be_bytes());
    want.extend_from_slice(&(-3.25_f64).to_be_bytes());
    want.extend_from_slice(&180.0_f32.to_be_bytes());
    want.extend_from_slice(&5.0_f32.to_be_bytes());
    want.push(1); // onGround: true, no horizontal-collision bit on this packet
    assert_eq!(encoded, Some((play::serverbound::MOVE_VEHICLE, want)));
}

#[test]
fn move_vehicle_on_ground_false_is_a_single_zero_byte() {
    let adapter = V770Adapter::new();
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::MoveVehicle {
                pos: Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                rotation: Rotation {
                    yaw: 0.0,
                    pitch: 0.0,
                },
                on_ground: false,
            },
        )
        .expect("encode move vehicle");
    let (_, bytes) = encoded.expect("some");
    assert_eq!(bytes.last(), Some(&0u8));
}
