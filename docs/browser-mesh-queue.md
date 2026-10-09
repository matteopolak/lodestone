# Browser mesh queue

## What it is

The browser terrain mesher keeps one pending capture intent per section in an indexed FIFO. Repeated updates coalesce before blocks and light are captured, and a coalesced section keeps its place in line.

## How it works

- Browser column, block and light invalidations enqueue `SectionIntent`s. `TerrainMesh::drain_meshes_with_world` captures the neighbourhood only when an intent reaches the front; the shared snapshot policy admits complete neighbourhoods, or incomplete ones with prior geometry or a forced rebuild. Native keeps immediate capture and generation-based completion filtering; explicit `MeshScheduler::submit` consumers share the same queue with owned snapshots.
- `SectionQueue` owns the payloads. A `HashMap<SectionKey, usize>` locates slots, slot indices link the FIFO, and a free list recycles vacated slots (a reused slot appends at the tail regardless of position). Replace, cancel and pop are constant expected work, with no scans, snapshot clones or tombstones. Queued intents hold no block or light handles. Coalescing preserves a pending forced allowance.
- An explicit empty outcome, or a deferred one without uploaded geometry, cancels pending work via `MeshScheduler::forget_generation`. Column unloads and re-decodes cancel matching keys from the index; teardown clears the queue and the completed `Vec` without meshing. Submission and cancellation also invalidate matching completed results.
- Each drain takes the completed list whole, then captures and meshes pending requests until the budget expires. The renderer handles removals before and after the drain (late capture can find an empty section with geometry still resident). Only successful or unchanged uploads call `mark_mesh_uploaded`; failed handoffs re-request through the normal path.

### Telemetry

`TerrainMesh::backlog` exposes `MeshBacklog::browser_queue` (`Some(BrowserMeshQueueStats)` on browser, `None` on native). `Sim::mesh_backlog` feeds `WasmMeshProfile::record`; with debug logging, the once-per-second profile adds a `wasm mesh queue` line, forwarded through the render-worker diagnostic message (not SDK `onProgress`).

- Insertion, replacement, cancellation and pop counts are cumulative per scheduler. Cancellations count pending snapshots removed by key, unload or teardown; absent-key cancels and empty pops count nothing; pops count the start of processing, not uploads.
- `queued_keys` is the current count; `high_water_keys` (logged `high_water_keys_lifetime`) updates on insertion so it keeps a peak reached before the frame's drain.
- `oldest_wait` is the front key's age since first submission (replacement preserves it, cancel-then-submit restarts it). `max_pop_wait` (`max_pop_wait_lifetime_ms`) is the largest age at the start of processing, excluding meshing and upload. Lifetime maxima and totals reset only with a new scheduler.
- Sampling reads only the front slot and counters, with no backlog walk or extra allocation; a new distinct key reads the clock once, and the drain reuses its deadline timestamps. These measure queue work and wait, not bytes or visible latency; compare with the worker drain/upload profile and presentation witnesses.

## How to change it

- Edit the FIFO and invalidation rules in `crates/lodestone-shell/src/mesher/browser_queue.rs`. `TerrainMesh::accept_snapshot` decides deferred-snapshot eligibility; capture and meshing stay in the world-aware drains. Cancelling before an eligible replacement loses its FIFO position.
- Tests: `mesher::browser_queue::tests` (bursts, head/middle/tail cancellation, slot reuse, completed invalidation, unload, teardown, counters, wait ages) plus `empty_or_unuploaded_deferred_outcomes_invalidate_older_meshes`. Compile the browser target after touching the platform seam.
- There is no admission or byte cap. A cap needs a reservation and retry path that keeps rejected sections eligible; silently dropping a dirty section leaves permanent holes.

## Configuration

`BROWSER_MESH_BUDGET` bounds a drain to about 4 ms, with at least one request per call (it cannot preempt one section). Leaf and biome-blend settings are read at mesh time. No environment variables.

## Dependencies

Standard-library collections only. Uses `SectionKey`, copy-on-write `SectionSnapshot`, `Meshed`, `TerrainMesh` and `crate::platform::Instant`.
