//! Server-side protocol-47 packet translation.
//!
//! The fixed-height chunk body deliberately stays local to this family: protocol
//! 47 writes flat little-endian legacy state words rather than the palette form
//! used by the later hosted legacy family.

use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Rotation, Vec3f};
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

use crate::PROTOCOL;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::entity::Animation;
use crate::packets::game::{
    BlockDig, BlockPlace, ClientboundChat, ClientboundPositionLook, JoinGame, ServerboundChat,
    ServerboundFlying, ServerboundLook, ServerboundPosition, ServerboundPositionLook,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};
use crate::packets::settings::Settings;

const CTX: Ctx = Ctx { version: PROTOCOL };
const COMPRESSION_THRESHOLD: i32 = 256;
const LEGACY_MIN_Y: i32 = 0;
const LEGACY_HEIGHT: i32 = 256;
const SECTION_EDGE: i32 = 16;
const SECTION_BLOCKS: usize = 4096;
const LIGHT_BYTES: usize = 2048;
const PLAINS_BIOME_ID: u8 = 1;
/// Protocol 47's released numeric block registry is the contiguous `0..=197`.
const LAST_PROTOCOL_47_BLOCK_ID: u32 = 197;

/// Server implementation for protocol 47.
#[derive(Clone, Copy, Debug, Default)]
pub struct V47ServerProtocol;

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-47 packet must encode"),
    }
}

fn decode_full<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

/// Wraps server text in the JSON component carried by this era's chat packet.
///
/// Position `1` selects normal chat history below; the component must escape
/// control characters so the text cannot become JSON structure.
fn legacy_text_component(message: &str) -> String {
    let mut json = String::with_capacity(message.len() + 11);
    json.push_str("{\"text\":\"");
    for ch in message.chars() {
        match ch {
            '\"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            ch if ch <= '\u{001f}' => {
                use std::fmt::Write as _;
                write!(json, "\\u{:04x}", ch as u32)
                    .expect("writing into a String cannot fail");
            }
            ch => json.push(ch),
        }
    }
    json.push_str("\"}");
    json
}

fn block_action(status: i32) -> Option<BlockActionKind> {
    match status {
        0 => Some(BlockActionKind::StartDestroy),
        1 => Some(BlockActionKind::AbortDestroy),
        2 => Some(BlockActionKind::StopDestroy),
        _ => None,
    }
}

fn block_face(face: i8) -> Option<BlockFace> {
    match face {
        0 => Some(BlockFace::Down),
        1 => Some(BlockFace::Up),
        2 => Some(BlockFace::North),
        3 => Some(BlockFace::South),
        4 => Some(BlockFace::West),
        5 => Some(BlockFace::East),
        _ => None,
    }
}

/// Converts protocol 47's signed cursor byte to a block-local coordinate.
///
/// The legacy packet carries sixteenths rather than the floats used by later
/// eras. Rejecting an out-of-range byte keeps malformed input from becoming a
/// plausible edge click in the shared placement consumer.
fn cursor_coordinate(value: i8) -> Option<f32> {
    (0..=15).contains(&value).then_some(f32::from(value) / 16.0)
}

fn legacy_composite(canonical: u32) -> Result<u32, ChunkEncodeError> {
    let composite = inverse::resolve(canonical).map_err(|_| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no exact protocol-47 representation"
        ))
    })?;
    if composite >> 4 > LAST_PROTOCOL_47_BLOCK_ID {
        return Err(ChunkEncodeError::new(format!(
            "canonical state {canonical} resolves to block id {}, which protocol 47 does not define",
            composite >> 4
        )));
    }
    Ok(composite)
}

fn encode_chunk_body(cx: i32, cz: i32, column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol 47 column bounds overflow"));
    };
    if column.min_y > LEGACY_MIN_Y || column_end < LEGACY_MIN_Y + LEGACY_HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol 47 requires columns covering y={LEGACY_MIN_Y} through y={}",
            LEGACY_MIN_Y + LEGACY_HEIGHT - 1
        )));
    }

    let air = lodestone_data::block_states::air_state_id();
    let mut bitmask = 0_u16;
    let mut block_data = Vec::new();
    let mut present = Vec::new();
    for section in 0..usize::try_from(LEGACY_HEIGHT / SECTION_EDGE).expect("fixed section count") {
        let y_base = LEGACY_MIN_Y
            + i32::try_from(section).expect("section fits in i32") * SECTION_EDGE;
        let mut states = Vec::with_capacity(SECTION_BLOCKS);
        for y in y_base..y_base + SECTION_EDGE {
            for z in 0..SECTION_EDGE {
                for x in 0..SECTION_EDGE {
                    states.push(column.block_state_id(x, y, z));
                }
            }
        }
        if states.iter().all(|&state| state == air) {
            continue;
        }
        for state in states {
            let legacy = legacy_composite(state)?;
            let word = u16::try_from(legacy)
                .expect("protocol-47 legacy state fits in a 16-bit chunk word");
            block_data.extend_from_slice(&word.to_le_bytes());
        }
        bitmask |= 1 << section;
        present.push(section);
    }

    let mut blob = Writer::default();
    blob.bytes(&block_data);
    for _ in &present {
        blob.bytes(&[0; LIGHT_BYTES]);
    }
    for _ in &present {
        blob.bytes(&[u8::MAX; LIGHT_BYTES]);
    }
    blob.bytes(&[PLAINS_BIOME_ID; 256]);

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bool(true);
    packet.u16(bitmask);
    packet
        .var_bytes(blob.as_slice())
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    Ok(packet.into_vec())
}

impl V47ServerProtocol {
    /// Converts and encodes a block update, preserving a failure when the
    /// canonical state has no exact legacy representation.
    pub fn try_encode_block_update(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        let canonical = lodestone_data::block_states::state_id(state).ok_or_else(|| {
            ChunkEncodeError::new(format!("unknown canonical block state {state}"))
        })?;
        let legacy = legacy_composite(canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(legacy).expect("legacy state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V47ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                if handshake.protocol_version != PROTOCOL {
                    return ServerBound::Ignored;
                }
                let next_state = if handshake.next_state == 2 {
                    State::Login
                } else {
                    State::Status
                };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == login::serverbound::LOGIN_START => {
                decode_full::<LoginStart>(payload).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart {
                        username: start.username,
                        uuid: Uuid::nil(),
                    }
                })
            }
            State::Play if packet_id == play::serverbound::BLOCK_DIG => {
                let Some(BlockDig {
                    status,
                    location: Position(pos),
                    face,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                match status {
                    0..=2 => {
                        let (Some(action), Some(face)) = (block_action(status), block_face(face)) else {
                            return ServerBound::Ignored;
                        };
                        ServerBound::BlockAction {
                            action,
                            pos,
                            face,
                            sequence: 0,
                        }
                    }
                    // Protocol 47 has no off-hand, so its remaining
                    // non-breaking `block_dig` statuses end at release-use.
                    // These packets still carry the position and face fields,
                    // but the actions themselves have no block target.
                    3 => ServerBound::ItemDropped { whole_stack: true },
                    4 => ServerBound::ItemDropped { whole_stack: false },
                    5 => ServerBound::ReleaseUseItem,
                    _ => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::BLOCK_PLACE => {
                let Some(BlockPlace {
                    location: Position(pos),
                    direction,
                    held_item: _,
                    cursor_x,
                    cursor_y,
                    cursor_z,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                let (Some(face), Some(cursor_x), Some(cursor_y), Some(cursor_z)) = (
                    block_face(direction),
                    cursor_coordinate(cursor_x),
                    cursor_coordinate(cursor_y),
                    cursor_coordinate(cursor_z),
                ) else {
                    // The `(-1, -1, -1), -1` in-air sentinel has neither a
                    // hand choice nor the instantaneous look direction the
                    // shared projectile consumer needs. Keep it ignored until
                    // this era can provide that missing input.
                    return ServerBound::Ignored;
                };
                ServerBound::UseItemOn {
                    pos,
                    face,
                    cursor: Vec3f::new(cursor_x, cursor_y, cursor_z),
                    // Protocol 47 predates off-hand and prediction sequences.
                    hand: 0,
                    sequence: 0,
                }
            }
            // Protocol 47's arm-animation request is an empty body. The
            // era has only a main hand, so the shared swing consumer receives
            // hand zero; any byte is a trailing-byte error, not a hand value.
            State::Play if packet_id == play::serverbound::ARM_ANIMATION => {
                if payload.is_empty() {
                    ServerBound::Swing { hand: 0 }
                } else {
                    ServerBound::Ignored
                }
            }
            State::Play if packet_id == play::serverbound::CHAT => {
                decode_full::<ServerboundChat>(payload).map_or(ServerBound::Ignored, |chat| {
                    // The one string carries both text and commands in this
                    // era. The shared command boundary receives no slash.
                    if let Some(command) = chat.message.strip_prefix('/') {
                        ServerBound::ChatCommand {
                            command: command.to_owned(),
                        }
                    } else {
                        // This wire form predates signing, so it can only
                        // represent the shared server's unsigned chat input.
                        ServerBound::Chat {
                            message: chat.message,
                            timestamp_millis: 0,
                            salt: 0,
                            signature: None,
                        }
                    }
                })
            }
            // Protocol 47 confirms a placement by echoing position/look; it
            // deliberately has no separate teleport id or confirmation frame.
            State::Play if packet_id == play::serverbound::POSITION => {
                decode_full::<ServerboundPosition>(payload).map_or(ServerBound::Ignored, |move_| {
                    ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: None,
                        on_ground: move_.on_ground,
                    }
                })
            }
            State::Play if packet_id == play::serverbound::POSITION_LOOK => {
                decode_full::<ServerboundPositionLook>(payload).map_or(ServerBound::Ignored, |move_| {
                    ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: Some(Rotation { yaw: move_.yaw, pitch: move_.pitch }),
                        on_ground: move_.on_ground,
                    }
                })
            }
            State::Play if packet_id == play::serverbound::LOOK => {
                decode_full::<ServerboundLook>(payload).map_or(ServerBound::Ignored, |look| {
                    ServerBound::PlayerRotated {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        on_ground: look.on_ground,
                    }
                })
            }
            State::Play if packet_id == play::serverbound::FLYING => {
                decode_full::<ServerboundFlying>(payload).map_or(ServerBound::Ignored, |flying| {
                    ServerBound::PlayerStatusOnly { on_ground: flying.on_ground }
                })
            }
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                decode_full::<KeepAliveResponse>(payload).map_or(ServerBound::Ignored, |response| {
                    ServerBound::KeepAlive {
                        id: i64::from(response.id),
                    }
                })
            }
            State::Play if packet_id == play::serverbound::SETTINGS => {
                decode_full::<Settings>(payload).map_or(ServerBound::Ignored, |settings| {
                    ServerBound::ClientInformationChanged {
                        view_distance: settings.view_distance,
                    }
                })
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        vec![
            send(
                login::clientbound::COMPRESS,
                &SetCompression {
                    threshold: COMPRESSION_THRESHOLD,
                },
            ),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send(
                login::clientbound::SUCCESS,
                &LoginSuccess {
                    uuid: uuid.to_string(),
                    username: username.to_owned(),
                },
            ),
        ]
    }

    fn has_configuration_phase(&self) -> bool {
        false
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send(
                play::clientbound::LOGIN,
                &JoinGame {
                    entity_id: 1,
                    game_mode: 0,
                    dimension: 0,
                    difficulty: 2,
                    max_players: 20,
                    level_type: "default".to_owned(),
                    reduced_debug_info: false,
                },
            ),
            send(
                play::clientbound::POSITION,
                &ClientboundPositionLook {
                    x: 8.0,
                    y: 100.0,
                    z: 8.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    flags: 0,
                },
            ),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-47 column")
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::MAP_CHUNK,
            payload: encode_chunk_body(cx, cz, column)?,
        })
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        let id = i32::try_from(id)
            .expect("protocol-47 keep-alive id must fit its signed VarInt wire field");
        send(play::clientbound::KEEP_ALIVE, &KeepAliveRequest { id })
    }

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        send(
            play::clientbound::CHAT,
            &ClientboundChat {
                message: legacy_text_component(message),
                // Protocol 47 uses position 1 for ordinary system chat.
                position: 1,
            },
        )
    }

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        send(
            play::clientbound::ANIMATION,
            &Animation {
                entity_id,
                animation: action,
            },
        )
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-47 state")
    }
}
