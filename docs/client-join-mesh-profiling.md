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
`CLIENT_JOIN_MESH_PROFILE` record
for seed 4242 and the configured render distance. It distinguishes the visible
view from the server's requested additional meshing halo, and records the connection,
terrain-loading, overlay-ready, all-visible-columns, all-server-columns,
all-visible-meshes-settled, and first-presented-terrain boundaries. Queue
high-water marks, uploaded geometry, the synchronous open-call cost, and
bounded phase totals identify work between those boundaries.

The September 15 default-distance control measured 2.02 seconds from the
world-open request to the first presented terrain and 30.96 seconds under the
original approximate completion gate. Delivered-column filtering for
tick-driven lighting reduced that approximate result to 28.69 seconds without
moving the first-presented boundary. After restoring the server's required
one-column meshing halo and replacing the high-water gate with exact coordinate
settlement, the same seed reached first terrain in 2.180 seconds, all 289 visible
columns in 27.819 seconds, all 361 requested server columns in 32.872 seconds,
and a submitted frame with every visible column meshed in 33.404 seconds. The
earlier totals are useful optimization controls but are not directly comparable
to the stricter final boundary. The matching Samply captures are
`client-join-mesh-default-view-before.json.gz` and
`client-join-mesh-default-view-delivered-light-after.json.gz` under
`bench-results/profiles/`; the remaining native tail is dominated by dynamic
cross-column lighting and world generation rather than client meshing.
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

The browser SDK reports the same player-facing boundaries through `onProgress`.
Its clock starts in the production create-world action and emits transition-only
events for `joining`, `loading-terrain`, `loading-overlay-ready`,
`first-terrain-presented`, and `full-view-presented`. Each event includes
`elapsedMs`, `loadedColumns`, `expectedColumns`, and `pendingMeshes`. The final
event also includes `settledColumns` and is emitted only after every exact
configured-view coordinate has arrived, meshing has
settled, uploads have been accepted by the renderer, and that frame has been
submitted for presentation. Integrated joins immediately advertise the requested
render distance plus the mesher's one-column dependency halo; this prevents the
configuration-phase default from shrinking the stream and making the visible
outer ring impossible to mesh.

## How to change it

Keep the workload finite and aggregate-only. Add measurements at existing
boundaries rather than logging packets or sections individually. Run the test
once as a control before attributing a hotspot to client work; a missing vanilla
atlas or GPU adapter is an environmental failure, not a valid zero result.
Browser consumers should retain the transition events as one join record rather
than sampling console output or treating the SDK's mount-level `first-frame`
event as terrain readiness.

## Configuration

Run `just samply-client-join-mesh` to build the release fixture and capture it.
The default radius is the normal client render distance; pass `--radius 1` for
the small control workload or `--dry-run` to print both commands without
building. The script discovers
the test executable reported by Cargo and wraps that exact executable with
`samply record --save-only`. If `LODESTONE_ASSETS` is unset, it uses the local
`.cache/mc/26.2` bundle when present. The simulation uses the live window-mode
resource path with a headless render target; headless simulation mode would
deliberately substitute the offline demo world and is rejected by the atlas
control.

## Dependencies

The fixture depends on the integrated server, the shell's client and mesh
pipeline, a headless wgpu adapter, and the vanilla asset bundle under the
repository's normal asset configuration.
