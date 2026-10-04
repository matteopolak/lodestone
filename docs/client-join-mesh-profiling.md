# Client Join and Mesh Profiling

## What it is

The ignored `client_join_mesh_profile` fixture measures a deterministic
singleplayer join from the world-open request through the connection phases,
initial-view delivery, CPU meshing, GPU upload, loading-screen readiness, and
the first presented terrain frame.

## How it works

The native `lodestone --benchmark singleplayer` workload instead measures an
actual textured window Surface, including HUD and acquisition. It creates a
normal Survival world with seed 4242 through the ordinary menus, waits for
playable terrain presentation, then warms up, stands, walks, and mines.
`--benchmark-walk-mine` also holds mining during walking and labels that phase
`singleplayer.walking_mining`. These are the existing movement, targeting and
held-attack consumers, not injected block mutations or an alternate tick loop.
Use `--sensitivity 0.5` for its one-time downward aim; record actual targets,
displacement, requested-view coverage and completed edits. Holding attack alone
does not prove an edit occurred. Set `--benchmark-mutation 0` to omit the final
stationary mining phase. Window size and resource availability must be verified
from the live run; requested dimensions and process exit zero are not evidence
of successful textured gameplay. See [presentation capture](presentation-capture.md)
for phase-scoped Surface submission records and retention limits.

The fixture starts its clock immediately before the same singleplayer open call
used by the interactive client. Resources and renderer state are created first,
matching the already-present menu screen. It uses the real `NetClient`, `Sim`,
terrain scheduler, `RenderState`, and `HeadlessTarget`, and emits one aggregate
`CLIENT_JOIN_MESH_PROFILE` record for seed 4242. The headless target receives
every uploaded section and a presented frame on each loop; it excludes the
window compositor and menu/HUD work. Its default target is 64×64, with a
configurable size for realistic GPU fragment work.
It reports observed mesh handoff cost to the same adaptive native frame budget
used by interactive redraw, so the fixture does not stay at the startup limit.
It uses the same eye camera and per-frame block/entity targeting step as the
interactive redraw path; those are required for an attack to reach a real block.
The fixture binds `NetClient` to the same ECS world the simulation renders,
as the interactive launch does. The new-world server pauses its initial ticks
until the fixture sends the post-present player-loaded acknowledgement. The
report includes that acknowledgement boundary; the server keeps streaming
padding columns afterward without holding the tick-owned action queue closed.
An optional movement phase keeps the same client and renderer running while the
player crosses into newly requested terrain.
`generation_phase_work` records calls, input-item counts, and elapsed sums for
the shared generation, snapshot, lighting, encoding, and delivery timers. The
movement record subtracts the counters at movement start from those at stop.
These are completed-operation intervals, not exclusive CPU time: parallel work
and nested timers can overlap, and an operation spanning the boundary is charged
when it finishes. The collector retains fixed phase totals, not per-column events.
Immutable admission separately reports queue wait, worker computation and
return-to-owner delay. Computation includes nested stage timers, so these
values must not be summed into an exclusive CPU partition.
`chunk_ingress` separately counts first loads and full replacements at the
authoritative client-world boundary. Exact block/biome/light storage equality
is not inferred from unchanged GPU uploads. Movement counts and comparison
elapsed sums subtract start from stop; the maximum comparison time remains a
session-lifetime maximum. Unknown adapter routes are excluded, and concurrent
counter reads need not describe one atomic packet application.
The schema-24 `server_tick.schedule_at_end` snapshot reports rolling wake-delay
p95 and session-lifetime wake/deadline maxima, catch-up admissions, recovery
yields and shed ticks. These include the whole active session, not only movement;
they separate executor service delay from the existing MSPT work measurement.
The default walking path holds forward, sprint, and jump. The optional flight
path requests creative mode through the integrated connection, waits for the
server's flight grant, then uses two jump presses to engage the normal flight
and movement packet path. It ascends to Y=200 through normal flight input
before flying forward and sprinting, keeping terrain out of the travel lane.
It starts the movement clock after the ascent, so command and takeoff time are
not confused with streaming latency. The report records
start/end height, position-correction count, and the first loss of flight,
if any, so an obstructed or canceled flight cannot be mistaken for streaming
throughput.

The report separates the capped initial loading square, the selected render
distance, and the integrated server's two padding rings. The first supplies the
mesher's 3×3 dependency neighborhood; the second keeps that neighborhood ready
as the player crosses a chunk boundary. It records received and
renderer-settled columns for each square, plus connection, overlay, first-terrain,
and full-server boundaries. `Sim::view_settlement_at_radius` queries the selected
distance without changing the initial loading gate. Frame, simulation-step,
upload, and render p99/max values expose hitches that aggregate CPU totals hide.
The initial square must not be mistaken for the full selected distance: a new
world caps the loading screen at radius six while the outer view keeps streaming.

For a radius-six, seed-4242 join at 1280×720, the 169 visible columns were
resident at 5.66 s, but their meshes settled at 7.72 s. A stage trace placed
the last radius-six delivery at 5.28 s and the last radius-seven halo delivery
at 7.31 s on the server clock; the client settled roughly 25 ms after the halo
arrived. The 56-column halo, not the per-frame mesh admission budget, explains
the gap. Generation dominated each later ring: encoding and delivery followed
the final generated column within about 10 ms. Check halo delivery before
attributing a resident-to-settled delay to client meshing.

Batch shape alone is not an adequate join optimization. In a local seed-4242,
radius-six release comparison, walking each ring as a contiguous perimeter
reduced the estimated cohort count but moved all-resident time only from 5.70
to 5.58 s. Retired instructions stayed near 178 billion, while peak RSS rose
from 493 to 546 MB, packet-neighbour admissions rose from 608 to 768, and
existing-column hits fell from 176 to 155. A larger cohort can pre-admit a
target that would otherwise reuse a neighbour completed by an earlier cohort;
preserve that reuse when changing admission order or cohort size.

In a paired native seed-4242 run at radius eight, a 1280×720 target, and 45
seconds of movement, the second padding ring changed newly visible columns
already presented on entry from 0/221 to 204/221. Median entry-to-presentation
fell from 451 ms to 0; the few early entries still waiting for the initial
outer stream kept p95 near 670 ms. First terrain stayed near 0.63 s, while the
loading overlay moved from 7.46 s to 7.71 s. Both runs held 20 TPS with no
tick overruns or frames above 33 ms. Peak resident size rose from 1.093 to
1.130 GB, and process retired instructions from 1.259 to 1.379 trillion. These
are local observations, not portable pass thresholds.
Separate startup fields cover GPU-context creation, client/resource construction,
and renderer construction without adding those costs to the world-open clock.
`chunk_count` is sampled after each simulation step as the world-insertion
boundary, avoiding a second drain of the network event channel. The loop is
paced at 60 Hz and advances the simulation by measured elapsed time. Samply can
profile the same test binary without changing the workload.

For CPU-counter evidence, run `just profile-join-hardware client --radius 1`.
This launches the exact release test executable under macOS Instruments and
writes the counters supported by the selected mode, process identity, and the
aggregate phase markers to one summary. The hardware wrapper defaults to radius
1 so a counter capture remains bounded; use `--run-id` for repeatable comparisons and
`--template` (or `LODESTONE_JOIN_XCTRACE_TEMPLATE`) for a local counter
template. Instruments reports these counters at process scope, so phase
durations remain wall-time markers rather than invented per-phase counters.
Legacy `counters-profile` tables must expose `Cycles Instructions` in that order
to report retired instructions and IPC. Instruments 27's Guided CPU Bottlenecks
mode instead exports named `cycle` metrics through `MetricAggregationForProcess`.
`profile-join-hardware.py::guided_counter_lines` selects the capture's exact target
PID and sums only precise buckets, validating column types and XML references.
Coarse buckets overlap the precise buckets: their totals are checked separately,
never added. Duplicate or overlapping buckets and disagreeing resolution totals
make the counters unavailable.

Guided mode reports captured active user-space cycles and the observed bucket
interval, with instructions and IPC explicitly unavailable. Its `Useful` metric
is a fraction of sustainable retired micro-operation bandwidth, normalized by
the core's maximum bandwidth. It is not a retired-instruction count and cannot
be converted into IPC. The installed Instruments analysis definitions specify
`sum` aggregation for cycles and `time-weighted-average` for bottleneck fractions.
Observed bucket intervals need not cover the entire process lifetime or any
individual workload phase. `scripts/fixtures/xctrace-guided-cycles.xml` preserves
an actual export excerpt for the focused parser tests; it is not a complete run.
The wrapper accepts Instruments' directory-backed trace bundles; the subsequent
table-of-contents export validates that the capture can actually be read.

With `LODESTONE_CLIENT_JOIN_MOVE_SECONDS` set, the fixture starts moving after
the loading overlay clears and terrain has been presented. It holds forward,
sprint, and jump, then records the delay to the first position change and chunk
crossing, visible-view mesh settlement, simulation ticks, pending meshes, and
frame/step tail latency during motion. The run fails if it never crosses a
chunk boundary; an unmoving player is not a valid streaming workload. Movement
can overlap delivery of the outer render-distance ring. The input is released
at the requested duration; any remaining time for the then-current view to
settle is reported separately rather than counted as extra movement.
Use a small-radius, longer flight to exhaust the initial buffer, and a larger
radius to measure sustained demand. Check `all_entered.preloaded_on_entry` before
calling a movement run a cold-stream test. The fixture's explicit radius does
not read the interactive client's saved render-distance option; compare like
settings, and do not treat radius-eight results as radius-32 performance.
The `view_at_stop` record separates missing columns from resident but unsettled
columns. A bounded 100 ms settlement tail records ready/waiting columns and
pending mesh sections through `Sim::mesh_backlog`, so a slow tail can be
assigned to delivery, admission, worker work, or upload without per-packet logs.
At most twelve one-second snapshots identify unsettled columns, their missing
sections, prior renderer presentations, and absent halo
coordinates. This distinguishes genuinely unseen terrain from a re-mesh that
temporarily invalidated an already presented section.
The record reports both strict latest-revision settlement and presented
coverage at movement stop. A section already on the GPU remains presented while
its replacement is built; a newly decoded column does not inherit that state.
For each chunk crossing, `new_view_columns` follows columns newly
exposed by the selected view square. It samples when each enters the view,
appears in the client world, and has all sections presented or known empty.
The load and presentation latency percentiles use only columns still visible at
the end of movement and report their completed counts alongside them; incomplete
columns are not silently treated as zero latency. Columns already loaded or
presented when they enter the view are counted separately; delivery percentiles
exclude the preloaded group. The 100 ms polling interval adds up to one sample
of uncertainty. First-mesh latency excludes columns already presented on entry:
a later replacement is not a first presentation. These timings separate stream delivery from client meshing
without relying on a global pending-work count. Unique and repeat section
upload counts show whether the renderer is receiving replacement meshes during
the same join, not just first-time geometry. `all_entered` retains every view-entry
episode, including columns that left the view before movement stopped. It counts
columns painted before exit and columns that left unpainted separately, and
reports entry-to-presentation latency only for completed episodes. This keeps a
long walk from hiding slow chunks that fell out of the final view. Each chunk
crossing samples the departing view once more; unresolved timings still have up
to one frame of observation uncertainty.

`mesh_work` counts source-column admissions, full-column snapshots,
neighbor-heal admissions, and light-patch
invalidations separately, so repeated uploads can be assigned to the path that
submitted them. Light-patch invalidations count loaded sections whose blocks
can sample the changed light; `light_patch_boundary_skips` counts non-air
adjacent sections excluded because their blocks are wholly interior.
`light_patch_absorbed_sections` counts non-air sections whose pending full-column
snapshot will read the patch without a separate light-only remesh, including
already presented sections. `column_absorbed_light_sections` counts previously
queued light intents consumed by a full-column capture of those same sections.
The native `mesh_work.native_scheduler` counters record submitted jobs, actual
geometry starts, jobs skipped before computation, and stale built results
discarded at handoff. They distinguish cancelled work from accepted geometry
that later produces an unchanged GPU upload; worker counters are independently
sampled atomics, not a transactional snapshot.
The movement timeline also samples each new column's 3×3 residency halo before
draining mesh results, then records its first returned section mesh and full
presentation. This distinguishes a column waiting for its outer dependency
ring from one queued behind worker or upload work. The halo sample is frame-
resolution; loaded and presented state still use the bounded 100 ms poll.
The fixture drains section removals before mesh results, matching the playable
renderer. `gpu_uploads_applied`, `gpu_uploads_unchanged`, and
`gpu_uploads_failed` report the production renderer's result for each mesh.
An unchanged result still settles the mesh queue, but does not rewrite GPU
buffers. `gpu_upload_cpu_ms` times only renderer handoff calls;
`mesh_upload_cpu_ms` also includes result accounting and readiness updates.
`removed_sections` counts unloads observed during the profile.
The native record also samples the integrated server's own tick clock and
world-tick witness at acknowledgement, movement start/stop, and completion.
`movement_tick_delta`, `movement_overrun_delta`, and the largest frame-sampled
gap between world-tick advances distinguish a server stall from a client
rendering hitch. These are server ticks, unlike the client's `sim_ticks` field.

`LODESTONE_CLIENT_JOIN_EDIT=1` aims downward after the first playable frame and
uses the normal attack path on a loaded block. The record separates the click
to local air-state change, replacement mesh upload, and first subsequent
presented frame. In survival, the first interval includes intended mining time;
it is not solely network latency. An absent ray target or unpresented edit
fails the bounded run. `LODESTONE_CLIENT_JOIN_CREATIVE_EDIT=1` waits for the
integrated server's creative ability grant before clicking, so the same
interval measures instant-break latency without block hardness. It requires
the edit phase and cannot be combined with item-drop profiling.
`LODESTONE_CLIENT_JOIN_DROP=1` then mines the exposed
block beneath it through the same input path, and records the delay to local
air, an authoritative item entity, its stack, and a frame with item geometry
submitted. It requires the edit phase and fails if no item frame appears within
12 seconds. When movement is also enabled, it begins after these effects have
been presented.

The browser SDK reports the same player-facing boundaries through `onProgress`.
Its clock starts in the production create-world action and emits transition-only
events for `joining`, `loading-terrain`, `loading-overlay-ready`,
`first-terrain-presented`, `gameplay-ready`, `full-view-presented`, and
`full-view-quiescent`. Each event includes `elapsedMs`, `loadedColumns`,
`expectedColumns`, `presentedColumns`, `settledColumns`, `pendingMeshes`,
`pendingColumns`, `pendingLightRemeshes`, and `pendingRemovals`.
`presentedColumns` counts requested-view columns whose sections have shown
geometry or an explicit empty result; it can retain earlier geometry during a
replacement. `settledColumns` counts columns whose every section has a latest
renderer handoff or explicit empty result. These are independent measurements,
so `full-view-presented` can report fewer settled than presented columns.
The first terrain and gameplay milestones force a current view sample after
frame presentation; unmeasured coverage is zero, never a reused earlier sample.

`pendingMeshes` counts scheduler work and ready results. `pendingColumns` counts
ready and forced column work, `pendingLightRemeshes` counts light intents awaiting
admission, and `pendingRemovals` counts renderer removals awaiting handoff.
`full-view-quiescent` requires latest requested-view coverage and all four queues
drained. Halo-only waiting columns do not block that milestone. These diagnostics
do not change gameplay input readiness. To extend the metrics, keep the shell's
`BrowserJoinTrace` samples and the SDK's `emit_join_progress` field mapping aligned.
Integrated joins immediately advertise the requested
render distance plus the shared two-column stream padding; this prevents the
configuration-phase default from shrinking the stream and making the visible
outer ring impossible to mesh.

Use the stable loopback origin `http://127.0.0.1:8080` for local browser
measurements so browser approvals can be reused. If that port is occupied,
identify its owner before starting the harness; do not stop an unrelated server
or silently switch to a new origin.

On the browser build, `?log=debug` also emits a bounded `wasm mesh drain and
upload profile` line once per second. It reports the rolling 120-frame p95 and
maximum frame-start gap, time spent in `Sim::drain_meshes` (CPU meshing), and
the renderer upload loop, the number of mesh results handed off during the
latest reporting interval, and peak ready-column, waiting-column,
forced-column, and pending-section backlog over that interval. The upload
duration is CPU wall time around the renderer handoff and readiness
acknowledgements; it does not measure when the GPU completes the work. The
browser's existing `frame_profile` summary still provides whole-frame phase
tails, with its `mesh_upload` phase combining CPU meshing and renderer handoff.
No frame or section records are emitted when debug logging is off.
The standalone join report retains `generationPhasesBeforeFullView`: bounded
aggregates of timing windows received after world-open/create started and before
the first full-view presentation event. This is a received-window observation,
not an exclusive CPU partition or an exact operation-boundary trace. The first
and final partial server reporting windows can fall outside those callback
boundaries; nested phase timers must not be summed. A new join resets the totals.
The companion `wasm mesh light sources` line separates cumulative local relight
inputs, jobs, visited cells, changed cells and skipped unchanged/light-equivalent
mutations from packet-path calls, admitted
sections, boundary skips and sections absorbed by a pending column capture.
Local counters belong to the app's debug-enabled lifetime; packet counters reset
with the terrain session. The probe retains their observed maxima under
`diagnosticSummary.meshLightSources`, not a sum of repeated cumulative reports.
Take a baseline in the same app/session before deriving action-local deltas.
Neither source count identifies every signal coalesced into a final mesh job.
`TerrainMesh::mesh_measurement` supplies fixed-size session totals for the
consumed request causes: column, section, light, and explicit snapshot. A
column cause includes coalesced neighbor-heal work; it does not identify every
signal replaced while that section waited in the queue. Capture timings cover
world-read acquisition and request resolution, including empty or deferred
results. Only accepted non-empty results increment `built`. Separate model,
fluid (including lava merge), visibility, packed-mesh, and output-fingerprint
timers report calls, elapsed sums, and maxima. The model and packed paths are
alternatives, not two passes over the same section. These phase sums exclude
queue accounting, tint diagnostics, and readiness bookkeeping; they need not
equal the whole drain time.
`TerrainMesh::record_mesh_handoff` receives the actual renderer result for each
measured mesh, preserving its consumed cause through the handoff. Per-cause
`applied`, `unchanged`, and `failed` counts distinguish CPU work producing an
identical renderer fingerprint from newly applied geometry and failed uploads.
An unchanged result still ran every CPU pass; it proves output equivalence at
the renderer, not equality of the block, biome, light, or resource inputs.
There is no input witness or meshing bypass. Aggregate storage is 960 bytes
per terrain resource plus one optional cause tag per result; no snapshot
handles, geometry cache, or per-section history are retained. Counters reset
on session end. Native and debug-disabled results have no measurement tag.
Measured model/fluid builds also collect their actual resolved sky/block reads
using an allocation-free 784-byte temporary bitset/counter probe. It counts
distinct packed light pairs, unique cells within the padded `-1..=16` cube,
total reads and out-of-domain reads. The probe is absent when measurement is
disabled and is discarded after each build. Its observation overhead is included
in the measured model/fluid times; these diagnostic timings are not uninstrumented
performance measurements. Empty-read, one-value, two-value, three/four-value and
larger-value mesh counts identify candidates for further investigation, not safe
reuse: even uniform inputs require geometry and resource identity proofs.
The per-cause `wasm mesh light reads` rows are cumulative, retained separately
as `diagnosticSummary.lightReadsColumn`/`Section`/`Light`/`Explicit`, and subtracted
in `lightReadCounterIntervals` with the same sampled-boundary/status rules as
mesh work. Extend `mesher::light_reads`, its aggregate in `mesher::measurement`,
the diagnostic in `app::redraw` and the probe parser together when changing this
measurement. There are no additional dependencies or configuration flags; it
uses the existing browser frame-profile switch.
The once-per-second `wasm mesh passes` diagnostic emits only causes with new
activity, with cumulative outcomes and phase elapsed sums/maxima. The standalone
responsiveness probe retains a latest row for each of the four causes and
fixed `diagnosticSummary.meshPassesColumn`, `meshPassesSection`, `meshPassesLight`,
and `meshPassesExplicit` groups, even after its 256 raw-sample cap. Metric names
start with `session` because values are measured-session totals or maxima,
not action-local counts or elapsed times. Repeated cumulative rows are never
added. A metric's `samples` counts received numeric observations during the
action, and `maximum` is the highest reported value in those observations.
These action summaries reset at action start and do not include baseline rows;
they cannot by themselves supply action deltas or whole-action percentiles.
Completed probe reports also export `meshCounterIntervals`, subtracting the
latest pre-action counters from the latest counters received during the action
for each cause. These are sampled intervals, not exact action partitions:
`baselineLagMs` and `tailLagMs` expose the gaps around the action, and
`sampledDurationMs` is the actual counter window. Phase sums and counts are
subtracted; cumulative maxima are not. Missing baselines, stale final samples,
incomplete rows and counter resets produce a status with `deltas: null`, never
invented zero work. These fixed four-cause records survive the raw sample cap
and are computed only when the opt-in probe stops. Update their arithmetic and
captured-row controls in `web/responsiveness_probe.test.mjs` when changing the
diagnostic field layout.
The standalone opt-in probe's `Walk + mine 20s` control holds the same ordinary
forward/sprint/jump inputs together with attack, and releases all of them on stop,
new join or worker failure. It neither changes game mode nor writes world blocks.
Check completed block-action traces and newly delivered columns before calling
the run a simultaneous edit/frontier stress test; walking with attack held can
miss blocks or be obstructed. Aim and game mode must be retained with the report.
`blockActionSummary` retains counts and fixed-field latency sums/maxima for
reports received while the action is active, even after its 32 raw block rows
are evicted. Missing milestones have no numeric sample; mining duration remains
gameplay time rather than transport delay. These are received-report aggregates,
not a complete attempt census: reports arriving after stop and reports lost
upstream are excluded, and no latency percentiles are inferred from the raw tail.
`web/build.rs` declares the imported probe JavaScript as a Cargo input: changing
it recompiles the entrypoint so wasm-bindgen's staged snippet cannot stay stale.
Verify the staged snippet when changing probe controls; editing generated output
is not a rebuild and does not establish the release's source identity.
Causes without received numeric observations remain absent. Phase milliseconds
are rounded to three decimal places in the diagnostic.
The mesh profile, queue aggregates, and CPU/GPU phase summary are forwarded to
the standalone page console, not just written to the render worker's console.
Periodic configured-view deficits and the independent Play-loop diagnostic
separate delivery gaps from resident geometry awaiting presentation.

The companion `wasm mesh queue` line reports cumulative insert, replacement,
cancellation, and pop counts plus current queued keys. Its high-water key count
captures admission peaks before the drain; high-water and maximum pop wait are
scheduler-lifetime values, not reporting-interval maxima. Oldest wait measures
the first admission of the front key, which replacements do not reset. These
counts exclude completed geometry and do not measure unique retained heap bytes.

The renderer runs in its own worker, so page Long Tasks do not measure its stalls.
The worker targets 60 Hz frame starts and subtracts input/redraw work before
waiting. A late frame still yields through a zero-delay timer; it never adds a
fresh full interval after heavy redraw work. Compare frame gaps against an idle
control rather than interpreting every gap above 16.7 ms as blocked work;
generation-worker health and renderer queue waits describe different scheduling
domains. The existing frame pacer still applies configured render limits.

## How to change it

Keep the workload finite and aggregate-only. Add measurements at existing
boundaries rather than logging packets or sections individually. Run the test
once as a control before attributing a hotspot to client work; a missing vanilla
atlas or GPU adapter is an environmental failure, not a valid zero result. Keep
the initial and selected-view settlement predicates separate when changing
readiness or render-distance behavior.
The native `NetClient` publishes a read-only `IntegratedTickMonitor` after it
opens a local server; it is absent for remote connections and browser workers.
Keep tick sampling out of the packet queue and avoid reading the tick-owned ECS
world directly.
Browser consumers should retain the transition events as one join record rather
than sampling console output or treating the SDK's mount-level `first-frame`
event as terrain readiness.
Keep the browser redundancy probe attached to the late-capture production path
and the actual renderer upload result. Call the handoff recorder exactly once
per returned result and publish its snapshot through the existing once-per-second
debug profile. Do not infer avoided meshing from unchanged output counts. Before
adding an input-equality optimization, measure the unchanged fraction by cause,
then account for every snapshot tag, light input, option, and classifier revision
in a bounded witness to the last successful renderer handoff.

## Configuration

Run `just samply-client-join-mesh` to build the release fixture and capture it.
The default radius is the normal client render distance; pass `--radius 1` for
the small control workload or `--dry-run` to print both commands without
building. `LODESTONE_CLIENT_JOIN_TARGET_SIZE=1280x720` selects a larger render
target; `LODESTONE_CLIENT_JOIN_RADIUS=8` selects the view radius when invoking
the test directly. `LODESTONE_CLIENT_JOIN_MOVE_SECONDS=20` adds a bounded
movement phase; unset or zero leaves the stationary join control unchanged.
`LODESTONE_CLIENT_JOIN_MOVE_MODE=flight` uses creative flight; `walk` is the
default. The flight profile requires the local integrated player to have game
mode permission.
`LODESTONE_CLIENT_JOIN_EDIT=1` adds the block-edit phase.
`LODESTONE_CLIENT_JOIN_CREATIVE_EDIT=1` makes that edit creative and instant;
it requires `LODESTONE_CLIENT_JOIN_EDIT=1` and excludes item-drop profiling.
`LODESTONE_CLIENT_JOIN_DROP=1` adds the item-drop phase and requires the edit
phase. The JSON `drop` object reports input-to-air, input-to-entity,
input-to-stack, and input-to-draw milliseconds separately; the first includes
the block's intended mining time.
`LODESTONE_JOIN_TRACE=1` enables the per-column server stage trace in this
fixture. Native traces distinguish initial queue entry, admission, worker start,
generation, encoding, and delivery; a later full-stage request has a separate
`upgrade_queued` entry. Queue-to-admission time measures scheduler backlog,
while worker-start-to-generation includes all work within a generation cohort.
This is diagnostic only: emitting lines per column perturbs timing, so compare
performance with the trace disabled.
The script discovers
the test executable reported by Cargo and wraps that exact executable with
`samply record --save-only`. If `LODESTONE_ASSETS` is unset, it uses the local
`.cache/mc/<version>` bundle when present. The simulation uses the live window-mode
resource path with a headless render target; headless simulation mode would
deliberately substitute the offline demo world and is rejected by the atlas
control.
The fixture checks settlement every frame until the initial acknowledgement;
afterward it samples the wider view at 100 ms intervals so observation does
not dominate the frame being measured.

## Dependencies

The fixture depends on the integrated server, the shell's client and mesh
pipeline, a headless wgpu adapter, and the vanilla asset bundle under the
repository's normal asset configuration.
