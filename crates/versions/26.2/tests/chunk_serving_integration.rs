//! Raw-wire coverage for the complete chunk-serving join path.
//!
//! The two protocol decorators below deliberately differ in only the light
//! retention capability. Both still delegate the real login, play, and chunk
//! encoders, so the packet under test is emitted by `serve_connection`, not by
//! a direct encoder call. The retained arm records the neighbour-bearing hook;
//! the fallback arm records the one-column hook. The body parser then checks
//! the heightmap map and the section counters without using the packet decoder
//! that discards those redundant counter fields.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use lodestone_core::{Reader, State, Writer};
use lodestone_net::{Connection, Transport, memory_pair};
use lodestone_server::{
    BlockEntityHandle, ChunkColumn, ChunkSource, MobHandle, NoEntities, ServerBound,
    ServerDirective, ServerProtocol, serve_connection,
};
use lodestone_v26_2::packets::chunk::ChunkShape;
use lodestone_v26_2::packet_ids::{configuration, login, play};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_world::PalettedContainer;
use uuid::Uuid;

#[path = "common/mod.rs"]
mod common;

const FLUID_SECTION: usize = 10;
const FLOOR_RELATIVE_TOP: u32 = 128;
const FLUID_RELATIVE_TOP: u32 = 165;

/// The floor makes spawn resolution deterministic; the two isolated fluids
/// make the section counter non-zero and distinguish it from the floor.
struct FlatFluidWorld;

impl ChunkSource for FlatFluidWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 63, z, "minecraft:stone");
            }
        }
        column.set_block(3, 100, 3, "minecraft:water[level=7]");
        column.set_block(11, 100, 3, "minecraft:lava[level=3]");
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// A small test-only decorator. The required join methods are forwarded so
/// the connection is genuine; optional methods intentionally retain the trait
/// defaults, which is the public way to exercise the one-column fallback.
struct TracingProtocol {
    retained: bool,
    neighbour_calls: Arc<AtomicUsize>,
    fallback_calls: Arc<AtomicUsize>,
}

impl TracingProtocol {
    fn retained() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let neighbour_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                retained: true,
                neighbour_calls: Arc::clone(&neighbour_calls),
                fallback_calls: Arc::clone(&fallback_calls),
            },
            neighbour_calls,
            fallback_calls,
        )
    }

    fn fallback() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let neighbour_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                retained: false,
                neighbour_calls: Arc::clone(&neighbour_calls),
                fallback_calls: Arc::clone(&fallback_calls),
            },
            neighbour_calls,
            fallback_calls,
        )
    }
}

impl ServerProtocol for TracingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        V770ServerProtocol.decode(state, packet_id, payload)
    }

    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        V770ServerProtocol.login_success(username, uuid)
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        V770ServerProtocol.begin_configuration()
    }

    fn encode_registry_data(&self) -> Vec<ServerDirective> {
        V770ServerProtocol.encode_registry_data()
    }

    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        V770ServerProtocol.begin_play(view_radius)
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        V770ServerProtocol.begin_chunk_batch()
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        V770ServerProtocol.encode_chunk(cx, cz, column)
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        V770ServerProtocol.end_chunk_batch(batch_size)
    }

    fn retains_initial_column_light(&self) -> bool {
        self.retained
    }

    fn uses_cross_column_light(&self) -> bool {
        self.retained
    }

    fn compute_initial_column_light_with_neighbours_in_dimension(
        &self,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: lodestone_server::dimension::Dimension,
    ) -> Option<lodestone_world::ColumnLight> {
        V770ServerProtocol.compute_initial_column_light_with_neighbours_in_dimension(
            column, neighbours, dimension,
        )
    }

    fn try_encode_chunk_with_neighbours_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        neighbours: &[(i32, i32, ChunkColumn)],
        dimension: lodestone_server::dimension::Dimension,
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        assert_eq!(
            neighbours.len(),
            8,
            "retained initial encoding must receive the complete adjacent footprint"
        );
        self.neighbour_calls.fetch_add(1, Ordering::Relaxed);
        V770ServerProtocol.try_encode_chunk_with_neighbours_in_dimension(
            cx, cz, column, neighbours, dimension,
        )
    }

    fn try_encode_chunk_in_dimension(
        &self,
        cx: i32,
        cz: i32,
        column: &ChunkColumn,
        dimension: lodestone_server::dimension::Dimension,
    ) -> Result<ServerDirective, lodestone_server::ChunkEncodeError> {
        self.fallback_calls.fetch_add(1, Ordering::Relaxed);
        V770ServerProtocol.try_encode_chunk_in_dimension(cx, cz, column, dimension)
    }
}

fn handshake_bytes() -> Vec<u8> {
    let mut writer = Writer::default();
    writer.var_i32(776);
    writer.string("localhost");
    writer.u16(25565);
    writer.var_i32(2);
    writer.into_vec()
}

fn hello_bytes(name: &str, uuid: Uuid) -> Vec<u8> {
    let mut writer = Writer::default();
    writer.string(name);
    writer.uuid(uuid);
    writer.into_vec()
}

async fn drain<T: Transport>(client: &mut Connection<T>) -> Vec<(i32, Vec<u8>)> {
    let mut packets = Vec::new();
    while let Ok(Ok(Some(packet))) =
        tokio::time::timeout(Duration::from_millis(500), client.read_packet()).await
    {
        packets.push(packet);
    }
    packets
}

async fn join<P: ServerProtocol + 'static>(protocol: P) -> Vec<(i32, Vec<u8>)> {
    let (client_io, server_io) = memory_pair();
    tokio::spawn(async move {
        let mut connection = Connection::new(server_io);
        let _ = serve_connection(
            &mut connection,
            &protocol,
            &FlatFluidWorld,
            &NoEntities,
            1,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await;
    });

    let mut client = Connection::new(client_io);
    client.write_packet(0, &handshake_bytes()).await.unwrap();
    let uuid = Uuid::new_v4();
    client.write_packet(0, &hello_bytes("ChunkCounters", uuid)).await.unwrap();
    let _ = common::read_login_packet(&mut client).await.unwrap();
    client
        .write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[])
        .await
        .unwrap();
    let _ = common::read_login_packet(&mut client).await.unwrap();
    client
        .write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[])
        .await
        .unwrap();
    drain(&mut client).await
}

struct ParsedChunk {
    maps: Vec<(i32, Vec<u64>)>,
    fluid_count: u16,
}

fn parse_chunk(payload: &[u8]) -> ParsedChunk {
    let shape = ChunkShape::overworld_1_21();
    let mut raw = Reader::new(payload);
    assert_eq!(raw.i32().expect("chunk x"), 0);
    assert_eq!(raw.i32().expect("chunk z"), 0);
    let map_count = raw.var_i32().expect("heightmap count");
    assert_eq!(map_count, 3);
    let mut maps = Vec::new();
    for _ in 0..map_count {
        let id = raw.var_i32().expect("heightmap key");
        let long_count = raw.var_i32().expect("heightmap long count");
        assert_eq!(long_count, 37);
        maps.push((
            id,
            (0..long_count)
                .map(|_| raw.i64().expect("heightmap long") as u64)
                .collect(),
        ));
    }

    let blob_len = usize::try_from(raw.var_i32().expect("section blob length"))
        .expect("non-negative section blob length");
    let mut sections = raw.take_reader(blob_len).expect("section blob");
    let mut fluid_count = None;
    for section in 0..shape.section_count {
        let _non_air = sections.i16().expect("section non-air count");
        let fluid = sections.i16().expect("section fluid count");
        if section == FLUID_SECTION {
            fluid_count = Some(u16::try_from(fluid).expect("non-negative fluid count"));
        }
        PalettedContainer::decode(shape.block_kind, &mut sections).expect("block states");
        PalettedContainer::decode(shape.biome_kind, &mut sections).expect("biomes");
    }
    sections.ensure_empty().expect("section blob has no trailing bytes");

    ParsedChunk {
        maps,
        fluid_count: fluid_count.expect("fluid section exists"),
    }
}

fn map_value(longs: &[u64], x: usize, z: usize) -> u32 {
    let index = x + z * 16;
    let long = longs[index / 7];
    ((long >> ((index % 7) * 9)) & 0x1ff) as u32
}

fn assert_chunk_content(packets: &[(i32, Vec<u8>)]) {
    let chunks: Vec<_> = packets
        .iter()
        .filter(|(id, _)| *id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT)
        .collect();
    assert!(chunks.len() >= 9, "the join pre-stream must emit its centre and neighbours");
    let (_, payload) = chunks
        .iter()
        .copied()
        .find(|(_, payload)| payload.len() > 8 && payload[0..8] == [0; 8])
        .unwrap_or(chunks[0]);
    let parsed = parse_chunk(payload);
    let mut ids: Vec<_> = parsed.maps.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 4, 5]);
    for (_, map) in &parsed.maps {
        assert_eq!(map_value(map, 0, 0), FLOOR_RELATIVE_TOP);
        assert_eq!(map_value(map, 3, 3), FLUID_RELATIVE_TOP);
        assert_eq!(map_value(map, 11, 3), FLUID_RELATIVE_TOP);
    }
    assert_eq!(parsed.fluid_count, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_serves_truthful_chunk_content_through_both_encoding_arms() {
    let (retained, neighbour_calls, fallback_calls) = TracingProtocol::retained();
    let retained_packets = join(retained).await;
    assert_chunk_content(&retained_packets);
    assert!(
        neighbour_calls.load(Ordering::Relaxed) > 0,
        "retained joins must invoke the neighbour-bearing chunk encoder"
    );
    assert_eq!(fallback_calls.load(Ordering::Relaxed), 0);

    let (fallback, neighbour_calls, fallback_calls) = TracingProtocol::fallback();
    let fallback_packets = join(fallback).await;
    assert_chunk_content(&fallback_packets);
    assert_eq!(neighbour_calls.load(Ordering::Relaxed), 0);
    assert!(
        fallback_calls.load(Ordering::Relaxed) > 0,
        "non-retained joins must invoke the one-column fallback encoder"
    );
}
