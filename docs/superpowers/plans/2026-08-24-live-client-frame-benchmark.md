# Live Client Frame Benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and run repeatable Java-backed terrain and showcase client workloads that identify the dominant real-world frame-time bottleneck without changing renderer behavior.

**Architecture:** Add an opt-in benchmark configuration and a small pure state machine that drives the production `WindowApp` with deterministic input, frame labels, fixed presentation settings, and clean exit. Extend the existing per-frame CSV with frame intervals and benchmark labels, then use a standalone Python runner to prepare the Java scene, launch and monitor the release client, summarize three trials, and retain compact same-machine results.

**Tech Stack:** Rust, winit, Lodestone's existing `FrameProfiler`/wgpu timestamp instrumentation, Python standard library, RCON, the existing Java 26.2 oracle scripts, and `samply`.

---

## File structure

- `crates/lodestone-shell/src/config.rs`: benchmark workload and duration CLI configuration.
- `crates/lodestone-shell/src/app/benchmark.rs`: pure benchmark segment/input state machine.
- `crates/lodestone-shell/src/app.rs`: module wiring and `WindowApp` ownership only.
- `crates/lodestone-shell/src/app/session.rs`: construct the opt-in driver.
- `crates/lodestone-shell/src/app/redraw.rs`: apply benchmark intent before `Sim::step`, label frames, force benchmark pacing/presentation, and request clean exit.
- `crates/lodestone-shell/src/app/lifecycle.rs`: fixed physical benchmark window size and focus-independent execution.
- `crates/lodestone-shell/src/app/frame_profile.rs`: carry frame interval and current benchmark segment into the dump.
- `crates/lodestone-shell/src/app/frame_profile_dump.rs`: serialize the two new CSV columns.
- `scripts/benchmark-scenes/showcase.txt`: deterministic RCON fixture for specialized render paths.
- `scripts/client-frame-benchmark.py`: oracle/scene runner, process monitor, CSV summarizer, and JSONL recorder.
- `scripts/test-client-frame-benchmark.py`: Python fixture tests for parsing and statistics.
- `docs/live-client-frame-benchmark.md`: operator guide, data contract, and interpretation limits.
- `docs/README.md`: generated documentation index.

### Task 1: Parse an explicit benchmark configuration

**Files:**
- Modify: `crates/lodestone-shell/src/config.rs`

- [ ] **Step 1: Write failing parser tests**

Add tests beside the existing `Config::from_args` tests:

```rust
#[test]
fn benchmark_flags_build_a_live_windowed_run_without_changing_defaults() {
    let normal = parse(&[]);
    assert_eq!(normal.benchmark, None);

    let terrain = parse(&[
        "--benchmark", "terrain",
        "--benchmark-warmup", "20",
        "--benchmark-stationary", "30",
        "--benchmark-moving", "60",
    ]);
    assert_eq!(terrain.mode, Mode::Window);
    assert_eq!(terrain.benchmark, Some(BenchmarkConfig {
        workload: BenchmarkWorkload::Terrain,
        warmup: Duration::from_secs(20),
        stationary: Duration::from_secs(30),
        moving: Duration::from_secs(60),
    }));
}

#[test]
fn benchmark_rejects_unknown_workloads_and_missing_durations() {
    assert!(matches!(
        Config::from_args(["--benchmark".into(), "castle".into()]),
        CliOutcome::Error(message) if message.contains("terrain or showcase")
    ));
    assert!(matches!(
        Config::from_args(["--benchmark-warmup".into()]),
        CliOutcome::Error(message) if message.contains("requires a value")
    ));
}
```

- [ ] **Step 2: Run the tests and observe the expected failure**

Run:

```bash
cargo test -p lodestone-shell --lib config::tests::benchmark -- --nocapture
```

Expected: compilation fails because `BenchmarkConfig`, `BenchmarkWorkload`, and `Config::benchmark` do not exist.

- [ ] **Step 3: Add the minimal configuration types and parser arms**

Add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkWorkload {
    Terrain,
    Showcase,
}

impl BenchmarkWorkload {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Terrain => "terrain",
            Self::Showcase => "showcase",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchmarkConfig {
    pub workload: BenchmarkWorkload,
    pub warmup: Duration,
    pub stationary: Duration,
    pub moving: Duration,
}

impl BenchmarkConfig {
    pub const PHYSICAL_SIZE: (u32, u32) = (2560, 1440);
}
```

Add `pub benchmark: Option<BenchmarkConfig>` to `Config`, default it to `None`, parse `--benchmark terrain|showcase` plus the three duration overrides, and emit an error for missing/invalid values instead of silently retaining a default. Defaults are 20/30/60 seconds. Add every flag to `Config::usage`.

- [ ] **Step 4: Run the focused parser tests**

Run the command from Step 2. Expected: all benchmark parser tests pass.

- [ ] **Step 5: Commit the configuration task**

Commit only `crates/lodestone-shell/src/config.rs` with:

```bash
git commit -m "feat(shell): parse live frame benchmark sessions" -- crates/lodestone-shell/src/config.rs
```

### Task 2: Put actual frame intervals and segment labels in the CSV

**Files:**
- Modify: `crates/lodestone-shell/src/app/frame_profile.rs`
- Modify: `crates/lodestone-shell/src/app/frame_profile_dump.rs`

- [ ] **Step 1: Write failing dump tests**

Add tests that create a temporary dump, label two frames, and assert exact header/row fields:

```rust
#[test]
fn dump_carries_frame_interval_and_benchmark_segment() {
    let path = unique_dump_path("segment-and-interval");
    let t0 = Instant::now();
    let mut profiler = FrameProfiler::new(t0, Some(&path));
    profiler.set_segment(Some("terrain.stationary"));
    profiler.begin_frame(t0);
    profiler.mark(FramePhase::Setup, t0 + Duration::from_millis(2));
    profiler.begin_frame(t0 + Duration::from_millis(17));
    profiler.set_segment(Some("terrain.moving"));
    profiler.mark(FramePhase::Setup, t0 + Duration::from_millis(19));
    profiler.begin_frame(t0 + Duration::from_millis(34));
    drop(profiler);

    let csv = std::fs::read_to_string(&path).unwrap();
    let mut lines = csv.lines();
    assert!(lines.next().unwrap().starts_with("frame,frame_interval_ms,segment,"));
    assert!(lines.next().unwrap().starts_with("1,17.0000,terrain.stationary,"));
    assert!(lines.next().unwrap().starts_with("2,17.0000,terrain.moving,"));
    let _ = std::fs::remove_file(path);
}
```

- [ ] **Step 2: Run and observe RED**

Run:

```bash
cargo test -p lodestone-shell --lib app::frame_profile::tests::dump_carries_frame_interval -- --nocapture
```

Expected: compilation fails because `set_segment` and the CSV columns do not exist.

- [ ] **Step 3: Implement interval and label capture**

Add `last_frame_start: Option<Instant>`, `pending_interval_ms: Option<f32>`, `segment: Option<&'static str>`, and `pending_segment: Option<&'static str>` to `FrameProfiler`. In `begin_frame`, finalize the prior frame first, calculate `now - last_frame_start`, snapshot the current segment for the new pending row, and update `last_frame_start`. Add:

```rust
pub(crate) fn set_segment(&mut self, segment: Option<&'static str>) {
    self.segment = segment;
}
```

Change `DumpWriter::write_header` to begin with `frame,frame_interval_ms,segment`. Change `write_row` to accept `Option<f32>` and `Option<&str>`, writing an empty interval only for the first unpaired frame and an empty label for ordinary play. Keep all skipped phase values empty.

- [ ] **Step 4: Run frame-profiler tests**

Run:

```bash
cargo test -p lodestone-shell --lib app::frame_profile -- --nocapture
```

Expected: all frame profiler tests pass, including the existing skipped-field control.

- [ ] **Step 5: Commit the dump schema task**

Commit the two explicit files with:

```bash
git commit -m "feat(shell): label raw frame profile intervals" -- crates/lodestone-shell/src/app/frame_profile.rs crates/lodestone-shell/src/app/frame_profile_dump.rs
```

### Task 3: Build the deterministic benchmark driver

**Files:**
- Create: `crates/lodestone-shell/src/app/benchmark.rs`
- Modify: `crates/lodestone-shell/src/app.rs`

- [ ] **Step 1: Write state-machine tests before wiring production**

The new module defines:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BenchmarkSegment {
    WaitingForJoin,
    Warmup,
    Stationary,
    Moving,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BenchmarkIntent {
    pub segment: BenchmarkSegment,
    pub forward: bool,
    pub sprint: bool,
    pub jump: bool,
    pub mouse_dx: f32,
    pub complete: bool,
}
```

Write tests using explicit `Instant` offsets:

```rust
#[test]
fn time_does_not_start_before_the_live_join() {
    let t0 = Instant::now();
    let mut driver = BenchmarkDriver::new(fixture_config());
    assert_eq!(driver.update(t0, false).segment, BenchmarkSegment::WaitingForJoin);
    assert_eq!(driver.update(t0 + Duration::from_secs(99), false).segment, BenchmarkSegment::WaitingForJoin);
    assert_eq!(driver.update(t0 + Duration::from_secs(100), true).segment, BenchmarkSegment::Warmup);
}

#[test]
fn terrain_runs_warmup_stationary_moving_then_completes() {
    let t0 = Instant::now();
    let mut driver = BenchmarkDriver::new(fixture_config());
    driver.update(t0, true);
    assert_eq!(driver.update(t0 + Duration::from_secs(20), true).segment, BenchmarkSegment::Stationary);
    let moving = driver.update(t0 + Duration::from_secs(50), true);
    assert_eq!(moving.segment, BenchmarkSegment::Moving);
    assert!(moving.forward && moving.sprint);
    assert!(driver.update(t0 + Duration::from_secs(110), true).complete);
}

#[test]
fn showcase_orbits_without_translation() {
    let t0 = Instant::now();
    let mut cfg = fixture_config();
    cfg.workload = BenchmarkWorkload::Showcase;
    let mut driver = BenchmarkDriver::new(cfg);
    driver.update(t0, true);
    let moving = driver.update(t0 + Duration::from_secs(50), true);
    assert!(!moving.forward && !moving.sprint && !moving.jump);
    assert!(moving.mouse_dx > 0.0);
}
```

- [ ] **Step 2: Run and observe RED**

Run:

```bash
cargo test -p lodestone-shell --lib app::benchmark -- --nocapture
```

Expected: compilation fails because the benchmark module is not implemented.

- [ ] **Step 3: Implement the pure state machine**

Use elapsed wall time from the first `connected == true` update. Warm-up includes a four-edge creative-flight sequence during its first 300 ms (`jump` on/off/on/off) for the terrain workload; the remainder is stationary. Terrain `Moving` holds forward and sprint. Showcase `Moving` produces a constant mouse delta sized to one 360-degree orbit over its configured moving duration. `label()` returns stable strings: `terrain.warmup`, `terrain.stationary`, `terrain.moving`, and their `showcase.*` counterparts.

No code in this module accesses a window, GPU, network socket, or filesystem.

- [ ] **Step 4: Run focused tests**

Run the command from Step 2. Expected: all state-machine tests pass.

- [ ] **Step 5: Commit the driver task**

Commit the new module and module-root edit with:

```bash
git add crates/lodestone-shell/src/app/benchmark.rs
git commit -m "feat(shell): add deterministic frame benchmark driver" -- crates/lodestone-shell/src/app/benchmark.rs crates/lodestone-shell/src/app.rs
```

### Task 4: Wire the driver into the real windowed frame loop

**Files:**
- Modify: `crates/lodestone-shell/src/app.rs`
- Modify: `crates/lodestone-shell/src/app/session.rs`
- Modify: `crates/lodestone-shell/src/app/redraw.rs`
- Modify: `crates/lodestone-shell/src/app/lifecycle.rs`
- Test: `crates/lodestone-shell/src/app/tests.rs`

- [ ] **Step 1: Write failing integration-level unit tests**

Add pure seam tests for the benchmark-only policy helpers:

```rust
#[test]
fn benchmark_policy_is_uncapped_unvsynced_and_uses_physical_1440p() {
    let config = benchmark_config(BenchmarkWorkload::Terrain);
    assert_eq!(window_physical_size(&config), Some((2560, 1440)));
    assert_eq!(target_fps(&config, 120, InactivityFpsLimit::Afk), None);
    assert_eq!(present_mode(&config, wgpu::PresentMode::Fifo), wgpu::PresentMode::AutoNoVsync);
}

#[test]
fn ordinary_policy_remains_persisted_option_driven() {
    let config = Config::default();
    assert_eq!(window_physical_size(&config), None);
    assert_eq!(present_mode(&config, wgpu::PresentMode::Fifo), wgpu::PresentMode::Fifo);
}
```

- [ ] **Step 2: Run and observe RED**

Run:

```bash
cargo test -p lodestone-shell --lib app::tests::benchmark_policy -- --nocapture
```

Expected: compilation fails because the helpers and `WindowApp::benchmark` do not exist.

- [ ] **Step 3: Add the minimal production wiring**

Add `benchmark: Option<BenchmarkDriver>` to `WindowApp`, construct it from `config.benchmark`, and in `redraw` update it immediately before `sim.step`. Apply intent through the existing `InputState` API:

```rust
self.sim.input_mut(|input| {
    input.set(Action::Forward, intent.forward);
    input.set(Action::Sprint, intent.sprint);
    input.set(Action::Jump, intent.jump);
    if intent.mouse_dx != 0.0 {
        input.add_mouse(intent.mouse_dx, 0.0);
    }
});
self.frame_profile.set_segment(Some(intent.segment.label()));
if intent.complete {
    self.ui.request_quit();
}
```

The update uses `self.sim.session_phase() == SessionPhase::Connected`; disconnected time remains `WaitingForJoin` and is excluded by its label. Benchmark mode makes `current_target_fps` return `None`, chooses `AutoNoVsync`, requests `PhysicalSize::new(2560, 1440)`, and ignores focus-loss pause/throttling while leaving cursor behavior alone. Ordinary play continues through the existing option-driven branches.

Log every segment transition on target `frame_benchmark`, including workload, label, elapsed seconds, player position, loaded columns, and current RSS. Log a final `benchmark complete` marker before requesting quit.

- [ ] **Step 4: Run focused app tests**

Run:

```bash
cargo test -p lodestone-shell --lib app::tests::benchmark_policy app::benchmark -- --nocapture
```

Expected: benchmark policies and driver tests pass.

- [ ] **Step 5: Run the version-free compile seam**

Run:

```bash
cargo check -p lodestone-shell --no-default-features
```

Expected: exit 0; the driver depends on no protocol family.

- [ ] **Step 6: Commit the frame-loop wiring**

Commit only the listed files with:

```bash
git commit -m "feat(shell): drive benchmark sessions through the live frame loop" -- crates/lodestone-shell/src/app.rs crates/lodestone-shell/src/app/session.rs crates/lodestone-shell/src/app/redraw.rs crates/lodestone-shell/src/app/lifecycle.rs crates/lodestone-shell/src/app/tests.rs
```

### Task 5: Add the dense Java showcase fixture

**Files:**
- Create: `scripts/benchmark-scenes/showcase.txt`
- Create: `crates/lodestone-shell/tests/frame_benchmark_showcase_fixture.rs`

- [ ] **Step 1: Write the fixture-control test**

The test loads the fixture with `include_str!` and requires each category by command token, with minimum repetition counts:

```rust
#[test]
fn showcase_exercises_every_requested_render_path() {
    let scene = include_str!("../../../scripts/benchmark-scenes/showcase.txt");
    for required in [
        "_sign[", "player_head", "_banner[", "item_frame", "map_id",
        "summon armor_stand", "summon sheep", "summon text_display",
        "summon item_display", "summon block_display", "particle ",
    ] {
        assert!(scene.contains(required), "showcase misses {required}");
    }
    assert!(scene.matches("item_frame").count() >= 16);
    assert!(scene.matches("_sign[").count() >= 24);
    assert!(scene.matches("_banner[").count() >= 16);
}
```

- [ ] **Step 2: Run and observe RED**

Run:

```bash
cargo test -p lodestone-shell --test frame_benchmark_showcase_fixture -- --nocapture
```

Expected: compilation fails because the scene file does not exist.

- [ ] **Step 3: Create the RCON scene**

Create a self-contained command file that first kills prior benchmark-tagged entities and clears a 64 x 32 x 64 plot. Build separated rows of at least 24 signs, 16 player heads, 16 patterned banners, 16 item frames containing four distinct filled maps, 12 armour stands with equipment, 24 passive mobs, text/item/block displays, chests/shulkers/campfires/beacons, glass/water/translucent blocks, and repeating particle commands. Tag summoned entities `lodestone_benchmark` so the next run can remove them exactly.

Use ordinary RCON commands only. Comments begin with `#`; no benchmark directive is interpreted by the Java server.

- [ ] **Step 4: Run the fixture test GREEN**

Run the command from Step 2. Expected: one passing integration test.

- [ ] **Step 5: Commit the fixture**

Commit the explicit files with:

```bash
git add scripts/benchmark-scenes/showcase.txt crates/lodestone-shell/tests/frame_benchmark_showcase_fixture.rs
git commit -m "test(shell): add dense live render showcase" -- scripts/benchmark-scenes/showcase.txt crates/lodestone-shell/tests/frame_benchmark_showcase_fixture.rs
```

### Task 6: Add the foreground runner and statistical summarizer

**Files:**
- Create: `scripts/client-frame-benchmark.py`
- Create: `scripts/test-client-frame-benchmark.py`
- Modify: `justfile`

- [ ] **Step 1: Write failing Python tests**

Cover nearest-rank percentiles, missed-frame counts, phase means over non-empty fields, metadata mismatch, and incomplete runs:

```python
class SummaryTests(unittest.TestCase):
    def test_percentiles_and_budget_misses(self):
        summary = summarize_rows([
            {"frame_interval_ms": "8.0", "segment": "terrain.stationary", "setup": "1.0"},
            {"frame_interval_ms": "17.0", "segment": "terrain.stationary", "setup": ""},
            {"frame_interval_ms": "34.0", "segment": "terrain.stationary", "setup": "3.0"},
        ])
        self.assertEqual(summary["frames"], 3)
        self.assertEqual(summary["p50_ms"], 17.0)
        self.assertEqual(summary["p95_ms"], 34.0)
        self.assertEqual(summary["over_16_67"], 2)
        self.assertEqual(summary["over_33_3"], 1)
        self.assertEqual(summary["phases_ms"]["setup"], 2.0)

    def test_missing_complete_marker_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "completion marker"):
            validate_run([], "log without marker")
```

- [ ] **Step 2: Run and observe RED**

Run:

```bash
python3 scripts/test-client-frame-benchmark.py
```

Expected: import/file failure because the runner module does not exist.

- [ ] **Step 3: Implement the standard-library runner**

The script accepts `--workload terrain|showcase`, `--trials N` (default 3), `--smoke`, `--samply`, and `--binary`. It:

1. starts the matching oracle in the foreground and checks its readiness;
2. for showcase, sends every non-comment scene line over RCON and rejects error replies;
3. launches the release binary as a child with `LODESTONE_FRAME_PROFILE_DUMP` pointing at a unique temporary CSV and `RUST_LOG=frame_profile=info,frame_benchmark=info,warn`;
4. waits in the foreground, samples child RSS every 250 ms, and enforces a deadline only to classify the run as failed;
5. requires exit code zero, a completion marker, non-empty stationary/moving segments, and the requested physical window size in the log;
6. prints per-segment median/p95/p99, budget misses, phase/subphase means, RSS start/peak/end, and trial spread;
7. appends one compact JSON object per segment to `bench-results/live_frame_profile.jsonl` with git SHA, machine, profile, scene, trial, and configuration.

Use `subprocess.Popen` plus `poll()` so RSS sampling does not background the benchmark from the agent's point of view: the runner is the one foreground process and does not return until its child exits. Open stdout/stderr log files directly rather than piping unread output.

`--smoke` overrides durations to 2/2/3 seconds and runs one trial. `--samply` runs one trial under `samply record -o <artifact> -- <binary> ...` and prints the artifact path without treating its duration as a comparable benchmark trial.

- [ ] **Step 4: Add canonical just recipes**

Add separate non-branching recipes:

```make
bench-client-terrain:
    python3 scripts/client-frame-benchmark.py --workload terrain

bench-client-showcase:
    python3 scripts/client-frame-benchmark.py --workload showcase

bench-client-smoke:
    python3 scripts/client-frame-benchmark.py --workload showcase --smoke
```

- [ ] **Step 5: Run Python tests GREEN**

Run the command from Step 2. Expected: all tests pass.

- [ ] **Step 6: Commit the runner**

Commit the explicit files with:

```bash
git add scripts/client-frame-benchmark.py scripts/test-client-frame-benchmark.py
git commit -m "feat(perf): automate live client frame trials" -- scripts/client-frame-benchmark.py scripts/test-client-frame-benchmark.py justfile
```

### Task 7: Document the benchmark before running expensive trials

**Files:**
- Create: `docs/live-client-frame-benchmark.md`
- Modify mechanically: `docs/README.md`

- [ ] **Step 1: Write the operator document**

Cover the required repository doc sections: what it is, how the Java-backed path works, exact smoke/full/samply commands, how to change workloads and durations, configuration, dependencies, CSV/JSONL schemas, foreground-run rule, GPU timestamp limitations, and the rule that synthetic `frame_profile` numbers are controls rather than substitutes for the live workloads.

- [ ] **Step 2: Regenerate and verify the docs index**

Run:

```bash
cargo xtask docs-index
cargo test -p xtask docs_index_matches_committed
```

Expected: the generated index contains `Live client frame benchmark` and the xtask test passes.

- [ ] **Step 3: Commit the documentation**

Commit only the two doc paths with:

```bash
git add docs/live-client-frame-benchmark.md
git commit -m "docs: explain the live client frame benchmark" -- docs/live-client-frame-benchmark.md docs/README.md
```

### Task 8: Smoke-test, gather the baseline, and identify one root cause

**Files:**
- Append generated compact records: `bench-results/live_frame_profile.jsonl`
- Create after measurement: `docs/client-frame-performance-2026-08-24.md`

- [ ] **Step 1: Build the release client once**

Run:

```bash
cargo build --release -p lodestone-shell --bin lodestone
```

Expected: exit 0 and `target/release/lodestone` exists.

- [ ] **Step 2: Run the short end-to-end smoke gate**

Run:

```bash
just bench-client-smoke
```

Expected: Java oracle readiness, successful join, warmup/stationary/moving markers, non-empty CSV segment summaries, zero exit status, and a completion marker.

- [ ] **Step 3: Run the synthetic control once on the same idle machine**

Run:

```bash
just bench-frame
```

Expected: every waypoint reports CPU and available GPU figures plus noise estimates. Treat this only as an instrumentation/control reading.

- [ ] **Step 4: Run three terrain trials in the foreground**

Run:

```bash
just bench-client-terrain
```

Expected: three complete stationary and moving summaries with trial spread.

- [ ] **Step 5: Run three showcase trials in the foreground**

Run:

```bash
just bench-client-showcase
```

Expected: three complete stationary and orbit summaries with trial spread.

- [ ] **Step 6: Record a CPU sample of the worst segment**

Run the corresponding runner with `--samply`, for example:

```bash
python3 scripts/client-frame-benchmark.py --workload showcase --samply
```

Expected: a symbolicated profile artifact path and clean benchmark completion.

- [ ] **Step 7: Write the baseline evidence report**

Create `docs/client-frame-performance-2026-08-24.md` containing the machine/build/configuration, raw artifact paths, trial tables, CPU-vs-GPU verdict, hottest `samply` stacks with inclusive sample shares, allocation/RSS evidence, one falsifiable root-cause hypothesis, rejected alternatives, and the exact metric selected as the optimization acceptance test.

- [ ] **Step 8: Write a second evidence-specific implementation plan**

Use the measured root cause to create `docs/superpowers/plans/2026-08-24-client-frame-optimization.md`. Name the exact production/test files, write the failing regression control, make one focused change, run the same three-trial protocol, and update the report with before/after ratios. Do not select culling, batching, caching, wgpu features, allocation work, or data-layout work until the profile identifies it.

- [ ] **Step 9: Commit baseline evidence**

Regenerate `docs/README.md`, then commit the compact JSONL and evidence report explicitly. Raw CSV and samply files remain local artifacts and are not committed.

