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
world encode, HUD/UI encode+submit, primary encoder finish, primary queue submit,
present — in fixed-size ring buffers, and
reports mean/p95/p99 with a skip count for any phase an early-return frame never
reached. The two largest phases are further split into sub-phases (world: prepare
buffers, terrain cull+draw, other draws; HUD: debug
gather, frame gather, HUD draw, container draw, menu overlays, GPU-timing overhead),
because "3ms across 60 draws" and "3ms across 6000" are different problems a single
bucket cannot separate, and because `queue_submit` alone (as opposed to command
recording) can include CPU waits for GPU backpressure.

World and ordinary HUD commands share one primary encoder in the windowed draw
path. `world_encode` ends after world recording; `encoder_finish` and `queue_submit`
measure the actual combined command-buffer boundaries. The HUD/UI phase sums its
gather/recording span and its later container/menu span, excluding those boundaries.
The primary submit occurs before container, recipe-book, menu and screenshot work.
The recipe-book panel reuses the ordinary HUD's icon buffers, so its uploads must
remain after that submit. Standalone render wrappers retain their own submission.

The dump includes actual `primary.encoders_created`, `primary.encoders_finished`
and `primary.queue_submissions` counts for world and ordinary HUD only; independent
overlay and screenshot submissions are excluded. Counts drain once per finalised
frame, including early-return frames, and reset when a new profiler starts. A normal
visible-HUD frame has one of each; an empty HUD does not add an encoder. The timing
reader discovers columns dynamically, so historic `world_encode_submit` and
`world.encoder_finish`/`world.queue_submit` files remain readable with their original
scope. Compare total sequential CPU phases across layouts, rather than treating a
renamed or smaller world bucket as a saving.

GPU timestamps use `wgpu`'s per-pass `TIMESTAMP_QUERY` feature. The `world`
interval spans real world passes; `first_person` measures the optional hand pass.
Neither covers the complete GPU frame. Do not add overlapping intervals, subtract
them from CPU frame time, or treat them as a presentation-completion measurement.
Calibration against independent GPU captures remains separate from association
tests.

Each frame reserves one of three independent query/readback slots before writing
timestamps. A busy ring drops the measurement, not the render. Completed samples
retain frame and submission identity, raw ticks, written-edge masks, timestamp
period, and per-segment status. Query resolve remains at the world-encoding tail;
readback mapping begins only after the primary encoder's actual submit. The timer's
submission IDs count its world measurements, rather than every queue operation.
Missing hand work is `not_run`; malformed edges
are `invalid`, never a previous frame's value. The newest completed sample is
published atomically with its age and drop/error counters. Logs and benchmark
aggregation deduplicate frame IDs. CPU summaries cover a window; GPU log records
are individual asynchronous samples, not the same cohort's averages.

GPU timestamps are enabled while F3 is visible, or on native builds with
`LODESTONE_GPU_PROFILE=1`. Ordinary gameplay has no query passes, readbacks, or
profiling submission. CPU profiling and presentation capture remain independent;
keep them enabled for matched timer-on/timer-off comparisons. Closing F3 releases
the timer unless the explicit flag is set. Unsupported devices remain unavailable.
`RenderState::set_gpu_timing_enabled` controls this boundary for other consumers.

Block-sprite animation uniforms update only when the integer game tick changes.
Replacing the model resources resets that revision to the new buffer's tick-zero
contents; the next render updates it if needed. Frame-dependent effects and section
fade clocks are not throttled by this rule.

Everything is visible live: F3 shows both blocks as text, Shift+F3 draws vanilla's
own pie-chart shape fed from the same counters (never inventing a fake second level
of nested wedges — the ten CPU phases are flat siblings, not a call tree), and
`RUST_LOG=frame_profile=info` emits the same two blocks once a second so a headless
or backgrounded session still records numbers. `LODESTONE_FRAME_PROFILE_DUMP=<path>`
writes one CSV row per frame (every phase/sub-phase, plus workload counts like
sections visited and chat lines) for offline analysis; a skipped phase writes an
empty cell, never a fabricated `0`, which would read as "free" rather than "did not
run".

The window driver requests a redraw only when the pacer's scheduled deadline is
due. Unfocused, occluded, capped, and detached sessions still advance simulation
on short waits, but a skipped presentation cannot immediately queue another
redraw and spin. A dump dominated by rows without an acquire/present phase is
therefore a sign to inspect window state, not a high rendered frame rate.
Failed menu and world acquisitions defer the next acquisition by the existing
8 ms background-service interval. This retry deadline is separate from OS
occlusion and focus; simulation continues, and a successful acquire clears it.
`FramePacer` owns the deadline for both native and browser drivers.

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

`scripts/client-frame-benchmark.py` drives the production native client joined to a
real Java 26.2 server, across four workloads — `terrain` (normal generated
terrain), `showcase` (a dense authored plot: signs, heads, banners, item frames,
armour stands, mobs, displays, particles), `megaworld` (the official Hermitcraft
Season 10 save, an untracked local cache installed separately), and `lovelier`
(Stampy's Lovelier World, similarly untracked) — through a `warmup` →
`stationary` (30s) → `moving` (a 360° orbit, 60s) → `complete` state machine, with a
fresh data directory and offline username per trial so no persisted option or
account state leaks between runs. The default remains built-in fullscreen with
uncapped pacing and no VSync request. On macOS that fullscreen policy refuses to trust a monitor's
name or primary-display flag (either external monitor can report as primary) and
instead maps every winit monitor to its CoreGraphics display id, requiring the
hardware-built-in panel and confirmed fullscreen before it will record a trial.
The explicit `--benchmark-window windowed` policy instead requires an observed
physical 2560×1440 framebuffer with `fullscreen=false`; an OS-clamped window fails.
For resolution scaling controls, `--benchmark-resolution 1280x720` changes the
declared physical size in windowed mode only. The runner forwards that size,
includes it in trial identity, and rejects a different observed framebuffer.
Accepted widths are 320–8192 and heights are 240–8192. This does not change normal
game window sizing or infer compositor presentation from surface handoffs.
Fullscreen records keep their actual positive backing dimensions, which may differ
from the requested size.

It reuses the same CSV/JSONL shape as the in-process profiler (documented above),
summarised per segment as frame-interval percentiles, budget-miss counts (16.67ms/
33.3ms), CPU/GPU phase means, and sampled process-tree RSS — comparable across runs by machine,
git sha and build profile. The frame interval is *everything* that delayed the next
redraw (CPU, GPU backpressure, compositor scheduling, OS noise); it is not
decomposable into "CPU part" and "GPU part" by subtraction. Asynchronous GPU
samples carry the same scope and calibration caveats as the in-process profiler.

Before a run can be written to `bench-results/live_frame_profile.jsonl`, the runner
also requires at least one positive `world.model_sections_visited` sample in both
the stationary and moving segments. This is a deliberately small production
consumer witness: live world submission must have visited a model section, so a
missing or disconnected counter bridge cannot leave a no-op run looking like valid
timing data. A segment with no count samples, or whose supplied samples are zero,
fractional, negative, or non-finite, fails. The hermetic control suite is `just
test-client-frame-benchmark`; it exercises missing, zero, fractional, and positive
controls without requiring a GPU or local oracle.

### Retained native trial evidence

Selected-segment capture uses the same frame-start clock as the benchmark driver.
Its trigger advances once from armed to active to finished; leaving the selected
phase closes the report before the next frame begins. An interrupted or unreached
phase is an error, not a shortened successful trial. Browser benchmark mounts
forward the report through `onProgress` before renderer shutdown; manual capture
is unavailable during an automatic benchmark session.

On macOS, benchmark bring-up also logs `SurfaceTarget::metal_surface_state`:
the live display-sync switch, drawable-pool capacity, attachment-only policy,
transaction mode, acquisition-timeout policy and backing dimensions. Reading
these properties does not measure available drawables, GPU execution or scanout.
The diagnostic runs only at benchmark bring-up, not once per rendered frame.

`--artifact-dir PATH` retains each trial's original `frames.csv`, `client.log`,
`resources.json`, stationary `presentation.json`, declared `options.json` when
provided, and `trial.json` in a fresh runner-created subdirectory, including failed trials.
The temporary account/data directory is excluded. Without this flag the existing
temporary-file lifecycle and summary history stay unchanged. `trial.json` records
artifact hashes, status, requested durations/camera/overlay, heavyweight scene hash
when present, checkout SHA, actual binary digest and machine identity. Its immutable
configuration digest covers the requested graphics settings, window/pacing policies,
and supplied input identity;
it does not prove the binary was built from that checkout or with release settings.

Observed physical framebuffer/fullscreen/render distance and phase-transition log
records accompany redraw/present-row counts. Frame intervals are measured between
redraw starts; skipped presentations remain visible and are not displayed FPS.
The effective eleven-field graphics settings, cap, VSync request, and configured
presentation request come from the client's `benchmark window ready` record.
An explicit comparison fails when settings or policies are missing or mismatched.
Unknown GPU adapter, simulation distance, backend-resolved present mode, and binary
build provenance remain `null`. `configured_present_mode` observes the wgpu API
request; `AutoNoVsync` does not prove uncapped hardware or compositor delivery. The
summarizer rejects empty, non-positive and non-finite measured intervals.

### Declared native comparison settings

`--settings PATH` accepts exactly these eleven production `Options` fields and
writes them into the fresh trial data directory. It rejects incomplete objects,
unknown or duplicate keys, wrong types, and out-of-range values before launching:

```json
{
  "framerate_limit": 60,
  "enable_vsync": false,
  "inactivity_fps_limit": "minimized",
  "fov": 70,
  "render_distance": 24,
  "graphics_preset": "custom",
  "cloud_status": "off",
  "cutout_leaves": true,
  "biome_blend_radius": 2,
  "entity_shadows": true,
  "particles": "all"
}
```

Integer ranges are `framerate_limit` 10–260, `fov` 30–110, `render_distance` 2–256,
and `biome_blend_radius` 0–7. The three boolean fields are `enable_vsync`,
`cutout_leaves`, and `entity_shadows`. Enum values are `minimized|afk` for inactivity,
`fast|fancy|fabulous|custom` for graphics preset, `off|fast|fancy` for clouds, and
`all|decreased|minimal` for particles. A blend radius of 2 samples a five-by-five area.
An entity-distance scale is unsupported and rejected rather than recorded as applied.

Windowed policy and `--benchmark-pacing options` require a settings declaration.
Options pacing uses the ordinary loaded cap/VSync consumers: 260 means unlimited,
not a 260-FPS cap. Historical `uncapped` policy overrides both to `(none,false)`
while still loading the declared visual settings. Prefer `inactivity_fps_limit=minimized`
for stationary comparisons; `afk` can lower the effective cap after inactivity.
The client CLI distance equals the declared options distance and the oracle's temporary
view distance is one chunk larger. Changing the preset alone does not establish visual
parity; the explicit visual fields and observed scene still need to match the other arm.

For example, save the declaration above as a local `comparison-settings.json`, then:

```sh
python3 scripts/client-frame-benchmark.py --workload terrain --trials 1 \
  --settings comparison-settings.json --benchmark-window windowed \
  --benchmark-pacing options --warmup-seconds 20 --stationary-seconds 3 \
  --moving-seconds 3 --artifact-dir bench-results/comparison
```

The three duration overrides replace only their specified defaults, including during
`--smoke`; stationary and moving durations must be positive. Heavyweight duration
still has its 120-second total bound. Settings and the written options bytes have
separate hashes, and requested identity stays separate from the client's observed values.
The retained options contain only declared graphics fields; account and user options
are never copied. Use `--artifact-dir` whenever raw evidence must outlive the trial.

Every runner trial selects `<workload>.stationary` through
`LODESTONE_PRESENTATION_CAPTURE_SEGMENT` and writes `presentation.json` via the
existing production capture. It requires schema 2, zero `droppedRows`, positive
elapsed time, nonzero successful world handoffs, consistent attempt/submission counts
and intervals, and effective cap/VSync context on successful rows. Percentiles use
successful post-present handoff intervals, separately from redraw-start CSV intervals.
The observed elapsed duration and skips remain explicit. `skipReasons` contains
fixed whole-capture counts for pacing, missing menu/world resources, and each
menu/world acquisition error, including
occlusion, timeout, outdated configuration, lost surface, and validation failure.
An unclassified count identifies an early return without an explicit reason.
Counting continues after the row cap; no per-attempt diagnostic log is emitted.
The capture stores at most 32,768 attempts (an export of at most about 6.3 MB);
a long uncapped stationary phase can overflow even though aggregate counts cover
the full phase. Shorten `--stationary-seconds` when it fails; a retained prefix is
not accepted as a full-interval percentile sample. Ten seconds leaves room for
roughly 3,000 attempts per second without truncation; legacy 30-second uncapped
defaults can still need a duration override on fast systems.

Handoffs do not prove compositor display cadence. Queue-completion callback delays
in the raw capture are neither GPU execution time nor scanout latency. The log's GPU
timestamp summaries pool snapshots across the whole launch and remain bottleneck
context rather than a stationary per-frame GPU series. Settings validation does not
prove scene equality, settled pipelines/chunks, or per-trial world restoration.

`LODESTONE_BENCHMARK_SCREENSHOT=1` requests a normal surface screenshot on the
transition from stationary to moving. The PNG is written under the trial's working
directory in `screenshots/`; its readback is outside the stationary measurement.
This native-only diagnostic uses the same capture path as the screenshot key.

The [resource sampler](client-resource-sampling.md) records the launched PID and
its descendants at one-second requested intervals, including a profiler wrapper
when enabled. Resource summaries cover the whole launch, not individual frame
segments. RSS sums can double-count shared pages; interval CPU comes from observed
cumulative CPU deltas, with 100% representing one core. Missing values remain
`null`, and sampling overhead, attribution gaps and bounds stay in the raw report.
The compatibility RSS fields now use this tree scope instead of sampling only
the launcher's PID. Their `end` value is the last live observation, not post-exit
memory. No additional RSS-only process query runs. Resident GPU memory and dedicated
VRAM remain unavailable unless a separate platform instrument supplies them;
tracked GPU allocations must not be substituted for residency.

Optional `--world-snapshot-manifest PATH` requires retained artifacts and declares
the exact files to hash under the selected oracle's `world/` directory:

```json
{"schema":1,"files":["level.dat","region/r.0.0.mca"]}
```

The oracle must already be stopped. Both local game/RCON ports must refuse a
connection before and after hashing; an active server or ambiguous connection error
fails preflight. The declaration is capped at 1 MiB, 512 unique canonical relative
regular files and 1 GiB total content; symbolic links, traversal, changed files and
an optional mismatched `snapshot_sha256` fail. The aggregate digest hashes canonical
JSON containing `schema: 1` and sorted `{path, bytes, sha256}` entries. This reads
only declared content, once before oracle startup, and never copies a large world.
Include every relevant dimension/entity/block-entity/level file for the scene.
Declared-file coverage is explicit: an archive-install marker is not substituted for
actual world bytes, and complete coverage or per-trial world restoration is not
inferred. Setup commands and subsequent server activity can change the world after
that prelaunch snapshot; this checkpoint does not yet implement reset/replay or a
Java-client comparison adapter.

For an official-texture comparison, `scripts/prepare-vanilla-comparison-assets.py`
stages cached same-release resources in a new isolated directory below
`.cache/benchmarks/`. It verifies `version.json`'s release id, checks archive CRCs,
and copies the complete original jar once as `lodestone-resources.zip`; no textures
are removed/replaced and no second jar cache copy is made. The supplied report goes
to `generated/reports/blocks.json`. `vanilla-comparison-assets.json` retains source
and staged SHA-1/SHA-256 identities plus a sorted inventory with every texture PNG's
path, size and SHA-256. Existing destinations are refused. Failed copies may leave
partial output without a valid completion manifest; choose a new destination for a retry.

The current native runner requests release 26.2/protocol 776, so its inputs must be
26.2. For example, after obtaining the matching cached report:

```sh
python3 scripts/prepare-vanilla-comparison-assets.py \
  --jar .cache/mc/26.2/client.jar \
  --blocks-json .cache/mc/26.2/generated/reports/blocks.json \
  --release 26.2 --out .cache/benchmarks/vanilla-26.2

LODESTONE_ASSETS="$PWD/.cache/benchmarks/vanilla-26.2" \
  python3 scripts/client-frame-benchmark.py --workload terrain \
  --stationary-seconds 3 \
  --artifact-dir bench-results/comparison
```

The existing `resources::asset_root` / `resources::open_pack_stack` consumer loads
that native override. The runner creates a clean `LODESTONE_DATA_DIR` for each trial,
excluding user resource-pack selections; direct binary launches must supply their
own new empty data directory. A separate `--release 26.3` stage is valid preparation,
but must not feed the current 26.2 runner before its production version cutover.
`--expected-sha1 HASH` rejects a source-jar mismatch against an independently
provided digest. The helper records that match, but does not verify the digest's
official provenance or the report's release by itself; keep independent download/
report-generation evidence with its manifest. Indexed sounds/fonts may need further
same-release official inputs: a full jar does not establish completeness of external
asset-index objects. Dependencies are Python's standard library and cached inputs;
there are no downloads or builds. Change the staging/inventory rules in `stage_assets`
and `inspect_archive`; finite controls run with `python3
scripts/test-prepare-vanilla-comparison-assets.py`.

The browser's existing initial SDK `resourcePack`/`blocksJson` byte inputs can consume
the staged resources in a benchmark host; its current stripped Whimscape artifact
does not match the official texture set. This override is benchmark-only: preserve
default packs, native/browser release assets, asset discovery fallbacks and portfolio
resources, and do not ship the official staged assets. Java/browser replay remains
separate work from native evidence retention and isolated staging.

### Integrated singleplayer Surface capture

`--benchmark singleplayer` creates a normal survival world with seed `4242`
through the menu's ownership and save handling, then uses the same integrated
server, controller, mesher, HUD and window Surface as play. Saves are isolated
under the temporary `lodestone-surface-<process-id>` directory. Persisted options
and the real account roster are read without copying credentials or synthesizing
ownership. An unowned launch fails rather than bypassing authorization.

The join clock starts before world creation. Choreography starts only after
ready terrain is handed to the Surface and the initial player-loaded message
is sent, not at protocol login. The phases are warmup, stationary capture,
sprint/jump walking without creative flight or camera orbit, and downward-look
mining. Walking precedes mining so digging underfoot cannot trap the exploration
phase in its own hole. Mining defaults to three seconds; `--benchmark-mutation` overrides it.
The other existing benchmark duration flags apply unchanged. A join still
waiting after 120 seconds logs a failure and requests clean shutdown.

Use a prebuilt release binary with staged resources:

```bash
LODESTONE_FRAME_PROFILE_DUMP=/private/tmp/lodestone-surface.csv \
RUST_LOG=warn,frame_benchmark=info,frame_profile=debug \
  lodestone --benchmark singleplayer --render-distance 8 \
  --benchmark-warmup 2 --benchmark-stationary 5 --benchmark-moving 20
```

The existing frame CSV records real acquire, draw/submit and present work.
Once-per-second `frame_benchmark` samples record position, resident/presented/
expected view columns, mesh backlog, RSS and integrated tick phase/wake maxima.
The samples also expose occupied and reserved terrain-mesh GPU buffer bytes.
These allocation counters exclude textures and other GPU buffers; they are not
total resident GPU memory or dedicated VRAM.
The ordinary benchmark fullscreen, resolution and uncapped presentation policy
still applies, so these are stress captures rather than persisted-option play.
An attack-request marker is not a successful-edit acknowledgement; inspect
the resulting pixels and authoritative updates. A walking phase can meet a
terrain wall, so verify displacement and newly presented chunks before treating
its duration as exploration evidence.

Change choreography in `app::benchmark::BenchmarkDriver` and its production
seams in `app::singleplayer_benchmark`, `WindowApp::draw_menu` and
`WindowApp::redraw`. Preserve the
normal menu launch and post-present readiness boundary. The workload depends
on the shell's existing resource staging, account metadata, integrated server,
window renderer and frame profiler; it does not need an external Java server.

### Heavyweight local profiling scenes

`heavyweight` is profiler-first local evidence, not comparable history or CI timing
data. Its runner asks the release `heavy-scene-server` example for a versioned,
hashed command plan, validates that exact plan, and launches the release client
through the same session, mesh, and presentation paths as play. Post-join and
mutation remain individual normal local-server commands. Setup is materialized as
a runner-owned temporary datapack function, reloaded once, then called once through
RCON. Each producer's normal command runs through the function dispatcher and
reports success to a temporary aggregate; the function returns the expected count
only when every producer succeeded. The temporary pack and aggregate are removed
after the run, including setup failures.
`--heavy-scenario`, `--heavy-seed`, `--heavy-scale`, and
`--heavy-mutation-seconds` choose deterministic emitted input; the runner never
rebuilds a command list or scene hash in Python. Smoke mode lowers scale and
durations, but never bypasses emitted witnesses. The runner permits scale 1–2,
mutation 0–10 seconds, and at most 120 seconds of client choreography; it also
always performs exactly one heavyweight trial, including Samply captures.

`mixed` is the ordinary broad scene. `dense-mixed` reuses those same production
builders at a fixed scale-one envelope: 2,048 tagged entities; 1,536 signs; 1,024
each of light and liquid cells; 768 transparent cells; 512 palette cells, block
entities, and scheduled producers. It is intentionally capped at scale one. Its
complete 7,937-command setup is retained even by smoke runs, but reaches the
production command dispatcher through the one temporary function invocation rather
than 7,937 request/response round trips. Reload plus that invocation share a
90-second wall deadline, so a stalled oracle cannot expand into thousands of
15-second socket waits. The emitted post-join phase places the benchmark player
above the compact subject volume before the warmup begins; that command is consumed
by the same local-server command path as the scene setup, rather than by a
renderer-only fixture. Use
`just profile-client-heavy-dense` for this opt-in Samply input.

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
and moving summaries. Each measured summary must retain a positive frame count,
finite non-negative timing percentiles in order, budget-miss counts, and the
summary object shapes emitted by the runner. This rejects an otherwise complete
sidecar handoff whose measured phase is an empty or zero-frame placeholder. Its
current record schema also names the client camera plan:
`stationary` holds the production benchmark still during the moving segment, while
`orbit` drives its deterministic full turn. This prevents two materially different
render-consumption paths from being compared as if they were one scene. The
validator rejects prior underidentified records and a dense-mixed record above its
fixed scale-one envelope. It intentionally does not decode the profile's
potentially large JSON payload; `profile-cost-table` owns the profile-format parse.

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
- **Extend declared comparison settings**: update `SETTINGS_FIELDS` and its typed
  validators, `_observed_trial_metadata`, and the client's bring-up metadata together.
  The setting needs a production consumer. Update `PRESENTATION_COLUMNS` only with
  the production capture schema and preserve the separate redraw/handoff boundaries.
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
- `--settings PATH`, `--benchmark-window builtin-fullscreen|windowed`,
  `--benchmark-pacing uncapped|options`, and `--{warmup,stationary,moving}-seconds N`
  — runner comparison settings, policies, and duration overrides. Window/pacing
  policies also exist on the client CLI; duration overrides forward to its existing flags.
- `LODESTONE_PRESENTATION_CAPTURE=<path>` and
  `LODESTONE_PRESENTATION_CAPTURE_SEGMENT=<workload>.stationary` — production native
  handoff capture selected and retained by the runner.
- `--benchmark heavyweight --heavy-scenario NAME --heavy-seed N --heavy-scale N
  --heavy-camera-plan stationary|orbit --benchmark-mutation SECONDS` — typed
  heavyweight client lifecycle input, normally supplied by the runner's emitted plan.
- `--validate-heavy-profile CAPTURE` / `just validate-client-heavy-profile <capture>`
  — validate a completed heavyweight capture and its sidecars without launching any
  workload or requiring Samply on `PATH`.
- `just test-client-frame-benchmark` — run finite, no-GPU controls for client-run
  completion, window/pacing policy, settings identity, capture validity, and the
  production render-submission witness.
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
