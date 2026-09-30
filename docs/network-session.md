# Shell network sessions

## What it is

The shell network session is the boundary between asynchronous protocol work and the synchronous
simulation/render loop. `lodestone_shell::net` keeps the public façade and session lifecycle, while
its focused submodules own forwarded events, latest-value state, and browser integrated-server transport.

## How it works

The native and browser clients use the same `NetClient` lifecycle and `ClientAction` relay. A background
driver decodes client events and folds them into `NetUpdate` values for the simulation to drain. Values
where only the newest snapshot matters—weather, biome metadata, command suggestions, sky-light defaults,
and resource-pack prompts—use shared cells in `net/state.rs` instead of adding queue traffic.

The inbound update relay is bounded. When a burst fills it, the driver retains the next update and
yields until the frame loop drains the relay. This preserves event order and the session without
blocking the browser's main thread. Closing the session wakes a waiting driver so shutdown can finish.
Backpressure waits are logged at exponentially spaced counts with their elapsed time, so a slow
consumer remains diagnosable without logging every update.
The frame-side update pass logs calls over 32 ms with total updates, column arrivals, and
section-block updates; compare those counts with backpressure waits when diagnosing stalls.

Whole-column adapters may return `DeferredChunkLoad`: decoding runs outside the shared world lock,
then `lodestone_client::driver::apply_deferred_chunk` compares the borrowed resident column and light
with the decoded input under the existing write guard. It always installs the complete new
`LoadedChunk`, without cloning columns or retaining a history cache. First arrivals keep
`ClientEvent::ChunkLoaded`; known resident replacements become `ClientEvent::ChunkReplaced`, whose
`terrain_changed` flag comes only from this exact comparison. Both reach the public event stream and
the unconditional `GameEventBus` hook. Chunk observation consumers must handle both variants.

The shell forwards replacements as `NetUpdate::ChunkReplaced`. Differing column layout, blocks,
biomes or light retain full arrival invalidation and neighbor healing. Equal terrain leaves readiness,
presented coverage and independently queued arrival, light, heal or resource work untouched. Unknown
adapter paths and biome-only patches retain the conservative `ChunkLoaded` path. Structural storage
equality may miss equivalent palette repacking; it never treats different stored terrain as equal.
Heightmaps and block entities are always replaced but do not enter this terrain comparison:
`ClientHandle::column_heightmap` feeds weather directly, and `Sim::block_entity_frame_snapshot` reads
the current world for the rendered block-entity sources each frame.

`ClientHandle::chunk_ingress_stats` exposes bounded per-session cumulative counters: first loads,
resident replacements, terrain-equal and terrain-differing replacements, and comparison nanoseconds
(sum and maximum). These cover only deferred whole-column ingress, not unknown adapter paths.
Terrain equality is not whole-packet equality. The timing excludes decode, world-lock waiting,
installation and old-payload destruction; snapshots of independent atomics are not transactional.
Compare these observations with mesh-work and GPU-handoff counts rather than interpreting an unchanged
GPU fingerprint as proof of an equal incoming column.

Browser singleplayer starts its integrated server in a Worker. `net/browser.rs` translates the Worker
`MessagePort` into the client transport and waits for an explicit startup-ready response; startup errors
and post-start crashes remain distinct so a failed launch cannot create a second world owner.

`lodestone_net::Connection` drains incoming transport bytes whenever a packet write or flush waits for
capacity. Both session drivers execute responses inside their selected packet/action branch, so their
ordinary read branch cannot run during that response. Reading ahead returns MessagePort byte credits
and drains bounded in-memory streams, allowing simultaneous sends to finish. The caller must resume
its normal read loop after sending; the connection does not run an independent background reader.

Read-ahead keeps raw wire bytes until the next packet read, after any login compression or encryption
transition. Bytes already buffered by the codec remain ahead of that raw queue. The queue is limited to 8 MiB;
overflow or transport failure poisons the connection so a partially sent frame cannot be followed by a
new packet. A peer's read-side EOF is remembered while the pending write finishes. Trace target `netbuf`
records `write:read-ahead` byte counts and queue occupancy alongside browser write/read traces.

Native integrated sessions use a separate 120-second per-packet read watchdog during the initial join.
The in-memory server resolves and admits a fresh world's spawn before it sends the first Play packet;
that legitimate work can exceed the 30-second watchdog used for remote sockets. Once Play begins, the
server's regular keep-alive cadence keeps the same watchdog well inside its bound. This is independent
of the loading screen's own readiness deadline, which measures terrain and assets after the session has
entered the world-loading phase.

## How to change it

Keep `net.rs` as the compatibility façade: public types moved into a submodule must be re-exported there,
and callers should continue to use `lodestone_shell::net` paths. Add replayable simulation inputs to
`net/events.rs`; add latest-value, lock-free or snapshot state to `net/state.rs`. Browser Worker control
messages and transport shutdown belong in `net/browser.rs`. Preserve the bounded relay behavior and the
native/wasm transport seam when changing session setup. Do not replace the asynchronous full-relay
wait with a blocking send: the browser frame loop must be able to drain the queue.

Keep the terrain comparison at authoritative driver ingress: the shared world has already changed
before simulation notifications run. Preserve the identical-replacement control alongside block-to-air,
biome/light, layout, unload/reload, independent-metadata and pending-work controls when changing it.
The clock is `lodestone_time::Instant` on both native and browser clients. Adding a terrain input requires
including it in the conservative comparison; resource/option invalidation remains independent.

Keep write-side read-ahead protocol-blind and defer codec input until packet reads. The networking tests
exercise simultaneous sends over tiny duplex buffers, a write-only deadlock control, codec transitions,
half-close ordering, and overflow. Changes to framing or backpressure must preserve these cases.

## Configuration

Protocol selection is supplied as a protocol number and resolved by `lodestone-registry`. The browser
Worker uses the bundled `lodestone-server-worker.js` endpoint. Relay capacities and native LAN/persistence
options remain constants or fields in `lodestone_shell::net`.
The raw write-side read-ahead bound is `MAX_WRITE_READ_AHEAD` in `lodestone_net::connection`; MessagePort
credit capacity remains independently bounded by `DEFAULT_MESSAGE_PORT_CREDIT_BYTES`.

## Dependencies

- `lodestone-client` for the version-free client handle, events, actions, and transport builder.
- `lodestone-registry` for protocol-number adapter resolution.
- `lodestone-server` for integrated sessions and native LAN publication.
- `lodestone-net`, `wasm-bindgen`, and `web-sys` for the browser Worker transport.
- `lodestone-shell::sim` and render/menu consumers for the forwarded events and shared state.
