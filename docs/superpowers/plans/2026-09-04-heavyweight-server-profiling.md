# Heavyweight Server Profiling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic release-built integrated-server profiler whose shared Rust scene plan can emit ordered RCON commands and witness requirements for the separate client profiling plan.

**Architecture:** `lodestone-server::heavy_scene` owns the canonical `HeavySceneSpec`, deterministic builders, versioned scene JSON, runtime witness schema, and the native `heavy-scene-server` example. The example either emits a scene plan without starting a server or drives the real integrated server over `DuplexStream`, so generation, batching, scheduled work, lighting, liquid updates, block entities, and entities remain production paths. The client plan consumes the emitted JSON; it does not reimplement scene generation.

**Tech Stack:** Rust 2024, `IntegratedServer`, `ChunkSource`, `Connection<DuplexStream>`, `lodestone-v26-2`, `serde`, `serde_json`, SHA-256, `just`, Samply 0.13.1, and `scripts/profile-cost-table.py`.

---

## Scope and file map

This plan owns only the server/foundation half. It deliberately does not modify the
shell, frame profiler, Python client runner, client fixture, or client documentation;
those belong to `docs/superpowers/plans/2026-09-04-heavyweight-client-profiling.md`.

- Create `crates/lodestone-server/src/heavy_scene.rs` — canonical Rust scene spec, builders, command IR, JSON emission, runtime witness records, readiness/deadline checks, and server harness.
- Modify `crates/lodestone-server/src/lib.rs` — expose `pub mod heavy_scene`.
- Modify `crates/lodestone-server/Cargo.toml` — add direct `serde` and `sha2` workspace dependencies if the existing normal dependency graph does not already expose them directly.
- Create `crates/lodestone-server/examples/heavy-scene-server.rs` — native release executable with `--emit-scene` and finite profiling modes.
- Create `crates/lodestone-server/tests/heavy_scene.rs` — pure deterministic, JSON compatibility, witness, parser, and production-driver controls.
- Modify `Justfile` — server-only smoke, scene-emission, Samply, and cost-table recipes; do not add client recipes here.
- Create `docs/heavyweight-server-profiling.md` — server entrypoints, emitted scene contract, witnesses, operations, and local Samply procedure.
- Generated `docs/README.md` is not hand-edited; a later implementation run may regenerate it after the new doc has a valid H1 and summary.

Shared-checkout rule: inspect `git status --short` before each task, edit only the
listed paths, never create a worktree, and never stage unrelated files. The plan may
refer to the client plan's public Python names, but does not modify its files.

## Client-plan handoff contract

The Rust emitter must produce the exact conceptual contract already used by the client
plan: `HeavySceneSpec { scenario, seed, scale }`, `HeavyScenePlan { spec, commands,
witnesses, scene_hash }`, `WitnessRequirement { segment, column, minimum }`, scenario
names `palette`, `transparency`, `light`, `liquid`, `sign`, `block-entity`, `entity`,
`scheduled`, `mixed`, and command phases `setup`, `after_join`, `mutation`.

The JSON object emitted by `--emit-scene` is versioned and has this shape:

```json
{"schema":1,"spec":{"scenario":"mixed","seed":17,"scale":1},"commands":{"setup":["kill @e[tag=lodestone_heavy_scene]"],"after_join":[],"mutation":["setblock -48 65 8 minecraft:sea_lantern"]},"witnesses":[{"segment":"heavyweight.stationary","column":"world.sign_text_vertices","minimum":6}],"scene_hash":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
```

The actual implementation must contain all generated commands in the arrays; the
short command values above illustrate the wire shape. The hash is the
SHA-256 of compact UTF-8 JSON containing `scenario`, `seed`, `scale`, all three command
arrays in that order, and witness triples in declaration order. The client plan should
load this object through a small JSON reader and retain its existing public Python
names, while removing its duplicate command-generation body in its own implementation.

### Task 1: Add the shared Rust types and canonical serialization

**Files:**
- Create: `crates/lodestone-server/src/heavy_scene.rs`
- Modify: `crates/lodestone-server/src/lib.rs`
- Modify: `crates/lodestone-server/Cargo.toml`
- Test: `crates/lodestone-server/tests/heavy_scene.rs`

- [ ] **Step 1: Write failing tests for the shared contract.**

```rust
use lodestone_server::heavy_scene::{HeavyScenario, HeavySceneSpec};

#[test]
fn canonical_plan_is_stable_and_uses_the_client_contract_names() {
    let spec = HeavySceneSpec::new(HeavyScenario::Mixed, 17, 1).unwrap();
    let first = spec.build_plan().unwrap();
    let second = spec.build_plan().unwrap();
    assert_eq!(first.scene_hash, second.scene_hash);
    assert_eq!(first.commands.setup, second.commands.setup);
    assert_eq!(first.commands.after_join, second.commands.after_join);
    assert_eq!(first.commands.mutation, second.commands.mutation);
    assert_eq!(first.json_value()["schema"], 1);
    assert_eq!(first.json_value()["spec"]["scenario"], "mixed");
    assert!(first.json_value()["commands"]["setup"].is_array());
}

#[test]
fn invalid_scenario_parameters_are_rejected_before_actions_exist() {
    assert!(HeavySceneSpec::new(HeavyScenario::Mixed, 17, 0).is_err());
    assert!(HeavySceneSpec::parse_name("not-a-scenario").is_none());
}
```

Run: `cargo test -p lodestone-server --test heavy_scene canonical_plan_is_stable_and_uses_the_client_contract_names invalid_scenario_parameters_are_rejected_before_actions_exist`

Expected: FAIL because `heavy_scene` and its public contract do not exist.

- [ ] **Step 2: Add the module export and direct dependencies.**

```rust
// crates/lodestone-server/src/lib.rs
pub mod heavy_scene;
```

Add direct dependencies beside the existing JSON dependency so the module does not
depend on transitive visibility:

```toml
serde = { workspace = true }
sha2 = { workspace = true }
```

- [ ] **Step 3: Define the public types with stable names and ordered phases.**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeavyScenario {
    Palette, Transparency, Light, Liquid, Sign, BlockEntity, Entity, Scheduled, Mixed,
}

impl HeavyScenario {
    pub const ALL: [Self; 9] = [
        Self::Palette, Self::Transparency, Self::Light, Self::Liquid, Self::Sign,
        Self::BlockEntity, Self::Entity, Self::Scheduled, Self::Mixed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Palette => "palette", Self::Transparency => "transparency",
            Self::Light => "light", Self::Liquid => "liquid", Self::Sign => "sign",
            Self::BlockEntity => "block-entity", Self::Entity => "entity",
            Self::Scheduled => "scheduled", Self::Mixed => "mixed",
        }
    }

    pub fn parse_name(name: &str) -> Option<Self> {
        Some(match name {
            "palette" => Self::Palette, "transparency" => Self::Transparency,
            "light" => Self::Light, "liquid" => Self::Liquid, "sign" => Self::Sign,
            "block-entity" => Self::BlockEntity, "entity" => Self::Entity,
            "scheduled" => Self::Scheduled, "mixed" => Self::Mixed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneCommandPhase { Setup, AfterJoin, Mutation }

impl SceneCommandPhase {
    pub const ALL: [Self; 3] = [Self::Setup, Self::AfterJoin, Self::Mutation];
    pub const fn as_str(self) -> &'static str {
        match self { Self::Setup => "setup", Self::AfterJoin => "after_join", Self::Mutation => "mutation" }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavySceneSpec { pub scenario: HeavyScenario, pub seed: u64, pub scale: u32 }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WitnessRequirement { pub segment: String, pub column: String, pub minimum: u64 }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderedSceneCommands {
    pub setup: Vec<String>,
    pub after_join: Vec<String>,
    pub mutation: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeavyScenePlan {
    pub schema: u32,
    pub spec: HeavySceneSpec,
    pub commands: OrderedSceneCommands,
    pub witnesses: Vec<WitnessRequirement>,
    pub scene_hash: String,
}

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

impl HeavySceneSpec {
    pub const MAX_COMMAND_BYTES: usize = 32_000;

    pub fn view_radius(&self) -> i32 { if self.scale == 1 { 1 } else { 2 } }

    pub fn expected_join_columns(&self) -> u64 {
        let radius = self.view_radius();
        u64::try_from((radius * 2 + 1).pow(2)).expect("positive view radius")
    }
}
```

`HeavySceneSpec::new` rejects scale zero. `HeavyScenePlan::json_value` must serialize
the fields in the handoff shape, and `canonical_bytes` must use a private ordered
struct rather than an unordered map:

```rust
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
```

- [ ] **Step 4: Run the shared-contract tests.**

Run: `cargo test -p lodestone-server --test heavy_scene canonical_plan_is_stable_and_uses_the_client_contract_names invalid_scenario_parameters_are_rejected_before_actions_exist`

Expected: PASS with both tests executed.

- [ ] **Step 5: Commit the shared contract.**

```bash
git diff --cached --quiet
git add crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/src/lib.rs crates/lodestone-server/Cargo.toml crates/lodestone-server/tests/heavy_scene.rs
git diff --cached --name-only
git commit -m "feat: define heavyweight server scene contract" -- crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/src/lib.rs crates/lodestone-server/Cargo.toml crates/lodestone-server/tests/heavy_scene.rs
git show --stat --oneline HEAD
```

Expected staged names: exactly the four listed paths.

### Task 2: Implement deterministic scenario builders and witness requirements

**Files:**
- Modify: `crates/lodestone-server/src/heavy_scene.rs`
- Test: `crates/lodestone-server/tests/heavy_scene.rs`

- [ ] **Step 1: Add failing builder and command-shape tests.**

```rust
#[test]
fn mixed_plan_contains_every_client_witness_family() {
    let plan = HeavySceneSpec::new(HeavyScenario::Mixed, 17, 1).unwrap().build_plan().unwrap();
    for required in [
        ("world.opaque_sections_drawn", 1), ("world.water_sections_drawn", 1),
        ("world.translucent_sections_drawn", 1), ("world.entities_drawn", 1),
        ("world.block_entities_drawn", 1), ("world.sign_text_vertices", 6),
        ("world.particles_drawn", 1), ("light.relight_cells_changed", 1),
        ("light.remesh_sections_submitted", 1),
    ] {
        assert!(plan.witnesses.iter().any(|w| w.column == required.0 && w.minimum == required.1));
    }
    assert!(!plan.commands.setup.is_empty());
    assert!(!plan.commands.mutation.is_empty());
}

#[test]
fn entity_actions_have_unique_positions_and_commands_are_bounded() {
    let plan = HeavySceneSpec::new(HeavyScenario::Entity, 19, 2).unwrap().build_plan().unwrap();
    let summons: Vec<_> = plan.commands.setup.iter().filter(|line| line.starts_with("summon ")).collect();
    assert_eq!(summons.len(), 2048);
    assert_eq!(summons.iter().collect::<std::collections::BTreeSet<_>>().len(), summons.len());
    assert!(plan.commands.setup.iter().all(|line| line.len() <= HeavySceneSpec::MAX_COMMAND_BYTES));
}
```

Run: `cargo test -p lodestone-server --test heavy_scene mixed_plan_contains_every_client_witness_family entity_actions_have_unique_positions_and_commands_are_bounded`

Expected: FAIL because builders and witness declarations are not implemented.

- [ ] **Step 2: Add deterministic ordered material tables and coordinate reservations.**

Use fixed arrays, a local seeded permutation, and disjoint origins. The palette
builder must vary states inside each section, not merely across sections:

```rust
const PALETTE_STATES: [&str; 12] = [
    "minecraft:stone", "minecraft:granite", "minecraft:diorite", "minecraft:andesite",
    "minecraft:deepslate", "minecraft:tuff", "minecraft:calcite", "minecraft:dripstone_block",
    "minecraft:oak_planks", "minecraft:spruce_planks", "minecraft:birch_planks", "minecraft:bricks",
];

fn subject_origin(index: u32) -> (i32, i32, i32) {
    let x = i32::try_from(index % 4).expect("bounded subject index") * 24 - 48;
    let z = i32::try_from(index / 4).expect("bounded subject index") * 24 - 48;
    (x, 64, z)
}
```

Implement `build_palette`, `build_transparency`, `build_light`, `build_liquid`,
`build_sign`, `build_block_entities`, `build_entities`, and `build_scheduled` as
private deterministic builders returning ordered command vectors and requested
witnesses. `Mixed` concatenates those eight builders in `HeavyScenario::ALL` order,
excluding `Mixed`, with only the documented light/liquid mutation overlap.

- [ ] **Step 3: Implement the concrete command families.**

Use commands accepted by the existing creative server. The builders must include the
following concrete representatives and minimum quantities at scale one:

```rust
fn sign_commands(scale: u32) -> Vec<String> {
    (0..24 * scale).map(|i| format!(
        "setblock {} 65 24 minecraft:oak_wall_sign[facing=north]{{front_text:{{has_glowing_text:1b,color:\"yellow\",messages:[{{text:\"HEAVY-{i:03}\"}},{{text:\"sign\"}},{{text:\"text\"}},{{text:\"witness\"}}]}}}}",
        -48 + i32::try_from(i * 2).expect("bounded sign index")
    )).collect()
}

fn block_entity_commands(scale: u32) -> Vec<String> {
    (0..4 * scale).flat_map(|i| {
        let x = 8 + i32::try_from(i * 2).expect("bounded block entity index");
        [
            format!("setblock {x} 65 24 minecraft:chest[facing=north]"),
            format!("setblock {x} 66 24 minecraft:purple_shulker_box[facing=up]"),
            format!("setblock {x} 67 24 minecraft:white_banner[rotation=0]"),
            format!("setblock {x} 68 24 minecraft:conduit[waterlogged=false]"),
        ]
    }).collect()
}
```

The other builders must place `minecraft:water`, `minecraft:sea_lantern`,
`minecraft:white_stained_glass`, `minecraft:glass_pane`, and repeating command blocks
in the same reserved plot. Entity commands use four supported families, stable tags,
unique coordinates, `NoAI:1b`, `NoGravity:1b`, and `PersistenceRequired:1b`.

- [ ] **Step 4: Declare the exact client-compatible witness requirements.**

Use the client plan's column names and segment labels without renaming:

```rust
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
```

For a named scenario, include only its relevant requirements; for `mixed`, flatten
all selected requirements in declaration order. `validate_ready` must report the
segment, column, observed value, and minimum for every missing or insufficient
witness.

- [ ] **Step 5: Run builder tests and commit.**

Run: `cargo test -p lodestone-server --test heavy_scene mixed_plan_contains_every_client_witness_family entity_actions_have_unique_positions_and_commands_are_bounded`

Expected: PASS with both tests executed.

```bash
git diff --cached --quiet
git add crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/tests/heavy_scene.rs
git diff --cached --name-only
git commit -m "feat: build deterministic heavyweight scenes" -- crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/tests/heavy_scene.rs
git show --stat --oneline HEAD
```

### Task 3: Add `--emit-scene` and versioned JSON output

**Files:**
- Modify: `crates/lodestone-server/src/heavy_scene.rs`
- Create: `crates/lodestone-server/examples/heavy-scene-server.rs`
- Test: `crates/lodestone-server/tests/heavy_scene.rs`

- [ ] **Step 1: Add parser and emission tests before implementation.**

```rust
#[test]
fn emit_arguments_require_exact_scene_inputs() {
    let args = HeavyServerArgs::parse_from([
        "heavy-scene-server", "--emit-scene", "-", "--scenario", "mixed",
        "--seed", "17", "--scale", "1",
    ]).unwrap();
    assert_eq!(args.emit_scene.as_deref(), Some("-"));
    assert_eq!(args.spec.scenario, HeavyScenario::Mixed);
    assert!(HeavyServerArgs::parse_from(["heavy-scene-server", "--emit-scene", "-"]).is_err());
}

#[test]
fn emitted_json_is_client_plan_compatible_and_does_not_start_a_server() {
    let spec = HeavySceneSpec::new(HeavyScenario::Sign, 17, 1).unwrap();
    let json = spec.build_plan().unwrap().json_string();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["spec"]["seed"], 17);
    assert!(value["commands"]["setup"].as_array().unwrap().len() >= 24);
    assert_eq!(value["scene_hash"].as_str().unwrap().len(), 64);
}
```

Run: `cargo test -p lodestone-server --test heavy_scene emit_arguments_require_exact_scene_inputs emitted_json_is_client_plan_compatible_and_does_not_start_a_server`

Expected: FAIL because `HeavyServerArgs` and the emission mode do not exist.

- [ ] **Step 2: Implement the shared manual argument parser.**

Define `HeavyServerArgs` in `heavy_scene.rs` so tests and the example use one parser:

```rust
pub struct HeavyServerArgs {
    pub emit_scene: Option<PathBuf>,
    pub spec: HeavySceneSpec,
    pub phase: ServerPhase,
    pub ticks: u64,
    pub output: PathBuf,
    pub wall_deadline: Duration,
    pub camera_plan: CameraPlan,
    pub smoke: bool,
}
```

Define the parser support types in the same module:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ServerPhase { Ready, Steady, Mutate }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CameraPlan { Stationary, Orbit }

#[derive(Debug, thiserror::Error)]
pub enum HeavyError {
    #[error("invalid heavy-scene argument: {0}")]
    Argument(String),
    #[error("heavy-scene I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("heavy-scene JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("heavy-scene deadline expired after {elapsed:?} in phase {phase} at action {action}")]
    Deadline { elapsed: Duration, phase: String, action: usize },
    #[error("heavy-scene witness failed: {0}")]
    Witness(String),
}
```

`HeavyServerArgs::parse_from` and `parse_env` are the only argument parsers. The
`for_test` constructor uses scenario-specific defaults and never reads process argv.

Accept exactly `--emit-scene <path|->`, `--scenario`, `--seed`, `--scale`,
`--phase ready|steady|mutate`, `--ticks`, `--output`, `--wall-deadline-secs`,
`--camera-plan stationary|orbit`, and `--smoke`. Reject unknown values, missing
required values, scale zero, nonpositive deadline, and missing ticks for scheduled or
liquid mutation. `--emit-scene` must return before constructing an integrated server.

- [ ] **Step 3: Implement the emission path with stdout discipline.**

```rust
fn emit_scene(plan: &HeavyScenePlan, destination: &Path) -> Result<(), HeavyError> {
    let json = plan.json_string();
    if destination == Path::new("-") {
        println!("{json}");
    } else {
        std::fs::write(destination, format!("{json}\n"))?;
    }
    Ok(())
}
```

The example's `main` must parse first, emit exactly one JSON object to stdout when
requested, and send diagnostics only to stderr. The output file contains one complete
JSON object followed by newline; it is not JSONL because the client plan consumes one
immutable scene plan. The runtime mode's separate `--output` remains JSONL.

- [ ] **Step 4: Add the release-example entrypoint and run bounded tests.**

```rust
fn main() -> Result<(), HeavyError> {
    let args = HeavyServerArgs::parse_env()?;
    let plan = args.spec.build_plan()?;
    if let Some(path) = args.emit_scene.as_deref() {
        return emit_scene(&plan, path);
    }
    HeavyServerHarness::run(args, plan)
}
```

Run: `cargo test -p lodestone-server --test heavy_scene emit_arguments_require_exact_scene_inputs emitted_json_is_client_plan_compatible_and_does_not_start_a_server`

Expected: PASS.

Run: `cargo build --release -p lodestone-server --example heavy-scene-server`

Expected: PASS and produce `target/release/examples/heavy-scene-server`.

Run: `target/release/examples/heavy-scene-server --emit-scene /tmp/heavy-scene.json --scenario mixed --seed 17 --scale 1`

Expected: exit 0, write one parseable version-1 JSON object, and print no server-start message to stdout.

- [ ] **Step 5: Commit the emission interface.**

```bash
git diff --cached --quiet
git add crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/examples/heavy-scene-server.rs crates/lodestone-server/tests/heavy_scene.rs
git diff --cached --name-only
git commit -m "feat: emit heavyweight RCON scene plans" -- crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/examples/heavy-scene-server.rs crates/lodestone-server/tests/heavy_scene.rs
git show --stat --oneline HEAD
```

### Task 4: Drive the real integrated server and emit runtime JSONL witnesses

**Files:**
- Modify: `crates/lodestone-server/src/heavy_scene.rs`
- Modify: `crates/lodestone-server/examples/heavy-scene-server.rs`
- Test: `crates/lodestone-server/tests/heavy_scene.rs`

- [ ] **Step 1: Add failing production-path controls.**

```rust
#[tokio::test]
async fn server_harness_receives_the_full_join_view_and_batch_markers() {
    let args = HeavyServerArgs::for_test(HeavyScenario::Palette);
    let plan = args.spec.build_plan().unwrap();
    let record = HeavyServerHarness::run(args, plan).await.unwrap();
    assert_eq!(record.consumed.join_columns, 9);
    assert!(record.consumed.chunk_payload_bytes > 0);
    assert!(record.consumed.chunk_batches > 0);
}

#[tokio::test]
async fn server_harness_rejects_a_missing_producer() {
    let args = HeavyServerArgs::for_test(HeavyScenario::Sign);
    let plan = args.spec.build_plan().unwrap();
    let error = HeavyServerHarness::run_with_witness(args, plan, |witness| {
        witness.consumed.signs = 0;
        witness.consumed.sign_vertices = 0;
    }).await.unwrap_err();
    assert!(error.to_string().contains("world.sign_text_vertices"));
}
```

Run: `cargo test -p lodestone-server --test heavy_scene server_harness_receives_the_full_join_view_and_batch_markers server_harness_rejects_a_missing_producer`

Expected: FAIL because the harness and runtime counters are not connected.

- [ ] **Step 2: Implement the deterministic `HeavyChunkSource`.**

Implement `ChunkSource::column`, `block_state`, `biome_state_at`, and `set_block` in
`HeavyChunkSource`, retaining edits and using atomics for generated-column, mutation,
and notification counts. Populate block entities with `ChunkColumn::set_block_entities`
for the chunk encoder to consume; do not call a protocol encoder directly from scene
construction.

- [ ] **Step 3: Implement the raw v770 peer over `DuplexStream`.**

Use the same production constructor and packet flow as existing server tests:

```rust
let protocol = lodestone_v26_2::server_protocol::V770ServerProtocol::default();
let source = HeavyChunkSource::new(plan.spec.clone());
let (server, client_end) = IntegratedServer::open_in_memory_with_entities(
    protocol, source, NoEntities, plan.spec.view_radius(),
);
let mut peer = Connection::new(client_end);
drive_login_and_join(&mut peer, plan.spec.expected_join_columns()).await?;
```

`drive_login_and_join` sends handshake, login, configuration acknowledgement, drains
all expected `CHUNK` packets, validates `CHUNK_BATCH_START`/`CHUNK_BATCH_FINISHED`,
counts payload bytes, and sends `CHUNK_BATCH_RECEIVED` for reactive batches. It must
not use TCP or a full renderer client.

Expose one harness entrypoint for both the example and integration tests:

```rust
pub struct HeavyServerHarness;

impl HeavyServerHarness {
    pub async fn run(
        args: HeavyServerArgs,
        plan: HeavyScenePlan,
    ) -> Result<HeavyRunRecord, HeavyError> {
        let mut run = Self::start(args, plan).await?;
        run.drive_join().await?;
        run.apply_phase().await?;
        run.finish().await
    }

    pub async fn run_with_witness<F>(
        args: HeavyServerArgs,
        plan: HeavyScenePlan,
        alter: F,
    ) -> Result<HeavyRunRecord, HeavyError>
    where
        F: FnOnce(&mut HeavyRunRecord),
    {
        let mut record = Self::run(args, plan).await?;
        alter(&mut record);
        record.validate_ready()?;
        Ok(record)
    }
}
```

`start`, `drive_join`, `apply_phase`, and `finish` are private methods on the
concrete run state and must carry the same `HeavyError` result through every failure
edge; tests must call only `HeavyServerHarness::run` or its deliberately injected
`run_with_witness` control.

- [ ] **Step 4: Implement finite steady/mutation phases and clean shutdown.**

`ready` installs the plan and validates join witnesses. `steady` drains the settled
server stream without mutation. `mutate` applies `HeavyAction::SetBlock` through the
normal serverbound path, enqueues scheduled work through the existing queue, advances
exactly `--ticks`, and records executed/remaining work. Use
`open_in_memory_with_entities` for encoding-only scenarios and
`open_in_memory_with_mobs_and_commands` only when a real tick loop or command
dispatch is required.

The harness must use a monotonic wall deadline around setup, join, and mutation. On
any timeout, peer error, readiness mismatch, or failed output write, append a failure
record, call `server.shutdown().await`, drop the peer, and return a named error.

- [ ] **Step 5: Define and write the versioned JSONL runtime record.**

```rust
#[derive(Debug, Serialize)]
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
```

Write one compact JSON object plus newline for each `ready`, `complete`, or `failed`
state. Flush each record. Keep captures and records local; do not append to ordinary
Criterion or comparable-history files.

- [ ] **Step 6: Run production controls and commit.**

Run: `cargo test -p lodestone-server --test heavy_scene server_harness_receives_the_full_join_view_and_batch_markers server_harness_rejects_a_missing_producer`

Expected: PASS with nonzero join columns, chunk bytes, and batch count.

Run: `cargo check -p lodestone-server --example heavy-scene-server --all-targets`

Expected: PASS.

```bash
git diff --cached --quiet
git add crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/examples/heavy-scene-server.rs crates/lodestone-server/tests/heavy_scene.rs
git diff --cached --name-only
git commit -m "feat: profile integrated server scene paths" -- crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/examples/heavy-scene-server.rs crates/lodestone-server/tests/heavy_scene.rs
git show --stat --oneline HEAD
```

### Task 5: Add server-only Just recipes and documentation

**Files:**
- Modify: `Justfile`
- Create: `docs/heavyweight-server-profiling.md`

- [ ] **Step 1: Add server scene emission and Samply recipes.**

Add only server recipes; the separate client plan owns client recipes:

```make
heavy-server-emit scenario="mixed" seed="1" scale="1":
    cargo build --release -p lodestone-server --example heavy-scene-server
    target/release/examples/heavy-scene-server --emit-scene /tmp/lodestone-heavy-scene.json --scenario {{scenario}} --seed {{seed}} --scale {{scale}}

samply-heavy-server scenario="mixed" seed="1" scale="1":
    cargo build --release -p lodestone-server --example heavy-scene-server
    samply record --save-only --unstable-presymbolicate -o bench-results/heavy-server.json.gz -- target/release/examples/heavy-scene-server --scenario {{scenario}} --seed {{seed}} --scale {{scale}} --phase mutate --ticks 40 --camera-plan stationary --wall-deadline-secs 180 --output bench-results/heavy-server.jsonl

profile-heavy-server capture:
    python3 scripts/profile-cost-table.py {{capture}}
```

Recipes must remain foreground commands, must not create duration baselines, and must
not write captures or JSONL into tracked paths.

- [ ] **Step 2: Write the server profiling doc.**

Cover what the harness is, the exact production entrypoints (`IntegratedServer`,
`ChunkSource`, join batching, light/liquid/scheduled paths, block-entity encoding),
the `--emit-scene` handoff contract, runtime JSONL fields, scenario and witness
requirements, worker-thread attribution, wall deadlines, failure behavior, and the
Samply 0.13.1 `threadCPUDelta`/sidecar workflow. State explicitly that this plan does
not measure GPU execution and does not define a timing gate. Include the client-plan
handoff command:

```bash
target/release/examples/heavy-scene-server --emit-scene - --scenario mixed --seed 17 --scale 1 > /tmp/heavy-scene.json
```

The doc must not duplicate the client runner's UI or frame-counter instructions.

- [ ] **Step 3: Regenerate the generated docs index without hand-editing it.**

Run: `just regen-docs-index`

Expected: the generator adds `docs/heavyweight-server-profiling.md` to `docs/README.md`; no direct edit to `docs/README.md` is made.

- [ ] **Step 4: Run doc and comment checks, then commit.**

Run: `cargo test -p xtask --test docs_index`

Expected: PASS with the generated index synchronized.

Run: `cargo xtask check-comment-voice`

Expected: PASS; comments use durable technical descriptions and contain no issue or
change-history narration.

```bash
git diff --cached --quiet
git add Justfile docs/heavyweight-server-profiling.md docs/README.md
git diff --cached --name-only
git commit -m "docs: describe heavyweight server profiling" -- Justfile docs/heavyweight-server-profiling.md docs/README.md
git show --stat --oneline HEAD
```

### Task 6: Final focused verification and local capture rehearsal

**Files:**
- Verify: `crates/lodestone-server/src/heavy_scene.rs`
- Verify: `crates/lodestone-server/examples/heavy-scene-server.rs`
- Verify: `crates/lodestone-server/tests/heavy_scene.rs`
- Verify: `Justfile`
- Verify: `docs/heavyweight-server-profiling.md`

- [ ] **Step 1: Run all server tests and structural checks.**

Run: `cargo test -p lodestone-server --test heavy_scene`

Expected: PASS with nonzero test count, including deliberate producer-removal failures.

Run: `cargo check -p lodestone-server --all-targets`

Expected: PASS.

Run: `cargo xtask islands --crate lodestone-server`

Expected: PASS with the release example consuming the public scene plan and the harness reaching the production server path.

Run: `cargo xtask check-comment-voice`

Expected: PASS.

- [ ] **Step 2: Verify the emitter is deterministic and client-readable.**

Run:

```bash
target/release/examples/heavy-scene-server --emit-scene /tmp/heavy-a.json --scenario mixed --seed 17 --scale 1
target/release/examples/heavy-scene-server --emit-scene /tmp/heavy-b.json --scenario mixed --seed 17 --scale 1
cmp /tmp/heavy-a.json /tmp/heavy-b.json
python3 -c 'import json; p=json.load(open("/tmp/heavy-a.json")); assert p["schema"] == 1; assert set(p["commands"]) == {"setup", "after_join", "mutation"}; assert len(p["scene_hash"]) == 64'
```

Expected: `cmp` is silent and the JSON assertion exits 0. Changing `--seed 18` must
change `scene_hash` and at least one ordered command while preserving schema and keys.

- [ ] **Step 3: Run the bounded server readiness path.**

Run: `just samply-heavy-server scenario=palette seed=1 scale=1`

Expected: foreground completion within the wall deadline, valid `.json.gz` capture and
symbol sidecar, nonzero chunk/batch witnesses, and a terminal JSONL record. A missing
output parent, zero scale, or absent scheduled tick count must fail with a named error.

- [ ] **Step 4: Inspect worker-thread cost separately.**

Run:

```bash
python3 scripts/profile-cost-table.py bench-results/heavy-server.json.gz
python3 scripts/profile-cost-table.py --thread tokio-runtime-worker bench-results/heavy-server.json.gz
```

Expected: the first command prints main-thread inclusive/self tables; the second prints
the worker table when that role exists or an explicit no-match result. Never combine
thread-local function indices into one table.

- [ ] **Step 5: Final staged-path review.**

Run:

```bash
git diff --check -- crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/examples/heavy-scene-server.rs crates/lodestone-server/tests/heavy_scene.rs Justfile docs/heavyweight-server-profiling.md docs/README.md
git status --short -- crates/lodestone-server/src/heavy_scene.rs crates/lodestone-server/examples/heavy-scene-server.rs crates/lodestone-server/tests/heavy_scene.rs Justfile docs/heavyweight-server-profiling.md docs/README.md
```

Expected: no whitespace errors; only the explicitly listed implementation/doc paths
are attributable to this plan. Do not commit captures, sidecars, temporary JSON, or
runtime JSONL records.

## Self-review against the approved design and client plan

- **Server ownership is disjoint:** this plan owns only the Rust scene model, native release example, runtime JSONL, server-only recipes, and server doc. It does not add shell flags, UI segments, frame counters, Python runner code, client fixtures, or client docs.
- **Handoff terminology matches:** the emitted JSON uses `HeavySceneSpec`, `HeavyScenePlan`, `WitnessRequirement`, `commands.setup`, `commands.after_join`, `commands.mutation`, the nine client scenario names, `scene_hash`, and the client plan's `heavyweight.*` witness columns.
- **No duplicate scene generator:** the client plan must load `--emit-scene` output; the server plan explicitly defines the release-example interface and canonical hash instead of maintaining a second Python command table.
- **Spec coverage:** palette pressure, opaque/cutout/translucent terrain, static/changing light, liquid and scheduled updates, signs, block entities, varied entities, mixed composition, readiness controls, anti-vacuity controls, wall deadlines, JSONL metadata, Just/Samply wrappers, profile-cost-table use, and worker-thread attribution each have an implementation task and focused verification.
- **Completeness:** every task names exact paths, symbols, commands, expected outcomes, and explicit staged commits. No duration baseline or CI performance gate is introduced.
