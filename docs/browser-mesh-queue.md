# Browser mesh queue

## What it is

The browser terrain mesher retains one pending capture intent per section in an
indexed FIFO. Repeated updates coalesce before capturing blocks and light, without
moving the section behind newer submissions.

## How it works

Browser column, block and light invalidations enqueue `SectionIntent` values.
`TerrainMesh::drain_meshes_with_world` captures the current neighbourhood only
when an intent reaches the front. The shared snapshot policy admits complete
neighbourhoods, or incomplete neighbourhoods with prior geometry or a forced
rebuild. Native retains immediate snapshot capture and generation-based worker
completion filtering. Explicit `MeshScheduler::submit` consumers may still supply
owned snapshots; these share the same indexed queue with capture intents.

`SectionQueue` owns the pending payloads directly. A `HashMap<SectionKey, usize>`
locates each occupied slot, and slot indices link the FIFO. Replacing a queued key
drops its previous request immediately and preserves its links. Cancelling or
popping a key unlinks its slot and returns that slot to a free list. A later new key
reuses a vacant slot and appends at the tail, independent of the slot's physical
position.

Key replacement, cancellation and pop perform constant expected index work;
insertion has amortized allocation cost. They do not scan queued snapshots, clone
snapshots or leave cancelled payloads behind as tombstones. The index contains only
queued keys. Slot storage and collection capacity retain their historical peak
size, while vacant slots contain no payloads. Production queued intents retain no
block or light snapshot handles. Coalescing preserves a pending forced allowance.

An explicit empty outcome or a deferred outcome without prior uploaded geometry
cancels pending work through `MeshScheduler::forget_generation`. Column unloads
enumerate matching keys from the index and cancel them, including sections that
never reached the renderer. Completed meshes remain in a separate small `Vec`;
submission and cancellation invalidate matching completed results there too. The
browser drain takes that completed list in full and then captures and meshes
pending requests until its time budget expires. Re-decoding a column also cancels
its prior requests before halo admission. Session teardown clears both collections
without capturing or meshing the backlog.

The renderer receives results through the world-aware terrain drain. It processes
removals both before and after that drain: late capture can discover an empty
section while prior geometry remains resident. Only successful or unchanged GPU
uploads receive `mark_mesh_uploaded` acknowledgement. Failed handoffs invalidate
the section through the normal request path, using actual renderer residency to
retain existing boundary geometry. Queue order
therefore determines which pending sections become eligible to appear first.
Coalescing changes scheduling work; a browser measurement is required to establish
its effect on frame time or visible loading latency.

### Queue telemetry

`TerrainMesh::backlog` exposes `MeshBacklog::browser_queue` as
`Some(BrowserMeshQueueStats)` on the browser and `None` on native. The production
consumer is `Sim::mesh_backlog` feeding `WasmMeshProfile::record` during redraw.
With debug logging enabled, the existing once-per-second profile emits an
additional aggregate `wasm mesh queue` line; it emits no individual section
records. Both the drain/upload profile and queue aggregate travel through the
existing render-worker diagnostic message to the standalone page console.
The existing CPU/GPU frame-phase summary uses the same forwarding path. These
diagnostics are not SDK `onProgress` events.

Insertion, replacement, cancellation and pop counts are cumulative for the
scheduler's lifetime. An insertion adds a distinct pending key; replacement
updates its existing payload. Cancellation counts pending snapshots removed by
key, column unload or teardown, including every pending key cleared together.
Absent-key cancellation and empty pops do not increment counts. Pops count the
start of capture or explicit-snapshot processing, including empty and deferred
outcomes, rather than renderer uploads. Completed-result removals are
outside these pending-queue counts.

`queued_keys` reports current pending keys, while `high_water_keys` records their
maximum count since scheduler creation. The high-water count updates on insertion,
so it preserves a peak reached before this frame's mesh drain, even when the
post-drain sample is empty. The log labels it `high_water_keys_lifetime`.

`oldest_wait` is the front key's age since its first submission, or zero when
empty. Replacing its snapshot preserves this age; cancelling and later submitting
the same key starts a new wait. `max_pop_wait` is the largest such age observed
when a pending request began processing, and is logged as
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
`TerrainMesh::accept_snapshot` responsible for deciding whether a deferred snapshot
is eligible, and keep capture and meshing inside the world-aware terrain drains.
Cancelling before an
eligible replacement would lose its FIFO position.

The native tests in `mesher::browser_queue::tests` cover replacement bursts,
head/middle/tail cancellation, slot reuse, completed-result invalidation, column
unload and teardown, plus cumulative counters and wait ages from explicit clock
arithmetic. The shared route control
`empty_or_unuploaded_deferred_outcomes_invalidate_older_meshes` checks both
non-submitting outcomes. Run these controls together with the existing native
generation and unload tests, then compile the browser target after changing the
platform seam. The late-capture controls distinguish a retained one-cube snapshot
(six quads) from three current separated cubes (18 quads), and cover forced/prior
geometry admission plus unload/re-arrival cancellation.

There is no admission or byte cap: distinct pending sections can still consume a
large metadata backlog. Adding a cap requires a reservation and retry path that
keeps rejected sections eligible for later submission; silently dropping a dirty
section can leave permanent missing terrain.

## Configuration

`BROWSER_MESH_BUDGET` bounds synchronous capture and meshing to a nominal four
milliseconds per drain, with at least one queued request processed per call. It
does not preempt the cost of one section. Leaf and biome-blend settings are read at mesh
time. The queue has no environment variables or independent ordering settings.

## Dependencies

The queue uses only standard-library collections and safe Rust. It relies on the
shell's `SectionKey`, copy-on-write `SectionSnapshot`, `Meshed`, and
`TerrainMesh` lifecycle, plus `crate::platform::Instant` for the browser drain's
deadline. It adds no external dependencies and does not change native worker
channels or upload budgets.
