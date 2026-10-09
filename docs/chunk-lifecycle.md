# Chunk lifecycle: residency, generation, and streaming

## What it is

Everything that decides which chunk columns exist in memory on the integrated server, what it costs to make one exist, and how a connected client's view stays in sync as it moves, plus the client-side single store for the terrain a session renders and collides against. The residency design constants are in [`plans/chunk-lifecycle.md`](./plans/chunk-lifecycle.md).

## How it works

### The chunk store

`ChunkStore` wraps any `ChunkSource` and is one: a bounded LRU cache of generated columns, added because a real generator costs about 900 ms per column (roughly 18 tick budgets) and every read, even a one-block probe, regenerated it. Three properties are load-bearing: generation runs with the store's lock released; a slower writer's insert never overwrites a faster one; and eviction is lossless, because `set_block` writes through to the wrapped source first.

Capacity derives from the streamed view size plus a fixed reserve for concurrent scans, and only grows with a live render-distance increase, never shrinks (shrinking would evict the innermost, least-recently-touched ring and cause a visible stall). Singleplayer follows the streamed view; both singleplayer and hosted worlds use a measured ceiling for the 256-chunk option, because the full 265,225-column square would cost several GB before meshes and wire buffers, so the join scheduler streams it incrementally while the cache saturates.

Tools needing the hosted retention boundary build it with `lodestone_server::retained_chunk_source_for_view_radius`, which takes any `ChunkSource`, returns another and keeps `ChunkStore` private; pass the same view radius as the consumer. The integrated server uses its own bounded policy through `IntegratedServer`.

### Tickets: residency independent of any view

A ticket/level graph answers what the LRU cannot: why a chunk should exist and how urgently. A ticket carries a level; the minimum level reachable from any ticket (by distance) decides resident, loaded-not-ticking or absent. Separate loading and simulation trackers let a loading-only ticket keep a chunk resident without ticking.

- `ChunkStore` is the one production consumer: a spawn-area ticket plus one loading+simulation pair per connected player (a shared column stays resident until both players leave). It checks in on its own read traffic and evicts through the same persistence-aware unload path as LRU eviction. Grants, moves and removals mark the graph dirty so the next cache operation reconciles immediately; unchanged traffic keeps the rate-limited check-in.
- Block-entity reads search the resident column, then fall back to the wrapped source when it has no sidecar, so generated containers stay discoverable after terrain was cached first.
- `ChunkSource::ticket_store` exposes the handle without reading terrain; `ChunkStore`, `DimensionalSource` and the `Arc`/`Box` wrappers forward it. A connection grants its pair in the resolved join dimension (including a restored native locator) before prestreaming or registering the player. Pair mutations hold one ticket-store lock; the two radii stay independently bounded.
- `PlayerTicketGuard` owns the active dimension's pair plus a home-spawn refresh handle. A dimension transition leases the destination pair before awaited wire delivery while the old pair stays active; failure or cancellation drops the lease; success adopts it, removes the old pair, reconciles both sources and then publishes presence, with no await and never two store locks at once. Handle identity, not equal coordinates, decides whether a new lease is needed; disconnect drops the currently owned pair.
- Sources with no ticket capability keep an isolated compatibility handle. A ticket-backed home needs every selected sibling to expose its own store; a missing capability fails the transition rather than silently pinning home terrain. Handle resolution happens at join or transition, never per tick, and handoffs do not request terrain, change retention or define mob activation.

### The ticked area follows the player

The columns the world tick simulates (random ticks, scheduled block/fluid ticks, natural spawning) centre on players, not world spawn. The coordinate list is cheap and rebuilt every tick; the terrain view natural spawning reads is rebuilt only when the list changes. A fixed-origin fallback square applies before a player's first movement packet and for tests with no players.

### Generation

The generator is pure per chunk with positionally seeded RNG, so a batch can fan out across scoped workers with no nondeterminism. Fix coordinate order before fanning out and encode/send in that order, never completion or hash-set order.

Production runs a single-threaded tokio runtime (server tick and all connections on one core). Fan-out fixes throughput; moving the batch to the blocking pool fixes latency. Calling in place instead panics on that runtime (it requires a multi-threaded one).

**Progressive generation** is a streaming request. `ChunkSource::column` always asks for `ChunkGenerationStage::Full`, so ticks, commands, collision and edits never see undecorated terrain. The streaming scheduler may call `column_at` with `Shaped` outside its full near band. An overworld shaped column has terrain through carving and structures but no ores, vegetation, top-layer work or generation-time spawn candidates, and is a valid packet needing no special client decoder.

- The tier is monotone: a full or edited column satisfies a shaped request; a later full request upgrades a shaped cache entry with the lock released; nothing downgrades. Disk columns win over shaped requests.
- Persistence is stricter: Anvil chunk NBT accepts only `Status = "minecraft:full"` and shaped columns are rejected before encoding. A missing or earlier marker reads as absent in `RegionChunkSource`, so it regenerates a complete column.
- `ColumnPipeline::with_generation_band` computes the band in Chebyshev distance, defaulting to all-full until a caller opts in. `DEFAULT_FULL_GENERATION_RADIUS` is 8 (margin over the simulation and interaction areas; not an allocation cap). Movement re-centres the band with the pending-column priority queue.
- At render distance 256 the square is 265,225 columns, about 7.9 GiB at a 31.1 KiB packed estimate before meshes and wire buffers. Progressive generation reduces construction work, not retained meshes or packets; any such ceiling needs a distant-LOD or residency policy, not a bigger chunk store.

### Encoding is offloaded

Generated-column jobs encode in the worker that produced them (lighting and serializing are CPU-heavy). The connection awaits the owned result from its selectable join future, so reads and ticks stay serviceable; wire order is fixed by request admission, not completion. `ChunkEncoder::try_encode_chunk` and `ServerProtocol::try_encode_chunk` are fallible and default to the infallible encoder; a rejecting encoder returns an owned diagnostic in coordinate order, and the connection closes after ending any batch whose opening marker was written (a view update that only accumulated its batch locally writes neither marker before disconnecting).

### View streaming and the keep-alive defect

How many columns are processed inside one unserviced async arm decides whether keep-alives survive a chunk-boundary crossing; moving work to a blocking pool does not fix it alone. The connection loop is a single-armed select, so awaiting a whole newly visible strip (dozens on a step, a full square on a teleport) left the socket unserviced, and a client that answered promptly still timed out. The fix streams a move like a join: compute coordinates synchronously and cheaply, then feed the incrementally draining pipeline so each pass pays for one column. A stall watchdog times each select-arm body (not the interval between passes, which is mostly idle and looks identical to a stall under a paused clock) and forgives a keep-alive only if the client was genuinely unreachable for a full interval.

On native hosts each authoritative tick runs on a dedicated timer-capable thread (`spawn_world_tick_task`); connections keep servicing packets while simulation runs. The browser keeps an event-loop task and a separate server worker. Generation still uses the bounded dispatcher.

The integrated liveness gate holds a requested column while timing Play-state ping echoes, reports median, p95 and maximum RTT plus tick stats, and fails if p95 exceeds 250 ms. The ignored profile `real_worldgen_keeps_play_packets_and_ticks_responsive_while_moving` uses the production Overworld source across five fresh views: `cargo test --release -p lodestone-server --test worldgen_tick_liveness real_worldgen_keeps_play_packets_and_ticks_responsive_while_moving -- --ignored --nocapture` (prints open time, delivered columns, tick rate, RTT percentiles).

### The client's single terrain store

The client once had two chunk stores (live session, offline/demo) with a three-term branch at every read site and diverging light rules, drop accounting and height limits. One ECS resource (`ChunkWorld`) replaced them. A read-only handle and a separate write handle name the same store, so render and collision systems cannot mutate it; writing is limited to prediction, net ingest and test harnesses. Facts the store cannot answer (dimension skylight default; renderer block-id space agreement) are tracked beside it and recomputed on session attach or dimension change.

The network driver keeps the write lock out of expensive full-column decodes: it first calls `VersionAdapter::decode_chunk_packet` without a sink (the 26.2 adapter does this for full chunk-with-light, returning an owned `DeferredChunkLoad`), takes the lock briefly to insert, drops it, then executes directives. Packet order and notification-after-insert hold. Adapters returning `None` keep `handle_packet`, including block updates and `sync_block_entity`. The `client_world` trace records `deferred_decode_us`, `adapter_or_apply_us`, `lock_wait_us`, `lock_hold_us` per packet.

### Measuring the client chunk pipeline

An instruction-denominated benchmark (hardware counters, since wall-clock is too noisy on a shared machine) walks decode, insert, snapshot, mesh and renderer submit over real terrain. Meshing dominates, fluid meshing disproportionately (many redundant neighbour queries per cell). Optimizations have paid off in instructions retired, not only locality, a useful diagnostic since locality gains show only in cycles per instruction.

## How to change it

- Do not add a "mutate one column in place" API: the tick loop already mutates a column and calls back into the store in the same breath, so a closure holding the lock self-deadlocks.
- Forward every source capability through dimensional and pointer wrappers. The event-only `ticket_store` default means no capability, not home-dimension ownership; production siblings missing it are rejected.
- Do not raise the tick-follow radius, parallel generation window or streaming batch size without re-checking what bounds it (worker parallelism, the LRU reserve, the client's ack-rate estimate).
- A lock held across a call that can re-enter it is a latent deadlock (loading a saved chunk's pending ticks can call back into the triggering structure); grep what a guarded section calls transitively before widening it.
- Every new `select!` arm in the connection loop must be timed by the stall watchdog (enter at start, mark at end), or it is invisible to keep-alive accounting.
- Implement a protocol's `try_encode_chunk` only when it can report an owned failure; callers own cleanup and end a batch only after its beginning marker reached the wire.
- Defer a packet only if its whole world effect is one chunk load: decode and validate the body before returning `DeferredChunkLoad`, which the driver applies before directives or the next packet. Sparse updates stay on `handle_packet` so ordered `WorldSink` calls keep block-entity sync.

## Configuration

- Store capacity: streamed view radius plus a fixed scan reserve, floored at a default and (hosted worlds only) capped.
- The tick-follow radius is a small constant sized separately for singleplayer and LAN.
- Ticket levels and timeouts are fixed constants, not tunable.
- `LODESTONE_WORLDGEN_WORKERS` overrides the worker count (default `max(available_parallelism - 1, 1)`); see [`worldgen-dispatch.md`](./worldgen-dispatch.md).
- Streaming batch size and keep-alive stall thresholds are constants in the server crate. The deferred decode hook is per-adapter and opt-in (26.2 uses it for full chunk loads only).

## Dependencies

Standard library for the store and ticket graph; `tokio`'s blocking pool (current-thread runtime in native production); the version-free `ServerProtocol`/`ChunkEncoder` seam (a family without an implementation keeps encoding on the connection task); the shared ECS crate and world-storage crate on the client, with no protocol version named.
