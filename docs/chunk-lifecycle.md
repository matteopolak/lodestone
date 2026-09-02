# Chunk lifecycle: residency, generation, and streaming

## What it is

Everything that decides which chunk columns exist in memory on the integrated server, how
expensive it is to make one exist, and how a connected client's own view of the world stays in
sync as it walks around — plus the client-side mirror of the same "one authoritative copy" idea
for the terrain a session renders and collides against.

## How it works

### The chunk store: a bounded cache in front of generation

`ChunkStore` wraps any `ChunkSource` and is itself one: a bounded, least-recently-used cache of
generated columns, added because a real terrain generator (carvers, ores, vegetation) costs on the
order of 900ms per column — roughly 18 tick budgets — and without a cache a column was
regenerated from scratch on every single read, including a probe that only wanted one block.
Three properties are load-bearing: generation happens with the store's lock released, so a
900ms generation never serializes concurrent generation elsewhere; a slower writer's insert never
overwrites a faster one that already landed; and eviction is lossless, because a `set_block` edit
is written through to the wrapped source *first*; dropping a cache entry only ever costs a future
regeneration, never a lost edit.

Capacity is a question of whose memory is being spent: singleplayer's cache is effectively
uncapped (the player's own render-distance choice, and the cost of capping it is regenerating the
ground under their own feet), while a hosted (LAN/dedicated) world caps it, since that memory is
an operator's, spent on behalf of players who did not choose the setting. Capacity derives from the
streamed view size plus a fixed reserve for concurrent scans, and it only ever grows to follow a
live render-distance increase mid-session — never shrinks back down — because shrinking would evict
exactly the columns nearest the player (the innermost, least-recently-touched ring), turning a
slider nudge into a visible regeneration stall.

### Chunk tickets: residency independent of any one connection's view

A ticket/level graph, ported from vanilla in shape, answers a question the LRU cache structurally
cannot: *why should this chunk exist at all, and how urgently* — independent of recency. A ticket
carries a level rather than a radius; the minimum level reachable from any active ticket, computed
by distance, decides whether a column is resident, loaded-but-not-ticking, or absent. Two
independent trackers exist (a loading level and a separate simulation level) so a loading-only
ticket can keep a chunk resident without making it tick. `ChunkStore` is the one production
consumer: it grants a spawn-area ticket and one loading+simulation ticket pair per connected
player (so a shared column near two players stays resident until *both* have moved away), checks
in with the ticket graph on its own read traffic, and evicts through the same persistence-aware
unload path its ordinary LRU eviction already uses — so a ticket-driven eviction is exactly as safe
as a capacity-driven one.

### The ticked/simulated area follows the player

The set of columns the world tick loop actually simulates (random ticks, scheduled block/fluid
ticks, natural mob spawning) is centred on the players rather than nailed to world spawn. Two
different things move at two different cadences: the coordinate list itself is cheap and rebuilt
every tick (integer arithmetic over a few dozen pairs), while the terrain view natural-spawning
reads from is only rebuilt when that list actually changes — keeping a whole area's worth of column
fetches out of any single unserviced window. A fallback square (the old fixed-origin behavior)
still applies when no player has moved yet, which matters for the real window between a join and a
player's first movement packet, as well as for tests that drive the tick loop with no players
connected at all.

### Generation is parallelized, but parallel is not the same as non-blocking

Because the terrain generator is pure per chunk and every RNG it touches is positionally seeded, a
batch of columns can be generated across scoped worker threads with no shared mutable state and no
run-to-run nondeterminism — which thread generates a given column, or when, cannot change what it
contains. A caller must still fix its coordinate order *before* fanning out and encode/send in that
same fixed order afterward, never in whatever order results happen to complete or a hash set
happens to iterate.

Fanning generation out across threads only addresses throughput. On a single-threaded tokio
runtime (what production actually builds, so that the server tick and every connection share one
core with no cross-thread synchronization), blocking that one thread for the whole batch would
stall every other task in the process. Moving the batch onto the blocking thread pool is what
fixes the *latency* side; a plausible-looking shortcut that skips the pool by calling in place
instead panics outright on that runtime, since it explicitly requires a multi-threaded one — this
matters because it is exactly the runtime shape production uses.

### Encoding is offloaded too, and it must ride the same worker that generated the column

Protocol-level encoding of a generated column is moved off the connection task and into the same
blocking worker that generated it, so the task that owes a player a reply to something they just
did never spends a large fraction of a millisecond's worth of instructions encoding terrain first.
Early on, the encode step was itself accidentally left serial — generation was fanned out, joined,
and only then walked one column at a time on a single thread to encode, which relocated the cost
rather than removing it. The fix applies the encode inside the very worker that generated each
column. Wire order is unaffected either way, since emission order is fixed at the moment a batch is
*enqueued*, not by completion order.

### View streaming, and the latency defect that caused false keep-alive timeouts

**The number of chunk columns processed inside one unserviced async arm is what determines whether
a connection's keep-alives survive a chunk-boundary crossing — moving that work to a blocking
thread pool does not fix this on its own, because offloading shortens neither the suspension point
nor how much of it sits inside one `await`.** A connection's read/write loop is a single-armed
select: whichever arm is running owns the whole connection for that pass, so awaiting a whole
newly-visible strip of columns (dozens on an ordinary step, a full square on a teleport) before
returning kept the socket unserviced for the entire strip — no packet from the player was read, no
keep-alive challenge could be sent, and no keep-alive reply could be read, so a client that had in
fact answered promptly still had its unanswered-looking challenge time out. The fix streams a move
exactly like a join: coordinates are computed synchronously and cheaply, then handed to the same
incrementally-draining pipeline the join burst already uses, so the connection loop only ever pays
for one column's worth of work per pass rather than a whole batch. A stall watchdog measures the
duration of each select-arm body specifically (not the interval between passes, which is mostly
idle waiting and looks identical to a real stall under a paused clock) and only forgives a
keep-alive once the accounting shows the client was genuinely unreachable for a full keep-alive
interval, rather than merely quiet because the loop itself was busy.

### The client's own single source of truth for terrain

The client side has the identical shape of problem in miniature: for a while there were genuinely
two independent in-memory chunk stores in the client process (one for a live network session, one
for an offline/demo world), only one of which was ever populated for a given session, with every
read site carrying a three-term branch to guess which. Consolidating to one ECS resource
(`ChunkWorld`) removed the branch and the divergence it had accumulated (differing light rules at
world-height boundaries, differing drop accounting, differing height-limit sources between the two
paths). A read-only handle and a separate write handle name the same underlying store, so a system
that only needs to read terrain (most of the render and collision path) cannot accidentally mutate
it — writing goes through the write handle alone, held only by the store's legitimate writers
(prediction, the net-ingest path, test harnesses). A couple of facts the store itself cannot answer
(whether the connected dimension has skylight by default; whether the renderer's block-id space
currently agrees with the store's) are tracked alongside it and recomputed whenever a session
attaches or a dimension changes, since both can change independently of the terrain itself.

### Measuring the client chunk pipeline's real cost

A dedicated, instruction-denominated benchmark (using hardware performance counters rather than
wall-clock time, which is far too noisy on a shared or thermally-throttled machine to attribute a
regression correctly) walks the whole client chunk path stage by stage — decode, insert into the
world, snapshot neighboring sections, mesh, submit to the renderer — over real generated terrain.
The consistent finding: meshing dominates the cost of bringing a chunk on screen, and fluid meshing
in particular is disproportionately expensive relative to how much of a typical column is actually
fluid, because each fluid cell resolves many small neighbor queries redundantly. Optimizing that
path has repeatedly paid off in instructions retired specifically (not merely in cache locality),
which is itself a useful diagnostic: an optimization that only helps locality will show up in
cycles-per-instruction, not in the raw instruction count.

## How to change it

- **Do not add a "mutate one column in place" API to the chunk store.** It looks like it would
  avoid a clone, but the tick loop already both mutates a column directly and separately calls
  back into the store for the same column in the same breath, so a closure-based API that holds
  the store's lock across the caller's mutation self-deadlocks.
- **Do not widen the ticket graph's `ChunkSource` trait surface casually.** A dimensional wrapper
  around every real production world source has no catch-all default-forwarding, so a new trait
  method reaching only some implementors silently behaves as "always resident" through the
  wrapper — the exact bug already caught once here. Ticket mutation is reached through the concrete
  store type instead of generic trait dispatch for this reason.
- **Do not raise the tick-follow radius, the parallel generation window, or a streaming batch size
  without re-checking what sizes it against.** Each of those numbers is derived from a real
  ceiling (available worker parallelism, the LRU's own reserve, a client's ack-rate estimate); a
  bigger number is not free just because it compiles.
- **A lock held across a call that can re-enter the same lock is a latent self-deadlock.** The
  scheduled-tick and ticket-graph code paths both had to be checked for this shape specifically
  (loading a saved chunk's pending ticks can call back into the very structure that triggered the
  load) — grep what a guarded section calls, transitively, before widening any critical section
  here.
- **Any new `select!` arm added to the connection loop must be timed by the stall watchdog** (enter
  at the start of the arm body, mark its pass at the end) or it becomes invisible to keep-alive
  accounting — a missing entry silently attributes no stall time to a genuinely slow arm, and a
  missing exit leaves the timer open for whichever arm runs next.

## Configuration

- Chunk-store capacity is derived from the streamed view radius plus a fixed concurrent-scan
  reserve, floored at a default and (for hosted worlds only) capped at a maximum.
- The tick-follow radius is a small fixed constant, independently sized for singleplayer versus
  LAN hosting.
- Ticket levels and timeouts are transcriptions of vanilla's own constants, not independently
  tunable.
- The parallel-generation worker count is the process's available parallelism; there is no manual
  override.
- Streaming batch size and the keep-alive stall thresholds are small constants in the server crate.

## Dependencies

- Standard library only for the store and ticket graph (no new external dependency).
- `tokio`'s blocking thread pool for offloaded generation and encoding; a current-thread runtime
  native build for the production shape this all has to work correctly under.
- The version-free `ServerProtocol`/`ChunkEncoder` seam for encoding, so any protocol family
  without an implementation simply keeps the pre-existing behavior of encoding on the connection
  task.
- The client-side resource lives in the shared ECS crate and is read by the mesher and the
  collision/render paths; it depends on the world-storage crate but names no protocol version.
