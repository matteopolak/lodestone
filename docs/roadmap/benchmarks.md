# Benchmarks and performance-regression detection

## What it is

The workspace benchmark architecture: the explicit targets measuring production subsystems, the per-crate Criterion harness they share, the two kinds of regression comparison, and the profiling workflow. It records the current target census, not a work plan.

## How it works

### Why it exists

One profiling investigation cut median frame time from 17.05 ms to 8.19 ms (main thread 94% to 56% of a core) by replacing per-section camera-buffer updates (about 4,000 `queue.write_buffer` calls per frame) with shared per-frame state (see [terrain rendering](../terrain-rendering.md)). The suite protects the structural counts that make this class visible; machine-specific durations are never CI thresholds.

### Targets

23 explicit `[[bench]]` targets across eight packages (automatic discovery is disabled, so recorder modules are not benchmarks). The count is a registry, not proof every workload is covered.

| area | targets |
|---|---|
| Chunk generation | `lodestone-worldgen`: `generation` |
| Client chunk data and lighting | `lodestone-world`: `chunk_load`, `heightmap_decode`, `light_propagation`, `light_application` |
| Entities | `lodestone-entity`: `pathfinding_search`, `mob_tick`; `lodestone-shell`: `entity_tick` |
| Physics | `lodestone-physics`: `movement_integration`, `collision_sweep`, `pose_fit_gate`, `crowd_push` |
| Rendering | `lodestone-render`: `meshing`, `render_submit`; `lodestone-shell`: `render_submit`, `frame_profile` |
| Protocol | `lodestone-v26-2`: `chunk_light_decode`, `palette_expansion`, `nbt_decode`, `registry_decode` |
| Memory and server | `lodestone-world`: `memory_footprint`, `session_rss`; `lodestone-server`: `server_tick` |

### Pathfinding classification

`pathfinding_search` measures the production A* through open terrain, detours, a serpentine maze, an unreachable pocket and a real collision-shape corridor. `PathTypeSet` keeps up to four classifications inline and falls back to the heap form for larger footprints, removing the common allocation without changing duplicate handling, malus ordering or the selected type. The benchmark checks reachability and route shape, takes 80 manual wall-time samples, and requires each obstacle scene to stay more than twice the `open_flat` control median and the unreachable scene to stay unreached. One release run measured the inline form at 0.85x to 0.97x of baseline (same-machine, not a threshold). Change `PathTypeSet` and its parity tests together and keep the overflow path.

### Harness

- Criterion is the runner: `harness = false`, dev-dependency with `default-features = false, features = ["cargo_bench_support"]`.
- `lodestone-testsupport::bench_record` (native-only, opt-in) appends one JSON object per metric to gitignored `bench-results/<name>.jsonl`: `{timestamp, git_sha, machine, profile, scene, metric, value, unit}`. Seven families keep tiny `benches/support.rs` shims; worldgen keeps a local wrapper for counter poisoning.
- A benchmark states whether its fixture is realistic or synthetic terrain, since shape is part of the result.
- `samply` with release debug info and CPU-time weighting is for attribution; Criterion and JSONL are for repeatable measurement.

### Catching regressions without flaking CI

- Prefer a ratio against something measured in the same run (old vs new path, N vs 2N, single vs neighbourhood).
- Otherwise compare to a stored baseline from the same machine with a documented band (for example 25%), never a cross-machine absolute.
- Deterministic counts are CI-gated, durations are not. `scripts/bench-gate.py` compares committed count baselines to a fresh run and rejects movement in either direction (a suspicious improvement can mean the benchmark stopped working); see [benchmark regression gate](../benchmark-regression-gate.md).
- Durations stay local or scheduled, with machine, load, profile and scene.
- For every ratio gate, state what real problem it catches (superlinear work in column count, bind-group count growing with resident sections).

`cargo xtask bench-compare <jsonl> --metric <m> --scene <s>` compares two recorded runs without rerunning: by default the latest against the previous on the same machine and profile (refusing if they differ), or pinned with `--candidate <sha> --baseline <sha>`, with `--tolerance <pct>` (default 25). It prints a ratio and verdict and exits non-zero outside the band. It does not label regression versus improvement, since metrics carry no direction. It is not a CI command; `bench-gate` is the CI counterpart.

### Profiling workflow

Verified against `samply` 0.13.1; after upgrading, run `python3 scripts/test-profile-cost-table.py`.

1. Release builds already carry DWARF (`[profile.release] debug = 2`).
2. Install `samply` (macOS needs no permissions; Linux needs `perf_event_paranoid <= 1` or `sudo`).
3. Record a real session, not just startup: `samply record --save-only --unstable-presymbolicate -o profile.json.gz -- ./target/release/lodestone`. On macOS put environment assignments before `samply record`; the system `env` after `--` stops Samply obtaining the task.
4. `python3 scripts/profile-cost-table.py profile.json.gz` prints inclusive and self tables for the main thread (`--thread <substring>` for another), weighted by `samples.threadCPUDelta` (CPU time, not sample count, so `acquire()` stalls are not read as work). It warns and falls back to sample counts when the data is absent; `--require-cpu-time` rejects missing, malformed (short or empty arrays) or zero-total deltas. Inclusive credit counts a recursive `(library, symbol)` once per sample. `--under <symbol-substring>` restricts both tables and the denominator to stacks beneath a boundary and fails on no match.
5. Read the `symbolicated N raw address(es) via sidecar, M unresolved` line; high `M` means the binary changed since recording.

Captures and sidecars are local; do not commit them.

### Evidence standards

- Record machine, load, profile and fixture with every duration.
- A count gate needs a control that changes the count and is rejected; a target that only prints a duration is not a detector.
- State whether a fixture is captured, embedded or synthetic (self-authored inputs suffice for throughput, not protocol compatibility).
- Run the benchmark or gate directly, not through a pipeline that hides its exit status.

## How to change it

- New benchmark: an explicit `[[bench]]` with `harness = false` in its package and a row in the census; keep the `benches/support.rs` shim (worldgen keeps its wrapper). A recorder schema change updates the shared module and its tests.
- New CI-gated metric: first prove it is a deterministic count or fixed-fixture quantity, add its committed baseline and extend the control suite. Never put a wall-clock duration in `bench-baselines/`.

## Configuration

`bench-results/` is gitignored. `just bench-record` produces the gated subset, `just bench-gate` compares with `bench-baselines/`, `just bench-baseline-update` updates values without changing tolerances; overrides and format are in the gate doc.

## Dependencies

Criterion (dev only), `lodestone-testsupport` fixtures, an installed `samply` and `scripts/profile-cost-table.py`.

## Open gaps

- `server_tick` records `IntegratedServer::tick_stats()` samples, but its paused clock gives every phase zero duration, so it cannot rank phase cost; a real-time production-shaped profile is absent.
- Its fixture is an in-memory floor. It gates a zero-cold-column count on the retained 5x5 area, a paused-clock zero-overrun backlog, and a bounded arm over the production overworld source (one setup generation, zero generated columns across 48 paused ticks); keep-alive controls reject filtered-out runs and keep wall-clock advisory. A live scene combining generated-column streaming with keep-alive service is future work.
- The recorder is native-only and feature-gated; worldgen's poisoning filter must fail closed on unlisted units.
- Count baselines are unvalidated across machine classes.
- GPU-dependent shell benchmarks need an adapter-equipped runner before joining the CI-gated set.
