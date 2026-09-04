//! Server-side protocol-47 packet translation.
//!
//! The fixed-height chunk body deliberately stays local to this family: protocol
//! 47 writes flat little-endian legacy state words rather than the palette form
//! used by the later hosted legacy family.

use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

use crate::PROTOCOL;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::game::{BlockDig, ClientboundPositionLook, JoinGame};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};

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

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-47 state")
    }
}
