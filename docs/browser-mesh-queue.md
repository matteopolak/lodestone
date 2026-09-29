# Browser mesh queue

## What it is

The browser terrain mesher retains one pending snapshot per section in an indexed
FIFO. Repeated updates replace that snapshot without moving the section behind
newer submissions, so a busy section keeps its turn in the mesh backlog.

## How it works

`TerrainMesh::route` submits ready snapshots and deferred snapshots allowed by
existing uploaded geometry or a forced rebuild through
`MeshScheduler::submit_current`. On the browser this delegates to
`BrowserMeshBacklog::submit`; on native it retains the existing
invalidate-then-submit sequence and generation-based completion filtering.

`SectionQueue` owns the pending payloads directly. A `HashMap<SectionKey, usize>`
locates each occupied slot, and slot indices link the FIFO. Replacing a queued key
drops its previous snapshot immediately and preserves its links. Cancelling or
popping a key unlinks its slot and returns that slot to a free list. A later new key
reuses a vacant slot and appends at the tail, independent of the slot's physical
position.

Key replacement, cancellation and pop perform constant expected index work;
insertion has amortized allocation cost. They do not scan queued snapshots, clone
snapshots or leave cancelled payloads behind as tombstones. The index contains only
queued keys. Slot storage and collection capacity retain their historical peak
size, while vacant slots contain no snapshot handles.

An explicit empty outcome or a deferred outcome without prior uploaded geometry
cancels pending work through `MeshScheduler::forget_generation`. Column unloads
enumerate matching keys from the index and cancel them, including sections that
never reached the renderer. Completed meshes remain in a separate small `Vec`;
submission and cancellation invalidate matching completed results there too. The
browser drain takes that completed list in full and then pops pending snapshots
until its time budget expires. Session teardown clears both collections without
meshing the backlog.

The renderer still receives results through `TerrainMesh::drain_meshes`, followed
by the app's normal upload and `mark_mesh_uploaded` acknowledgement. Queue order
therefore determines which pending sections become eligible to appear first.
Coalescing changes scheduling work; a browser measurement is required to establish
its effect on frame time or visible loading latency.

### Queue telemetry

`TerrainMesh::backlog` exposes `MeshBacklog::browser_queue` as
`Some(BrowserMeshQueueStats)` on the browser and `None` on native. The production
consumer is `Sim::mesh_backlog` feeding `WasmMeshProfile::record` during redraw.
With debug logging enabled, the existing once-per-second profile emits an
additional aggregate `wasm mesh queue` line; it emits no individual section
records.

Insertion, replacement, cancellation and pop counts are cumulative for the
scheduler's lifetime. An insertion adds a distinct pending key; replacement
updates its existing payload. Cancellation counts pending snapshots removed by
key, column unload or teardown, including every pending key cleared together.
Absent-key cancellation and empty pops do not increment counts. Pops count the
start of CPU meshing, rather than renderer uploads. Completed-result removals are
outside these pending-queue counts.

`queued_keys` reports current pending keys, while `high_water_keys` records their
maximum count since scheduler creation. The high-water count updates on insertion,
so it preserves a peak reached before this frame's mesh drain, even when the
post-drain sample is empty. The log labels it `high_water_keys_lifetime`.

`oldest_wait` is the front key's age since its first submission, or zero when
empty. Replacing its snapshot preserves this age; cancelling and later submitting
the same key starts a new wait. `max_pop_wait` is the largest such age observed
when a pending snapshot began meshing, and is logged as
`max_pop_wait_lifetime_ms`. It excludes mesh computation and renderer upload time;
the existing drain/upload measurements cover those phases. Both lifetime maxima
and operation totals survive clearing the queue and reset only with a new
scheduler.

Sampling reads only the front slot and counters. A new distinct key reads the
portable clock once; replacement and cancellation read no clock. An empty stats
sample also reads no clock, and the budgeted mesh drain reuses its existing
deadline-check timestamps for popped wait ages. No backlog walk, snapshot clone or
additional allocation is needed for telemetry. These fields measure queue work
and wait, not heap bytes or visible input latency.

The standalone browser renders inside its render worker, whose loop waits sixteen
milliseconds after redraw. Its frame-start gap therefore includes that pacing and
render work; page-main-thread long-task records do not measure render-worker mesh
stalls. Compare the queue ages with the existing worker drain/upload profile and
renderer presentation witnesses rather than treating a queued result as visible.

## How to change it

Change the indexed FIFO and browser invalidation rules in
`crates/lodestone-shell/src/mesher/browser_queue.rs`. Keep
`TerrainMesh::route` responsible for deciding whether a deferred snapshot is
eligible, and keep meshing inside the scheduler drains. Cancelling before an
eligible replacement would lose its FIFO position.

The native tests in `mesher::browser_queue::tests` cover replacement bursts,
head/middle/tail cancellation, slot reuse, completed-result invalidation, column
unload and teardown, plus cumulative counters and wait ages from explicit clock
arithmetic. The shared route control
`empty_or_unuploaded_deferred_outcomes_invalidate_older_meshes` checks both
non-submitting outcomes. Run these controls together with the existing native
generation and unload tests, then compile the browser target after changing the
platform seam.

There is no admission or byte cap: distinct pending sections can still consume a
large amount of memory. Adding a cap requires a reservation and retry path that
keeps rejected sections eligible for later submission; silently dropping a dirty
section can leave permanent missing terrain.

## Configuration

`BROWSER_MESH_BUDGET` bounds synchronous meshing to a nominal four milliseconds per
drain, with at least one queued section meshed per call. It does not bound snapshot
admission or the cost of one section. Leaf and biome-blend settings are read at mesh
time. The queue has no environment variables or independent ordering settings.

## Dependencies

The queue uses only standard-library collections and safe Rust. It relies on the
shell's `SectionKey`, copy-on-write `SectionSnapshot`, `Meshed`, and
`TerrainMesh` lifecycle, plus `crate::platform::Instant` for the browser drain's
deadline. It adds no external dependencies and does not change native worker
channels or upload budgets.
