//! Browser **singleplayer** — the real integrated server and the real client,
//! connected in-process, running entirely inside the browser event loop.
//!
//! This is the W-next headline: no relay, no CORS, no socket, no Docker. A
//! single [`IntegratedServer::open_in_memory`] serves worldgen chunks over an
//! in-memory [`DuplexStream`]; the real [`ClientBuilder::connect_with`] drives
//! `lodestone-client` on the other end. Both halves are spawned by the crate's
//! `spawn` seam, which on wasm is `wasm-bindgen-futures`' `spawn_local` — so this
//! module is the first time the client/server stack has actually *run* in a
//! browser, converting the "spawn_local drives it with no tokio runtime" claim
//! from believed to reproduced.
//!
//! ## What is real here, and what is a stand-in
//!
//! Real: the client connect/login/dispatch driver, the shared `lodestone-net`
//! codec (framing/length prefixes), the client-owned world store + `WorldSink`,
//! the integrated-server loop, the density-function worldgen router, and the
//! `Transport` seam — all under `spawn_local`. The single stand-in is the *wire
//! format*: a matched [`StandInProtocol`] (server→client encode) and
//! [`StandInAdapter`] (client decode) sharing one trivial self-describing layout,
//! exactly as `lodestone-server`'s `client_integration.rs` test does. The real
//! versioned 26.2 wire format is the reported `v770` `ServerProtocol` seam
//! (owned by `impl-v770`), which drops in where [`StandInProtocol`] sits.
//!
//! ## The one browser adaptation
//!
//! The native test's worldgen `Resolver` reads density-function JSON with
//! `std::fs`. A browser has no filesystem, so [`BrowserResolver`] serves the same
//! JSON from a map fetched once (`assets/worldgen.json`, the 97 density/noise
//! files concatenated). This is the same "cross the filesystem wall once, at the
//! edge, then run the unchanged sync pipeline" shape the asset path already uses.
//!
//! ## What this does NOT do (a boundary, not a gap to paper over)
//!
//! It does not *render* the generated world. Meshing needs the client to expose
//! `world.chunk(pos) -> Option<Arc<LoadedChunk>>` (a `ChunkColumn` snapshot);
//! the handle currently exposes only `block_at`/`loaded_chunk_count`.
//! Reconstructing a column from per-block `block_at` would be a shadow world —
//! a half-build across a crate boundary — so this probe proves the stack *up to*
//! that seam (chunks received, blocks correct, timing) and stops. The seam is
//! routed to `impl-client`.

use std::collections::HashMap;

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

// The shared trivial wire vocabulary (ids collide across states exactly as
// vanilla's do — handshake and login-start are both id 0).
const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const CHUNK: i32 = 0x27;
const AIR: u32 = 0;
const STONE: u32 = 1;

// ---------------------------------------------------------------------------
// Server side: encodes columns as (cx, cz, min_y, height, one var-int per block).
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
                ServerBound::LoginStart { username }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_sequence(&self, username: &str) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.string(username);
        vec![
            ServerDirective::Send {
                packet_id: LOGIN_SUCCESS,
                payload: w.as_slice().to_vec(),
            },
            ServerDirective::SetState(State::Play),
        ]
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ServerColumn) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: Self::encode_column(cx, cz, column),
        }
    }
}

// ---------------------------------------------------------------------------
// Client side: decodes the same layout into the world.
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
        assert!(height % 16 == 0, "height must be a whole number of sections");
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
            ConnectionState::Login if packet_id == LOGIN_SUCCESS => {
                Ok(vec![Directive::SetState(ConnectionState::Play)])
            }
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
// Worldgen wiring — the browser adaptation: a fetched-map resolver, no std::fs.
// ---------------------------------------------------------------------------

/// The same density/noise JSON the native `FsResolver` reads from disk, served
/// instead from a map fetched once (`assets/worldgen.json`). Keys are the file's
/// path under `worldgen_data/` without the `.json` extension, e.g.
/// `density_function/overworld/depth` or `noise/continentalness`.
struct BrowserResolver {
    map: HashMap<String, Value>,
}

impl BrowserResolver {
    fn get(&self, kind: &str, id: &str) -> &Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let key = format!("{kind}/{name}");
        self.map
            .get(&key)
            .unwrap_or_else(|| panic!("missing worldgen json: {key}"))
    }
}

impl Resolver for BrowserResolver {
    fn density_function(&self, id: &str) -> Value {
        self.get("density_function", id).clone()
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.get("noise", id);
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

fn overworld_final_density(seed: i64, map: &HashMap<String, Value>) -> Density {
    let settings = map
        .get("noise_settings/overworld")
        .expect("noise_settings/overworld in worldgen.json");
    let resolver = BrowserResolver { map: map.clone() };
    let builder = Builder::new(seed, &resolver);
    builder.build(&settings["noise_router"]["final_density"])
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "SinglePlayer".into(),
        // A constant UUID: no getrandom needed, and singleplayer login does not
        // depend on it being unique.
        uuid: Uuid::from_u128(0x10de_5701_0000_0000_0000_0000_0000_0001),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// What the probe proved, surfaced to the HUD.
pub struct Report {
    pub play_reached: bool,
    pub chunks: usize,
    /// Time to generate one worldgen column on the browser's single thread
    /// (median of a few `column(0,0)` calls), in milliseconds.
    pub worldgen_ms: f64,
    /// A sampled solid block from the generated column, if any.
    pub sample: Option<(i32, i32, i32, u32)>,
    pub solid_sampled: usize,
    pub checked_sampled: usize,
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

/// Run the in-browser singleplayer probe end to end.
///
/// `map_bytes` is the fetched `assets/worldgen.json`. Returns a [`Report`] or a
/// diagnostic string. Never panics on the happy path; worldgen JSON shape errors
/// surface as a message rather than tearing down the page.
pub async fn run_singleplayer(map_bytes: &[u8]) -> Result<Report, String> {
    let map: HashMap<String, Value> =
        serde_json::from_slice(map_bytes).map_err(|e| format!("worldgen.json parse: {e}"))?;

    let seed = 42_i64;
    let min_y = -64;
    let height = 96; // 6 sections
    let view_radius = 0; // just chunk (0,0)

    // --- worldgen timing (item 1c): how long one column takes on one thread ---
    // Build a dedicated source and time `column(0,0)`. The FIRST call warms lazy
    // state; report the median of a few. This synchronous call blocks the event
    // loop — a UX finding in itself if it is slow.
    let density = overworld_final_density(seed, &map);
    let timing_source = WorldgenChunkSource::new(density.clone(), min_y, height);
    let mut samples = Vec::new();
    for _ in 0..3 {
        let t0 = now_ms();
        let col = timing_source.column(0, 0);
        let dt = now_ms() - t0;
        // Touch the column so the generation can't be optimised away.
        std::hint::black_box(col.is_solid(8, 0, 8));
        samples.push(dt);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let worldgen_ms = samples[samples.len() / 2];

    // --- the real stack: integrated server ↔ real client, in-memory transport ---
    // `_server` (not `_`) keeps the server task alive for the poll; dropping it at
    // function end signals shutdown and aborts the task (its documented Drop).
    let source = WorldgenChunkSource::new(density, min_y, height);
    let (_server, client_io) =
        IntegratedServer::open_in_memory(StandInProtocol, source, view_radius);

    let (handle, _events) =
        ClientBuilder::new(address(), profile(), Box::new(StandInAdapter)).connect_with(client_io);

    // Poll for the chunk. `gloo_timers` gives a wasm-safe async sleep (the native
    // test's `tokio::time::sleep` would panic — no runtime). Yielding here lets
    // the spawn_local'd server + client driver tasks make progress. Chunks only
    // arrive in `Play`, so `loaded_chunk_count() > 0` proves login → Play too.
    let deadline = now_ms() + 30_000.0;
    while handle.loaded_chunk_count() == 0 {
        if now_ms() > deadline {
            return Err(format!(
                "no chunk within 30s (worldgen {worldgen_ms:.0} ms/chunk — likely still generating)"
            ));
        }
        gloo_timers::future::TimeoutFuture::new(20).await;
    }
    let play_reached = true;
    let chunks = handle.loaded_chunk_count();

    // Sample blocks the client surfaces and count solids (non-vacuity).
    let mut sample = None;
    let mut solid = 0usize;
    let mut checked = 0usize;
    for y in min_y..min_y + height {
        for z in (0..16).step_by(4) {
            for x in (0..16).step_by(4) {
                if let Some(id) = handle.block_at(BlockPos::new(x, y, z)) {
                    checked += 1;
                    if id != AIR {
                        solid += 1;
                        if sample.is_none() {
                            sample = Some((x, y, z, id));
                        }
                    }
                }
            }
        }
    }

    // `_server` drops here → shutdown + task abort (documented Drop behaviour).
    Ok(Report {
        play_reached,
        chunks,
        worldgen_ms,
        sample,
        solid_sampled: solid,
        checked_sampled: checked,
    })
}
