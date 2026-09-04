//! Hermetic differential proof for a generated falling-block action domain.
//!
//! The server side uses the public integrated-server tick counter, chunk source
//! and scheduled-tick feed. The expected side is an independent small model of
//! the bounded scenario: each of sand, red sand and gravel is scheduled two
//! ticks after placement, falls through air and lands on the fixed stone floor.
//! A deliberately wrong-read control proves that the comparison reports the
//! first bad tick.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lodestone_core::State;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ScheduledTickQueue, ServerBound,
    ServerDirective, ServerProtocol, TickPriority,
};
use uuid::Uuid;

const AIR: &str = "minecraft:air";
const SAND: &str = "minecraft:sand";
const RED_SAND: &str = "minecraft:red_sand";
const GRAVEL: &str = "minecraft:gravel";
const STONE: &str = "minecraft:stone";
const GRAVITY_TICK: &str = "gravity";
const FALLING_POS: (i32, i32, i32) = (0, 2, 0);
const LANDING_POS: (i32, i32, i32) = (0, 1, 0);
const FLOOR_POS: (i32, i32, i32) = (0, 0, 0);
const GRAVITY: f64 = 0.04;
const AIR_DRAG: f64 = 0.98;
const DELAY_AFTER_PLACE: u64 = 2;

#[derive(Clone)]
struct GravitySource {
    blocks: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl GravitySource {
    fn new() -> Self {
        let mut blocks = HashMap::new();
        blocks.insert(FLOOR_POS, STONE.to_owned());
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

impl ServerProtocol for GravityProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ChunkColumn,
    ) -> ServerDirective {
        ServerDirective::None
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

struct GravityServerOracle {
    server: IntegratedServer,
    source: GravitySource,
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
        let source_view = source.clone();
        let (server, _client) = {
            let _guard = runtime.enter();
            IntegratedServer::open_in_memory_with_mobs(
                GravityProtocol,
                source,
                (0..=0, 0..=0),
                (0, 0),
                0,
                0,
            )
        };
        let initial_tick = server
            .server_tick_count()
            .expect("gravity fixture must have a live tick loop");
        let feed = server
            .block_ticks()
            .expect("gravity fixture must expose its block-tick feed")
            .clone();
        Self {
            server,
            source: source_view,
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
        self.source.set_block(pos.0, pos.1, pos.2, state);
        if matches!(state.as_str(), SAND | RED_SAND | GRAVEL) {
            let mut pending = ScheduledTickQueue::new();
            pending.schedule(
                *pos,
                GRAVITY_TICK.to_owned(),
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
        let actual = self.source.block_state(pos.0, pos.1, pos.2);
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

#[derive(Default)]
struct GravityExpectedWorld {
    blocks: HashMap<(i32, i32, i32), String>,
    scheduled_tick: Option<u64>,
    falling_state: Option<String>,
    falling_y: Option<f64>,
    velocity_y: f64,
    tick: u64,
    wrong_read_after_tick: Option<u64>,
}

impl GravityExpectedWorld {
    fn new(wrong_read_after_tick: Option<u64>) -> Self {
        let mut world = Self {
            wrong_read_after_tick,
            ..Self::default()
        };
        world.blocks.insert(FLOOR_POS, STONE.to_owned());
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
                self.scheduled_tick = Some(self.tick + DELAY_AFTER_PLACE + 1);
                self.falling_state = Some(state.clone());
            }
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.tick += 1;
        if self.scheduled_tick == Some(self.tick) {
            self.scheduled_tick = None;
            if self.blocks.get(&FALLING_POS).is_some_and(|state| {
                matches!(state.as_str(), SAND | RED_SAND | GRAVEL)
            })
                && self
                    .blocks
                    .get(&(FALLING_POS.0, FALLING_POS.1 - 1, FALLING_POS.2))
                    .is_none_or(|state| state == AIR)
            {
                self.blocks.insert(FALLING_POS, AIR.to_owned());
                self.falling_y = Some(f64::from(FALLING_POS.1));
                self.velocity_y = 0.0;
            }
        }
        if let Some(y) = self.falling_y {
            let after_gravity = self.velocity_y - GRAVITY;
            self.velocity_y = after_gravity * AIR_DRAG;
            let next_y = y + after_gravity;
            if next_y <= f64::from(LANDING_POS.1) {
                self.blocks.insert(
                    LANDING_POS,
                    self.falling_state
                        .clone()
                        .expect("falling state is set when gravity is scheduled"),
                );
                self.falling_y = None;
            } else {
                self.falling_y = Some(next_y);
            }
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
