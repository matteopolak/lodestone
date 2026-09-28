# Client Join and Mesh Profiling

## What it is

The ignored `client_join_mesh_profile` fixture measures a deterministic
singleplayer join from the world-open request through the connection phases,
initial-view delivery, CPU meshing, GPU upload, loading-screen readiness, and
the first presented terrain frame.

## How it works

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
The new-world server pauses its initial ticks until the fixture sends the same
post-present player-loaded acknowledgement as the interactive redraw path.
The report includes that acknowledgement boundary.
An optional movement phase keeps the same client and renderer running while the
player crosses into newly requested terrain.

The report separates the capped initial loading square, the selected render
distance, and the server's additional meshing halo. It records received and
renderer-settled columns for each square, plus connection, overlay, first-terrain,
and full-server boundaries. `Sim::view_settlement_at_radius` queries the selected
distance without changing the initial loading gate. Frame, simulation-step,
upload, and render p99/max values expose hitches that aggregate CPU totals hide.
The initial square must not be mistaken for the full selected distance: a new
world caps the loading screen at radius six while the outer view keeps streaming.

On a quiet native seed-4242 run at selected radius eight and a 1280×720 target,
first terrain was presented at 0.66 s, the initial 169-column view was settled
at 9.16 s, all 289 selected-view columns at 14.48 s, and all 361 requested
server columns at 14.08 s. Frame p99 was 4.63 ms with one 56 ms maximum; the
remaining wall time was between chunk arrival and settled meshes, not sustained
frame-thread or GPU submission saturation. These are local observations, not
portable pass thresholds.
Separate startup fields cover GPU-context creation, client/resource construction,
and renderer construction without adding those costs to the world-open clock.
`chunk_count` is sampled after each simulation step as the world-insertion
boundary, avoiding a second drain of the network event channel. The loop is
paced at 60 Hz and advances the simulation by measured elapsed time. Samply can
profile the same test binary without changing the workload.

For CPU-counter evidence, run `just profile-join-hardware client --radius 1`.
This launches the exact release test executable under macOS Instruments and
writes retired instructions, cycles, IPC, process identity, and the aggregate
phase markers to one summary. The hardware wrapper defaults to radius 1 so a
counter capture remains bounded; use `--run-id` for repeatable comparisons and
`--template` (or `LODESTONE_JOIN_XCTRACE_TEMPLATE`) for a local counter
template. Instruments reports these counters at process scope, so phase
durations remain wall-time markers rather than invented per-phase counters.
The selected template must expose `Cycles Instructions` in that order; other
counter layouts are reported as unavailable rather than interpreted as this
pair.

With `LODESTONE_CLIENT_JOIN_MOVE_SECONDS` set, the fixture starts moving after
the loading overlay clears and terrain has been presented. It holds forward,
sprint, and jump, then records the delay to the first position change and chunk
crossing, visible-view mesh settlement, simulation ticks, pending meshes, and
frame/step tail latency during motion. The run fails if it never crosses a
chunk boundary; an unmoving player is not a valid streaming workload. Movement
can overlap delivery of the outer render-distance ring. The input is released
at the requested duration; any remaining time for the then-current view to
settle is reported separately rather than counted as extra movement.
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
For each chunk crossing, `new_view_columns` follows the unique columns newly
exposed by the selected view square. It samples when each enters the view,
appears in the client world, and has all sections presented or known empty.
The load and presentation latency percentiles use only columns still visible at
the end of movement and report their completed counts alongside them; incomplete
columns are not silently treated as zero latency. Columns already loaded or
presented when they enter the view are counted separately; delivery percentiles
exclude the preloaded group. The 100 ms polling interval adds up to one sample
of uncertainty. These timings separate stream delivery from client meshing
without relying on a global pending-work count. Unique and repeat section
upload counts show whether the renderer is receiving replacement meshes during
the same join, not just first-time geometry. `mesh_work` counts source-column
admissions, full-column snapshots, neighbor-heal admissions, and light-patch
invalidations separately, so repeated uploads can be assigned to the path that
submitted them.
The movement timeline also samples each new column's 3×3 residency halo before
draining mesh results, then records its first returned section mesh and full
presentation. This distinguishes a column waiting for its outer dependency
ring from one queued behind worker or upload work. The halo sample is frame-
resolution; loaded and presented state still use the bounded 100 ms poll.
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
fails the bounded run. When movement is also enabled, it begins after the edit
has been presented.

The browser SDK reports the same player-facing boundaries through `onProgress`.
Its clock starts in the production create-world action and emits transition-only
events for `joining`, `loading-terrain`, `loading-overlay-ready`,
`first-terrain-presented`, and `full-view-presented`. Each event includes
`elapsedMs`, `loadedColumns`, `expectedColumns`, and `pendingMeshes`. The final
event also includes `settledColumns`, meaning columns with an explicitly empty
result or a section mesh handed to the renderer. It checks the exact configured
view after presentation, without waiting for unrelated background remesh jobs
to reach zero. Integrated joins immediately advertise the requested
render distance plus the mesher's one-column dependency halo; this prevents the
configuration-phase default from shrinking the stream and making the visible
outer ring impossible to mesh.

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

## Configuration

Run `just samply-client-join-mesh` to build the release fixture and capture it.
The default radius is the normal client render distance; pass `--radius 1` for
the small control workload or `--dry-run` to print both commands without
building. `LODESTONE_CLIENT_JOIN_TARGET_SIZE=1280x720` selects a larger render
target; `LODESTONE_CLIENT_JOIN_RADIUS=8` selects the view radius when invoking
the test directly. `LODESTONE_CLIENT_JOIN_MOVE_SECONDS=20` adds a bounded
movement phase; unset or zero leaves the stationary join control unchanged.
`LODESTONE_CLIENT_JOIN_EDIT=1` adds the block-edit phase.
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
`.cache/mc/26.2` bundle when present. The simulation uses the live window-mode
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
