//! Server-side packet translation for protocol 762 (Minecraft 1.19.4).
//!
//! The host owns this protocol's join registry and 24-section inline-light
//! chunk layout. Its wire-state inverse is exact: canonical states without one
//! unique 762 state return an error instead of becoming a different block.

use lodestone_core::{
    Ctx, Decode, Encode, Nbt, NbtTag, Reader, State, Writer, encode_body, write_named_nbt,
};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Rotation, Vec3f};
use lodestone_server::{ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol};
use lodestone_world::{Heightmap, LongArrayFraming, PaletteKind, PalettedContainer};
use uuid::Uuid;

use crate::PROTOCOL_1_19_4;
use crate::canonical::wire_state_for_762;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::game::{
    BlockDig, BlockPlace, ClientboundPositionLook, JoinGame, ServerboundFlying, ServerboundLook,
    ServerboundArmAnimation, ServerboundPosition, ServerboundPositionLook,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};

const CTX: Ctx = Ctx {
    version: PROTOCOL_1_19_4,
};
const COMPRESSION_THRESHOLD: i32 = 256;
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const SECTION_EDGE: i32 = 16;
const SECTION_COUNT: usize = 24;
const SECTION_BLOCKS: usize = 4096;
const PLAINS_BIOME_ID: i32 = 0;

/// Server implementation for protocol 762.
#[derive(Clone, Copy, Debug, Default)]
pub struct V762ServerProtocol;

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-762 packet must encode"),
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
    wire_state_for_762(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no unique exact protocol-762 representation"
        ))
    })
}

fn encode_heightmaps(column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let mut heightmap = Heightmap::new(HEIGHT as u32);
    let air = lodestone_data::block_states::air_state_id();
    for z in 0..16usize {
        for x in 0..16usize {
            let height = (MIN_Y..MIN_Y + HEIGHT)
                .rev()
                .find(|&y| column.block_state_id(x as i32, y, z as i32) != air)
                .map_or(0, |y| {
                    u32::try_from(y - MIN_Y + 1).expect("height is non-negative")
                });
            heightmap.set(x, z, height);
        }
    }
    let nbt = Nbt::Compound(vec![(
        "MOTION_BLOCKING".to_owned(),
        Nbt::LongArray(heightmap.longs().iter().map(|&value| value as i64).collect()),
    )]);
    let mut out = Writer::default();
    write_named_nbt(&mut out, "", &nbt).map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    Ok(out.into_vec())
}

fn dimension_codec() -> Vec<u8> {
    let registry = Nbt::Compound(vec![(
        "minecraft:dimension_type".to_owned(),
        Nbt::Compound(vec![(
            "value".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    (
                        "name".to_owned(),
                        Nbt::String("minecraft:overworld".to_owned()),
                    ),
                    ("id".to_owned(), Nbt::Int(0)),
                    (
                        "element".to_owned(),
                        Nbt::Compound(vec![
                            ("min_y".to_owned(), Nbt::Int(MIN_Y)),
                            ("height".to_owned(), Nbt::Int(HEIGHT)),
                        ]),
                    ),
                ])],
            },
        )]),
    )]);
    let mut writer = Writer::default();
    write_named_nbt(&mut writer, "", &registry).expect("fixed dimension registry must encode");
    writer.into_vec()
}

fn encode_container(writer: &mut Writer, kind: PaletteKind, values: &[u32]) -> bool {
    let container = PalettedContainer::from_values(kind, values);
    let single = container.is_single();
    container.encode(writer);
    if single {
        // This protocol puts the zero packed-long count after a one-value
        // container; the chunk decoder consumes that count explicitly.
        writer.var_i32(0);
    }
    single
}

fn encode_chunk_body(
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol-762 column bounds overflow"));
    };
    if column.min_y > MIN_Y || column_end < MIN_Y + HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-762 requires columns covering y={MIN_Y} through y={}",
            MIN_Y + HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-762 chunk block entities are not implemented",
        ));
    }
    for qy in 0..usize::try_from(HEIGHT / 4).expect("fixed biome layers") {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-762 representation",
                        column.biome_cell(qx, qy, qz)
                    )));
                }
            }
        }
    }

    let air = lodestone_data::block_states::air_state_id();
    let block_kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
    let biome_kind = PaletteKind::biomes().with_framing(LongArrayFraming::Prefixed);
    let biome_values = [u32::try_from(PLAINS_BIOME_ID).expect("plains id fits u32"); 64];
    let mut sections = Writer::default();
    let mut trailing_padding = 0usize;
    for section in 0..SECTION_COUNT {
        let y_base =
            MIN_Y + i32::try_from(section).expect("section fits i32") * SECTION_EDGE;
        let mut states = Vec::with_capacity(SECTION_BLOCKS);
        for y in y_base..y_base + SECTION_EDGE {
            for z in 0..SECTION_EDGE {
                for x in 0..SECTION_EDGE {
                    states.push(column.block_state_id(x, y, z));
                }
            }
        }
        let non_air = states.iter().filter(|&&state| state != air).count();
        sections.i16(i16::try_from(non_air).expect("section has at most 4096 blocks"));
        let wire_states: Result<Vec<u32>, _> = states.iter().copied().map(wire_state).collect();
        if encode_container(&mut sections, block_kind, &wire_states?) {
            trailing_padding += 1;
        }
        let _ = encode_container(&mut sections, biome_kind, &biome_values);
    }
    for _ in 0..trailing_padding {
        sections.u8(0);
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bytes(&encode_heightmaps(column)?);
    packet
        .var_bytes(&sections.into_vec())
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    packet.bool(false);
    for _ in 0..4 {
        packet.var_i32(0);
    }
    packet.var_i32(0);
    packet.var_i32(0);
    Ok(packet.into_vec())
}

impl V762ServerProtocol {
    /// Converts and encodes a block update without replacing an unsupported state.
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
        payload.var_i32(i32::try_from(wire).expect("protocol-762 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V762ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                let next_state = if handshake.protocol_version == PROTOCOL_1_19_4 {
                    if handshake.next_state == 2 { State::Login } else { State::Status }
                } else {
                    return ServerBound::Ignored;
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
                    sequence,
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
                    sequence,
                }
            }
            State::Play if packet_id == play::serverbound::BLOCK_PLACE => {
                let Some(BlockPlace {
                    hand,
                    location: Position(pos),
                    direction,
                    cursor_x,
                    cursor_y,
                    cursor_z,
                    inside_block: _,
                    sequence,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                let (Ok(hand), Ok(face)) = (u8::try_from(hand), i8::try_from(direction)) else {
                    return ServerBound::Ignored;
                };
                let Some(face) = block_face(face) else {
                    return ServerBound::Ignored;
                };
                if hand > 1 {
                    return ServerBound::Ignored;
                }
                ServerBound::UseItemOn {
                    pos,
                    face,
                    cursor: Vec3f {
                        x: cursor_x,
                        y: cursor_y,
                        z: cursor_z,
                    },
                    sequence,
                    hand,
                }
            }
            State::Play if packet_id == play::serverbound::ARM_ANIMATION => {
                let Some(hand) = decode_full::<ServerboundArmAnimation>(payload)
                    .and_then(|packet| u8::try_from(packet.hand).ok())
                    .filter(|&hand| hand <= 1)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::Swing { hand }
            }
            State::Play if packet_id == play::serverbound::POSITION => {
                let Some(ServerboundPosition {
                    x,
                    y,
                    z,
                    on_ground,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::PlayerMoved {
                    x,
                    y,
                    z,
                    rotation: None,
                    on_ground,
                }
            }
            State::Play if packet_id == play::serverbound::POSITION_LOOK => {
                let Some(ServerboundPositionLook {
                    x,
                    y,
                    z,
                    yaw,
                    pitch,
                    on_ground,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::PlayerMoved {
                    x,
                    y,
                    z,
                    rotation: Some(Rotation { yaw, pitch }),
                    on_ground,
                }
            }
            State::Play if packet_id == play::serverbound::LOOK => {
                let Some(ServerboundLook {
                    yaw,
                    pitch,
                    on_ground,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::PlayerRotated {
                    yaw,
                    pitch,
                    on_ground,
                }
            }
            State::Play if packet_id == play::serverbound::FLYING => {
                let Some(ServerboundFlying { on_ground }) = decode_full(payload) else {
                    return ServerBound::Ignored;
                };
                ServerBound::PlayerStatusOnly { on_ground }
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
                    uuid,
                    username: username.to_owned(),
                    properties: Vec::new(),
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

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send(
                play::clientbound::LOGIN,
                &JoinGame {
                    entity_id: 1,
                    is_hardcore: false,
                    game_mode: 0,
                    previous_game_mode: -1,
                    world_names: vec!["minecraft:overworld".to_owned()],
                    dimension_codec: dimension_codec(),
                    world_type: "minecraft:overworld".to_owned(),
                    world_name: "minecraft:overworld".to_owned(),
                    hashed_seed: 0,
                    max_players: 20,
                    view_distance: view_radius,
                    simulation_distance: view_radius,
                    reduced_debug_info: false,
                    enable_respawn_screen: true,
                    is_debug: false,
                    is_flat: true,
                    has_death_location: false,
                    death_dimension: None,
                    death_location: None,
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

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-762 column")
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

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(entity_id);
        payload.u8(action);
        ServerDirective::Send {
            packet_id: play::clientbound::ANIMATION,
            payload: payload.into_vec(),
        }
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-762 state")
    }
}
