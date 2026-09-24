//! End-to-end: the **real** `lodestone-client` connects to `lodestone-server`
//! in-process and receives worldgen chunks, asserted **block-for-block**.
//!
//! This is the first end-to-end test in the project that needs no Docker
//! container. A single [`IntegratedServer::open_in_memory`] hands the client one
//! end of a [`memory_pair`](lodestone_net::memory_pair) duplex; the real
//! [`ClientBuilder::connect_with`] drives the real client on the other end. The
//! only stand-in is the **wire format**: this test supplies a matched pair — a
//! [`StandInProtocol`] (server → client encode) and a [`StandInAdapter`] (client
//! decode) — sharing one trivial, self-describing layout.
//!
//! What is therefore *real* and under test here, all at once:
//!
//! * the client's connect/login/dispatch driver ([`lodestone_client`]),
//! * the shared network codec — framing, length prefixes ([`lodestone_net`]),
//! * the client-owned world store and its `WorldSink` seam ([`lodestone_world`]),
//! * the integrated-server loop and lifecycle ([`serve_connection`],
//!   [`IntegratedServer`]),
//! * the density-function worldgen router ([`lodestone_worldgen`]),
//! * the [`Transport`](lodestone_net::Transport) seam.
//!
//! The one thing it does **not** exercise is the *versioned* `26.2` wire format
//! (paletted `level_chunk_with_light`, registries, NBT). That lives in the
//! version crate and is the reported [`ServerProtocol`] seam: a `v770`
//! `ServerProtocol` (client-bound encoders + server-bound decoders) drops in
//! where [`StandInProtocol`] sits, and a real `v770` `VersionAdapter` replaces
//! [`StandInAdapter`], and this same assertion then covers the real format.
//!
//! What would have to break for this to fail: any block diverging between what
//! worldgen generated on the server and what the client surfaces via
//! `block_at`, a framing error, a lost chunk, or the login state machine not
//! reaching `Play`. The non-vacuity guard additionally fails if the terrain is
//! empty air, so "delivered nothing, correctly" cannot pass.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_client::{
    BlockPos, ChunkPos, ClientBuilder, ClientEvent, ConnectionState, Directive, LoginProfile,
    ServerAddress, VersionAdapter,
};
use lodestone_core::{Reader, State, Writer};
use lodestone_model::{AdapterError, ClientAction};
use lodestone_server::{
    ChunkColumn as ServerColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective,
    ServerProtocol, WorldgenChunkSource,
};
use lodestone_world::{
    ChunkColumn as WorldColumn, ChunkPos as WorldChunkPos, ColumnLight, Heightmaps, LoadedChunk,
    PaletteKind, WorldSink,
};
use lodestone_worldgen::density::{Builder, Density, NoiseParams, Resolver};
use serde_json::Value;
use uuid::Uuid;

// A trivial shared wire vocabulary. Ids collide across states exactly as
// vanilla's do (handshake and login-start are both id 0), which is why
// `decode`/`handle_packet` are keyed on `(state, id)`.
const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const LOGIN_SUCCESS: i32 = 2;
const FINISH_CONFIGURATION: i32 = 3;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;

// Block-state ids carried in the stand-in chunk packet. `0` is air (elided);
// any solid worldgen block is sent as `STONE`.
const AIR: u32 = 0;
const STONE: u32 = 1;

// ---------------------------------------------------------------------------
// Server side: a ServerProtocol that encodes columns as (cx, cz, min_y, height,
// then one var-int block-state id per block).
// ---------------------------------------------------------------------------

struct StandInProtocol;

impl StandInProtocol {
    fn encode_column(cx: i32, cz: i32, col: &ServerColumn) -> Vec<u8> {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        w.var_i32(col.min_y);
        w.var_i32(col.height);
        for y in col.min_y..col.min_y + col.height {
            for z in 0..16 {
                for x in 0..16 {
                    let id = if col.is_solid(x, y, z) { STONE } else { AIR };
                    w.var_i32(id as i32);
                }
            }
        }
        w.as_slice().to_vec()
    }
}

impl ServerProtocol for StandInProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                let username = r.string(16).expect("username");
                ServerBound::LoginStart {
                    username,
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.string(username);
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: w.as_slice().to_vec(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: FINISH_CONFIGURATION,
            payload: Vec::new(),
        }]
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerColumn) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: Self::encode_column(cx, cz, column),
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(batch_size);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: w.as_slice().to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// Client side: a VersionAdapter that decodes the same layout into the world.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct StandInAdapter;

impl StandInAdapter {
    fn decode_column(payload: &[u8]) -> (i32, i32, LoadedChunk) {
        let mut r = Reader::new(payload);
        let cx = r.var_i32().expect("cx");
        let cz = r.var_i32().expect("cz");
        let min_y = r.var_i32().expect("min_y");
        let height = r.var_i32().expect("height");
        assert!(
            height % 16 == 0,
            "height must be a whole number of sections"
        );
        let section_count = (height / 16) as usize;

        let mut column = WorldColumn::new(
            min_y,
            section_count,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            AIR,
            0,
        );
        for y in min_y..min_y + height {
            for z in 0..16usize {
                for x in 0..16usize {
                    let id = r.var_i32().expect("block id") as u32;
                    if id != AIR {
                        column.set_block(x, y, z, id);
                    }
                }
            }
        }
        let chunk = LoadedChunk::new(
            column,
            ColumnLight::new(section_count),
            Heightmaps::new(),
            Vec::new(),
        );
        (cx, cz, chunk)
    }
}

impl VersionAdapter for StandInAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }
    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["stand-in"]
    }
    fn supports(&self, _protocol: i32) -> bool {
        true
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        let mut login = Writer::default();
        login.string(&profile.username);
        Ok(vec![
            Directive::Send {
                packet_id: HANDSHAKE,
                payload: vec![2],
            },
            Directive::SetState(ConnectionState::Login),
            Directive::Send {
                packet_id: LOGIN_START,
                payload: login.as_slice().to_vec(),
            },
        ])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        match state {
            ConnectionState::Login if packet_id == LOGIN_SUCCESS => Ok(vec![
                Directive::Send {
                    packet_id: LOGIN_ACKNOWLEDGED,
                    payload: Vec::new(),
                },
                Directive::SetState(ConnectionState::Configuration),
            ]),
            ConnectionState::Configuration if packet_id == FINISH_CONFIGURATION => Ok(vec![
                Directive::Send {
                    packet_id: FINISH_CONFIGURATION,
                    payload: Vec::new(),
                },
                Directive::SetState(ConnectionState::Play),
            ]),
            ConnectionState::Play if packet_id == CHUNK => {
                let (cx, cz, chunk) = Self::decode_column(payload);
                world.load(WorldChunkPos::new(cx, cz), chunk);
                Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded {
                    pos: ChunkPos::new(cx, cz),
                })])
            }
            _ => Ok(Vec::new()),
        }
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Worldgen wiring (shared with `integrated_memory.rs`).
// ---------------------------------------------------------------------------

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).expect("read worldgen json");
        serde_json::from_str(&text).expect("parse worldgen json")
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().unwrap() as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_f64().unwrap())
                .collect(),
        }
    }
}

fn overworld_final_density(seed: i64, root: &Path) -> Density {
    let resolver = FsResolver {
        root: root.to_path_buf(),
    };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let builder = Builder::new(seed, &resolver);
    builder
        .build(&settings["noise_router"]["final_density"])
        .expect("bundled final_density density-function document")
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "SinglePlayer".into(),
        uuid: Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

#[tokio::test]
async fn real_client_receives_worldgen_chunks_in_process() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let seed = 42_i64;
    let sample_min_y = -64;
    let sample_height = 96; // 6 sections; a whole number of sections
    let view_radius = 0; // single chunk (0,0)

    let final_density = overworld_final_density(seed, &root);
    let source = WorldgenChunkSource::new(final_density.clone(), sample_min_y, sample_height);
    // Independent reference the client's blocks are checked against.
    let reference = WorldgenChunkSource::new(final_density, sample_min_y, sample_height);

    // Start the integrated server in-process; get the client's transport end.
    let (server, client_io) =
        IntegratedServer::open_in_memory(StandInProtocol, source, view_radius);

    // The *real* client drives the other end.
    let (handle, _events) =
        ClientBuilder::new(address(), profile(), Box::new(StandInAdapter)).connect_with(client_io);

    // Wait for the chunk to arrive (poll; never assert immediately). Generating
    // a full column point-samples the overworld density router per block, which
    // is several seconds in a debug build — hence a generous deadline, not a
    // tight race.
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_secs(60);
    while handle.loaded_chunk_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "client never received a chunk within 60s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(handle.loaded_chunk_count(), 1, "exactly one chunk expected");

    // Block-for-block: every block the client surfaces must equal what worldgen
    // generated on the server (mapped through the stand-in's air/stone coding).
    let expected = reference.column(0, 0);
    let mut checked = 0usize;
    let mut solid = 0usize;
    for y in sample_min_y..sample_min_y + sample_height {
        for z in 0..16 {
            for x in 0..16 {
                let want = if expected.is_solid(x, y, z) {
                    STONE
                } else {
                    AIR
                };
                let got = handle.block_at(BlockPos::new(x, y, z));
                assert_eq!(
                    got,
                    Some(want),
                    "block mismatch at ({x},{y},{z}): client={got:?} worldgen={want}"
                );
                checked += 1;
                if want == STONE {
                    solid += 1;
                }
            }
        }
    }

    assert_eq!(checked, 16 * 16 * sample_height as usize);
    // Non-vacuity: the seeded router must have produced terrain, so this is a
    // real block-content comparison and not "correctly delivered nothing".
    assert!(
        solid > 0,
        "worldgen produced no solid blocks — vacuous check"
    );

    println!(
        "real client received chunk (0,0) over in-memory transport; \
         {checked} blocks verified block-for-block against worldgen, {solid} solid"
    );

    server.shutdown().await;
}
