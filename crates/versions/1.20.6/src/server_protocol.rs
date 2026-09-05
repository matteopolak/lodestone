//! Server-side packet translation for protocol 766 (Minecraft 1.20.6).
//!
//! The host owns this protocol's join registry and 24-section inline-light
//! chunk layout. Its wire-state inverse is exact: canonical states without one
//! unique 766 state return an error instead of becoming a different block.

use lodestone_core::{
    Ctx, Decode, Encode, Nbt, Reader, State, Writer, encode_body, write_network_nbt,
};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Rotation};
use lodestone_server::{ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol};
use lodestone_world::{Heightmap, LongArrayFraming, PaletteKind, PalettedContainer};
use uuid::Uuid;

use crate::PROTOCOL_1_20_6;
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::configuration::RegistryData;
use crate::packets::game::{
    BlockDig, ClientboundPositionLook, JoinGame, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, SpawnInfo,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};

const CTX: Ctx = Ctx {
    version: PROTOCOL_1_20_6,
};
const COMPRESSION_THRESHOLD: i32 = 256;
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const SECTION_EDGE: i32 = 16;
const SECTION_COUNT: usize = 24;
const SECTION_BLOCKS: usize = 4096;

/// Server implementation for protocol 766.
#[derive(Clone, Copy, Debug, Default)]
pub struct V766ServerProtocol;

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-766 packet must encode"),
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
    wire_inverse().get(&canonical).copied().flatten().ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no unique exact protocol-766 representation"
        ))
    })
}

fn encode_heightmaps(column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let mut heightmap = Heightmap::new(HEIGHT as u32);
    for z in 0..16usize {
        for x in 0..16usize {
            let height = (MIN_Y..MIN_Y + HEIGHT)
                .rev()
                .find(|&y| !matches!(
                    lodestone_data::block_states::block_name(column.block_state_id(x as i32, y, z as i32)),
                    Some("minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
                ))
                .map_or(0, |y| {
                    u32::try_from(y - MIN_Y + 1).expect("height is non-negative")
                });
            heightmap.set(x, z, height);
        }
    }
    let nbt = Nbt::Compound(vec![(
        "WORLD_SURFACE".to_owned(),
        Nbt::LongArray(heightmap.longs().iter().map(|&value| value as i64).collect()),
    )]);
    let mut out = Writer::default();
    write_network_nbt(&mut out, &nbt).map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    Ok(out.into_vec())
}

/// Reads canonical states in the fixed hosted Overworld window.
struct HostedLightVolume<'a>(&'a ChunkColumn);

impl lodestone_world::BlockVolume for HostedLightVolume<'_> {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        if !(MIN_Y..MIN_Y + HEIGHT).contains(&y) {
            return lodestone_data::block_states::air_state_id();
        }
        self.0.block_state_id(x as i32, y, z as i32)
    }

    fn min_y(&self) -> i32 { MIN_Y }

    fn section_count(&self) -> usize { SECTION_COUNT }
}

struct CanonicalLightProperties;

impl lodestone_world::LightProperties for CanonicalLightProperties {
    fn opacity(&self, state: u32) -> u8 {
        let state = lodestone_data::block_states::StateId::new(state)
            .expect("server column contains canonical block states");
        lodestone_data::light_props::light_props(state).0
    }

    fn emission(&self, state: u32) -> u8 {
        let state = lodestone_data::block_states::StateId::new(state)
            .expect("server column contains canonical block states");
        lodestone_data::light_props::light_props(state).1
    }
}

fn served_light(column: &ChunkColumn) -> lodestone_world::ColumnLight {
    lodestone_world::compute_column_light(&HostedLightVolume(column), &CanonicalLightProperties)
}

fn served_light_with_neighbours(
    column: &ChunkColumn,
    neighbours: &[(i32, i32, ChunkColumn)],
) -> lodestone_world::ColumnLight {
    let center = HostedLightVolume(column);
    let volumes = neighbours
        .iter()
        .map(|(_, _, neighbour)| HostedLightVolume(neighbour))
        .collect::<Vec<_>>();
    let mut neighbourhood = lodestone_world::Neighbourhood::new(&center);
    for ((dx, dz, _), volume) in neighbours.iter().zip(&volumes) {
        neighbourhood = neighbourhood.with(*dx, *dz, volume);
    }
    lodestone_world::compute_column_light_with_neighbours(&neighbourhood, &CanonicalLightProperties)
}

fn wire_inverse() -> &'static std::collections::BTreeMap<u32, Option<u32>> {
    static INVERSE: std::sync::OnceLock<std::collections::BTreeMap<u32, Option<u32>>> =
        std::sync::OnceLock::new();
    INVERSE.get_or_init(|| {
        let mut inverse = std::collections::BTreeMap::new();
        for (wire, &canonical) in crate::generated_canonical::STATE_TO_CANONICAL.iter().enumerate() {
            inverse.entry(canonical)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(u32::try_from(wire).expect("wire state fits u32")));
        }
        inverse
    })
}

fn configuration_packets() -> &'static [(i32, Vec<u8>)] {
    static PACKETS: std::sync::OnceLock<Vec<(i32, Vec<u8>)>> = std::sync::OnceLock::new();
    PACKETS.get_or_init(|| {
        include_str!("generated/hosting-configuration.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| {
                let mut fields = line.split_whitespace();
                assert_eq!(fields.next(), Some("configuration"));
                let id = fields.next().expect("fixture packet id").parse().expect("numeric packet id");
                let hex = fields.next().expect("fixture packet body");
                assert_eq!(hex.len() % 2, 0);
                let bytes = (0..hex.len()).step_by(2)
                    .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("fixture hex"))
                    .collect();
                (id, bytes)
            })
            .collect()
    })
}

fn registry_index(registry: &str, entry: &str) -> i32 {
    for (id, payload) in configuration_packets() {
        if *id == configuration::clientbound::REGISTRY_DATA {
            let data = decode_full::<RegistryData>(payload).expect("captured registry is valid");
            if data.registry == registry {
                return i32::try_from(data.entries.iter().position(|value| value.id == entry)
                    .expect("captured registry contains required entry")).expect("registry index fits");
            }
        }
    }
    panic!("captured configuration is missing required registry {registry}");
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
        return Err(ChunkEncodeError::new("protocol-766 column bounds overflow"));
    };
    if column.min_y > MIN_Y || column_end < MIN_Y + HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-766 requires columns covering y={MIN_Y} through y={}",
            MIN_Y + HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-766 chunk block entities are not implemented",
        ));
    }
    for qy in 0..usize::try_from(HEIGHT / 4).expect("fixed biome layers") {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-766 representation",
                        column.biome_cell(qx, qy, qz)
                    )));
                }
            }
        }
    }

    let air = lodestone_data::block_states::air_state_id();
    let block_kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
    let biome_kind = PaletteKind::biomes().with_framing(LongArrayFraming::Prefixed);
    let biome_values = [u32::try_from(registry_index("minecraft:worldgen/biome", "minecraft:plains"))
        .expect("plains id fits u32"); 64];
    let mut sections = Writer::default();
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
        let _ = encode_container(&mut sections, block_kind, &wire_states?);
        let _ = encode_container(&mut sections, biome_kind, &biome_values);
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bytes(&encode_heightmaps(column)?);
    packet
        .var_bytes(&sections.into_vec())
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    served_light(column).encode(&mut packet);
    Ok(packet.into_vec())
}

impl V766ServerProtocol {
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
        payload.var_i32(i32::try_from(wire).expect("protocol-766 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V766ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                let next_state = if handshake.protocol_version == PROTOCOL_1_20_6 {
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
                        uuid: start.uuid,
                    }
                })
            }
            State::Login if packet_id == login::serverbound::LOGIN_ACKNOWLEDGED && payload.is_empty() => {
                ServerBound::LoginAcknowledged
            }
            State::Configuration if packet_id == configuration::serverbound::FINISH_CONFIGURATION
                && payload.is_empty() => ServerBound::ConfigurationFinished,
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
            State::Play if packet_id == play::serverbound::CHUNK_BATCH_RECEIVED => {
                decode_full::<crate::packets::game::ChunkBatchReceived>(payload)
                    .map_or(ServerBound::Ignored, |ack| ServerBound::ChunkBatchAcknowledged {
                        desired_chunks_per_tick: ack.chunks_per_tick,
                    })
            }
            State::Play if packet_id == play::serverbound::POSITION => {
                decode_full::<ServerboundPosition>(payload).map_or(
                    ServerBound::Ignored,
                    |move_| ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: None,
                        on_ground: move_.on_ground,
                    },
                )
            }
            State::Play if packet_id == play::serverbound::POSITION_LOOK => {
                decode_full::<ServerboundPositionLook>(payload).map_or(
                    ServerBound::Ignored,
                    |move_| ServerBound::PlayerMoved {
                        x: move_.x,
                        y: move_.y,
                        z: move_.z,
                        rotation: Some(Rotation {
                            yaw: move_.yaw,
                            pitch: move_.pitch,
                        }),
                        on_ground: move_.on_ground,
                    },
                )
            }
            State::Play if packet_id == play::serverbound::LOOK => {
                decode_full::<ServerboundLook>(payload).map_or(
                    ServerBound::Ignored,
                    |look| ServerBound::PlayerRotated {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        on_ground: look.on_ground,
                    },
                )
            }
            State::Play if packet_id == play::serverbound::FLYING => {
                decode_full::<ServerboundFlying>(payload).map_or(
                    ServerBound::Ignored,
                    |status| ServerBound::PlayerStatusOnly {
                        on_ground: status.on_ground,
                    },
                )
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
                    strict_error_handling: true,
                },
            ),
        ]
    }

    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        configuration_packets().iter().map(|(packet_id, payload)| ServerDirective::Send {
            packet_id: *packet_id,
            payload: payload.clone(),
        }).collect()
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: configuration::clientbound::FINISH_CONFIGURATION,
            payload: Vec::new(),
        }]
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send(
                play::clientbound::LOGIN,
                &JoinGame {
                    entity_id: 1,
                    is_hardcore: false,
                    world_names: vec!["minecraft:overworld".to_owned()],
                    max_players: 20,
                    view_distance: view_radius.max(1),
                    simulation_distance: view_radius.max(1),
                    reduced_debug_info: false,
                    enable_respawn_screen: true,
                    do_limited_crafting: false,
                    world_state: SpawnInfo {
                        dimension: registry_index("minecraft:dimension_type", "minecraft:overworld"),
                        world_name: "minecraft:overworld".to_owned(),
                        hashed_seed: 0,
                        game_mode: 0,
                        previous_game_mode: 255,
                        is_debug: false,
                        is_flat: true,
                        has_death_location: false,
                        death_dimension: None,
                        death_location: None,
                        portal_cooldown: 0,
                    },
                    enforces_secure_chat: false,
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
        ServerDirective::Send {
            packet_id: play::clientbound::CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-766 column")
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

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(batch_size);
        ServerDirective::Send {
            packet_id: play::clientbound::CHUNK_BATCH_FINISHED,
            payload: payload.into_vec(),
        }
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-766 state")
    }

    fn compute_column_light(&self, column: &ChunkColumn) -> Option<lodestone_world::ColumnLight> {
        let end = column.min_y.checked_add(column.height)?;
        if column.min_y > MIN_Y || end < MIN_Y + HEIGHT {
            return None;
        }
        Some(served_light(column))
    }

    fn uses_cross_column_light(&self) -> bool {
        true
    }

    fn compute_column_light_with_neighbours(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
    ) -> Option<lodestone_world::ColumnLight> {
        let end = column.min_y.checked_add(column.height)?;
        if column.min_y > MIN_Y || end < MIN_Y + HEIGHT {
            return None;
        }
        Some(served_light_with_neighbours(column, neighbours))
    }

    fn encode_light_update(&self, cx: i32, cz: i32, light: &lodestone_world::ColumnLight) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(cx);
        payload.var_i32(cz);
        light.encode(&mut payload);
        ServerDirective::Send {
            packet_id: play::clientbound::UPDATE_LIGHT,
            payload: payload.into_vec(),
        }
    }
}
