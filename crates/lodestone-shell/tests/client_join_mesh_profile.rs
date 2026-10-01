//! Bounded native profile for the client half of a singleplayer join.

#![cfg(not(target_arch = "wasm32"))]
#![recursion_limit = "256"]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use lodestone::config::{Config, Mode};
use lodestone::app::integrated_stream_radius;
use lodestone::gpu::{RenderState, SectionUploadOutcome};
use lodestone::menu::loading::ConnectPhase;
use lodestone::mesher::{MeshBacklog, record_native_mesh_upload_cost};
use lodestone::net::NetClient;
use lodestone::sim::Sim;
use lodestone_controller::Action;
use lodestone_ecs::Abilities;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};
use lodestone_time::Instant;
use lodestone_model::action::{
    ChatMode, ClientAction, ClientSettings, DisplayedSkinParts, MainHand, ParticleStatus,
};
use lodestone_model::Reported;
use lodestone_server::worldgen_progress::{WorldgenTimingPhase, WorldgenTimingSample, WorldgenTimingTotals};

const SEED: i64 = 4242;
const FLIGHT_TRAVEL_Y: f64 = 200.0;
const DEADLINE: Duration = Duration::from_secs(120);
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

type GenerationTimings = [WorldgenTimingTotals; WorldgenTimingPhase::ALL.len()];

static GENERATION_TIMINGS: Mutex<GenerationTimings> = Mutex::new([WorldgenTimingTotals {
    calls: 0,
    items: 0,
    elapsed: Duration::ZERO,
    maximum: Duration::ZERO,
}; WorldgenTimingPhase::ALL.len()]);

fn record_generation_timing(sample: WorldgenTimingSample) {
    GENERATION_TIMINGS.lock().unwrap()[sample.phase.index()].record(sample);
}

fn generation_timings_report(start: GenerationTimings, end: GenerationTimings) -> serde_json::Value {
    serde_json::json!(WorldgenTimingPhase::ALL.map(|phase| {
        let before = start[phase.index()];
        let after = end[phase.index()];
        serde_json::json!({
            "phase": phase.name(),
            "calls": after.calls.saturating_sub(before.calls),
            "items": after.items.saturating_sub(before.items),
            "elapsed_sum_ms": after.elapsed.saturating_sub(before.elapsed).as_secs_f64() * 1000.0,
        })
    }))
}

fn chunk_ingress_stats(sim: &Sim) -> Option<lodestone_client::ChunkIngressStats> {
    let shared = sim.net()?.shared_handle();
    Some(shared.get()?.chunk_ingress_stats())
}

fn chunk_ingress_report(
    start: lodestone_client::ChunkIngressStats,
    end: lodestone_client::ChunkIngressStats,
) -> serde_json::Value {
    serde_json::json!({
        "first_loads": end.first_loads.saturating_sub(start.first_loads),
        "replacements": end.replacements.saturating_sub(start.replacements),
        "terrain_unchanged": end.terrain_unchanged.saturating_sub(start.terrain_unchanged),
        "terrain_changed": end.terrain_changed.saturating_sub(start.terrain_changed),
        "comparison_elapsed_ms": end.comparison_ns.saturating_sub(start.comparison_ns) as f64 / 1_000_000.0,
        "comparison_max_lifetime_ms": end.comparison_max_ns as f64 / 1_000_000.0,
    })
}

struct EditProbe {
    aiming_since: Instant,
    target: Option<[i32; 3]>,
    initial_state: Option<u32>,
    clicked_at: Option<Instant>,
    changed: Option<Duration>,
    mesh_uploaded: Option<Duration>,
    presented: Option<Duration>,
}

struct DropProbe {
    previous_target: [i32; 3],
    existing_items: HashSet<i32>,
    target: Option<[i32; 3]>,
    initial_state: Option<u32>,
    clicked_at: Option<Instant>,
    changed_at: Option<Instant>,
    entity_at: Option<Instant>,
    stack_at: Option<Instant>,
    extracted_at: Option<Instant>,
    extracted_item_at: Option<Instant>,
    drawn_at: Option<Instant>,
    max_item_drops_drawn: usize,
    entity_id: Option<i32>,
    item: Option<String>,
}

impl DropProbe {
    fn new(sim: &Sim, previous_target: [i32; 3]) -> Self {
        let existing_items = sim.net().into_iter().flat_map(NetClient::entities)
            .filter(|entity| entity.entity_type.path() == "item")
            .map(|entity| entity.entity_id)
            .collect();
        Self {
            previous_target,
            existing_items,
            target: None,
            initial_state: None,
            clicked_at: None,
            changed_at: None,
            entity_at: None,
            stack_at: None,
            extracted_at: None,
            extracted_item_at: None,
            drawn_at: None,
            max_item_drops_drawn: 0,
            entity_id: None,
            item: None,
        }
    }

    fn observe(&mut self, sim: &mut Sim) {
        if self.clicked_at.is_none() {
            if let Some(hit) = sim.target()
                && hit.block[0] == self.previous_target[0]
                && hit.block[2] == self.previous_target[2]
                && hit.block[1] < self.previous_target[1]
            {
                let state = sim.chunk_world().read().block_state_at(
                    hit.block[0], hit.block[1], hit.block[2],
                );
                if state.is_some_and(|id| id != lodestone_data::block_states::StateId::AIR.raw()) {
                    self.target = Some(hit.block);
                    self.initial_state = state;
                    self.clicked_at = Some(Instant::now());
                    sim.begin_attack();
                }
            }
        } else if self.changed_at.is_none()
            && self.target.is_some_and(|block| {
                sim.chunk_world().read().block_state_at(block[0], block[1], block[2])
                    == Some(lodestone_data::block_states::StateId::AIR.raw())
            })
        {
            self.changed_at = Some(Instant::now());
            sim.end_attack();
        }
        let Some(target) = self.target else { return };
        let Some(net) = sim.net() else { return };
        for entity in net.entities() {
            if entity.entity_type.path() != "item"
                || self.existing_items.contains(&entity.entity_id)
                || (entity.position.x - f64::from(target[0])).abs() > 2.0
                || (entity.position.y - f64::from(target[1])).abs() > 2.0
                || (entity.position.z - f64::from(target[2])).abs() > 2.0
            {
                continue;
            }
            self.entity_at.get_or_insert_with(Instant::now);
            self.entity_id.get_or_insert(entity.entity_id);
            if let Reported::Reported(Some(stack)) = &entity.item {
                self.stack_at.get_or_insert_with(Instant::now);
                self.item.get_or_insert_with(|| stack.item.to_string());
            }
        }
        if self.clicked_at.is_some_and(|clicked| clicked.elapsed() >= Duration::from_secs(12))
            && self.drawn_at.is_none()
        {
            let mining = sim.ecs().read().resource::<lodestone::interact::MiningPredictor>().0.target();
            let net_state = self.target.and_then(|block| net.block_at(lodestone_model::BlockPos::new(block[0], block[1], block[2])));
            let first_state = net.block_at(lodestone_model::BlockPos::new(self.previous_target[0], self.previous_target[1], self.previous_target[2]));
            let first_client_state = sim.chunk_world().read().block_state_at(
                self.previous_target[0], self.previous_target[1], self.previous_target[2],
            );
            panic!("dropped item did not reach a presented frame: target={:?} initial={:?} client_changed={} net_state={:?} first_server_view={:?} first_client_state={:?} mining={mining:?} entity_target={:?} entity={} stack={} extracted={} extracted_item={} max_item_drops_drawn={} item={:?}",
                self.target, self.initial_state, self.changed_at.is_some(), net_state, first_state, first_client_state, sim.entity_target(), self.entity_at.is_some(), self.stack_at.is_some(), self.extracted_at.is_some(), self.extracted_item_at.is_some(), self.max_item_drops_drawn, self.item);
        }
    }
}

impl EditProbe {
    fn new() -> Self {
        Self {
            aiming_since: Instant::now(),
            target: None,
            initial_state: None,
            clicked_at: None,
            changed: None,
            mesh_uploaded: None,
            presented: None,
        }
    }
}

struct ColumnTimeline {
    position: (i32, i32),
    entered_at: Instant,
    left_view_at: Option<Instant>,
    loaded_at: Option<Instant>,
    halo_ready_at: Option<Instant>,
    first_mesh_at: Option<Instant>,
    presented_at: Option<Instant>,
    preloaded: bool,
    prepresented: bool,
    waiting_for_halo_on_entry: bool,
    missing_halo_on_entry: bool,
}

#[derive(Default)]
struct MovementColumnProbe {
    columns: Vec<ColumnTimeline>,
    last_poll: Option<Instant>,
}

impl MovementColumnProbe {
    fn enter_view(&mut self, sim: &Sim, old: (i32, i32), new: (i32, i32), radius: i32) {
        self.last_poll = None;
        self.observe(sim);
        self.observe_halos(sim);
        let entered_at = Instant::now();
        for timeline in &mut self.columns {
            let (x, z) = timeline.position;
            if timeline.left_view_at.is_none()
                && ((x - new.0).abs() > radius || (z - new.1).abs() > radius)
            {
                timeline.left_view_at = Some(entered_at);
            }
        }
        let loaded: HashSet<_> = sim
            .net()
            .map_or_else(Vec::new, NetClient::loaded_chunks)
            .into_iter()
            .map(|pos| (pos.x, pos.z))
            .collect();
        for z in new.1 - radius..=new.1 + radius {
            for x in new.0 - radius..=new.0 + radius {
                if (x - old.0).abs() <= radius && (z - old.1).abs() <= radius {
                    continue;
                }
                let preloaded = loaded.contains(&(x, z));
                let prepresented = preloaded && sim.resident_column_presented(x, z) == Some(true);
                let status = preloaded.then(|| sim.mesh_column_status(x, z)).flatten();
                let halo_ready = status.as_ref().is_some_and(|s| s.absent_halo.is_empty());
                self.columns.push(ColumnTimeline {
                    position: (x, z),
                    entered_at,
                    left_view_at: None,
                    loaded_at: preloaded.then_some(entered_at),
                    halo_ready_at: halo_ready.then_some(entered_at),
                    first_mesh_at: None,
                    presented_at: prepresented.then_some(entered_at),
                    preloaded,
                    prepresented,
                    waiting_for_halo_on_entry: status.as_ref().is_some_and(|s| s.waiting_for_halo),
                    missing_halo_on_entry: status.as_ref().is_some_and(|s| !s.absent_halo.is_empty()),
                });
            }
        }
        self.last_poll = None;
    }

    fn observe_halos(&mut self, sim: &Sim) {
        for timeline in &mut self.columns {
            let (x, z) = timeline.position;
            if timeline.left_view_at.is_none()
                && timeline.halo_ready_at.is_none()
                && sim.mesh_column_status(x, z).is_some_and(|s| s.absent_halo.is_empty())
            {
                timeline.halo_ready_at = Some(Instant::now());
            }
        }
    }

    fn mesh_result(&mut self, cx: i32, cz: i32) {
        if let Some(timeline) = self.columns.iter_mut().rev().find(|timeline| {
            timeline.position == (cx, cz) && timeline.left_view_at.is_none()
        }) && !timeline.prepresented {
            timeline.first_mesh_at.get_or_insert_with(Instant::now);
        }
    }

    fn observe(&mut self, sim: &Sim) {
        if self.last_poll.is_some_and(|last| last.elapsed() < Duration::from_millis(100)) {
            return;
        }
        self.last_poll = Some(Instant::now());
        let Some(net) = sim.net() else {
            return;
        };
        let loaded: HashSet<_> = net.loaded_chunks().into_iter().map(|pos| (pos.x, pos.z)).collect();
        for timeline in &mut self.columns {
            if timeline.left_view_at.is_some() || !loaded.contains(&timeline.position) {
                continue;
            }
            let now = Instant::now();
            timeline.loaded_at.get_or_insert(now);
            if timeline.presented_at.is_none()
                && sim.resident_column_presented(timeline.position.0, timeline.position.1) == Some(true)
            {
                timeline.presented_at = Some(now);
            }
        }
    }

    fn all_entered_report(&self, movement_stop: Option<Instant>) -> serde_json::Value {
        let stop = movement_stop.unwrap_or_else(Instant::now);
        let mut preloaded = 0;
        let mut prepresented = 0;
        let mut loaded_by_exit_or_stop = 0;
        let mut halo_ready_by_exit_or_stop = 0;
        let mut presented_by_exit_or_stop = 0;
        let mut left_unpresented = 0;
        let mut visible_unpresented_at_stop = 0;
        let mut enter_to_halo = Vec::new();
        let mut halo_to_first_mesh = Vec::new();
        let mut first_mesh_to_present = Vec::new();
        let mut enter_to_present = Vec::new();
        for timeline in &self.columns {
            let deadline = timeline.left_view_at.unwrap_or(stop);
            preloaded += usize::from(timeline.preloaded);
            prepresented += usize::from(timeline.prepresented);
            loaded_by_exit_or_stop += usize::from(timeline.loaded_at.is_some_and(|at| at <= deadline));
            if let Some(halo) = timeline.halo_ready_at.filter(|at| *at <= deadline) {
                halo_ready_by_exit_or_stop += 1;
                enter_to_halo.push(halo.duration_since(timeline.entered_at));
                if let Some(mesh) = timeline.first_mesh_at.filter(|at| *at >= halo && *at <= deadline)
                {
                    halo_to_first_mesh.push(mesh.duration_since(halo));
                }
            }
            if let Some(presented) = timeline.presented_at.filter(|at| *at <= deadline) {
                presented_by_exit_or_stop += 1;
                enter_to_present.push(presented.duration_since(timeline.entered_at));
                if let Some(mesh) = timeline.first_mesh_at.filter(|at| *at <= presented) {
                    first_mesh_to_present.push(presented.duration_since(mesh));
                }
            } else if timeline.left_view_at.is_some() {
                left_unpresented += 1;
            } else {
                visible_unpresented_at_stop += 1;
            }
        }
        let p = |samples: &[Duration], percent| {
            (!samples.is_empty()).then(|| percentile(samples, percent))
        };
        serde_json::json!({
            "entered": self.columns.len(),
            "preloaded_on_entry": preloaded,
            "prepresented_on_entry": prepresented,
            "loaded_by_exit_or_stop": loaded_by_exit_or_stop,
            "halo_ready_by_exit_or_stop": halo_ready_by_exit_or_stop,
            "presented_by_exit_or_stop": presented_by_exit_or_stop,
            "left_unpresented": left_unpresented,
            "visible_unpresented_at_stop": visible_unpresented_at_stop,
            "enter_to_halo_p50_ms": p(&enter_to_halo, 50),
            "enter_to_halo_p95_ms": p(&enter_to_halo, 95),
            "halo_to_first_mesh_p50_ms": p(&halo_to_first_mesh, 50),
            "halo_to_first_mesh_p95_ms": p(&halo_to_first_mesh, 95),
            "first_mesh_to_present_p50_ms": p(&first_mesh_to_present, 50),
            "first_mesh_to_present_p95_ms": p(&first_mesh_to_present, 95),
            "enter_to_present_p50_ms": p(&enter_to_present, 50),
            "enter_to_present_p95_ms": p(&enter_to_present, 95),
        })
    }

    fn report(
        &self,
        center: (i32, i32),
        radius: i32,
        movement_stop: Option<Instant>,
    ) -> serde_json::Value {
        let mut enter_to_load = Vec::new();
        let mut load_to_present = Vec::new();
        let mut enter_to_present = Vec::new();
        let mut enter_to_halo = Vec::new();
        let mut halo_to_first_mesh = Vec::new();
        let mut first_mesh_to_present = Vec::new();
        let mut visible = 0;
        let mut visible_loaded = 0;
        let mut visible_presented = 0;
        let mut presented_during_movement = 0;
        let mut preloaded = 0;
        let mut prepresented = 0;
        let mut waiting_for_halo_on_entry = 0;
        let mut missing_halo_on_entry = 0;
        let mut halo_ready = 0;
        let mut first_mesh_received = 0;
        for timeline in &self.columns {
            let (x, z) = timeline.position;
            if timeline.left_view_at.is_some()
                || (x - center.0).abs() > radius
                || (z - center.1).abs() > radius
            {
                continue;
            }
            visible += 1;
            preloaded += usize::from(timeline.preloaded);
            prepresented += usize::from(timeline.prepresented);
            waiting_for_halo_on_entry += usize::from(timeline.waiting_for_halo_on_entry);
            missing_halo_on_entry += usize::from(timeline.missing_halo_on_entry);
            if let Some(halo) = timeline.halo_ready_at {
                halo_ready += 1;
                enter_to_halo.push(halo.duration_since(timeline.entered_at));
            }
            if let Some(first_mesh) = timeline.first_mesh_at {
                first_mesh_received += 1;
                if let Some(halo) = timeline.halo_ready_at && first_mesh >= halo {
                    halo_to_first_mesh.push(first_mesh.duration_since(halo));
                }
            }
            if let Some(loaded) = timeline.loaded_at {
                visible_loaded += 1;
                if !timeline.preloaded {
                    enter_to_load.push(loaded.duration_since(timeline.entered_at));
                }
            }
            if let Some(presented) = timeline.presented_at {
                visible_presented += 1;
                enter_to_present.push(presented.duration_since(timeline.entered_at));
                if let Some(first_mesh) = timeline.first_mesh_at {
                    first_mesh_to_present.push(presented.duration_since(first_mesh));
                }
                if let Some(loaded) = timeline.loaded_at && !timeline.preloaded {
                    load_to_present.push(presented.duration_since(loaded));
                }
                if movement_stop.is_some_and(|stop| presented <= stop) {
                    presented_during_movement += 1;
                }
            }
        }
        let p = |samples: &[Duration], percent| {
            (!samples.is_empty()).then(|| percentile(samples, percent))
        };
        serde_json::json!({
            "new_columns_entered": self.columns.len(),
            "all_entered": self.all_entered_report(movement_stop),
            "still_visible_at_end": visible,
            "still_visible_loaded": visible_loaded,
            "still_visible_presented": visible_presented,
            "preloaded_on_entry": preloaded,
            "prepresented_on_entry": prepresented,
            "waiting_for_halo_on_entry": waiting_for_halo_on_entry,
            "missing_halo_on_entry": missing_halo_on_entry,
            "halo_ready": halo_ready,
            "first_mesh_received": first_mesh_received,
            "presented_during_movement": presented_during_movement,
            "enter_to_halo_p50_ms": p(&enter_to_halo, 50),
            "enter_to_halo_p95_ms": p(&enter_to_halo, 95),
            "halo_to_first_mesh_p50_ms": p(&halo_to_first_mesh, 50),
            "halo_to_first_mesh_p95_ms": p(&halo_to_first_mesh, 95),
            "first_mesh_to_present_p50_ms": p(&first_mesh_to_present, 50),
            "first_mesh_to_present_p95_ms": p(&first_mesh_to_present, 95),
            "enter_to_load_p50_ms": p(&enter_to_load, 50),
            "enter_to_load_p95_ms": p(&enter_to_load, 95),
            "load_to_present_p50_ms": p(&load_to_present, 50),
            "load_to_present_p95_ms": p(&load_to_present, 95),
            "enter_to_present_p50_ms": p(&enter_to_present, 50),
            "enter_to_present_p95_ms": p(&enter_to_present, 95),
        })
    }
}

fn enabled(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => {
            assert_eq!(value, "1", "{name} must be 1 when set");
            true
        }
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => panic!("invalid {name}: {error}"),
    }
}

fn movement_duration() -> Duration {
    let seconds = std::env::var("LODESTONE_CLIENT_JOIN_MOVE_SECONDS")
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .expect("LODESTONE_CLIENT_JOIN_MOVE_SECONDS must be an integer")
        })
        .unwrap_or(0);
    assert!(seconds <= 60, "movement profile must be at most 60 seconds");
    Duration::from_secs(seconds)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MovementMode {
    Walking,
    CreativeFlight,
}

fn movement_mode() -> MovementMode {
    match std::env::var("LODESTONE_CLIENT_JOIN_MOVE_MODE").as_deref() {
        Ok("flight") => MovementMode::CreativeFlight,
        Ok("walk") | Err(std::env::VarError::NotPresent) => MovementMode::Walking,
        other => panic!("LODESTONE_CLIENT_JOIN_MOVE_MODE must be walk or flight: {other:?}"),
    }
}

#[derive(Default)]
struct FlightStartup {
    requested_at: Option<Instant>,
    first_jump_tick: Option<u64>,
    first_jump_released: bool,
    second_jump_pressed: bool,
}

impl FlightStartup {
    fn advance(&mut self, sim: &mut Sim) -> bool {
        if self.requested_at.is_none() {
            sim.net().expect("connected flight profile").send_action(ClientAction::SendCommand {
                command: "gamemode creative".to_string(),
            });
            self.requested_at = Some(Instant::now());
            return false;
        }
        let abilities = sim.ecs().read().get::<Abilities>(sim.local_player()).copied();
        if abilities.is_some_and(|abilities| abilities.flying) {
            if sim.player().position.y < FLIGHT_TRAVEL_Y {
                sim.input_mut(|input| input.set(Action::Jump, true));
                return false;
            }
            sim.input_mut(|input| input.set(Action::Jump, false));
            return true;
        }
        assert!(self.requested_at.is_some_and(|at| at.elapsed() < Duration::from_secs(15)),
            "creative flight did not engage: abilities={abilities:?}");
        if !abilities.is_some_and(|abilities| abilities.may_fly) {
            return false;
        }
        match self.first_jump_tick {
            None => {
                self.first_jump_tick = Some(sim.tick_count());
                sim.input_mut(|input| input.set(Action::Jump, true));
            }
            Some(first) if sim.tick_count() > first && !self.first_jump_released => {
                self.first_jump_released = true;
                sim.input_mut(|input| input.set(Action::Jump, false));
            }
            Some(first) if sim.tick_count() > first + 1 && !self.second_jump_pressed => {
                self.second_jump_pressed = true;
                sim.input_mut(|input| input.set(Action::Jump, true));
            }
            _ => {}
        }
        false
    }
}

fn chunk_at(x: f64, z: f64) -> (i32, i32) {
    ((x / 16.0).floor() as i32, (z / 16.0).floor() as i32)
}

fn profile_radius() -> u32 {
    std::env::var("LODESTONE_CLIENT_JOIN_RADIUS")
        .ok()
        .map(|value| value.parse().expect("LODESTONE_CLIENT_JOIN_RADIUS must be an integer"))
        .unwrap_or(lodestone::config::DEFAULT_RENDER_DISTANCE)
}

fn target_size() -> (u32, u32) {
    std::env::var("LODESTONE_CLIENT_JOIN_TARGET_SIZE")
        .ok()
        .map(|value| {
            let (width, height) = value
                .split_once('x')
                .expect("LODESTONE_CLIENT_JOIN_TARGET_SIZE must be WIDTHxHEIGHT");
            let width: u32 = width.parse().expect("invalid target width");
            let height: u32 = height.parse().expect("invalid target height");
            assert!(width > 0 && height > 0, "target dimensions must be positive");
            (width, height)
        })
        .unwrap_or((64, 64))
}

fn percentile(samples: &[Duration], percent: usize) -> f64 {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    ordered[(ordered.len() * percent).div_ceil(100) - 1].as_secs_f64() * 1000.0
}

fn unsettled_columns(sim: &Sim, radius: u32) -> serde_json::Value {
    let columns = sim.unsettled_view_columns_at_radius(radius).unwrap_or_default();
    serde_json::json!(columns.iter().take(8).map(|status| serde_json::json!({
        "chunk": status.chunk,
        "missing_sections": status.missing_sections,
        "prior_presentations": status.prior_presentations,
        "waiting_for_halo": status.waiting_for_halo,
        "absent_halo": status.absent_halo,
    })).collect::<Vec<_>>())
}

fn profile_config(radius: u32) -> Config {
    Config {
        mode: Mode::Window,
        render_distance: radius,
        ..Config::default()
    }
}

fn main() {
    lodestone_server::worldgen_progress::install_timing_sink(record_generation_timing)
        .expect("the profile owns generation timing collection");
    if std::env::var_os("LODESTONE_JOIN_TRACE").is_some()
        || std::env::var_os("LODESTONE_CLIENT_JOIN_DROP").is_some()
    {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,lodestone_join_trace=info,lodestone_block_trace=debug")))
            .with_ansi(false)
            .try_init()
            .expect("join trace subscriber");
    }
    let radius = profile_radius();
    let move_for = movement_duration();
    let movement_mode = movement_mode();
    let measure_edit = enabled("LODESTONE_CLIENT_JOIN_EDIT");
    let measure_drop = enabled("LODESTONE_CLIENT_JOIN_DROP");
    let creative_edit = enabled("LODESTONE_CLIENT_JOIN_CREATIVE_EDIT");
    assert!(!measure_drop || measure_edit, "drop profiling requires the first block edit");
    assert!(!creative_edit || measure_edit, "creative edit profiling requires the block edit");
    assert!(!creative_edit || !measure_drop, "creative block edits do not produce item drops");
    let server_radius = integrated_stream_radius(radius);
    let expected_visible_columns = ((radius as usize) * 2 + 1).pow(2);
    let expected_server_columns = ((server_radius as usize) * 2 + 1).pow(2);
    let startup_started = Instant::now();
    let gpu_started = Instant::now();
    let ctx = GpuContext::new_headless_blocking().expect("a headless adapter is required");
    let gpu_context_ns = gpu_started.elapsed().as_nanos();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (target_width, target_height) = target_size();
    let mut target = HeadlessTarget::new(device, target_width, target_height, format);
    let sim_started = Instant::now();
    let mut sim = Sim::new(profile_config(radius));
    let sim_setup_ns = sim_started.elapsed().as_nanos();
    assert!(
        sim.vanilla_atlas().is_some(),
        "the client join profile requires the vanilla block atlas: {:?}",
        sim.asset_banner()
    );
    let protocol = Config::default().protocol;
    let server_protocol = lodestone_registry::server_protocol_for_protocol(protocol)
        .expect("the default protocol must have an integrated server");
    let render_started = Instant::now();
    let mut render = RenderState::new(
        device,
        queue,
        format,
        target_width,
        target_height,
        sim.vanilla_atlas(),
    );
    let render_setup_ns = render_started.elapsed().as_nanos();
    let startup_ns = startup_started.elapsed().as_nanos();
    let started = Instant::now();
    let open_started = Instant::now();
    let net = NetClient::open_singleplayer(
        server_protocol,
        protocol,
        SEED,
        lodestone::menu::create_world::WorldTypePreset::Normal,
        i32::try_from(server_radius).expect("profile radius fits i32"),
        true,
        Some((sim.ecs().clone(), sim.local_player())),
        None,
    );
    let open_ns = open_started.elapsed().as_nanos();
    sim.attach_net(net);
    sim.net()
        .expect("singleplayer client is attached")
        .send_action(ClientAction::SetClientSettings(ClientSettings {
            locale: "en_us".to_string(),
            view_distance: i8::try_from(server_radius).unwrap_or(i8::MAX),
            chat_mode: ChatMode::Full,
            chat_colors: true,
            skin_parts: DisplayedSkinParts {
                cape: true,
                jacket: true,
                left_sleeve: true,
                right_sleeve: true,
                left_pants_leg: true,
                right_pants_leg: true,
                hat: true,
            },
            main_hand: MainHand::Right,
            text_filtering: false,
            allow_server_listing: true,
            particle_status: ParticleStatus::All,
        }));
    sim.arm_new_world_loading(radius);
    let expected_initial_columns = sim
        .terrain_progress()
        .expect("the new-world loading view is declared")
        .expected;
    let mut previous_frame = Instant::now();
    let mut first_column = None;
    let mut first_mesh = None;
    let mut first_presented_terrain = None;
    let mut joining = None;
    let mut loading_terrain = None;
    let mut overlay_ready = None;
    let mut player_loaded_ack = None;
    let mut all_initial_columns = None;
    let mut all_initial_meshes_settled = None;
    let mut all_visible_columns = None;
    let mut all_server_columns = None;
    let mut all_visible_meshes_settled = None;
    let mut max_columns = 0usize;
    let mut max_settled_initial_columns = 0usize;
    let mut max_settled_visible_columns = 0usize;
    let mut max_pending = 0usize;
    let mut step_count = 0u64;
    let mut mesh_count = 0usize;
    let mut section_uploads = HashMap::<_, usize>::new();
    let mut unchanged_gpu_uploads = 0usize;
    let mut applied_gpu_uploads = 0usize;
    let mut failed_gpu_uploads = 0usize;
    let mut removed_sections = 0usize;
    let mut quad_count = 0usize;
    let mut step_ns = 0u128;
    let mut removal_ns = 0u128;
    let mut mesh_drain_ns = 0u128;
    let mut upload_ns = 0u128;
    let mut gpu_upload_ns = 0u128;
    let mut render_ns = 0u128;
    let mut frame_samples = Vec::new();
    let mut step_samples = Vec::new();
    let mut upload_samples = Vec::new();
    let mut render_samples = Vec::new();
    let mut movement_started: Option<Instant> = None;
    let mut flight_startup = FlightStartup::default();
    let mut flight_lost_at = None;
    let mut flight_lost_state = None;
    let mut movement_stopped: Option<Instant> = None;
    let mut movement_origin: Option<(f64, f64)> = None;
    let mut movement_start_y = None;
    let mut movement_end_y = None;
    let mut movement_start_teleports = None;
    let mut movement_end_teleports = None;
    let mut movement_end_position: Option<(f64, f64)> = None;
    let mut movement_first_effect = None;
    let mut movement_first_chunk_change = None;
    let mut movement_first_settled_view = None;
    let mut movement_post_stop_settled = None;
    let mut movement_last_chunk = None;
    let mut movement_chunk_changes = 0usize;
    let mut movement_columns = MovementColumnProbe::default();
    let mut movement_min_settled = usize::MAX;
    let mut movement_last_settled = 0usize;
    let mut movement_max_pending = 0usize;
    let mut movement_frames = Vec::new();
    let mut movement_steps = Vec::new();
    let mut movement_start_tick = None;
    let mut movement_end_tick = None;
    let mut movement_start_generation = None;
    let mut movement_end_generation = None;
    let mut movement_start_ingress = None;
    let mut movement_end_ingress = None;
    let mut movement_stop_view = None;
    let mut movement_stop_presentation = None;
    let mut movement_post_stop_presented = None;
    let mut movement_stop_backlog: Option<MeshBacklog> = None;
    let mut movement_settlement_tail = Vec::new();
    let mut movement_gap_snapshots = Vec::new();
    let mut last_gap_probe: Option<Instant> = None;
    let mut server_tick_at_ack = None;
    let mut server_tick_at_move_start = None;
    let mut server_tick_at_move_end = None;
    let mut server_tick_last_change: Option<(u64, Instant)> = None;
    let mut server_tick_max_gap = Duration::ZERO;
    let mut last_view_probe: Option<Instant> = None;
    let mut edit: Option<EditProbe> = None;
    let mut drop_probe: Option<DropProbe> = None;

    while started.elapsed() < DEADLINE {
        let frame_started = Instant::now();
        let dt = previous_frame.elapsed().as_secs_f64();
        previous_frame = frame_started;
        if let Some(start) = movement_started
            && movement_stopped.is_none()
            && start.elapsed() >= move_for
        {
            sim.input_mut(|input| {
                input.set(Action::Forward, false);
                input.set(Action::Sprint, false);
                input.set(Action::Jump, false);
            });
            let position = sim.player().position;
            movement_end_position = Some((position.x, position.z));
            movement_end_y = Some(position.y);
            movement_end_teleports = Some(sim.teleport_count);
            movement_end_tick = Some(sim.tick_count());
            movement_end_generation = Some(*GENERATION_TIMINGS.lock().unwrap());
            movement_end_ingress = chunk_ingress_stats(&sim);
            movement_stop_view = sim.view_settlement_at_radius(radius);
            movement_stop_presentation = sim.view_presentation_at_radius(radius);
            if movement_stop_presentation.is_some_and(|(resident, presented, expected)| {
                resident == expected && presented == expected
            }) {
                movement_post_stop_presented = Some(Duration::ZERO);
            }
            movement_stop_backlog = Some(sim.mesh_backlog());
            server_tick_at_move_end = sim
                .net()
                .and_then(NetClient::integrated_tick_monitor)
                .map(|monitor| monitor.snapshot());
            movement_stopped = Some(Instant::now());
        }
        let step_started = Instant::now();
        sim.step(dt);
        if movement_mode == MovementMode::CreativeFlight
            && let Some(start) = movement_started
            && movement_stopped.is_none()
            && flight_lost_at.is_none()
            && !sim.ecs().read().get::<Abilities>(sim.local_player()).is_some_and(|abilities| abilities.flying)
        {
            flight_lost_at = Some(start.elapsed());
            let player = sim.player();
            let position = player.position;
            let block_below = sim.chunk_world().read().block_state_at(
                position.x.floor() as i32,
                position.y.floor() as i32 - 1,
                position.z.floor() as i32,
            );
            flight_lost_state = Some(serde_json::json!({
                "position": [position.x, position.y, position.z],
                "on_ground": player.on_ground,
                "block_below": block_below,
                "teleport_count": sim.teleport_count,
            }));
        }
        sim.update_target(target_width as f32 / target_height as f32);
        if let Some(probe) = edit.as_mut() {
            if probe.clicked_at.is_none() {
                if let Some(hit) = sim.target() {
                    let state = sim.chunk_world().read().block_state_at(
                        hit.block[0], hit.block[1], hit.block[2],
                    );
                    if state.is_some_and(|id| id != lodestone_data::block_states::StateId::AIR.raw()) {
                        probe.target = Some(hit.block);
                        probe.initial_state = state;
                        probe.clicked_at = Some(Instant::now());
                        sim.begin_attack();
                    }
                }
            } else if probe.changed.is_none()
                && probe.target.is_some_and(|block| {
                    sim.chunk_world().read().block_state_at(block[0], block[1], block[2])
                        == Some(lodestone_data::block_states::StateId::AIR.raw())
                })
            {
                probe.changed = probe.clicked_at.map(|clicked| clicked.elapsed());
                sim.end_attack();
            }
            assert!(
                probe.aiming_since.elapsed() < Duration::from_secs(15) || probe.presented.is_some(),
                "block edit did not reach a presented mesh: target={:?} changed={:?} uploaded={:?}",
                probe.target,
                probe.changed,
                probe.mesh_uploaded,
            );
        }
        if let Some(probe) = drop_probe.as_mut() {
            probe.observe(&mut sim);
        }
        let step_elapsed = step_started.elapsed();
        step_ns += step_elapsed.as_nanos();
        step_samples.push(step_elapsed);
        if let Some((start, (origin_x, origin_z))) = movement_started.zip(movement_origin)
            && movement_stopped.is_none()
        {
            movement_steps.push(step_elapsed);
            let position = sim.player().position;
            let distance = (position.x - origin_x).hypot(position.z - origin_z);
            if distance > 0.01 {
                movement_first_effect.get_or_insert(start.elapsed());
            }
            let chunk = chunk_at(position.x, position.z);
            if movement_last_chunk != Some(chunk) {
                if let Some(previous) = movement_last_chunk {
                    movement_columns.enter_view(&sim, previous, chunk, radius as i32);
                }
                movement_chunk_changes += 1;
                movement_last_chunk = Some(chunk);
                movement_first_chunk_change.get_or_insert(start.elapsed());
            }
        }
        step_count += 1;

        match sim.connect_phase() {
            ConnectPhase::Connecting => {}
            ConnectPhase::Joining => {
                joining.get_or_insert(started.elapsed());
            }
            ConnectPhase::LoadingTerrain => {
                loading_terrain.get_or_insert(started.elapsed());
            }
        }

        let columns = sim.chunk_count();
        max_columns = max_columns.max(columns);
        if columns > 0 {
            first_column.get_or_insert(started.elapsed());
        }
        if columns >= expected_server_columns {
            all_server_columns.get_or_insert(started.elapsed());
        }
        max_pending = max_pending.max(sim.pending_meshes());
        if movement_started.is_some() {
            movement_columns.observe_halos(&sim);
        }
        let removal_started = Instant::now();
        for key in sim.drain_removals() {
            render.remove_section(&key);
            removed_sections += 1;
        }
        removal_ns += removal_started.elapsed().as_nanos();
        let mesh_started = Instant::now();
        let meshes = sim.drain_meshes();
        mesh_drain_ns += mesh_started.elapsed().as_nanos();
        let upload_started = Instant::now();
        let mut upload_count = 0;
        for meshed in meshes {
            let key = meshed.key;
            let mesh = &meshed.mesh;
            movement_columns.mesh_result(key.cx, key.cz);
            first_mesh.get_or_insert(started.elapsed());
            quad_count += mesh.quad_count();
            mesh_count += 1;
            *section_uploads.entry(key).or_default() += 1;
            let gpu_upload_started = Instant::now();
            match render.upload_meshed(device, queue, &meshed) {
                SectionUploadOutcome::Applied => applied_gpu_uploads += 1,
                SectionUploadOutcome::Unchanged => unchanged_gpu_uploads += 1,
                SectionUploadOutcome::Failed => failed_gpu_uploads += 1,
            }
            gpu_upload_ns += gpu_upload_started.elapsed().as_nanos();
            sim.mark_mesh_uploaded(key);
            upload_count += 1;
            if let Some(probe) = edit.as_mut()
                && probe.changed.is_some()
                && probe.mesh_uploaded.is_none()
                && probe.target.is_some_and(|block| {
                    let origin = key.origin();
                    (0..3).all(|axis| block[axis] >= origin[axis] && block[axis] < origin[axis] + 16)
                })
            {
                probe.mesh_uploaded = probe.clicked_at.map(|clicked| clicked.elapsed());
            }
        }
        let upload_elapsed = upload_started.elapsed();
        record_native_mesh_upload_cost(mesh_started.elapsed(), upload_count);
        sim.refresh_terrain_readiness();
        upload_ns += upload_elapsed.as_nanos();
        upload_samples.push(upload_elapsed);

        if sim.world_wait().is_none() {
            overlay_ready.get_or_insert(started.elapsed());
        }
        if all_initial_meshes_settled.is_none() {
            if let Some((resident, settled, expected)) = sim.visible_view_settlement() {
                assert_eq!(expected, expected_initial_columns);
                max_settled_initial_columns = max_settled_initial_columns.max(settled);
                if resident == expected {
                    all_initial_columns.get_or_insert(started.elapsed());
                }
                if settled >= expected {
                    all_initial_meshes_settled.get_or_insert(started.elapsed());
                }
            }
        }
        if player_loaded_ack.is_none()
            || last_view_probe.is_none_or(|last| last.elapsed() >= Duration::from_millis(100))
        {
            last_view_probe = Some(Instant::now());
            if let Some((resident, settled, expected)) = sim.view_settlement_at_radius(radius) {
                assert_eq!(expected, expected_visible_columns);
                max_settled_visible_columns = max_settled_visible_columns.max(settled);
                if resident == expected {
                    all_visible_columns.get_or_insert(started.elapsed());
                }
                if settled >= expected {
                    all_visible_meshes_settled.get_or_insert(started.elapsed());
                }
                if let Some(start) = movement_started {
                    if movement_stopped.is_none() {
                        movement_min_settled = movement_min_settled.min(settled);
                    }
                    movement_last_settled = settled;
                    if movement_first_chunk_change.is_some()
                        && resident == expected
                        && settled == expected
                    {
                        movement_first_settled_view.get_or_insert(start.elapsed());
                    }
                    if let Some(stop) = movement_stopped {
                        if movement_post_stop_presented.is_none()
                            && sim.view_presentation_at_radius(radius).is_some_and(
                                |(resident, presented, expected)| {
                                    resident == expected && presented == expected
                                },
                            )
                        {
                            movement_post_stop_presented = Some(stop.elapsed());
                        }
                        if movement_gap_snapshots.len() < 12
                            && last_gap_probe.is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
                        {
                            last_gap_probe = Some(Instant::now());
                            movement_gap_snapshots.push(serde_json::json!({
                                "elapsed_ms": stop.elapsed().as_secs_f64() * 1000.0,
                                "columns": unsettled_columns(&sim, radius),
                            }));
                        }
                        if movement_settlement_tail.len() < 64 {
                            let backlog = sim.mesh_backlog();
                            movement_settlement_tail.push(serde_json::json!({
                                "elapsed_ms": stop.elapsed().as_secs_f64() * 1000.0,
                                "resident": resident,
                                "settled": settled,
                                "ready_columns": backlog.ready_columns,
                                "waiting_columns": backlog.waiting_columns,
                                "forced_columns": backlog.forced_columns,
                                "pending_sections": backlog.pending_sections,
                            }));
                        }
                        if resident == expected && settled == expected {
                            movement_post_stop_settled.get_or_insert(stop.elapsed());
                        }
                    }
                }
            }
        }

        let render_started = Instant::now();
        let frame = target.acquire().expect("headless target acquire");
        let entity_draws = sim.entity_draws();
        let stats = render.render(
            device,
            queue,
            frame.view(),
            &sim.camera(target_width as f32 / target_height as f32),
            None,
            &entity_draws,
        );
        frame.present(queue);
        if let Some(probe) = drop_probe.as_mut() {
            probe.max_item_drops_drawn = probe.max_item_drops_drawn.max(stats.item_drops_drawn);
            if let Some(draw) = entity_draws.iter().find(|draw| Some(draw.id) == probe.entity_id) {
                probe.extracted_at.get_or_insert_with(Instant::now);
                if draw.item.is_some() {
                    probe.extracted_item_at.get_or_insert_with(Instant::now);
                    if stats.item_drops_drawn > 0 {
                        probe.drawn_at.get_or_insert_with(Instant::now);
                    }
                }
            }
        }
        if movement_started.is_some() {
            movement_columns.observe(&sim);
        }
        if let Some(probe) = edit.as_mut()
            && probe.mesh_uploaded.is_some()
            && probe.presented.is_none()
        {
            probe.presented = probe.clicked_at.map(|clicked| clicked.elapsed());
        }
        if measure_drop && drop_probe.is_none()
            && let Some(previous_target) = edit.as_ref()
                .filter(|probe| probe.presented.is_some())
                .and_then(|probe| probe.target)
        {
            drop_probe = Some(DropProbe::new(&sim, previous_target));
        }
        if sim.acknowledge_presented_initial_world() {
            player_loaded_ack = Some(started.elapsed());
            server_tick_at_ack = sim
                .net()
                .and_then(NetClient::integrated_tick_monitor)
                .map(|monitor| monitor.snapshot());
        }
        if player_loaded_ack.is_some()
            && let Some(monitor) = sim.net().and_then(NetClient::integrated_tick_monitor)
        {
            let count = monitor.server_tick_count();
            let now = Instant::now();
            match server_tick_last_change {
                Some((previous, changed)) if count != previous => {
                    server_tick_max_gap = server_tick_max_gap.max(now.duration_since(changed));
                    server_tick_last_change = Some((count, now));
                }
                Some((_, changed)) => {
                    server_tick_max_gap = server_tick_max_gap.max(now.duration_since(changed));
                }
                None => server_tick_last_change = Some((count, now)),
            }
        }
        let render_elapsed = render_started.elapsed();
        render_ns += render_elapsed.as_nanos();
        render_samples.push(render_elapsed);
        frame_samples.push(frame_started.elapsed());
        if movement_started.is_some() && movement_stopped.is_none() {
            movement_frames.push(frame_started.elapsed());
            movement_max_pending = movement_max_pending.max(sim.pending_meshes());
        }
        if first_presented_terrain.is_none() && (stats.sections_drawn > 0 || stats.water_sections_drawn > 0) {
            first_presented_terrain = Some(started.elapsed());
        }

        if creative_edit && player_loaded_ack.is_some() && flight_startup.requested_at.is_none() {
            sim.net().expect("connected creative edit profile").send_action(ClientAction::SendCommand {
                command: "gamemode creative".to_string(),
            });
            flight_startup.requested_at = Some(Instant::now());
        }
        let creative_ready = !creative_edit || sim.ecs().read().get::<Abilities>(sim.local_player())
            .is_some_and(|abilities| abilities.instabuild);
        if measure_edit && edit.is_none() && player_loaded_ack.is_some() && creative_ready {
            sim.player_mut(|player| player.pitch = 80.0);
            edit = Some(EditProbe::new());
        }

        if move_for > Duration::ZERO
            && movement_started.is_none()
            && overlay_ready.is_some()
            && first_presented_terrain.is_some()
            && (!measure_edit || edit.as_ref().is_some_and(|probe| probe.presented.is_some()))
            && (!measure_drop || drop_probe.as_ref().is_some_and(|probe| probe.drawn_at.is_some()))
            && (movement_mode == MovementMode::Walking || flight_startup.advance(&mut sim))
        {
            let position = sim.player().position;
            movement_origin = Some((position.x, position.z));
            movement_start_y = Some(position.y);
            movement_start_teleports = Some(sim.teleport_count);
            movement_last_chunk = Some(chunk_at(position.x, position.z));
            movement_start_tick = Some(sim.tick_count());
            movement_start_generation = Some(*GENERATION_TIMINGS.lock().unwrap());
            movement_start_ingress = chunk_ingress_stats(&sim);
            server_tick_at_move_start = sim
                .net()
                .and_then(NetClient::integrated_tick_monitor)
                .map(|monitor| monitor.snapshot());
            sim.input_mut(|input| {
                input.set(Action::Forward, true);
                input.set(Action::Sprint, true);
                input.set(Action::Jump, movement_mode == MovementMode::Walking);
            });
            movement_started = Some(Instant::now());
        }

        if overlay_ready.is_some()
            && all_server_columns.is_some()
            && all_visible_columns.is_some()
            && all_visible_meshes_settled.is_some()
            && first_presented_terrain.is_some()
            && (move_for == Duration::ZERO || movement_post_stop_settled.is_some())
            && (!measure_edit || edit.as_ref().is_some_and(|probe| probe.presented.is_some()))
            && (!measure_drop || drop_probe.as_ref().is_some_and(|probe| probe.drawn_at.is_some()))
        {
            break;
        }
        std::thread::sleep(FRAME_INTERVAL.saturating_sub(frame_started.elapsed()));
    }

    let elapsed = started.elapsed();
    let mesh_work = sim.mesh_work_counters();
    let mut uploads_per_section: Vec<_> = section_uploads.values().copied().collect();
    uploads_per_section.sort_unstable();
    let max_uploads_per_section = uploads_per_section.last().copied().unwrap_or(0);
    let p95_uploads_per_section = uploads_per_section
        .get((uploads_per_section.len() * 95).div_ceil(100).saturating_sub(1))
        .copied()
        .unwrap_or(0);
    let ms = |value: Option<Duration>| value.map(|d| d.as_secs_f64() * 1000.0);
    let movement_distance = movement_origin.map(|(x, z)| {
        let (end_x, end_z) = movement_end_position.unwrap_or_else(|| {
            let position = sim.player().position;
            (position.x, position.z)
        });
        (end_x - x).hypot(end_z - z)
    });
    let movement = movement_started.map(|start| serde_json::json!({
        "requested_seconds": move_for.as_secs(),
        "mode": match movement_mode {
            MovementMode::Walking => "walk",
            MovementMode::CreativeFlight => "flight",
        },
        "elapsed_ms": movement_stopped.map_or_else(|| start.elapsed(), |stop| stop.duration_since(start)).as_secs_f64() * 1000.0,
        "horizontal_distance_blocks": movement_distance,
        "start_y": movement_start_y,
        "end_y": movement_end_y,
        "flight_lost_after_ms": flight_lost_at.map(|elapsed: Duration| elapsed.as_secs_f64() * 1000.0),
        "flight_lost_state": flight_lost_state,
        "teleport_count_delta": movement_start_teleports.zip(movement_end_teleports).map(|(start, end)| end.saturating_sub(start)),
        "start_chunk": movement_origin.map(|(x, z)| chunk_at(x, z)),
        "end_chunk": movement_last_chunk,
        "first_position_effect_ms": ms(movement_first_effect),
        "first_chunk_change_ms": ms(movement_first_chunk_change),
        "first_shifted_view_settled_ms": ms(movement_first_settled_view),
        "post_stop_view_settle_ms": ms(movement_post_stop_settled),
        "post_stop_view_present_ms": ms(movement_post_stop_presented),
        "presentation_at_stop": movement_stop_presentation.map(|(resident, presented, expected)| serde_json::json!({
            "resident": resident,
            "presented": presented,
            "expected": expected,
        })),
        "view_at_stop": movement_stop_view.map(|(resident, settled, expected)| serde_json::json!({
            "resident": resident,
            "settled": settled,
            "expected": expected,
        })),
        "mesh_backlog_at_stop": movement_stop_backlog.map(|backlog| serde_json::json!({
            "ready_columns": backlog.ready_columns,
            "waiting_columns": backlog.waiting_columns,
            "forced_columns": backlog.forced_columns,
            "pending_sections": backlog.pending_sections,
        })),
        "settlement_tail": movement_settlement_tail,
        "gap_snapshots": movement_gap_snapshots,
        "chunk_changes": movement_chunk_changes,
        "new_view_columns": movement_columns.report(
            movement_last_chunk.expect("moving player has a chunk"),
            radius as i32,
            movement_stopped,
        ),
        "min_settled_columns": movement_min_settled.min(expected_visible_columns),
        "last_settled_columns": movement_last_settled,
        "max_pending_meshes": movement_max_pending,
        "sim_ticks": movement_end_tick.unwrap_or_else(|| sim.tick_count()).saturating_sub(movement_start_tick.unwrap_or(0)),
        "generation_phase_work": movement_start_generation.map(|start| generation_timings_report(
            start,
            movement_end_generation.unwrap_or_else(|| *GENERATION_TIMINGS.lock().unwrap()),
        )),
        "chunk_ingress": movement_start_ingress.zip(movement_end_ingress)
            .map(|(start, end)| chunk_ingress_report(start, end)),
        "frame_p99_ms": percentile(&movement_frames, 99),
        "frame_max_ms": percentile(&movement_frames, 100),
        "step_p99_ms": percentile(&movement_steps, 99),
        "step_max_ms": percentile(&movement_steps, 100),
        "frames_over_33ms": movement_frames.iter().filter(|elapsed| **elapsed > Duration::from_millis(33)).count(),
        "frames_over_100ms": movement_frames.iter().filter(|elapsed| **elapsed > Duration::from_millis(100)).count(),
    }));
    let server_tick_at_end = sim
        .net()
        .and_then(NetClient::integrated_tick_monitor)
        .map(|monitor| monitor.snapshot());
    let server_tick = serde_json::json!({
        "at_ack": server_tick_at_ack.map(|(_, count)| count),
        "at_move_start": server_tick_at_move_start.map(|(_, count)| count),
        "at_move_end": server_tick_at_move_end.map(|(_, count)| count),
        "at_end": server_tick_at_end.map(|(_, count)| count),
        "movement_tick_delta": server_tick_at_move_start.zip(server_tick_at_move_end).map(|((_, start), (_, end))| end.saturating_sub(start)),
        "movement_overrun_delta": server_tick_at_move_start.zip(server_tick_at_move_end).map(|((start, _), (end, _))| end.overrun_count.saturating_sub(start.overrun_count)),
        "tps_at_end": server_tick_at_end.map(|(stats, _)| stats.tps),
        "mspt_avg_at_end": server_tick_at_end.map(|(stats, _)| stats.mspt_avg_ms),
        "schedule_at_end": server_tick_at_end.map(|(stats, _)| serde_json::json!({
            "wait_count": stats.schedule.wait_count,
            "wake_p95_ms": stats.schedule.service_lateness_p95_ms,
            "wake_max_ms": stats.schedule.service_lateness_max_ms,
            "deadline_max_ms": stats.schedule.deadline_lateness_max_ms,
            "catch_up_ticks": stats.schedule.catch_up_ticks,
            "cooperative_yields": stats.schedule.cooperative_yields,
            "cooperative_yield_max_ms": stats.schedule.cooperative_yield_max_ms,
            "shed_ticks": stats.schedule.shed_ticks,
        })),
        "max_observed_tick_gap_ms": server_tick_max_gap.as_secs_f64() * 1000.0,
    });
    let edit = edit.map(|probe| serde_json::json!({
        "mode": if creative_edit { "creative" } else { "survival" },
        "target": probe.target,
        "initial_state": probe.initial_state,
        "aim_ms": probe.clicked_at.map(|clicked| clicked.duration_since(probe.aiming_since).as_secs_f64() * 1000.0),
        "input_to_air_ms": ms(probe.changed),
        "input_to_mesh_upload_ms": ms(probe.mesh_uploaded),
        "input_to_present_ms": ms(probe.presented),
    }));
    let drop_report = drop_probe.map(|probe| serde_json::json!({
        "target": probe.target,
        "initial_state": probe.initial_state,
        "item": probe.item,
        "input_to_air_ms": probe.clicked_at.zip(probe.changed_at).map(|(start, end)| end.duration_since(start).as_secs_f64() * 1000.0),
        "input_to_entity_ms": probe.clicked_at.zip(probe.entity_at).map(|(start, end)| end.duration_since(start).as_secs_f64() * 1000.0),
        "input_to_stack_ms": probe.clicked_at.zip(probe.stack_at).map(|(start, end)| end.duration_since(start).as_secs_f64() * 1000.0),
        "input_to_draw_ms": probe.clicked_at.zip(probe.drawn_at).map(|(start, end)| end.duration_since(start).as_secs_f64() * 1000.0),
    }));
    let report = serde_json::json!({
        "schema": "lodestone-client-join-mesh-profile-v24",
        "seed": SEED,
        "target_size": [target_width, target_height],
        "visible_radius": radius,
        "requested_server_radius": server_radius,
        "expected_initial_columns": expected_initial_columns,
        "expected_visible_columns": expected_visible_columns,
        "requested_server_columns": expected_server_columns,
        "startup_cpu_ms": startup_ns as f64 / 1_000_000.0,
        "gpu_context_cpu_ms": gpu_context_ns as f64 / 1_000_000.0,
        "sim_setup_cpu_ms": sim_setup_ns as f64 / 1_000_000.0,
        "render_setup_cpu_ms": render_setup_ns as f64 / 1_000_000.0,
        "timed_out": elapsed >= DEADLINE,
        "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
        "generation_phase_work": generation_timings_report(
            GenerationTimings::default(), *GENERATION_TIMINGS.lock().unwrap(),
        ),
        "chunk_ingress": chunk_ingress_stats(&sim).map(|end| chunk_ingress_report(
            lodestone_client::ChunkIngressStats::default(), end,
        )),
        "first_column_ms": ms(first_column),
        "first_mesh_ms": ms(first_mesh),
        "first_presented_terrain_ms": ms(first_presented_terrain),
        "joining_phase_ms": ms(joining),
        "loading_terrain_phase_ms": ms(loading_terrain),
        "overlay_ready_ms": ms(overlay_ready),
        "player_loaded_ack_ms": ms(player_loaded_ack),
        "all_initial_columns_ms": ms(all_initial_columns),
        "all_initial_meshes_settled_ms": ms(all_initial_meshes_settled),
        "all_visible_columns_ms": ms(all_visible_columns),
        "all_requested_server_columns_ms": ms(all_server_columns),
        "all_visible_meshes_settled_ms": ms(all_visible_meshes_settled),
        "max_loaded_columns": max_columns,
        "max_settled_initial_columns": max_settled_initial_columns,
        "max_settled_visible_columns": max_settled_visible_columns,
        "max_pending_meshes": max_pending,
        "steps": step_count,
        "meshes_uploaded": mesh_count,
        "unique_sections_uploaded": section_uploads.len(),
        "repeat_section_uploads": mesh_count.saturating_sub(section_uploads.len()),
        "gpu_uploads_applied": applied_gpu_uploads,
        "gpu_uploads_unchanged": unchanged_gpu_uploads,
        "gpu_uploads_failed": failed_gpu_uploads,
        "removed_sections": removed_sections,
        "uploads_per_section_p95": p95_uploads_per_section,
        "uploads_per_section_max": max_uploads_per_section,
        "mesh_work": {
            "native_scheduler": {
                "submitted": mesh_work.native_scheduler.submitted,
                "started": mesh_work.native_scheduler.started,
                "skipped_before_mesh": mesh_work.native_scheduler.skipped_before_mesh,
                "stale_results_discarded": mesh_work.native_scheduler.stale_results_discarded,
            },
            "column_arrivals": mesh_work.column_arrivals,
            "redecoded_column_arrivals": mesh_work.redecoded_column_arrivals,
            "column_snapshot_sections": mesh_work.column_snapshot_sections,
            "column_absorbed_light_sections": mesh_work.column_absorbed_light_sections,
            "neighbor_dirty_admissions": mesh_work.neighbor_dirty_admissions,
            "light_patch_calls": mesh_work.light_patch_calls,
            "light_patch_invalidations": mesh_work.light_patch_invalidations,
            "light_patch_boundary_skips": mesh_work.light_patch_boundary_skips,
            "light_patch_absorbed_sections": mesh_work.light_patch_absorbed_sections,
            "light_section_snapshots": mesh_work.light_section_snapshots,
        },
        "uploaded_quads": quad_count,
        "open_singleplayer_cpu_ms": open_ns as f64 / 1_000_000.0,
        "step_cpu_ms": step_ns as f64 / 1_000_000.0,
        "mesh_removal_cpu_ms": removal_ns as f64 / 1_000_000.0,
        "mesh_drain_cpu_ms": mesh_drain_ns as f64 / 1_000_000.0,
        "mesh_upload_cpu_ms": upload_ns as f64 / 1_000_000.0,
        "gpu_upload_cpu_ms": gpu_upload_ns as f64 / 1_000_000.0,
        "render_cpu_ms": render_ns as f64 / 1_000_000.0,
        "average_step_ms": step_ns as f64 / step_count.max(1) as f64 / 1_000_000.0,
        "frame_p95_ms": percentile(&frame_samples, 95),
        "frame_p99_ms": percentile(&frame_samples, 99),
        "frame_max_ms": percentile(&frame_samples, 100),
        "step_p99_ms": percentile(&step_samples, 99),
        "step_max_ms": percentile(&step_samples, 100),
        "upload_p99_ms": percentile(&upload_samples, 99),
        "upload_max_ms": percentile(&upload_samples, 100),
        "render_p99_ms": percentile(&render_samples, 99),
        "render_max_ms": percentile(&render_samples, 100),
        "frames_over_33ms": frame_samples.iter().filter(|elapsed| **elapsed > Duration::from_millis(33)).count(),
        "frames_over_100ms": frame_samples.iter().filter(|elapsed| **elapsed > Duration::from_millis(100)).count(),
        "movement": movement,
        "server_tick": server_tick,
        "edit": edit,
        "drop": drop_report,
    });
    println!("CLIENT_JOIN_MESH_PROFILE {report}");
    assert!(!report["timed_out"].as_bool().unwrap_or(true), "{report}");
    assert!(first_presented_terrain.is_some(), "no terrain reached a presented frame: {report}");
    assert!(player_loaded_ack.is_some(), "the new world was never acknowledged after presentation: {report}");
    assert!(server_tick_at_ack.is_some(), "the integrated server tick monitor was unavailable: {report}");
    if move_for > Duration::ZERO {
        assert!(movement_chunk_changes > 0, "movement never entered a new chunk: {report}");
    }
    if movement_mode == MovementMode::CreativeFlight {
        assert!(flight_lost_at.is_none(), "flight ended during streamed movement: {report}");
    }
    if measure_edit {
        assert!(report["edit"]["input_to_present_ms"].is_number(), "block edit never reached a presented frame: {report}");
    }
    if measure_drop {
        assert!(report["drop"]["input_to_draw_ms"].is_number(), "block drop never reached a presented frame: {report}");
    }
}

#[test]
fn already_presented_columns_exclude_replacement_mesh_latency() {
    let entered = Instant::now() - Duration::from_secs(1);
    let stop = entered + Duration::from_secs(2);
    let mut probe = MovementColumnProbe::default();
    for (position, prepresented) in [((0, 0), true), ((1, 0), false)] {
        probe.columns.push(ColumnTimeline {
            position,
            entered_at: entered,
            left_view_at: None,
            loaded_at: Some(entered),
            halo_ready_at: Some(entered),
            first_mesh_at: None,
            presented_at: prepresented.then_some(entered),
            preloaded: true,
            prepresented,
            waiting_for_halo_on_entry: false,
            missing_halo_on_entry: false,
        });
    }
    probe.mesh_result(0, 0);
    probe.mesh_result(1, 0);
    assert!(probe.columns[0].first_mesh_at.is_none());
    assert!(probe.columns[1].first_mesh_at.is_some());
    probe.columns.pop();
    for report in [probe.report((0, 0), 1, Some(stop)), probe.all_entered_report(Some(stop))] {
        assert_eq!(report["prepresented_on_entry"], 1);
        assert_eq!(report["enter_to_present_p95_ms"], 0.0);
        assert!(report["halo_to_first_mesh_p95_ms"].is_null());
        assert!(report["first_mesh_to_present_p95_ms"].is_null());
    }
}

#[test]
fn exited_columns_remain_in_movement_latency_census() {
    let entered = Instant::now() - Duration::from_secs(1);
    let exit = entered + Duration::from_millis(200);
    let mut probe = MovementColumnProbe::default();
    for (position, presented_at) in [
        ((0, 0), Some(entered + Duration::from_millis(50))),
        ((1, 0), None),
    ] {
        probe.columns.push(ColumnTimeline {
            position,
            entered_at: entered,
            left_view_at: Some(exit),
            loaded_at: Some(entered),
            halo_ready_at: Some(entered),
            first_mesh_at: presented_at,
            presented_at,
            preloaded: true,
            prepresented: false,
            waiting_for_halo_on_entry: false,
            missing_halo_on_entry: false,
        });
    }
    let report = probe.all_entered_report(Some(exit));
    assert_eq!(report["entered"], 2);
    assert_eq!(report["presented_by_exit_or_stop"], 1);
    assert_eq!(report["left_unpresented"], 1);
    assert_eq!(report["enter_to_present_p95_ms"], 50.0);
}

#[test]
#[ignore = "bounded client join profile; run with --nocapture under Samply"]
fn client_join_mesh_profile() {
    main();
}
