//! Movement encoding for protocol 5: the field order, the eye-height reading,
//! and the teleport confirmation.
//!
//! # Why this file exists at all
//!
//! Every assertion here corresponds to a defect that was live in this crate
//! and that no compile, no round-trip and no join replay caught. All three are
//! silent against a real server: it neither errors nor closes the connection,
//! it simply stops accepting movement, so the only visible symptom is a player
//! who cannot move. The measurements that told the right shape from the wrong
//! one are recorded on the packet types in `packets::game`; these tests are the
//! cheap guard that keeps the outcome.
//!
//! Two of the three are field *orders* within a body of same-typed fields, and
//! that is exactly the class `decode(encode(x)) == x` cannot see: both arms
//! agree, and the packet is the right length either way. So the assertions
//! below read the encoded body at byte offsets rather than through the same
//! struct that produced it.

use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, Rotation, Vec3, VersionAdapter,
};
use lodestone_world::World;

/// The eye offset a standing player's stance sits at above its feet.
///
/// Restated here rather than imported so the assertions are not checking the
/// adapter against itself. The server's own acceptance window for
/// `stance - y` is 0.1 to 1.65, which is the outside constraint this value has
/// to satisfy and which the tests below assert directly.
const EYE_HEIGHT: f64 = 1.62;

/// Reads a big-endian `f64` at a byte offset in an encoded body.
fn f64_at(body: &[u8], offset: usize) -> f64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&body[offset..offset + 8]);
    f64::from_be_bytes(bytes)
}

/// Encodes one `Move` and returns the packet id and body.
fn encode_move(pos: Vec3, rotation: Rotation) -> (i32, Vec<u8>) {
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::Move {
                pos,
                rotation,
                on_ground: true,
                horizontal_collision: false,
            },
        )
        .expect("encode a move")
        .expect("a first move always produces a packet")
}

#[test]
fn the_stance_follows_the_feet_rather_than_preceding_them() {
    // Distinct, non-round coordinates so that no two fields can be confused
    // for each other and no swap coincides. In particular y and z differ in
    // sign as well as magnitude.
    let pos = Vec3::new(133.25, 71.5, -48.75);
    let (packet_id, body) = encode_move(pos, Rotation { yaw: 41.5, pitch: -12.25 });

    assert_eq!(
        packet_id,
        lodestone_v1_7::packet_ids::play::serverbound::POSITION_LOOK,
        "a move that both translates and rotates uses the combined packet"
    );
    assert_eq!(
        body.len(),
        8 * 4 + 4 * 2 + 1,
        "four doubles, two floats and the on-ground byte"
    );

    assert_eq!(f64_at(&body, 0), pos.x, "x is first");
    assert_eq!(f64_at(&body, 8), pos.y, "the feet y is second, before the stance");
    assert_eq!(
        f64_at(&body, 16),
        pos.y + EYE_HEIGHT,
        "the stance is third, after the feet"
    );
    assert_eq!(f64_at(&body, 24), pos.z, "z is last of the four");

    // The constraint that makes the order matter rather than being arbitrary:
    // the server range-checks this difference, and the transposed order gives
    // it the negative of this value.
    let stance_above_feet = f64_at(&body, 16) - f64_at(&body, 8);
    assert!(
        stance_above_feet > 0.1 && stance_above_feet < 1.65,
        "the stance must sit inside the window the server accepts, got {stance_above_feet}"
    );
}

#[test]
fn the_position_only_packet_uses_the_same_order() {
    // A move with no rotation change picks the narrower packet, which repeats
    // the same four doubles. A transposition fixed in one and not the other
    // would leave movement working only while the player also turned.
    let pos = Vec3::new(-9.75, 12.125, 300.5);
    let (packet_id, body) = encode_move(pos, Rotation { yaw: 0.0, pitch: 0.0 });

    assert_eq!(
        packet_id,
        lodestone_v1_7::packet_ids::play::serverbound::POSITION,
        "a translation with no rotation uses the position-only packet"
    );
    assert_eq!(body.len(), 8 * 4 + 1, "four doubles and the on-ground byte");
    assert_eq!(f64_at(&body, 0), pos.x, "x is first");
    assert_eq!(f64_at(&body, 8), pos.y, "the feet y is second");
    assert_eq!(f64_at(&body, 16), pos.y + EYE_HEIGHT, "the stance is third");
    assert_eq!(f64_at(&body, 24), pos.z, "z is last");
}

/// Builds a clientbound `position` body: `f64` x, stance, z, `f32` yaw, pitch,
/// and a trailing on-ground boolean.
///
/// Note the middle double is the **stance**, which is the shape this era
/// actually sends; a body built with feet there is a different packet.
fn clientbound_position(x: f64, stance: f64, z: f64, yaw: f32, pitch: f32) -> Vec<u8> {
    let mut body = Vec::with_capacity(33);
    body.extend_from_slice(&x.to_be_bytes());
    body.extend_from_slice(&stance.to_be_bytes());
    body.extend_from_slice(&z.to_be_bytes());
    body.extend_from_slice(&yaw.to_be_bytes());
    body.extend_from_slice(&pitch.to_be_bytes());
    body.push(0);
    body
}

#[test]
fn a_teleport_is_reported_at_the_feet_and_confirmed_immediately() {
    // A server teleporting a player to feet y 80.0 sends 81.62 in this field;
    // that pairing was measured over RCON against a real server, and it is the
    // one input on which the two readings of the field disagree by something a
    // reader will notice.
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    let mut world = World::new();
    let body = clientbound_position(100.0, 81.62, -250.0, 90.0, 15.0);

    let directives = adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            lodestone_v1_7::packet_ids::play::clientbound::POSITION,
            &body,
        )
        .expect("handle a teleport");

    let teleported = directives
        .iter()
        .find_map(|directive| match directive {
            Directive::Emit(ClientEvent::TeleportPlayer { pos, .. }) => Some(*pos),
            _ => None,
        })
        .expect("a teleport is reported to the caller");
    assert!(
        (teleported.y - 80.0).abs() < 1e-9,
        "the reported y is the feet, not the eye position: got {}",
        teleported.y
    );
    assert!((teleported.x - 100.0).abs() < 1e-9, "x passes through");
    assert!((teleported.z + 250.0).abs() < 1e-9, "z passes through");

    // The confirmation. Without it the server holds the player at the pending
    // teleport and discards every move, with no error and no disconnect.
    let (packet_id, confirm) = directives
        .iter()
        .find_map(|directive| match directive {
            Directive::Send { packet_id, payload } => Some((*packet_id, payload.clone())),
            _ => None,
        })
        .expect("the teleport is confirmed with a serverbound packet");
    assert_eq!(
        packet_id,
        lodestone_v1_7::packet_ids::play::serverbound::POSITION_LOOK,
        "the confirmation is a serverbound position_look"
    );
    assert_eq!(
        f64_at(&confirm, 8),
        80.0,
        "the confirmation reports the feet in the feet slot"
    );
    assert_eq!(
        f64_at(&confirm, 16),
        81.62,
        "the confirmation echoes the server's own stance verbatim rather than \
         recomputing it, so no rounding can push it out of range"
    );
}

#[test]
fn the_move_after_a_teleport_is_measured_from_where_the_server_put_the_player() {
    // The state seam: a teleport that did not reseed the movement state leaves
    // the next move reporting the old coordinates, which re-triggers the same
    // hold the confirmation just cleared. Standing still after a teleport must
    // therefore *not* produce a position packet.
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    let mut world = World::new();
    let body = clientbound_position(100.0, 81.62, -250.0, 90.0, 15.0);
    adapter
        .handle_packet(
            &mut world,
            ConnectionState::Play,
            lodestone_v1_7::packet_ids::play::clientbound::POSITION,
            &body,
        )
        .expect("handle a teleport");

    let (packet_id, _) = adapter
        .encode_action(
            ConnectionState::Play,
            &ClientAction::Move {
                pos: Vec3::new(100.0, 80.0, -250.0),
                rotation: Rotation { yaw: 90.0, pitch: 15.0 },
                on_ground: true,
                horizontal_collision: false,
            },
        )
        .expect("encode a move")
        .expect("a move always produces some packet");
    assert_eq!(
        packet_id,
        lodestone_v1_7::packet_ids::play::serverbound::FLYING,
        "standing exactly where the server put us is the status-only packet, \
         not a position claiming to have moved"
    );
}
