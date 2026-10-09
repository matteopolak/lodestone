# Render benchmarks

## What it is

Three instruments for "where does frame time go, and did a change help": a live CPU/GPU frame profiler in every build, a criterion harness across many crates, and a full-client live benchmark that joins a real server and measures the windowed game.

## How it works

### The live frame profiler

`FrameProfiler` (`crates/lodestone-shell/src/app/frame_profile.rs`) times each named CPU phase of `WindowApp::redraw` (setup, sim tick, mesh upload, acquire, prepare, world encode, HUD/UI, encoder finish, queue submit, present) in ring buffers and reports mean/p95/p99 plus a skip count. World and HUD are split into sub-phases, since "3 ms across 60 draws" and "across 6000" differ and `queue_submit` can include GPU backpressure waits.

- World and ordinary HUD share one primary encoder; `encoder_finish` and `queue_submit` measure the combined boundary. The dump includes `primary.encoders_created`, `encoders_finished` and `queue_submissions` (one of each on a visible-HUD frame). Compare total sequential CPU phases across layouts, not a renamed or smaller bucket.
- GPU timestamps use per-pass `TIMESTAMP_QUERY`. The `world` interval spans world passes and `first_person` the hand pass; neither is the whole GPU frame, so never subtract them from CPU time or add overlapping intervals.
- Each frame reserves one of three query/readback slots; a busy ring drops the measurement, not the render. Missing hand work is `not_run`, malformed edges `invalid`, never a previous frame's value. GPU log records are individual asynchronous samples, not the CPU window's cohort.
- GPU timing is on while F3 is visible or with `LODESTONE_GPU_PROFILE=1` (native); ordinary play has no query passes. `RenderState::set_gpu_timing_enabled` is the consumer switch.
- Block-sprite animation uniforms update only when the integer game tick changes.
- F3 shows the numbers as text, Shift+F3 as a flat pie chart (the phases are siblings, not a call tree), `RUST_LOG=frame_profile=info` logs once a second, and `LODESTONE_FRAME_PROFILE_DUMP=<path>` writes a CSV row per frame. A skipped phase writes an empty cell, never `0`.
- The window driver redraws only when the pacer's deadline is due, so a dump dominated by rows with no acquire/present phase means inspect window state, not high FPS. Failed acquisitions defer the next by 8 ms; `FramePacer` owns the deadline for native and browser.

`just bench-frame` (`crates/lodestone-shell/benches/frame_profile.rs`) is the reproducible counterpart: a fixed camera path over a demo world at four waypoints (level, yawed, looking down, looking up). It asserts counts and relations within a run (residency does not move under pure camera rotation; a residency sweep grows with radius) and records medians to local JSONL as an advisory comparison. It cannot exercise the live-vanilla model path or a real HUD.

### The live client frame benchmark

`scripts/client-frame-benchmark.py` drives the production native client joined to a real Java 26.2 server through `warmup`, `stationary` (30 s), `moving` (360 degree orbit, 60 s) and `complete`, with a fresh data directory and offline username per trial.

| workload | scene |
|---|---|
| `terrain` | normal generated terrain |
| `showcase` | dense authored plot: signs, heads, banners, frames, stands, mobs, displays, particles |
| `megaworld`, `lovelier` | untracked local-cache save worlds installed separately |

- Default window policy is built-in fullscreen, uncapped, no VSync. On macOS it maps winit monitors to CoreGraphics display ids and requires the hardware-built-in panel plus confirmed fullscreen. `--benchmark-window windowed` requires an observed 2560x1440 framebuffer; `--benchmark-resolution WxH` (widths 320-8192, heights 240-8192) declares another windowed size and rejects a different observed one.
- Output reuses the profiler's CSV/JSONL shape, summarised per segment as frame-interval percentiles, budget misses (16.67/33.3 ms), phase means and process-tree RSS, appended to `bench-results/live_frame_profile.jsonl`. The frame interval covers everything that delayed the next redraw and cannot be split into CPU and GPU parts by subtraction.
- A run is written only if `world.model_sections_visited` is positive in both stationary and moving segments, a witness that a disconnected counter bridge cannot yield a valid-looking no-op run. `just test-client-frame-benchmark` runs the no-GPU controls.
- Selected-segment capture uses the frame-start clock; an interrupted or unreached phase is an error. `--artifact-dir PATH` retains `frames.csv`, `client.log`, `resources.json`, `presentation.json`, `options.json` and `trial.json` (artifact hashes, status, scene hash, checkout SHA, binary digest, machine identity) per trial, including failures. Unknown adapter, simulation distance, resolved present mode and build provenance stay `null`; `configured_present_mode` is the API request, not proof of delivery.
- `--settings PATH` takes exactly eleven production `Options` fields and rejects incomplete, unknown, duplicate, mistyped or out-of-range input before launch:

```json
{"framerate_limit": 60, "enable_vsync": false, "inactivity_fps_limit": "minimized",
 "fov": 70, "render_distance": 24, "graphics_preset": "custom", "cloud_status": "off",
 "cutout_leaves": true, "biome_blend_radius": 2, "entity_shadows": true, "particles": "all"}
```

Ranges: `framerate_limit` 10-260 (260 = unlimited), `fov` 30-110, `render_distance` 2-256, `biome_blend_radius` 0-7. Windowed policy and `--benchmark-pacing options` require a declaration. Prefer `inactivity_fps_limit=minimized` for stationary comparisons. Changing the preset alone does not establish visual parity.

```sh
python3 scripts/client-frame-benchmark.py --workload terrain --trials 1 \
  --settings comparison-settings.json --benchmark-window windowed \
  --benchmark-pacing options --warmup-seconds 20 --stationary-seconds 3 \
  --moving-seconds 3 --artifact-dir bench-results/comparison
```

- Every trial captures `<workload>.stationary` presentation via `LODESTONE_PRESENTATION_CAPTURE_SEGMENT`, requiring schema 2, zero `droppedRows`, consistent counts and successful-row cap/VSync context. Percentiles use successful post-present handoff intervals. `skipReasons` counts pacing, missing resources and each acquisition error. The capture stores at most 32,768 attempts, so shorten `--stationary-seconds` on fast uncapped systems (about 10 s leaves room for 3,000 attempts/s); a truncated prefix is not accepted. Handoffs do not prove compositor cadence.
- `LODESTONE_BENCHMARK_SCREENSHOT=1` writes a PNG at the stationary-to-moving transition.
- The [resource sampler](client-resource-sampling.md) records the launched PID tree each second: whole-launch scope, RSS sums that may double-count shared pages, CPU where 100% is one core, `null` for missing values. It reports no GPU residency or VRAM.
- `--world-snapshot-manifest PATH` (`{"schema":1,"files":[...]}`) hashes declared files under the stopped oracle's `world/` directory (at most 512 unique regular files, 1 GiB, 1 MiB manifest; symlinks, traversal and changed files fail). It is a prelaunch checkpoint, not reset/replay.
- `scripts/prepare-vanilla-comparison-assets.py` stages cached same-release resources under `.cache/benchmarks/` for an official-texture comparison (refuses existing destinations, verifies release id and CRCs, writes `vanilla-comparison-assets.json`). Point `LODESTONE_ASSETS` at the result. The runner requests 26.2/protocol 776, so inputs must be 26.2. Benchmark-only: never ship staged assets. Tests: `python3 scripts/test-prepare-vanilla-comparison-assets.py`.

### Integrated singleplayer capture

`--benchmark singleplayer` creates a survival world with seed `4242` through the normal menu ownership and save handling, then runs warmup, stationary, sprint/jump walking, and downward mining (default 3 s, `--benchmark-mutation`). The join clock starts before world creation; choreography starts after ready terrain reaches the Surface. A join still waiting after 120 s fails and shuts down. Walking precedes mining so digging cannot trap exploration. Verify displacement before treating the walking phase as evidence; an attack marker is not an edit acknowledgement.

```bash
LODESTONE_FRAME_PROFILE_DUMP=/private/tmp/lodestone-surface.csv \
RUST_LOG=warn,frame_benchmark=info,frame_profile=debug \
  lodestone --benchmark singleplayer --render-distance 8 \
  --benchmark-warmup 2 --benchmark-stationary 5 --benchmark-moving 20
```

Per-second `frame_benchmark` samples record position, resident/presented/expected columns, mesh backlog, RSS, tick phase maxima and terrain-mesh GPU buffer bytes (not total VRAM). Change choreography in `app::benchmark::BenchmarkDriver`, `app::singleplayer_benchmark`, `WindowApp::draw_menu` and `WindowApp::redraw`.

### Heavyweight profiling scenes

`heavyweight` is profiler-first local evidence, not CI history. The runner asks the release `heavy-scene-server` example for a versioned, hashed command plan and never rebuilds commands or the scene hash in Python. Setup runs as a temporary datapack function, reloaded once and called once through RCON, then removed. `--heavy-scenario`, `--heavy-seed`, `--heavy-scale` (1-2), `--heavy-mutation-seconds` (0-10) choose deterministic input; at most 120 s of choreography, exactly one trial.

- `mixed` is the broad scene; `dense-mixed` is a fixed scale-one envelope (2,048 entities, 1,536 signs, 1,024 each of light and liquid cells, 768 transparent cells, 512 palette cells, block entities and scheduled producers) whose 7,937-command setup runs in one function call under a 90 s deadline. `just profile-client-heavy-dense` is its Samply input.
- The CSV carries production witnesses `world.opaque_sections_drawn`, `water_sections_drawn`, `translucent_sections_drawn`, `entities_drawn`, `block_entities_drawn`, `sign_text_vertices` and `particles_drawn`. A required witness below its emitted minimum invalidates the run. The mutation segment checks relight/remesh reachability; stationary and moving are the timing arms.
- `just profile-client-heavy` needs a prebuilt release binary and writes a Samply capture plus JSON scene record under `bench-results/`; `just profile-cost-table <capture>` gives `threadCPUDelta`-weighted attribution. On macOS it requires fullscreen on the built-in display and a native GPU adapter.
- `just validate-client-heavy-profile <capture>` checks the capture, `*.json.syms.json` and `*.record.json` (release profile, scenario, scale, scene hash, phase durations, nonzero frame counts, ordered percentiles, camera plan `stationary` or `orbit`) without decoding the profile.

### The criterion harness

Each covered crate (including `lodestone-render`/`lodestone-shell`) has benches doing two things: a one-shot `Instant` measurement recorded through a `support` module (a JSONL line with timestamp, git sha, machine, profile, scene, metric, value into gitignored `bench-results/<name>.jsonl`, with an advisory plus-or-minus 25% ratio against the last matching run), and a criterion `bench_function` for statistical sampling and `--save-baseline`/`--baseline`. Neither replaces the other. `support.rs` is a deliberate per-crate copy kept identical by a documented diff check; promote it to a crate once a fifth site needs it.

**Prefer a count to a duration.** Identical binaries swing 20%+ in wall clock. Assert draw-list sizes, arena and atlas occupancy, bind-group-switch counts (`RenderStats::terrain_camera_bind_group_switches`, counted by pointer identity) and instance counts exactly; record only unavoidable timings as advisory. Two traps are guarded: world-shaped fixtures that degenerate to near-zero cost (each bench asserts against this before timing), and state persisting across a naive iteration closure, which needs `iter_batched` with fresh setup.

## How to change it

- **Add a CPU phase or GPU segment:** add a variant to `FramePhase`/`WorldSubphase`/`HudSubphase` and its `ALL`/`name` list (not tied at compile time), then call `mark`/`record_world_subphase`. Append, never insert, a GPU segment name, because order fixes query-set indices.
- **Add a bench:** a `.rs` file under the crate's `benches/`, a `Cargo.toml` entry, `mod support;`. For a new crate, copy `support.rs` and update its header.
- **Regression tolerance:** the `0.75..=1.25` literal in every `support.rs` copy, changed together.
- **Extend declared settings:** update `SETTINGS_FIELDS`, its validators, `_observed_trial_metadata` and the client's bring-up metadata together; the setting needs a production consumer. Change `PRESENTATION_COLUMNS` only with the production capture schema.
- Always run `--release` and say which profile produced a number. Run on an idle machine; a duration under load is a sample.

## Configuration

- `LODESTONE_FRAME_PROFILE_DUMP`, `RUST_LOG=frame_profile=info`, `LODESTONE_GPU_PROFILE=1`, F3 / Shift+F3.
- Client flags: `--benchmark terrain|showcase|megaworld|lovelier|singleplayer|heavyweight`, `--benchmark-debug-overlay closed|open`, `--benchmark-{warmup,stationary,moving} SECONDS`, `--heavy-camera-plan stationary|orbit`, `--validate-heavy-profile CAPTURE`.
- Runner flags: `--trials N`, `--smoke`, `--samply`, `--debug-overlay closed|open|both`, `--binary PATH`, `--settings`, `--benchmark-window builtin-fullscreen|windowed`, `--benchmark-pacing uncapped|options`, `--{warmup,stationary,moving}-seconds N` (these override defaults including under `--smoke`).
- `LODESTONE_PRESENTATION_CAPTURE=<path>` and `LODESTONE_PRESENTATION_CAPTURE_SEGMENT=<workload>.stationary`.
- `bench-results/*.jsonl` is gitignored local history; a fresh clone has no baseline. Criterion flags after `--` (`--quick`, `--sample-size`, `--save-baseline`) work on every covered bench.

## Dependencies

- `wgpu` `TIMESTAMP_QUERY` (native only; elsewhere GPU timing is unavailable, never zero).
- `crate::platform::Instant` (`lodestone-time`) for CPU clocks; `std::time::Instant::now()` traps on wasm32.
- `criterion` (`cargo_bench_support` only) and `serde_json` in each covered crate.
- The live benchmark needs a release binary, local oracle servers under `.cache/mc/`, and on macOS `objc2-core-graphics` for built-in-display selection.
