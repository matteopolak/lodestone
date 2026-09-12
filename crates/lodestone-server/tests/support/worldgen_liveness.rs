//! Shared fixture for the integrated-server world-generation liveness gate.
//!
//! The source blocks exactly one newly visible column behind an explicit gate.
//! The protocol is deliberately tiny, but it drives the real login, view,
//! movement, block-use, scheduled-tick, and packet-dispatch paths. The gate's
//! bounded fallback release makes an inline generation call fail with an
//! assertion instead of leaving a test process hung forever.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lodestone_core::{Reader, State, Writer};
use lodestone_model::{BlockFace, BlockPos, Vec3f};
use lodestone_server::{ChunkColumn, ChunkSource, ServerBound, ServerDirective, ServerProtocol};
use uuid::Uuid;

pub const HANDSHAKE: i32 = 0;
pub const LOGIN_START: i32 = 0;
pub const LOGIN_SUCCESS: i32 = 2;
pub const LOGIN_ACKNOWLEDGED: i32 = 3;
pub const FINISH_CONFIGURATION: i32 = 3;
pub const CHUNK_BATCH_START: i32 = 10;
pub const CHUNK: i32 = 0x27;
pub const CHUNK_BATCH_FINISHED: i32 = 11;

pub const USE_BUTTON: i32 = 100;
pub const MOVE_PLAYER: i32 = 101;
pub const CLIENT_ACTION: i32 = 102;
pub const BLOCK_UPDATE: i32 = 103;

pub const BLOCKED_COLUMN: (i32, i32) = (8, 0);
// Keep the button in chunk (2, 0). The liveness test later moves the player to
// chunk (5, 0) to request the held generation column (8, 0); chunk 2 remains
// inside both the radius-4 retained view and the radius-3 authoritative tick
// area, so a scheduled button release can be observed while generation is held.
pub const BUTTON_POS: (i32, i32, i32) = (33, 64, 0);
pub const BUTTON_OFF: &str =
    "minecraft:stone_button[face=floor,facing=north,powered=false]";
pub const BUTTON_ON: &str =
    "minecraft:stone_button[face=floor,facing=north,powered=true]";

/// A generation gate that is held by the test until the liveness assertions
/// have observed progress. The timeout is an intentional control: an
/// implementation that blocks the runtime thread eventually resumes, marks
/// `auto_released`, and fails the test's "still held" assertion.
#[derive(Clone, Debug)]
pub struct GenerationGate {
    state: Arc<GateState>,
}

#[derive(Debug)]
struct GateState {
    started: AtomicBool,
    released: AtomicBool,
    auto_released: AtomicBool,
    completed: AtomicBool,
    hold_for: Duration,
}

impl GenerationGate {
    #[must_use]
    pub fn new(hold_for: Duration) -> Self {
        Self {
            state: Arc::new(GateState {
                started: AtomicBool::new(false),
                released: AtomicBool::new(false),
                auto_released: AtomicBool::new(false),
                completed: AtomicBool::new(false),
                hold_for,
            }),
        }
    }

    /// Blocks the caller until the test releases generation or the bounded
    /// negative-control timeout releases it on an independent OS thread.
    pub fn wait(&self) {
        if !self.state.started.swap(true, Ordering::SeqCst) {
            let state = Arc::clone(&self.state);
            thread::spawn(move || {
                let deadline = Instant::now() + state.hold_for;
                while !state.released.load(Ordering::SeqCst) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(1));
                }
                if !state.released.swap(true, Ordering::SeqCst) {
                    state.auto_released.store(true, Ordering::SeqCst);
                }
            });
        }
        while !self.state.released.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        self.state.completed.store(true, Ordering::SeqCst);
    }

    pub fn release(&self) {
        self.state.released.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn started(&self) -> bool {
        self.state.started.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn released(&self) -> bool {
        self.state.released.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn auto_released(&self) -> bool {
        self.state.auto_released.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn completed(&self) -> bool {
        self.state.completed.load(Ordering::SeqCst)
    }
}

/// A small source with one solid spawn pad, one hand-usable button, and one
/// deliberately slow column. Edits are retained outside the server's cache so
/// the test can observe the button's authoritative state without reading a
/// private server handle.
#[derive(Clone, Debug)]
pub struct LivenessWorld {
    gate: GenerationGate,
    edits: Arc<Mutex<BTreeMap<(i32, i32, i32), String>>>,
}

impl LivenessWorld {
    #[must_use]
    pub fn new(gate: GenerationGate) -> Self {
        Self {
            gate,
            edits: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn base_column(cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 128);
        if (cx, cz) == (0, 0) || (cx, cz) == (2, 0) {
            // The spawn search examines local (0, 0) first. A full stone floor
            // there yields feet at y=64; the button is one block away so it
            // does not obstruct the player's body at spawn.
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block(x, 63, z, "minecraft:stone");
                }
            }
            column.set_block(
                BUTTON_POS.0.rem_euclid(16),
                BUTTON_POS.1,
                BUTTON_POS.2.rem_euclid(16),
                BUTTON_OFF,
            );
        }
        column
    }

    fn apply_edits(&self, cx: i32, cz: i32, column: &mut ChunkColumn) {
        let edits = self.edits.lock().expect("world edits lock");
        for (&(x, y, z), state) in edits.iter() {
            if x.div_euclid(16) == cx && z.div_euclid(16) == cz {
                column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }
        }
    }

    #[must_use]
    pub fn state(&self, pos: (i32, i32, i32)) -> String {
        self.block_state(pos.0, pos.1, pos.2)
    }
}

impl ChunkSource for LivenessWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        if (cx, cz) == BLOCKED_COLUMN {
            self.gate.wait();
        }
        let mut column = Self::base_column(cx, cz);
        self.apply_edits(cx, cz, &mut column);
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let mut column = Self::base_column(cx, cz);
        self.apply_edits(cx, cz, &mut column);
        column
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        Self::base_column(cx, cz)
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.edits
            .lock()
            .expect("world edits lock")
            .insert((x, y, z), name.to_owned());
    }
}

#[derive(Clone, Debug, Default)]
pub struct ActionProbe {
    total: Arc<AtomicUsize>,
    client_actions: Arc<AtomicUsize>,
}

impl ActionProbe {
    pub fn record(&self) {
        self.total.fetch_add(1, Ordering::SeqCst);
    }

    pub fn record_client_action(&self) {
        self.total.fetch_add(1, Ordering::SeqCst);
        self.client_actions.fetch_add(1, Ordering::SeqCst);
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn client_action_count(&self) -> usize {
        self.client_actions.load(Ordering::SeqCst)
    }
}

/// Minimal protocol vocabulary used by the integration test. It intentionally
/// leaves ordinary server encoders at their defaults; only the chunk, block
/// update, and login markers needed by the test cross the in-memory wire.
#[derive(Clone, Debug)]
pub struct LivenessProtocol {
    pub actions: ActionProbe,
}

impl LivenessProtocol {
    #[must_use]
    pub fn new(actions: ActionProbe) -> Self {
        Self { actions }
    }
}

impl ServerProtocol for LivenessProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut reader = Reader::new(payload);
                ServerBound::LoginStart {
                    username: reader.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == USE_BUTTON => {
                self.actions.record();
                ServerBound::UseItemOn {
                    pos: BlockPos::new(BUTTON_POS.0, BUTTON_POS.1, BUTTON_POS.2),
                    face: BlockFace::Up,
                    cursor: Vec3f::new(0.5, 0.5, 0.5),
                    sequence: 0,
                    hand: 0,
                }
            }
            State::Play if packet_id == MOVE_PLAYER => {
                self.actions.record();
                let mut reader = Reader::new(payload);
                ServerBound::PlayerMoved {
                    x: reader.f64().expect("move x"),
                    y: reader.f64().expect("move y"),
                    z: reader.f64().expect("move z"),
                    rotation: None,
                    on_ground: reader.bool().expect("move grounded"),
                }
            }
            State::Play if packet_id == CLIENT_ACTION => {
                self.actions.record_client_action();
                ServerBound::ClientTickEnded
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: Vec::new(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
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

    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(cx);
        writer.var_i32(cz);
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(batch_size);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: &str) -> ServerDirective {
        let mut writer = Writer::default();
        writer.i32(x);
        writer.i32(y);
        writer.i32(z);
        writer.string(state);
        ServerDirective::Send {
            packet_id: BLOCK_UPDATE,
            payload: writer.as_slice().to_vec(),
        }
    }
}
