//! Deterministic integrated-server scenes for heavyweight profiling.
//!
//! The scene plan is the single source of truth for server-side setup actions,
//! ordered command phases, and the witness columns consumed by profiling tools.
//! It is intentionally serializable so a release-built server can hand the
//! exact same plan to a separate client-side runner.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use lodestone_time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use lodestone_core::{Reader, Writer};
use lodestone_net::Connection;
use tokio::io::DuplexStream;
use lodestone_model::BlockPos;
#[cfg(not(target_arch = "wasm32"))]
use lodestone_model::{ResourceKey, Vec3};

use crate::{
    BlockEntity, ChunkColumn, ChunkSource, IntegratedServer, MobHandle, NoEntities, ServerProtocol,
};
use crate::block_entities::SignData;

/// A bounded workload family exposed by the heavyweight profiling contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HeavyScenario {
    Palette,
    Transparency,
    Light,
    Liquid,
    Sign,
    BlockEntity,
    Entity,
    Scheduled,
    Mixed,
    DenseMixed,
}

impl HeavyScenario {
    /// The independently selectable scene families. Composite scenes reuse
    /// these builders so setup remains one normal command path.
    pub const FOCUSED: [Self; 8] = [
        Self::Palette,
        Self::Transparency,
        Self::Light,
        Self::Liquid,
        Self::Sign,
        Self::BlockEntity,
        Self::Entity,
        Self::Scheduled,
    ];

    pub const ALL: [Self; 10] = [
        Self::Palette,
        Self::Transparency,
        Self::Light,
        Self::Liquid,
        Self::Sign,
        Self::BlockEntity,
        Self::Entity,
        Self::Scheduled,
        Self::Mixed,
        Self::DenseMixed,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Palette => "palette",
            Self::Transparency => "transparency",
            Self::Light => "light",
            Self::Liquid => "liquid",
            Self::Sign => "sign",
            Self::BlockEntity => "block-entity",
            Self::Entity => "entity",
            Self::Scheduled => "scheduled",
            Self::Mixed => "mixed",
            Self::DenseMixed => "dense-mixed",
        }
    }

    #[must_use]
    pub fn parse_name(name: &str) -> Option<Self> {
        Some(match name {
            "palette" => Self::Palette,
            "transparency" => Self::Transparency,
            "light" => Self::Light,
            "liquid" => Self::Liquid,
            "sign" => Self::Sign,
            "block-entity" => Self::BlockEntity,
            "entity" => Self::Entity,
            "scheduled" => Self::Scheduled,
            "mixed" => Self::Mixed,
            "dense-mixed" => Self::DenseMixed,
            _ => return None,
        })
    }
}

/// The three ordered phases of a scene plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneCommandPhase {
    Setup,
    AfterJoin,
    Mutation,
}

impl SceneCommandPhase {
    pub const ALL: [Self; 3] = [Self::Setup, Self::AfterJoin, Self::Mutation];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::AfterJoin => "after_join",
            Self::Mutation => "mutation",
        }
    }
}

/// Errors raised before a scene can produce actions or runtime output.
#[derive(Debug, thiserror::Error)]
pub enum HeavyError {
    #[error("invalid heavy-scene argument: {0}")]
    Argument(String),
    #[error("heavy-scene I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("heavy-scene JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("heavy-scene peer failed: {0}")]
    Peer(String),
    #[error("heavy-scene deadline expired after {elapsed:?} in phase {phase} at action {action}")]
    Deadline {
        elapsed: std::time::Duration,
        phase: String,
        action: usize,
    },
    #[error("heavy-scene witness failed: {0}")]
    Witness(String),
    #[error("heavy-scene runtime scenario is not supported yet: {0}")]
    Unsupported(String),
}

/// Runtime phase selected by the release example.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerPhase {
    Ready,
    Steady,
    Mutate,
}

impl ServerPhase {
    fn parse(value: &str) -> Result<Self, HeavyError> {
        match value {
            "ready" => Ok(Self::Ready),
            "steady" => Ok(Self::Steady),
            "mutate" => Ok(Self::Mutate),
            _ => Err(HeavyError::Argument(format!("invalid phase {value:?}"))),
        }
    }
}

/// Camera arrangement recorded in runtime metadata for the client handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraPlan {
    Stationary,
    Orbit,
}

impl CameraPlan {
    fn parse(value: &str) -> Result<Self, HeavyError> {
        match value {
            "stationary" => Ok(Self::Stationary),
            "orbit" => Ok(Self::Orbit),
            _ => Err(HeavyError::Argument(format!("invalid camera plan {value:?}"))),
        }
    }
}

/// Arguments shared by the release executable and parser controls.
#[derive(Debug, Clone)]
pub struct HeavyServerArgs {
    pub emit_scene: Option<PathBuf>,
    pub spec: HeavySceneSpec,
    pub phase: ServerPhase,
    pub ticks: u64,
    pub output: PathBuf,
    pub wall_deadline: std::time::Duration,
    pub camera_plan: CameraPlan,
    pub smoke: bool,
}

impl HeavyServerArgs {
    pub fn parse_from<I, S>(arguments: I) -> Result<Self, HeavyError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = arguments.into_iter().map(Into::into);
        let _program = args.next();
        let mut emit_scene = None;
        let mut scenario = None;
        let mut seed: u64 = 1;
        let mut scale: u32 = 1;
        let mut phase = ServerPhase::Ready;
        let mut ticks: u64 = 0;
        let mut output = PathBuf::from("bench-results/heavy-server.jsonl");
        let mut wall_deadline_secs = 180u64;
        let mut camera_plan = CameraPlan::Stationary;
        let mut smoke = false;
        let mut saw_seed = false;
        let mut saw_scale = false;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--emit-scene" => {
                    emit_scene = Some(PathBuf::from(required_arg(&mut args, &flag)?));
                }
                "--scenario" => {
                    let name = required_arg(&mut args, &flag)?;
                    scenario = Some(HeavyScenario::parse_name(&name).ok_or_else(|| {
                        HeavyError::Argument(format!("invalid scenario {name:?}"))
                    })?);
                }
                "--seed" => {
                    let raw = required_arg(&mut args, &flag)?;
                    seed = raw
                        .parse()
                        .map_err(|_| HeavyError::Argument(format!("invalid seed {raw:?}")))?;
                    saw_seed = true;
                }
                "--scale" => {
                    let raw = required_arg(&mut args, &flag)?;
                    scale = raw
                        .parse()
                        .map_err(|_| HeavyError::Argument(format!("invalid scale {raw:?}")))?;
                    saw_scale = true;
                }
                "--phase" => phase = ServerPhase::parse(&required_arg(&mut args, &flag)?)?,
                "--ticks" => {
                    let raw = required_arg(&mut args, &flag)?;
                    ticks = raw
                        .parse()
                        .map_err(|_| HeavyError::Argument(format!("invalid ticks {raw:?}")))?;
                }
                "--output" => output = PathBuf::from(required_arg(&mut args, &flag)?),
                "--wall-deadline-secs" => {
                    let raw = required_arg(&mut args, &flag)?;
                    wall_deadline_secs = raw.parse().map_err(|_| {
                        HeavyError::Argument(format!("invalid wall deadline {raw:?}"))
                    })?;
                }
                "--camera-plan" => camera_plan = CameraPlan::parse(&required_arg(&mut args, &flag)?)?,
                "--smoke" => smoke = true,
                _ => return Err(HeavyError::Argument(format!("unknown argument {flag}"))),
            }
        }
        let scenario = scenario.ok_or_else(|| HeavyError::Argument("--scenario is required".to_string()))?;
        let spec = HeavySceneSpec::new(scenario, seed, scale)?;
        if !saw_seed && emit_scene.is_some() {
            return Err(HeavyError::Argument("--seed is required with --emit-scene".to_string()));
        }
        if !saw_scale && emit_scene.is_some() {
            return Err(HeavyError::Argument("--scale is required with --emit-scene".to_string()));
        }
        if wall_deadline_secs == 0 {
            return Err(HeavyError::Argument("wall deadline must be positive".to_string()));
        }
        if matches!(phase, ServerPhase::Mutate)
            && matches!(scenario, HeavyScenario::Scheduled | HeavyScenario::Liquid)
            && ticks == 0
        {
            return Err(HeavyError::Argument(
                "scheduled and liquid mutation require positive --ticks".to_string(),
            ));
        }
        Ok(Self {
            emit_scene,
            spec,
            phase,
            ticks,
            output,
            wall_deadline: std::time::Duration::from_secs(wall_deadline_secs),
            camera_plan,
            smoke,
        })
    }

    pub fn parse_env() -> Result<Self, HeavyError> {
        Self::parse_from(std::env::args())
    }

    #[must_use]
    pub fn for_test(scenario: HeavyScenario) -> Self {
        Self {
            emit_scene: None,
            spec: HeavySceneSpec::new(scenario, 1, 1).expect("test scale is valid"),
            phase: ServerPhase::Ready,
            ticks: 0,
            output: PathBuf::from("/tmp/lodestone-heavy-server-test.jsonl"),
            wall_deadline: std::time::Duration::from_secs(30),
            camera_plan: CameraPlan::Stationary,
            smoke: true,
        }
    }
}

/// Immutable inputs for one deterministic scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavySceneSpec {
    pub scenario: HeavyScenario,
    pub seed: u64,
    pub scale: u32,
}

/// A single client-side readiness requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessRequirement {
    pub segment: String,
    pub column: String,
    pub minimum: u64,
}

/// Commands grouped in their execution order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedSceneCommands {
    pub setup: Vec<String>,
    pub after_join: Vec<String>,
    pub mutation: Vec<String>,
}

/// Versioned, hashed handoff artifact consumed by the client profiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavyScenePlan {
    pub schema: u32,
    pub spec: HeavySceneSpec,
    pub commands: OrderedSceneCommands,
    pub witnesses: Vec<WitnessRequirement>,
    pub scene_hash: String,
}

/// Deterministic counters written by a heavyweight server run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavyCounts {
    pub join_columns: u64,
    pub chunk_batches: u64,
    pub chunk_payload_bytes: u64,
    pub sections: u64,
    pub distinct_states: u64,
    pub opaque_cells: u64,
    pub cutout_cells: u64,
    pub translucent_cells: u64,
    pub liquid_cells: u64,
    pub light_emitters: u64,
    pub light_cells_changed: u64,
    pub scheduled_enqueued: u64,
    pub scheduled_executed: u64,
    pub scheduled_remaining: u64,
    pub signs: u64,
    pub sign_vertices: u64,
    pub block_entity_records: u64,
    pub block_entity_draws: u64,
    pub entities_spawned: u64,
    pub entities_extracted: u64,
    pub entities_drawn: u64,
    pub liquid_meshes: u64,
    pub relight_remeshes: u64,
    pub server_ticks: u64,
}

pub type RequestedCounts = HeavyCounts;
pub type InstalledCounts = HeavyCounts;
pub type ConsumedCounts = HeavyCounts;

/// Versioned JSONL record for one finite server profiling run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeavyRunRecord {
    pub schema: u32,
    pub run_id: String,
    pub executable_kind: String,
    pub git_sha: String,
    pub platform: String,
    pub arch: String,
    pub pid: u32,
    pub scenario_hash: String,
    pub scenario: HeavyScenario,
    pub seed: u64,
    pub scale: u32,
    pub phase: ServerPhase,
    pub requested: RequestedCounts,
    pub installed: InstalledCounts,
    pub consumed: ConsumedCounts,
    pub setup_ms: u128,
    pub warmup_ms: u128,
    pub status: String,
    pub failure: Option<String>,
}

impl HeavyRunRecord {
    pub fn validate_ready(&self) -> Result<(), HeavyError> {
        let requirements = runtime_requirements_for_scenario(self.scenario);
        let mut missing = Vec::new();
        for requirement in &requirements {
            let observed = witness_value(&self.consumed, &requirement.column);
            if observed < requirement.minimum {
                missing.push(format!(
                    "{} {} observed {} minimum {}",
                    requirement.segment, requirement.column, observed, requirement.minimum
                ));
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(HeavyError::Witness(missing.join("; ")))
        }
    }
}

/// Readiness facts the server harness can actually observe. These are narrower
/// than the immutable client-plan witnesses: a ready-phase server run proves
/// that source cells were installed and encoded into the joined view, but it
/// cannot claim a client mesh submission or a mutation-time relight.
fn runtime_requirements_for_scenario(scenario: HeavyScenario) -> Vec<WitnessRequirement> {
    match scenario {
        HeavyScenario::Palette => vec![witness(
            "heavyweight.server-ready",
            "server.opaque_cells_encoded",
            1,
        )],
        HeavyScenario::Transparency => vec![witness(
            "heavyweight.server-ready",
            "server.translucent_cells_encoded",
            1,
        )],
        HeavyScenario::Light => vec![witness(
            "heavyweight.server-ready",
            "server.light_emitters_encoded",
            1,
        )],
        HeavyScenario::Liquid => vec![witness(
            "heavyweight.server-ready",
            "server.liquid_cells_encoded",
            1,
        )],
        HeavyScenario::Entity => vec![witness(
            "heavyweight.server-ready",
            "world.entities_drawn",
            1,
        )],
        HeavyScenario::Sign
        | HeavyScenario::BlockEntity
        | HeavyScenario::Scheduled
        | HeavyScenario::Mixed
        | HeavyScenario::DenseMixed => requirements_for_scenario(scenario),
    }
}

fn requirements_for_scenario(scenario: HeavyScenario) -> Vec<WitnessRequirement> {
    if matches!(scenario, HeavyScenario::Mixed | HeavyScenario::DenseMixed) {
        return HeavyScenario::FOCUSED
            .into_iter()
            .flat_map(requirements_for_scenario)
            .collect();
    }
    let indexes: &[usize] = match scenario {
        HeavyScenario::Palette => &[0],
        HeavyScenario::Transparency => &[1],
        HeavyScenario::Light => &[7, 8],
        HeavyScenario::Liquid => &[2],
        HeavyScenario::Sign => &[3],
        HeavyScenario::BlockEntity => &[4],
        HeavyScenario::Entity => &[5],
        HeavyScenario::Scheduled => &[6],
        HeavyScenario::Mixed | HeavyScenario::DenseMixed => {
            unreachable!("composite scenes are handled above")
        }
    };
    indexes
        .iter()
        .map(|index| {
            let (segment, column, minimum) = STATIC_WITNESSES[*index];
            witness(segment, column, minimum)
        })
        .collect()
}

fn witness_value(counts: &HeavyCounts, column: &str) -> u64 {
    match column {
        "server.opaque_cells_encoded" => counts.opaque_cells,
        "server.translucent_cells_encoded" => counts.translucent_cells,
        "server.light_emitters_encoded" => counts.light_emitters,
        "server.liquid_cells_encoded" => counts.liquid_cells,
        "world.opaque_sections_drawn" => counts.opaque_cells,
        "world.water_sections_drawn" => counts.liquid_cells,
        "world.translucent_sections_drawn" => counts.translucent_cells,
        "world.entities_drawn" => counts.entities_drawn,
        "world.block_entities_drawn" => counts.block_entity_draws,
        "world.sign_text_vertices" => counts.sign_vertices,
        "world.particles_drawn" => counts.scheduled_executed,
        "light.relight_cells_changed" => counts.light_cells_changed,
        "light.remesh_sections_submitted" => counts.relight_remeshes,
        _ => 0,
    }
}

#[derive(Serialize)]
struct CanonicalPlan<'a> {
    scenario: &'a str,
    seed: u64,
    scale: u32,
    setup: &'a [String],
    after_join: &'a [String],
    mutation: &'a [String],
    witnesses: &'a [WitnessRequirement],
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn required_arg<I>(args: &mut I, flag: &str) -> Result<String, HeavyError>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| HeavyError::Argument(format!("missing value for {flag}")))
}

const PALETTE_STATES: [&str; 12] = [
    "minecraft:stone",
    "minecraft:granite",
    "minecraft:diorite",
    "minecraft:andesite",
    "minecraft:deepslate",
    "minecraft:tuff",
    "minecraft:calcite",
    "minecraft:dripstone_block",
    "minecraft:oak_planks",
    "minecraft:spruce_planks",
    "minecraft:birch_planks",
    "minecraft:bricks",
];

const STATIC_WITNESSES: &[(&str, &str, u64)] = &[
    ("heavyweight.stationary", "world.opaque_sections_drawn", 1),
    ("heavyweight.stationary", "world.translucent_sections_drawn", 1),
    ("heavyweight.stationary", "world.water_sections_drawn", 1),
    ("heavyweight.stationary", "world.sign_text_vertices", 6),
    ("heavyweight.stationary", "world.block_entities_drawn", 1),
    ("heavyweight.stationary", "world.entities_drawn", 1),
    ("heavyweight.stationary", "world.particles_drawn", 1),
    ("heavyweight.mutation", "light.relight_cells_changed", 1),
    ("heavyweight.mutation", "light.remesh_sections_submitted", 1),
];

#[derive(Default)]
struct BuildOutput {
    setup: Vec<String>,
    after_join: Vec<String>,
    mutation: Vec<String>,
    witnesses: Vec<WitnessRequirement>,
}

fn subject_origin(index: u32) -> (i32, i32, i32) {
    let x = i32::try_from(index % 4).expect("bounded subject index") * 24 - 48;
    let z = i32::try_from(index / 4).expect("bounded subject index") * 24 - 48;
    (x, 64, z)
}

fn witness(segment: &str, column: &str, minimum: u64) -> WitnessRequirement {
    WitnessRequirement {
        segment: segment.to_string(),
        column: column.to_string(),
        minimum,
    }
}

fn add_witnesses(out: &mut BuildOutput, names: &[(&str, &str, u64)]) {
    out.witnesses
        .extend(names.iter().map(|(segment, column, minimum)| witness(segment, column, *minimum)));
}

fn build_palette(scale: u32, seed: u64) -> BuildOutput {
    let mut out = BuildOutput::default();
    out.setup.push("kill @e[tag=lodestone_heavy_scene]".to_string());
    let count = 64u32.saturating_mul(scale);
    for index in 0..count {
        let (x, y, z) = subject_origin(index % 16);
        let material = PALETTE_STATES[((index as u64 + seed) % PALETTE_STATES.len() as u64) as usize];
        out.setup.push(format!("setblock {} {} {} {}", x + (index % 8) as i32, y + (index / 8) as i32, z, material));
    }
    add_witnesses(&mut out, &[STATIC_WITNESSES[0]]);
    out
}

fn build_transparency(scale: u32) -> BuildOutput {
    let mut out = BuildOutput::default();
    let count = 48u32.saturating_mul(scale);
    for index in 0..count {
        let (x, y, z) = subject_origin(16 + index % 8);
        let block = if index % 2 == 0 { "minecraft:white_stained_glass" } else { "minecraft:glass_pane" };
        out.setup.push(format!("setblock {} {} {} {}", x + (index % 6) as i32, y + (index / 6) as i32, z, block));
    }
    add_witnesses(&mut out, &[STATIC_WITNESSES[1]]);
    out
}

fn build_light(scale: u32) -> BuildOutput {
    let mut out = BuildOutput::default();
    for index in 0..16u32.saturating_mul(scale) {
        let (x, y, z) = subject_origin(24 + index % 8);
        out.setup.push(format!("setblock {} {} {} minecraft:sea_lantern", x + (index % 4) as i32, y, z + (index / 4) as i32));
    }
    out.mutation.push("setblock -48 65 8 minecraft:sea_lantern".to_string());
    add_witnesses(&mut out, &[STATIC_WITNESSES[7], STATIC_WITNESSES[8]]);
    out
}

fn build_liquid(scale: u32) -> BuildOutput {
    let mut out = BuildOutput::default();
    for index in 0..32u32.saturating_mul(scale) {
        let (x, y, z) = subject_origin(32 + index % 8);
        out.setup.push(format!("setblock {} {} {} minecraft:water", x + (index % 8) as i32, y, z + (index / 8) as i32));
    }
    out.mutation.push("setblock -24 65 8 minecraft:water".to_string());
    add_witnesses(&mut out, &[STATIC_WITNESSES[2]]);
    out
}

fn sign_commands(scale: u32) -> Vec<String> {
    (0..24u32.saturating_mul(scale))
        .map(|index| {
            // A compact grid keeps the dense composition inside one normal
            // client render-distance view instead of making the sign count a
            // long, mostly culled strip.
            let x = -48 + i32::try_from(index % 48).expect("bounded sign column") * 2;
            let z = 24 + i32::try_from(index / 48).expect("bounded sign row") * 2;
            format!(
                "setblock {x} 65 {z} minecraft:oak_wall_sign[facing=north]{{front_text:{{has_glowing_text:1b,color:\"yellow\",messages:[{{text:\"HEAVY-{index:03}\"}},{{text:\"sign\"}},{{text:\"text\"}},{{text:\"witness\"}}]}}}}"
            )
        })
        .collect()
}

fn build_sign(scale: u32) -> BuildOutput {
    let mut out = BuildOutput::default();
    out.setup.extend(sign_commands(scale));
    add_witnesses(&mut out, &[STATIC_WITNESSES[3]]);
    out
}

fn block_entity_commands(scale: u32) -> Vec<String> {
    (0..4u32.saturating_mul(scale))
        .flat_map(|index| {
            let x = 64 + i32::try_from(index % 32).expect("bounded block entity column") * 2;
            let z = 24 + i32::try_from(index / 32).expect("bounded block entity row") * 2;
            [
                format!("setblock {x} 65 {z} minecraft:chest[facing=north]"),
                format!("setblock {x} 66 {z} minecraft:purple_shulker_box[facing=up]"),
                format!("setblock {x} 67 {z} minecraft:white_banner[rotation=0]"),
                format!("setblock {x} 68 {z} minecraft:conduit[waterlogged=false]"),
            ]
        })
        .collect()
}

fn build_block_entities(scale: u32) -> BuildOutput {
    let mut out = BuildOutput::default();
    out.setup.extend(block_entity_commands(scale));
    add_witnesses(&mut out, &[STATIC_WITNESSES[4]]);
    out
}

fn build_entities(scale: u32, seed: u64) -> BuildOutput {
    let mut out = BuildOutput::default();
    let count = 1024u32.saturating_mul(scale);
    for index in 0..count {
        let row = index / 32;
        let column = index % 32;
        let x = -48 + column as i32;
        let z = -48 + row as i32;
        let kind = match ((index as u64 + seed) % 4) as u8 {
            0 => "minecraft:pig",
            1 => "minecraft:cow",
            2 => "minecraft:sheep",
            _ => "minecraft:chicken",
        };
        out.setup.push(format!(
            "summon {kind} {x} 65 {z} {{NoAI:1b,NoGravity:1b,PersistenceRequired:1b,Tags:[\"lodestone_heavy_scene\",\"heavy_entity\"]}}"
        ));
    }
    add_witnesses(&mut out, &[STATIC_WITNESSES[5]]);
    out
}

fn build_scheduled(scale: u32) -> BuildOutput {
    let mut out = BuildOutput::default();
    for index in 0..8u32.saturating_mul(scale) {
        let x = -8 + (index % 4) as i32 * 2;
        let z = 40 + (index / 4) as i32 * 2;
        out.setup.push(format!(
            "setblock {x} 65 {z} minecraft:repeating_command_block[facing=up]{{auto:1b,Command:\"particle minecraft:flame ~ ~1 ~ 0 0 0 0 1\"}}"
        ));
    }
    out.mutation.push("setblock -8 65 40 minecraft:repeating_command_block[facing=up]".to_string());
    add_witnesses(&mut out, &[STATIC_WITNESSES[6]]);
    out
}

fn dense_scale(scenario: HeavyScenario, requested: u32) -> u32 {
    let multiplier = match scenario {
        HeavyScenario::Palette => 8,
        HeavyScenario::Transparency => 16,
        HeavyScenario::Light => 64,
        HeavyScenario::Liquid => 32,
        HeavyScenario::Sign => 64,
        HeavyScenario::BlockEntity => 32,
        HeavyScenario::Entity => 2,
        HeavyScenario::Scheduled => 64,
        HeavyScenario::Mixed | HeavyScenario::DenseMixed => {
            unreachable!("density is selected for focused builders")
        }
    };
    requested.saturating_mul(multiplier)
}

fn build_for_scenario(spec: &HeavySceneSpec) -> BuildOutput {
    if matches!(spec.scenario, HeavyScenario::Mixed | HeavyScenario::DenseMixed) {
        let mut mixed = BuildOutput::default();
        for scenario in HeavyScenario::FOCUSED {
            let scale = if spec.scenario == HeavyScenario::DenseMixed {
                dense_scale(scenario, spec.scale)
            } else {
                spec.scale
            };
            let part = build_for_scenario(&HeavySceneSpec {
                scenario,
                scale,
                ..spec.clone()
            });
            mixed.setup.extend(part.setup);
            mixed.after_join.extend(part.after_join);
            mixed.mutation.extend(part.mutation);
            mixed.witnesses.extend(part.witnesses);
        }
        return mixed;
    }
    match spec.scenario {
        HeavyScenario::Palette => build_palette(spec.scale, spec.seed),
        HeavyScenario::Transparency => build_transparency(spec.scale),
        HeavyScenario::Light => build_light(spec.scale),
        HeavyScenario::Liquid => build_liquid(spec.scale),
        HeavyScenario::Sign => build_sign(spec.scale),
        HeavyScenario::BlockEntity => build_block_entities(spec.scale),
        HeavyScenario::Entity => build_entities(spec.scale, spec.seed),
        HeavyScenario::Scheduled => build_scheduled(spec.scale),
        HeavyScenario::Mixed | HeavyScenario::DenseMixed => {
            unreachable!("composite scenes are handled before the scenario match")
        }
    }
}

impl HeavySceneSpec {
    pub const MAX_COMMAND_BYTES: usize = 32_000;
    pub const MAX_SCALE: u32 = 16;
    /// This composition deliberately fixes its high density at scale one. A
    /// second multiplier would turn one local Samply setup into more than
    /// fifteen thousand sequential server commands.
    pub const MAX_DENSE_MIXED_SCALE: u32 = 1;
    /// The live entity rehearsal keeps the per-process mob simulation bounded;
    /// larger scales remain available for immutable plan emission and an
    /// external client profile, but are rejected before a server starts.
    pub const MAX_RUNTIME_ENTITIES: usize = 2_048;

    pub fn new(scenario: HeavyScenario, seed: u64, scale: u32) -> Result<Self, HeavyError> {
        if scale == 0 {
            return Err(HeavyError::Argument("scale must be positive".to_string()));
        }
        if scale > Self::MAX_SCALE {
            return Err(HeavyError::Argument(format!(
                "scale must not exceed {}",
                Self::MAX_SCALE
            )));
        }
        if scenario == HeavyScenario::DenseMixed && scale > Self::MAX_DENSE_MIXED_SCALE {
            return Err(HeavyError::Argument(format!(
                "dense-mixed scale must not exceed {}",
                Self::MAX_DENSE_MIXED_SCALE
            )));
        }
        Ok(Self { scenario, seed, scale })
    }

    #[must_use]
    pub fn view_radius(&self) -> i32 {
        if self.scale == 1 { 1 } else { 2 }
    }

    #[must_use]
    pub fn expected_join_columns(&self) -> u64 {
        let radius = self.view_radius();
        u64::try_from((radius * 2 + 1).pow(2)).expect("positive view radius")
    }

    /// The smallest join view that includes each generated server-side
    /// producer. The public plan keeps its client-facing camera contract;
    /// runtime mode expands only its in-memory server view so its consumed
    /// counters can be tied to decoded chunk coordinates rather than to a
    /// source-global total.
    #[must_use]
    fn runtime_view_radius(&self) -> i32 {
        let producer_radius = match self.scenario {
            HeavyScenario::Transparency => 4,
            HeavyScenario::Light => 7,
            HeavyScenario::Liquid => 10,
            _ => 1,
        };
        self.view_radius().max(producer_radius)
    }

    #[must_use]
    fn expected_runtime_join_columns(&self) -> u64 {
        let radius = self.runtime_view_radius();
        u64::try_from((radius * 2 + 1).pow(2)).expect("positive runtime view radius")
    }

    pub fn build_plan(&self) -> Result<HeavyScenePlan, HeavyError> {
        let mut built = build_for_scenario(self);
        // The Python runner sends this phase only after the benchmark player
        // has joined. A top-down view covers the compact mixed volumes, so a
        // successful profile cannot be a populated but never-submitted scene.
        built.after_join.push("tp @a 0 220 0 0 90".to_string());
        let commands = OrderedSceneCommands {
            setup: built.setup,
            after_join: built.after_join,
            mutation: built.mutation,
        };
        let witnesses = built.witnesses;
        let canonical = CanonicalPlan {
            scenario: self.scenario.as_str(),
            seed: self.seed,
            scale: self.scale,
            setup: &commands.setup,
            after_join: &commands.after_join,
            mutation: &commands.mutation,
            witnesses: &witnesses,
        };
        let bytes = serde_json::to_vec(&canonical)?;
        Ok(HeavyScenePlan {
            schema: 1,
            spec: self.clone(),
            commands,
            witnesses,
            scene_hash: sha256_hex(&bytes),
        })
    }
}

impl HeavyScenePlan {
    pub fn json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("scene plan is serializable")
    }

    pub fn json_string(&self) -> String {
        serde_json::to_string(self).expect("scene plan is serializable")
    }
}

/// Writes one immutable scene plan, or emits it as the sole stdout object when
/// `destination` is `-`.
pub fn emit_scene(plan: &HeavyScenePlan, destination: &std::path::Path) -> Result<(), HeavyError> {
    let json = plan.json_string();
    if destination == std::path::Path::new("-") {
        println!("{json}");
    } else {
        std::fs::write(destination, format!("{json}\n"))?;
    }
    Ok(())
}

/// Counters owned by the deterministic source and observed by the harness.
#[derive(Debug, Default)]
struct HeavySourceStats {
    opaque_cells: std::sync::atomic::AtomicU64,
    translucent_cells: std::sync::atomic::AtomicU64,
    liquid_cells: std::sync::atomic::AtomicU64,
    light_emitters: std::sync::atomic::AtomicU64,
    state_names: Mutex<HashSet<String>>,
    by_column: Mutex<HashMap<(i32, i32), SourceColumnMetrics>>,
}

#[derive(Debug, Default)]
struct SourceColumnMetrics {
    opaque_cells: u64,
    translucent_cells: u64,
    liquid_cells: u64,
    light_emitters: u64,
    state_names: HashSet<String>,
}

/// A bounded, retained source used by the profiling harness. It is deliberately
/// a real [`ChunkSource`]: the integrated server owns chunk retention and the
/// version protocol owns encoding, while this source only supplies deterministic
/// terrain and records the work that crossed that boundary.
#[derive(Clone)]
pub struct HeavyChunkSource {
    columns: Arc<Mutex<HashMap<(i32, i32), ChunkColumn>>>,
    stats: Arc<HeavySourceStats>,
}

impl HeavyChunkSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            columns: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(HeavySourceStats::default()),
        }
    }

    fn fresh_column(cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for y in 0..64 {
            for x in 0..16 {
                for z in 0..16 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        // Keep each column distinguishable without introducing a dependency on
        // the scenario command interpreter.
        let marker = if (cx + cz).rem_euclid(3) == 0 {
            "minecraft:glass"
        } else {
            "minecraft:oak_planks"
        };
        column.set_block(cx.rem_euclid(16), 64, cz.rem_euclid(16), marker);
        column.set_block(0, 65, 0, "minecraft:chest");
        column.set_block(1, 65, 0, "minecraft:oak_sign");
        column.set_block_entities(vec![
            (
                BlockPos::new(cx * 16, 65, cz * 16),
                BlockEntity::Container {
                    id: "minecraft:chest".to_string(),
                    slots: vec![None; 27],
                },
            ),
            (
                BlockPos::new(cx * 16 + 1, 65, cz * 16),
                BlockEntity::Sign(SignData::default()),
            ),
        ]);
        column
    }

    fn column_for(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut columns = self.columns.lock().expect("heavy source lock");
        if let Some(column) = columns.get(&(cx, cz)) {
            return column.clone();
        }
        let column = Self::fresh_column(cx, cz);
        columns.insert((cx, cz), column.clone());
        column
    }

    fn stats(&self) -> SourceMetrics {
        let columns = self.columns.lock().expect("heavy source columns lock");
        SourceMetrics {
            retained_columns: columns.len() as u64,
            sections: columns.values().map(|column| column.section_count() as u64).sum(),
            opaque_cells: self.stats.opaque_cells.load(std::sync::atomic::Ordering::Relaxed),
            translucent_cells: self.stats.translucent_cells.load(std::sync::atomic::Ordering::Relaxed),
            liquid_cells: self.stats.liquid_cells.load(std::sync::atomic::Ordering::Relaxed),
            light_emitters: self.stats.light_emitters.load(std::sync::atomic::Ordering::Relaxed),
            distinct_states: self.stats.state_names.lock().expect("heavy source state lock").len() as u64,
        }
    }

    fn metrics_for_coordinates(&self, coordinates: &HashSet<(i32, i32)>) -> SourceMetrics {
        let columns = self.columns.lock().expect("heavy source columns lock");
        let by_column = self.stats.by_column.lock().expect("heavy source metric lock");
        let mut metrics = SourceMetrics::default();
        let mut states = HashSet::new();
        for coordinate in coordinates {
            if let Some(column) = columns.get(coordinate) {
                metrics.retained_columns += 1;
                metrics.sections += column.section_count() as u64;
            }
            if let Some(column_metrics) = by_column.get(coordinate) {
                metrics.opaque_cells += column_metrics.opaque_cells;
                metrics.translucent_cells += column_metrics.translucent_cells;
                metrics.liquid_cells += column_metrics.liquid_cells;
                metrics.light_emitters += column_metrics.light_emitters;
                states.extend(column_metrics.state_names.iter().cloned());
            }
        }
        metrics.distinct_states = states.len() as u64;
        metrics
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceMetrics {
    retained_columns: u64,
    sections: u64,
    opaque_cells: u64,
    translucent_cells: u64,
    liquid_cells: u64,
    light_emitters: u64,
    distinct_states: u64,
}

impl Default for SourceMetrics {
    fn default() -> Self {
        Self {
            retained_columns: 0,
            sections: 0,
            opaque_cells: 0,
            translucent_cells: 0,
            liquid_cells: 0,
            light_emitters: 0,
            distinct_states: 0,
        }
    }
}

impl ChunkSource for HeavyChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.column_for(cx, cz)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let column = self.column_for(x.div_euclid(16), z.div_euclid(16));
        column
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let mut columns = self.columns.lock().expect("heavy source lock");
        let column = columns
            .entry((cx, cz))
            .or_insert_with(|| Self::fresh_column(cx, cz));
        column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
        self.stats
            .state_names
            .lock()
            .expect("heavy source state lock")
            .insert(name.to_string());
        let mut by_column = self.stats.by_column.lock().expect("heavy source metric lock");
        let column_metrics = by_column.entry((cx, cz)).or_default();
        column_metrics.state_names.insert(name.to_string());
        if is_translucent_heavy_state(name) {
            self.stats
                .translucent_cells
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            column_metrics.translucent_cells += 1;
        } else if name == "minecraft:water" {
            self.stats
                .liquid_cells
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            column_metrics.liquid_cells += 1;
        } else if name == "minecraft:sea_lantern" {
            self.stats
                .light_emitters
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            column_metrics.light_emitters += 1;
        } else if name != "minecraft:air" {
            self.stats
                .opaque_cells
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            column_metrics.opaque_cells += 1;
        }
    }
}

/// The transparency scene's two generated state names. The source records
/// them at installation time and the harness only accepts the count after the
/// matching column coordinates have been decoded from chunk packets.
fn is_translucent_heavy_state(name: &str) -> bool {
    matches!(name, "minecraft:white_stained_glass" | "minecraft:glass_pane")
}

/// Drives one finite join against the production integrated server and records
/// the wire-level work needed by the heavyweight profiler.
pub struct HeavyServerHarness;

impl HeavyServerHarness {
    /// Runs the harness with a concrete protocol supplied by the release
    /// example or an integration test. Keeping the protocol generic avoids a
    /// version dependency cycle in the version-free server crate.
    pub async fn run<P>(
        args: HeavyServerArgs,
        plan: HeavyScenePlan,
        protocol: P,
    ) -> Result<HeavyRunRecord, HeavyError>
    where
        P: ServerProtocol + 'static,
    {
        let output = args.output.clone();
        let phase = args.phase;
        let scenario = args.spec.clone();
        let scenario_hash = plan.scene_hash.clone();
        if !matches!(
            args.spec.scenario,
            HeavyScenario::Palette
                | HeavyScenario::Transparency
                | HeavyScenario::Light
                | HeavyScenario::Liquid
                | HeavyScenario::Entity
        ) {
            let error = HeavyError::Unsupported(format!(
                "{} requires an integrated entity/tick producer that is not wired in this slice",
                args.spec.scenario.as_str()
            ));
            let _ = write_runtime_record(&output, &failed_record(&scenario, &scenario_hash, phase, &error));
            return Err(error);
        }
        if args.phase != ServerPhase::Ready {
            let error = HeavyError::Unsupported(
                "only --phase ready is supported until the integrated tick/command path is wired".to_string(),
            );
            let _ = write_runtime_record(&output, &failed_record(&scenario, &scenario_hash, phase, &error));
            return Err(error);
        }
        let started = Instant::now();
        let deadline = args.wall_deadline;
        match tokio::time::timeout(deadline, Self::run_inner(args, plan, protocol)).await {
            Ok(Ok(record)) => Ok(record),
            Ok(Err(error)) => {
                let _ = write_runtime_record(
                    &output,
                    &failed_record(&scenario, &scenario_hash, phase, &error),
                );
                Err(error)
            }
            Err(_) => {
                let error = HeavyError::Deadline {
                    elapsed: started.elapsed(),
                    phase: "join".to_string(),
                    action: 0,
                };
                let _ = write_runtime_record(
                    &output,
                    &failed_record(&scenario, &scenario_hash, phase, &error),
                );
                Err(error)
            }
        }
    }

    async fn run_inner<P>(
        args: HeavyServerArgs,
        plan: HeavyScenePlan,
        protocol: P,
    ) -> Result<HeavyRunRecord, HeavyError>
    where
        P: ServerProtocol + 'static,
    {
        let source = HeavyChunkSource::new();
        let stats_source = source.clone();
        apply_setblock_commands(&source, &plan.commands.setup);
        let entity_region = if plan.spec.scenario == HeavyScenario::Entity {
            // The spawn lattice is `[-48,-17]` on both axes. Leave a bounded
            // eight-block movement margin so the location witness remains
            // tied to this arena after a few real mob ticks.
            Some((-56.0, 0.0, -56.0, 0.0))
        } else {
            None
        };
        let (server, io, mob_handle, requested_entities) =
            if plan.spec.scenario == HeavyScenario::Entity {
                #[cfg(target_arch = "wasm32")]
                {
                    return Err(HeavyError::Unsupported(
                        "the entity runtime rehearsal requires the native mob simulation"
                            .to_string(),
                    ));
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let entity_actions = entity_spawn_actions(&plan.commands.setup)?;
                    if entity_actions.len() > HeavySceneSpec::MAX_RUNTIME_ENTITIES {
                        return Err(HeavyError::Unsupported(format!(
                            "entity runtime population {} exceeds the bounded maximum {}",
                            entity_actions.len(),
                            HeavySceneSpec::MAX_RUNTIME_ENTITIES
                        )));
                    }
                    let (server, io) = IntegratedServer::open_in_memory_with_mobs(
                        protocol,
                        source,
                        (-4..=3, -4..=3),
                        (0, 0),
                        0,
                        plan.spec.runtime_view_radius(),
                    );
                    let mobs = server.mobs().cloned().ok_or_else(|| {
                        HeavyError::Unsupported("entity runtime has no mob handle".to_string())
                    })?;
                    wait_for_mob_reseed(&mobs).await?;
                    if entity_actions.is_empty() {
                        server
                            .world_state()
                            .set_rule("spawn_mobs", "false")
                            .map_err(|error| {
                                HeavyError::Unsupported(format!(
                                    "entity negative control could not disable natural spawning: {error}"
                                ))
                            })?;
                    }
                    let mut spawned = 0u64;
                    for (entity_type, position) in &entity_actions {
                        if server.spawn_mob(entity_type.clone(), *position).is_none() {
                            return Err(HeavyError::Unsupported(
                                "entity runtime could not spawn through the live mob handle"
                                    .to_string(),
                            ));
                        }
                        spawned += 1;
                    }
                    // Let the production tick loop publish the handle's newly
                    // spawned snapshots through its live source before the join
                    // starts. This is a bounded hand-off, not a synthetic count:
                    // the subsequent packets still have to be decoded below.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    (server, io, Some(mobs), spawned)
                }
            } else {
                let (server, io) = IntegratedServer::open_in_memory_with_entities(
                    protocol,
                    source,
                    NoEntities,
                    plan.spec.runtime_view_radius(),
                );
                (server, io, None::<MobHandle>, 0)
            };
        let mut peer = Connection::new(io);
        let started = Instant::now();
        let join = drive_v770_join(
            &mut peer,
            plan.spec.expected_runtime_join_columns(),
            requested_entities,
            entity_region,
        )
        .await;
        let join = join;
        let installed_entities = mob_handle
            .as_ref()
            .map_or(0, |mobs| mobs.with(|sim| sim.snapshots().len() as u64));
        let server_ticks = server.server_tick_count().unwrap_or(0);
        drop(peer);
        server.shutdown().await;
        let (
            join_columns,
            batches,
            payload_bytes,
            sent_coordinates,
            entity_packets,
            entity_positions_in_region,
        ) = join?;
        let metrics = stats_source.stats();
        let consumed_metrics = stats_source.metrics_for_coordinates(&sent_coordinates);
        let (requested, installed, consumed) = counts_for(
            &plan,
            join_columns,
            batches,
            payload_bytes,
            metrics,
            consumed_metrics,
            server_ticks,
            requested_entities,
            installed_entities,
            entity_packets,
        );
        let record = HeavyRunRecord {
            schema: 1,
            run_id: format!("heavy-{}-{}", plan.spec.as_str(), std::process::id()),
            executable_kind: "heavy-scene-server".to_string(),
            git_sha: option_env!("GIT_SHA").unwrap_or("unknown").to_string(),
            platform: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            pid: std::process::id(),
            scenario_hash: plan.scene_hash.clone(),
            scenario: plan.spec.scenario,
            seed: plan.spec.seed,
            scale: plan.spec.scale,
            phase: args.phase,
            requested,
            installed,
            consumed,
            setup_ms: started.elapsed().as_millis(),
            warmup_ms: 0,
            status: "complete".to_string(),
            failure: None,
        };
        if let Err(error) = record.validate_ready() {
            return Err(HeavyError::Witness(format!(
                "{error}; installed opaque_cells={}; consumed opaque_cells={}; installed translucent_cells={}; consumed translucent_cells={}; installed liquid_cells={}; consumed liquid_cells={}; installed light_emitters={}; consumed light_emitters={}; installed entities_spawned={}; consumed entities_extracted={}; entity_positions_in_region={}; server_ticks={}; chunk_payload_bytes={}",
                record.installed.opaque_cells,
                record.consumed.opaque_cells,
                record.installed.translucent_cells,
                record.consumed.translucent_cells,
                record.installed.liquid_cells,
                record.consumed.liquid_cells,
                record.installed.light_emitters,
                record.consumed.light_emitters,
                record.installed.entities_spawned,
                record.consumed.entities_extracted,
                entity_positions_in_region,
                record.consumed.server_ticks,
                record.consumed.chunk_payload_bytes
            )));
        }
        if plan.spec.scenario == HeavyScenario::Entity
            && entity_positions_in_region < requested_entities
        {
            return Err(HeavyError::Witness(format!(
                "entity position witness observed {entity_positions_in_region} in the planned region, minimum {requested_entities}"
            )));
        }
        write_runtime_record(&args.output, &record)?;
        Ok(record)
    }
}

fn write_runtime_record(destination: &std::path::Path, record: &HeavyRunRecord) -> Result<(), HeavyError> {
    if let Some(parent) = destination.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record)?;
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(destination)?;
    writeln!(file, "{line}")?;
    file.flush().map_err(HeavyError::Io)
}

fn failed_record(
    spec: &HeavySceneSpec,
    scenario_hash: &str,
    phase: ServerPhase,
    error: &HeavyError,
) -> HeavyRunRecord {
    HeavyRunRecord {
        schema: 1,
        run_id: format!("heavy-{}-{}", spec.as_str(), std::process::id()),
        executable_kind: "heavy-scene-server".to_string(),
        git_sha: option_env!("GIT_SHA").unwrap_or("unknown").to_string(),
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        pid: std::process::id(),
        scenario_hash: scenario_hash.to_string(),
        scenario: spec.scenario,
        seed: spec.seed,
        scale: spec.scale,
        phase,
        requested: HeavyCounts::default(),
        installed: HeavyCounts::default(),
        consumed: HeavyCounts::default(),
        setup_ms: 0,
        warmup_ms: 0,
        status: "failed".to_string(),
        failure: Some(error.to_string()),
    }
}

fn counts_for(
    plan: &HeavyScenePlan,
    join_columns: u64,
    batches: u64,
    payload_bytes: u64,
    metrics: SourceMetrics,
    consumed_metrics: SourceMetrics,
    server_ticks: u64,
    requested_entities: u64,
    installed_entities: u64,
    consumed_entities: u64,
) -> (HeavyCounts, HeavyCounts, HeavyCounts) {
    let mut requested_cells = HeavyCounts::default();
    for name in plan
        .commands
        .setup
        .iter()
        .filter_map(|command| setblock_state_name(command))
    {
        if is_translucent_heavy_state(name) {
            requested_cells.translucent_cells += 1;
        } else if name == "minecraft:water" {
            requested_cells.liquid_cells += 1;
        } else if name == "minecraft:sea_lantern" {
            requested_cells.light_emitters += 1;
        } else if name != "minecraft:air" {
            requested_cells.opaque_cells += 1;
        }
    }
    let requested = HeavyCounts {
        join_columns: plan.spec.expected_runtime_join_columns(),
        opaque_cells: requested_cells.opaque_cells,
        translucent_cells: requested_cells.translucent_cells,
        liquid_cells: requested_cells.liquid_cells,
        light_emitters: requested_cells.light_emitters,
        entities_spawned: requested_entities,
        ..HeavyCounts::default()
    };
    let installed = HeavyCounts {
        join_columns: metrics.retained_columns,
        sections: metrics.sections,
        distinct_states: metrics.distinct_states,
        opaque_cells: metrics.opaque_cells,
        translucent_cells: metrics.translucent_cells,
        liquid_cells: metrics.liquid_cells,
        light_emitters: metrics.light_emitters,
        entities_spawned: installed_entities,
        ..HeavyCounts::default()
    };
    let consumed = HeavyCounts {
        join_columns,
        chunk_batches: batches,
        chunk_payload_bytes: payload_bytes,
        sections: consumed_metrics.sections,
        distinct_states: consumed_metrics.distinct_states,
        opaque_cells: consumed_metrics.opaque_cells,
        translucent_cells: consumed_metrics.translucent_cells,
        liquid_cells: consumed_metrics.liquid_cells,
        light_emitters: consumed_metrics.light_emitters,
        entities_extracted: consumed_entities,
        entities_drawn: consumed_entities,
        server_ticks,
        ..HeavyCounts::default()
    };
    (requested, installed, consumed)
}

#[cfg(not(target_arch = "wasm32"))]
fn entity_spawn_actions(commands: &[String]) -> Result<Vec<(ResourceKey, Vec3)>, HeavyError> {
    commands
        .iter()
        .filter(|command| command.starts_with("summon "))
        .map(|command| {
            let mut fields = command.split_whitespace();
            let _ = fields.next();
            let kind = fields
                .next()
                .ok_or_else(|| HeavyError::Argument("summon is missing an entity type".to_string()))?;
            let x = fields
                .next()
                .ok_or_else(|| HeavyError::Argument("summon is missing x".to_string()))?
                .parse::<f64>()
                .map_err(|_| HeavyError::Argument("summon x is not numeric".to_string()))?;
            let y = fields
                .next()
                .ok_or_else(|| HeavyError::Argument("summon is missing y".to_string()))?
                .parse::<f64>()
                .map_err(|_| HeavyError::Argument("summon y is not numeric".to_string()))?;
            let z = fields
                .next()
                .ok_or_else(|| HeavyError::Argument("summon is missing z".to_string()))?
                .parse::<f64>()
                .map_err(|_| HeavyError::Argument("summon z is not numeric".to_string()))?;
            let entity_type = ResourceKey::from_str(kind)
                .map_err(|_| HeavyError::Argument(format!("invalid summon entity type {kind:?}")))?;
            Ok((entity_type, Vec3::new(x, y, z)))
        })
        .collect()
}

#[cfg(not(target_arch = "wasm32"))]
async fn wait_for_mob_reseed(mobs: &MobHandle) -> Result<(), HeavyError> {
    let started = Instant::now();
    let limit = std::time::Duration::from_secs(5);
    while started.elapsed() < limit {
        if mobs.with(|sim| sim.next_id()) >= 1000 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Err(HeavyError::Deadline {
        elapsed: started.elapsed(),
        phase: "entity-seed".to_string(),
        action: 0,
    })
}

fn apply_setblock_commands(source: &HeavyChunkSource, commands: &[String]) {
    for command in commands {
        let mut fields = command.split_whitespace();
        if fields.next() != Some("setblock") {
            continue;
        }
        let (Some(raw_x), Some(raw_y), Some(raw_z), Some(raw_name)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(x), Ok(y), Ok(z)) = (raw_x.parse(), raw_y.parse(), raw_z.parse()) else {
            continue;
        };
        let name = raw_name.split(['[', '{']).next().unwrap_or(raw_name);
        source.set_block(x, y, z, name);
    }
}

fn setblock_state_name(command: &str) -> Option<&str> {
    let mut fields = command.split_whitespace();
    if fields.next() != Some("setblock") {
        return None;
    }
    let _x = fields.next()?;
    let _y = fields.next()?;
    let _z = fields.next()?;
    let name = fields.next()?;
    Some(name.split(['[', '{']).next().unwrap_or(name))
}

async fn drive_v770_join(
    peer: &mut Connection<DuplexStream>,
    expected_columns: u64,
    expected_entities: u64,
    entity_region: Option<(f64, f64, f64, f64)>,
) -> Result<(u64, u64, u64, HashSet<(i32, i32)>, u64, u64), HeavyError> {
    const HANDSHAKE: i32 = 0;
    const LOGIN_HELLO: i32 = 0;
    const LOGIN_COMPRESSION: i32 = 3;
    const LOGIN_FINISHED: i32 = 2;
    const LOGIN_ACKNOWLEDGED: i32 = 3;
    const CONFIG_FINISH: i32 = 3;
    const PLAY_PLAYER_LOADED: i32 = 44;
    const PLAY_MOVE_PLAYER_POS: i32 = 30;
    const PLAY_CLIENT_TICK_END: i32 = 13;
    const PLAY_CHUNK_BATCH_FINISHED: i32 = 11;
    const PLAY_CHUNK_BATCH_START: i32 = 12;
    const PLAY_CHUNK: i32 = 45;
    const PLAY_ADD_ENTITY: i32 = 1;
    const PLAY_CHUNK_BATCH_RECEIVED: i32 = 11;

    let mut handshake = Writer::default();
    handshake.var_i32(776);
    handshake.string("localhost");
    handshake.u16(25565);
    handshake.var_i32(2);
    peer.write_packet(HANDSHAKE, handshake.as_slice()).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
    let mut hello = Writer::default();
    hello.string("HeavyScene");
    hello.uuid(uuid::Uuid::from_u128(0x553));
    peer.write_packet(LOGIN_HELLO, hello.as_slice()).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
    loop {
        let (id, payload) = next_packet(peer).await?;
        if id == LOGIN_COMPRESSION {
            let threshold = Reader::new(&payload).var_i32().map_err(|error| HeavyError::Peer(error.to_string()))?;
            peer.set_compression(threshold);
        } else if id == LOGIN_FINISHED {
            break;
        }
    }
    peer.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
    loop {
        let (id, _payload) = next_packet(peer).await?;
        if id == CONFIG_FINISH {
            break;
        }
    }
    peer.write_packet(CONFIG_FINISH, &[]).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
    // Register the connection with the real mob simulation. The production
    // stream intentionally has no viewer until the client sends both readiness
    // and its first position, so a chunk-only join would otherwise make an
    // entity population look absent even though the tick loop is live.
    peer.write_packet(PLAY_PLAYER_LOADED, &[]).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
    let mut position = Writer::default();
    position.f64(0.0);
    position.f64(65.0);
    position.f64(0.0);
    position.u8(1);
    peer.write_packet(PLAY_MOVE_PLAYER_POS, position.as_slice()).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
    let mut columns = 0;
    let mut batches = 0;
    let mut payload_bytes = 0;
    let mut in_batch = false;
    let mut sent_coordinates = HashSet::new();
    let mut entity_packets = 0;
    let mut entity_positions_in_region = 0;
    while columns < expected_columns || in_batch || entity_packets < expected_entities {
        let (id, payload) = next_packet(peer).await?;
        match id {
            PLAY_CHUNK_BATCH_START => in_batch = true,
            PLAY_CHUNK => {
                columns += 1;
                payload_bytes += payload.len() as u64;
                let mut chunk = Reader::new(&payload);
                let cx = chunk.i32().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let cz = chunk.i32().map_err(|error| HeavyError::Peer(error.to_string()))?;
                sent_coordinates.insert((cx, cz));
            }
            PLAY_ADD_ENTITY => {
                let mut entity = Reader::new(&payload);
                let _id = entity.var_i32().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let _uuid = entity.uuid().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let _type = entity.var_i32().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let x = entity.f64().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let _y = entity.f64().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let z = entity.f64().map_err(|error| HeavyError::Peer(error.to_string()))?;
                entity_packets += 1;
                if let Some((min_x, max_x, min_z, max_z)) = entity_region
                    && (min_x..max_x).contains(&x)
                    && (min_z..max_z).contains(&z)
                {
                    entity_positions_in_region += 1;
                }
            }
            PLAY_CHUNK_BATCH_FINISHED => {
                batches += 1;
                in_batch = false;
                let _reported = Reader::new(&payload).var_i32().map_err(|error| HeavyError::Peer(error.to_string()))?;
                let mut batch_ack = Writer::default();
                batch_ack.f32(20.0);
                peer.write_packet(PLAY_CHUNK_BATCH_RECEIVED, batch_ack.as_slice()).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
                // The integrated loop registers a viewer from the first
                // post-join inbound play packet. Repeat readiness and the
                // position after the batch ack, when the server has completed
                // its join transition, so the next streaming pass can expose
                // the live mob source.
                peer.write_packet(PLAY_PLAYER_LOADED, &[]).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
                let mut position = Writer::default();
                position.f64(0.0);
                position.f64(65.0);
                position.f64(0.0);
                position.u8(1);
                peer.write_packet(PLAY_MOVE_PLAYER_POS, position.as_slice()).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
                peer.write_packet(PLAY_CLIENT_TICK_END, &[]).await.map_err(|error| HeavyError::Peer(error.to_string()))?;
            }
            _ => {}
        }
        if columns >= expected_columns && !in_batch && entity_packets >= expected_entities {
            break;
        }
    }
    Ok((
        columns,
        batches,
        payload_bytes,
        sent_coordinates,
        entity_packets,
        entity_positions_in_region,
    ))
}

async fn next_packet(peer: &mut Connection<DuplexStream>) -> Result<(i32, Vec<u8>), HeavyError> {
    peer.read_packet()
        .await
        .map_err(|error| HeavyError::Peer(error.to_string()))?
        .ok_or_else(|| HeavyError::Peer("peer closed before the join completed".to_string()))
}

impl HeavySceneSpec {
    fn as_str(&self) -> &'static str {
        self.scenario.as_str()
    }
}
