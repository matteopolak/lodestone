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
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, Rotation, Vec3, VersionAdapter,
};
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

// ---------------------------------------------------------------------------
// The first move after a teleport
// ---------------------------------------------------------------------------

/// A [`WorldSink`] that ignores everything — a teleport is not terrain.
#[derive(Default)]
struct NullSink;

impl lodestone_world::WorldSink for NullSink {
    fn load(&mut self, _pos: lodestone_world::ChunkPos, _chunk: lodestone_world::LoadedChunk) {}
    fn merge(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::ColumnPatch) {}
    fn set_block(&mut self, _x: i32, _y: i32, _z: i32, _state: u32) {}
    fn set_blocks(
        &mut self,
        _section_x: i32,
        _section_y: i32,
        _section_z: i32,
        _blocks: &[(u8, u8, u8, u32)],
    ) {
    }
    fn merge_light(&mut self, _pos: lodestone_world::ChunkPos, _patch: lodestone_world::LightPatch) {
    }
    fn merge_biomes(
        &mut self,
        _pos: lodestone_world::ChunkPos,
        _patch: lodestone_world::BiomePatch,
    ) {
    }
    fn unload(&mut self, _pos: lodestone_world::ChunkPos) {}
    fn set_block_entity(&mut self, _x: i32, _y: i32, _z: i32, _type_id: u32, _nbt: lodestone_core::Nbt) {}
    fn sync_block_entity(
        &mut self,
        _x: i32,
        _y: i32,
        _z: i32,
        _block_entity_type: Option<u32>,
    ) -> lodestone_world::BlockEntitySync {
        lodestone_world::BlockEntitySync::ChunkAbsent
    }
}

/// Hand-built clientbound teleport body: varint id, absolute `x`/`y`/`z`,
/// a zero delta-movement triple, `f32` yaw and pitch, then the `relatives`
/// mask. Built here from `to_be_bytes` rather than from any encoder in the
/// crate under test.
fn player_position_payload(id: u8, x: f64, y: f64, z: f64, relatives: i32) -> Vec<u8> {
    assert!(id < 0x80, "single-byte varint only");
    let mut bytes = vec![id];
    for value in [x, y, z, 0.0, 0.0, 0.0] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(&0.0f32.to_be_bytes());
    bytes.extend_from_slice(&0.0f32.to_be_bytes());
    bytes.extend_from_slice(&relatives.to_be_bytes());
    bytes
}

#[test]
fn player_position_keeps_delta_movement_and_all_relative_bits() {
    let mut bytes = vec![7];
    for value in [11.0f64, 64.0, -9.0, 0.25, 1.5, -0.75] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(&35.0f32.to_be_bytes());
    bytes.extend_from_slice(&(-12.0f32).to_be_bytes());
    let relatives: i32 = (1 << 0) | (1 << 4) | (1 << 5) | (1 << 7) | (1 << 8);
    bytes.extend_from_slice(&relatives.to_be_bytes());

    let directives = V770Adapter::new()
        .handle_packet(
            &mut NullSink,
            ConnectionState::Play,
            play::clientbound::PLAYER_POSITION,
            &bytes,
        )
        .expect("decode player position correction");
    let event = directives
        .iter()
        .find_map(|directive| match directive {
            Directive::Emit(event @ ClientEvent::TeleportPlayer { .. }) => Some(event),
            _ => None,
        })
        .expect("teleport event");
    let ClientEvent::TeleportPlayer {
        flags, velocity, ..
    } = event
    else {
        unreachable!();
    };
    assert!(flags.relative_x);
    assert!(!flags.relative_y);
    assert!(flags.relative_pitch);
    let velocity = velocity.expect("this protocol carries correction velocity");
    assert_eq!(velocity.delta, Vec3::new(0.25, 1.5, -0.75));
    assert!(velocity.relative_x);
    assert!(!velocity.relative_y);
    assert!(velocity.relative_z);
    assert!(velocity.rotate_delta);
}

fn accept_teleport(adapter: &V770Adapter, id: u8, target: Vec3, relatives: i32) {
    adapter
        .handle_packet(
            &mut NullSink,
            ConnectionState::Play,
            play::clientbound::PLAYER_POSITION,
            &player_position_payload(id, target.x, target.y, target.z, relatives),
        )
        .expect("handle teleport");
}

/// The teleport target used below. Deliberately 151 blocks above the baseline
/// pose and one block off in `z`, the shape the survival oracle actually
/// produced — a rewrite that quietly did nothing could not pass for a correct
/// one against a target a hand's breadth away.
const TELEPORT_TARGET: Vec3 = Vec3 {
    x: -44.5,
    y: 220.0,
    z: -376.5,
};

/// **The invariant**: once this adapter has confirmed a teleport, the next
/// movement packet it writes claims that teleport's target, whatever pose the
/// simulation upstream had already built its claim from.
///
/// A vanilla client cannot violate this — it applies the pose, confirms, and
/// sends, on one thread. Ours can: the confirmation is written the instant the
/// packet decodes, while a movement action built from the old pose may already
/// be sitting in the driver's queue, three hops downstream of the simulation
/// and out of reach of everything above. The real server answers such a claim
/// with a corrective teleport (its speed rule, which unlike its
/// positional-disagreement rule does not zero the vertical component), so
/// getting this wrong rubber-bands the player on ordinary teleports.
#[test]
fn the_first_move_after_a_teleport_claims_the_teleport_target_not_the_stale_pose() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    accept_teleport(&adapter, 7, TELEPORT_TARGET, 0);

    // The claim the simulation had already built: still the pre-teleport pose.
    let encoded = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(BASE_POS, BASE_ROT, true, true),
        )
        .expect("encode move")
        .expect("a move this far from the last sent position always sends");

    // `PosRot` is not asserted here — rotation dirtiness is the sibling tests'
    // subject. What matters is the three coordinates and the flags byte.
    let (packet_id, body) = encoded;
    assert_eq!(packet_id, play::serverbound::MOVE_PLAYER_POS);
    let decoded: MovePlayerPos = decode(&body);
    assert_eq!(
        (decoded.x, decoded.y, decoded.z),
        (TELEPORT_TARGET.x, TELEPORT_TARGET.y, TELEPORT_TARGET.z),
        "the first move after a teleport must claim the target, not the pose the simulation \
         still held"
    );
    assert_eq!(
        decoded.flags, 0x00,
        "vanilla's own post-teleport send passes neither on-ground nor horizontal-collision, \
         whatever the caller computed"
    );
}

/// The control for the test above: the same adapter, the same teleport, and a
/// claim that is *already* the target moved by one ordinary tick is left
/// exactly as the caller built it. Without this, a rewrite that fired on every
/// move would pass the test above and destroy all movement.
#[test]
fn a_first_move_that_already_agrees_with_the_teleport_is_left_alone() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    accept_teleport(&adapter, 8, TELEPORT_TARGET, 0);

    // One tick of free fall from the target: 0.0784 blocks, far inside the
    // one-block staleness threshold and far outside the send-dirty epsilon.
    let honest = Vec3 {
        x: TELEPORT_TARGET.x,
        y: TELEPORT_TARGET.y - 0.0784,
        z: TELEPORT_TARGET.z,
    };
    let (packet_id, body) = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(honest, BASE_ROT, false, false),
        )
        .expect("encode move")
        .expect("0.0784 blocks is well past the send-dirty epsilon");
    assert_eq!(packet_id, play::serverbound::MOVE_PLAYER_POS);
    let decoded: MovePlayerPos = decode(&body);
    assert_eq!(
        (decoded.x, decoded.y, decoded.z),
        (honest.x, honest.y, honest.z),
        "a claim that already agrees with the teleport must reach the wire untouched"
    );
}

/// A **relative** teleport authorises no absolute target — this adapter holds
/// no player position to resolve a delta against — so the claim after one is
/// left alone rather than snapped onto whatever absolute target preceded it.
/// That target is where the player *was* before the relative move, so writing
/// it would be worse than doing nothing.
#[test]
fn a_relative_teleport_clears_the_target_and_the_next_move_is_untouched() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    accept_teleport(&adapter, 9, TELEPORT_TARGET, 0);
    // Relative in x, y and z. The mask's exact bit layout is the decoder's
    // business; what matters is that a nonzero positional mask is not absolute.
    accept_teleport(&adapter, 10, Vec3 { x: 1.0, y: 2.0, z: 3.0 }, 0b111);

    let claim = Vec3 {
        x: BASE_POS.x + 5.0,
        y: BASE_POS.y,
        z: BASE_POS.z,
    };
    let (_, body) = adapter
        .encode_action(ConnectionState::Play, &move_action(claim, BASE_ROT, true, false))
        .expect("encode move")
        .expect("five blocks always sends");
    let decoded: MovePlayerPos = decode(&body);
    assert_eq!(
        (decoded.x, decoded.y, decoded.z),
        (claim.x, claim.y, claim.z),
        "a relative teleport must not leave a superseded absolute target behind for the next \
         claim to be snapped onto"
    );
}

/// The **discriminating** control for the rewrite above, and the one the two
/// tests either side of it cannot supply: a first post-teleport claim that is
/// neither the teleport target nor the pose this adapter last sent, but a
/// third location entirely.
///
/// That shape is what a caller driving the client library directly produces —
/// `ClientHandle::move_to`/`set_position`/`walk_to` run no physics and place
/// the player wherever the caller asks, so a headless caller's first move
/// after any absolute teleport (a join placement, a respawn, a server-issued
/// teleport) is routinely many blocks from that teleport's target while having
/// nothing to do with the pre-teleport pose. It is not stale: nothing
/// overtook it, because it was built after the teleport landed.
///
/// The staleness the rewrite exists to catch has a sharper signature than
/// "far from the target": a claim overtaken by a teleport still carries the
/// pose this adapter last put on the wire, because that is what the producer
/// upstream was still holding. A claim that has moved on from that pose was
/// built by something that had already seen the teleport, and lying about it
/// hides a move the server is entitled to see and to judge for itself.
///
/// Without this arm, a rewrite keyed on distance alone silently swallows the
/// first movement of every headless session — the client reports the spawn it
/// was placed at, the server agrees the player never moved, and every
/// consequence of moving (view streaming, chunk unload, a knockback direction
/// measured from the attacker's position) is computed at the wrong place with
/// no error anywhere.
#[test]
fn a_first_move_to_a_third_location_is_the_callers_own_and_reaches_the_wire() {
    let adapter = V770Adapter::new();
    establish_baseline(&adapter);

    accept_teleport(&adapter, 11, TELEPORT_TARGET, 0);

    // 160 blocks along x from the target: far outside the staleness threshold,
    // and 160-ish blocks from `BASE_POS` too, so neither of the two poses the
    // adapter knows about can be mistaken for this one.
    let deliberate = Vec3 {
        x: TELEPORT_TARGET.x + 160.0,
        y: TELEPORT_TARGET.y,
        z: TELEPORT_TARGET.z,
    };
    let (packet_id, body) = adapter
        .encode_action(
            ConnectionState::Play,
            &move_action(deliberate, BASE_ROT, true, false),
        )
        .expect("encode move")
        .expect("160 blocks always sends");
    assert_eq!(packet_id, play::serverbound::MOVE_PLAYER_POS);
    let decoded: MovePlayerPos = decode(&body);
    assert_eq!(
        (decoded.x, decoded.y, decoded.z),
        (deliberate.x, deliberate.y, deliberate.z),
        "a first post-teleport claim that has moved on from the last sent pose was built \
         after the teleport, not before it, and must reach the wire as the caller built it"
    );
}
