# Client Join and Mesh Profiling

## What it is

The ignored `client_join_mesh_profile` fixture measures a deterministic singleplayer join (seed 4242): world-open request, connection phases, initial-view delivery, CPU meshing, GPU upload, loading-screen readiness and the first presented terrain frame, emitted as one aggregate `CLIENT_JOIN_MESH_PROFILE` record.

## How it works

The fixture starts its clock just before the singleplayer open call the interactive client uses and runs the real `NetClient`, `Sim`, terrain scheduler and `RenderState` against a `HeadlessTarget` (default 64x64), excluding compositor and menu/HUD work. Mesh handoff feeds the same adaptive frame budget as interactive redraw.

- The new-world server pauses initial ticks until the fixture sends the post-present player-loaded acknowledgement, which the report includes.
- The report separates the capped initial loading square (radius 6 for a new world), the selected render distance and the server's two padding rings (the first supplies the mesher's 3x3 neighbourhood, the second keeps it ready across chunk crossings). `Sim::view_settlement_at_radius` queries the selected distance without changing the loading gate; keep the two predicates separate.
- Frame, step, upload and render p99/max expose hitches that aggregates hide. Phase timers (`generation_phase_work`, admission queue/worker/return) are completed-operation intervals; nested and parallel timers overlap, so never sum them. `chunk_ingress` counts first loads and full replacements at the client-world boundary (unchanged GPU uploads do not prove identical inputs). `server_tick.schedule_at_end` gives wake-delay p95, wake/deadline maxima, catch-up admissions and shed ticks; `movement_tick_delta`/`movement_overrun_delta` separate a server stall from a client hitch.

**Movement phase** (`LODESTONE_CLIENT_JOIN_MOVE_SECONDS`): the fixture moves after the overlay clears (forward, sprint, jump) and fails if the player never crosses a chunk boundary. Flight mode requests creative, ascends to Y=200 then flies (clock starts after ascent; the report records heights, corrections and first loss of flight). `view_at_stop` separates missing from resident-but-unsettled columns; `Sim::mesh_backlog` attributes a slow tail to delivery, admission, worker or upload. `new_view_columns` follows columns exposed per crossing (entry, client-world appearance, full presentation); latency percentiles cover only columns still visible at stop, and `all_entered` keeps every entry episode, so check `all_entered.preloaded_on_entry` before calling a run a cold-stream test. The fixture radius ignores the saved render-distance option (radius 8 says nothing about 32). `mesh_work` separates column admissions, snapshots, neighbour-heal admissions and light-patch invalidations; `mesh_work.native_scheduler` counts submitted, started, skipped and stale-discarded jobs; `gpu_uploads_applied/unchanged/failed` give the renderer's per-mesh result.

**Edit and drop phases.** `..._EDIT=1` aims down and attacks through normal input, recording click to local air, replacement upload and next presented frame (survival includes mining time); a missing ray target or unpresented edit fails. `..._CREATIVE_EDIT=1` gives instant-break latency. `..._DROP=1` then mines the block beneath and records delay to air, item entity, stack and an item-geometry draw (12 s timeout).

**Native benchmark.** `lodestone --benchmark singleplayer` measures a real textured window Surface including HUD: a Survival world (seed 4242) through menus, then warm-up, standing, walking and mining. `--benchmark-walk-mine` holds mining while walking (phase `singleplayer.walking_mining`), `--benchmark-mutation 0` omits the final stationary mining phase, `--sensitivity 0.5` sets the one-time downward aim. Holding attack proves nothing: record actual targets and completed edits ([presentation capture](presentation-capture.md)).

**Reference points (local, not thresholds).** Radius 6 at 1280x720: 169 visible columns resident at 5.66 s but settled at 7.72 s, the gap explained by the 56-column halo (last delivery 7.31 s), not the mesh admission budget. Walking each ring as a contiguous perimeter moved all-resident time only 5.70 to 5.58 s while peak RSS rose 493 to 546 MB and existing-column hits fell 176 to 155 (preserve neighbour reuse when changing admission order). Radius 8, 45 s movement: the second padding ring raised columns already presented on entry from 0/221 to 204/221 and median entry-to-presentation fell 451 ms to 0, at +37 MB RSS and a 0.25 s later overlay.

**Hardware counters.** `just profile-join-hardware client --radius 1` runs the release test under macOS Instruments, writing counters, process identity and phase markers to one summary (counters are process-scoped, phases stay wall-time markers). Legacy `counters-profile` tables need `Cycles Instructions` in that order for instructions and IPC. Instruments 27 Guided CPU Bottlenecks exports named `cycle` metrics: `profile-join-hardware.py::guided_counter_lines` sums only precise buckets for the exact target PID (coarse buckets overlap and are checked, never added); instructions and IPC are unavailable and `Useful` is a bandwidth fraction. Parser tests use `scripts/fixtures/xctrace-guided-cycles.xml`.

**Browser.** The SDK `onProgress` reports `elapsedMs` and column/queue counts for `joining`, `loading-terrain`, `loading-overlay-ready`, `first-terrain-presented`, `gameplay-ready`, `full-view-presented`, `full-view-quiescent`. `presentedColumns` (geometry or explicit empty) and `settledColumns` (latest renderer handoff) are independent. `full-view-quiescent` needs latest coverage plus `pendingMeshes`, `pendingColumns`, `pendingLightRemeshes` and `pendingRemovals` drained (halo-only waiters do not block it). Integrated joins advertise render distance plus the two padding columns immediately. `?log=debug` emits once-per-second `wasm mesh drain and upload profile`, `wasm mesh queue`, `wasm mesh passes`, `wasm mesh light sources` and `wasm mesh light reads` lines (upload time is CPU wall time around the handoff, not GPU completion; nothing with debug off). `TerrainMesh::mesh_measurement` keeps fixed-size per-cause totals and `TerrainMesh::record_mesh_handoff` must be called once per returned result; light-read timings are diagnostic only. The responsiveness probe (`web/responsiveness_probe.test.mjs`) derives `meshCounterIntervals` by subtraction (missing baselines or resets give `deltas: null`); its `Walk + mine 20s` control needs `blockActionSummary` and new columns verified before being called an edit-plus-frontier stress test. The renderer runs in a worker, so page Long Tasks miss its stalls: compare frame gaps against an idle control. Use `http://127.0.0.1:8080` locally; if the port is taken identify its owner rather than killing it.

## How to change it

- Keep the workload finite and aggregate-only; add measurements at existing boundaries, never per-packet or per-section logs. Run once as a control before attributing a hotspot to client work (a missing atlas or GPU adapter is an environment failure, not zero).
- `NetClient` publishes a read-only `IntegratedTickMonitor` for local servers only: keep tick sampling out of the packet queue and never read the tick-owned ECS world.
- Keep `BrowserJoinTrace` samples and the SDK's `emit_join_progress` mapping aligned. A light-read measurement change updates `mesher::light_reads`, `mesher::measurement`, the diagnostic in `app::redraw` and the probe parser together. `web/build.rs` declares the probe JavaScript as a Cargo input; verify the staged wasm-bindgen snippet after editing probe controls.
- Before an input-equality optimisation measure the unchanged fraction per cause and bound a witness over every snapshot tag, light input and option.

## Configuration

`just samply-client-join-mesh` builds the release fixture and captures with `samply record --save-only` (`--radius 1` control, `--dry-run` to print commands).

| Variable | Effect |
|---|---|
| `LODESTONE_CLIENT_JOIN_RADIUS` | view radius when running the test directly |
| `LODESTONE_CLIENT_JOIN_TARGET_SIZE` | e.g. `1280x720` render target |
| `LODESTONE_CLIENT_JOIN_MOVE_SECONDS` | adds a movement phase; unset/0 is the stationary control |
| `LODESTONE_CLIENT_JOIN_MOVE_MODE` | `walk` (default) or `flight` (needs game-mode permission) |
| `LODESTONE_CLIENT_JOIN_EDIT` | adds the block-edit phase |
| `LODESTONE_CLIENT_JOIN_CREATIVE_EDIT` | instant creative edit; requires EDIT, excludes DROP |
| `LODESTONE_CLIENT_JOIN_DROP` | adds the item-drop phase; requires EDIT |
| `LODESTONE_JOIN_TRACE` | per-column server stage trace; diagnostic only, perturbs timing |
| `LODESTONE_ASSETS` | asset bundle; defaults to local `.cache/mc/<version>` |

The simulation uses the live window-mode resource path with a headless target (headless simulation mode would substitute the offline demo world). Settlement is checked every frame until the acknowledgement, then every 100 ms.

## Dependencies

The integrated server, the shell's client and mesh pipeline, a headless wgpu adapter and the vanilla asset bundle.
