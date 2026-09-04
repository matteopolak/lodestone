# Benchmarks and performance-regression detection

## What this is

The workspace benchmark architecture: the explicit targets that measure production
subsystems, the per-crate Criterion harness they share, and the two kinds of
regression comparison. It records the current target census rather than a work plan.

## Why this exists

One profiling investigation reduced median frame time from **17.05 ms** to **8.19 ms**
by replacing per-section camera-buffer updates (about 4,000 `queue.write_buffer` calls
per frame) with shared per-frame state. Main-thread use fell from 94% to 56% of one core.
The measured mechanism and capture conditions are retained in
[`../terrain-rendering.md`](../terrain-rendering.md). The benchmark suite protects the
structural counts that make this class of regression visible; it does not treat a
machine-specific duration as a CI threshold.

## What is measured, and why those things

The explicit Cargo targets cover the cost centres where a local change can otherwise look
harmless while scaling with world size, entity count, or render distance:

| area | current targets |
|---|---|
| Chunk generation | `lodestone-worldgen`: `generation` |
| Client chunk data and lighting | `lodestone-world`: `chunk_load`, `heightmap_decode`, `light_propagation`, `light_application` |
| Entities | `lodestone-entity`: `pathfinding_search`, `mob_tick`; `lodestone-shell`: `entity_tick` |
| Physics | `lodestone-physics`: `movement_integration`, `collision_sweep`, `pose_fit_gate`, `crowd_push` |
| Rendering | `lodestone-render`: `meshing`, `render_submit`; `lodestone-shell`: `render_submit`, `frame_profile` |
| Protocol | `lodestone-v26-2`: `chunk_light_decode`, `palette_expansion`, `nbt_decode`, `registry_decode` |
| Memory and server work | `lodestone-world`: `memory_footprint`, `session_rss`; `lodestone-server`: `server_tick` |

The census is **23 explicit `[[bench]]` targets across eight packages**. The packages
disable automatic bench discovery, so the recorder modules are not runnable benchmark
targets. This count is a registry of Cargo targets, not proof that every production
workload is represented.

## Harness design

- **Criterion** is the common runner. Each benchmark declaration sets `harness = false`
  and each owning package uses Criterion as a dev-dependency with
  `default-features = false, features = ["cargo_bench_support"]`; this keeps optional
  reporting and parallelism dependencies out of normal library builds.
- **Shared recording** lives in the native-only, opt-in
  `lodestone-testsupport::bench_record` module. The seven ordinary families retain
  tiny `benches/support.rs` re-export shims; worldgen keeps a local wrapper around it
  because counter-poisoning is specific to that crate. It appends one JSON object per
  metric to the gitignored `bench-results/<name>.jsonl` file:
  `{timestamp, git_sha, machine, profile, scene, metric, value, unit}`.
- **Fixtures** use shared realistic or synthetic terrain helpers where a benchmark needs
  terrain input. A benchmark must state which it uses, because fixture shape is part of
  what its result means.
- **Profiling** uses `samply`, release debug information, and CPU-time weighting. The
  workflow below is for attribution; Criterion and JSONL records are for repeatable
  measurement.

## How a regression is caught, without flaking CI

The repository deliberately uses two comparison paths because a duration and a
deterministic count have different reliability properties:

- **Prefer a ratio against something measured in the same run** wherever a natural
  pairing exists: old-path vs new-path, N vs 2N scaling, or single vs neighbourhood.
- **Where no natural pairing exists**, compare against a *stored baseline* from a previous
  run on the *same machine*, with a documented tolerance band (e.g. ±25%) — never a bare
  cross-machine absolute number.
- **Deterministic counts are CI-gated; durations are not.** `scripts/bench-gate.py`
  compares committed count baselines with a fresh run in CI. A count from committed code
  and a committed fixture is portable enough to gate; a duration is not. The gate rejects
  unexpected movement in either direction, including a suspicious improvement caused by
  a benchmark that stopped doing work. See
  [`../benchmark-regression-gate.md`](../benchmark-regression-gate.md).
- **Durations remain local, manual, or scheduled comparisons.** A recorded duration is
  useful only with its machine, load, profile, and scene; it must not become an implicit
  CI hardware requirement.
- **State what would represent the thing actually worth catching**, for every ratio gate
  — for example, superlinear work in column count or bind-group count increasing with
  resident section count.

### `cargo xtask bench-compare`

`cargo xtask bench-compare` compares two recorded JSONL runs without re-running a
benchmark. It defaults to adjacent matching runs and can instead select a candidate and
baseline by commit prefix.

```bash
cargo xtask bench-compare bench-results/light_propagation.jsonl \
  --metric neighbourhood_factor_vs_single \
  --scene "3x3 realistic terrain neighbourhood"
# -> baseline 9.6636x @ 33d0ad5bdfe4, candidate 9.7057x @ e95dbe39349f, ratio 1.004 -> OK
```

With no `--baseline`/`--candidate`, it compares the most recent recorded run against
the one immediately before it on the same machine and build profile (refusing the
comparison outright, rather than silently answering, if the two differ) — the same
pairing `record()` already does inline. Either can be pinned to a specific commit with
a git-sha prefix, for an explicit before/after comparison across a change:
`--candidate <sha-of-your-change> --baseline <sha-before-it>`. `--tolerance <pct>` sets
the band (default 25, i.e. ±25%, matching the literal in `support.rs`).

It prints a ratio and a verdict and exits non-zero when the ratio falls outside the
tolerance band. It is deliberately not a CI command: it compares two entries of a
*per-machine* log, which is the right instrument for a local before/after measurement
and the wrong one for a fresh runner.

The CI-facing counterpart is `scripts/bench-gate.py`, which compares a fresh run against
a *committed* baseline and so needs no local history — see
[`../benchmark-regression-gate.md`](../benchmark-regression-gate.md). The two are
complementary rather than alternatives: `bench-compare` handles durations on one machine,
`bench-gate` handles machine-independent counts everywhere.

The tool deliberately does not label a result "regression" or "improvement" — a metric
recorded in `bench-results/*.jsonl` carries no annotation of which direction is better
(lower is better for a `_ms` timing, higher is better for a throughput count), so it
reports the ratio and lets the caller, who knows what the metric means, read the
direction.

## Profiling workflow

> **Verified against `samply` 0.13.1** (`samply --version`), on a real capture, 2026-08-07.
> After upgrading it, run the parser controls:
>
> ```bash
> python3 scripts/test-profile-cost-table.py
> ```

1. **Build with debug info in release.** Already the default here --
   `[profile.release] debug = 2` is committed in the root `Cargo.toml` specifically for
   this (`samply`/Instruments profiling). A plain `cargo build --release` already
   carries the DWARF `samply` needs; no separate profiling profile to keep in sync.
2. **Install `samply`** if it is not already on `PATH` (`cargo install samply`, or see
   [mstange/samply](https://github.com/mstange/samply) for platform-specific setup --
   macOS needs no special permissions for a same-user process, Linux needs
   `/proc/sys/kernel/perf_event_paranoid` at 1 or below, or `sudo`).
3. **Record**, against the real release binary, with presymbolication on:
   ```bash
   samply record --save-only --unstable-presymbolicate \
     -o profile.json.gz -- ./target/release/lodestone
   ```
   `--save-only` writes a capture without opening the interactive UI; the sidecar emitted
   by `--unstable-presymbolicate` supplies symbols to the next step. Profile a real
   session, not only startup frames.
4. **Run the join**:
   ```bash
   python3 scripts/profile-cost-table.py profile.json.gz
   ```
   Prints two tables for the main thread (or `--thread <substring>` for another one):
   **inclusive** (this function or something it called) and **self** (leaf frames
   only), each weighted by `samples.threadCPUDelta` -- summed CPU time actually spent,
   not sample count, which is what makes this the *correct* instrument rather than the
   one that reads `acquire()` stalls as work ([`../camera-and-view.md`](../camera-and-view.md)'s
   occluded-`CAMetalLayer` finding is the same trap, found independently). The script
   warns loudly and falls back to sample-count weighting only if the capture genuinely
   has no `threadCPUDelta` data -- never silently.
5. **Read the sidecar-join warning line.** `symbolicated N raw address(es) via sidecar,
   M unresolved` -- a high `M` usually means the binary changed between recording and
   the sidecar being written (rebuild, then re-record) or the profiled process wasn't
   the one `--unstable-presymbolicate` actually symbolicated against.

`scripts/profile-cost-table.py --help` has the option reference. Its tests cover both
supported profile layouts, inclusive and self-time attribution, and library-relative
address joins. Captures and their symbol sidecars are local artifacts; do not commit
them.

## Evidence standards

- **Record the conditions.** A duration without machine, load, profile, and fixture or
  scene is not comparable. The JSONL record carries those fields with every metric.
- **Verify the detector, not just the benchmark.** A count gate needs a control that
  changes the count and is rejected; a target that only prints a duration is not a
  regression detector.
- **Keep the workload honest.** State whether a fixture is captured, embedded, or
  synthetic. A self-authored input can be sufficient for throughput measurement but does
  not establish protocol compatibility.
- **Read the command's real status.** Run the benchmark or gate directly rather than
  through a filtering pipeline that can hide its exit status.

## How to change it

Add a benchmark by creating an explicit `[[bench]]` declaration in its owning package,
setting `harness = false`, and adding the target to the census above. Keep the owning
package's `benches/support.rs` re-export shim for ordinary comparable metrics;
worldgen must continue using its local counter-guard wrapper. If the recorder schema
changes, update the shared module, its focused tests, and the shim surface only if the
API changes.

To add a CI-gated metric, first prove that it is a deterministic count or fixed-fixture
quantity, then add its committed baseline and extend the control suite. Do not add a
wall-clock duration to `bench-baselines/`; use `bench-compare` for that measurement.

## Configuration and dependencies

`bench-results/` is gitignored local data. `just bench-record` produces the gated subset,
`just bench-gate` compares it with `bench-baselines/`, and `just bench-baseline-update`
updates values without changing tolerances. The gate accepts only deterministic units;
[`../benchmark-regression-gate.md`](../benchmark-regression-gate.md) documents its
environment overrides and baseline format.

Benchmark packages depend on Criterion only for dev targets. Their shared fixture helpers
come from `lodestone-testsupport`; profiling additionally depends on an installed
`samply` and the repository's `scripts/profile-cost-table.py` parser.

## Verified remaining gaps

The benchmark infrastructure is in place, but its coverage is not complete:

- `IntegratedServer::tick_stats()` exposes phase samples, and `server_tick` records the
  sample counts and diagnostic worst-phase window. Its paused clock gives every phase a
  zero duration, so it cannot rank production phase cost; a real-time,
  production-shaped profile remains absent.
- The server-tick fixture is an in-memory floor rather than a generator- or region-backed
  production-shaped workload.
- The shared recorder is intentionally native-only and feature-gated because it writes
  local files and reads process metadata. Worldgen's counter-poisoning filter remains a
  local wrapper and must fail closed for unlisted units.
- Committed count baselines have not been validated across more than one machine class.
- GPU-dependent shell benchmarks need an adapter-equipped runner before they can join the
  CI-gated set.

The benchmark census also does not claim end-to-end coverage for systems without an
explicit Cargo target. Add a target only after defining the workload, fixture provenance,
and the appropriate duration-or-count comparison path.
