//! Hermetic differential proof for two source blocks filling a dry slab.
//!
//! The server side uses the public integrated-server tick counter, retained
//! chunk source and scheduled-tick feed. The expected side is an independent
//! map for this finite action: opposing source blocks stay in place and the
//! dry slab between them becomes waterlogged. A deliberately wrong-read
//! control proves first-divergence reporting.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::io::DuplexStream;

use lodestone_core::{Reader, State, Writer};
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use lodestone_net::{Connection, Transport};
use lodestone_server::{
    BlockTickFeed, ChunkColumn, ChunkSource, IntegratedServer, ScheduledTickKind,
    ScheduledTickQueue, ServerBound, ServerDirective, ServerProtocol, TickPriority,
};
use uuid::Uuid;

const AIR: &str = "minecraft:air";
const WATER: &str = "minecraft:water[level=0]";
const STONE: &str = "minecraft:stone";
const SLAB_DRY: &str = "minecraft:oak_slab[type=bottom,waterlogged=false]";
const SLAB_WET: &str = "minecraft:oak_slab[type=bottom,waterlogged=true]";
const FLUID_TICK: &str = "lodestone:fluid";
// Keep the fixture inside the initial resident chunk so the delayed fluid
// callback can run without asking the tick loop to synchronously generate a
// cold column.
const SOURCE_POS: (i32, i32, i32) = (8, 1, 0);
const SLAB_POS: (i32, i32, i32) = (9, 1, 0);
const OTHER_SOURCE_POS: (i32, i32, i32) = (10, 1, 0);
const FLOOR_POS: (i32, i32, i32) = (8, 0, 0);
const SLAB_FLOOR_POS: (i32, i32, i32) = (9, 0, 0);
const OTHER_FLOOR_POS: (i32, i32, i32) = (10, 0, 0);
const FLUID_DELAY: u64 = 0;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const LOGIN_SUCCESS: i32 = 2;
const FINISH_CONFIGURATION: i32 = 3;
const SET_TIME_S2C: i32 = 43;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;

async fn drive_login_and_join<T: Transport>(client: &mut Connection<T>, username: &str) {
    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut writer = Writer::default();
    writer.string(username);
    client
        .write_packet(LOGIN_START, writer.as_slice())
        .await
        .expect("login start");

    let (packet_id, payload) = client
        .read_packet()
        .await
        .expect("login success read")
        .expect("login success packet");
    assert_eq!(packet_id, LOGIN_SUCCESS);
    let mut reader = Reader::new(&payload);
    assert_eq!(reader.string(16).expect("login username"), username);

    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (packet_id, _) = client
        .read_packet()
        .await
        .expect("join time read")
        .expect("join time packet");
    assert_eq!(packet_id, SET_TIME_S2C);
    let (packet_id, _) = client
        .read_packet()
        .await
        .expect("join batch start read")
        .expect("join batch start packet");
    assert_eq!(packet_id, CHUNK_BATCH_START);
    let (packet_id, _) = client
        .read_packet()
        .await
        .expect("join chunk read")
        .expect("join chunk packet");
    assert_eq!(packet_id, CHUNK);
    let (packet_id, _) = client
        .read_packet()
        .await
        .expect("join batch finish read")
        .expect("join batch finish packet");
    assert_eq!(packet_id, CHUNK_BATCH_FINISHED);
}

#[derive(Clone)]
struct WaterloggingSource {
    blocks: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl WaterloggingSource {
    fn new() -> Self {
        let mut blocks = HashMap::new();
        blocks.insert(FLOOR_POS, STONE.to_owned());
        blocks.insert(SLAB_FLOOR_POS, STONE.to_owned());
        blocks.insert(OTHER_FLOOR_POS, STONE.to_owned());
        for x in (SOURCE_POS.0 - 1)..=(OTHER_SOURCE_POS.0 + 1) {
            blocks.insert((x, SOURCE_POS.1, -1), STONE.to_owned());
            blocks.insert((x, SOURCE_POS.1, 1), STONE.to_owned());
        }
        blocks.insert((SOURCE_POS.0 - 1, SOURCE_POS.1, 0), STONE.to_owned());
        blocks.insert((OTHER_SOURCE_POS.0 + 1, SOURCE_POS.1, 0), STONE.to_owned());
        Self {
            blocks: Arc::new(Mutex::new(blocks)),
        }
    }
}

impl ChunkSource for WaterloggingSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        let blocks = self.blocks.lock().expect("waterlogging source lock");
        for (&(x, y, z), state) in blocks.iter() {
            if x.div_euclid(16) == cx && z.div_euclid(16) == cz {
                column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.blocks
            .lock()
            .expect("waterlogging source lock")
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| AIR.to_owned())
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, state: &str) {
        self.blocks
            .lock()
            .expect("waterlogging source lock")
            .insert((x, y, z), state.to_owned());
    }
}

struct WaterloggingProtocol;

impl ServerProtocol for WaterloggingProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut reader = Reader::new(payload);
                let username = reader.string(16).expect("waterlogging username");
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
        let mut writer = Writer::default();
        writer.string(username);
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: writer.as_slice().to_vec(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        let mut writer = Writer::default();
        writer.i64(game_time);
        match day_time {
            Some(day_time) => {
                writer.bool(true);
                writer.i64(day_time);
            }
            None => writer.bool(false),
        }
        ServerDirective::Send {
            packet_id: SET_TIME_S2C,
            payload: writer.as_slice().to_vec(),
        }
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: Vec::new(),
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
}

struct WaterloggingServerOracle {
    server: IntegratedServer,
    feed: BlockTickFeed,
    _client: Connection<DuplexStream>,
    next_server_tick: u64,
    first_advance: bool,
    runtime: tokio::runtime::Runtime,
}

impl WaterloggingServerOracle {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("waterlogging fixture runtime");
        let source = WaterloggingSource::new();
        let (server, client_io) = {
            let _guard = runtime.enter();
            IntegratedServer::open_in_memory_with_mobs(
                WaterloggingProtocol,
                source,
                (0..=0, 0..=0),
                (0, 0),
                0,
                0,
            )
        };
        let mut client = Connection::new(client_io);
        runtime.block_on(drive_login_and_join(&mut client, "waterlogging"));
        let initial_tick = server
            .server_tick_count()
            .expect("waterlogging fixture must have a live tick loop");
        let feed = server
            .block_ticks()
            .expect("waterlogging fixture must expose its block-tick feed")
            .clone();
        Self {
            server,
            feed,
            _client: client,
            next_server_tick: initial_tick + 1,
            first_advance: true,
            runtime,
        }
    }
}

impl WorldOracle for WaterloggingServerOracle {
    type Error = String;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        let Action::SetBlock { pos, state } = action else {
            return Err("waterlogging fixture accepts only SetBlock actions".to_owned());
        };
        if !matches!(state.as_str(), SLAB_DRY | WATER) {
            return Err(format!("waterlogging fixture does not know {state}"));
        }
        let state_id = lodestone_data::block_states::StateId::from_state_str(state)
            .ok_or_else(|| format!("waterlogging fixture does not know {state}"))?;
        self.server
            .set_resident_block_state_id(pos.0, pos.1, pos.2, state_id)
            .map_err(|error| format!("set resident waterlogging block: {error}"))?;
        if state == WATER {
            let mut pending = ScheduledTickQueue::new();
            pending.schedule(
                *pos,
                ScheduledTickKind::from_name(FLUID_TICK),
                FLUID_DELAY,
                TickPriority::Normal,
            );
            self.feed
                .request_fluid_scheduled_ticks(pending.drain_due(u64::MAX, usize::MAX));
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        // The feed is asynchronous: the first request may arrive before or
        // after the loop's next drain. Give that initial handoff one extra
        // server tick so this oracle's relative tick 0 always observes the
        // scheduled source reaction, independent of thread scheduling.
        let target = if self.first_advance {
            self.next_server_tick + 1
        } else {
            self.next_server_tick
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let server = &self.server;
        self.runtime.block_on(async move {
            loop {
                if server.server_tick_count().is_some_and(|tick| tick >= target) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!("integrated waterlogging server did not reach tick {target}"));
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })?;
        self.first_advance = false;
        self.next_server_tick = target + 1;
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self
            .server
            .resident_block_state_id(pos.0, pos.1, pos.2)
            .map(|state| state.canonical_state());
        Ok(actual.and_then(|actual| {
            candidates
                .iter()
                .find(|candidate| candidate.as_str() == actual)
                .cloned()
        }))
    }
}

struct WaterloggingExpectedWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    scheduled_tick: Option<u64>,
    tick: u64,
    wrong_read_after_tick: Option<u64>,
}

impl WaterloggingExpectedWorld {
    fn new(wrong_read_after_tick: Option<u64>) -> Self {
        let mut blocks = HashMap::new();
        blocks.insert(FLOOR_POS, STONE.to_owned());
        blocks.insert(SLAB_FLOOR_POS, STONE.to_owned());
        blocks.insert(OTHER_FLOOR_POS, STONE.to_owned());
        blocks.insert(SLAB_POS, SLAB_DRY.to_owned());
        Self {
            blocks,
            scheduled_tick: None,
            tick: 0,
            wrong_read_after_tick,
        }
    }
}

impl WorldOracle for WaterloggingExpectedWorld {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        if let Action::SetBlock { pos, state } = action {
            self.blocks.insert(*pos, state.clone());
            if state == WATER {
                self.scheduled_tick = Some(self.tick + FLUID_DELAY + 1);
            }
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.tick += 1;
        if self.scheduled_tick == Some(self.tick) {
            self.scheduled_tick = None;
            self.blocks.insert(SLAB_POS, SLAB_WET.to_owned());
        }
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self.blocks.get(&pos).map_or(AIR, String::as_str);
        let actual = if self
            .wrong_read_after_tick
            .is_some_and(|fault_tick| self.tick >= fault_tick && pos == SLAB_POS)
        {
            SLAB_DRY
        } else {
            actual
        };
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

fn script() -> Script {
    Script::new(vec![
        ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: SLAB_POS,
                state: SLAB_DRY.to_owned(),
            },
        },
        ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: SOURCE_POS,
                state: WATER.to_owned(),
            },
        },
        ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: OTHER_SOURCE_POS,
                state: WATER.to_owned(),
            },
        },
    ])
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    [
        (SOURCE_POS, vec![AIR.to_owned(), WATER.to_owned()]),
        (OTHER_SOURCE_POS, vec![AIR.to_owned(), WATER.to_owned()]),
        (SLAB_POS, vec![SLAB_DRY.to_owned(), SLAB_WET.to_owned()]),
        (FLOOR_POS, vec![STONE.to_owned(), AIR.to_owned()]),
        (SLAB_FLOOR_POS, vec![STONE.to_owned(), AIR.to_owned()]),
        (OTHER_FLOOR_POS, vec![STONE.to_owned(), AIR.to_owned()]),
    ]
    .into_iter()
    .collect()
}

#[test]
fn source_water_waterlogs_adjacent_slab() {
    let result = thread::spawn(|| {
        let mut expected = WaterloggingExpectedWorld::new(None);
        let mut server = WaterloggingServerOracle::new();
        run_differential(&script(), &region(), &mut expected, &mut server, 4)
    })
    .join()
    .expect("waterlogging differential thread");
    assert!(matches!(result, DifferentialOutcome::Agreed), "{result:?}");
}

#[test]
fn waterlogging_control_reports_the_first_wrong_read() {
    let result = thread::spawn(|| {
        let mut expected = WaterloggingExpectedWorld::new(Some(2));
        let mut server = WaterloggingServerOracle::new();
        run_differential(&script(), &region(), &mut expected, &mut server, 4)
    })
    .join()
    .expect("waterlogging differential control thread");
    match result {
        DifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 1);
            assert_eq!(divergence.pos, SLAB_POS);
            assert_eq!(divergence.left.as_deref(), Some(SLAB_DRY));
            assert_eq!(divergence.right.as_deref(), Some(SLAB_WET));
        }
        other => panic!("waterlogging control did not diverge: {other:?}"),
    }
}
