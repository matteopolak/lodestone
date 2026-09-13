//! Server-side packet translation for protocol 756 (Minecraft 1.17.1).
//!
//! This host deliberately has its own packet table, canonical-state inverse,
//! dimension entry and chunk framing. Protocol 758 changes the join and chunk
//! layout, so it must add a separate implementation rather than sharing this
//! one by protocol range.

use lodestone_core::{Ctx, Decode, Encode, Nbt, Reader, State, Writer, encode_body, write_named_nbt};
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, ItemComponents, ItemStack, Rotation, Text, Vec3, Vec3f,
};
use lodestone_server::{ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol};
use lodestone_world::{Heightmap, LongArrayFraming, PaletteKind, PalettedContainer};
use uuid::Uuid;

use crate::PROTOCOL_1_17_1;
use crate::adapter::PROTOCOL_1_18_2;
use crate::canonical::{wire_state_for_756, wire_state_for_758};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packet_ids::{handshaking, login, play};
use crate::packet_ids_758::{handshaking as handshaking_758, login as login_758, play as play_758};
use crate::packets::game::{
    BlockDig, BlockPlace, ClientCommand, ClientboundChat, ClientboundPositionLook, JoinGame,
    ServerboundChat, ServerboundArmAnimation, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, Respawn, UpdateHealth,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};
use crate::packets::window::{
    OpenWindow, ServerboundCloseWindow, ServerboundHeldItemSlot, SetSlot, WindowClick,
    WindowItems,
};
use crate::registry;

const CTX: Ctx = Ctx {
    version: PROTOCOL_1_17_1,
};
const CTX_758: Ctx = Ctx {
    version: PROTOCOL_1_18_2,
};
const COMPRESSION_THRESHOLD: i32 = 256;
const MIN_Y: i32 = 0;
const HEIGHT: i32 = 256;
const SECTION_EDGE: i32 = 16;
const SECTION_COUNT: usize = 16;
const SECTION_BLOCKS: usize = 4096;
const PLAINS_BIOME_ID: i32 = 1;
const MODERN_MIN_Y: i32 = -64;
const MODERN_HEIGHT: i32 = 384;
const MODERN_SECTION_COUNT: usize = 24;

/// Server implementation for protocol 756.
#[derive(Clone, Copy, Debug, Default)]
pub struct V756ServerProtocol;

/// Server implementation for protocol 758.
#[derive(Clone, Copy, Debug, Default)]
pub struct V758ServerProtocol;

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-756 packet must encode"),
    }
}

fn send_758<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX_758).expect("fixed protocol-758 packet must encode"),
    }
}

/// Wraps a plain server message in the JSON text object this era's clientbound
/// chat body expects. Decoration has already happened before this boundary, so
/// only JSON string escaping belongs here.
fn system_chat_text(message: &str) -> String {
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

fn system_chat(message: &str) -> ClientboundChat {
    ClientboundChat {
        message: system_chat_text(message),
        // This is regular chat history, not an action-bar overlay.
        position: 1,
        sender: Uuid::nil(),
    }
}

fn json_text_component(message: &Text) -> String {
    system_chat_text(&message.to_plain_string())
}

fn respawn_info(min_y: i32, height: i32) -> Respawn {
    Respawn {
        dimension: dimension_type(min_y, height),
        world_name: "minecraft:overworld".to_owned(),
        hashed_seed: 0,
        game_mode: 0,
        previous_game_mode: u8::MAX,
        is_debug: false,
        is_flat: true,
        copy_metadata: false,
    }
}

fn decode_full<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

fn decode_full_758<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX_758).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

fn decode_item_slot(
    protocol: i32,
    slot: crate::packets::slot::Slot,
) -> Result<Option<ItemStack>, ()> {
    let crate::packets::slot::Slot::Item { id, count, nbt } = slot else {
        return Ok(None);
    };
    if nbt.is_some() {
        return Err(());
    }
    let count = u32::try_from(count).map_err(|_| ())?;
    if count == 0 {
        return Err(());
    }
    Ok(Some(ItemStack {
        item: registry::item(protocol, id).ok_or(())?,
        count,
        components: ItemComponents::default(),
    }))
}

fn encode_item_slot(protocol: i32, item: Option<&ItemStack>) -> crate::packets::slot::Slot {
    let Some(item) = item else {
        return crate::packets::slot::Slot::Empty;
    };
    let id = registry::item_id(protocol, &item.item)
        .unwrap_or_else(|| panic!("protocol-{protocol} has no numeric item id for {}", item.item));
    let count = i8::try_from(item.count)
        .unwrap_or_else(|_| panic!("container item count {} does not fit i8", item.count));
    assert!(count > 0, "container item count must be positive");
    assert_eq!(item.components, ItemComponents::default(), "legacy container slots cannot carry components");
    crate::packets::slot::Slot::Item { id, count, nbt: None }
}

fn decode_container_click(protocol: i32, packet: WindowClick) -> Option<ServerBound> {
    let changed_slots = packet
        .changed_slots
        .into_iter()
        .map(|change| {
            Some((
                i32::from(change.location),
                decode_item_slot(protocol, change.item).ok()?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let carried_item = decode_item_slot(protocol, packet.cursor_item).ok()?;
    Some(ServerBound::ContainerClicked {
        window_id: i32::from(packet.window_id),
        state_id: packet.state_id,
        slot: i32::from(packet.slot),
        button: packet.mouse_button,
        click_type: packet.mode,
        changed_slots,
        carried_item,
    })
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
    wire_state_for_756(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no unique exact protocol-756 representation"
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
                .map_or(0, |y| u32::try_from(y + 1).expect("height is non-negative"));
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

fn dimension_type(min_y: i32, height: i32) -> Vec<u8> {
    let nbt = Nbt::Compound(vec![
        ("min_y".to_owned(), Nbt::Int(min_y)),
        ("height".to_owned(), Nbt::Int(height)),
    ]);
    let mut writer = Writer::default();
    write_named_nbt(&mut writer, "", &nbt).expect("fixed dimension entry must encode");
    writer.into_vec()
}

fn dimension_codec() -> Vec<u8> {
    let mut writer = Writer::default();
    write_named_nbt(&mut writer, "", &Nbt::Compound(Vec::new()))
        .expect("fixed dimension codec must encode");
    writer.into_vec()
}

fn encode_chunk_body(cx: i32, cz: i32, column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol-756 column bounds overflow"));
    };
    if column.min_y > MIN_Y || column_end < MIN_Y + HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-756 requires columns covering y={MIN_Y} through y={}",
            MIN_Y + HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-756 chunk block entities are not implemented",
        ));
    }
    for qy in 0..usize::try_from(HEIGHT / 4).expect("fixed biome layers") {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-756 representation",
                        column.biome_cell(qx, qy, qz)
                    )));
                }
            }
        }
    }

    let air = lodestone_data::block_states::air_state_id();
    let kind = PaletteKind::block_states().with_framing(LongArrayFraming::Prefixed);
    let mut mask = 0_u64;
    let mut sections = Writer::default();
    for section in 0..SECTION_COUNT {
        let y_base = MIN_Y + i32::try_from(section).expect("section fits i32") * SECTION_EDGE;
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
        let wire_states: Result<Vec<u32>, _> = states.iter().copied().map(wire_state).collect();
        PalettedContainer::from_values(kind, &wire_states?).encode(&mut sections);
        mask |= 1_u64 << section;
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    if mask == 0 {
        packet.var_i32(0);
    } else {
        packet.var_i32(1);
        packet.i64(mask as i64);
    }
    packet.bytes(&encode_heightmaps(column)?);
    packet.var_i32(1024);
    for _ in 0..1024 {
        packet.var_i32(PLAINS_BIOME_ID);
    }
    packet
        .var_bytes(&sections.into_vec())
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    Ok(packet.into_vec())
}

impl V756ServerProtocol {
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
        payload.var_i32(i32::try_from(wire).expect("protocol-756 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V756ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                let next_state = if handshake.protocol_version == PROTOCOL_1_17_1 {
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
            State::Play if packet_id == play::serverbound::CLIENT_COMMAND => {
                decode_full::<ClientCommand>(payload).map_or(ServerBound::Ignored, |command| {
                    ServerBound::ClientCommand { action: command.action }
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
            State::Play if packet_id == play::serverbound::BLOCK_PLACE => {
                let Some(BlockPlace {
                    hand,
                    location: Position(pos),
                    direction,
                    cursor_x,
                    cursor_y,
                    cursor_z,
                    inside_block: _,
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
                    // This era predates the block-prediction sequence, so the
                    // consumer receives the only unambiguous sentinel.
                    sequence: lodestone_model::PredictionSequence::INITIAL.as_wire(),
                    hand,
                }
            }
            State::Play if packet_id == play::serverbound::CHAT => {
                decode_full::<ServerboundChat>(payload).map_or(ServerBound::Ignored, |chat| {
                    // This era's one-string chat body predates the signed
                    // fields. Keep that absence explicit at the shared
                    // broadcast boundary rather than manufacturing a value.
                    ServerBound::Chat {
                        message: chat.message,
                        timestamp_millis: 0,
                        salt: 0,
                        signature: None,
                    }
                })
            }
            State::Play if packet_id == play::serverbound::ARM_ANIMATION => {
                let Some(hand) = decode_full::<ServerboundArmAnimation>(payload)
                    .and_then(|packet| lodestone_model::Hand::from_wire_ordinal(packet.hand))
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::Swing { hand }
            }
            State::Play if packet_id == play::serverbound::HELD_ITEM_SLOT => {
                let Some(slot) = decode_full::<ServerboundHeldItemSlot>(payload)
                    .and_then(|packet| u8::try_from(packet.slot).ok())
                    .filter(|&slot| slot < 9)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::CarriedItemChanged { slot }
            }
            State::Play if packet_id == play::serverbound::WINDOW_CLICK => {
                decode_full::<WindowClick>(payload)
                    .and_then(|packet| decode_container_click(PROTOCOL_1_17_1, packet))
                    .unwrap_or(ServerBound::Ignored)
            }
            State::Play if packet_id == play::serverbound::CLOSE_WINDOW => {
                decode_full::<ServerboundCloseWindow>(payload).map_or(ServerBound::Ignored, |packet| {
                    ServerBound::ContainerClosed { window_id: i32::from(packet.window_id) }
                })
            }
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                decode_full::<KeepAliveResponse>(payload).map_or(ServerBound::Ignored, |response| {
                    ServerBound::KeepAlive { id: response.id }
                })
            }
            // The four movement forms have distinct payloads.  In particular,
            // only the first two carry a position, which is what drives the
            // integrated server's view recentering and tick-area publication.
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
                decode_full::<ServerboundLook>(payload).map_or(ServerBound::Ignored, |look| {
                    ServerBound::PlayerRotated {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        on_ground: look.on_ground,
                    }
                })
            }
            State::Play if packet_id == play::serverbound::FLYING => {
                decode_full::<ServerboundFlying>(payload).map_or(ServerBound::Ignored, |status| {
                    ServerBound::PlayerStatusOnly {
                        on_ground: status.on_ground,
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
                    uuid,
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
                    dimension: dimension_type(MIN_Y, HEIGHT),
                    world_name: "minecraft:overworld".to_owned(),
                    hashed_seed: 0,
                    max_players: 20,
                    view_distance: view_radius,
                    simulation_distance: view_radius,
                    reduced_debug_info: false,
                    enable_respawn_screen: true,
                    is_debug: false,
                    is_flat: true,
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
                    dismount_vehicle: false,
                },
            ),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-756 column")
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

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        send(play::clientbound::CHAT, &system_chat(message))
    }

    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        send(
            play::clientbound::UPDATE_HEALTH,
            &UpdateHealth {
                health: health.clamp(0.0, 20.0),
                food: food.clamp(0, 20),
                food_saturation: saturation.clamp(0.0, 20.0),
            },
        )
    }

    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(player_entity_id);
        payload.i32(-1);
        payload.string(&json_text_component(message));
        ServerDirective::Send {
            packet_id: play::clientbound::DEATH_COMBAT_EVENT,
            payload: payload.into_vec(),
        }
    }

    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        self.encode_respawn_with_teleport_id(0, spawn)
    }

    fn encode_respawn_with_teleport_id(
        &self,
        teleport_id: i32,
        spawn: Vec3,
    ) -> Vec<ServerDirective> {
        vec![
            send(
                play::clientbound::RESPAWN,
                &respawn_info(MIN_Y, HEIGHT),
            ),
            send(
                play::clientbound::POSITION,
                &ClientboundPositionLook {
                    x: spawn.x,
                    y: spawn.y,
                    z: spawn.z,
                    yaw: 0.0,
                    pitch: 0.0,
                    flags: 0,
                    teleport_id,
                    dismount_vehicle: false,
                },
            ),
        ]
    }

    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        let menu = menu.parse().expect("container menu must be a resource key");
        let inventory_type = registry::menu_id(PROTOCOL_1_17_1, &menu)
            .unwrap_or_else(|| panic!("protocol-756 has no menu registry id for {menu}"));
        send(
            play::clientbound::OPEN_WINDOW,
            &OpenWindow {
                window_id,
                inventory_type,
                window_title: system_chat_text(title),
            },
        )
    }

    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        let items = items
            .iter()
            .map(|item| encode_item_slot(PROTOCOL_1_17_1, item.as_ref()))
            .collect();
        send(
            play::clientbound::WINDOW_ITEMS,
            &WindowItems {
                window_id: u8::try_from(window_id).expect("protocol-756 window id must fit u8"),
                state_id,
                items,
                carried_item: encode_item_slot(PROTOCOL_1_17_1, carried),
            },
        )
    }

    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        send(
            play::clientbound::SET_SLOT,
            &SetSlot {
                window_id: i8::try_from(window_id).expect("protocol-756 window id must fit i8"),
                state_id,
                slot: i16::try_from(slot).expect("protocol-756 slot must fit i16"),
                item: encode_item_slot(PROTOCOL_1_17_1, item),
            },
        )
    }

    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        let window_id = u8::try_from(window_id).expect("protocol-756 window id must fit u8");
        let property = i16::try_from(property).expect("protocol-756 property must fit i16");
        let value = i16::try_from(value).expect("protocol-756 value must fit i16");
        let mut payload = Writer::default();
        payload.u8(window_id);
        payload.i16(property);
        payload.i16(value);
        ServerDirective::Send { packet_id: play::clientbound::CRAFT_PROGRESS_BAR, payload: payload.into_vec() }
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

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        send(play::clientbound::KEEP_ALIVE, &KeepAliveRequest { id })
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-756 state")
    }
}

fn wire_state_758(canonical: u32) -> Result<u32, ChunkEncodeError> {
    wire_state_for_758(canonical).ok_or_else(|| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no unique exact protocol-758 representation"
        ))
    })
}

fn encode_heightmaps_758(column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let mut heightmap = Heightmap::new(MODERN_HEIGHT as u32);
    let air = lodestone_data::block_states::air_state_id();
    for z in 0..16usize {
        for x in 0..16usize {
            let height = (MODERN_MIN_Y..MODERN_MIN_Y + MODERN_HEIGHT)
                .rev()
                .find(|&y| column.block_state_id(x as i32, y, z as i32) != air)
                .map_or(0, |y| {
                    u32::try_from(y - MODERN_MIN_Y + 1).expect("height is non-negative")
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

fn encode_container_758(writer: &mut Writer, kind: PaletteKind, values: &[u32]) -> bool {
    let container = PalettedContainer::from_values(kind, values);
    let single = container.is_single();
    container.encode(writer);
    if single {
        // Protocol 758 retains the zero long-array count after a single-value
        // container. Its decoder consumes that byte explicitly before parsing
        // the following container.
        writer.var_i32(0);
    }
    single
}

fn encode_chunk_body_758(
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol-758 column bounds overflow"));
    };
    if column.min_y > MODERN_MIN_Y || column_end < MODERN_MIN_Y + MODERN_HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol-758 requires columns covering y={MODERN_MIN_Y} through y={}",
            MODERN_MIN_Y + MODERN_HEIGHT - 1
        )));
    }
    if !column.block_entities().is_empty() {
        return Err(ChunkEncodeError::new(
            "protocol-758 chunk block entities are not implemented",
        ));
    }
    for qy in 0..usize::try_from(MODERN_HEIGHT / 4).expect("fixed biome layers") {
        for qz in 0..4 {
            for qx in 0..4 {
                if column.biome_cell(qx, qy, qz) != "minecraft:plains" {
                    return Err(ChunkEncodeError::new(format!(
                        "biome {} has no exact protocol-758 representation",
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
    for section in 0..MODERN_SECTION_COUNT {
        let y_base = MODERN_MIN_Y
            + i32::try_from(section).expect("section fits i32") * SECTION_EDGE;
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
        let wire_states: Result<Vec<u32>, _> = states.iter().copied().map(wire_state_758).collect();
        if encode_container_758(&mut sections, block_kind, &wire_states?) {
            trailing_padding += 1;
        }
        let _ = encode_container_758(&mut sections, biome_kind, &biome_values);
    }
    for _ in 0..trailing_padding {
        sections.u8(0);
    }

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bytes(&encode_heightmaps_758(column)?);
    packet
        .var_bytes(&sections.into_vec())
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    packet.var_i32(0);
    // Empty masks and arrays mean no light layer is supplied. The client
    // retains its normal missing-light handling for this initial column.
    packet.bool(false);
    for _ in 0..4 {
        packet.var_i32(0);
    }
    packet.var_i32(0);
    packet.var_i32(0);
    Ok(packet.into_vec())
}

impl V758ServerProtocol {
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
        let wire = wire_state_758(canonical)?;
        let mut payload = Writer::default();
        payload.i64(pack_position(BlockPos::new(x, y, z)));
        payload.var_i32(i32::try_from(wire).expect("protocol-758 state fits in i32"));
        Ok(ServerDirective::Send {
            packet_id: play_758::clientbound::BLOCK_CHANGE,
            payload: payload.into_vec(),
        })
    }
}

impl ServerProtocol for V758ServerProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == handshaking_758::serverbound::SET_PROTOCOL => {
                let Some(handshake) = decode_full_758::<SetProtocol>(payload) else {
                    return ServerBound::Ignored;
                };
                let next_state = if handshake.protocol_version == PROTOCOL_1_18_2 {
                    if handshake.next_state == 2 { State::Login } else { State::Status }
                } else {
                    return ServerBound::Ignored;
                };
                ServerBound::Handshake { next_state }
            }
            State::Login if packet_id == login_758::serverbound::LOGIN_START => {
                decode_full_758::<LoginStart>(payload).map_or(ServerBound::Ignored, |start| {
                    ServerBound::LoginStart {
                        username: start.username,
                        uuid: Uuid::nil(),
                    }
                })
            }
            State::Play if packet_id == play_758::serverbound::CLIENT_COMMAND => {
                decode_full_758::<ClientCommand>(payload).map_or(ServerBound::Ignored, |command| {
                    ServerBound::ClientCommand { action: command.action }
                })
            }
            State::Play if packet_id == play_758::serverbound::BLOCK_DIG => {
                let Some(BlockDig {
                    status,
                    location: Position(pos),
                    face,
                }) = decode_full_758(payload)
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
            State::Play if packet_id == play_758::serverbound::BLOCK_PLACE => {
                let Some(BlockPlace {
                    hand,
                    location: Position(pos),
                    direction,
                    cursor_x,
                    cursor_y,
                    cursor_z,
                    inside_block: _,
                }) = decode_full_758(payload)
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
                    sequence: lodestone_model::PredictionSequence::INITIAL.as_wire(),
                    hand,
                }
            }
            State::Play if packet_id == play_758::serverbound::CHAT => {
                decode_full_758::<ServerboundChat>(payload).map_or(
                    ServerBound::Ignored,
                    |chat| ServerBound::Chat {
                        message: chat.message,
                        timestamp_millis: 0,
                        salt: 0,
                        signature: None,
                    },
                )
            }
            State::Play if packet_id == play_758::serverbound::ARM_ANIMATION => {
                let Some(hand) = decode_full_758::<ServerboundArmAnimation>(payload)
                    .and_then(|packet| lodestone_model::Hand::from_wire_ordinal(packet.hand))
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::Swing { hand }
            }
            State::Play if packet_id == play_758::serverbound::HELD_ITEM_SLOT => {
                let Some(slot) = decode_full_758::<ServerboundHeldItemSlot>(payload)
                    .and_then(|packet| u8::try_from(packet.slot).ok())
                    .filter(|&slot| slot < 9)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::CarriedItemChanged { slot }
            }
            State::Play if packet_id == play_758::serverbound::WINDOW_CLICK => {
                decode_full_758::<WindowClick>(payload)
                    .and_then(|packet| decode_container_click(PROTOCOL_1_18_2, packet))
                    .unwrap_or(ServerBound::Ignored)
            }
            State::Play if packet_id == play_758::serverbound::CLOSE_WINDOW => {
                decode_full_758::<ServerboundCloseWindow>(payload).map_or(ServerBound::Ignored, |packet| {
                    ServerBound::ContainerClosed { window_id: i32::from(packet.window_id) }
                })
            }
            State::Play if packet_id == play_758::serverbound::KEEP_ALIVE => {
                decode_full_758::<KeepAliveResponse>(payload).map_or(ServerBound::Ignored, |response| {
                    ServerBound::KeepAlive { id: response.id }
                })
            }
            State::Play if packet_id == play_758::serverbound::POSITION => {
                decode_full_758::<ServerboundPosition>(payload).map_or(
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
            State::Play if packet_id == play_758::serverbound::POSITION_LOOK => {
                decode_full_758::<ServerboundPositionLook>(payload).map_or(
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
            State::Play if packet_id == play_758::serverbound::LOOK => {
                decode_full_758::<ServerboundLook>(payload).map_or(
                    ServerBound::Ignored,
                    |look| ServerBound::PlayerRotated {
                        yaw: look.yaw,
                        pitch: look.pitch,
                        on_ground: look.on_ground,
                    },
                )
            }
            State::Play if packet_id == play_758::serverbound::FLYING => {
                decode_full_758::<ServerboundFlying>(payload).map_or(
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
            send_758(
                login_758::clientbound::COMPRESS,
                &SetCompression {
                    threshold: COMPRESSION_THRESHOLD,
                },
            ),
            ServerDirective::SetCompression(COMPRESSION_THRESHOLD),
            send_758(
                login_758::clientbound::SUCCESS,
                &LoginSuccess {
                    uuid,
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

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        vec![
            send_758(
                play_758::clientbound::LOGIN,
                &JoinGame {
                    entity_id: 1,
                    is_hardcore: false,
                    game_mode: 0,
                    previous_game_mode: -1,
                    world_names: vec!["minecraft:overworld".to_owned()],
                    dimension_codec: dimension_codec(),
                    dimension: dimension_type(MODERN_MIN_Y, MODERN_HEIGHT),
                    world_name: "minecraft:overworld".to_owned(),
                    hashed_seed: 0,
                    max_players: 20,
                    view_distance: view_radius,
                    simulation_distance: view_radius,
                    reduced_debug_info: false,
                    enable_respawn_screen: true,
                    is_debug: false,
                    is_flat: true,
                },
            ),
            send_758(
                play_758::clientbound::POSITION,
                &ClientboundPositionLook {
                    x: 8.0,
                    y: 100.0,
                    z: 8.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    flags: 0,
                    teleport_id: 0,
                    dismount_vehicle: false,
                },
            ),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-758 column")
    }

    fn try_encode_chunk(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
    ) -> Result<ServerDirective, ChunkEncodeError> {
        Ok(ServerDirective::Send {
            packet_id: play_758::clientbound::MAP_CHUNK,
            payload: encode_chunk_body_758(cx, cz, column)?,
        })
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        send_758(play_758::clientbound::CHAT, &system_chat(message))
    }

    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        send_758(
            play_758::clientbound::UPDATE_HEALTH,
            &UpdateHealth {
                health: health.clamp(0.0, 20.0),
                food: food.clamp(0, 20),
                food_saturation: saturation.clamp(0.0, 20.0),
            },
        )
    }

    fn encode_player_combat_kill(&self, player_entity_id: i32, message: &Text) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(player_entity_id);
        payload.i32(-1);
        payload.string(&json_text_component(message));
        ServerDirective::Send {
            packet_id: play_758::clientbound::DEATH_COMBAT_EVENT,
            payload: payload.into_vec(),
        }
    }

    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        self.encode_respawn_with_teleport_id(0, spawn)
    }

    fn encode_respawn_with_teleport_id(
        &self,
        teleport_id: i32,
        spawn: Vec3,
    ) -> Vec<ServerDirective> {
        vec![
            send_758(
                play_758::clientbound::RESPAWN,
                &respawn_info(MODERN_MIN_Y, MODERN_HEIGHT),
            ),
            send_758(
                play_758::clientbound::POSITION,
                &ClientboundPositionLook {
                    x: spawn.x,
                    y: spawn.y,
                    z: spawn.z,
                    yaw: 0.0,
                    pitch: 0.0,
                    flags: 0,
                    teleport_id,
                    dismount_vehicle: false,
                },
            ),
        ]
    }

    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        let menu = menu.parse().expect("container menu must be a resource key");
        let inventory_type = registry::menu_id(PROTOCOL_1_18_2, &menu)
            .unwrap_or_else(|| panic!("protocol-758 has no menu registry id for {menu}"));
        send_758(
            play_758::clientbound::OPEN_WINDOW,
            &OpenWindow {
                window_id,
                inventory_type,
                window_title: system_chat_text(title),
            },
        )
    }

    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        let items = items
            .iter()
            .map(|item| encode_item_slot(PROTOCOL_1_18_2, item.as_ref()))
            .collect();
        send_758(
            play_758::clientbound::WINDOW_ITEMS,
            &WindowItems {
                window_id: u8::try_from(window_id).expect("protocol-758 window id must fit u8"),
                state_id,
                items,
                carried_item: encode_item_slot(PROTOCOL_1_18_2, carried),
            },
        )
    }

    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        send_758(
            play_758::clientbound::SET_SLOT,
            &SetSlot {
                window_id: i8::try_from(window_id).expect("protocol-758 window id must fit i8"),
                state_id,
                slot: i16::try_from(slot).expect("protocol-758 slot must fit i16"),
                item: encode_item_slot(PROTOCOL_1_18_2, item),
            },
        )
    }

    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        let window_id = u8::try_from(window_id).expect("protocol-758 window id must fit u8");
        let property = i16::try_from(property).expect("protocol-758 property must fit i16");
        let value = i16::try_from(value).expect("protocol-758 value must fit i16");
        let mut payload = Writer::default();
        payload.u8(window_id);
        payload.i16(property);
        payload.i16(value);
        ServerDirective::Send { packet_id: play_758::clientbound::CRAFT_PROGRESS_BAR, payload: payload.into_vec() }
    }

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(entity_id);
        payload.u8(action);
        ServerDirective::Send {
            packet_id: play_758::clientbound::ANIMATION,
            payload: payload.into_vec(),
        }
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        send_758(play_758::clientbound::KEEP_ALIVE, &KeepAliveRequest { id })
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-758 state")
    }
}
