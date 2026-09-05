//! Server-side protocol-404 packet translation.
//!
//! The protocol-404 state space is flat but not canonical. This host reverses
//! only the committed per-family state table and rejects a canonical state
//! without one unique wire value.

use std::collections::BTreeMap;

use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Rotation};
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

use crate::PROTOCOL;
use crate::canonical::wire_state_for;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::game::{
    BlockDig, ClientboundPositionLook, JoinGame, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, TeleportConfirm,
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
const PLAINS_BIOME_ID: i32 = 1;

/// Server implementation for protocol 404.
#[derive(Clone, Copy, Debug, Default)]
pub struct V404ServerProtocol;

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-404 packet must encode"),
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

fn wire_state(canonical: u32) -> Result<u32, ChunkEncodeError> {
    wire_state_for(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no unique exact protocol-404 representation"
        ))
    })
}

fn bits_for_palette(len: usize) -> u8 {
    let bits = usize::BITS - (len.saturating_sub(1)).leading_zeros();
    u8::try_from(bits.max(4)).expect("protocol-404 palette width fits in u8")
}

fn pack_indices(values: &[u32], bits: u8) -> Vec<u64> {
    let width = usize::from(bits);
    let mut longs = vec![0_u64; (values.len() * width).div_ceil(64)];
    for (index, &value) in values.iter().enumerate() {
        let bit_index = index * width;
        let long_index = bit_index / 64;
        let offset = bit_index % 64;
        longs[long_index] |= u64::from(value) << offset;
        if offset + width > 64 {
            longs[long_index + 1] |= u64::from(value) >> (64 - offset);
        }
    }
    longs
}

fn encode_section(blob: &mut Writer, states: &[u32]) -> Result<(), ChunkEncodeError> {
    let mut palette = Vec::new();
    let mut indices = Vec::with_capacity(states.len());
    let mut palette_indices = BTreeMap::new();

    for &state in states {
        let wire = wire_state(state)?;
        let next = u32::try_from(palette.len()).expect("section palette cannot exceed u32");
        let index = *palette_indices.entry(wire).or_insert_with(|| {
            palette.push(wire);
            next
        });
        indices.push(index);
    }

    if palette.len() <= 256 {
        let bits = bits_for_palette(palette.len());
        blob.u8(bits);
        blob.var_i32(i32::try_from(palette.len()).expect("section palette fits in i32"));
        for state in palette {
            blob.var_i32(i32::try_from(state).expect("protocol-404 state fits in i32"));
        }
        let longs = pack_indices(&indices, bits);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    } else {
        const GLOBAL_BITS: u8 = 14;
        blob.u8(GLOBAL_BITS);
        blob.var_i32(0);
        let values: Result<Vec<u32>, _> = states.iter().copied().map(wire_state).collect();
        let longs = pack_indices(&values?, GLOBAL_BITS);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    }
    blob.bytes(&[0; LIGHT_BYTES]);
    blob.bytes(&[u8::MAX; LIGHT_BYTES]);
    Ok(())
}

fn encode_chunk_body(cx: i32, cz: i32, column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol 404 column bounds overflow"));
    };
    if column.min_y > LEGACY_MIN_Y || column_end < LEGACY_MIN_Y + LEGACY_HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol 404 requires columns covering y={LEGACY_MIN_Y} through y={}",
            LEGACY_MIN_Y + LEGACY_HEIGHT - 1
        )));
    }

    let air = lodestone_data::block_states::air_state_id();
    let mut bitmask = 0_u32;
    let mut blob = Writer::default();
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
        encode_section(&mut blob, &states)?;
        bitmask |= 1 << section;
    }
    for _ in 0..256 {
        blob.i32(PLAINS_BIOME_ID);
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bool(true);
    packet.var_i32(bitmask as i32);
    packet
        .var_bytes(blob.as_slice())
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    Ok(packet.into_vec())
}

impl V404ServerProtocol {
    /// Converts and encodes one block update without substituting a state.
    pub fn try_encode_block_update(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        let canonical = lodestone_data::block_states::state_id(state)
            .ok_or_else(|| ChunkEncodeError::new(format!("unknown canonical block state {state}")))?;
        let wire = wire_state(canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(wire).expect("protocol-404 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V404ServerProtocol {
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
                    // The four non-breaking actions share 1.13.2's
                    // `block_dig` body. The client adapter already emits
                    // these statuses, and each has a version-free server
                    // consumer; dropping them here made the input keys inert
                    // after successful wire encoding.
                    3 => ServerBound::ItemDropped { whole_stack: true },
                    4 => ServerBound::ItemDropped { whole_stack: false },
                    5 => ServerBound::ReleaseUseItem,
                    6 => ServerBound::SwapItemInHand,
                    _ => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::TELEPORT_CONFIRM => {
                decode_full::<TeleportConfirm>(payload).map_or(ServerBound::Ignored, |confirm| {
                    ServerBound::TeleportationAccepted { id: confirm.teleport_id }
                })
            }
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
                    ServerBound::KeepAlive { id: response.id }
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
                    teleport_id: 0,
                },
            ),
        ]
    }

    fn uses_teleport_acknowledgements(&self) -> bool {
        true
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-404 column")
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
        send(play::clientbound::KEEP_ALIVE, &KeepAliveRequest { id })
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-404 state")
    }
}
