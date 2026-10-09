# Browser world-generation worker

## What it is

The browser world-generation worker keeps the authoritative integrated server in a dedicated Web Worker and optionally runs owned packet preparation and multi-column generation on a bounded WebAssembly thread pool. The page receives protocol bytes through one transferred `MessagePort`; startup, pool selection and progress use a separate control/progress channel.

## How it works

### Artifacts and selection

- `web/scripts/stage_worker.sh` builds two bindgen outputs from the worker crate: a serial module and an atomics module (pinned nightly, `wasm-bindgen-rayon`). Both use the `worker-release` profile.
- `worker_bootstrap.js` checks `crossOriginIsolated`, shared-memory construction, Atomics wait/notify and a shared-memory validation module, then picks the threaded or serial artifact. The pool is capped at four workers and leaves one hardware lane for the server.
- A rejection after threaded initialisation starts is terminal; it never falls back mid-way to serial. Child workers never own world state, and results are accepted on the server owner in canonical order.
- The shell sends its integrated stream radius as `viewRadius` in the launch envelope; the server primes one column and streams the rest through its bounded deferred window. The loading gate stays capped at radius six.
- `cancel_worker` cancels only the active epoch (`join_scheduler::cancel_browser_worker_epoch`): unstarted requests are skipped, running ones finish but are never delivered.

### Generation and packet preparation

- Join generation batches run as local tasks on the owner, one column after another; a column does not yield part-way. Offloaded helpers in `chunk` yield between columns, and the threaded build fans batches out via `run_worldgen_jobs`. Worker selection alone does not prove parallelism; check job counts and pool diagnostics.
- `join_scheduler::prepare_owned_initial_packet` moves an owned `InitialPacketInput` through the executor: bounded dispatcher on native, `owned_compute` (one job at a time on a single permit, held until the owner accepts) on threaded wasm, inline on serial wasm. The job computes light, assembles the column and encodes the directive. Dropping the waiter skips a queued job; a running job finishes and its result is discarded.
- The Rayon code in `owned_compute` is compiled only for atomics wasm or native tests. Both confinement tables allow this module; Rayon elsewhere is an error.
- Native and browser Play loops share `ConnectionService`: an 8 ms occupied-work budget and a 64-pass limit per loop pass, yielding only between complete operations. Retained-light protocols share one deduplicated relight queue, serviced one entry at a time.
- Generation yields use `lodestone_time::browser_yield` (a FIFO host message task when available, else a zero-delay timer). Use `lodestone_time::Instant` for shared elapsed time, never native runtime deadline types.

### Diagnostics

All diagnostics are opt-in and flow over the progress port; none extend the SDK readiness contract.

- **Join milestones**: `gameplay-ready` (terrain passes the shared input gate and the cover is gone), first terrain, cover readiness, `full-view-presented` (may retain old geometry) and `full-view-quiescent` (mesh, light and repair queues drained) are separate.
- **Worker health**: after ready, a one-second host timer calls `sample_worker(epoch, callbackGapMs)`, posting `worker-health` with tick count, overruns, MSPT, callback gap and tick-wake statistics. `observedTps` is wall-time throughput; `tps` (`budget_tps` in the console) estimates capacity from tick cost. Low MSPT with high wake delay points at timer or executor service.
- **Phase timing** (debug/trace log level only): a fixed shared phase array (`lodestone_server::worldgen_progress`, `WorldgenTimingPhase::ALL`) accumulates calls, items, sum and max, drained into `worldgen-timing` messages. Timings are elapsed intervals that overlap one another, not CPU time. Notable groups: `immutable-*`, `packet-preparation-*` (permit-wait, pool-wait, compute, return-wait, acceptance, permit-hold), `generation-poll`, `encode-poll`, `connection-poll`, `wire-send`, `browser-yield`.
- **Connection and transport**: `lodestone_server::connection_progress::ConnectionProbe` samples the Play loop at most once per second (centre, radius, delivered columns, passes); a separate timer reports the selected operation and transport byte counters. Healthy tick telemetry does not prove the Play loop is advancing.
- **Probe panel**: `?log=debug&probe=1` adds a panel (`web/responsiveness_probe.js`, attached by `web/src/main.rs`) that drives a 20 s sprint/jump walk or a 3 s mining action through the public input bridge, after first terrain and overlay readiness. It uses virtual focus and never creates terrain or edits blocks. The JSON report lands in `#lodestone-responsiveness-report`, with baseline/final snapshots, at most 256 samples and a `diagnosticSummary` of maxima. It also toggles [block-action tracing](block-action-latency.md). Sampled diagnostics do not prove a block was broken or drawn; check the visible result.
- **Long-task harness**: `lodestone-worldgen-long-task-harness.js` is staged as a diagnostic; call `LodestoneWorldgenMeasurement.measure({ seed: "42", runtimeMs: 5000 })`. `null` health fields mean unknown, not healthy.
- `web/scripts/measure_worker_size.sh` reports raw/gzip/Brotli size of both worker variants, independent of the page-only `scripts/wasm-size.sh`.

## How to change it

- Keep the artifact names in sync between `stage_worker.sh` and `worker.js`; both bindgen runs share one staging directory. A launch-envelope change must update bootstrap tests and the shell launcher together.
- Never move protocol bytes onto the progress channel, or send an owner, source or store to child workers.
- After changing atomics flags, the bindgen invocation or the pool cap, rerun the worker control tests and the foreground Wasm builds.
- If the generated Rayon helper's call shape changes, update `patch_threaded_worker_helper.mjs` and its staging assertion; the patcher fails on unrecognised input on purpose.
- Keep packet preparation on the shared permit, release results before unrelated awaits, and preserve the one-slot ordered encode queue.
- Timing guards belong at real production call sites, outside parallel item loops, ended before yielding. Adding a phase to `WorldgenTimingPhase::ALL` is enough for the buffer; update the worker serializer and shell forwarding together when changing message fields.
- Changing `sample_worker` arguments means updating the Rust export and the harness (it filters on launch epoch).
- Tune connection-loop limits in `connection_service` with movement and block-edit traces.
- Run `node --test web/responsiveness_probe.test.mjs` after touching held-input lifetime or report bounds.

## Configuration

- `wasm-threads` Cargo feature in `web/worker/Cargo.toml`; built with `-C target-feature=+atomics,+bulk-memory` and `-Z build-std=panic_abort,std`. Rayon enables `web_spin_lock`.
- `?log=debug` or SDK `logLevel: "debug"` enables phase timing, e.g. `worldgen timing: phase=packet-lighting calls=7 items=7 sum_ms=48.200 max_ms=9.300`.
- `viewRadius`: non-negative i32; the shell always supplies it (render distance 9 gives 11); callers that omit it get 8. `start_worker` takes it after the log-level argument.
- The host must send `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`, or the serial artifact is chosen.

## Dependencies

- `lodestone-shell` (server entry point), `wasm-bindgen`/`web-sys`, optional `wasm-bindgen-rayon` and Rayon.
- Staging needs the pinned nightly, `rust-src`, `wasm-bindgen` and Trunk's staging directory.
