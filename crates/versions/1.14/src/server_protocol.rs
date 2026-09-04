//! Server-side packet translation for protocols 498 (1.14.4), 578 (1.15.2)
//! and 754 (1.16.5).
//!
//! Each hosted member is deliberately protocol-specific: the three packet
//! registries, login-success UUID forms, join layouts, biome arrays and
//! section packing rules are selected by the concrete implementation. The
//! encoders lower only canonical states present in their committed source
//! table and report every unsupported state instead of replacing it with air.

use std::collections::BTreeMap;

use lodestone_core::{Ctx, Decode, Encode, Nbt, Reader, State, Writer, encode_body, write_named_nbt};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Text};
use lodestone_server::{ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol};
use lodestone_world::{Heightmap, LongArrayFraming, PaletteKind, PalettedContainer};
use uuid::Uuid;

use crate::{PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5};
use crate::canonical::{wire_state_for_498, wire_state_for_578, wire_state_for_754};
use crate::packet_ids_578::{handshaking, login, play};
use crate::packet_ids::{handshaking as handshaking_754, login as login_754, play as play_754};
use crate::packet_ids_498::{handshaking as handshaking_498, login as login_498, play as play_498};
use crate::packets::game::{BlockDig, ClientboundPositionLook, JoinGameLegacy, KickDisconnect};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccessString, SetCompression};
use crate::packets::position::{Position, pack_position};

const CTX: Ctx = Ctx { version: PROTOCOL_1_15_2 };
const CTX_498: Ctx = Ctx { version: PROTOCOL_1_14_4 };
const CTX_754: Ctx = Ctx { version: PROTOCOL_1_16_5 };
const COMPRESSION_THRESHOLD: i32 = 256;
const MIN_Y: i32 = 0;
const HEIGHT: i32 = 256;
const SECTION_EDGE: i32 = 16;
const SECTION_COUNT: usize = 16;
const SECTION_BLOCKS: usize = 4096;
const PLAINS_BIOME_ID: i32 = 1;
const GLOBAL_BITS: u8 = 14;

/// Server implementation for protocol 578 (Minecraft 1.15.2).
#[derive(Clone, Copy, Debug, Default)]
pub struct V578ServerProtocol;

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-578 packet must encode"),
    }
}

fn send_754<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX_754).expect("fixed protocol-754 packet must encode"),
    }
}

fn send_498<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX_498).expect("fixed protocol-498 packet must encode"),
    }
}

fn decode_full<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

fn decode_full_754<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX_754).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

fn decode_full_498<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX_498).ok()?;
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

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing into a String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn wire_state(canonical: u32) -> Result<u32, ChunkEncodeError> {
    wire_state_for_578(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no exact protocol-578 representation"
        ))
    })
}

fn wire_state_754(canonical: u32) -> Result<u32, ChunkEncodeError> {
    wire_state_for_754(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no exact protocol-754 representation"
        ))
    })
}

fn wire_state_498(canonical: u32) -> Result<u32, ChunkEncodeError> {
    wire_state_for_498(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no exact protocol-498 representation"
        ))
    })
}

fn bits_for_palette(len: usize) -> u8 {
    let bits = usize::BITS - (len.saturating_sub(1)).leading_zeros();
    u8::try_from(bits.max(4)).expect("protocol-578 palette width fits in u8")
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
            blob.var_i32(i32::try_from(state).expect("protocol-578 state fits in i32"));
        }
        let longs = pack_indices(&indices, bits);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    } else {
        blob.u8(GLOBAL_BITS);
        blob.var_i32(0);
        let values: Result<Vec<u32>, _> = states.iter().copied().map(wire_state).collect();
        let longs = pack_indices(&values?, GLOBAL_BITS);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    }
    Ok(())
}

fn encode_section_498(blob: &mut Writer, states: &[u32]) -> Result<(), ChunkEncodeError> {
    let mut palette = Vec::new();
    let mut indices = Vec::with_capacity(states.len());
    let mut palette_indices = BTreeMap::new();

    for &state in states {
        let wire = wire_state_498(state)?;
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
            blob.var_i32(i32::try_from(state).expect("protocol-498 state fits in i32"));
        }
        let longs = pack_indices(&indices, bits);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    } else {
        blob.u8(GLOBAL_BITS);
        blob.var_i32(0);
        let values: Result<Vec<u32>, _> = states.iter().copied().map(wire_state_498).collect();
        let longs = pack_indices(&values?, GLOBAL_BITS);
        blob.var_i32(i32::try_from(longs.len()).expect("section long count fits in i32"));
        for long in longs {
            blob.i64(long as i64);
        }
    }
    Ok(())
}

fn encode_heightmaps(column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let mut heightmap = Heightmap::new(HEIGHT as u32);
    for z in 0..16usize {
        for x in 0..16usize {
            let height = (MIN_Y..MIN_Y + HEIGHT)
                .rev()
                .find(|&y| {
                    column.block_state_id(x as i32, y, z as i32)
                        != lodestone_data::block_states::air_state_id()
                })
                .map_or(0, |y| u32::try_from(y + 1).expect("height is non-negative"));
            heightmap.set(x, z, height);
        }
    }
    let nbt = Nbt::Compound(vec![("MOTION_BLOCKING".to_owned(), Nbt::LongArray(
        heightmap.longs().iter().map(|&value| value as i64).collect(),
    ))]);
    let mut out = Writer::default();
    write_named_nbt(&mut out, "", &nbt)
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    Ok(out.into_vec())
}

fn encode_chunk_body(cx: i32, cz: i32, column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol-578 column bounds overflow"));
    };
    if column.min_y > MIN_Y || column_end < MIN_Y + HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-578 requires columns covering y={MIN_Y} through y={}",
            MIN_Y + HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-578 chunk block entities are not implemented",
        ));
    }
    for qy in 0..(HEIGHT as usize / 4) {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-578 representation",
                        column.biome_cell(qx, qy, qz)
                    )));
                }
            }
        }
    }

    let air = lodestone_data::block_states::air_state_id();
    let mut bitmask = 0_u32;
    let mut sections = Writer::default();
    for section in 0..SECTION_COUNT {
        let y_base = MIN_Y + section as i32 * SECTION_EDGE;
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
        let non_air = states.iter().filter(|&&state| state != air).count();
        sections.i16(i16::try_from(non_air).expect("section has at most 4096 blocks"));
        encode_section(&mut sections, &states)?;
        bitmask |= 1 << section;
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bool(true);
    packet.var_i32(bitmask as i32);
    packet.bytes(&encode_heightmaps(column)?);
    for _ in 0..1024 {
        packet.i32(PLAINS_BIOME_ID);
    }
    let section_bytes = sections.into_vec();
    packet.var_bytes(&section_bytes)
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    Ok(packet.into_vec())
}

fn encode_chunk_body_754(
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol-754 column bounds overflow"));
    };
    if column.min_y > MIN_Y || column_end < MIN_Y + HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-754 requires columns covering y={MIN_Y} through y={}",
            MIN_Y + HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-754 chunk block entities are not implemented",
        ));
    }
    for qy in 0..(HEIGHT as usize / 4) {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-754 representation",
                        column.biome_cell(qx, qy, qz)
                    )));
                }
            }
        }
    }

    let air = lodestone_data::block_states::air_state_id();
    let wire_kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
    let mut bitmask = 0_u32;
    let mut sections = Writer::default();
    for section in 0..SECTION_COUNT {
        let y_base = MIN_Y + section as i32 * SECTION_EDGE;
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
        let non_air = states.iter().filter(|&&state| state != air).count();
        sections.i16(i16::try_from(non_air).expect("section has at most 4096 blocks"));
        let wire_states: Result<Vec<u32>, _> = states.iter().copied().map(wire_state_754).collect();
        PalettedContainer::from_values(wire_kind, &wire_states?).encode(&mut sections);
        bitmask |= 1 << section;
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bool(true);
    packet.var_i32(bitmask as i32);
    packet.bytes(&encode_heightmaps(column)?);
    packet.var_i32(1024);
    for _ in 0..1024 {
        packet.var_i32(PLAINS_BIOME_ID);
    }
    let section_bytes = sections.into_vec();
    packet
        .var_bytes(&section_bytes)
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    Ok(packet.into_vec())
}

fn encode_chunk_body_498(
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol-498 column bounds overflow"));
    };
    if column.min_y > MIN_Y || column_end < MIN_Y + HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-498 requires columns covering y={MIN_Y} through y={}",
            MIN_Y + HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-498 chunk block entities are not implemented",
        ));
    }
    for qy in 0..(HEIGHT as usize / 4) {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-498 representation",
                        column.biome_cell(qx, qy, qz)
                    )));
                }
            }
        }
    }

    let air = lodestone_data::block_states::air_state_id();
    let mut bitmask = 0_u32;
    let mut sections = Writer::default();
    for section in 0..SECTION_COUNT {
        let y_base = MIN_Y + section as i32 * SECTION_EDGE;
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
        let non_air = states.iter().filter(|&&state| state != air).count();
        sections.i16(i16::try_from(non_air).expect("section has at most 4096 blocks"));
        encode_section_498(&mut sections, &states)?;
        bitmask |= 1 << section;
    }

    // Protocol 498 keeps the 16x16 biome array inside chunkData after the
    // section records; the outer packet still carries one VarInt buffer
    // length and then the trailing block-entity count.
    let mut chunk_data = sections.into_vec();
    for _ in 0..256 {
        chunk_data.extend_from_slice(&PLAINS_BIOME_ID.to_be_bytes());
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bool(true);
    packet.var_i32(bitmask as i32);
    packet.bytes(&encode_heightmaps(column)?);
    packet
        .var_bytes(&chunk_data)
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    Ok(packet.into_vec())
}

impl V578ServerProtocol {
    /// Converts one canonical state into the protocol-578 block-update packet.
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
        payload.var_i32(i32::try_from(wire).expect("protocol-578 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V578ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                if handshake.protocol_version != PROTOCOL_1_15_2 {
                    return ServerBound::Ignored;
                }
                let next_state = if handshake.next_state == 2 { State::Login } else { State::Status };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == login::serverbound::LOGIN_START => {
                decode_full::<LoginStart>(payload).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart { username: start.username, uuid: Uuid::nil() }
                })
            }
            State::Play if packet_id == play::serverbound::BLOCK_DIG => {
                let Some(BlockDig { status, location: Position(pos), face }) = decode_full(payload) else {
                    return ServerBound::Ignored;
                };
                let (Some(action), Some(face)) = (block_action(status), block_face(face)) else {
                    return ServerBound::Ignored;
                };
                ServerBound::BlockAction { action, pos, face, sequence: 0 }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        vec![
            send(login::clientbound::COMPRESS, &SetCompression { threshold: COMPRESSION_THRESHOLD }),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send(login::clientbound::SUCCESS, &LoginSuccessString { uuid: uuid.to_string(), username: username.to_owned() }),
        ]
    }

    fn has_configuration_phase(&self) -> bool { false }

    fn begin_configuration(&self) -> Vec<ServerDirective> { Vec::new() }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send(play::clientbound::LOGIN, &JoinGameLegacy {
                entity_id: 1,
                game_mode: 0,
                dimension: 0,
                hashed_seed: 0,
                max_players: 20,
                level_type: "default".to_owned(),
                view_distance: view_radius,
                reduced_debug_info: false,
                enable_respawn_screen: true,
            }),
            send(play::clientbound::POSITION, &ClientboundPositionLook {
                x: 8.0,
                y: 100.0,
                z: 8.0,
                yaw: 0.0,
                pitch: 0.0,
                flags: 0,
                teleport_id: 0,
            }),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective { ServerDirective::None }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-578 column")
    }

    fn try_encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(ServerDirective::Send { packet_id: play::clientbound::MAP_CHUNK, payload: encode_chunk_body(cx, cz, column)? })
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective { ServerDirective::None }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-578 state")
    }

    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        if state != State::Play {
            return ServerDirective::None;
        }
        send(play::clientbound::KICK_DISCONNECT, &KickDisconnect {
            reason: format!(
                "{{\"text\":\"{}\"}}",
                json_string(&reason.to_plain_string())
            ),
        })
    }
}

const RAW_DIMENSION_CODEC_NBT: &[u8] = &[
    0x0a, 0x00, 0x04, b'r', b'o', b'o', b't', 0x08, 0x00, 0x04, b'n', b'a', b'm', b'e', 0x00,
    0x13, b'm', b'i', b'n', b'e', b'c', b'r', b'a', b'f', b't', b':', b'o', b'v', b'e', b'r', b'w',
    b'o', b'r', b'l', b'd', 0x00,
];
const RAW_DIMENSION_TYPE_NBT: &[u8] = &[
    0x0a, 0x00, 0x03, b'd', b'i', b'm', 0x01, 0x00, 0x07, b'n', b'a', b't', b'u', b'r', b'a', b'l',
    0x01, 0x00,
];

/// Server implementation for protocol 754 (Minecraft 1.16.5).
#[derive(Clone, Copy, Debug, Default)]
pub struct V754ServerProtocol;

impl V754ServerProtocol {
    /// Converts one canonical state into the protocol-754 block-update packet.
    pub fn try_encode_block_update(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        let canonical = lodestone_data::block_states::state_id(state)
            .ok_or_else(|| ChunkEncodeError::new(format!("unknown canonical block state {state}")))?;
        let wire = wire_state_754(canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(wire).expect("protocol-754 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play_754::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V754ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking_754::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full_754::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                if handshake.protocol_version != PROTOCOL_1_16_5 {
                    return ServerBound::Ignored;
                }
                let next_state = if handshake.next_state == 2 { State::Login } else { State::Status };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == login_754::serverbound::LOGIN_START => {
                decode_full_754::<LoginStart>(payload).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart { username: start.username, uuid: Uuid::nil() }
                })
            }
            State::Play if packet_id == play_754::serverbound::BLOCK_DIG => {
                let Some(BlockDig { status, location: Position(pos), face }) =
                    decode_full_754(payload)
                else {
                    return ServerBound::Ignored;
                };
                let (Some(action), Some(face)) = (block_action(status), block_face(face)) else {
                    return ServerBound::Ignored;
                };
                ServerBound::BlockAction { action, pos, face, sequence: 0 }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        vec![
            send_754(login_754::clientbound::COMPRESS, &SetCompression { threshold: COMPRESSION_THRESHOLD }),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send_754(login_754::clientbound::SUCCESS, &crate::packets::login::LoginSuccess {
                uuid,
                username: username.to_owned(),
            }),
        ]
    }

    fn has_configuration_phase(&self) -> bool { false }

    fn begin_configuration(&self) -> Vec<ServerDirective> { Vec::new() }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send_754(play_754::clientbound::LOGIN, &crate::packets::game::JoinGame {
                entity_id: 1,
                is_hardcore: false,
                game_mode: 0,
                previous_game_mode: 255,
                world_names: vec!["minecraft:overworld".to_owned()],
                dimension_codec: RAW_DIMENSION_CODEC_NBT.to_vec(),
                dimension: RAW_DIMENSION_TYPE_NBT.to_vec(),
                world_name: "minecraft:overworld".to_owned(),
                hashed_seed: 0,
                max_players: 20,
                view_distance: view_radius,
                reduced_debug_info: false,
                enable_respawn_screen: true,
                is_debug: false,
                is_flat: false,
            }),
            send_754(play_754::clientbound::POSITION, &ClientboundPositionLook {
                x: 8.0,
                y: 100.0,
                z: 8.0,
                yaw: 0.0,
                pitch: 0.0,
                flags: 0,
                teleport_id: 0,
            }),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective { ServerDirective::None }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-754 column")
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(ServerDirective::Send {
            packet_id: play_754::clientbound::MAP_CHUNK,
            payload: encode_chunk_body_754(cx, cz, column)?,
        })
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective { ServerDirective::None }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-754 state")
    }

    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        if state != State::Play {
            return ServerDirective::None;
        }
        send_754(play_754::clientbound::KICK_DISCONNECT, &KickDisconnect {
            reason: format!("{{\"text\":\"{}\"}}", json_string(&reason.to_plain_string())),
        })
    }
}

/// Server implementation for protocol 498 (Minecraft 1.14.4).
#[derive(Clone, Copy, Debug, Default)]
pub struct V498ServerProtocol;

impl V498ServerProtocol {
    /// Converts one canonical state into the protocol-498 block-update packet.
    pub fn try_encode_block_update(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: &str,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        let canonical = lodestone_data::block_states::state_id(state)
            .ok_or_else(|| ChunkEncodeError::new(format!("unknown canonical block state {state}")))?;
        let wire = wire_state_498(canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(wire).expect("protocol-498 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play_498::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V498ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking_498::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full_498::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                if handshake.protocol_version != PROTOCOL_1_14_4 {
                    return ServerBound::Ignored;
                }
                let next_state = if handshake.next_state == 2 { State::Login } else { State::Status };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == login_498::serverbound::LOGIN_START => {
                decode_full_498::<LoginStart>(payload).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart { username: start.username, uuid: Uuid::nil() }
                })
            }
            State::Play if packet_id == play_498::serverbound::BLOCK_DIG => {
                let Some(BlockDig { status, location: Position(pos), face }) =
                    decode_full_498(payload)
                else {
                    return ServerBound::Ignored;
                };
                let (Some(action), Some(face)) = (block_action(status), block_face(face)) else {
                    return ServerBound::Ignored;
                };
                ServerBound::BlockAction { action, pos, face, sequence: 0 }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        vec![
            send_498(login_498::clientbound::COMPRESS, &SetCompression { threshold: COMPRESSION_THRESHOLD }),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send_498(login_498::clientbound::SUCCESS, &LoginSuccessString {
                uuid: uuid.to_string(),
                username: username.to_owned(),
            }),
        ]
    }

    fn has_configuration_phase(&self) -> bool { false }

    fn begin_configuration(&self) -> Vec<ServerDirective> { Vec::new() }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send_498(play_498::clientbound::LOGIN, &JoinGameLegacy {
                entity_id: 1,
                game_mode: 0,
                dimension: 0,
                hashed_seed: 0,
                max_players: 20,
                level_type: "default".to_owned(),
                view_distance: view_radius,
                reduced_debug_info: false,
                enable_respawn_screen: true,
            }),
            send_498(play_498::clientbound::POSITION, &ClientboundPositionLook {
                x: 8.0,
                y: 100.0,
                z: 8.0,
                yaw: 0.0,
                pitch: 0.0,
                flags: 0,
                teleport_id: 0,
            }),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective { ServerDirective::None }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-498 column")
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(ServerDirective::Send {
            packet_id: play_498::clientbound::MAP_CHUNK,
            payload: encode_chunk_body_498(cx, cz, column)?,
        })
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective { ServerDirective::None }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-498 state")
    }

    fn encode_disconnect(&self, state: State, reason: &Text) -> ServerDirective {
        if state != State::Play {
            return ServerDirective::None;
        }
        send_498(play_498::clientbound::KICK_DISCONNECT, &KickDisconnect {
            reason: format!("{{\"text\":\"{}\"}}", json_string(&reason.to_plain_string())),
        })
    }
}
