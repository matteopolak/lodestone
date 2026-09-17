//! Hermetic differential proof for a generated falling-block action domain.
//!
//! The server side uses the public integrated-server tick counter, chunk source
//! and scheduled-tick feed. The expected side is an independent small model of
//! the bounded scenario: each of sand, red sand and gravel is scheduled two
//! ticks after placement, falls through air and lands on the fixed stone floor.
//! A deliberately wrong-read control proves that the comparison reports the
//! first bad tick.

#[allow(dead_code)]
#[path = "support/differential_generation.rs"]
mod differential_generation;

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lodestone_core::{Reader, State, Writer};
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ScheduledTickKind, ScheduledTickQueue, ServerBound,
    ServerDirective, ServerProtocol, TickPriority,
};
use lodestone_net::{Connection, Transport};
use tokio::io::DuplexStream;
use uuid::Uuid;

use differential_generation::{GenerationDomain, SearchBudget, SearchOutcome, search_and_shrink};

const AIR: &str = "minecraft:air";
const SAND: &str = "minecraft:sand";
const RED_SAND: &str = "minecraft:red_sand";
const GRAVEL: &str = "minecraft:gravel";
const STONE: &str = "minecraft:stone";
const FALLING_POS: (i32, i32, i32) = (0, 2, 0);
const LANDING_POS: (i32, i32, i32) = (0, 1, 0);
const FLOOR_POS: (i32, i32, i32) = (0, 0, 0);
const GENERATED_FALLING_POSITIONS: [(i32, i32, i32); 2] = [(0, 2, 0), (2, 2, 0)];
const GRAVITY: f64 = 0.04;
const AIR_DRAG: f64 = 0.98;
const DELAY_AFTER_PLACE: u64 = 2;
const GENERATED_SETTLE_TICKS: u64 = 7;

#[derive(Clone)]
struct GravitySource {
    blocks: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl GravitySource {
    fn new() -> Self {
        let mut blocks = HashMap::new();
        for (x, _, z) in GENERATED_FALLING_POSITIONS {
            blocks.insert((x, FLOOR_POS.1, z), STONE.to_owned());
        }
        Self {
            blocks: Arc::new(Mutex::new(blocks)),
        }
    }
}

impl ChunkSource for GravitySource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        let blocks = self.blocks.lock().expect("gravity source lock");
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
            .expect("gravity source lock")
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
            .expect("gravity source lock")
            .insert((x, y, z), state.to_owned());
    }
}

struct GravityProtocol;

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
    let mut chunk_count = 0;
    loop {
        let (packet_id, _) = client
            .read_packet()
            .await
            .expect("join chunk read")
            .expect("join chunk packet");
        match packet_id {
            CHUNK => chunk_count += 1,
            CHUNK_BATCH_FINISHED => break,
            other => panic!("unexpected join packet {other}"),
        }
    }
    assert!(chunk_count > 0);
}

impl ServerProtocol for GravityProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut reader = Reader::new(payload);
                let username = reader.string(16).expect("gravity username");
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

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        let mut writer = Writer::default();
        writer.var_i32(_batch_size);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: writer.as_slice().to_vec(),
        }
    }
}

struct GravityServerOracle {
    server: IntegratedServer,
    _client: Connection<DuplexStream>,
    feed: lodestone_server::BlockTickFeed,
    next_server_tick: u64,
    runtime: tokio::runtime::Runtime,
}

impl GravityServerOracle {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("gravity fixture runtime");
        let source = GravitySource::new();
        let (server, client_io) = {
            let _guard = runtime.enter();
            IntegratedServer::open_in_memory_with_mobs(
                GravityProtocol,
                source,
                (-1..=1, -1..=1),
                (0, 0),
                0,
            )
        };
        let mut client = Connection::new(client_io);
        runtime.block_on(drive_login_and_join(&mut client, "gravity"));
        let seed_deadline = Instant::now() + Duration::from_secs(2);
        runtime.block_on(async {
            loop {
                let resident = (-1..=1).all(|cz| {
                    (-1..=1).all(|cx| {
                        server
                            .resident_block_state_id(cx * 16, FLOOR_POS.1, cz * 16)
                            .is_some()
                    })
                });
                if resident {
                    return;
                }
                assert!(
                    Instant::now() < seed_deadline,
                    "gravity fixture did not retain its 3x3 tick footprint"
                );
                tokio::task::yield_now().await;
            }
        });
        let initial_tick = server
            .server_tick_count()
            .expect("gravity fixture must have a live tick loop");
        let feed = server
            .block_ticks()
            .expect("gravity fixture must expose its block-tick feed")
            .clone();
        Self {
            server,
            _client: client,
            feed,
            next_server_tick: initial_tick + 1,
            runtime,
        }
    }

    fn wait_for_next_tick(&mut self) -> Result<(), String> {
        let target = self.next_server_tick;
        let deadline = Instant::now() + Duration::from_secs(2);
        let server = &self.server;
        self.runtime.block_on(async move {
            loop {
                if server.server_tick_count().is_some_and(|tick| tick >= target) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!("integrated gravity server did not reach tick {target}"));
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })?;
        self.next_server_tick = target + 1;
        Ok(())
    }
}

impl WorldOracle for GravityServerOracle {
    type Error = String;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        let Action::SetBlock { pos, state } = action else {
            return Err("gravity fixture accepts only SetBlock actions".to_owned());
        };
        if !matches!(state.as_str(), AIR | SAND | RED_SAND | GRAVEL | STONE) {
            return Err(format!("gravity fixture does not know {state}"));
        }
        let state_id = lodestone_data::block_states::StateId::from_state_str(state)
            .ok_or_else(|| format!("gravity fixture does not know {state}"))?;
        self.server
            .set_resident_block_state_id(pos.0, pos.1, pos.2, state_id)
            .map_err(|error| format!("set resident gravity block: {error}"))?;
        if matches!(state.as_str(), SAND | RED_SAND | GRAVEL) {
            let mut pending: ScheduledTickQueue<ScheduledTickKind> = ScheduledTickQueue::new();
            pending.schedule(
                *pos,
                ScheduledTickKind::Gravity,
                DELAY_AFTER_PLACE,
                TickPriority::Normal,
            );
            self.feed
                .request_scheduled_ticks(pending.drain_due(u64::MAX, usize::MAX));
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.wait_for_next_tick()
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self
            .server
            .resident_block_state_id(pos.0, pos.1, pos.2)
            .map(|state| state.canonical_state())
            .unwrap_or_else(|| AIR.to_owned());
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

#[derive(Default)]
struct GravityExpectedWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    scheduled_ticks: HashMap<(i32, i32, i32), u64>,
    falling: HashMap<(i32, i32, i32), FallingBlock>,
    tick: u64,
    wrong_read_after_tick: Option<u64>,
}

struct FallingBlock {
    state: String,
    y: f64,
    velocity_y: f64,
}

impl GravityExpectedWorld {
    fn new(wrong_read_after_tick: Option<u64>) -> Self {
        let mut world = Self {
            wrong_read_after_tick,
            ..Self::default()
        };
        for (x, _, z) in GENERATED_FALLING_POSITIONS {
            world.blocks.insert((x, FLOOR_POS.1, z), STONE.to_owned());
        }
        world
    }
}

impl WorldOracle for GravityExpectedWorld {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        if let Action::SetBlock { pos, state } = action {
            self.blocks.insert(*pos, state.clone());
            if matches!(state.as_str(), SAND | RED_SAND | GRAVEL) {
                // The feed is drained at the next server tick, then rebases
                // this relative delay onto that tick's counter.
                self.scheduled_ticks
                    .entry(*pos)
                    .or_insert(self.tick + DELAY_AFTER_PLACE + 1);
            }
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.tick += 1;
        let due = self
            .scheduled_ticks
            .iter()
            .filter_map(|(&pos, &scheduled_tick)| (scheduled_tick == self.tick).then_some(pos))
            .collect::<Vec<_>>();
        for pos in due {
            self.scheduled_ticks.remove(&pos);
            let state = self.blocks.get(&pos).cloned().unwrap_or_else(|| AIR.to_owned());
            let below = (pos.0, pos.1 - 1, pos.2);
            if matches!(state.as_str(), SAND | RED_SAND | GRAVEL)
                && self.blocks.get(&below).is_none_or(|state| state == AIR)
            {
                self.blocks.insert(pos, AIR.to_owned());
                self.falling.insert(
                    pos,
                    FallingBlock {
                        state,
                        y: f64::from(pos.1),
                        velocity_y: 0.0,
                    },
                );
            }
        }
        let mut landed = Vec::new();
        for (&origin, block) in &mut self.falling {
            let after_gravity = block.velocity_y - GRAVITY;
            block.velocity_y = after_gravity * AIR_DRAG;
            let next_y = block.y + after_gravity;
            if next_y <= f64::from(origin.1 - 1) {
                landed.push((origin, block.state.clone()));
            } else {
                block.y = next_y;
            }
        }
        for (origin, state) in landed {
            self.blocks.insert((origin.0, origin.1 - 1, origin.2), state);
            self.falling.remove(&origin);
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
            .is_some_and(|fault_tick| self.tick >= fault_tick && pos == FALLING_POS)
        {
            if actual == AIR { SAND } else { AIR }
        } else {
            actual
        };
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

fn script() -> Script {
    script_for(SAND)
}

fn script_for(state: &str) -> Script {
    Script::new(vec![ScriptStep {
        tick: 0,
        action: Action::SetBlock {
            pos: FALLING_POS,
            state: state.to_owned(),
        },
    }])
}

fn generated_scripts() -> [&'static str; 3] {
    [SAND, RED_SAND, GRAVEL]
}

fn generated_domain() -> GenerationDomain {
    GenerationDomain::new(
        GENERATED_FALLING_POSITIONS.to_vec(),
        vec![
            AIR.to_owned(),
            SAND.to_owned(),
            RED_SAND.to_owned(),
            GRAVEL.to_owned(),
        ],
        3,
        // `GravitySource` supplies fixture setup before the server retains a
        // column. Keep every generated edit in that pre-tick window; later
        // edits need the server's public world-mutation path instead.
        0,
    )
    .expect("the generated gravity domain is valid")
}

fn generated_budget() -> SearchBudget {
    SearchBudget {
        seed: 0x549_6a71,
        cases: 8,
        shrink_attempts: 32,
    }
}

fn generated_region() -> Vec<((i32, i32, i32), Vec<String>)> {
    let falling_states = vec![
        AIR.to_owned(),
        SAND.to_owned(),
        RED_SAND.to_owned(),
        GRAVEL.to_owned(),
    ];
    GENERATED_FALLING_POSITIONS
        .into_iter()
        .flat_map(|(x, y, z)| {
            [
                ((x, y, z), falling_states.clone()),
                ((x, y - 1, z), falling_states.clone()),
                ((x, y - 2, z), vec![STONE.to_owned(), AIR.to_owned()]),
            ]
        })
        .collect()
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    [
        (
            FALLING_POS,
            vec![AIR.to_owned(), SAND.to_owned(), RED_SAND.to_owned(), GRAVEL.to_owned()],
        ),
        (
            LANDING_POS,
            vec![AIR.to_owned(), SAND.to_owned(), RED_SAND.to_owned(), GRAVEL.to_owned()],
        ),
        (FLOOR_POS, vec![STONE.to_owned(), AIR.to_owned()]),
    ]
    .into_iter()
    .collect()
}

#[test]
fn falling_block_action_matches_the_live_integrated_server() {
    for state in generated_scripts() {
        let result = thread::spawn(move || {
            let mut expected = GravityExpectedWorld::new(None);
            let mut server = GravityServerOracle::new();
            run_differential(&script_for(state), &region(), &mut expected, &mut server, 12)
        })
        .join()
        .expect("falling-block differential thread");
        assert!(matches!(result, DifferentialOutcome::Agreed), "{state}: {result:?}");
    }
}

#[test]
fn generated_falling_block_sequences_match_the_integrated_server() {
    let outcome = thread::spawn(|| {
        search_and_shrink(
            &generated_domain(),
            generated_budget(),
            &generated_region(),
            GENERATED_SETTLE_TICKS,
            || (GravityExpectedWorld::new(None), GravityServerOracle::new()),
        )
    })
    .join()
    .expect("generated falling-block differential thread");

    assert!(
        matches!(outcome, SearchOutcome::NoDivergence { cases_run: 8 }),
        "the fixed generated gravity stream must agree with the independent arithmetic oracle: {outcome:?}"
    );
}

#[test]
fn generated_falling_block_sequences_shrink_a_wrong_read() {
    let outcome = thread::spawn(|| {
        search_and_shrink(
            &generated_domain(),
            SearchBudget {
                cases: 1,
                ..generated_budget()
            },
            &generated_region(),
            GENERATED_SETTLE_TICKS,
            || (GravityExpectedWorld::new(Some(1)), GravityServerOracle::new()),
        )
    })
    .join()
    .expect("generated falling-block detector thread");

    let SearchOutcome::Found(found) = outcome else {
        panic!("the generated falling-block detector must find its wrong read: {outcome:?}");
    };
    assert_eq!(found.minimal_divergence.tick, 0);
    assert_eq!(found.minimal_divergence.pos, FALLING_POS);
    assert!(
        found.minimal_script.steps.len() <= found.original_script.steps.len(),
        "shrinking must not add generated actions"
    );
}

#[test]
fn falling_block_control_reports_the_first_wrong_read() {
    let result = thread::spawn(|| {
        let mut expected = GravityExpectedWorld::new(Some(1));
        let mut server = GravityServerOracle::new();
        run_differential(&script(), &region(), &mut expected, &mut server, 0)
    })
    .join()
    .expect("falling-block differential control thread");
    match result {
        DifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.pos, FALLING_POS);
            assert_eq!(divergence.left.as_deref(), Some(AIR));
            assert_eq!(divergence.right.as_deref(), Some(SAND));
        }
        other => panic!("falling-block control did not diverge: {other:?}"),
    }
}
