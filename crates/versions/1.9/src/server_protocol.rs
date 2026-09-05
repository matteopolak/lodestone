//! Server-side protocol-340 packet translation.
//!
//! This module supplies the earliest hosted wire family. It keeps the shared
//! server's canonical block-state storage and converts each state only at the
//! packet boundary. A state or column shape this wire cannot represent is an
//! encoding error; emitting a different block would make the client and server
//! disagree about the world.

use std::collections::BTreeMap;

use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Rotation, Vec3f};
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, HOTBAR_SIZE, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

use crate::PROTOCOL;
use crate::adapter::{PROTOCOL_1_9_4, PROTOCOL_1_10_2, PROTOCOL_1_11_2};
use crate::packet_ids::{handshaking, login, play};
use crate::packets::common::{
    KeepAliveRequest, KeepAliveRequestVarInt, KeepAliveResponse, KeepAliveResponseVarInt,
};
use crate::packets::game::{
    Animation, BlockDig, BlockPlace, BlockPlaceByteCursor, ClientboundChat,
    ClientboundPositionLook, JoinGame, ServerboundChat, ServerboundFlying, ServerboundLook,
    ServerboundPosition,
    ServerboundArmAnimation, ServerboundPositionLook, TeleportConfirm,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};
use crate::packets::settings::Settings;
use crate::packets::window::ServerboundHeldItemSlot;

const CTX_340: Ctx = Ctx { version: PROTOCOL };
const CTX_110: Ctx = Ctx {
    version: PROTOCOL_1_9_4,
};
const CTX_210: Ctx = Ctx {
    version: PROTOCOL_1_10_2,
};
const CTX_316: Ctx = Ctx {
    version: PROTOCOL_1_11_2,
};
const COMPRESSION_THRESHOLD: i32 = 256;
const LEGACY_MIN_Y: i32 = 0;
const LEGACY_HEIGHT: i32 = 256;
const SECTION_EDGE: i32 = 16;
const SECTION_BLOCKS: usize = 4096;
const LIGHT_BYTES: usize = 2048;
const PLAINS_BIOME_ID: u8 = 1;

/// Server implementation for protocol 340.
#[derive(Clone, Copy, Debug, Default)]
pub struct V340ServerProtocol;

/// Server implementation for protocol 110.
#[derive(Clone, Copy, Debug, Default)]
pub struct V110ServerProtocol;

/// Server implementation for protocol 210.
#[derive(Clone, Copy, Debug, Default)]
pub struct V210ServerProtocol;

/// Server implementation for protocol 316.
#[derive(Clone, Copy, Debug, Default)]
pub struct V316ServerProtocol;

#[derive(Clone, Copy)]
struct ServerPacketIds {
    handshake: i32,
    login_start: i32,
    compression: i32,
    login_success: i32,
    block_dig: i32,
    block_place: i32,
    arm_animation: i32,
    held_item_slot: i32,
    chat_serverbound: i32,
    teleport_confirm: i32,
    flying: i32,
    position_serverbound: i32,
    position_look: i32,
    look: i32,
    settings: i32,
    keep_alive_clientbound: i32,
    keep_alive_serverbound: i32,
    join: i32,
    position: i32,
    map_chunk: i32,
    block_change: i32,
    chat_clientbound: i32,
    animation_clientbound: i32,
}

const IDS_340: ServerPacketIds = ServerPacketIds {
    handshake: handshaking::serverbound::SET_PROTOCOL,
    login_start: login::serverbound::LOGIN_START,
    compression: login::clientbound::COMPRESS,
    login_success: login::clientbound::SUCCESS,
    block_dig: play::serverbound::BLOCK_DIG,
    block_place: play::serverbound::BLOCK_PLACE,
    arm_animation: play::serverbound::ARM_ANIMATION,
    held_item_slot: play::serverbound::HELD_ITEM_SLOT,
    chat_serverbound: play::serverbound::CHAT,
    teleport_confirm: play::serverbound::TELEPORT_CONFIRM,
    flying: play::serverbound::FLYING,
    position_serverbound: play::serverbound::POSITION,
    position_look: play::serverbound::POSITION_LOOK,
    look: play::serverbound::LOOK,
    settings: play::serverbound::SETTINGS,
    keep_alive_clientbound: play::clientbound::KEEP_ALIVE,
    keep_alive_serverbound: play::serverbound::KEEP_ALIVE,
    join: play::clientbound::LOGIN,
    position: play::clientbound::POSITION,
    map_chunk: play::clientbound::MAP_CHUNK,
    block_change: play::clientbound::BLOCK_CHANGE,
    chat_clientbound: play::clientbound::CHAT,
    animation_clientbound: play::clientbound::ANIMATION,
};

const IDS_316: ServerPacketIds = ServerPacketIds {
    handshake: crate::packet_ids_316::handshaking::serverbound::SET_PROTOCOL,
    login_start: crate::packet_ids_316::login::serverbound::LOGIN_START,
    compression: crate::packet_ids_316::login::clientbound::COMPRESS,
    login_success: crate::packet_ids_316::login::clientbound::SUCCESS,
    block_dig: crate::packet_ids_316::play::serverbound::BLOCK_DIG,
    block_place: crate::packet_ids_316::play::serverbound::BLOCK_PLACE,
    arm_animation: crate::packet_ids_316::play::serverbound::ARM_ANIMATION,
    held_item_slot: crate::packet_ids_316::play::serverbound::HELD_ITEM_SLOT,
    chat_serverbound: crate::packet_ids_316::play::serverbound::CHAT,
    teleport_confirm: crate::packet_ids_316::play::serverbound::TELEPORT_CONFIRM,
    flying: crate::packet_ids_316::play::serverbound::FLYING,
    position_serverbound: crate::packet_ids_316::play::serverbound::POSITION,
    position_look: crate::packet_ids_316::play::serverbound::POSITION_LOOK,
    look: crate::packet_ids_316::play::serverbound::LOOK,
    settings: crate::packet_ids_316::play::serverbound::SETTINGS,
    keep_alive_clientbound: crate::packet_ids_316::play::clientbound::KEEP_ALIVE,
    keep_alive_serverbound: crate::packet_ids_316::play::serverbound::KEEP_ALIVE,
    join: crate::packet_ids_316::play::clientbound::LOGIN,
    position: crate::packet_ids_316::play::clientbound::POSITION,
    map_chunk: crate::packet_ids_316::play::clientbound::MAP_CHUNK,
    block_change: crate::packet_ids_316::play::clientbound::BLOCK_CHANGE,
    chat_clientbound: crate::packet_ids_316::play::clientbound::CHAT,
    animation_clientbound: crate::packet_ids_316::play::clientbound::ANIMATION,
};

const IDS_210: ServerPacketIds = ServerPacketIds {
    handshake: crate::packet_ids_210::handshaking::serverbound::SET_PROTOCOL,
    login_start: crate::packet_ids_210::login::serverbound::LOGIN_START,
    compression: crate::packet_ids_210::login::clientbound::COMPRESS,
    login_success: crate::packet_ids_210::login::clientbound::SUCCESS,
    block_dig: crate::packet_ids_210::play::serverbound::BLOCK_DIG,
    block_place: crate::packet_ids_210::play::serverbound::BLOCK_PLACE,
    arm_animation: crate::packet_ids_210::play::serverbound::ARM_ANIMATION,
    held_item_slot: crate::packet_ids_210::play::serverbound::HELD_ITEM_SLOT,
    chat_serverbound: crate::packet_ids_210::play::serverbound::CHAT,
    teleport_confirm: crate::packet_ids_210::play::serverbound::TELEPORT_CONFIRM,
    flying: crate::packet_ids_210::play::serverbound::FLYING,
    position_serverbound: crate::packet_ids_210::play::serverbound::POSITION,
    position_look: crate::packet_ids_210::play::serverbound::POSITION_LOOK,
    look: crate::packet_ids_210::play::serverbound::LOOK,
    settings: crate::packet_ids_210::play::serverbound::SETTINGS,
    keep_alive_clientbound: crate::packet_ids_210::play::clientbound::KEEP_ALIVE,
    keep_alive_serverbound: crate::packet_ids_210::play::serverbound::KEEP_ALIVE,
    join: crate::packet_ids_210::play::clientbound::LOGIN,
    position: crate::packet_ids_210::play::clientbound::POSITION,
    map_chunk: crate::packet_ids_210::play::clientbound::MAP_CHUNK,
    block_change: crate::packet_ids_210::play::clientbound::BLOCK_CHANGE,
    chat_clientbound: crate::packet_ids_210::play::clientbound::CHAT,
    animation_clientbound: crate::packet_ids_210::play::clientbound::ANIMATION,
};

const IDS_110: ServerPacketIds = ServerPacketIds {
    handshake: crate::packet_ids_110::handshaking::serverbound::SET_PROTOCOL,
    login_start: crate::packet_ids_110::login::serverbound::LOGIN_START,
    compression: crate::packet_ids_110::login::clientbound::COMPRESS,
    login_success: crate::packet_ids_110::login::clientbound::SUCCESS,
    block_dig: crate::packet_ids_110::play::serverbound::BLOCK_DIG,
    block_place: crate::packet_ids_110::play::serverbound::BLOCK_PLACE,
    arm_animation: crate::packet_ids_110::play::serverbound::ARM_ANIMATION,
    held_item_slot: crate::packet_ids_110::play::serverbound::HELD_ITEM_SLOT,
    chat_serverbound: crate::packet_ids_110::play::serverbound::CHAT,
    teleport_confirm: crate::packet_ids_110::play::serverbound::TELEPORT_CONFIRM,
    flying: crate::packet_ids_110::play::serverbound::FLYING,
    position_serverbound: crate::packet_ids_110::play::serverbound::POSITION,
    position_look: crate::packet_ids_110::play::serverbound::POSITION_LOOK,
    look: crate::packet_ids_110::play::serverbound::LOOK,
    settings: crate::packet_ids_110::play::serverbound::SETTINGS,
    keep_alive_clientbound: crate::packet_ids_110::play::clientbound::KEEP_ALIVE,
    keep_alive_serverbound: crate::packet_ids_110::play::serverbound::KEEP_ALIVE,
    join: crate::packet_ids_110::play::clientbound::LOGIN,
    position: crate::packet_ids_110::play::clientbound::POSITION,
    map_chunk: crate::packet_ids_110::play::clientbound::MAP_CHUNK,
    block_change: crate::packet_ids_110::play::clientbound::BLOCK_CHANGE,
    chat_clientbound: crate::packet_ids_110::play::clientbound::CHAT,
    animation_clientbound: crate::packet_ids_110::play::clientbound::ANIMATION,
};

fn send<T: Encode>(packet_id: i32, packet: &T, ctx: Ctx, protocol: i32) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, ctx)
            .unwrap_or_else(|_| panic!("fixed protocol-{protocol} packet must encode")),
    }
}

fn decode_full<T: Decode>(payload: &[u8], ctx: Ctx) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, ctx).ok()?;
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

fn use_item_on(
    location: Position,
    direction: i32,
    hand: i32,
    cursor: Vec3f,
) -> ServerBound {
    let Some(hand) = u8::try_from(hand).ok().filter(|hand| *hand <= 1) else {
        return ServerBound::Ignored;
    };
    if !cursor.x.is_finite()
        || !cursor.y.is_finite()
        || !cursor.z.is_finite()
        || !(0.0..=1.0).contains(&cursor.x)
        || !(0.0..=1.0).contains(&cursor.y)
        || !(0.0..=1.0).contains(&cursor.z)
    {
        return ServerBound::Ignored;
    }

    if BlockPos::from(location) == BlockPos::new(-1, -1, -1) && direction == -1 {
        // This era multiplexes use-in-air into `block_place`. The packet has
        // no rotation, so retain its explicit absence as zeroes rather than
        // borrowing state from an earlier movement packet.
        return ServerBound::UseItem {
            hand,
            yaw: 0.0,
            pitch: 0.0,
        };
    }

    let Some(face) = i8::try_from(direction).ok().and_then(block_face) else {
        return ServerBound::Ignored;
    };

    ServerBound::UseItemOn {
        pos: BlockPos::from(location),
        face,
        cursor,
        sequence: 0,
        hand,
    }
}

fn byte_cursor(value: i8) -> Option<f32> {
    (0..=15).contains(&value).then(|| f32::from(value) / 16.0)
}

fn uses_varint_keep_alive(protocol: i32) -> bool {
    matches!(
        protocol,
        PROTOCOL_1_9_4 | PROTOCOL_1_10_2 | PROTOCOL_1_11_2
    )
}

fn bits_for_palette(len: usize) -> u8 {
    let bits = usize::BITS - (len.saturating_sub(1)).leading_zeros();
    u8::try_from(bits.max(4)).expect("protocol-340 palette width fits in u8")
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

fn legacy_state(protocol: i32, state: u32) -> Result<u32, ChunkEncodeError> {
    let legacy = inverse::resolve(state).map_err(|_| {
        ChunkEncodeError::new(format!(
            "canonical state {state} has no exact protocol-{protocol} representation"
        ))
    })?;
    let block_id = legacy >> 4;
    let supported = match protocol {
        PROTOCOL_1_9_4 => block_id <= 212 || block_id == 255,
        PROTOCOL_1_10_2 => block_id <= 212 || block_id == 255,
        PROTOCOL_1_11_2 => block_id <= 234 || block_id == 255,
        _ => true,
    };
    if !supported {
        return Err(ChunkEncodeError::new(format!(
            "canonical state {state} has no exact protocol-{protocol} representation"
        )));
    }
    Ok(legacy)
}

fn encode_section(
    protocol: i32,
    blob: &mut Writer,
    states: &[u32],
) -> Result<(), ChunkEncodeError> {
    let mut palette = Vec::new();
    let mut indices = Vec::with_capacity(states.len());
    let mut palette_indices = BTreeMap::new();

    for &state in states {
        let legacy = legacy_state(protocol, state)?;
        let next = u32::try_from(palette.len()).expect("section palette cannot exceed u32");
        let index = *palette_indices.entry(legacy).or_insert_with(|| {
            palette.push(legacy);
            next
        });
        indices.push(index);
    }

    if palette.len() <= 256 {
        let bits = bits_for_palette(palette.len());
        blob.u8(bits);
        blob.var_i32(i32::try_from(palette.len()).expect("section palette fits in i32"));
        for state in palette {
            blob.var_i32(i32::try_from(state).expect("legacy state fits in i32"));
        }
        let longs = pack_indices(&indices, bits);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    } else {
        const GLOBAL_BITS: u8 = 13;
        blob.u8(GLOBAL_BITS);
        blob.var_i32(0);
        let values: Vec<u32> = states
            .iter()
            .map(|&state| {
                legacy_state(protocol, state)
                    .expect("states were validated while building palette")
            })
            .collect();
        let longs = pack_indices(&values, GLOBAL_BITS);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    }
    blob.bytes(&[0; LIGHT_BYTES]);
    blob.bytes(&[u8::MAX; LIGHT_BYTES]);
    Ok(())
}

fn encode_chunk_body(
    protocol: i32,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new(format!(
            "protocol {protocol} column bounds overflow"
        )));
    };
    if column.min_y > LEGACY_MIN_Y || column_end < LEGACY_MIN_Y + LEGACY_HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol {protocol} requires columns covering y={LEGACY_MIN_Y} through y={}",
            LEGACY_MIN_Y + LEGACY_HEIGHT - 1
        )));
    }

    let air = lodestone_data::block_states::air_state_id();
    let mut bitmask = 0_u32;
    let mut blob = Writer::default();
    for section in 0..usize::try_from(LEGACY_HEIGHT / SECTION_EDGE).expect("fixed section count") {
        let y_base = LEGACY_MIN_Y + i32::try_from(section).expect("section fits in i32") * SECTION_EDGE;
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
        encode_section(protocol, &mut blob, &states)?;
        bitmask |= 1 << section;
    }
    blob.bytes(&[PLAINS_BIOME_ID; 256]);

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

impl V340ServerProtocol {
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
        let legacy = legacy_state(PROTOCOL, canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(legacy).expect("legacy state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: IDS_340.block_change,
            payload: payload.into_vec(),
        })
    }
}

impl V110ServerProtocol {
    /// Converts and encodes a block update, preserving a failure when the
    /// canonical state has no exact protocol-110 representation.
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
        let legacy = legacy_state(PROTOCOL_1_9_4, canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(legacy).expect("legacy state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: IDS_110.block_change,
            payload: payload.into_vec(),
        })
    }
}

impl V210ServerProtocol {
    /// Converts and encodes a block update, preserving a failure when the
    /// canonical state has no exact protocol-210 representation.
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
        let legacy = legacy_state(PROTOCOL_1_10_2, canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(legacy).expect("legacy state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: IDS_210.block_change,
            payload: payload.into_vec(),
        })
    }
}

impl V316ServerProtocol {
    /// Converts and encodes a block update, preserving a failure when the
    /// canonical state has no exact protocol-316 representation.
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
        let legacy = legacy_state(PROTOCOL_1_11_2, canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(legacy).expect("legacy state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: IDS_316.block_change,
            payload: payload.into_vec(),
        })
    }
}

fn decode_packet(
    protocol: i32,
    ids: ServerPacketIds,
    ctx: Ctx,
    state: State,
    packet_id: i32,
    payload: &[u8],
) -> ServerBound {
        match state {
            State::Handshaking if packet_id == ids.handshake => {
                let Some(handshake) = decode_full::<SetProtocol>(payload, ctx) else {
                    return ServerBound::Ignored;
                };
                if handshake.protocol_version != protocol {
                    return ServerBound::Ignored;
                }
                let next_state = if handshake.next_state == 2 {
                    State::Login
                } else {
                    State::Status
                };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == ids.login_start => {
                decode_full::<LoginStart>(payload, ctx).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart {
                        username: start.username,
                        uuid: Uuid::nil(),
                    }
                })
            }
            State::Play if packet_id == ids.block_dig => {
                let Some(BlockDig {
                    status,
                    location: Position(pos),
                    face,
                }) = decode_full(payload, ctx)
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
            State::Play if packet_id == ids.block_place => {
                if protocol >= PROTOCOL_1_11_2 {
                    let Some(BlockPlace {
                        location,
                        direction,
                        hand,
                        cursor_x,
                        cursor_y,
                        cursor_z,
                    }) = decode_full(payload, ctx)
                    else {
                        return ServerBound::Ignored;
                    };
                    use_item_on(
                        location,
                        direction,
                        hand,
                        Vec3f::new(cursor_x, cursor_y, cursor_z),
                    )
                } else {
                    let Some(BlockPlaceByteCursor {
                        location,
                        direction,
                        hand,
                        cursor_x,
                        cursor_y,
                        cursor_z,
                    }) = decode_full(payload, ctx)
                    else {
                        return ServerBound::Ignored;
                    };
                    let (Some(cursor_x), Some(cursor_y), Some(cursor_z)) = (
                        byte_cursor(cursor_x),
                        byte_cursor(cursor_y),
                        byte_cursor(cursor_z),
                    ) else {
                        return ServerBound::Ignored;
                    };
                    use_item_on(
                        location,
                        direction,
                        hand,
                        Vec3f::new(cursor_x, cursor_y, cursor_z),
                    )
                }
            }
            State::Play if packet_id == ids.arm_animation => {
                let Some(hand) = decode_full::<ServerboundArmAnimation>(payload, ctx)
                    .and_then(|packet| u8::try_from(packet.hand).ok())
                    .filter(|&hand| hand <= 1)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::Swing { hand }
            }
            State::Play if packet_id == ids.held_item_slot => {
                let Some(slot) = decode_full::<ServerboundHeldItemSlot>(payload, ctx)
                    .and_then(|packet| u8::try_from(packet.slot).ok())
                    .filter(|&slot| slot < HOTBAR_SIZE)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::CarriedItemChanged { slot }
            }
            State::Play if packet_id == ids.chat_serverbound => {
                decode_full::<ServerboundChat>(payload, ctx).map_or(ServerBound::Ignored, |chat| {
                    // This era predates signed chat. Its one-string body has no
                    // timestamp, salt, or signature, so the shared server's
                    // permissive legacy-chat path receives their explicit
                    // unsigned values rather than manufacturing a signature.
                    ServerBound::Chat {
                        message: chat.message,
                        timestamp_millis: 0,
                        salt: 0,
                        signature: None,
                    }
                })
            }
            State::Play if packet_id == ids.teleport_confirm => {
                decode_full::<TeleportConfirm>(payload, ctx).map_or(ServerBound::Ignored, |confirm| {
                    ServerBound::TeleportationAccepted { id: confirm.teleport_id }
                })
            }
            State::Play if packet_id == ids.position_serverbound => {
                decode_full::<ServerboundPosition>(payload, ctx).map_or(ServerBound::Ignored, |move_| {
                    ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: None,
                        on_ground: move_.on_ground,
                    }
                })
            }
            State::Play if packet_id == ids.position_look => {
                decode_full::<ServerboundPositionLook>(payload, ctx).map_or(ServerBound::Ignored, |move_| {
                    ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: Some(Rotation { yaw: move_.yaw, pitch: move_.pitch }),
                        on_ground: move_.on_ground,
                    }
                })
            }
            State::Play if packet_id == ids.look => {
                decode_full::<ServerboundLook>(payload, ctx).map_or(ServerBound::Ignored, |look| {
                    ServerBound::PlayerRotated {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        on_ground: look.on_ground,
                    }
                })
            }
            State::Play if packet_id == ids.flying => {
                decode_full::<ServerboundFlying>(payload, ctx).map_or(ServerBound::Ignored, |flying| {
                    ServerBound::PlayerStatusOnly { on_ground: flying.on_ground }
                })
            }
            State::Play if packet_id == ids.settings => {
                decode_full::<Settings>(payload, ctx).map_or(ServerBound::Ignored, |settings| {
                    ServerBound::ClientInformationChanged {
                        view_distance: settings.view_distance,
                    }
                })
            }
            State::Play if packet_id == ids.keep_alive_serverbound => {
                let id = if uses_varint_keep_alive(protocol) {
                    let Some(response) = decode_full::<KeepAliveResponseVarInt>(payload, ctx) else {
                        return ServerBound::Ignored;
                    };
                    i64::from(response.id)
                } else {
                    let Some(response) = decode_full::<KeepAliveResponse>(payload, ctx) else {
                        return ServerBound::Ignored;
                    };
                    response.id
                };
                ServerBound::KeepAlive { id }
            }
            _ => ServerBound::Ignored,
        }
}

fn login_success(
    ids: ServerPacketIds,
    ctx: Ctx,
    protocol: i32,
    username: &str,
    uuid: Uuid,
) -> Vec<ServerDirective> {
    vec![
        send(
            ids.compression,
            &SetCompression {
                threshold: COMPRESSION_THRESHOLD,
            },
            ctx,
            protocol,
        ),
        ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
        send(
            ids.login_success,
            &LoginSuccess {
                uuid: uuid.to_string(),
                username: username.to_owned(),
            },
            ctx,
            protocol,
        ),
    ]
}

fn begin_play(ids: ServerPacketIds, ctx: Ctx, protocol: i32) -> Vec<ServerDirective> {
    vec![
        send(
            ids.join,
            &JoinGame {
                entity_id: 1,
                game_mode: 0,
                dimension: 0,
                difficulty: 2,
                max_players: 20,
                level_type: "default".to_owned(),
                reduced_debug_info: false,
            },
            ctx,
            protocol,
        ),
        send(
            ids.position,
            &ClientboundPositionLook {
                x: 8.0,
                y: 100.0,
                z: 8.0,
                yaw: 0.0,
                pitch: 0.0,
                flags: 0,
                teleport_id: 0,
            },
            ctx,
            protocol,
        ),
    ]
}

fn try_encode_chunk(
    protocol: i32,
    ids: ServerPacketIds,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<ServerDirective, ChunkEncodeError> {
    Ok(ServerDirective::Send {
        packet_id: ids.map_chunk,
        payload: encode_chunk_body(protocol, cx, cz, column)?,
    })
}

fn encode_keep_alive(
    protocol: i32,
    ids: ServerPacketIds,
    ctx: Ctx,
    id: i64,
) -> ServerDirective {
    if uses_varint_keep_alive(protocol) {
        let id = i32::try_from(id)
            .expect("protocol-110/210/316 keep-alive id must fit its signed VarInt wire field");
        send(
            ids.keep_alive_clientbound,
            &KeepAliveRequestVarInt { id },
            ctx,
            protocol,
        )
    } else {
        send(
            ids.keep_alive_clientbound,
            &KeepAliveRequest { id },
            ctx,
            protocol,
        )
    }
}

/// Serializes a plain server message as the legacy JSON text-component form.
///
/// The 1.9-era `chat` packet carries JSON rather than the later network NBT
/// component. Only a literal component is needed here: chat decoration has
/// already happened in the shared server before it calls `encode_system_chat`.
fn legacy_text_component(message: &str) -> String {
    let mut json = String::with_capacity(message.len() + 11);
    json.push_str("{\"text\":\"");
    for ch in message.chars() {
        match ch {
            '"' => json.push_str("\\\""),
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

fn encode_system_chat(
    protocol: i32,
    ids: ServerPacketIds,
    ctx: Ctx,
    message: &str,
) -> ServerDirective {
    send(
        ids.chat_clientbound,
        &ClientboundChat {
            message: legacy_text_component(message),
            // The pre-1.13 `chat` packet uses position 1 for ordinary system
            // chat. The shared server has already excluded action-bar output.
            position: 1,
        },
        ctx,
        protocol,
    )
}

fn encode_animate(
    protocol: i32,
    ids: ServerPacketIds,
    ctx: Ctx,
    entity_id: i32,
    action: u8,
) -> ServerDirective {
    send(
        ids.animation_clientbound,
        &Animation {
            entity_id,
            animation: action,
        },
        ctx,
        protocol,
    )
}

macro_rules! impl_server_protocol {
    ($type:ty, $protocol:expr, $ids:expr, $ctx:expr) => {
        impl ServerProtocol for $type {
            fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
                decode_packet($protocol, $ids, $ctx, state, packet_id, payload)
            }

            fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
                login_success($ids, $ctx, $protocol, username, uuid)
            }

            fn has_configuration_phase(&self) -> bool {
                false
            }

            fn begin_configuration(&self) -> Vec<ServerDirective> {
                Vec::new()
            }

            fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
                begin_play($ids, $ctx, $protocol)
            }

            fn uses_teleport_acknowledgements(&self) -> bool {
                true
            }

            fn begin_chunk_batch(&self) -> ServerDirective {
                ServerDirective::None
            }

            fn encode_chunk(
                &self,
                cx: i32,
                cz: i32,
                column: &ChunkColumn,
            ) -> ServerDirective {
                self.try_encode_chunk(cx, cz, column).unwrap_or_else(|_| {
                    panic!(
                        "call try_encode_chunk to handle an unrepresentable protocol-{} column",
                        $protocol
                    )
                })
            }

            fn try_encode_chunk(
                &self,
                cx: i32,
                cz: i32,
                column: &ChunkColumn,
            ) -> Result<ServerDirective, ChunkEncodeError> {
                try_encode_chunk($protocol, $ids, cx, cz, column)
            }

            fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
                ServerDirective::None
            }

            fn encode_keep_alive(&self, id: i64) -> ServerDirective {
                encode_keep_alive($protocol, $ids, $ctx, id)
            }

            fn encode_system_chat(&self, message: &str) -> ServerDirective {
                encode_system_chat($protocol, $ids, $ctx, message)
            }

            fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
                encode_animate($protocol, $ids, $ctx, entity_id, action)
            }

            fn encode_block_update(
                &self,
                x: i32,
                y: i32,
                z: i32,
                state: &str,
            ) -> ServerDirective {
                self.try_encode_block_update(x, y, z, state).unwrap_or_else(|_| {
                    panic!(
                        "call try_encode_block_update to handle an unrepresentable protocol-{} state",
                        $protocol
                    )
                })
            }
        }
    }
}

impl_server_protocol!(V340ServerProtocol, PROTOCOL, IDS_340, CTX_340);
impl_server_protocol!(V110ServerProtocol, PROTOCOL_1_9_4, IDS_110, CTX_110);
impl_server_protocol!(V210ServerProtocol, PROTOCOL_1_10_2, IDS_210, CTX_210);
impl_server_protocol!(V316ServerProtocol, PROTOCOL_1_11_2, IDS_316, CTX_316);

#[cfg(test)]
mod tests {
    use super::*;

    // VarInt length 14 followed by `legacy "chat"\n`. This is deliberately
    // a literal wire body: encoding the packet here would let a matching
    // encoder and decoder hide the same length or trailing-byte mistake.
    const CHAT_BODY: &[u8] = b"\x0elegacy \"chat\"\n";
    const CHAT_BODY_WITH_TRAILING_BYTE: &[u8] = b"\x0elegacy \"chat\"\n\0";
    const SYSTEM_CHAT_BODY: &[u8] = b"\x1c{\"text\":\"legacy \\\"chat\\\"\\n\"}\x01";

    #[test]
    fn every_hosted_table_lifts_literal_legacy_chat_and_encodes_its_echo() {
        for (protocol, ids, ctx) in [
            (PROTOCOL_1_9_4, IDS_110, CTX_110),
            (PROTOCOL_1_10_2, IDS_210, CTX_210),
            (PROTOCOL_1_11_2, IDS_316, CTX_316),
            (PROTOCOL, IDS_340, CTX_340),
        ] {
            assert_eq!(
                decode_packet(protocol, ids, ctx, State::Play, ids.chat_serverbound, CHAT_BODY),
                ServerBound::Chat {
                    message: "legacy \"chat\"\n".to_owned(),
                    timestamp_millis: 0,
                    salt: 0,
                    signature: None,
                },
                "protocol {protocol} must consume its generated chat id"
            );
            assert_eq!(
                decode_packet(
                    protocol,
                    ids,
                    ctx,
                    State::Play,
                    ids.chat_serverbound,
                    CHAT_BODY_WITH_TRAILING_BYTE,
                ),
                ServerBound::Ignored,
                "protocol {protocol} must not accept a chat prefix with extra bytes"
            );

            let ServerDirective::Send { packet_id, payload } =
                encode_system_chat(protocol, ids, ctx, "legacy \"chat\"\n")
            else {
                panic!("protocol {protocol} must encode a legacy chat reply");
            };
            assert_eq!(packet_id, ids.chat_clientbound);
            assert_eq!(payload, SYSTEM_CHAT_BODY);
        }
    }

    #[test]
    fn legacy_chat_component_escapes_each_json_control_boundary() {
        assert_eq!(
            legacy_text_component("quote=\" slash=\\ newline=\n control=\u{0007}"),
            "{\"text\":\"quote=\\\" slash=\\\\ newline=\\n control=\\u0007\"}"
        );
    }
}
