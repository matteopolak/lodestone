# Browser world-generation worker

## What it is

The browser world-generation worker keeps the authoritative integrated server in a dedicated Web Worker and optionally runs its immutable shaped-admission work through a bounded WebAssembly thread pool. The page receives protocol bytes through one transferred `MessagePort`; startup, pool selection, and world-generation progress use a separate control/progress channel.

## How it works

`web/scripts/stage_worker.sh` produces two bindgen outputs from the same worker crate: a portable serial module and an atomics-enabled module built with the pinned nightly and `wasm-bindgen-rayon`. Staging patches the generated no-bundler Rayon helper to call the bindgen initialization export with its current object-shaped API, avoiding one deprecation warning per child worker. `worker_bootstrap.js` checks `crossOriginIsolated`, shared-memory construction, Atomics wait/notify, and a shared-memory Wasm validation module before selecting the threaded artifact. The pool is capped at four workers and leaves one reported hardware lane for the server connection and tick tasks.

If the capability probe is negative, the bootstrap selects the serial artifact. A rejection after threaded initialization begins is terminal and reports an error; it never mixes a partially initialized threaded module with a fresh serial module. The immutable executor is the only parallel boundary; mutable feature, top-layer, overlay, and packet commits remain in canonical server order and retain their cancellation, memory-budget, and fingerprint checks.

The serial production request uses an async adapter rather than the synchronous compatibility entry point. It yields to the browser macrotask queue after each shaped admission and ordered mutable source, then before packet encoding and after light settlement. This keeps the worker responsive between bounded generation operations without changing source order or allowing partial mutable commits. The threaded artifact uses the same session and commit path. Its immutable executor supports Rayon, but the preferred pristine-world shaped batch currently uses a serial map on Wasm; selecting a threaded artifact does not prove that batch ran in parallel.

The shell sends its already-computed integrated stream radius in the launch envelope's `viewRadius`, and the worker passes it to the authoritative server unchanged. This uses the same `integrated_stream_radius` policy as native singleplayer: configured render distance plus the mesh-dependency and movement-lookahead padding. The initial playable loading gate remains capped at radius six. The server still primes one column and streams the remaining desired view through its bounded deferred generation window; the larger halo is not an eager startup barrier.

The launch epoch is registered by the worker's Rust entry point. `cancel_worker` sets shared request cancellation only for the active epoch, and each session checkpoint observes it before admitting later work or settling the packet. Already-running synchronous work finishes cooperatively; its uncommitted transaction is discarded and committed prefixes remain reusable.

After the server reports ready, the bootstrap calls the optional `sample_worker(epoch, callbackGapMs)` export on a one-second host timer. The gap comes from `performance.now()` between callbacks, so it includes time when the worker event loop could not run. Sampling ends when the export reports that the epoch is no longer active, or when cancellation, reset, or startup failure clears the timer. The Rust export posts `worker-health` records through the existing progress port; each includes tick count and witness, overrun count, MSPT, and callback gap.

At debug or trace level, the worker also installs a timing sink for production operations. A fixed phase array accumulates call count, requested item count, elapsed sum, and longest call. The health callback drains nonempty phases into one `worldgen-timing` message, and the render worker forwards them to the page console. Normal logging levels do not install the sink, so generation performs no timing clock reads. Collection uses the existing console-log level rather than requiring a tracing subscriber.

An optional `lodestone_server::connection_progress` sink samples
the browser Play connection state at most once per second. It reports
the current center/radius, owed and uniquely delivered columns, send operations,
remaining generation work, loop passes, and client loading state. A stopped
record is emitted when the observed loop scope exits or its future is dropped;
it does not distinguish transport EOF, error, and cancellation. A panic that
aborts the worker cannot emit that final record. These messages use the existing
progress port and appear as `connection` diagnostics in the standalone console.
World tick health alone does not prove that this connection is advancing.

An independent diagnostic timer also reports the Play loop's selected operation,
its elapsed time, and the current target or input packet. It continues while an
operation is awaiting generation or outbound credit; pass and delivery counts
still advance only in the actual connection loop. The timer holds only a weak
probe reference and stops after connection scope exit. `transport` diagnostics
report both endpoint byte counters, credit windows, and pending-write age using
the transport's existing heartbeat. Endpoint numbers are local to each worker,
so compare the `side=client` and `side=server` labels rather than IDs alone.
Snapshots are opt-in at Debug level and are not SDK readiness events. A normal
transport close cancels its timer; an absent final transport sample alone does
not prove a live wait or a disconnect.

Shared block-update timing uses `lodestone_time::Instant`, which reads the host
monotonic clock in the browser. Native runtime deadline types are compile-time
confined to native targets; they must not be used for shared elapsed-time
measurements. A clock trap can abandon one browser async task while independent
world ticks and host callbacks continue, so healthy tick telemetry alone cannot
establish Play-loop liveness.

At Debug level the renderer reports configured-view resident/presented deficits
once per second, including movement after the full-view presentation milestone.
The loading overlay uses its smaller
initial window, so its received count is not the configured-view count. Compare
view deficits with connection delivery and mesh queue age before attributing a
delay to generation. These sampled console diagnostics are debug-only and do not
extend the SDK progress-event contract.

The standalone page's optional `?log=debug&probe=1` panel holds the public host
input bridge for a 20-second sprint/jump walk or a three-second mining action.
Join through the normal menus and aim at a block before mining. This exercises
the ordinary controller, server, protocol, meshing, and surface renderer; it
does not create terrain or edit a world directly. The panel uses virtual host
focus/pointer-lock state, not hardware input latency. It releases held inputs
on completion, Stop, page exit, or a worker error.
The controls wait for both first presented terrain and loading-overlay readiness;
terrain drawn behind the loading overlay is not a playable-world signal.

The final report is printed to the console and stored as JSON text in
`#lodestone-responsiveness-report`. It includes timestamped baseline diagnostics
and at most 256 samples; exact sample counts and truncation remain explicit.
These are sampled diagnostics, not per-action acknowledgements. Server-center
changes prove chunk-boundary travel; a Walk command by itself does not. A mining
input does not prove a block was broken, confirmed, or drawn. Check the visible
result and packet/render evidence before reporting those outcomes.

Lease acquisition, pre-ore preparation, structure context, and shaped-product construction are measured separately. Prefix import, mutable settlement, and snapshot assembly cover synchronous session work, excluding browser yields. `packet-lighting` times the actual light computation, not its session completion marker; `packet-encoding` times the actual protocol encoder, not a snapshot pass-through. `wire-send` includes framing, compression, and awaiting transport credit, so it is wall time rather than CPU time and can overlap other work. Item counts describe operation inputs, not unique generated columns or cache misses. Phase totals are diagnostics, not retired instructions or a complete partition of join time.

`lodestone-worldgen-long-task-harness.js` is staged as a diagnostic asset. Load it from a browser test page or DevTools, then call `LodestoneWorldgenMeasurement.measure({ seed: "42", runtimeMs: 5000 })`. Its report includes worker startup milestones, executor mode, startup/runtime duration, page `longtask` entries, and same-epoch worker-health samples. `maxCallbackGapMs` and `tickAdvancement` are `null` when there are no usable health samples; missing samples are unknown, not evidence of healthy ticks. `under100ms` is false when the browser does not expose the Long Tasks API, so an empty sample cannot be mistaken for proof of the target.

`web/scripts/measure_worker_size.sh` builds both worker variants and reports each post-bindgen Wasm file's raw/gzip/Brotli size, generated JavaScript glue raw/gzip size, and bindgen helper snippets. It is intentionally independent from the page-only `scripts/wasm-size.sh` gate.

## How to change it

The standalone probe lives in `web/responsiveness_probe.js` and is attached to
the page's existing render Worker by `web/src/main.rs`. Its inputs go through
the same `kind: "input"` messages as real canvas events. Run
`node --test web/responsiveness_probe.test.mjs` when changing held-input lifetime
or report bounds. The panel is disabled without `probe=1`; it does not change
the SDK API or readiness milestones.

Keep the serial and threaded artifact names in sync between `stage_worker.sh` and `worker.js`; bindgen's output directory is shared by both builds, so the second invocation must continue to use the same staging directory. Any change to the launch envelope must update the bootstrap tests and the shell's worker launcher together. Do not move protocol bytes onto the progress channel, and do not send mutable lifecycle state to child workers. Re-run the worker control tests and the foreground Wasm builds after changing the atomics flags, bindgen invocation, or pool cap.

If changing worker-health sampling, keep the export's epoch and callback-gap arguments aligned with the Rust implementation. The harness filters on its launch epoch, and its tick advancement is the last sampled `tickCount` minus the first; keep absent or insufficient samples represented as `null`.

Timing phases and accumulation live in `lodestone_server::worldgen_progress`; operation guards belong at the actual production call sites. Keep guards outside parallel item loops and end synchronous guards before yielding. Update the worker serializer and shell diagnostic forwarding together when changing the `worldgen-timing` message. An idle sample sends no phase rows; a callback delayed by generation reports its accumulated work on the next callback, not at the nominal one-second boundary.

Change `ConnectionProbe` and its production loop call for connection sampling;
update the worker serializer and shell forwarding together if fields change.
The disabled probe allocates no state and reads no clock. Its latest observation
is retained even between reports, so the stopped record contains the most recent
loop state rather than only the last one-second sample.

If the Rayon helper's generated call shape changes, update `patch_threaded_worker_helper.mjs` and its staging assertion together. The patcher is intentionally narrow: it fails rather than silently rewriting an unrecognized helper.

The server-side executor must continue to use the persistent pool only for immutable admission. The serial adapter owns browser yield points around those same session stages; a JavaScript task queue must not become a second world owner or reorder mutable commits.

## Configuration

The threaded build is selected by the `wasm-threads` Cargo feature in `web/worker/Cargo.toml` and is compiled with `-C target-feature=+atomics,+bulk-memory` plus `-Z build-std=panic_abort,std` in `stage_worker.sh`. The runtime pool cap is four workers. Health sampling runs at a one-second interval after ready. `runtimeMs` controls only how long the measurement harness observes the ready worker; it does not alter game scheduling.

The worker's optional Rayon dependency enables `web_spin_lock` for Wasm synchronization.

`?log=debug` on the standalone page, or SDK `logLevel: "debug"`, enables sampled phase timing. Example console record: `worldgen timing: phase=packet-lighting calls=7 items=7 sum_ms=48.200 max_ms=9.300`. Each record covers work since the preceding successful sample, not the whole session.

`viewRadius` is a nonnegative signed 32-bit integer. Normal shell launches always supply the shared integrated stream radius; for example, render distance nine launches a server radius of eleven. Older control callers may omit `viewRadius`, retaining the worker's compatibility default of eight. The Wasm `start_worker` export accepts the optional radius after its existing log-level argument, preserving earlier argument positions.

Cancellation is scoped to the active worker epoch and is cooperative at session stage boundaries.

The deployment serving the page must send `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`. Without cross-origin isolation the serial artifact is selected deliberately.

## Dependencies

The worker depends on `lodestone-shell` for the authoritative server entry point, `wasm-bindgen`/`web-sys` for the transferable ports, and optional `wasm-bindgen-rayon` plus Rayon for the atomics-enabled build. The staging hook requires the repository's pinned nightly, `rust-src`, `wasm-bindgen`, and Trunk's staging directory. The bootstrap uses worker `setTimeout` and `performance.now()` for health sampling; the measurement harness uses browser `Worker`, `MessageChannel`, `PerformanceObserver`, and `performance.now()` APIs.
