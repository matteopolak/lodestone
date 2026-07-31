//! [`V770ServerProtocol`]: the server-side mirror of [`V770Adapter`].
//!
//! Where [`V770Adapter`] lifts clientbound protocol-776 packets into the
//! version-free client model, this type does the opposite side of the same
//! wire format: it *encodes* the clientbound packets a real vanilla 26.2
//! client expects and *decodes* the serverbound packets it sends, so
//! `lodestone-server`'s [`serve_connection`](lodestone_server::serve_connection)
//! loop can drive a real `lodestone-client` end to end over the in-memory
//! transport, with no fake wire format standing in.
//!
//! # Scope
//!
//! This implements the minimum sequence needed for a client to reach
//! [`State::Play`] and receive a rendered view: handshake, login, the
//! (empty) configuration phase, the play join sequence (join game, default
//! spawn, initial teleport, chunk-cache center), `level_chunk_with_light`
//! for every column in the initial view, a post-join welcome chat, entity
//! spawn/update/remove for the mob simulation, server-initiated keep-alive
//! with a disconnect-on-timeout, time-of-day, and view streaming
//! (chunk-cache-center / forget / send) as the player moves between chunk
//! columns — the scheduling for all three lives in `lodestone-server`'s
//! `serve_play`; this module only supplies their encoders (and, for
//! keep-alive and movement, decoders) — and, since `docs/block-edit.md`,
//! decoders for the two serverbound editing packets (`player_action`'s three
//! destroy phases, `use_item_on`'s placement) plus the `block_update`
//! encoder that confirms an edit back to the acting client. See that doc for
//! what block editing does and does not cover; the wire layout here is a
//! faithful decode/encode of the real packets regardless of scope.
//!
//! # Why hand-written encoding is correct, not just convenient
//!
//! Every struct this module constructs and calls `.encode()` on already
//! derives `Decode` and is asserted against real bytes elsewhere in this
//! crate (`tests/join_flow.rs`'s golden vectors, `tests/live_chunk.rs`'s live
//! server capture). Deriving `Encode` on the same struct definition — rather
//! than hand-rolling a mirror-image encoder — is what keeps the two
//! directions from drifting apart: a field added to one is added to both.
//! The handful of packets with no existing struct (the `player_position`
//! teleport, `set_chunk_cache_center`) are written directly against
//! [`V770Adapter`]'s own decode logic for those same packets, which is the
//! best available specification for their wire layout.

use lodestone_core::{Ctx, Decode, Encode, Nbt, Reader, Writer, write_network_nbt};
use lodestone_model::{BlockActionKind, BlockFace, BlockPos};
use lodestone_server::{
    ChunkColumn as ServerChunkColumn, EntitySnapshot, ServerBound, ServerDirective, ServerProtocol,
};
use lodestone_world::{ChunkColumn as WorldChunkColumn, ChunkSection, ColumnLight, Heightmaps};
use uuid::Uuid;

use lodestone_data::block_states::{block_name, properties};
use lodestone_data::entity_types::entity_type_id;
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::chunk::ChunkShape;
use crate::packets::common::KeepAlive;
use crate::packets::configuration::FinishConfiguration;
use crate::packets::entity::{pack_degrees, write_lp_vec3};
use crate::packets::game::{
    GameLogin, GlobalPos, MovePlayerPos, MovePlayerPosRot, PlayerAction, SetDefaultSpawnPosition,
    SetHealth, UseItemOn,
};
use crate::packets::handshake::Intention;
use crate::packets::login::{LoginFinished, LoginHello};

/// The overworld world-clock's registry holder id
/// (`WorldClocks::bootstrap` registers `minecraft:overworld` first,
/// `minecraft:the_end` second — see `packets::time::ClockUpdate::holder_id`'s
/// doc comment). The only clock this crate ever anchors: the integrated
/// server always joins into the overworld (this type's own doc comment).
const OVERWORLD_CLOCK_HOLDER_ID: i32 = 0;

/// Fixed decoding/encoding context for protocol 776 (mirrors [`crate::adapter`]'s
/// own `CTX`; kept private to this module since only this file names raw
/// packet ids on the server side).
const CTX: Ctx = Ctx { version: 776 };

/// Fallback: the block-state id for `minecraft:stone`, resolved by name at
/// construction so a change to the generated table cannot silently desync
/// this from the real registry id (see `tests/server_protocol.rs`'s pinning
/// test for the non-vacuity check).
fn stone_id() -> u32 {
    // Registry id `1` is asserted to be `minecraft:stone` by
    // `tests/block_states.rs`; re-deriving it by name here (rather than the
    // bare literal) means a regenerated table that ever renumbered stone
    // would fail loudly at the lookup below instead of silently sending the
    // wrong block.
    (0..).find(|&id| block_name(id) == Some("minecraft:stone")).expect(
        "generated block-state table has no `minecraft:stone` entry — regenerate or fix the table",
    )
}

/// Fallback: the block-state id for `minecraft:air`, resolved the same way as
/// [`stone_id`] and for the same reason. Used both as
/// [`resolve_state_id`]'s no-match fallback and, indirectly, wherever this
/// module needs air's id without hardcoding registry id `0`.
fn air_id() -> u32 {
    (0..).find(|&id| block_name(id) == Some("minecraft:air")).expect(
        "generated block-state table has no `minecraft:air` entry — regenerate or fix the table",
    )
}

/// Resolves a canonical block-state string ([`ServerChunkColumn`]'s own
/// vocabulary, e.g. `"minecraft:water[level=0]"`, `"minecraft:stone"`) to its
/// protocol-776 registry id, via a linear scan matching both the block name
/// and its property values against the generated state table —
/// [`stone_id`]/[`air_id`] special-case the two propertyless states this
/// module ever *writes*; this is the general form needed for
/// [`V770ServerProtocol::encode_block_update`], which must also echo back
/// whatever pre-existing (possibly-propertied) state already occupied a
/// placement's neighbour cell.
///
/// # Falls back to air on no match
///
/// A block-update confirmation is best-effort feedback (see
/// `docs/block-edit.md`), not the server's authoritative state — that stays
/// in [`ServerChunkColumn`]'s own string form, which this function only
/// reads. If a state string this version's table cannot parse back ever
/// reaches here (a property spelling/order drift), sending air is a
/// visibly-wrong confirmation rather than a panic or a corrupted wire id.
fn resolve_state_id(state: &str) -> u32 {
    let (name, raw_props) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut wanted: Vec<(&str, &str)> = if raw_props.is_empty() {
        Vec::new()
    } else {
        raw_props
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .collect()
    };
    wanted.sort_unstable();

    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| {
            if block_name(id) != Some(name) {
                return false;
            }
            let mut have: Vec<(&str, &str)> = properties(id).unwrap_or(&[]).to_vec();
            have.sort_unstable();
            have == wanted
        })
        .unwrap_or_else(air_id)
}

/// Unpacks vanilla's `BlockPos.asLong` form (the inverse of
/// [`pack_block_pos`]): `x` in the high 26 bits, `z` in the middle 26 bits,
/// `y` in the low 12 bits, each sign-extended back out via a
/// left-then-arithmetic-right shift pair. Mirrors `V770Adapter`'s own private
/// `unpack_block_pos` exactly (kept as a local duplicate here — this module
/// already keeps its own hand-written mirrors of the adapter's `pack`/encode
/// helpers rather than sharing them across the decode/encode boundary, per
/// this file's own module doc).
fn unpack_block_pos(packed: i64) -> BlockPos {
    let x = (packed >> 38) as i32;
    let y = ((packed << 52) >> 52) as i32;
    let z = ((packed << 26) >> 38) as i32;
    BlockPos::new(x, y, z)
}

/// Maps `Direction.get3DDataValue` (`0` down … `5` east) back to a
/// [`BlockFace`] — the inverse of `V770Adapter`'s own `face_ordinal`. Any
/// value outside `0..=5` (a malformed packet) falls back to `East` rather
/// than panicking; the resulting `ServerBound` still carries a valid
/// position, so the worst case is a break/place computed against the wrong
/// face, not a dropped connection.
fn face_from_ordinal(ordinal: i32) -> BlockFace {
    match ordinal {
        0 => BlockFace::Down,
        1 => BlockFace::Up,
        2 => BlockFace::North,
        3 => BlockFace::South,
        4 => BlockFace::West,
        _ => BlockFace::East,
    }
}

/// Encodes a packet body into a fresh byte buffer.
fn encode_body<T: Encode>(packet: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    packet
        .encode(&mut writer, CTX)
        .expect("encoding a well-formed struct into a `Vec<u8>` writer cannot fail");
    writer.into_vec()
}

/// Builds a [`ServerDirective::Send`] from a packet id and an encodable body.
fn send<T: Encode>(packet_id: i32, packet: &T) -> ServerDirective {
    ServerDirective::Send {
        packet_id,
        payload: encode_body(packet),
    }
}

/// Decodes a packet body, asserting the payload was consumed to the last
/// byte. Returns `None` on any decode error or trailing bytes rather than
/// panicking: a malformed packet from the wire should drop that packet, not
/// take down the connection.
fn decode_full<T: Decode>(payload: &[u8]) -> Option<T> {
    let mut reader = Reader::new(payload);
    let value = T::decode(&mut reader, CTX).ok()?;
    reader.ensure_empty().ok()?;
    Some(value)
}

/// Packs a block position into vanilla's `BlockPos.asLong` form: `x` in the
/// high 26 bits, `z` in the middle 26 bits, `y` in the low 12 bits.
fn pack_block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38)
        | ((i64::from(z) & 0x3FF_FFFF) << 12)
        | (i64::from(y) & 0xFFF)
}

/// Hand-written encoder for the clientbound `player_position` (teleport)
/// packet, which has no existing struct in `packets::game` because it is
/// currently only ever *decoded* (see `V770Adapter::handle_player_position`).
///
/// Wire layout (mirrors the decode side exactly): VarInt teleport id, position
/// `f64`×3, delta-movement `f64`×3 (zero — an absolute teleport carries no
/// velocity), yaw/pitch `f32`, then a big-endian `i32` relative-flags bit set
/// (`0` — every field here is absolute).
fn encode_player_position_teleport(
    id: i32,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(id);
    w.f64(x);
    w.f64(y);
    w.f64(z);
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.f32(yaw);
    w.f32(pitch);
    w.i32(0);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `set_chunk_cache_center` packet:
/// two VarInt chunk coordinates, no other fields.
fn encode_chunk_cache_center_body(cx: i32, cz: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(cx);
    w.var_i32(cz);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `forget_level_chunk` packet: a
/// single packed `i64` — `x` in the low 32 bits, `z` in the high 32 — mirroring
/// vanilla's `ChunkPos.pack` exactly as `V770Adapter::handle_play`'s
/// `FORGET_LEVEL_CHUNK` decode arm already reads it (`adapter.rs`, the
/// `packed as i32` / `(packed >> 32) as i32` pair).
fn encode_forget_chunk_body(cx: i32, cz: i32) -> Vec<u8> {
    let packed = (i64::from(cx) & 0xFFFF_FFFF) | ((i64::from(cz) & 0xFFFF_FFFF) << 32);
    let mut w = Writer::default();
    w.i64(packed);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `block_update` packet: a packed
/// `BlockPos` long ([`pack_block_pos`]) followed by a VarInt block-state
/// registry id — mirrors `ClientboundBlockUpdatePacket.STREAM_CODEC`
/// (`BlockPos.STREAM_CODEC` composed with `ByteBufCodecs.idMapper(Block
/// .BLOCK_STATE_REGISTRY)`, `ClientboundBlockUpdatePacket.java:14-20`) and
/// this crate's own decode of the same packet in `V770Adapter::handle_play`'s
/// `BLOCK_UPDATE` arm (`adapter.rs`), which reads the identical
/// packed-i64-then-VarInt shape.
fn encode_block_update_body(x: i32, y: i32, z: i32, state_id: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(pack_block_pos(x, y, z));
    w.var_i32(state_id as i32);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `set_time` packet, mirroring
/// `packets::time::SetTime`'s `Decode` impl exactly (that struct has no
/// `Encode` impl to reuse — the map-valued field it decodes cannot come from
/// an existing bidirectional struct the way this module's other reused types
/// do, so a hand-written mirror is the documented fallback here, same as
/// `encode_player_position_teleport`/`encode_chunk_cache_center_body` above).
///
/// Wire layout: `i64` `game_time`, then a VarInt-counted list of clock
/// updates. `day_time` of `None` sends an empty list (vanilla's
/// once-a-second `forceGameTimeSynchronization` broadcast, which
/// deliberately leaves the client's held day/night anchor untouched — see
/// `packets::time::SetTime::day_clock`'s doc comment). `day_time` of
/// `Some(total_ticks)` sends exactly one update, anchoring the overworld
/// clock (`OVERWORLD_CLOCK_HOLDER_ID`) to `total_ticks` at the normal 1:1
/// rate: a **plain** VarInt holder id (no `+1`/inline convention — see
/// `packets::time::ClockUpdate::holder_id`'s doc comment on why that
/// differs from the *other* holder codec), a VarLong tick count, then two
/// big-endian `f32`s (partial tick `0.0`, rate `1.0`).
fn encode_set_time_body(game_time: i64, day_time: Option<i64>) -> Vec<u8> {
    let mut w = Writer::default();
    w.i64(game_time);
    match day_time {
        Some(total_ticks) => {
            w.var_i32(1);
            w.var_i32(OVERWORLD_CLOCK_HOLDER_ID);
            w.var_i64(total_ticks);
            w.f32(0.0); // partial_tick
            w.f32(1.0); // rate: normal day/night speed, never paused
        }
        None => {
            w.var_i32(0);
        }
    }
    w.into_vec()
}

/// Hand-written encoder for the clientbound `system_chat` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_play`'s `SYSTEM_CHAT` arm). Wire layout (mirrors the
/// decode side exactly): a network-form NBT text component (root tag id +
/// payload, no root name — vanilla's `ComponentSerialization.TRUSTED_STREAM_CODEC`),
/// then a big-endian `bool` overlay flag (`false` selects normal chat history,
/// `true` the action-bar overlay).
fn encode_system_chat(message: &str, overlay: bool) -> Vec<u8> {
    let component = Nbt::Compound(vec![("text".to_owned(), Nbt::String(message.to_owned()))]);
    let mut w = Writer::default();
    write_network_nbt(&mut w, &component).expect("plain string NBT component always encodes");
    w.bool(overlay);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `add_entity` packet, which has no
/// existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_add_entity`, the exact mirror of this wire layout).
///
/// Wire layout: VarInt id, UUID, VarInt entity-type id, position `f64`×3,
/// low-precision velocity ([`write_lp_vec3`]), then three signed-byte angles
/// in **pitch, yaw, head_yaw** order (note: this order is reversed from
/// `move_entity`'s yaw-then-pitch), then a trailing VarInt `data` field
/// (vanilla sends `0` for ordinary mobs).
///
/// An `entity_type` with no match in this version's registry (a typo, or a
/// key from a version this table doesn't cover) falls back to network id `0`
/// rather than failing the whole spawn — a wrong model is recoverable, a
/// dropped connection is not.
fn encode_add_entity_body(entity: &EntitySnapshot) -> Vec<u8> {
    let type_id = entity_type_id(&entity.entity_type.to_string()).unwrap_or(0);
    let mut w = Writer::default();
    w.var_i32(entity.id);
    w.uuid(entity.uuid);
    w.var_i32(type_id);
    w.f64(entity.position.x);
    w.f64(entity.position.y);
    w.f64(entity.position.z);
    write_lp_vec3(
        &mut w,
        entity.velocity.x,
        entity.velocity.y,
        entity.velocity.z,
    );
    w.i8(pack_degrees(entity.rotation.pitch));
    w.i8(pack_degrees(entity.rotation.yaw));
    w.i8(pack_degrees(entity.head_yaw));
    w.var_i32(0);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `teleport_entity` packet (the
/// entity-position analogue of [`encode_player_position_teleport`]), which has
/// no existing struct because it is currently only ever *decoded* (see
/// `V770Adapter::handle_entity_position`).
///
/// Wire layout: VarInt id, position `f64`×3, delta-movement `f64`×3 (zero —
/// an absolute update carries no velocity here; velocity travels separately
/// via `set_entity_motion`), yaw/pitch as **`f32`** (unlike `add_entity`'s
/// signed-byte angles), a trailing big-endian `i32` relative-flags bit set
/// (`0` — every field is absolute), then a `bool` on-ground flag. All mobs
/// the sim currently spawns are land-walkers (`MobShape::land`), so on-ground
/// is hardcoded `true`; `EntitySnapshot` carries no on-ground field yet to
/// derive this from.
fn encode_teleport_entity(entity: &EntitySnapshot) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity.id);
    w.f64(entity.position.x);
    w.f64(entity.position.y);
    w.f64(entity.position.z);
    w.f64(0.0);
    w.f64(0.0);
    w.f64(0.0);
    w.f32(entity.rotation.yaw);
    w.f32(entity.rotation.pitch);
    w.i32(0);
    w.bool(true);
    w.into_vec()
}

/// Hand-written encoder for the clientbound `rotate_head` packet: VarInt id
/// then one signed-byte angle ([`pack_degrees`]), the exact mirror of the
/// inline `ROTATE_HEAD` decode arm in `V770Adapter::handle_play`.
fn encode_rotate_head(entity_id: i32, head_yaw: f32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(entity_id);
    w.i8(pack_degrees(head_yaw));
    w.into_vec()
}

/// Encodes the trailing `GameLogin::rest` bytes: the spawn-info fields not
/// modelled as named struct fields (see that struct's doc comment for why).
/// None of these are consumed by `V770Adapter::handle_play`'s `LOGIN` arm, so
/// their exact values only need to be well-formed, not vanilla-authentic.
fn encode_game_login_rest() -> Vec<u8> {
    let mut w = Writer::default();
    w.i8(-1); // previous_game_type: none
    w.bool(false); // is_debug
    w.bool(false); // is_flat
    w.bool(false); // has_last_death_location
    w.var_i32(0); // portal_cooldown
    w.var_i32(63); // sea_level
    w.bool(false); // online_mode (no auth in the integrated server)
    w.bool(false); // enforces_secure_chat
    w.into_vec()
}

/// Converts one `lodestone-server` bool-grid column into the version-free
/// [`WorldChunkColumn`] the wire codec speaks, mapping the server's
/// `is_solid` grid onto stone/air block-state ids under `shape`.
///
/// Iterates section-major (matching wire order) and skips sections the
/// source reports as entirely air, since [`WorldChunkColumn::set_section`]
/// already elides those — a column that is all air outside `shape`'s window
/// (nothing above/below the source's own vertical extent) is therefore free.
fn build_world_column(
    shape: &ChunkShape,
    source: &ServerChunkColumn,
    stone: u32,
) -> WorldChunkColumn {
    let mut column = WorldChunkColumn::new(
        shape.min_y,
        shape.section_count,
        shape.block_kind,
        shape.biome_kind,
        shape.air_id,
        shape.biome_id,
    );

    for section_index in 0..shape.section_count {
        let base_y = shape.min_y + (section_index * ChunkSection::EDGE) as i32;
        let mut section = ChunkSection::new(
            shape.block_kind,
            shape.biome_kind,
            shape.air_id,
            shape.biome_id,
        );
        for ly in 0..ChunkSection::EDGE {
            let wy = base_y + ly as i32;
            for lz in 0..ChunkSection::EDGE {
                for lx in 0..ChunkSection::EDGE {
                    if source.is_solid(lx as i32, wy, lz as i32) {
                        section.set_block(lx, ly, lz, stone);
                    }
                }
            }
        }
        if !section.is_empty(shape.biome_id) {
            column.set_section(section_index, Some(section));
        }
    }

    column
}

/// Encodes one [`WorldChunkColumn`] into the `level_chunk_with_light` body,
/// mirroring `LevelChunkWithLight`'s decode in `packets::chunk` exactly:
/// `x`, `z`, empty heightmaps, the length-prefixed section blob (per section
/// two leading shorts — non-air count then fluid count, always `0` — then the
/// block-state container then the biome container), an empty block-entity
/// list, then the trailing light payload.
///
/// Heightmaps are sent empty and light is sent as all-`Missing`: both are
/// valid, decodable wire forms (confirmed against `Heightmaps`/`ColumnLight`'s
/// own encode logic), so the client accepts the column even though real
/// lighting and heightmap computation are not implemented yet — a documented
/// gap, not a hidden one.
fn encode_column_body(cx: i32, cz: i32, shape: &ChunkShape, column: &WorldChunkColumn) -> Vec<u8> {
    let mut w = Writer::default();
    w.i32(cx);
    w.i32(cz);

    Heightmaps::new().encode(&mut w);

    let mut section_blob = Writer::default();
    for section_index in 0..shape.section_count {
        // A freshly synthesized empty section for indices the column elided
        // (all-air, default biome) — every section index still gets bytes on
        // the wire; there is no "skip empty section" shortcut.
        let synthesized;
        let section = match column.section(section_index) {
            Some(section) => section,
            None => {
                synthesized = ChunkSection::new(
                    shape.block_kind,
                    shape.biome_kind,
                    shape.air_id,
                    shape.biome_id,
                );
                &synthesized
            }
        };
        section_blob.i16(section.non_air_count() as i16);
        section_blob.i16(0); // fluid count: this pipeline models no fluids yet
        section.block_states().encode(&mut section_blob);
        section.biomes().encode(&mut section_blob);
    }
    let section_bytes = section_blob.into_vec();
    w.var_i32(section_bytes.len() as i32);
    w.bytes(&section_bytes);

    w.var_i32(0); // block entities: none generated yet

    ColumnLight::new(shape.section_count).encode(&mut w);

    w.into_vec()
}

/// Server-side implementation of the protocol-776 (Minecraft 26.2) wire
/// format, driving `lodestone-server`'s [`ServerProtocol`] seam.
///
/// Holds no per-connection state: unlike [`V770Adapter`] (which tracks the
/// current dimension's [`ChunkShape`] across `login`/`respawn`), the server
/// always joins into the overworld today, so the shape is a constant rather
/// than connection state. A future respawn/dimension-change feature would
/// need to thread shape through here the same way the adapter does.
#[derive(Debug, Clone, Copy, Default)]
pub struct V770ServerProtocol;

impl ServerProtocol for V770ServerProtocol {
    fn decode(&self, state: lodestone_core::State, packet_id: i32, payload: &[u8]) -> ServerBound {
        use lodestone_core::State;

        match state {
            State::Handshaking if packet_id == handshaking::serverbound::INTENTION => {
                match decode_full::<Intention>(payload) {
                    Some(intention) => {
                        let next_state = if intention.next_state == 2 {
                            State::Login
                        } else {
                            State::Status
                        };
                        ServerBound::Handshake { next_state }
                    }
                    None => ServerBound::Ignored,
                }
            }
            State::Login if packet_id == login::serverbound::HELLO => {
                match decode_full::<LoginHello>(payload) {
                    Some(hello) => ServerBound::LoginStart {
                        username: hello.name,
                        uuid: hello.profile_id,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Login if packet_id == login::serverbound::LOGIN_ACKNOWLEDGED => {
                ServerBound::LoginAcknowledged
            }
            State::Configuration
                if packet_id == configuration::serverbound::FINISH_CONFIGURATION =>
            {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == play::serverbound::KEEP_ALIVE => {
                match decode_full::<KeepAlive>(payload) {
                    Some(keep_alive) => ServerBound::KeepAlive { id: keep_alive.id },
                    None => ServerBound::Ignored,
                }
            }
            // Only the two movement packets that carry a position matter to
            // the loop (view streaming needs x/z, nothing else); rotation-only
            // and status-only movement stay `Ignored` — see
            // `ServerBound::PlayerMoved`'s doc comment.
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_POS => {
                match decode_full::<MovePlayerPos>(payload) {
                    Some(m) => ServerBound::PlayerMoved {
                        x: m.x,
                        y: m.y,
                        z: m.z,
                    },
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::MOVE_PLAYER_POS_ROT => {
                match decode_full::<MovePlayerPosRot>(payload) {
                    Some(m) => ServerBound::PlayerMoved {
                        x: m.x,
                        y: m.y,
                        z: m.z,
                    },
                    None => ServerBound::Ignored,
                }
            }
            // Ordinals 0-2 are the three destroy phases; 3-7 are the item
            // actions (drop/release/swap/stab) this crate has no inventory
            // model to act on, so they decode to `Ignored` rather than a new
            // `ServerBound` variant — see `ServerBound::BlockAction`'s doc
            // comment.
            State::Play if packet_id == play::serverbound::PLAYER_ACTION => {
                match decode_full::<PlayerAction>(payload) {
                    Some(action) => {
                        let pos = unpack_block_pos(action.pos);
                        let face = face_from_ordinal(i32::from(action.direction));
                        match action.action {
                            0 => ServerBound::BlockAction {
                                action: BlockActionKind::StartDestroy,
                                pos,
                                face,
                                sequence: action.sequence,
                            },
                            1 => ServerBound::BlockAction {
                                action: BlockActionKind::AbortDestroy,
                                pos,
                                face,
                                sequence: action.sequence,
                            },
                            2 => ServerBound::BlockAction {
                                action: BlockActionKind::StopDestroy,
                                pos,
                                face,
                                sequence: action.sequence,
                            },
                            _ => ServerBound::Ignored,
                        }
                    }
                    None => ServerBound::Ignored,
                }
            }
            State::Play if packet_id == play::serverbound::USE_ITEM_ON => {
                match decode_full::<UseItemOn>(payload) {
                    Some(use_item) => ServerBound::UseItemOn {
                        pos: unpack_block_pos(use_item.pos),
                        face: face_from_ordinal(use_item.face),
                        sequence: use_item.sequence,
                    },
                    None => ServerBound::Ignored,
                }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        let finished = LoginFinished {
            profile_id: uuid,
            name: username.to_string(),
            properties: Vec::new(),
            session_id: uuid,
        };
        vec![send(login::clientbound::LOGIN_FINISHED, &finished)]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        // Minimum sequence: go straight to the finish signal. The client only
        // needs dimension type/biome registries if it derives chunk shape from
        // them, and `ChunkShape::for_dimension` hardcodes shape by dimension
        // name instead — so registry data, known-packs negotiation, and the
        // code-of-conduct exchange are all real vanilla packets this join
        // sequence does not yet need to send. See the module docs' scope note.
        vec![send(
            configuration::clientbound::FINISH_CONFIGURATION,
            &FinishConfiguration,
        )]
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        let login = GameLogin {
            entity_id: 1,
            hardcore: false,
            levels: vec!["minecraft:overworld".to_string()],
            max_players: 20,
            view_distance: view_radius.max(1),
            simulation_distance: view_radius.max(1),
            reduced_debug_info: false,
            show_death_screen: true,
            do_limited_crafting: false,
            dimension_type: 0,
            dimension: "minecraft:overworld".to_string(),
            seed: 0,
            game_type: 0, // survival
            rest: encode_game_login_rest(),
        };

        let spawn_x = 8;
        let spawn_y = 100;
        let spawn_z = 8;
        let spawn_position = SetDefaultSpawnPosition {
            location: GlobalPos {
                dimension: "minecraft:overworld".to_string(),
                position: pack_block_pos(spawn_x, spawn_y, spawn_z),
            },
            yaw: 0.0,
            pitch: 0.0,
        };

        let teleport_payload = encode_player_position_teleport(
            0,
            f64::from(spawn_x),
            f64::from(spawn_y),
            f64::from(spawn_z),
            0.0,
            0.0,
        );

        vec![
            send(play::clientbound::LOGIN, &login),
            send(
                play::clientbound::SET_DEFAULT_SPAWN_POSITION,
                &spawn_position,
            ),
            ServerDirective::Send {
                packet_id: play::clientbound::PLAYER_POSITION,
                payload: teleport_payload,
            },
            // Spawn is chunk (0, 0) (`spawn_x`/`spawn_z` = 8, inside that
            // column), matching `serve_connection`'s own initial view
            // center — reused via the trait method rather than duplicating
            // the encoder so join-time and move-time cache-center packets
            // can never drift apart.
            self.encode_chunk_cache_center(0, 0),
            // Vanilla fresh-spawn defaults. Without this the client's
            // `PlayerSnapshot::health` stays `None` (never having received a
            // `SetHealth`), which a HUD would show as absent/dead rather than
            // full health.
            send(
                play::clientbound::SET_HEALTH,
                &SetHealth {
                    health: 20.0,
                    food: 20,
                    saturation: 5.0,
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

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerChunkColumn) -> ServerDirective {
        let shape = ChunkShape::overworld_1_21();
        let world_column = build_world_column(&shape, column, stone_id());
        let payload = encode_column_body(cx, cz, &shape, &world_column);
        ServerDirective::Send {
            packet_id: play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            payload,
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        use crate::packets::game::ChunkBatchFinished;
        send(
            play::clientbound::CHUNK_BATCH_FINISHED,
            &ChunkBatchFinished { batch_size },
        )
    }

    fn welcome_message(&self) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: play::clientbound::SYSTEM_CHAT,
            payload: encode_system_chat("Welcome to Lodestone", false),
        }]
    }

    fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::ADD_ENTITY,
            payload: encode_add_entity_body(entity),
        }
    }

    fn encode_entity_update(
        &self,
        _prev: Option<&EntitySnapshot>,
        current: &EntitySnapshot,
    ) -> Vec<ServerDirective> {
        // MVP: always send an absolute position/rotation update rather than
        // computing a relative delta. `V770ServerProtocol` is a zero-sized,
        // stateless unit struct shared (via `Arc`) across every connection
        // (see `IntegratedServer::bind`), so it cannot safely hold per-entity
        // "last-sent" state itself — and vanilla's own `TELEPORT_ENTITY`
        // decodes into the exact same `ClientEvent::EntityMoved` a relative
        // move packet would produce, so this is 100% wire-valid, just not
        // bandwidth-optimal. `_prev` is accepted (unused for now) so a future
        // delta-encoding pass can use it without another signature change.
        vec![
            ServerDirective::Send {
                packet_id: play::clientbound::TELEPORT_ENTITY,
                payload: encode_teleport_entity(current),
            },
            ServerDirective::Send {
                packet_id: play::clientbound::ROTATE_HEAD,
                payload: encode_rotate_head(current.id, current.head_yaw),
            },
        ]
    }

    fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(ids.len() as i32);
        for &id in ids {
            w.var_i32(id);
        }
        ServerDirective::Send {
            packet_id: play::clientbound::REMOVE_ENTITIES,
            payload: w.into_vec(),
        }
    }

    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        // `KeepAlive` (`packets::common`) is identical on the wire in both
        // directions, so the same bidirectional struct this module's
        // `decode` arm above decodes the echo with also encodes the
        // challenge — no mirror-image encoder needed.
        send(play::clientbound::KEEP_ALIVE, &KeepAlive { id })
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::SET_TIME,
            payload: encode_set_time_body(game_time, day_time),
        }
    }

    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::SET_CHUNK_CACHE_CENTER,
            payload: encode_chunk_cache_center_body(cx, cz),
        }
    }

    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::FORGET_LEVEL_CHUNK,
            payload: encode_forget_chunk_body(cx, cz),
        }
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        ServerDirective::Send {
            packet_id: play::clientbound::BLOCK_UPDATE,
            payload: encode_block_update_body(x, y, z, resolve_state_id(state)),
        }
    }
}

#[cfg(test)]
mod block_edit_tests {
    use super::*;
    use lodestone_core::State;

    fn encode<T: Encode>(packet: &T) -> Vec<u8> {
        let mut w = Writer::default();
        packet.encode(&mut w, CTX).expect("well-formed struct encodes");
        w.into_vec()
    }

    /// `PLAYER_ACTION` ordinal `0` (`START_DESTROY_BLOCK`) round-trips
    /// through the real derived `Encode`/decode into
    /// `ServerBound::BlockAction`, with `pos`/`face` unpacked correctly —
    /// pinning both [`unpack_block_pos`] against [`pack_block_pos`] and
    /// [`face_from_ordinal`] against a non-trivial (non-zero) face.
    #[test]
    fn decode_player_action_start_destroy() {
        let proto = V770ServerProtocol;
        let body = encode(&PlayerAction {
            action: 0,
            pos: pack_block_pos(1, 2, 3),
            direction: 1, // Up
            sequence: 42,
        });
        let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
        assert_eq!(
            decoded,
            ServerBound::BlockAction {
                action: BlockActionKind::StartDestroy,
                pos: BlockPos::new(1, 2, 3),
                face: BlockFace::Up,
                sequence: 42,
            }
        );
    }

    /// The other two destroy ordinals decode to their matching
    /// `BlockActionKind`, proving the ordinal mapping is not just
    /// coincidentally right for `0`.
    #[test]
    fn decode_player_action_abort_and_stop() {
        let proto = V770ServerProtocol;
        for (ordinal, expected) in [
            (1, BlockActionKind::AbortDestroy),
            (2, BlockActionKind::StopDestroy),
        ] {
            let body = encode(&PlayerAction {
                action: ordinal,
                pos: pack_block_pos(0, 0, 0),
                direction: 0,
                sequence: 0,
            });
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
            assert_eq!(
                decoded,
                ServerBound::BlockAction {
                    action: expected,
                    pos: BlockPos::new(0, 0, 0),
                    face: BlockFace::Down,
                    sequence: 0,
                },
                "ordinal {ordinal}"
            );
        }
    }

    /// The item-action ordinals (`3`..=`7`: drop/release/swap/stab) share the
    /// wire packet but carry no terrain edit — this crate has no inventory
    /// model to act on them, so they must decode to `Ignored`, not silently
    /// fall into one of the three destroy phases.
    #[test]
    fn decode_player_action_item_ordinals_are_ignored() {
        let proto = V770ServerProtocol;
        for ordinal in 3..=7 {
            let body = encode(&PlayerAction {
                action: ordinal,
                pos: 0,
                direction: 0,
                sequence: 0,
            });
            let decoded = proto.decode(State::Play, play::serverbound::PLAYER_ACTION, &body);
            assert_eq!(decoded, ServerBound::Ignored, "ordinal {ordinal}");
        }
    }

    /// `USE_ITEM_ON` round-trips into `ServerBound::UseItemOn`, including a
    /// negative Y (below `y = 0`) to pin `unpack_block_pos`'s sign extension.
    #[test]
    fn decode_use_item_on() {
        let proto = V770ServerProtocol;
        let body = encode(&UseItemOn {
            hand: 0,
            pos: pack_block_pos(5, -10, -7),
            face: 3, // South
            cursor_x: 0.5,
            cursor_y: 1.0,
            cursor_z: 0.5,
            inside_block: false,
            world_border_hit: false,
            sequence: 7,
        });
        let decoded = proto.decode(State::Play, play::serverbound::USE_ITEM_ON, &body);
        assert_eq!(
            decoded,
            ServerBound::UseItemOn {
                pos: BlockPos::new(5, -10, -7),
                face: BlockFace::South,
                sequence: 7,
            }
        );
    }

    /// [`resolve_state_id`] round-trips the two propertyless states this
    /// crate ever writes back to themselves via [`block_name`].
    #[test]
    fn resolve_state_id_round_trips_stone_and_air() {
        assert_eq!(
            block_name(resolve_state_id("minecraft:stone")),
            Some("minecraft:stone")
        );
        assert_eq!(
            block_name(resolve_state_id("minecraft:air")),
            Some("minecraft:air")
        );
    }

    /// [`resolve_state_id`] must match on properties too, not just the block
    /// name — otherwise every propertied state of a block would resolve to
    /// whichever one happens to be first in the table. Picks a real
    /// propertied entry straight from the generated table rather than
    /// guessing a property string, so this cannot pass by coincidence.
    #[test]
    fn resolve_state_id_matches_properties_not_just_name() {
        let propertied_id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|&id| !properties(id).unwrap().is_empty())
            .expect("generated table has at least one propertied state");
        let name = block_name(propertied_id).unwrap();
        let props = properties(propertied_id).unwrap();
        let state_str = format!(
            "{name}[{}]",
            props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(resolve_state_id(&state_str), propertied_id);
    }

    /// [`resolve_state_id`] falls back to air rather than panicking on a
    /// string the generated table has no match for.
    #[test]
    fn resolve_state_id_falls_back_to_air_on_no_match() {
        assert_eq!(resolve_state_id("minecraft:not_a_real_block"), air_id());
    }

    /// Pins `encode_block_update`'s wire layout end to end: packed `BlockPos`
    /// then a VarInt state id, nothing else — the shape
    /// `ClientboundBlockUpdatePacket.STREAM_CODEC` specifies
    /// (`ClientboundBlockUpdatePacket.java:14-20`).
    #[test]
    fn encode_block_update_wire_layout() {
        let proto = V770ServerProtocol;
        let directive = proto.encode_block_update(1, 2, 3, "minecraft:stone");
        match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, play::clientbound::BLOCK_UPDATE);
                let mut r = Reader::new(&payload);
                let packed = r.i64().expect("packed pos");
                assert_eq!(packed, pack_block_pos(1, 2, 3));
                let id = r.var_i32().expect("state id");
                assert_eq!(id as u32, stone_id());
                r.ensure_empty().expect("no trailing bytes");
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }
}
