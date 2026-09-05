//! Server-side protocol-5 packet translation.
//!
//! Protocol 5 keeps its chunk encoding local: each column is a zlib stream of
//! separately grouped type, metadata, and light arrays rather than a later
//! era's flat or paletted section representation.

use std::io::Write as _;

use flate2::{Compression, write::ZlibEncoder};
use lodestone_canonical::inverse;
use lodestone_core::{Ctx, Decode, Encode, Reader, State, Writer, encode_body};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos, Rotation, Vec3f};
use lodestone_server::{
    ChunkColumn, ChunkEncodeError, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

use crate::PROTOCOL;
use crate::packet_ids::{handshaking, login, play};
use crate::packets::game::{
    ClientboundChat, ClientboundPositionLook, JoinGame, KeepAliveRequest, KeepAliveResponse,
    EntityAction, ServerboundArmAnimation, ServerboundChat, ServerboundFlying,
    ServerboundLook, ServerboundPosition, ServerboundPositionLook,
};
use crate::packets::entity::Animation;
use crate::packets::handshake::SetProtocol;
use crate::packets::login::{LoginStart, LoginSuccess};
use crate::packets::settings::Settings;
use crate::packets::window::ServerboundHeldItemSlot;
use crate::packets::world::{BlockChange, BlockDig, BlockPlace};
use crate::packets::position::PositionIbi;

const CTX: Ctx = Ctx { version: PROTOCOL };
const LEGACY_MIN_Y: i32 = 0;
const LEGACY_HEIGHT: i32 = 256;
const SECTION_EDGE: i32 = 16;
const SECTION_BLOCKS: usize = 4096;
const NIBBLE_BYTES: usize = 2048;
const PLAINS_BIOME_ID: u8 = 1;
const STANDING_EYE_HEIGHT: f64 = 1.62;
/// Protocol 5's numeric block registry has a five-id gap before its final six
/// blocks: `0..=164` and `170..=175` are valid; `165..=169` are not.
const fn protocol_5_defines_block_id(block_id: u32) -> bool {
    block_id <= 164 || (block_id >= 170 && block_id <= 175)
}

/// Server implementation for protocol 5.
#[derive(Clone, Copy, Debug, Default)]
pub struct V5ServerProtocol;

struct EncodedSection {
    index: usize,
    types: Vec<u8>,
    metadata: Vec<u8>,
    add: Option<Vec<u8>>,
}

fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet, CTX).expect("fixed protocol-5 packet must encode"),
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
/// The packet itself has no system/action-bar discriminator. Escaping every
/// JSON control character keeps message text from becoming a component
/// fragment.
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

fn block_action(status: i8) -> Option<BlockActionKind> {
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

/// Converts protocol 5's signed cursor byte to its block-local coordinate.
///
/// The wire range is nominally `0..=15`, one sixteenth per unit. Invalid
/// signed values are rejected before they reach placement rather than being
/// clamped into a plausible click location.
fn cursor_coordinate(value: i8) -> Option<f32> {
    (0..=15).contains(&value).then_some(f32::from(value) / 16.0)
}

fn legacy_composite(canonical: u32) -> Result<u32, ChunkEncodeError> {
    let composite = inverse::resolve(canonical).map_err(|_| {
        ChunkEncodeError::new(format!(
            "canonical state {canonical} has no exact protocol-5 representation"
        ))
    })?;
    if !protocol_5_defines_block_id(composite >> 4) {
        return Err(ChunkEncodeError::new(format!(
            "canonical state {canonical} resolves to block id {}, which protocol 5 does not define",
            composite >> 4
        )));
    }
    Ok(composite)
}

fn set_nibble(bytes: &mut [u8], index: usize, value: u8) {
    let slot = &mut bytes[index / 2];
    if index.is_multiple_of(2) {
        *slot = (*slot & 0xF0) | (value & 0x0F);
    } else {
        *slot = (*slot & 0x0F) | ((value & 0x0F) << 4);
    }
}

fn encode_chunk_body(cx: i32, cz: i32, column: &ChunkColumn) -> Result<Vec<u8>, ChunkEncodeError> {
    let Some(column_end) = column.min_y.checked_add(column.height) else {
        return Err(ChunkEncodeError::new("protocol 5 column bounds overflow"));
    };
    if column.min_y > LEGACY_MIN_Y || column_end < LEGACY_MIN_Y + LEGACY_HEIGHT {
        return Err(ChunkEncodeError::new(format!(
            "protocol 5 requires columns covering y={LEGACY_MIN_Y} through y={}",
            LEGACY_MIN_Y + LEGACY_HEIGHT - 1
        )));
    }

    let air = lodestone_data::block_states::air_state_id();
    let mut bitmask = 0_u16;
    let mut sections = Vec::new();
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

        let mut types = vec![0; SECTION_BLOCKS];
        let mut metadata = vec![0; NIBBLE_BYTES];
        let mut add = vec![0; NIBBLE_BYTES];
        let mut has_add = false;
        for (index, state) in states.into_iter().enumerate() {
            let composite = legacy_composite(state)?;
            let block_id = composite >> 4;
            let block_id = u16::try_from(block_id).map_err(|_| {
                ChunkEncodeError::new(format!(
                    "protocol-5 block id {} exceeds its 12-bit representation",
                    composite >> 4
                ))
            })?;
            if block_id > 0x0FFF {
                return Err(ChunkEncodeError::new(format!(
                    "protocol-5 block id {block_id} exceeds its 12-bit representation"
                )));
            }
            types[index] = block_id as u8;
            set_nibble(&mut metadata, index, (composite & 0x0F) as u8);
            let high = (block_id >> 8) as u8;
            if high != 0 {
                has_add = true;
                set_nibble(&mut add, index, high);
            }
        }
        bitmask |= 1 << section;
        sections.push(EncodedSection {
            index: section,
            types,
            metadata,
            add: has_add.then_some(add),
        });
    }

    let mut inflated = Vec::new();
    for section in &sections {
        inflated.extend_from_slice(&section.types);
    }
    for section in &sections {
        inflated.extend_from_slice(&section.metadata);
    }
    for _ in &sections {
        inflated.extend_from_slice(&[0; NIBBLE_BYTES]);
    }
    for _ in &sections {
        inflated.extend_from_slice(&[u8::MAX; NIBBLE_BYTES]);
    }
    let mut add_mask = 0_u16;
    for section in &sections {
        if let Some(add) = &section.add {
            add_mask |= 1 << section.index;
            inflated.extend_from_slice(add);
        }
    }
    inflated.extend_from_slice(&[PLAINS_BIOME_ID; 256]);

    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::default());
    compressor
        .write_all(&inflated)
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    let compressed = compressor
        .finish()
        .map_err(|error| ChunkEncodeError::new(error.to_string()))?;
    let compressed_len = i32::try_from(compressed.len())
        .map_err(|_| ChunkEncodeError::new("protocol-5 compressed chunk body exceeds i32"))?;

    let mut packet = Writer::default();
    packet.i32(cx);
    packet.i32(cz);
    packet.bool(true);
    packet.u16(bitmask);
    packet.u16(add_mask);
    packet.i32(compressed_len);
    packet.bytes(&compressed);
    Ok(packet.into_vec())
}

impl V5ServerProtocol {
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
        let composite = legacy_composite(canonical)?;
        let block_type = i32::try_from(composite >> 4)
            .map_err(|_| ChunkEncodeError::new("protocol-5 block id exceeds i32"))?;
        let y = u8::try_from(y)
            .map_err(|_| ChunkEncodeError::new(format!("protocol 5 cannot encode y={y}")))?;
        Ok(send(
            play::clientbound::BLOCK_CHANGE,
            &BlockChange {
                location: PositionIbi { x, y, z },
                block_type,
                metadata: (composite & 0x0F) as u8,
            },
        ))
    }
}

impl ServerProtocol for V5ServerProtocol {
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
                    x,
                    y,
                    z,
                    face,
                }) = decode_full(payload)
                else {
                    return ServerBound::Ignored;
                };
                match status {
                    0..=2 => {
                        let (Some(action), Some(face)) = (block_action(status), block_face(face))
                        else {
                            return ServerBound::Ignored;
                        };
                        ServerBound::BlockAction {
                            action,
                            pos: BlockPos::new(x, i32::from(y), z),
                            face,
                            sequence: 0,
                        }
                    }
                    // These no-target actions share the legacy block-dig
                    // body. The adapter already emits all three statuses;
                    // preserving them here lets the version-free inventory
                    // and use-state consumers observe the corresponding keys.
                    3 => ServerBound::ItemDropped { whole_stack: true },
                    4 => ServerBound::ItemDropped { whole_stack: false },
                    5 => ServerBound::ReleaseUseItem,
                    _ => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::CHAT => {
                decode_full::<ServerboundChat>(payload).map_or(ServerBound::Ignored, |chat| {
                    // The single string serves both purposes in this era. A
                    // leading slash is not part of the command text the
                    // shared command consumer accepts.
                    if let Some(command) = chat.message.strip_prefix('/') {
                        ServerBound::ChatCommand {
                            command: command.to_owned(),
                        }
                    } else {
                        // Signed chat did not exist yet, so use the shared
                        // server's explicit unsigned legacy form rather than
                        // inventing a timestamp or signature.
                        ServerBound::Chat {
                            message: chat.message,
                            timestamp_millis: 0,
                            salt: 0,
                            signature: None,
                        }
                    }
                })
            }
            State::Play if packet_id == play::serverbound::BLOCK_PLACE => {
                let Some(BlockPlace {
                    x,
                    y,
                    z,
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
                    // The `(-1, 255, -1), -1` use-in-air sentinel has no
                    // canonical hand-independent action in this hosted era;
                    // keep it ignored until the server has one.
                    return ServerBound::Ignored;
                };
                ServerBound::UseItemOn {
                    pos: BlockPos::new(x, i32::from(y), z),
                    face,
                    cursor: Vec3f::new(cursor_x, cursor_y, cursor_z),
                    // The wire pre-dates off-hand and prediction sequences.
                    hand: 0,
                    sequence: 0,
                }
            }
            // Protocol 5 carries the sender id and an animation ordinal in
            // this request, but the host derives the sender from the
            // connection. Only ordinal 1 is the arm swing; every other
            // ordinal is a different animation and must not reach the swing
            // consumer. The body is decoded exactly so a valid request with
            // trailing bytes cannot be accepted as a second frame.
            State::Play if packet_id == play::serverbound::ARM_ANIMATION => {
                let Some(ServerboundArmAnimation { animation, .. }) = decode_full(payload) else {
                    return ServerBound::Ignored;
                };
                if animation == 1 {
                    ServerBound::Swing { hand: 0 }
                } else {
                    ServerBound::Ignored
                }
            }
            // This era's entity-action ordinals start at one. The shared
            // wake consumer represents leave-bed as action zero, so only its
            // wire ordinal three crosses this version boundary. The sender id
            // belongs to the connection and is intentionally not trusted.
            State::Play if packet_id == play::serverbound::ENTITY_ACTION => {
                let Some(EntityAction { action_id: 3, .. }) = decode_full(payload) else {
                    return ServerBound::Ignored;
                };
                ServerBound::PlayerCommand { action: 0 }
            }
            State::Play if packet_id == play::serverbound::HELD_ITEM_SLOT => {
                let Some(slot) = decode_full::<ServerboundHeldItemSlot>(payload)
                    .and_then(|packet| u8::try_from(packet.slot_id).ok())
                    .filter(|&slot| slot < 9)
                else {
                    return ServerBound::Ignored;
                };
                ServerBound::CarriedItemChanged { slot }
            }
            // Protocol 5 has no teleport-confirm packet. Its position and
            // position/look frames are both the teleport echo and ordinary
            // movement, distinguished by the server's pending-position state.
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
                        id: i64::from(response.keep_alive_id),
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
        vec![send(
            login::clientbound::SUCCESS,
            &LoginSuccess {
                uuid: uuid.to_string(),
                username: username.to_owned(),
            },
        )]
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
                },
            ),
            send(
                play::clientbound::POSITION,
                &ClientboundPositionLook {
                    x: 8.0,
                    stance: 100.0 + STANDING_EYE_HEIGHT,
                    z: 8.0,
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: false,
                },
            ),
        ]
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        self.try_encode_chunk(cx, cz, column)
            .expect("call try_encode_chunk to handle an unrepresentable protocol-5 column")
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
        let keep_alive_id = i32::try_from(id)
            .expect("protocol-5 keep-alive id must fit its signed i32 wire field");
        send(
            play::clientbound::KEEP_ALIVE,
            &KeepAliveRequest { keep_alive_id },
        )
    }

    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        send(
            play::clientbound::CHAT,
            &ClientboundChat {
                message: legacy_text_component(message),
            },
        )
    }

    fn encode_animate(&self, entity_id: i32, action: u8) -> ServerDirective {
        // Protocol 5 predates the off-hand. The shared server uses action 3
        // for an off-hand swing, but this era's client interprets that byte as
        // a critical-hit animation. Degrade that one canonical action to the
        // only honest swing representation rather than showing a false hit.
        let animation = if action == 3 { 0 } else { action };
        send(
            play::clientbound::ANIMATION,
            &Animation { entity_id, animation },
        )
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        self.try_encode_block_update(x, y, z, state)
            .expect("call try_encode_block_update to handle an unrepresentable protocol-5 state")
    }
}
