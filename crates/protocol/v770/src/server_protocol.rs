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
//! spawn, initial teleport, chunk-cache center), and
//! `level_chunk_with_light` for every column in the initial view. It does
//! not yet cover keep-alives, entity spawn/move, time, or health — those are
//! follow-up work once the join+chunk path is proven (see the crate's
//! `tests/server_integration.rs`).
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

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_server::{
    ChunkColumn as ServerChunkColumn, ServerBound, ServerDirective, ServerProtocol,
};
use lodestone_world::{ChunkColumn as WorldChunkColumn, ChunkSection, ColumnLight, Heightmaps};
use uuid::Uuid;

use crate::block_states::block_name;
use crate::packet_ids::{configuration, handshaking, login, play};
use crate::packets::chunk::ChunkShape;
use crate::packets::configuration::FinishConfiguration;
use crate::packets::game::{GameLogin, GlobalPos, SetDefaultSpawnPosition};
use crate::packets::handshake::Intention;
use crate::packets::login::{LoginFinished, LoginHello};

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
fn encode_chunk_cache_center(cx: i32, cz: i32) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(cx);
    w.var_i32(cz);
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

        let cache_center_payload = encode_chunk_cache_center(0, 0);

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
            ServerDirective::Send {
                packet_id: play::clientbound::SET_CHUNK_CACHE_CENTER,
                payload: cache_center_payload,
            },
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
}
