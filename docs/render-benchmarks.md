# Render benchmarks

## What it is

Three complementary instruments for answering "where does frame time go, and did a
change actually help": a live in-process CPU/GPU frame profiler that ships in every
build, a reproducible criterion-based benchmark harness used across many crates
(worldgen, protocol decode, physics, and this cluster's own render/entity
benchmarks), and a full-client live benchmark that joins a real Java server and
measures the actual windowed game.

## How it works

### The live frame profiler

`FrameProfiler` (`crates/lodestone-shell/src/app/frame_profile.rs`) times each named
CPU phase of `WindowApp::redraw` — setup, sim tick, mesh upload, acquire, prepare,
world encode+submit, HUD/UI encode+submit, present — in fixed-size ring buffers, and
reports mean/p95/p99 with a skip count for any phase an early-return frame never
reached. The two largest phases are further split into sub-phases (world: prepare
buffers, terrain cull+draw, other draws, encoder finish, queue submit; HUD: debug
gather, frame gather, HUD draw, container draw, menu overlays, GPU-timing overhead),
because "3ms across 60 draws" and "3ms across 6000" are different problems a single
bucket cannot separate, and because `queue_submit` alone (as opposed to command
recording) is where the CPU actually blocks on GPU backpressure.

Real GPU pass timings ride alongside via `wgpu`'s per-pass `TIMESTAMP_QUERY`
feature (not the encoder-level variant, which Apple GPUs do not support) — four
segments: the block pass alone, the first-person hand pass alone, and two
**bracketing spans** (`world_total`, `hud_total`) that cover everything submitted in
each command buffer by stamping an empty dummy pass at each edge. Read the GPU
numbers against the CPU ones before optimising anything: a CPU phase measures how
long it took to *record* commands, not how long the GPU took to run them, and a
frame can be cheap to record and expensive to execute or vice versa. The two
bracketing spans are diagnostics, not measurements — an empty stamp pass shares no
attachment with the real work it brackets, so nothing actually orders it against
them, and both `span < enclosed pass` and `span >= sum of passes` have been observed
on real hardware (GPU passes pipeline rather than executing strictly in sequence).
Trust the two real per-pass segments; treat the totals as a hint.

Everything is visible live: F3 shows both blocks as text, Shift+F3 draws vanilla's
own pie-chart shape fed from the same counters (never inventing a fake second level
of nested wedges — the eight CPU phases are flat siblings, not a call tree), and
`RUST_LOG=frame_profile=info` emits the same two blocks once a second so a headless
or backgrounded session still records numbers. `LODESTONE_FRAME_PROFILE_DUMP=<path>`
writes one CSV row per frame (every phase/sub-phase, plus workload counts like
sections visited and chat lines) for offline analysis; a skipped phase writes an
empty cell, never a fabricated `0`, which would read as "free" rather than "did not
run".

`just bench-frame` (`crates/lodestone-shell/benches/frame_profile.rs`) is this
instrument's reproducible counterpart: a fixed camera path over a fixed demo world
at four waypoints chosen to hit different regimes (level, yawed, looking down for
maximum visible sections, looking up for minimum). It asserts counts and relations
within one run (residency must not move under pure camera rotation; a residency
sweep must actually grow with radius) rather than an absolute duration, and records
medians to a local JSONL history as an advisory before/after comparison. It cannot
exercise the real live-vanilla model path (needs no `client.jar`, only the packed/
demo path) or a real HUD — both stated in its own output rather than left for a
reader to discover from a suspiciously small number.

### The live client frame benchmark

`scripts/client-frame-benchmark.py` drives the actual fullscreen client joined to a
real Java 26.2 server, across four comparable workloads — `terrain` (normal generated
terrain), `showcase` (a dense authored plot: signs, heads, banners, item frames,
armour stands, mobs, displays, particles), `megaworld` (the official Hermitcraft
Season 10 save, an untracked local cache installed separately), and `lovelier`
(Stampy's Lovelier World, similarly untracked) — through a `warmup` →
`stationary` (30s) → `moving` (a 360° orbit, 60s) → `complete` state machine, with a
fresh data directory and offline username per trial so no persisted option or
account state leaks between runs. On macOS the runner refuses to trust a monitor's
name or primary-display flag (either external monitor can report as primary) and
instead maps every winit monitor to its CoreGraphics display id, requiring the
hardware-built-in panel and confirmed fullscreen before it will record a trial.

It reuses the same CSV/JSONL shape as the in-process profiler (documented above),
summarised per segment as frame-interval percentiles, budget-miss counts (16.67ms/
33.3ms), CPU/GPU phase means, and client RSS — comparable across runs by machine,
git sha and build profile. The frame interval is *everything* that delayed the next
redraw (CPU, GPU backpressure, compositor scheduling, OS noise); it is not
decomposable into "CPU part" and "GPU part" by subtraction, and the two bracketing
GPU spans carry the identical caveat as the in-process profiler's.

### Heavyweight local profiling scenes

`heavyweight` is profiler-first local evidence, not comparable history or CI timing
data. Its runner asks the release `heavy-scene-server` example for a versioned,
hashed command plan, validates that exact plan, sends the ordered setup,
post-join, and mutation commands through the normal local server, and launches the
release client through the same session, mesh, and presentation paths as play.
`--heavy-scenario`, `--heavy-seed`, `--heavy-scale`, and
`--heavy-mutation-seconds` choose deterministic emitted input; the runner never
rebuilds a command list or scene hash in Python. Smoke mode lowers scale and
durations, but never bypasses emitted witnesses. The runner permits scale 1–2,
mutation 0–10 seconds, and at most 120 seconds of client choreography; it also
always performs exactly one heavyweight trial, including Samply captures.

The frame CSV carries the production submission witnesses
`world.opaque_sections_drawn`, `world.water_sections_drawn`,
`world.translucent_sections_drawn`, `world.entities_drawn`,
`world.block_entities_drawn`, `world.sign_text_vertices`, and
`world.particles_drawn`. A required witness whose observed maximum is below the
emitted minimum invalidates the run; zero is evidence only that nothing of that
kind was submitted. The mutation segment is retained for relight/remesh reachability,
while stationary and moving segments are the timing interpretation arms.

`just profile-client-heavy` requires a prebuilt release `lodestone` binary and
records a bounded Samply capture plus a JSON scene sidecar under `bench-results/`.
Open the capture in Samply for a flamegraph/call tree, then run `just
profile-cost-table <capture>` for `threadCPUDelta`-weighted inclusive and self-time
attribution. Inspect the main thread and each named non-idle worker separately.
On macOS, the runner also requires fullscreen confirmation on the hardware-built-in
display and a native GPU adapter; it does not treat a successful process exit as a
render witness.

Before interpreting or sharing a saved heavyweight capture, run `just
validate-client-heavy-profile <capture>`. The runner performs the same check before
reporting a successful capture. It is a quick local artifact check: it requires a
nonempty capture, Samply `*.json.syms.json` sidecar, and the runner's
`*.record.json` scene record, then verifies the record's capture path, release
profile, scenario, bounded scales, scene hash, four phase durations, and stationary
and moving summaries. It intentionally does not decode the profile's potentially
large JSON payload; `profile-cost-table` owns the profile-format parse.

### The benchmark harness (criterion + `support`)

A generic per-crate criterion harness, currently implemented in the crates most
worth measuring, including this cluster's own `lodestone-render`/`lodestone-shell`
render-submit and meshing benches. Each bench function does two things: a one-shot
`std::time::Instant` measurement recorded via a small `support` module (one JSON
line per run — timestamp, git sha, machine, profile, scene, metric, value — appended
to a gitignored `bench-results/<name>.jsonl`, with an advisory ±25% ratio against the
last matching run) and an ordinary criterion `bench_function` for criterion's own
statistical sampling and local `--save-baseline`/`--baseline` before/after workflow.
Neither replaces the other: criterion's own numbers never leave `target/criterion/`
and carry no scene metadata, and the JSONL recording has none of criterion's outlier
detection.

The `support.rs` module is currently a deliberate copy-paste across each covered
crate rather than a shared dependency, kept identical by convention and a documented
diff check — promoting it to a real crate is judged worthwhile only once a fifth
site needs it, since every current site was added under concurrent-edit pressure
from other work in the same crates.

**Prefer a count to a duration wherever one is available**, and say so when it
isn't: a wall-clock figure on a shared or even a quiet machine has been measured to
swing 20%+ between two runs of an *identical* binary, which is wider than most real
optimisations. The render/entity bench batch follows this throughout — draw-list
sizes, mesh-arena occupancy, atlas occupancy, bind-group-switch counts and instance
buffer counts are all asserted as exact counts or count relations, with only a
handful of genuinely unavoidable measurements (CPU submit time, raw mesh timings)
recorded as an advisory baseline rather than gated. The concrete render-relevant
counters worth knowing about: `RenderStats::terrain_camera_bind_group_switches`
(counts by bind-group *pointer identity*, so a run of draws reusing one bind group
via dynamic offset correctly contributes one, not per-draw-call); and
`lodestone_render::atlas_occupancy` (used/total pixels, computed CPU-side from the
same sprite-rect data the atlas builder itself uses).

Two general benchmarking traps this harness has paid for and now guards against
mechanically wherever it can: a **world**-shaped fixture (an empty or uniform
section, an open-flat pathfinding scene) that degenerates to near-zero cost
regardless of whether the algorithm under test is correct, which every affected
bench now asserts against directly before timing starts; and a **duration**-shaped
trap where state persists across a naive iteration closure (a growing store, a mob
that has already reached its goal and gone idle), which needs `iter_batched` with
fresh per-iteration setup rather than one long-lived `b.iter` closure.

## How to change it

- **Add a CPU phase or GPU segment** to the live profiler: add a variant to
  `FramePhase`/`WorldSubphase`/`HudSubphase` (and its `ALL`/`name` list — nothing
  ties the two together at compile time, so double-check both after adding one),
  call `mark`/`record_world_subphase` at the new checkpoint. A GPU segment name
  must be appended (not inserted) to the segment list the timer is constructed
  with, because segment order fixes query-set indices.
- **Add a bench to a crate the harness already covers**: a `.rs` file under that
  crate's `benches/`, a matching `Cargo.toml` entry, `mod support;` for
  `support::record`. **Add the harness to a new crate**: copy `support.rs` from an
  existing one and update its header comment.
- **Change the regression tolerance**: the `0.75..=1.25` literal in every
  `support.rs` copy — change it in all of them at once.
- **Always run `--release`.** This workspace's debug backend is not representative
  of a real player's build; quote every number alongside which profile produced it.
- **Run a benchmark on an otherwise idle machine**, and treat a duration gathered
  under load as a sample, not a measurement — re-run alone before calling a
  timing-shaped result a regression.

## Configuration

- `LODESTONE_FRAME_PROFILE_DUMP=<path>` — per-frame CSV dump; unset records nothing
  extra and never panics if the path is unwritable.
- `RUST_LOG=frame_profile=info` (or broader) — the once-a-second tracing summary.
- F3 / Shift+F3 — the live text overlay and pie chart, in-game.
- `--benchmark terrain|showcase|megaworld|lovelier`, `--benchmark-debug-overlay
  closed|open`, `--benchmark-{warmup,stationary,moving} SECONDS` — the client's own
  live-benchmark flags; `scripts/client-frame-benchmark.py --trials N|--smoke|
  --samply|--debug-overlay closed|open|both|--binary PATH` drives them.
- `--benchmark heavyweight --heavy-scenario NAME --heavy-seed N --heavy-scale N
  --heavy-camera-plan stationary|orbit --benchmark-mutation SECONDS` — typed
  heavyweight client lifecycle input, normally supplied by the runner's emitted plan.
- `--validate-heavy-profile CAPTURE` / `just validate-client-heavy-profile <capture>`
  — validate a completed heavyweight capture and its sidecars without launching any
  workload or requiring Samply on `PATH`.
- `bench-results/*.jsonl` and `bench-results/live_frame_profile.jsonl` — gitignored,
  local-only history; a fresh clone has no baseline to compare against.
- Criterion CLI flags after `--` (`--quick`, `--sample-size`, `--save-baseline`/
  `--baseline`) work on every harness-covered bench.

## Dependencies

- `wgpu`'s `TIMESTAMP_QUERY` feature (native only; the profiler reports GPU timing
  as unavailable, never as a fabricated zero, everywhere it's absent, browser
  included).
- `crate::platform::Instant` (`lodestone-time`) for every CPU-side clock in the
  profiler — never `std::time::Instant::now()` directly, which traps on `wasm32`.
- `criterion` (`cargo_bench_support` feature only, no `plotters`/`rayon`) as a
  dev-dependency in every harness-covered crate; `serde_json` for the JSONL
  encode/decode.
- The live client benchmark additionally needs a release binary, the relevant local
  oracle server(s) under `.cache/mc/`, and (on macOS) CoreGraphics via
  `objc2-core-graphics` for authoritative built-in-display selection.
