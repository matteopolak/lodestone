//! A captured initial chunk's palette-backed blocks, heightmaps, fluids, and light through public client state.
#![cfg(feature = "v26-2")]

use std::time::Duration;

use lodestone_client::{
    BlockPos, ChunkPos, ClientBuilder, ClientEvent, ConnectionState, Directive, EventStream,
    LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_core::Reader;
use lodestone_data::block_states::state_id;
use lodestone_model::{AdapterError, ClientAction, WorldSink};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{ChunkColumn as ServerChunkColumn, ServerDirective, ServerProtocol};
use lodestone_v26_2::{
    V770Adapter, V770ServerProtocol,
    packet_ids::play,
    packets::chunk::{ChunkShape, LevelChunkWithLight},
};
use serde::{Deserialize, Serialize};
use lodestone_world::{Heightmaps, LightData, PalettedContainer};
use tokio::io::DuplexStream;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

const CHUNK: [i32; 2] = [0, 0];
const MIN_Y: i32 = -64;
#[cfg(feature = "rcon-oracle")]
const CAPTURE_ORIGIN: (i32, i32, i32) = (0, 100, 0);

#[derive(Debug, Serialize, Deserialize)]
struct Capture {
    source: String,
    protocol: i32,
    chunk: [i32; 2],
    blocks: Vec<CapturedBlock>,
    motion_blocking_tops: Vec<CapturedHeight>,
    #[serde(default)]
    fluid_counts: Vec<CapturedFluidCount>,
    packet: CapturedPacket,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapturedBlock {
    pos: [i32; 3],
    state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapturedHeight {
    pos: [i32; 2],
    top_y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapturedFluidCount {
    section: usize,
    cells: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct CapturedPacket {
    packet_id: i32,
    #[serde(with = "compressed_bytes")]
    payload: Vec<u8>,
}

/// Stores the externally captured packet compactly; replay restores its exact
/// bytes before handing them to the production adapter.
mod compressed_bytes {
    use std::io::{Read, Write};

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
    use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
    use serde::{Deserialize, Deserializer, Serializer, de::Error, ser::Error as _};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut compressed = ZlibEncoder::new(Vec::new(), Compression::best());
        compressed.write_all(bytes).map_err(S::Error::custom)?;
        let compressed = compressed.finish().map_err(S::Error::custom)?;
        serializer.serialize_str(&BASE64.encode(compressed))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = String::deserialize(deserializer)?;
        let compressed = BASE64.decode(input).map_err(D::Error::custom)?;
        let mut decompressed = Vec::new();
        ZlibDecoder::new(compressed.as_slice())
            .read_to_end(&mut decompressed)
            .map_err(D::Error::custom)?;
        Ok(decompressed)
    }
}

/// Enters Play without reproducing login. Captured packets still flow through
/// the production 26.2 adapter and the normal client driver.
#[derive(Debug)]
struct PlayAdapter(V770Adapter);

impl VersionAdapter for PlayAdapter {
    fn protocol_version(&self) -> i32 {
        self.0.protocol_version()
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        self.0.minecraft_versions()
    }

    fn supports(&self, protocol: i32) -> bool {
        self.0.supports(protocol)
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::SetState(ConnectionState::Play)])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        self.0.handle_packet(world, state, packet_id, payload)
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        self.0.encode_action(state, action)
    }
}

struct CapturedClient {
    handle: lodestone_client::ClientHandle,
    events: EventStream,
    peer: Connection<DuplexStream>,
    runtime: Runtime,
}

impl CapturedClient {
    fn new() -> Self {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("captured chunk content runtime");
        let (client_io, server_io) = memory_pair();
        let (handle, events) = runtime.block_on(async {
            ClientBuilder::new(
                ServerAddress {
                    host: "captured-chunk-content".into(),
                    port: 0,
                },
                LoginProfile {
                    username: "CapturedChunkContent".into(),
                    uuid: Uuid::nil(),
                },
                Box::new(PlayAdapter(V770Adapter::new())),
            )
            .connect_with(client_io)
        });
        Self {
            handle,
            events,
            peer: Connection::new(server_io),
            runtime,
        }
    }

    fn replay_chunk(&mut self, packet: &CapturedPacket) -> Result<(), String> {
        self.runtime.block_on(async {
            self.peer
                .write_packet(packet.packet_id, &packet.payload)
                .await
                .map_err(|error| format!("write captured chunk packet: {error}"))?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = self
                        .events
                        .recv()
                        .await
                        .ok_or_else(|| "client driver ended before chunk content event".to_owned())?;
                    if matches!(event, ClientEvent::ChunkLoaded { pos } if (pos.x, pos.z) == (CHUNK[0], CHUNK[1])) {
                        return Ok(());
                    }
                }
            })
            .await
            .map_err(|_| "captured chunk content event deadline".to_owned())?
        })
    }

    fn compare_content(&self, capture: &Capture) -> Result<(), String> {
        let chunk = ChunkPos::new(capture.chunk[0], capture.chunk[1]);
        let dimensions = self
            .handle
            .world_dimensions()
            .ok_or_else(|| "chunk replay did not expose world dimensions".to_owned())?;
        if dimensions.min_y != MIN_Y {
            return Err(format!(
                "captured overworld min_y changed: expected {MIN_Y}, got {}",
                dimensions.min_y
            ));
        }

        let heightmap = self
            .handle
            .column_heightmap(chunk)
            .ok_or_else(|| "captured chunk omitted MOTION_BLOCKING heightmap".to_owned())?;
        for height in &capture.motion_blocking_tops {
            let actual =
                i32::try_from(heightmap.get(height.pos[0] as usize, height.pos[1] as usize))
                    .map_err(|_| format!("heightmap value does not fit i32 at {:?}", height.pos))?
                    + dimensions.min_y;
            if actual != height.top_y {
                return Err(format!(
                    "MOTION_BLOCKING top at {:?}: expected {}, got {actual}",
                    height.pos, height.top_y
                ));
            }
        }

        for block in &capture.blocks {
            let expected = state_id(&block.state).ok_or_else(|| {
                format!("fixture state missing from generated data: {}", block.state)
            })?;
            let pos = BlockPos::new(block.pos[0], block.pos[1], block.pos[2]);
            let actual = self.handle.block_at(pos);
            if actual != Some(expected) {
                return Err(format!(
                    "block at {:?}: expected {} ({expected}), got {actual:?}",
                    block.pos, block.state
                ));
            }

            let section_index = usize::try_from((block.pos[1] - dimensions.min_y) / 16)
                .map_err(|_| format!("block below world minimum: {:?}", block.pos))?;
            let section = self
                .handle
                .section_at(chunk, section_index)
                .ok_or_else(|| format!("palette-backed section {section_index} was elided"))?;
            let section_value = section.get_block(
                block.pos[0].rem_euclid(16) as usize,
                (block.pos[1] - dimensions.min_y).rem_euclid(16) as usize,
                block.pos[2].rem_euclid(16) as usize,
            );
            if section_value != expected {
                return Err(format!(
                    "section palette value at {:?}: expected {} ({expected}), got {section_value}",
                    block.pos, block.state
                ));
            }
        }
        Ok(())
    }
}

impl Drop for CapturedClient {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

fn captured_fixture() -> Capture {
    fixture("chunk_content_26_2.json")
}

fn fluid_fixture() -> Capture {
    fixture("chunk_content_fluids_26_2.json")
}

fn fixture(name: &str) -> Capture {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/{name}"));
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .expect("run the ignored capture acquisition test to create the external fixture"),
    )
    .expect("parse captured chunk content fixture")
}

fn wire_fluid_counts(payload: &[u8], shape: &ChunkShape) -> Result<Vec<u16>, String> {
    let mut raw = Reader::new(payload);
    raw.i32().map_err(|error| format!("chunk x: {error}"))?;
    raw.i32().map_err(|error| format!("chunk z: {error}"))?;
    Heightmaps::decode(shape.world_height, &mut raw).map_err(|error| format!("heightmaps: {error}"))?;
    let section_blob_len = raw
        .var_i32()
        .map_err(|error| format!("section blob length: {error}"))? as usize;
    let mut section_blob = raw
        .take_reader(section_blob_len)
        .map_err(|error| format!("section blob: {error}"))?;

    let mut counts = Vec::with_capacity(shape.section_count);
    for _ in 0..shape.section_count {
        section_blob
            .i16()
            .map_err(|error| format!("non-air count: {error}"))?;
        let count = section_blob
            .i16()
            .map_err(|error| format!("fluid count: {error}"))?;
        counts.push(u16::try_from(count).map_err(|_| format!("negative fluid count: {count}"))?);
        PalettedContainer::decode(shape.block_kind, &mut section_blob)
            .map_err(|error| format!("block palette: {error}"))?;
        PalettedContainer::decode(shape.biome_kind, &mut section_blob)
            .map_err(|error| format!("biome palette: {error}"))?;
    }
    section_blob
        .ensure_empty()
        .map_err(|error| format!("section blob trailing bytes: {error}"))?;
    Ok(counts)
}

fn compare_fluid_counts(capture: &Capture, actual: &[u16]) -> Result<(), String> {
    if capture.fluid_counts.is_empty() {
        return Err("fixture has no external fluid-count annotations".into());
    }
    for expected in &capture.fluid_counts {
        let found = actual
            .get(expected.section)
            .ok_or_else(|| format!("fluid-count section {} is outside packet", expected.section))?;
        if *found != expected.cells {
            return Err(format!(
                "fluid count in section {}: expected {}, got {found}",
                expected.section, expected.cells
            ));
        }
    }
    Ok(())
}

#[test]
fn captured_chunk_content_reaches_public_client_state() {
    let capture = captured_fixture();
    assert_eq!(capture.protocol, 776);
    assert_eq!(capture.chunk, CHUNK);
    assert_eq!(
        capture.packet.packet_id,
        play::clientbound::LEVEL_CHUNK_WITH_LIGHT
    );
    assert!(
        capture.blocks.len() >= 3,
        "the fixture needs distinct state probes to exercise its palette"
    );
    assert!(
        !capture.motion_blocking_tops.is_empty(),
        "the fixture needs externally annotated heightmap probes"
    );

    let mut client = CapturedClient::new();
    client
        .replay_chunk(&capture.packet)
        .expect("captured chunk content replay");
    client
        .compare_content(&capture)
        .expect("captured palette and heightmap comparison");
}

/// The external packet is also a raw-wire control for fields the public client
/// state deliberately normalizes. In particular, a decode/encode cycle would
/// not prove that the section fluid counter was truthful because the decoder
/// consumes that redundant short and does not retain it. Read the captured
/// section prefixes directly, then decode the same bytes through the
/// production packet type and require light data to reach client state as
/// well. This particular external fixture is deliberately superflat and has
/// no fluid placement, so its outside-derived control is that every fluid
/// counter is zero; the positive fluid-count control uses distinct water/lava
/// levels in its own external fixture.
#[test]
fn external_chunk_fixture_preserves_wire_counters_and_light() {
    let capture = captured_fixture();
    let shape = ChunkShape::overworld_1_21();

    let mut raw = Reader::new(&capture.packet.payload);
    assert_eq!(raw.i32().expect("chunk x"), capture.chunk[0]);
    assert_eq!(raw.i32().expect("chunk z"), capture.chunk[1]);
    Heightmaps::decode(shape.world_height, &mut raw).expect("external heightmaps");
    let section_blob_len = raw.var_i32().expect("section blob length") as usize;
    let mut section_blob = raw
        .take_reader(section_blob_len)
        .expect("external section blob");

    let mut fluid_sections = 0usize;
    let mut fluid_cells = 0usize;
    for _ in 0..shape.section_count {
        let _non_air = section_blob.i16().expect("external non-air count");
        let fluid = section_blob.i16().expect("external fluid count");
        if fluid > 0 {
            fluid_sections += 1;
            fluid_cells += fluid as usize;
        }
        PalettedContainer::decode(shape.block_kind, &mut section_blob)
            .expect("external block palette");
        PalettedContainer::decode(shape.biome_kind, &mut section_blob)
            .expect("external biome palette");
    }
    section_blob
        .ensure_empty()
        .expect("external section blob has no trailing bytes");
    assert_eq!(
        fluid_sections, 0,
        "external superflat fixture's source commands place no fluids"
    );
    assert_eq!(
        fluid_cells, 0,
        "external superflat fixture must not invent fluid-bearing states"
    );

    let mut decoded_reader = Reader::new(&capture.packet.payload);
    let decoded = LevelChunkWithLight::decode(&mut decoded_reader, &shape)
        .expect("external chunk packet");
    decoded_reader
        .ensure_empty()
        .expect("external chunk has no trailing bytes");
    assert!(
        (0..decoded.light.light_section_count())
            .any(|index| !matches!(decoded.light.sky(index), LightData::Missing)),
        "external chunk light payload must reach the production decoder"
    );
}

/// A second external packet holds one sealed water cell and one sealed lava
/// cell in the same section. Its annotations come from the two source
/// commands, so the two-count expectation does not originate in the encoder
/// or decoder under test.
#[test]
fn external_fluid_chunk_fixture_preserves_nonzero_wire_counters() {
    let capture = fluid_fixture();
    let shape = ChunkShape::overworld_1_21();
    assert_eq!(capture.protocol, 776);
    assert_eq!(capture.chunk, CHUNK);
    assert_eq!(
        capture.packet.packet_id,
        play::clientbound::LEVEL_CHUNK_WITH_LIGHT
    );
    assert_eq!(capture.blocks.len(), 2, "fixture needs water and lava probes");

    let counts = wire_fluid_counts(&capture.packet.payload, &shape)
        .expect("read externally captured fluid counters");
    compare_fluid_counts(&capture, &counts)
        .expect("external fluid-count annotations must match the raw packet");
    assert_eq!(
        counts.iter().copied().filter(|&count| count > 0).count(),
        1,
        "the sealed fixture has exactly one fluid-bearing section"
    );

    let mut decoded_reader = Reader::new(&capture.packet.payload);
    let decoded = LevelChunkWithLight::decode(&mut decoded_reader, &shape)
        .expect("decode external fluid chunk packet");
    decoded_reader
        .ensure_empty()
        .expect("external fluid chunk has no trailing bytes");
    for block in &capture.blocks {
        let expected = state_id(&block.state).expect("external fixture state exists locally");
        assert_eq!(
            decoded.column.get_block(
                block.pos[0].rem_euclid(16) as usize,
                block.pos[1],
                block.pos[2].rem_euclid(16) as usize,
            ),
            expected,
            "production decoder state at {:?}",
            block.pos
        );
    }

    let mut client = CapturedClient::new();
    client
        .replay_chunk(&capture.packet)
        .expect("external fluid chunk replay");
    client
        .compare_content(&capture)
        .expect("external fluid states reach public client state");
}

/// The counter annotations are an independent detector: a one-count mutation
/// cannot agree with the raw external packet even though the packet still
/// decodes and the public client state intentionally does not retain counters.
#[test]
fn corrupted_external_fluid_count_annotation_is_detected() {
    let mut capture = fluid_fixture();
    let shape = ChunkShape::overworld_1_21();
    let counts = wire_fluid_counts(&capture.packet.payload, &shape)
        .expect("read externally captured fluid counters");
    capture.fluid_counts[0].cells += 1;
    let error = compare_fluid_counts(&capture, &counts)
        .expect_err("a corrupt external fluid-count annotation must not agree");
    assert!(
        error.contains("fluid count in section"),
        "control must report the mismatching counter, got: {error}"
    );
}

/// The versioned server encoder receives the same externally named fluid
/// states and must write the counter annotations to its own packet body.
#[test]
fn production_encoder_matches_external_fluid_counter_annotations() {
    let capture = fluid_fixture();
    let shape = ChunkShape::overworld_1_21();
    let mut source = ServerChunkColumn::new(shape.min_y, shape.world_height as i32);
    for block in &capture.blocks {
        source.set_block(block.pos[0], block.pos[1], block.pos[2], &block.state);
    }
    let directive = ServerProtocol::encode_chunk(
        &V770ServerProtocol,
        capture.chunk[0],
        capture.chunk[1],
        &source,
    );
    let payload = match directive {
        ServerDirective::Send { payload, .. } => payload,
        other => panic!("expected encoded chunk packet, got {other:?}"),
    };
    let counts = wire_fluid_counts(&payload, &shape)
        .expect("read production encoder fluid counters");
    compare_fluid_counts(&capture, &counts)
        .expect("production encoder must match external fluid-count annotations");
}

#[test]
fn corrupted_chunk_content_annotation_is_detected() {
    let mut capture = captured_fixture();
    let mut client = CapturedClient::new();
    client
        .replay_chunk(&capture.packet)
        .expect("captured chunk content replay");

    capture.blocks[0].state = "minecraft:diamond_block".into();
    let error = client
        .compare_content(&capture)
        .expect_err("a corrupt external state annotation must not agree");
    assert!(
        error.contains("block at"),
        "the control must fail at the named public block read, got: {error}"
    );
}

/// Captures one packet after external commands prepare a bounded chunk column.
#[cfg(feature = "rcon-oracle")]
#[tokio::test]
#[ignore = "requires the headless 26.2 creative oracle on 25570/25571"]
async fn acquire_chunk_content_from_external_server() {
    use lodestone_fuzz::differential::{Action, WorldOracle, rcon::RconOracle};
    use lodestone_world::World;
    use tokio::net::TcpStream;

    const BLOCKS: [([i32; 3], &str); 3] = [
        ([3, 300, 3], "minecraft:oak_log[axis=x]"),
        ([7, 300, 3], "minecraft:oak_log[axis=z]"),
        ([11, 300, 3], "minecraft:glowstone"),
    ];

    async fn apply(
        conn: &mut Connection<TcpStream>,
        state: &mut ConnectionState,
        directive: Directive,
    ) {
        match directive {
            Directive::Send { packet_id, payload } => {
                conn.write_packet(packet_id, &payload).await.expect("send")
            }
            Directive::SetState(next) => *state = next,
            Directive::SetCompression(threshold) => conn.set_compression(threshold),
            Directive::Disconnect(reason) => {
                panic!("capture disconnected: {}", reason.to_plain_string())
            }
            _ => {}
        }
    }

    fn chunk_pos_from_level_chunk(payload: &[u8]) -> (i32, i32) {
        let mut reader = Reader::new(payload);
        (
            reader.i32().expect("level chunk x"),
            reader.i32().expect("level chunk z"),
        )
    }

    async fn next_target_chunk(
        conn: &mut Connection<TcpStream>,
        adapter: &V770Adapter,
        state: &mut ConnectionState,
        world: &mut World,
    ) -> CapturedPacket {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (packet_id, payload) = conn
                    .read_packet()
                    .await
                    .expect("capture read")
                    .expect("capture closed");
                let directives = adapter
                    .handle_packet(world, *state, packet_id, &payload)
                    .expect("capture decode");
                for directive in directives {
                    apply(conn, state, directive).await;
                }
                if *state == ConnectionState::Play
                    && packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT
                    && chunk_pos_from_level_chunk(&payload) == (CHUNK[0], CHUNK[1])
                {
                    return CapturedPacket { packet_id, payload };
                }
            }
        })
        .await
        .expect("captured chunk content deadline")
    }

    let mut rcon =
        RconOracle::connect("127.0.0.1:25571", "lodestone", CAPTURE_ORIGIN).expect("RCON");
    rcon.apply(&Action::RunCommand("forceload add 0 0".into()))
        .expect("force load capture chunk");
    for (pos, state) in BLOCKS {
        rcon.apply(&Action::RunCommand(format!(
            "setblock {} {} {} {state}",
            pos[0], pos[1], pos[2]
        )))
        .expect("prepare captured block");
    }

    let username = lodestone_testsupport::unique_username();
    let adapter = V770Adapter::new();
    let mut conn = tokio::time::timeout(
        Duration::from_secs(5),
        Connection::connect("127.0.0.1:25570"),
    )
    .await
    .expect("connect deadline")
    .expect("start scripts/live-oracles/creative.sh");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter
        .begin_login(
            &LoginProfile {
                username,
                uuid: Uuid::new_v4(),
            },
            &ServerAddress {
                host: "127.0.0.1".into(),
                port: 25570,
            },
        )
        .expect("login")
    {
        apply(&mut conn, &mut state, directive).await;
    }
    let mut world = World::new();
    let packet = next_target_chunk(&mut conn, &adapter, &mut state, &mut world).await;

    for (pos, _) in BLOCKS {
        rcon.apply(&Action::RunCommand(format!(
            "setblock {} {} {} minecraft:air",
            pos[0], pos[1], pos[2]
        )))
        .expect("capture cleanup");
    }
    rcon.apply(&Action::RunCommand("forceload remove 0 0".into()))
        .expect("release capture chunk");

    let capture = Capture {
        source: "Unmodified payload captured from the external 26.2 creative server on 25570, protocol 776. Before the capture client joined, RCON force-loaded chunk (0,0) and placed oak_log[axis=x] at (3,300,3), oak_log[axis=z] at (7,300,3), and glowstone at (11,300,3). The state and MOTION_BLOCKING-top annotations derive from those external source commands; the three blocks are above every other block in their columns. Capture entry point: acquire_chunk_content_from_external_server.".into(),
        protocol: 776,
        chunk: CHUNK,
        blocks: BLOCKS
            .into_iter()
            .map(|(pos, state)| CapturedBlock { pos, state: state.into() })
            .collect(),
        motion_blocking_tops: BLOCKS
            .into_iter()
            .map(|(pos, _)| CapturedHeight {
                pos: [pos[0], pos[2]],
                top_y: pos[1] + 1,
            })
            .collect(),
        fluid_counts: Vec::new(),
        packet,
    };
    let capture = serde_json::to_string_pretty(&capture).expect("capture JSON");
    if let Ok(path) = std::env::var("LODESTONE_CHUNK_CONTENT_CAPTURE_OUT") {
        std::fs::write(&path, &capture).expect("write explicitly requested capture output");
        println!("CHUNK_CONTENT_CAPTURE_WRITTEN={path}");
    } else {
        println!("CHUNK_CONTENT_CAPTURE={capture}");
    }
}

/// Captures a sealed pair of fluid cells so their wire counters are
/// observable independently of the production encoder.
#[cfg(feature = "rcon-oracle")]
#[tokio::test]
#[ignore = "requires the headless 26.2 creative oracle on 25570/25571"]
async fn acquire_fluid_chunk_from_external_server() {
    use lodestone_fuzz::differential::{Action, WorldOracle, rcon::RconOracle};
    use lodestone_world::World;
    use tokio::net::TcpStream;

    const BLOCKS: [([i32; 3], &str); 2] = [
        ([3, 100, 3], "minecraft:water[level=7]"),
        ([11, 100, 3], "minecraft:lava[level=3]"),
    ];

    async fn apply(
        conn: &mut Connection<TcpStream>,
        state: &mut ConnectionState,
        directive: Directive,
    ) {
        match directive {
            Directive::Send { packet_id, payload } => {
                conn.write_packet(packet_id, &payload).await.expect("send")
            }
            Directive::SetState(next) => *state = next,
            Directive::SetCompression(threshold) => conn.set_compression(threshold),
            Directive::Disconnect(reason) => {
                panic!("capture disconnected: {}", reason.to_plain_string())
            }
            _ => {}
        }
    }

    fn chunk_pos_from_level_chunk(payload: &[u8]) -> (i32, i32) {
        let mut reader = Reader::new(payload);
        (
            reader.i32().expect("level chunk x"),
            reader.i32().expect("level chunk z"),
        )
    }

    async fn next_target_chunk(
        conn: &mut Connection<TcpStream>,
        adapter: &V770Adapter,
        state: &mut ConnectionState,
        world: &mut World,
    ) -> CapturedPacket {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (packet_id, payload) = conn
                    .read_packet()
                    .await
                    .expect("capture read")
                    .expect("capture closed");
                let directives = adapter
                    .handle_packet(world, *state, packet_id, &payload)
                    .expect("capture decode");
                for directive in directives {
                    apply(conn, state, directive).await;
                }
                if *state == ConnectionState::Play
                    && packet_id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT
                    && chunk_pos_from_level_chunk(&payload) == (CHUNK[0], CHUNK[1])
                {
                    return CapturedPacket { packet_id, payload };
                }
            }
        })
        .await
        .expect("captured fluid chunk deadline")
    }

    let mut rcon =
        RconOracle::connect("127.0.0.1:25571", "lodestone", CAPTURE_ORIGIN).expect("RCON");
    rcon.apply(&Action::RunCommand("forceload add 0 0".into()))
        .expect("force load capture chunk");
    for (pos, _) in BLOCKS {
        let (x, y, z) = (pos[0], pos[1], pos[2]);
        rcon.apply(&Action::RunCommand(format!(
            "fill {} {} {} {} {} {} minecraft:glass hollow",
            x - 1,
            y - 1,
            z - 1,
            x + 1,
            y + 1,
            z + 1,
        )))
        .expect("seal fluid source cell");
    }
    for (pos, state) in BLOCKS {
        rcon.apply(&Action::RunCommand(format!(
            "setblock {} {} {} {state}",
            pos[0], pos[1], pos[2]
        )))
        .expect("place captured fluid");
    }

    let username = lodestone_testsupport::unique_username();
    let adapter = V770Adapter::new();
    let mut conn = tokio::time::timeout(
        Duration::from_secs(5),
        Connection::connect("127.0.0.1:25570"),
    )
    .await
    .expect("connect deadline")
    .expect("start scripts/live-oracles/creative.sh");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter
        .begin_login(
            &LoginProfile {
                username,
                uuid: Uuid::new_v4(),
            },
            &ServerAddress {
                host: "127.0.0.1".into(),
                port: 25570,
            },
        )
        .expect("login")
    {
        apply(&mut conn, &mut state, directive).await;
    }
    let mut world = World::new();
    let packet = next_target_chunk(&mut conn, &adapter, &mut state, &mut world).await;

    for (pos, _) in BLOCKS {
        let (x, y, z) = (pos[0], pos[1], pos[2]);
        rcon.apply(&Action::RunCommand(format!(
            "fill {} {} {} {} {} {} minecraft:air",
            x - 1,
            y - 1,
            z - 1,
            x + 1,
            y + 1,
            z + 1,
        )))
        .expect("remove fluid enclosure");
    }
    rcon.apply(&Action::RunCommand("forceload remove 0 0".into()))
        .expect("release capture chunk");

    let capture = Capture {
        source: "Unmodified payload captured from the external 26.2 creative server on 25570, protocol 776. Before the capture client joined, RCON force-loaded chunk (0,0), sealed two one-cell glass enclosures, then placed water[level=7] at (3,100,3) and lava[level=3] at (11,100,3). The enclosures prevent either source from spreading before capture. The state and one-section fluid-count annotation derive from those external source commands. Capture entry point: acquire_fluid_chunk_from_external_server.".into(),
        protocol: 776,
        chunk: CHUNK,
        blocks: BLOCKS
            .into_iter()
            .map(|(pos, state)| CapturedBlock { pos, state: state.into() })
            .collect(),
        // The glass lids are intentionally above both fluid cells, so these
        // positions are state probes rather than independent heightmap probes.
        motion_blocking_tops: Vec::new(),
        fluid_counts: vec![CapturedFluidCount {
            section: usize::try_from((BLOCKS[0].0[1] - MIN_Y) / 16).expect("fluid section"),
            cells: BLOCKS.len() as u16,
        }],
        packet,
    };
    let capture = serde_json::to_string_pretty(&capture).expect("capture JSON");
    if let Ok(path) = std::env::var("LODESTONE_FLUID_CHUNK_CONTENT_CAPTURE_OUT") {
        std::fs::write(&path, &capture).expect("write explicitly requested capture output");
        println!("FLUID_CHUNK_CONTENT_CAPTURE_WRITTEN={path}");
    } else {
        println!("FLUID_CHUNK_CONTENT_CAPTURE={capture}");
    }
}
