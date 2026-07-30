# Benchmarks and performance-regression detection

## What this is

The decomposition behind epic [#78](https://github.com/matteopolak/lodestone/issues/78):
what gets measured, why those things and not others, the harness shape, and how a
regression is caught without turning CI into a flake generator. The individual
measurements are filed as sub-issues of #78 (see the table below); this doc is the
argument for the shape they share, not a duplicate of any one of them.

## Why this exists

This repo has run exactly one real performance investigation, and it found a **2.08×**
frame-time win sitting in plain sight: `render_inner` rewrote every loaded section's
camera uniform every frame — ~4000 `queue.write_buffer` calls per frame, each allocating
and destroying a Metal staging buffer. Median frame time 17.05 ms → 8.19 ms; the main
thread went from 94% of a core to 56%. See issue #75 and
[`../section-camera-uniform.md`](../section-camera-uniform.md).

**Nothing would have caught that, and nothing would catch it coming back.** No benchmark
suite existed; the only reason #75 was found at all was a `samply` session run because a
frame felt slow, not because a gate turned red. That is the gap this epic fills — and the
fix it shipped is not universally applied yet: the packed/demo-world render path has the
*same* per-section-uniform shape today, tracked separately as issue #76, specifically
because nothing measures it.

## What is measured, and why those things

Chosen by where real cost has already been found or is structurally likely, not by
completeness for its own sake:

| area | why it's in scope |
|---|---|
| **Chunk generation** (server-side) | The one subsystem verified bit-exact against JVM oracles element-wise (noise router, density, carvers, surface+aquifer, ore features — `HANDOFF.md` §4), so per the evidence standard below it is "stable enough to benchmark meaningfully" without re-deriving correctness first. `HANDOFF.md` §4 also names an explicit open question: release-mode per-chunk cost was never measured until `bench_worldgen.rs`, and the per-stage cost split it once computed was thrown away rather than kept. |
| **Client chunk pipeline** | Decode, palette expansion, light application, meshing, upload, frontier remesh. This is the exact class of cost #75 lives in — the terrain path is the one place a measured 2×+ regression has already happened. |
| **Lighting** | `lodestone-world/tests/memory.rs` already has two real timing tests here, one of which (`measure_neighbour_light_cost`) was the repo's worked example of the wall-clock-ceiling trap (below) failing under load on a perfectly healthy 8.7× scaling factor. |
| **Entities** | Tick cost, interpolation, and the crowd push (`docs/entity-push.md`) are all real O(N) or O(pairs) mechanisms with no measured baseline and an easy path to accidental superlinearity — the entity-count analogue of #75's per-section bug. |
| **Physics** | Movement integration and collision sweeps run every tick for every entity against a real, non-trivial per-block-state shape census (32,366 states) — a plausible place for narrow-phase cost to hide. |
| **Render submit** | Draw-call and bind-group counts as a *measured* per-frame quantity, not an assumption — directly because #75 shows how badly an assumption can drift, and #76 shows the same shape already re-exists elsewhere in the tree. |
| **Protocol** | Packet decode/encode throughput for the highest-volume packet (chunk-with-light), NBT, registries — one-time or per-chunk cost that scales with render distance. |
| **Memory** | Resident-set growth over a session (observed climbing to ~600 MB in play), per-chunk/per-section footprint (already measured in `lodestone-world/tests/memory.rs`, just not tracked), and arena/atlas occupancy against the fixed ceilings `docs/section-camera-uniform.md` documents. |

The full per-item breakdown, with the specific file each benchmark extends or the specific
gap it closes, lives in the sub-issues of [#78](https://github.com/matteopolak/lodestone/issues/78)
(also on the [project board](https://github.com/users/matteopolak/projects/7)):

- **Harness** — [#79](https://github.com/matteopolak/lodestone/issues/79) (framework choice),
  [#80](https://github.com/matteopolak/lodestone/issues/80) (fixtures),
  [#81](https://github.com/matteopolak/lodestone/issues/81) (recording),
  [#82](https://github.com/matteopolak/lodestone/issues/82) (regression detection),
  [#83](https://github.com/matteopolak/lodestone/issues/83) (profiling workflow)
- **Chunk generation** — [#84](https://github.com/matteopolak/lodestone/issues/84) (throughput),
  [#85](https://github.com/matteopolak/lodestone/issues/85) (stage split),
  [#86](https://github.com/matteopolak/lodestone/issues/86) (parallel scaling + RNG determinism),
  [#87](https://github.com/matteopolak/lodestone/issues/87) (region-scale throughput and memory)
- **Client chunk pipeline** — [#88](https://github.com/matteopolak/lodestone/issues/88) (decode/palette),
  [#89](https://github.com/matteopolak/lodestone/issues/89) (light application),
  [#90](https://github.com/matteopolak/lodestone/issues/90) (`mesh_simple`),
  [#91](https://github.com/matteopolak/lodestone/issues/91) (`mesh_models`),
  [#92](https://github.com/matteopolak/lodestone/issues/92) (frontier remesh)
- **Lighting** — [#93](https://github.com/matteopolak/lodestone/issues/93) (tracked baselines
  for the two existing timing tests), [#94](https://github.com/matteopolak/lodestone/issues/94)
  (relight-after-block-change), [#95](https://github.com/matteopolak/lodestone/issues/95)
  (cross-chunk propagation at scale)
- **Entities** — [#97](https://github.com/matteopolak/lodestone/issues/97) (tick throughput),
  [#99](https://github.com/matteopolak/lodestone/issues/99) (interpolation),
  [#102](https://github.com/matteopolak/lodestone/issues/102) (crowd push),
  [#106](https://github.com/matteopolak/lodestone/issues/106) (render planning/upload),
  [#111](https://github.com/matteopolak/lodestone/issues/111) (pathfinding search)
- **Physics** — [#115](https://github.com/matteopolak/lodestone/issues/115) (movement integration),
  [#120](https://github.com/matteopolak/lodestone/issues/120) (collision sweeps),
  [#124](https://github.com/matteopolak/lodestone/issues/124) (pose-fit gate)
- **Render submit** — [#128](https://github.com/matteopolak/lodestone/issues/128)
  (draw-call/bind-group count), [#133](https://github.com/matteopolak/lodestone/issues/133)
  (`render_inner` CPU submit time)
- **Protocol** — [#137](https://github.com/matteopolak/lodestone/issues/137) (chunk-with-light
  throughput), [#142](https://github.com/matteopolak/lodestone/issues/142) (NBT),
  [#146](https://github.com/matteopolak/lodestone/issues/146) (registry decode)
- **Memory** — [#151](https://github.com/matteopolak/lodestone/issues/151) (session RSS growth),
  [#155](https://github.com/matteopolak/lodestone/issues/155) (per-chunk/section footprint
  tracking), [#160](https://github.com/matteopolak/lodestone/issues/160) (arena/atlas occupancy)

These numbers are a snapshot at filing time. If a sub-issue is closed, split, or renumbered,
the tracker (the sub-issue list on #78, or the project board) is authoritative — treat a
mismatch here as this doc having gone stale, not the tracker.

## What already exists

Four different ad-hoc shapes, none of them tracked:

- **`crates/lodestone-allocbench`** — its own binary family (one global allocator per
  binary; `--all-features` structurally cannot pass here and must exclude the crate).
  `bench.sh` drives it across thread counts and free-modes, reading peak RSS from
  `/usr/bin/time -l` and throughput from the binary's own `RESULT` line, into a CSV.
- **`crates/lodestone-server/examples/bench_worldgen.rs`** — `cargo run --release
  --example bench_worldgen`. Real generator, real columns, warm-up before timing,
  mean/median/p95/min/max per chunk, serial and parallel wall-clock with a speedup ratio.
  Prints to stdout only.
- **`crates/lodestone-render/tests/{world_mesher_bench,scene_bench}.rs`** — `#[test]`
  functions run through `cargo test`, with real anti-vacuity assertions (`quads > 0`,
  `CullStats::is_meaningful()`) alongside printed timing. Good sanity tests; no tracked
  baseline.
- **`crates/lodestone-world/tests/memory.rs`** — same shape, for heap footprint
  (`measure_*_column`) and light-recompute timing (`measure_light_recompute_cost`,
  `measure_neighbour_light_cost`).

None of these write a comparable, persisted number anywhere, and the four sources use
three incompatible output shapes (a binary's CSV, an example's stdout report, and two
different `#[test]` functions' `println!`s). The harness work below is choosing one shape
and retrofitting these, not starting from zero.

## Harness design

**Decision to be made and recorded by the harness sub-issues, not asserted here as
already settled.** The evaluation on the table:

- **`criterion`** for pure-function benchmarks over in-memory data — meshing, light
  propagation, physics integration, protocol decode. Its `--save-baseline`/`--baseline`
  flow gives local before/after comparison for free. Its dependency tree
  (`itertools`, `regex`, `walkdir`, and `plotters` unless disabled) needs checking against
  this workspace's `--all-features`/wasm-neutral constraints before any crate adopts it.
- **The existing hand-rolled `tests/*_bench.rs` shape** stays for anything that needs a
  GPU device, a multi-crate integration path, or `/usr/bin/time -l`-style RSS — none of
  which criterion measures.
- **Fixtures**: one shared "realistic terrain" fixture API (worldgen-backed for
  correctness-sensitive benches, a faster synthetic twin at the same public shape for
  benches that need many columns cheaply), so a meshing number and a light number are
  measuring comparable terrain instead of four different hand-rolled shapes as today.
- **Recording**: a plain append-only `bench-results/<name>.jsonl` (gitignored — local
  measurement data, not committed, same treatment as `target/`), one JSON object per run:
  `{timestamp, git_sha, machine, profile, config, metric: value, …}`. Machine, build
  profile and scene configuration travel with every number, per the evidence standard
  below — a number without them is not comparable across runs or across machines.
- **Profiling**: the `samply` + `debug = 2` + `threadCPUDelta`-weighting workflow that
  found #75, packaged as a repeatable script rather than tribal knowledge in a closed
  issue. `[profile.release] debug = 2` is already committed in the root `Cargo.toml`; the
  sampling/analysis half is not yet a tool.

## How a regression is caught, without flaking CI

Two incidents already burned this repo in exactly this spot, both recorded in
`CLAUDE.md`, and both shape the policy:

1. **The wall-clock-ceiling trap.** `measure_neighbour_light_cost` asserted
   `hood_best < 200.0` while its own comment named the *ratio* as the deliverable. A 3×3
   neighbourhood is nine columns; the measured factor was ~8.7× — essentially exactly
   linear and perfectly healthy — yet the ceiling silently implied `single_best < 22.2 ms`,
   an undocumented machine-speed constraint, and it failed under load. It is now
   `factor < 12.0`: a ratio against a paired measurement taken microseconds apart in the
   same run, so a busy CPU cannot flip the verdict.
2. **Duration is one of four species of vacuous test**, and the flaw is not readable from
   the test source — it lives in the relationship between test lifetime and system
   counters. A benchmark that only reports "N milliseconds" without a documented
   scaling expectation is this species waiting to happen.

The policy, applied to every benchmark under this epic:

- **Prefer a ratio against something measured in the same run** wherever a natural
  pairing exists: old-path vs new-path, N vs 2N scaling, single vs neighbourhood. This is
  the `measure_neighbour_light_cost` shape, generalised.
- **Where no natural pairing exists**, compare against a *stored baseline* from a previous
  run on the *same machine*, with a documented tolerance band (e.g. ±25%) — never a bare
  cross-machine absolute number.
- **Nothing under this epic fails a PR by default.** These are local/manual/scheduled
  checks a developer runs before or after a perf-sensitive change, reported as a diff
  against the stored baseline. The *sanity*-ceiling tests that already exist (anti-vacuity
  assertions like `quads > 0`, generous absolute ceilings like `mean_ms < 50.0`) stay in
  the normal `cargo test --workspace --all-targets` suite unchanged; only the *regression*
  numbers get baseline-diff treatment, and that treatment is explicitly not wired into a
  blocking CI gate as part of this epic — whether it ever should be is a decision for a
  later issue, once there is a body of tracked baselines worth gating on.
- **State what would represent the thing actually worth catching**, for every ratio gate
  — "superlinear in column count" for light, "bind-group count independent of section
  count" for render submit — mirroring the fix already made to
  `measure_neighbour_light_cost`.

## Evidence standards this epic inherits

From `CLAUDE.md`, restated here because a benchmark is where they matter most:

- **An expected value must originate outside the code under test.** For a benchmark the
  analogue is: a number is meaningless without the conditions it was measured under.
  Record machine, load, build profile and what the scene contained — every recorded
  result under the harness above carries this.
- **Never read a benchmark's success through a pipeline.** `cargo test --workspace | grep
  … | tail` once reported "exit code 0" while cargo returned 101 and its own last line was
  `error: 1 target failed:`. Let cargo write to a file and check its real exit status.
- **A truncated search is not a negative result.** `| head` once hid a real hit
  (`Player.java:1408`'s swim-descent constants) and produced a confidently wrong
  conclusion. Before writing "no such benchmark exists" or "this is unmeasured", grep for
  the producer across the whole tree, not the consumer in one named file.
- **A self-authored oracle validates the behaviour you chose to model.** Where a benchmark
  compares against our own encoder/decoder round-trip instead of captured server bytes, say
  so — the hermetic-fixture trap that produced "49 × 'unexpected end of input'" elsewhere
  in this repo is the same failure mode as a benchmark that only ever measures against
  itself.

## Worked example: issue #75

The shape every benchmark under this epic should be able to reproduce, in miniature:

| | before | after |
|---|---|---|
| median frame time | 17.05 ms | 8.19 ms |
| main-thread CPU | 94% of one core | 56% of one core |
| `write_buffer` calls/frame | ~4000–5000 (one per resident section) | 1 (shared uniform) + 1 arena write per newly-uploaded section |
| mechanism | per-section camera buffer + bind group, rewritten every frame | shared per-frame buffer (binding 0) + dynamic-offset arena (binding 1), written once per section lifetime |

Measured with `samply record --save-only --unstable-presymbolicate` against a release
build with `debug = 2` (no codegen change), on a live session (~94 s of play), weighting
samples by `samples.threadCPUDelta` rather than sample count — sample-count weighting
reads blocked time (e.g. `acquire()` stalling on an occluded `CAMetalLayer`, a real,
separately-documented trap in `docs/frame-pacing.md`) as work, which would have produced
a wrong attribution here. Full detail in `docs/section-camera-uniform.md`.

The number this epic should be able to reproduce on demand, going forward, is not "8.19 ms"
in isolation — it is the *shape*: bind-group and write-buffer counts staying flat as
resident section count grows, which is exactly what the render-submit sub-issues track.

## Known open gaps at the time of writing

- **#76** — the packed/demo-world render path (`BlockPipeline`, `RenderState::sections`)
  has the *same* per-section-uniform shape #75 fixed for the model/fluid path, bounded
  today only by the demo world's small radius. It is the first thing the render-submit
  benchmarks should measure, both as the honest current baseline and as the target for
  whatever fixes it.
- **Pathfinding is not "once it exists"** — `lodestone-entity/src/pathfinding` (search,
  navigation, node, heap, world — ~1900 lines) is a real, callable algorithm today. What
  is missing is the *consumer*: `lodestone-autopilot`, the plugin shell that would wire
  search results into gameplay, tracked separately as issue #38 and a confirmed island.
  The search algorithm's cost does not need to wait on that wiring to be benchmarked.
- **No benchmark in this repo runs a regression check that would fail a loaded CI runner
  by default.** That is deliberate for now (see "How a regression is caught" above); it
  is recorded here so it reads as a decision rather than an omission if revisited later.
