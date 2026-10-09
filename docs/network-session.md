# Shell network sessions

## What it is

The boundary between asynchronous protocol work and the synchronous simulation and render loop. `lodestone_shell::net` keeps the public facade and session lifecycle; submodules own forwarded events, latest-value state and the browser integrated-server transport.

## How it works

- Native and browser clients share the `NetClient` lifecycle and `ClientAction` relay. A background driver decodes client events into `NetUpdate`s for the simulation to drain. Newest-only values (weather, biome metadata, command suggestions, sky-light defaults, resource-pack prompts) use shared cells in `net/state.rs` instead of queue traffic.
- The inbound relay is bounded: when a burst fills it, the driver holds the next update and yields until the frame loop drains, preserving order without blocking the browser main thread. Closing the session wakes a waiting driver. Backpressure waits are logged at exponentially spaced counts; the frame-side update pass logs calls over 32 ms with update, column and section-block counts.
- **Deferred chunk loads.** Whole-column adapters may return `DeferredChunkLoad`: decoding runs outside the world lock, then `lodestone_client::driver::apply_deferred_chunk` compares the borrowed resident column and light with the decoded input under the write guard and always installs the complete new `LoadedChunk` (no clones, no history cache). First arrivals emit `ClientEvent::ChunkLoaded`; replacements emit `ClientEvent::ChunkReplaced` whose `terrain_changed` comes only from this exact comparison. Both reach the public event stream and the `GameEventBus`, and consumers must handle both.
- The shell forwards `NetUpdate::ChunkReplaced`. A differing layout, blocks, biomes or light does full arrival invalidation and neighbour healing; equal terrain leaves readiness, presented coverage and queued work untouched. Unknown adapter paths and biome-only patches stay on the conservative `ChunkLoaded` path. Structural equality may miss equivalent palette repacking but never treats different terrain as equal. Heightmaps and block entities are always replaced outside this comparison (`ClientHandle::column_heightmap` feeds weather; `Sim::block_entity_frame_snapshot` reads the world each frame).
- `ClientHandle::chunk_ingress_stats` gives cumulative per-session counters (first loads, replacements, equal and differing replacements, comparison nanoseconds sum and max) for deferred ingress only. Timing excludes decode, lock waits, installation and old-payload drops; snapshots are not transactional. Compare with mesh-work and GPU-handoff counts.
- **Browser singleplayer** runs the integrated server in a Worker; `net/browser.rs` turns its `MessagePort` into the client transport and waits for an explicit ready response. Startup errors and post-start crashes stay distinct so a failed launch cannot create a second world owner.
- **Write-side read-ahead.** `lodestone_net::Connection` drains incoming bytes whenever a packet write or flush waits for capacity, because both drivers run responses inside their packet branch and the ordinary read branch cannot run meanwhile. This returns MessagePort byte credits and drains in-memory streams so simultaneous sends can finish. Callers must resume their read loop; there is no background reader. Raw bytes stay queued until the next packet read, after any compression or encryption transition (codec-buffered bytes stay ahead). The queue is capped at 8 MiB; overflow or transport failure poisons the connection so a partial frame cannot be followed by a new packet. A peer's read EOF is remembered while the write finishes. Trace target `netbuf` logs `write:read-ahead` counts and occupancy.
- **Watchdogs.** Native integrated sessions use a 120 s per-packet read watchdog during initial join, because the in-memory server resolves and admits a fresh world's spawn before the first Play packet; remote sockets use 30 s. After Play begins keep-alives keep it well inside bounds. It is separate from the loading screen's readiness deadline.

## How to change it

- Keep `net.rs` a facade: re-export public types moved into submodules. Replayable simulation inputs go in `net/events.rs`, latest-value or lock-free state in `net/state.rs`, Worker control and shutdown in `net/browser.rs`.
- Never replace the asynchronous full-relay wait with a blocking send; the browser frame loop must drain the queue.
- Keep the terrain comparison at authoritative driver ingress (the shared world already changed before notifications run), using `lodestone_time::Instant`. A new terrain input must join the conservative comparison; keep the identical-replacement control with block-to-air, biome/light, layout, unload/reload, metadata and pending-work controls.
- Keep read-ahead protocol-blind and defer codec input until packet reads. Preserve the tiny-duplex simultaneous-send, write-only deadlock, codec-transition, half-close and overflow tests.

## Configuration

Protocol selection is a number resolved by `lodestone-registry`. The browser Worker uses the bundled `lodestone-server-worker.js`. Relay capacities and native LAN and persistence options are constants or fields in `lodestone_shell::net`. `MAX_WRITE_READ_AHEAD` in `lodestone_net::connection` bounds read-ahead; `DEFAULT_MESSAGE_PORT_CREDIT_BYTES` bounds MessagePort credit independently.

## Dependencies

`lodestone-client` (handle, events, actions, transport builder), `lodestone-registry`, `lodestone-server` (integrated sessions, LAN), `lodestone-net`, `wasm-bindgen`, `web-sys`, and `lodestone-shell::sim` plus render and menu consumers.
