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

## How to change it

Change the indexed FIFO and browser invalidation rules in
`crates/lodestone-shell/src/mesher/browser_queue.rs`. Keep
`TerrainMesh::route` responsible for deciding whether a deferred snapshot is
eligible, and keep meshing inside the scheduler drains. Cancelling before an
eligible replacement would lose its FIFO position.

The native tests in `mesher::browser_queue::tests` cover replacement bursts,
head/middle/tail cancellation, slot reuse, completed-result invalidation, column
unload and teardown. The shared route control
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
