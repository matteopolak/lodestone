//! Server-side protocol-404 packet translation.
//!
//! The protocol-404 state space is flat but not canonical. This host reverses
//! only the committed per-family state table and rejects a canonical state
//! without one unique wire value.

use std::collections::BTreeMap;

use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, ItemStack, Rotation, Vec3f};
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

use crate::PROTOCOL;
use crate::canonical::wire_state_for;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::common::{KeepAliveRequest, KeepAliveResponse};
use crate::packets::game::{
    BlockDig, BlockPlace, ClientboundChat, ClientboundPositionLook, JoinGame,
    ServerboundArmAnimation, ServerboundChat, ServerboundFlying, ServerboundLook,
    ServerboundPosition, ServerboundPositionLook, TeleportConfirm,
};
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess, SetCompression};
use crate::packets::position::{Position, pack_position};
use crate::packets::settings::Settings;
use crate::packets::window::{ServerboundCloseWindow, ServerboundHeldItemSlot, WindowClick};

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

/// Lifts protocol 404's three `use_entity` wire forms into the shared server
/// interaction inputs. The first two VarInts select the target and action;
/// attack has no fields after them, plain interaction appends a hand, and
/// interact-at appends three hit coordinates followed by a hand. The precise
/// coordinates are consumed and validated even though the current server
/// interaction model has no part-specific target, so a transposed or trailing
/// field cannot reach the mob consumer.
fn entity_use(payload: &[u8]) -> ServerBound {
    let mut reader = Reader::new(payload);
    let Ok(target) = reader.var_i32() else {
        return ServerBound::Ignored;
    };
    let Ok(mouse) = reader.var_i32() else {
        return ServerBound::Ignored;
    };
    match mouse {
        1 if reader.ensure_empty().is_ok() => ServerBound::Attack { entity_id: target },
        0 => {
            let Ok(hand) = reader.var_i32() else {
                return ServerBound::Ignored;
            };
            if lodestone_model::Hand::from_wire_ordinal(hand).is_none()
                || reader.ensure_empty().is_err()
            {
                return ServerBound::Ignored;
            }
            ServerBound::InteractEntity {
                entity_id: target,
                hand,
                using_secondary_action: false,
            }
        }
        2 => {
            let Ok(_x) = reader.f32() else {
                return ServerBound::Ignored;
            };
            let Ok(_y) = reader.f32() else {
                return ServerBound::Ignored;
            };
            let Ok(_z) = reader.f32() else {
                return ServerBound::Ignored;
            };
            let Ok(hand) = reader.var_i32() else {
                return ServerBound::Ignored;
            };
            if lodestone_model::Hand::from_wire_ordinal(hand).is_none()
                || reader.ensure_empty().is_err()
            {
                return ServerBound::Ignored;
            }
            ServerBound::InteractEntity {
                entity_id: target,
                hand,
                using_secondary_action: false,
            }
        }
        _ => ServerBound::Ignored,
    }
}

/// Wraps server-provided plain text in the JSON component carried by this
/// era's ordinary chat packet.
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

/// Lifts protocol 404's packed-position block-use body into the shared
/// placement consumer. This era carries position before direction and hand,
/// has no inline item stack, and predates prediction sequences; the server
/// resolves the held item and receives sequence zero.
fn block_use(
    BlockPlace {
        location: Position(pos),
        direction,
        hand,
        cursor_x,
        cursor_y,
        cursor_z,
    }: BlockPlace,
) -> ServerBound {
    let (Ok(direction), Ok(hand)) = (i8::try_from(direction), u8::try_from(hand)) else {
        return ServerBound::Ignored;
    };
    let Some(face) = block_face(direction) else {
        return ServerBound::Ignored;
    };
    if hand > 1 {
        return ServerBound::Ignored;
    }
    ServerBound::UseItemOn {
        pos,
        face,
        cursor: Vec3f::new(cursor_x, cursor_y, cursor_z),
        sequence: 0,
        hand,
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
            State::Play if packet_id == play::serverbound::BLOCK_PLACE => {
                decode_full::<BlockPlace>(payload).map_or(ServerBound::Ignored, block_use)
            }
            State::Play if packet_id == play::serverbound::CHAT => {
                decode_full::<ServerboundChat>(payload).map_or(ServerBound::Ignored, |chat| {
                    ServerBound::Chat {
                        message: chat.message,
                        timestamp_millis: 0,
                        salt: 0,
                        signature: None,
                    }
                })
            }
            // Protocol 404 keeps attack and both right-click forms in one
            // `use_entity` packet. Dispatch on its action VarInt before
            // decoding the action-specific tail; accepting one fixed struct
            // here would either reject valid interact-at frames or reinterpret
            // their hit coordinates as a hand.
            State::Play if packet_id == play::serverbound::USE_ENTITY => entity_use(payload),
            // `arm_animation` is a one-field packet, but it is a visible
            // multiplayer action: the shared host appends it to its broadcast
            // feed and every other connection renders the matching clientbound
            // `animation`. Restrict the hand ordinal here so malformed input
            // cannot become an arbitrary animation action downstream.
            State::Play if packet_id == play::serverbound::ARM_ANIMATION => {
                match decode_full::<ServerboundArmAnimation>(payload) {
                    Some(ServerboundArmAnimation { hand }) => lodestone_model::Hand::from_wire_ordinal(hand)
                        .map_or(ServerBound::Ignored, |hand| ServerBound::Swing { hand }),
                    _ => ServerBound::Ignored,
                }
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
                let Some(WindowClick {
                    window_id,
                    slot,
                    button,
                    mode,
                    item: _,
                    action: _,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                if !(0..=6).contains(&mode) {
                    return ServerBound::Ignored;
                }
                ServerBound::ContainerClicked {
                    window_id: i32::from(window_id),
                    state_id: 0,
                    slot: i32::from(slot),
                    button,
                    click_type: i32::from(mode),
                    changed_slots: Vec::new(),
                    carried_item: None,
                }
            }
            State::Play if packet_id == play::serverbound::CLOSE_WINDOW => {
                decode_full::<ServerboundCloseWindow>(payload).map_or(
                    ServerBound::Ignored,
                    |close| ServerBound::ContainerClosed {
                        window_id: i32::from(close.window_id),
                    },
                )
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

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(entity_id);
        payload.u8(action);
        ServerDirective::Send {
            packet_id: play::clientbound::ANIMATION,
            payload: payload.into_vec(),
        }
    }

    /// Protocol 404's passenger update is a VarInt vehicle id followed by a
    /// VarInt count and that many VarInt passenger ids. This is the visible
    /// result of mounting through the shared `MobSim::interact` consumer.
    fn encode_set_passengers(&self, vehicle_id: i32, passenger_ids: &[i32]) -> ServerDirective {
        let mut payload = Writer::default();
        payload.var_i32(vehicle_id);
        payload.var_i32(i32::try_from(passenger_ids.len()).unwrap_or(i32::MAX));
        for &passenger_id in passenger_ids {
            payload.var_i32(passenger_id);
        }
        ServerDirective::Send {
            packet_id: play::clientbound::SET_PASSENGERS,
            payload: payload.into_vec(),
        }
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

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        send(
            play::clientbound::CHAT,
            &ClientboundChat {
                message: legacy_text_component(message),
                position: 1,
            },
        )
    }

    fn encode_open_screen(&self, window_id: i32, menu: &str, title: &str) -> ServerDirective {
        let Ok(window_id) = u8::try_from(window_id) else {
            return ServerDirective::None;
        };
        let (inventory_type, slot_count) = match menu {
            "minecraft:generic_9x3" => ("minecraft:chest", 27),
            "minecraft:generic_3x3" => ("minecraft:dispenser", 9),
            "minecraft:furnace" => ("minecraft:furnace", 3),
            "minecraft:hopper" => ("minecraft:hopper", 5),
            "minecraft:beacon" => ("minecraft:beacon", 1),
            other => (other, 0),
        };
        send(
            play::clientbound::OPEN_WINDOW,
            &crate::packets::window::OpenWindow {
                window_id,
                inventory_type: inventory_type.to_owned(),
                window_title: legacy_text_component(title),
                slot_count,
                entity_id: None,
            },
        )
    }

    fn encode_container_content(
        &self,
        window_id: i32,
        _state_id: i32,
        items: &[Option<ItemStack>],
        _carried: Option<&ItemStack>,
    ) -> ServerDirective {
        let Ok(window_id) = u8::try_from(window_id) else {
            return ServerDirective::None;
        };
        let items = items.iter().map(|item| legacy_slot(item.as_ref())).collect();
        send(
            play::clientbound::WINDOW_ITEMS,
            &crate::packets::window::WindowItems { window_id, items },
        )
    }

    fn encode_container_slot(
        &self,
        window_id: i32,
        _state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        let (Ok(window_id), Ok(slot)) = (i8::try_from(window_id), i16::try_from(slot)) else {
            return ServerDirective::None;
        };
        send(
            play::clientbound::SET_SLOT,
            &crate::packets::window::SetSlot {
                window_id,
                slot,
                item: legacy_slot(item),
            },
        )
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        send(play::clientbound::KEEP_ALIVE, &KeepAliveRequest { id })
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-404 state")
    }
}

fn legacy_slot(item: Option<&ItemStack>) -> crate::packets::slot::Slot {
    let Some(item) = item.filter(|item| item.count > 0) else {
        return crate::packets::slot::Slot::Empty;
    };
    let Some((id, _)) = crate::generated_item_types::ITEM_TYPES
        .iter()
        .find(|(_, name)| *name == item.item.to_string())
    else {
        return crate::packets::slot::Slot::Empty;
    };
    let (Ok(id), Ok(count)) = (i32::try_from(*id), i8::try_from(item.count)) else {
        return crate::packets::slot::Slot::Empty;
    };
    crate::packets::slot::Slot::Item {
        id,
        count,
        nbt: None,
    }
}
