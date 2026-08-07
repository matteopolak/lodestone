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

**Update:** the harness decision below is now implemented for two crates —
`lodestone-worldgen` (`benches/generation.rs`, closing #84/#85) and `lodestone-v770`
(`benches/{chunk_light_decode,nbt_decode,registry_decode}.rs`, closing the protocol
decode benches under #137/#142/#146). See
[`../benchmark-harness.md`](../benchmark-harness.md) for how it actually works, how to
extend it, and why the `support.rs` recording helper is duplicated per-crate rather than
promoted to a shared crate. `lodestone-allocbench`, `bench_worldgen.rs`,
`world_mesher_bench.rs`/`scene_bench.rs` and `lodestone-world/tests/memory.rs` above are
untouched by this pass (out of its scope) and remain in their original ad-hoc shapes.

## Harness design

**Decided for worldgen and protocol decode (below); still open for the other areas'
sub-issues** (meshing, light, entities, physics, render submit — each is its own
harness-shape decision when that sub-issue is picked up, not automatically inherited).

- **`criterion`** for pure-function benchmarks over in-memory data — meshing, light
  propagation, physics integration, protocol decode. Its `--save-baseline`/`--baseline`
  flow gives local before/after comparison for free. Its dependency tree
  (`itertools`, `regex`, `walkdir`, and `plotters` unless disabled) needs checking against
  this workspace's `--all-features`/wasm-neutral constraints before any crate adopts it.
  **Checked for `lodestone-worldgen` and `lodestone-v770`**: both add it as a
  `default-features = false, features = ["cargo_bench_support"]` dev-dependency, which
  excludes `plotters`/`rayon`/`itertools`/`regex`/`walkdir` entirely — dev-only, so it
  does not touch either crate's wasm-facing lib build. See
  [`../benchmark-harness.md`](../benchmark-harness.md#configuration).
- **The existing hand-rolled `tests/*_bench.rs` shape** stays for anything that needs a
  GPU device, a multi-crate integration path, or `/usr/bin/time -l`-style RSS — none of
  which criterion measures.
- **Fixtures**: one shared "realistic terrain" fixture API (worldgen-backed for
  correctness-sensitive benches, a faster synthetic twin at the same public shape for
  benches that need many columns cheaply), so a meshing number and a light number are
  measuring comparable terrain instead of four different hand-rolled shapes as today.
- **Recording**: a plain append-only `bench-results/<name>.jsonl` (gitignored — local
  measurement data, not committed, same treatment as `target/`), one JSON object per
  *metric* per run: `{timestamp, git_sha, machine, profile, scene, metric, value, unit}`
  — one line per named metric rather than one line per run with several keys, so a new
  metric never needs a schema change. Machine, build profile and scene configuration
  travel with every number, per the evidence standard below — a number without them is
  not comparable across runs or across machines. **Implemented** as
  `benches/support.rs`'s `record()` in both `lodestone-worldgen` and `lodestone-v770`;
  see [`../benchmark-harness.md`](../benchmark-harness.md) for the exact shape and why
  the file is duplicated rather than shared.
- **Profiling**: the `samply` + `debug = 2` + `threadCPUDelta`-weighting workflow that
  found #75, packaged as a repeatable script (issue #83) rather than tribal knowledge in
  a closed issue. `[profile.release] debug = 2` is already committed in the root
  `Cargo.toml`; the sampling/analysis half is `scripts/profile-cost-table.py` -- see
  below for the full recipe.

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

### `cargo xtask bench-compare` (issue #82)

The policy above needed one concrete tool it did not yet have: a way to compare two
*specific* recorded runs from `bench-results/*.jsonl` without re-running a bench.
`benches/support.rs`'s `record()` already prints a ratio against the immediately
preceding same-machine/profile/scene run every time a bench executes — useful for
"did the run I just took regress vs the last one," but not for "compare the run from
before my change against the run from after it" once other runs (or other people's
runs, on a shared machine) have landed in between.

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
tolerance band — useful to a human, or to some future opt-in script — but nothing here
runs it: **issue #82 decides explicitly not to wire this into CI**, per the "nothing
fails a PR by default" policy above. A nightly or manually-triggered workflow that
shells out to it once a body of tracked baselines exists is a reasonable next step, not
attempted here.

The tool deliberately does not label a result "regression" or "improvement" — a metric
recorded in `bench-results/*.jsonl` carries no annotation of which direction is better
(lower is better for a `_ms` timing, higher is better for a throughput count), so it
reports the ratio and lets the caller, who knows what the metric means, read the
direction.

Demonstrated against `light_propagation.jsonl`'s real history above: issue #80's
fixture consolidation (`lodestone-world`'s `tests/memory.rs` and
`benches/light_propagation.rs` switching from a hand-rolled `realistic_terrain_column`
to the shared `lodestone_testsupport::bench_fixtures::synthetic_overworld_column`)
measured a 1.004× ratio on the neighbourhood-factor metric across that change — the
tool correctly reads that as "no real change," which is what the fixture swap was
supposed to produce.

## Profiling workflow (issue #83)

The end-to-end recipe that found #75's 2.08× win, now a repeatable tool instead of
tribal knowledge in a closed issue's writeup.

> **Verified against `samply` 0.13.1** (`samply --version`), on a real capture, 2026-08-07.
> The script parses samply's saved-profile format, which is not ours and has already moved
> underneath us once — so the version is recorded here to make the next drift
> *attributable* rather than a mystery. **After upgrading samply, run the gate:**
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
   `--save-only` skips opening the interactive UI server (this repo profiles headlessly,
   then reads the saved file); `--unstable-presymbolicate` is what writes the
   `profile.json.syms.json` sidecar the next step needs. Profile a real session (issue
   #75's own capture was ~94 s of actual play), not a few frames at startup.
4. **Run the join**:
   ```bash
   python3 scripts/profile-cost-table.py profile.json.gz
   ```
   Prints two tables for the main thread (or `--thread <substring>` for another one):
   **inclusive** (this function or something it called) and **self** (leaf frames
   only), each weighted by `samples.threadCPUDelta` -- summed CPU time actually spent,
   not sample count, which is what makes this the *correct* instrument rather than the
   one that reads `acquire()` stalls as work (`docs/frame-pacing.md`'s
   occluded-`CAMetalLayer` finding is the same trap, found independently). The script
   warns loudly and falls back to sample-count weighting only if the capture genuinely
   has no `threadCPUDelta` data -- never silently.
5. **Read the sidecar-join warning line.** `symbolicated N raw address(es) via sidecar,
   M unresolved` -- a high `M` usually means the binary changed between recording and
   the sidecar being written (rebuild, then re-record) or the profiled process wasn't
   the one `--unstable-presymbolicate` actually symbolicated against.

`scripts/profile-cost-table.py --help` has the option reference; its module doc covers
the join in full (both table layouts, `profile["libs"][i].debugName`, and the sidecar's
`data[].symbol_table`/`known_addresses`/`string_table`).

### How this workflow broke, and what now stops it (U20)

The script shipped reading exactly one profile shape, derived from
`fxprof-processed-profile`'s source rather than from a capture. Every `samply` 0.13.1
capture therefore died on `KeyError: 'shared'`, and **two separate agents (#496, #498)
hit it independently before it was fixed** — while `docs/roadmap/benchmarks.md` went on
documenting it as *the* workflow. Four distinct defects, all confirmed against a real
0.13.1 capture:

| # | defect | how it failed |
|---|---|---|
| 1 | read only the hoisted `profile["shared"]` tables | `KeyError: 'shared'` — **loud** |
| 2 | `stackTable.prefix` read as `prefixOffset` | **silent wrong answer** (below) |
| 3 | join documented on address alone | **silent misattribution** across libraries |
| 4 | sidecar spelling | already correct — `p.json.syms.json`, both spellings tried |

**The two layouts.** samply carries `stackTable`/`frameTable`/`funcTable`/
`resourceTable`/`stringArray` either **per thread** (`preprocessedProfileVersion`
absent or ≤ 55) or **hoisted** into `profile["shared"]` (≥ 56). **samply 0.13.1 emits
the per-thread form and no `preprocessedProfileVersion` key at all** (`meta.version: 24`),
so *absent means per-thread*, not unknown. Dispatch is on the version, never on
`"shared" in profile`: a presence check silently picks a branch when the format moves,
and in the per-thread layout **function indices are not comparable across threads**, so
a wrongly-picked branch reports a plausible table built from the wrong thread's indices.
A version above the script's `MAX_KNOWN_PROFILE_VERSION`, or a shape contradicting its
own version, is a hard error naming the version.

**`prefix` vs `prefixOffset` is the trap worth remembering**, because it is the one that
does not crash. `prefix[i]` is the parent's *index* (`null` at the root); `prefixOffset[i]`
is a *delta* (parent = `i - offset`, `0` at the root). Measured on the committed fixture:
reading one as the other leaves the **self-time table byte-identical** — a leaf is a leaf
whichever way you walk upward — while the **inclusive table silently loses the root frame**.
A gate asserting only self time is vacuous against this entire class, which is why
`scripts/test-profile-cost-table.py` asserts both and keeps the wrong reading as an
executed control.

**The join key is `(library, address)`.** `known_addresses` are library-relative, so
every library's `.text` starts at a small RVA and collisions across libraries are
routine. An address-only join does not fail — it files cost under whichever library was
indexed last. The gate builds a two-library fixture colliding at RVA `0x1000`, joins it
on address alone, and asserts all 100 units land on the *wrong* symbol; if the fixture
ever stopped colliding, that control would report itself premise-false instead of
quietly passing.

**The gate.** `python3 scripts/test-profile-cost-table.py` — stdlib only, no pytest.
20 checks over three committed fixtures in `scripts/fixtures/profile-cost-table/`: a
**real samply 0.13.1 capture** subsampled to 311 samples (~3 KB), plus two synthetic
colliding profiles, one per layout. The real capture's subject is a 3-function probe
whose `gamma` calls `alpha(n)` and `beta(n/4)`, so the expected **4:1 leaf ratio
originates outside this code entirely** — that is what separates "the join did not
crash" from "the join attributed to the right symbols"; the measured ratio is 5.01,
against 0.25 for the swapped-attribution hypothesis. Every fix is mutation-tested:
seven broken copies of the script (via `PROFILE_COST_TABLE_PATH`, which points the
suite at a copy so nothing in the shared checkout is edited) each turn the suite red,
and the layout mutation reproduces the original `KeyError: 'shared'` exactly.

**It is not yet wired into `cargo test`** — it is a Python script and no crate owns it.
Run it after a samply upgrade, or when touching the script.

**No profiling data is committed by this tooling.** `profile.json.gz` and its sidecar
are local, one-off artifacts of a specific investigation; check `git status` before
committing anything after a profiling session.

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
