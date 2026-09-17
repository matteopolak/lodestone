# Browser world-generation worker

## What it is

The browser world-generation worker keeps the authoritative integrated server in a dedicated Web Worker and optionally runs its immutable shaped-admission work through a bounded WebAssembly thread pool. The page receives protocol bytes through one transferred `MessagePort`; startup, pool selection, and world-generation progress use a separate control/progress channel.

## How it works

`web/scripts/stage_worker.sh` produces two bindgen outputs from the same worker crate: a portable serial module and an atomics-enabled module built with the pinned nightly and `wasm-bindgen-rayon`. Staging patches the generated no-bundler Rayon helper to call the bindgen initialization export with its current object-shaped API, avoiding one deprecation warning per child worker. `worker_bootstrap.js` checks `crossOriginIsolated`, shared-memory construction, Atomics wait/notify, and a shared-memory Wasm validation module before selecting the threaded artifact. The pool is capped at four workers and leaves one reported hardware lane for the server connection and tick tasks.

If the capability probe is negative, the bootstrap selects the serial artifact. A rejection after threaded initialization begins is terminal and reports an error; it never mixes a partially initialized threaded module with a fresh serial module. The immutable executor is the only parallel boundary; mutable feature, top-layer, overlay, and packet commits remain in canonical server order and retain their cancellation, memory-budget, and fingerprint checks.

The serial production request uses an async adapter rather than the synchronous compatibility entry point. It yields to the browser macrotask queue after each shaped admission and ordered mutable source, then before packet encoding and after light settlement. This keeps the worker responsive between bounded generation operations without changing source order or allowing partial mutable commits. The threaded artifact uses the same session and commit path; only immutable admission is dispatched to Rayon.

The launch epoch is registered by the worker's Rust entry point. `cancel_worker` sets shared request cancellation only for the active epoch, and each session checkpoint observes it before admitting later work or settling the packet. Already-running synchronous work finishes cooperatively; its uncommitted transaction is discarded and committed prefixes remain reusable.

`lodestone-worldgen-long-task-harness.js` is staged as a diagnostic asset. Load it from a browser test page or DevTools, then call `LodestoneWorldgenMeasurement.measure({ seed: "42", runtimeMs: 5000 })`. Its report includes the worker startup milestones, executor mode, startup/runtime duration, and page `longtask` entries. `under100ms` is false when the browser does not expose the Long Tasks API, so an empty sample cannot be mistaken for proof of the target.

`web/scripts/measure_worker_size.sh` builds both worker variants and reports each post-bindgen Wasm file's raw/gzip/Brotli size, generated JavaScript glue raw/gzip size, and bindgen helper snippets. It is intentionally independent from the page-only `scripts/wasm-size.sh` gate.

## How to change it

Keep the serial and threaded artifact names in sync between `stage_worker.sh` and `worker.js`; bindgen's output directory is shared by both builds, so the second invocation must continue to use the same staging directory. Any change to the launch envelope must update the bootstrap tests and the shell's worker launcher together. Do not move protocol bytes onto the progress channel, and do not send mutable lifecycle state to child workers. Re-run the worker control tests and the foreground Wasm builds after changing the atomics flags, bindgen invocation, or pool cap.

If the Rayon helper's generated call shape changes, update `patch_threaded_worker_helper.mjs` and its staging assertion together. The patcher is intentionally narrow: it fails rather than silently rewriting an unrecognized helper.

The server-side executor must continue to use the persistent pool only for immutable admission. The serial adapter owns browser yield points around those same session stages; a JavaScript task queue must not become a second world owner or reorder mutable commits.

## Configuration

The threaded build is selected by the `wasm-threads` Cargo feature in `web/worker/Cargo.toml` and is compiled with `-C target-feature=+atomics,+bulk-memory` plus `-Z build-std=panic_abort,std` in `stage_worker.sh`. The runtime pool cap is four workers. `runtimeMs` controls only how long the measurement harness observes the ready worker; it does not alter game scheduling.

The worker's optional Rayon dependency enables `web_spin_lock` for Wasm synchronization.

Cancellation is scoped to the active worker epoch and is cooperative at session stage boundaries.

The deployment serving the page must send `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`. Without cross-origin isolation the serial artifact is selected deliberately.

## Dependencies

The worker depends on `lodestone-shell` for the authoritative server entry point, `wasm-bindgen`/`web-sys` for the transferable ports, and optional `wasm-bindgen-rayon` plus Rayon for the atomics-enabled build. The staging hook requires the repository's pinned nightly, `rust-src`, `wasm-bindgen`, and Trunk's staging directory. The measurement harness uses only browser `Worker`, `MessageChannel`, `PerformanceObserver`, and `performance.now()` APIs.
