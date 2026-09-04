# Heavyweight Client Profiling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic heavyweight live-client workload that exercises normal server-to-client rendering paths, proves every requested subsystem is present with witnesses, and produces local Samply-ready captures without introducing performance thresholds.

**Architecture:** `lodestone-server::heavy_scene` owns `HeavySceneSpec`, `HeavyScenePlan`, deterministic command generation, scene hashing, and witness requirements. The existing Python runner invokes the release `heavy-scene-server --emit-scene -` mode, validates its immutable versioned JSON, and submits its ordered RCON commands unchanged; the shipped `lodestone` binary retains benchmark lifecycle, camera, profiling, and renderer-consumption responsibilities.

**Tech Stack:** Rust 2024, Python standard library, existing RCON runner, release DWARF, Samply, wgpu render statistics, `scripts/profile-cost-table.py`, `just`.

---

## Scope and emitted-scene boundary

The Python runner is a strict consumer of the server plan's JSON object from:

```text
target/release/examples/heavy-scene-server --emit-scene - --scenario mixed --seed 17 --scale 2 --camera-plan orbit
```

The object has `schema: 1`, `spec`, ordered `commands` with exactly `setup`,
`after_join`, and `mutation`, ordered `witnesses` with `segment`, `column`, and
positive `minimum`, plus `scene_hash`. The runner must preserve every command's order,
not calculate a hash, and reject a nonzero emitter exit, invalid JSON, wrong schema,
identity mismatch, changed command-phase order, blank command, or invalid witness.

| File | Change |
|---|---|
| `scripts/client-frame-benchmark.py` | Consume `--emit-scene` JSON, submit ordered RCON phases, enforce readiness/mutation/witness checks, and write client manifests. |
| `scripts/test-client-frame-benchmark.py` | Add hermetic fake-emitter, phase-order, negative-control, manifest, and Samply command tests. |
| `crates/lodestone-shell/src/config.rs`, `src/app/benchmark.rs` | Add heavyweight CLI, mutation lifecycle, and existing intent-path camera control. |
| `crates/lodestone-shell/src/gpu/gpu_timing.rs`, `src/gpu/frame.rs`, `src/app/frame_profile_dump.rs`, `src/app/frame_profile.rs` | Surface real render-consumption witnesses in the frame CSV. |
| `Justfile`, `docs/render-benchmarks.md` | Add foreground local wrappers and durable operating guidance. |

Do not edit `docs/README.md` or create a worktree. The shared index must be empty
before each explicit-path commit.

### Task 1: Consume the server-owned emitted scene

**Files:**
- Modify: `scripts/client-frame-benchmark.py`
- Modify: `scripts/test-client-frame-benchmark.py`

- [ ] **Step 1: Add failing fake-emitter controls.**

```python
payload = {
    "schema": 1,
    "spec": {"scenario": "mixed", "seed": 17, "scale": 2},
    "commands": {"setup": ["setblock 0 64 0 minecraft:stone"], "after_join": [], "mutation": ["setblock 0 64 0 minecraft:air"]},
    "witnesses": [{"segment": "heavyweight.stationary", "column": "world.entities_drawn", "minimum": 1}],
    "scene_hash": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
}
with mock.patch.object(MODULE.subprocess, "run", return_value=CompletedProcess([], 0, json.dumps(payload), "")) as run:
    scene = MODULE._emit_heavy_scene(pathlib.Path("/tmp/heavy-scene-server"), "mixed", 17, 2, "orbit")
self.assertEqual(scene["commands"]["mutation"], ["setblock 0 64 0 minecraft:air"])
self.assertIn("--emit-scene", run.call_args.args[0])
```

Add independent negative controls for a nonzero exit, `schema: 2`, a mismatched
`spec.seed`, command keys out of `setup`/`after_join`/`mutation` order, a blank
command, and `minimum: 0`; each must assert its own named error.

- [ ] **Step 2: Run the control and verify it fails.**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: FAIL because `_emit_heavy_scene` does not exist.

- [ ] **Step 3: Implement the strict emitter consumer.**

```python
HEAVY_SCENE_EMITTER = ROOT / "target" / "release" / "examples" / "heavy-scene-server"
HEAVY_COMMAND_PHASES = ("setup", "after_join", "mutation")

def _emit_heavy_scene(emitter, scenario, seed, scale, camera_plan):
    result = subprocess.run([
        str(emitter), "--emit-scene", "-", "--scenario", scenario, "--seed", str(seed),
        "--scale", str(scale), "--camera-plan", camera_plan,
    ], check=False, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(f"heavy scene emitter failed ({result.returncode}): {result.stderr.strip()}")
    try:
        scene = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"heavy scene emitter returned invalid JSON: {error}") from error
    _validate_emitted_scene(scene, scenario, seed, scale)
    return scene
```

`_validate_emitted_scene` accepts only the exact server object shape above, preserves
the `commands` dictionary insertion order, and requires a nonempty `scene_hash`. It
does not define `HeavySceneSpec`, create commands, construct witnesses, or compute a
hash in Python.

- [ ] **Step 4: Run the fake-emitter controls and commit.**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: PASS; each malformed emission fails through its matching detector.

```bash
git diff --cached --quiet
git add scripts/client-frame-benchmark.py scripts/test-client-frame-benchmark.py
git commit -m "feat: consume emitted heavyweight scenes" -- scripts/client-frame-benchmark.py scripts/test-client-frame-benchmark.py
git show --stat --oneline HEAD
```

### Task 2: Add heavyweight client configuration and a mutation segment

**Files:**
- Modify: `crates/lodestone-shell/src/config.rs:1820-2190`
- Modify: `crates/lodestone-shell/src/app/benchmark.rs:14-190`
- Test: `crates/lodestone-shell/src/config.rs:2262-2340`
- Test: `crates/lodestone-shell/src/app/benchmark.rs:211-315`

- [ ] **Step 1: Write failing configuration and driver tests**

Add these tests before changing parser or driver code:

```rust
#[test]
fn heavyweight_flags_require_a_scenario_and_preserve_the_explicit_values() {
    assert!(matches!(Config::from_args(args(["--benchmark", "heavyweight"])), CliOutcome::Error(msg)
        if msg.contains("--heavy-scenario requires --benchmark heavyweight")));
    let CliOutcome::Run(config) = Config::from_args(args([
        "--benchmark", "heavyweight", "--heavy-scenario", "mixed",
        "--heavy-seed", "17", "--heavy-scale", "2", "--heavy-camera-plan", "orbit", "--benchmark-mutation", "7",
    ])) else { panic!("heavyweight flags must parse") };
    let heavy = config.benchmark.unwrap().heavyweight.unwrap();
    assert_eq!(heavy.scenario, "mixed");
    assert_eq!(heavy.seed, 17);
    assert_eq!(heavy.scale, 2);
    assert_eq!(heavy.camera_plan, "orbit");
}

#[test]
fn heavyweight_runs_a_mutation_segment_before_stationary_measurement() {
    let t0 = Instant::now();
    let mut cfg = fixture_config();
    cfg.workload = BenchmarkWorkload::Heavyweight;
    cfg.mutation = Duration::from_secs(7);
    let mut driver = BenchmarkDriver::new(cfg);
    let _ = driver.update(t0, true);
    assert_eq!(driver.update(t0 + Duration::from_secs(20), true).segment, BenchmarkSegment::Mutation);
    assert_eq!(driver.label(BenchmarkSegment::Mutation), "heavyweight.mutation");
    assert_eq!(driver.update(t0 + Duration::from_secs(27), true).segment, BenchmarkSegment::Stationary);
}
```

- [ ] **Step 2: Run the focused Rust tests and verify they fail**

Run: `cargo test -p lodestone-shell --lib heavyweight_`

Expected: FAIL because `BenchmarkWorkload::Heavyweight`,
`BenchmarkSegment::Mutation`, and `BenchmarkConfig::heavyweight` do not exist.

- [ ] **Step 3: Implement typed CLI state and mutation timing**

In `config.rs`, add this local launch configuration next to `BenchmarkWorkload`. It
stores server-emitter input verbatim; it is not a duplicate scene enum or builder:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeavyweightConfig {
    pub scenario: String,
    pub seed: u64,
    pub scale: u32,
    pub camera_plan: String,
}
```

Add `Heavyweight` to `BenchmarkWorkload` and map it to `"heavyweight"`. Add
`pub heavyweight: Option<HeavyweightConfig>` and `pub mutation: Duration` to
`BenchmarkConfig`. The ordinary workloads construct `heavyweight: None` and
`mutation: Duration::ZERO`; heavyweight requires `Some(HeavyweightConfig)`.

Parse these exact flags in `Config::from_args`:

```text
--heavy-scenario <palette|transparency|light|liquid|sign|block-entity|entity|scheduled|mixed>
--heavy-seed <u64>                 default 1
--heavy-scale <u32>=1              default 1
--heavy-camera-plan <stationary|orbit> default stationary
--benchmark-mutation <seconds>    default 0 except heavyweight runner input
```

Reject a heavy flag without `--benchmark heavyweight`, reject heavyweight without
`--heavy-scenario`, reject scale `0`, reject an unsupported camera plan, and reject
non-heavy workloads with a nonzero mutation duration. The emitter remains the authority
for scenario validity. Add heavyweight to the help string and parser error enumerations.

In `app/benchmark.rs`, insert `Mutation` between `Warmup` and `Stationary`. Define
`mutation_start = warmup`, `stationary_start = mutation_start + mutation`, and keep
all existing timing expressions unchanged when `mutation == Duration::ZERO`. Add every
`heavyweight.<segment>` label. Heavyweight's moving arm follows `Showcase`: orbit only,
with no forward, sprint, or jump input.


- [ ] **Step 4: Run the focused Rust tests and existing benchmark controls**

Run: `cargo test -p lodestone-shell --lib heavyweight_`

Expected: PASS for both new heavyweight controls.

Run: `cargo test -p lodestone-shell --lib benchmark_flags_build_a_live_windowed_run_without_changing_defaults`

Expected: PASS; existing terrain/showcase behavior remains unchanged.

Run: `cargo test -p lodestone-shell --lib showcase_orbits_without_translation`

Expected: PASS. Existing terrain/showcase behavior remains unchanged; heavyweight
labels are `heavyweight.warmup`, `heavyweight.mutation`, `heavyweight.stationary`, and
`heavyweight.moving` in order.

- [ ] **Step 5: Commit configuration and lifecycle changes safely**

Run:

```bash
git diff --cached --quiet
git add crates/lodestone-shell/src/config.rs crates/lodestone-shell/src/app/benchmark.rs
git commit -m "feat: add heavyweight benchmark lifecycle" -- crates/lodestone-shell/src/config.rs crates/lodestone-shell/src/app/benchmark.rs
git show --stat --oneline HEAD
```

Expected: exactly the two Rust files are committed.

### Task 3: Surface production render witnesses in the frame CSV

**Files:**
- Modify: `crates/lodestone-shell/src/gpu/gpu_timing.rs:626-710`
- Modify: `crates/lodestone-shell/src/gpu/frame.rs:1039-1048,2180-2270`
- Modify: `crates/lodestone-shell/src/app/frame_profile_dump.rs:65-175`
- Modify: `crates/lodestone-shell/src/app/frame_profile.rs:1046-1090`

- [ ] **Step 1: Write the failing CSV bridge control**

Extend `dump_pairs_world_and_hud_timings_with_per_frame_workload_counts` with the
following pairwise-distinct values and assertions:

```rust
crate::gpu::gpu_timing::record_world_subphase_counts(WorldSubphaseCounts {
    packed_sections_visited: 137,
    model_sections_visited: 911,
    opaque_sections_drawn: 23,
    water_sections_drawn: 29,
    translucent_sections_drawn: 31,
    entities_drawn: 37,
    block_entities_drawn: 41,
    sign_text_vertices: 43,
    particles_drawn: 47,
});

for (name, expected) in [
    ("world.opaque_sections_drawn", "23"),
    ("world.water_sections_drawn", "29"),
    ("world.translucent_sections_drawn", "31"),
    ("world.entities_drawn", "37"),
    ("world.block_entities_drawn", "41"),
    ("world.sign_text_vertices", "43"),
    ("world.particles_drawn", "47"),
] {
    let column = header.iter().position(|column| *column == name).unwrap();
    assert_eq!(row[column], expected, "wrong {name} in {csv}");
}
```

Add the same fields with different values to
`gpu_timing::world_subphase_tests::record_and_take_round_trip_the_real_elapsed_time_not_a_placeholder`.

- [ ] **Step 2: Run the bridge controls and verify compilation fails**

Run: `cargo test -p lodestone-shell --lib dump_pairs_world_and_hud_timings_with_per_frame_workload_counts`

Expected: FAIL with missing `WorldSubphaseCounts` fields and missing CSV headers.

Run: `cargo test -p lodestone-shell --lib record_and_take_round_trip_the_real_elapsed_time_not_a_placeholder`

Expected: FAIL with missing `WorldSubphaseCounts` fields and missing CSV headers.

- [ ] **Step 3: Extend the one existing frame-to-profiler bridge**

Add these fields to `WorldSubphaseCounts` in `gpu_timing.rs`:

```rust
pub opaque_sections_drawn: usize,
pub water_sections_drawn: usize,
pub translucent_sections_drawn: usize,
pub entities_drawn: usize,
pub block_entities_drawn: usize,
pub sign_text_vertices: u32,
pub particles_drawn: usize,
```

Do not create a second thread-local or a renderer-only report path. Move the existing
`record_world_subphase_counts` call from the opaque-terrain checkpoint in
`RenderState::render_inner` to immediately after `stats.vram_reserved_bytes` is set,
where all entity, sign, block-entity, water, translucent, and particle passes have
updated the one real `RenderStats`. Keep the terrain timing checkpoint at its current
location. The final count recording is:

```rust
crate::gpu::gpu_timing::record_world_subphase_counts(
    crate::gpu::gpu_timing::WorldSubphaseCounts {
        packed_sections_visited: self.sections.len(),
        model_sections_visited: self.model.as_ref().map_or(0, |model| model.sections.len()),
        opaque_sections_drawn: stats.sections_drawn,
        water_sections_drawn: stats.water_sections_drawn,
        translucent_sections_drawn: stats.translucent_sections_drawn,
        entities_drawn: stats.entities_drawn,
        block_entities_drawn: stats.block_entities_drawn,
        sign_text_vertices: stats.sign_text_vertices,
        particles_drawn: stats.particles_drawn,
    },
);
```

Append the seven `world.*` headers in `DumpWriter::write_header` and append those same
values in `DumpWriter::write_row`, preserving the header/write order exactly. These are
real zeroes on a rendered frame, unlike skipped timing spans, so write them as numeric
fields whenever `world_counts` is present.

- [ ] **Step 4: Run the bridge controls and all library profiler tests**

Run: `cargo test -p lodestone-shell --lib frame_profile`

Expected: PASS for the CSV bridge controls.

Run: `cargo test -p lodestone-shell --lib world_subphase_tests`

Expected: PASS. The CSV row contains all seven exact values; a deliberate removal of
the final `record_world_subphase_counts` call makes the bridge test fail because its
world-count columns are empty or stale.

- [ ] **Step 5: Commit the witness bridge safely**

Run:

```bash
git diff --cached --quiet
git add crates/lodestone-shell/src/gpu/gpu_timing.rs crates/lodestone-shell/src/gpu/frame.rs crates/lodestone-shell/src/app/frame_profile_dump.rs crates/lodestone-shell/src/app/frame_profile.rs
git commit -m "feat: record heavyweight render witnesses" -- crates/lodestone-shell/src/gpu/gpu_timing.rs crates/lodestone-shell/src/gpu/frame.rs crates/lodestone-shell/src/app/frame_profile_dump.rs crates/lodestone-shell/src/app/frame_profile.rs
git show --stat --oneline HEAD
```

Expected: exactly the four bridge files are committed.

### Task 4: Integrate generated commands, segment actions, and JSONL witnesses

**Files:**
- Modify: `scripts/client-frame-benchmark.py:24-75,288-365,386-735`
- Modify: `scripts/test-client-frame-benchmark.py:1-220`

- [ ] **Step 1: Write failing runner tests**

Add these cases to `scripts/test-client-frame-benchmark.py`:

```python
def test_heavy_client_command_forwards_all_typed_scene_flags(self):
    scene = fixture_emitted_scene(scenario="mixed", seed=17, scale=2)
    command = MODULE._client_command(
        pathlib.Path("/tmp/lodestone"), "heavyweight", 25570, (2, 7, 2, 3), "closed",
        camera_plan="orbit", heavy_scene=scene,
    )
    self.assertEqual(command[command.index("--heavy-scenario") + 1], "mixed")
    self.assertEqual(command[command.index("--heavy-seed") + 1], "17")
    self.assertEqual(command[command.index("--heavy-scale") + 1], "2")
    self.assertEqual(command[command.index("--heavy-camera-plan") + 1], "orbit")
    self.assertEqual(command[command.index("--benchmark-mutation") + 1], "7")

def test_heavy_metadata_keeps_hash_and_witnesses_out_of_comparable_history(self):
    scene = fixture_emitted_scene(scenario="sign", seed=17, scale=1, scene_hash="abc123")
    record = MODULE._heavy_record(
        "heavyweight", 1, pathlib.Path("/tmp/lodestone"), (2, 7, 2, 3), "closed",
        1, scene, {"heavyweight.stationary": {}},
    )
    self.assertEqual(record["scene_hash"], "abc123")
    self.assertEqual(record["seed"], 17)
    self.assertNotIn("p50_ms", record)

def test_missing_required_heavy_witness_invalidates_the_trial(self):
    scene = fixture_emitted_scene(scenario="sign", seed=17, scale=1)
    with self.assertRaisesRegex(RuntimeError, "world.sign_text_vertices.*maximum 0"):
        MODULE._validate_heavy_witnesses(
            scene, {"heavyweight.stationary": {"world.sign_text_vertices": {"max": 0.0}}}
        )
```

Add a transition-dispatch control using a fake log that contains each exact structured
marker once. It must assert that `setup` commands run before the client starts,
`after_join` runs after `heavyweight.warmup`, and `mutation` runs only after
`heavyweight.mutation`; omitting the mutation marker must raise
`RuntimeError("heavyweight mutation segment was never observed")`.

- [ ] **Step 2: Run the runner controls and verify they fail**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: FAIL because `_heavy_record`, witness validation, heavy command forwarding,
and transition dispatch do not exist.

- [ ] **Step 3: Make heavyweight a first-class runner workload**

Add `"heavyweight"` to `ORACLES` with the existing creative oracle configuration,
and replace the showcase-specific scene function with a shared executor:

```python
def run_rcon_commands(rcon_port: int, phase: str, commands: tuple[str, ...]) -> None:
    with RconClient(rcon_port) as rcon:
        for index, command in enumerate(commands, 1):
            reply = rcon.command(command)
            if _is_scene_error(command, reply):
                raise RuntimeError(
                    f"{phase} command {index}/{len(commands)} failed: {command}\n"
                    f"{reply or '(empty response)'}"
                )
```

Keep `prepare_showcase` as a thin caller of `run_rcon_commands`; do not weaken its
existing filtering of player-targeted commands. Add `prepare_heavy_scene` that calls
`_emit_heavy_scene` and passes only `scene["commands"]["setup"]` before the client
process begins.

Extend `parse_args` with a fixed choice tuple that mirrors the nine scenario names in
the versioned emitter contract:

```python
parser.add_argument("--heavy-scenario")
parser.add_argument("--heavy-seed", type=int, default=1)
parser.add_argument("--heavy-scale", type=int, default=2)
parser.add_argument("--heavy-camera-plan", choices=("stationary", "orbit"), default="orbit")
parser.add_argument("--heavy-mutation-seconds", type=int, default=7)
```

Require `--heavy-scenario` exactly when `--workload heavyweight`; reject a negative
seed, scale below one, or mutation seconds below zero with `parser.error`. Set smoke
durations to `(2, 1, 2, 3)` for heavyweight and full durations to `(20, 7, 30, 60)`.
The tuple order is always `(warmup, mutation, stationary, moving)`. For a smoke run,
invoke `_emit_heavy_scene` with effective `scale=1` regardless of the requested scale;
for a full run use the requested scale, whose runner default is `2` (2,048 tagged
entities). Store both `requested_scale` and the emitted `spec.scale` in the local
record so the artifact cannot misrepresent density.

Extend `_client_command` to receive the four durations, `camera_plan`, and an optional
validated emitted-scene dictionary. For heavyweight append the five exact flags from
`scene["spec"]`:

```python
"--heavy-scenario", scene["spec"]["scenario"],
"--heavy-seed", str(scene["spec"]["seed"]),
"--heavy-scale", str(scene["spec"]["scale"]),
"--heavy-camera-plan", camera_plan,
"--benchmark-mutation", str(mutation),
```

In `run_trial`, track `joined_configured`, `after_join_applied`, and
`mutation_applied`. Read the appended client log while the child lives. Match the
existing transition field text `segment=heavyweight.warmup` or
`segment="heavyweight.warmup"`; call `configure_joined_player`, then
`run_rcon_commands(rcon_port, "after_join", scene["commands"]["after_join"])`. On the analogous
`heavyweight.mutation` marker call the mutation batch once. Do not dispatch mutation
from a wall-clock deadline or a CSV row: the client transition marker is the common
clock edge. Require the mutation marker whenever `scene["commands"]["mutation"]` is
nonempty.

After `validate_run`, call `summarize_rows` for mutation, stationary, and moving, and
pass all three maps to `_validate_heavy_witnesses(scene, segments)`. The validator
checks only the already-validated emitter requirements; it does not generate or alter
them. Use segment keys exactly
`"heavyweight.mutation"`, `"heavyweight.stationary"`, and `"heavyweight.moving"`.
Persist all three maps for diagnosis, but treat only stationary and moving timings as
profile interpretation; the mutation map is solely a proof that the changed light and
liquid work reached the existing relight/remesh counters.

Add the seven new `world.*` names to `COUNT_COLUMNS`. Write heavyweight records to
`bench-results/heavyweight_scene.jsonl`, not `live_frame_profile.jsonl`, with this
exact construction:

```python
def _heavy_record(workload, trial, binary, durations, debug_overlay, requested_scale, scene, segments):
    spec = scene["spec"]
    return {
        "schema": 1,
        "workload": workload,
        "trial": trial,
        "profile": "release",
        "git_sha": _git_sha(),
        "machine": platform.platform(),
        "arch": platform.machine(),
        "binary": str(binary),
        "scenario": spec["scenario"],
        "seed": spec["seed"],
        "requested_scale": requested_scale,
        "scale": spec["scale"],
        "scene_hash": scene["scene_hash"],
        "debug_overlay": debug_overlay,
        "durations_seconds": dict(zip(("warmup", "mutation", "stationary", "moving"), durations)),
        "segments": segments,
    }
```

Append one sorted, compact JSON line only after witnesses pass. For `--samply`, preserve
the existing one-trial/one-overlay rule, write the profile under
`bench-results/profiles/`, and write the same heavyweight record as a JSON sidecar in
that directory with a `capture` field naming the emitted profile. Print both paths.
Do not call `_append_records` for heavyweight or for any Samply run; the sidecar is a
local per-capture record, not comparable-history data.

- [ ] **Step 4: Run the Python runner tests**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: PASS. The test with a zero sign witness raises the named failure; legacy
workloads still parse and build the same client command without heavyweight flags.

- [ ] **Step 5: Commit runner integration safely**

Run:

```bash
git diff --cached --quiet
git add scripts/client-frame-benchmark.py scripts/test-client-frame-benchmark.py
git commit -m "feat: run heavyweight client profiling scenes" -- scripts/client-frame-benchmark.py scripts/test-client-frame-benchmark.py
git show --stat --oneline HEAD
```

Expected: exactly the two runner files are committed. There is no Python scene
generator; the Rust release example remains the single authority.

### Task 5: Add structural coverage of emitted full-density scene data

**Files:**
- Create: `crates/lodestone-shell/tests/gpu/frame_benchmark_heavyweight_fixture.rs`
- Modify: `crates/lodestone-shell/tests/gpu.rs`
- Create: `scripts/fixtures/heavyweight-mixed-scale-1.json`

- [ ] **Step 1: Write the failing structural test registration**

Add the module declaration to `crates/lodestone-shell/tests/gpu.rs`:

```rust
#[path = "gpu/frame_benchmark_heavyweight_fixture.rs"]
mod frame_benchmark_heavyweight_fixture;
```

Create the test file with this initial control:

```rust
#[test]
fn heavyweight_snapshot_has_every_required_live_category() {
    let scene = include_str!("../../../../scripts/fixtures/heavyweight-mixed-scale-1.json");
    for required in [
        "minecraft:water", "minecraft:sea_lantern", "minecraft:white_stained_glass",
        "_sign[", "minecraft:chest", "summon armor_stand", "summon sheep",
        "repeating_command_block",
    ] {
        assert!(scene.contains(required), "heavyweight snapshot misses {required}");
    }
    assert!(scene.matches("summon ").count() >= 1024);
}
```

- [ ] **Step 2: Run the test and verify the snapshot is absent**

Run: `cargo test -p lodestone-shell --test gpu heavyweight_snapshot_has_every_required_live_category`

Expected: FAIL because `scripts/fixtures/heavyweight-mixed-scale-1.json` does not exist.

- [ ] **Step 3: Generate and commit the deterministic snapshot**

Build the authoritative Rust emitter and generate the exact checked-in static control
artifact with:

```bash
cargo build --release -p lodestone-server --example heavy-scene-server
target/release/examples/heavy-scene-server --emit-scene scripts/fixtures/heavyweight-mixed-scale-1.json --scenario mixed --seed 17 --scale 1 --camera-plan orbit
```

Extend the Rust control with exact lower bounds for the independent scene paths:

```rust
assert!(scene.matches("_sign[").count() >= 24);
assert!(scene.matches("minecraft:sea_lantern").count() >= 64);
assert!(scene.matches("minecraft:water").count() >= 64);
assert!(scene.matches("minecraft:white_stained_glass").count() >= 32);
assert!(scene.matches("minecraft:chest").count() >= 4);
assert!(scene.matches("repeating_command_block").count() >= 4);
```

The snapshot is the complete versioned JSON object, not a second schema or generator.
It is a structural control only. It does not replace RCON acceptance or render witness
validation, and it must never be hand-edited; regeneration through the Rust emitter
with the same arguments must produce byte-identical content and `scene_hash`.

- [ ] **Step 4: Run the structural test and emitter reproducibility control**

Run: `cargo test -p lodestone-shell --test gpu heavyweight_snapshot_has_every_required_live_category`

Expected: PASS. Deleting one category token makes the named assertion fail, proving the
test is not merely parsing a valid text file.

Emit the same scene to `/tmp/heavyweight-mixed-scale-1.json` and compare it byte for
byte with the committed fixture. Expected: identical output.

- [ ] **Step 5: Commit structural data and control safely**

Run:

```bash
git diff --cached --quiet
git add scripts/fixtures/heavyweight-mixed-scale-1.json crates/lodestone-shell/tests/gpu/frame_benchmark_heavyweight_fixture.rs crates/lodestone-shell/tests/gpu.rs
git commit -m "test: cover heavyweight scene categories" -- scripts/fixtures/heavyweight-mixed-scale-1.json crates/lodestone-shell/tests/gpu/frame_benchmark_heavyweight_fixture.rs crates/lodestone-shell/tests/gpu.rs
git show --stat --oneline HEAD
```

Expected: exactly the emitter-generated fixture, test, and test-binary registration
are committed. The server-owned emitter is unchanged.

### Task 6: Add local profiling recipes and operator documentation

**Files:**
- Modify: `Justfile:402-427`
- Modify: `docs/render-benchmarks.md:60-170`

- [ ] **Step 1: Write the documentation assertions as text checks**

Extend `scripts/test-client-frame-benchmark.py` with a source-level control that reads
`Justfile` and `docs/render-benchmarks.md` and requires these exact user-facing strings:

```python
root = pathlib.Path(__file__).resolve().parents[1]
self.assertIn("bench-client-heavy:", (root / "Justfile").read_text(encoding="utf-8"))
self.assertIn("profile-client-heavy:", (root / "Justfile").read_text(encoding="utf-8"))
doc = (root / "docs" / "render-benchmarks.md").read_text(encoding="utf-8")
self.assertIn("heavyweight", doc)
self.assertIn("threadCPUDelta", doc)
self.assertIn("profile-cost-table.py", doc)
```

- [ ] **Step 2: Run the source-level control and verify it fails**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: FAIL because the heavyweight recipes and documentation text are absent.

- [ ] **Step 3: Add exact wrappers and documentation**

Add these recipes after the existing client benchmark recipes:

```make
# One low-density end-to-end run. It still requires every selected witness.
bench-client-heavy-smoke:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario mixed --smoke

# Full local workload; records local JSONL witnesses but no cross-machine duration baseline.
bench-client-heavy:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario mixed

# One release-client Samply capture. The runner prints the capture and sidecar paths.
profile-client-heavy:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario mixed --samply

# Convert a printed local capture into CPU-delta-weighted self and inclusive tables.
profile-cost-table capture:
    python3 scripts/profile-cost-table.py {{capture}}
```

In `docs/render-benchmarks.md`, add a subsection under the live client benchmark that
states:

1. `heavyweight` is profiler-first local evidence, not comparable-history or CI timing
   data;
2. `--heavy-scenario`, `--heavy-seed`, `--heavy-scale`, and
   `--heavy-mutation-seconds` select deterministic command data;
3. smoke lowers density/duration but never bypasses witnesses;
4. `just profile-client-heavy` requires a release `lodestone` binary and writes the
   capture/sidecar locally;
5. open the emitted capture in the local Samply UI, then run
   `just profile-cost-table <capture>` for `threadCPUDelta`-weighted attribution;
6. inspect the main thread and each named non-idle worker separately; and
7. macOS runs require confirmed fullscreen on the hardware-built-in display and a
   native GPU adapter.

Document the seven CSV columns and explain that a missing witness invalidates the run;
do not document a frame-time target.

- [ ] **Step 4: Run documentation and runner controls**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: PASS. The test proves the operator commands and CPU-delta attribution text
are discoverable from the repository, without claiming the commands ran.

- [ ] **Step 5: Commit recipes and docs safely**

Run:

```bash
git diff --cached --quiet
git add Justfile docs/render-benchmarks.md scripts/test-client-frame-benchmark.py
git commit -m "docs: describe heavyweight client profiling" -- Justfile docs/render-benchmarks.md scripts/test-client-frame-benchmark.py
git show --stat --oneline HEAD
```

Expected: exactly the recipe, documentation, and source-level control changes are
committed. Do not edit `docs/README.md`.

### Task 7: Verify connectedness and perform one local live rehearsal

**Files:**
- Verify: all files listed in Tasks 1-6.

- [ ] **Step 1: Run all focused static controls**

Run: `python3 scripts/test-client-frame-benchmark.py`

Expected: PASS.

Run: `cargo test -p lodestone-shell --lib frame_profile`

Expected: PASS.

Run: `cargo test -p lodestone-shell --lib world_subphase_tests`

Expected: PASS.

Run: `cargo test -p lodestone-shell --test gpu heavyweight_snapshot_has_every_required_live_category`

Expected: PASS.

- [ ] **Step 2: Run architectural and compilation checks**

Run: `cargo check -p lodestone-shell --all-targets`

Expected: PASS.

Run: `cargo xtask islands --crate lodestone-shell`

Expected: PASS with no new heavyweight-only production island. The runner must consume
the release emitter output, the client must consume the workload, and the renderer must emit the
witness fields through the actual frame dump.

Run: `just check-comment-voice`

Expected: PASS.

- [ ] **Step 3: Run the low-density end-to-end acceptance control**

Run: `just bench-client-heavy-smoke`

Expected: one heavyweight run reaches warmup, mutation, stationary, moving, and
complete; RCON accepts setup and mutation batches; output includes a scene hash and
nonzero witnesses required by `mixed`. It must fail if an RCON command is rejected, a
mutation marker is absent, the frame CSV is missing, or one required witness maximum
is zero.

- [ ] **Step 4: Record and inspect one local capture on macOS**

Run: `cargo build --release -p lodestone-shell --bin lodestone`

Expected: PASS; the release binary contains DWARF from the committed release profile.

Run: `just profile-client-heavy`

Expected: one fullscreen heavyweight trial and a printed
capture and sidecar path under `bench-results/profiles/`. The run fails rather than
skipping when Samply, the local server, the built-in display, or a native GPU adapter
is unavailable.

Run `just profile-cost-table` with the exact capture path printed by the preceding
command.

Expected: self and inclusive tables explicitly labeled `threadCPUDelta`; run the same
command with `--thread` and the exact non-idle worker name recorded in that sidecar.
A fallback-weight warning is retained in the local record rather than treated as a
timing result. Do not create a final empty commit; any discovered defect starts a new
bounded, test-first correction task using explicit shared-checkout paths.

## Completion criteria

- The shipped release client accepts and labels the heavyweight workload, including a
  dedicated mutation segment that is excluded from stationary/moving timing summaries.
- The runner consumes deterministic commands and witness requirements emitted from the
  server-owned `HeavySceneSpec`, obtains ordinary RCON acceptance, and records the
  emitted stable scenario hash without duplicating generation in Python.
- Each focused scenario and `mixed` have a declared witness at a real render or update
  consumption boundary; a deliberately missing producer makes the relevant run fail.
- The frame CSV carries the seven render witnesses as numeric per-rendered-frame
  counts, and the runner summarizes them as counts rather than milliseconds.
- Heavyweight JSONL remains local, per-run evidence and is not appended to the existing
  comparable-history file or made into a duration baseline.
- Smoke, full, and Samply/cost-table commands are documented and tested for discovery.
- Focused tests, shell all-target check, islands, and comment-voice checks pass before
  the implementation is handed off.
