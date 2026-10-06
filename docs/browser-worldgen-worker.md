# Browser world-generation worker

## What it is

The browser world-generation worker keeps the authoritative integrated server in a dedicated Web Worker and optionally runs owned packet preparation and multi-column generation through a bounded WebAssembly thread pool. The page receives protocol bytes through one transferred `MessagePort`; startup, pool selection, and world-generation progress use a separate control/progress channel.

## How it works

Join progress includes `gameplay-ready` once a submitted terrain frame passes
the shell's shared gameplay-input gate and its initial loading cover is gone.
The standalone responsiveness probe consumes this signal for Creative and
existing-world joins, which do not emit a Survival loading-cover milestone.
First terrain, cover readiness, and full-view settlement remain separate
milestones; a complete view alone does not assert that gameplay input is ready.

`web/scripts/stage_worker.sh` produces two bindgen outputs from the same worker crate: a portable serial module and an atomics-enabled module built with the pinned nightly and `wasm-bindgen-rayon`. Both use the web workspace's `worker-release` profile, which retains compact release settings while optimizing generation and server crates for speed without expanding the page profile. Staging patches the generated no-bundler Rayon helper to call the bindgen initialization export with its current object-shaped API, avoiding one deprecation warning per child worker. `worker_bootstrap.js` checks `crossOriginIsolated`, shared-memory construction, Atomics wait/notify, and a shared-memory Wasm validation module before selecting the threaded artifact. The pool is capped at four workers and leaves one reported hardware lane for the server connection and tick tasks.

If the capability probe is negative, the bootstrap selects the serial artifact. A rejection after threaded initialization begins is terminal and reports an error; it never mixes a partially initialized threaded module with a fresh serial module. Child workers never own world state; generation results and packet commits are accepted on the server owner in canonical order.

Join generation batches run as local tasks on the server owner and generate their columns one after another; a column does not yield part-way, so a long column is a synchronous span in both artifacts. The offloaded column helpers in `chunk` yield to the browser macrotask queue between columns. In the threaded artifact, batch generation through `run_worldgen_jobs` (the 26.3 source's multi-column path) fans out over the pool. Worker selection alone is not evidence of parallel execution: inspect job counts and pool diagnostics as well.

Initial packet preparation moves the owned `InitialPacketInput` through
`join_scheduler::prepare_owned_initial_packet`. Native connections submit it to
the bounded dispatcher; the threaded browser runs it through `owned_compute`,
one job at a time on a single permit held until the owner accepts the result;
the serial browser runs it inline. The job computes light, assembles the packet
column and encodes the directive without touching a mutable source. Acceptance
releases the permit before framing or transport waits. Dropping the waiter
skips a queued job, and a running job finishes and releases its permit while
its result is discarded. The Rayon implementation in `owned_compute` is
compiled only for atomics-enabled Wasm or native tests. Both confinement-rule
tables explicitly permit this backend module; new Rayon call sites outside the
approved modules remain errors.

The shell sends its already-computed integrated stream radius in the launch envelope's `viewRadius`, and the worker passes it to the authoritative server unchanged. This uses the same `integrated_stream_radius` policy as native singleplayer: configured render distance plus the mesh-dependency and movement-lookahead padding. The initial playable loading gate remains capped at radius six. The server still primes one column and streams the remaining desired view through its bounded deferred generation window; the larger halo is not an eager startup barrier.

The launch epoch is registered by the worker's Rust entry point. `cancel_worker` cancels the join requests of the active epoch only (`join_scheduler::cancel_browser_worker_epoch`): a request not yet generated is skipped, and one already running finishes but its result is never delivered.

After the server reports ready, the bootstrap calls the optional `sample_worker(epoch, callbackGapMs)` export on a one-second host timer. The gap comes from `performance.now()` between callbacks, so it includes time when the worker event loop could not run. Sampling ends when the export reports that the epoch is no longer active, or when cancellation, reset, or startup failure clears the timer. The Rust export posts `worker-health` records through the existing progress port; each includes tick count and witness, overrun count, MSPT, and callback gap. `observedTps` divides the tick-count delta by this measured callback interval; its first sample is null and paused intervals report zero. The existing `tps` field estimates capacity from tick work cost, not wall-time throughput: the console labels it `budget_tps` and reports `observed_tps` separately. Starting or cancelling a worker clears the delta baseline.

Health samples also expose `tickWaitCount`, `tickWakeP95Ms`, `tickWakeMaxMs`,
`tickDeadlineMaxMs`, `tickCatchUpCount`, `tickCooperativeYields`, `tickYieldMaxMs` and
`tickShedCount`. Wake delay excludes deadline debt already present when a wait
was requested. Low MSPT with high wake delay points to timer/executor service,
not expensive simulation. The shared world deadline policy retains ordinary
lateness and bounds recovery; it does not change connection timer semantics.

At debug or trace level, the worker also installs a timing sink for production operations. A fixed shared phase array accumulates call count, requested item count, elapsed sum, and longest call across the server and compute workers. Short locks cover recording and draining only; JavaScript message construction and posting occur after the lock is released. Failed posting restores the drained totals without discarding newer samples. The health callback drains nonempty phases into one `worldgen-timing` message, and the render worker forwards them to the page console. Normal logging levels do not install the sink, so generation performs no timing clock reads. Collection uses the existing console-log level rather than requiring a tracing subscriber.

`immutable-queue-wait` covers permit waiting and submission-to-start delay,
`immutable-compute` covers the complete worker job, and `immutable-return`
covers completion-to-owner acceptance. Compute time includes its nested stage
timers; all are elapsed intervals, not exclusive CPU measurements.

The owned executor's `packet-preparation-*` phases distinguish six intervals: `permit-wait` before the shared permit is
acquired, `pool-wait` from submission until worker entry, `compute` during the
job body, `return-wait` from completion until acceptance starts or the result
is discarded, `acceptance` during the owner's callback, and `permit-hold`
across pool wait, computation, return wait and acceptance. A discarded result
has no acceptance scope. RAII closes active intervals on cancellation, closed
channels and unwinding, and the permit remains owned until acceptance or
discard finishes. Stage timers overlap compute; permit hold overlaps the
other permit-owned intervals. These timings identify delay by operation role
without changing the one-permit executor or one-slot ordered encode window.
They do not report current queue occupancy or retained-byte estimates.

`generation-poll` times each synchronous poll of the independently driven local
generation future. `encode-poll` does the same for each ordered encode future.
`connection-poll` encloses the Play connection poll, including any consecutive
ready packet, relight and publication operations before it suspends.
The guards end when a poll returns, including `Pending`; time between polls is
excluded. These enclosing measurements overlap the stage guards and include
lock stalls or host descheduling during a poll, so they are server-thread
occupancy rather than CPU cycles. Compare their maxima with wake delay before
treating a long suspended yield or completion-to-acceptance interval as work.

Native and browser Play loops share `ConnectionService`: each loop pass checks
an eight-millisecond occupied-work budget and a 64-pass limit. Its poll wrapper
charges only active polls, preserving the accumulated budget across suspension
and repeated wakeups. Exhaustion yields between complete operations, using the
native executor or browser host, before admitting another pass. It never yields
inside a packet assembly or mutable transaction. An individual operation can
still exceed the budget; `connection-dispatch` separates inbound packet work
from the enclosing connection poll. Adjust the limits in `connection_service`
using movement and block-edit traces, not generation throughput alone.

For retained-light protocols, direct edits use the same deduplicated relight
queue on native and browser connections. Packet dispatch sends the block and
inventory effects without synchronously recomputing the entire neighboring
light footprint. The connection services one queued relight at a time between
input, tick and chunk operations; its service budget bounds consecutive ready
relights. Protocols without retained light keep the synchronous fallback.

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

Join progress distinguishes `full-view-presented` from `full-view-quiescent`.
The first can retain earlier geometry while replacement meshes are queued. The
second is sampled after Surface presentation and requires the requested view's
latest mesh handoffs or explicit empty classifications, with scheduler work,
column repairs, light intents and removals drained. Halo-only waiting columns
outside the view do not block it. This does not change gameplay readiness.
`pendingMeshes` counts scheduler work and ready results; `pendingLightRemeshes`
counts light intents not yet admitted. Join probes continue every 100 ms through
quiescence without debug logging, then retain the debug-only one-second cadence.

Wasm generation batches run as independently driven local tasks. The connection
polls its one-slot ordered encode queue while continuing packet and tick service;
dropping a batch receiver cancels its task. Generation errors are returned
through the normal chunk error boundary, not promoted to a panic.

After the singleton first target, a batch holds up to `generation_window`
targets and generates them in order on the owner. Measure worker-health gaps as
well as total batch time.

The `browser-yield` timing phase records actual host scheduling wait around
cooperative generation yields. Its totals are elapsed wait, not CPU work, and
can overlap other task timings. Compare yield maxima with worker-health gaps
before attributing a slow join to timer scheduling.
Calls count timing scopes, including capacity polls and cancelled waits; they
are not counts of completed JavaScript callbacks.

Generation cooperation uses `lodestone_time::browser_yield`: a FIFO host message
task when supported, otherwise a zero-delay timer. Slot and overlapping-region
polls retain timer waits rather than turning into fast repeated atomic/lock
checks. Tick deadlines remain host timers. Both worker variants use the same
policy; mutable state never moves into a host callback.

The standalone page's optional `?log=debug&probe=1` panel holds the public host
input bridge for a 20-second sprint/jump walk or a three-second mining action.
Join through the normal menus and aim at a block before mining. `Aim down`
sends a downward mouse-look delta through the same public input bridge;
at default sensitivity it aims straight down. Check the rendered target, then
start `Mine 3s` separately so look and attack cannot use the same stale target.
Neither control chooses or edits a block directly.
This exercises
the ordinary controller, server, protocol, meshing, and surface renderer; it
does not create terrain or edit a world directly. The panel uses virtual host
focus/pointer-lock state, not hardware input latency. It releases held inputs
on completion, Stop, page exit, or a worker error.
Stopping leaves pointer-lock state unchanged: forging an unlock would pause the
game and make the next action probe exercise a menu instead of gameplay.
The controls wait for both first presented terrain and loading-overlay readiness;
terrain drawn behind the loading overlay is not a playable-world signal.

The final report is printed to the console and stored as JSON text in
`#lodestone-responsiveness-report`. It includes timestamped baseline and final
diagnostics and at most 256 intervening samples; exact sample counts and
truncation remain explicit. The final snapshots survive sample truncation, so
end-of-walk delivery and presentation deficits remain observable.
Action reports also include `diagnosticSummary`, with fixed view, server, mesh
and mesh-queue groups. Each metric records its received sample count and maximum,
continuing beyond the raw-example cap and resetting at each action start.
Unavailable values remain absent. These are maxima of received diagnostics:
window percentiles are not whole-action percentiles, and cumulative overruns are
not an action-local overrun count.
Sampled-light admission checks, reads, skips and retained bytes are separate from
actual mesh builds. Witness capture timings retain each request cause independently;
`lightInputCaptureIntervals` reports cumulative call/time deltas with baseline and
tail lag, using the same missing/reset rules as mesh-pass intervals. A new join
clears all latest diagnostics so a previous world's counters cannot become its baseline.
The optional panel also toggles [block-action tracing](block-action-latency.md).
Its `blockActions.rows` retains the latest 32 trace messages independently of
diagnostic churn. `completedRows` separately retains the latest 32 parsed
`air-observed` reports and `non-air-observed` reports with `restored=true`, so
abort churn cannot evict every completed example. An unknown outcome or a
restoration flag alone is not a completion. `droppedRows` and
`droppedCompletedRows` count each ring's evictions; both rings reset on a new
create/open join and report snapshots copy their rows. The panel exposes worker
errors after startup. Receipt timestamps are not worker milestone times.
The report also retains the current join's create/open, first-terrain,
loading-overlay-ready, full-view-presented and full-view-quiescent milestones before any action probe starts.
Each includes page receipt time and the worker's elapsed time and column/mesh
counters when supplied. Only received milestones are recorded, in receipt order;
a new join clears the previous record. Completed action reports survive later
progress within the same join, with their join snapshot updated.
Latest timing rows are retained per phase, and `generationPhases` accumulates
every received timing row while the probe is active, even after the raw-sample
cap. At most 64 diagnostic categories are retained. These totals describe
completed operations reported during the capture, not an exclusive CPU partition
or operations whose entire lifetime necessarily falls inside it.
These are sampled diagnostics, not per-action acknowledgements. Server-center
changes prove chunk-boundary travel; a Walk command by itself does not. A mining
input does not prove a block was broken, confirmed, or drawn. Check the visible
result and packet/render evidence before reporting those outcomes.

`snapshot-assembly` covers building the packet column from generated terrain. `packet-lighting` times the actual light computation, not its session completion marker; `packet-encoding` times the actual protocol encoder, not a snapshot pass-through. `wire-send` includes framing, compression, and awaiting transport credit, so it is wall time rather than CPU time and can overlap other work. Item counts describe operation inputs, not unique generated columns or cache misses. Phase totals are diagnostics, not retired instructions or a complete partition of join time.

`lodestone-worldgen-long-task-harness.js` is staged as a diagnostic asset. Load it from a browser test page or DevTools, then call `LodestoneWorldgenMeasurement.measure({ seed: "42", runtimeMs: 5000 })`. Its report includes worker startup milestones, executor mode, startup/runtime duration, page `longtask` entries, and same-epoch worker-health samples. `maxCallbackGapMs` and `tickAdvancement` are `null` when there are no usable health samples; missing samples are unknown, not evidence of healthy ticks. `under100ms` is false when the browser does not expose the Long Tasks API, so an empty sample cannot be mistaken for proof of the target.

The standalone `?probe=1` join report also retains the selected executor and initialized
compute-pool width from the preparing-world startup message. `runtime: null` means the
message has not been observed, not serial execution. The report resets this identity on
each new join. Pool initialization proves the selected runtime and width, not simultaneous
use of every worker; compare immutable job timings separately from server-poll occupancy.

`web/scripts/measure_worker_size.sh` builds both worker variants and reports each post-bindgen Wasm file's raw/gzip/Brotli size, generated JavaScript glue raw/gzip size, and bindgen helper snippets. It is intentionally independent from the page-only `scripts/wasm-size.sh` gate.

## How to change it

The standalone probe lives in `web/responsiveness_probe.js` and is attached to
the page's existing render Worker by `web/src/main.rs`. Its inputs go through
the same `kind: "input"` messages as real canvas events. Run
`node --test web/responsiveness_probe.test.mjs` when changing held-input lifetime
or report bounds. The panel is disabled without `probe=1`; it does not change
the SDK API or readiness milestones.

Keep the serial and threaded artifact names in sync between `stage_worker.sh` and `worker.js`; bindgen's output directory is shared by both builds, so the second invocation must continue to use the same staging directory. Any change to the launch envelope must update the bootstrap tests and the shell's worker launcher together. Do not move protocol bytes onto the progress channel or send an owner materializer, source or store to child workers. Re-run the worker control tests and the foreground Wasm builds after changing the atomics flags, bindgen invocation, or pool cap.

If changing worker-health sampling, keep the export's epoch and callback-gap arguments aligned with the Rust implementation. The harness filters on its launch epoch, and its tick advancement is the last sampled `tickCount` minus the first; keep absent or insufficient samples represented as `null`.

Timing phases and accumulation live in `lodestone_server::worldgen_progress`; operation guards belong at the actual production call sites. Keep guards outside parallel item loops and end synchronous guards before yielding. The worker enumerates `WorldgenTimingPhase::ALL`, so adding a phase there automatically uses the existing fixed buffer and message bridge. Update the worker serializer and shell diagnostic forwarding together when changing the `worldgen-timing` message fields. An idle sample sends no phase rows; a callback delayed by generation reports its accumulated work on the next callback, not at the nominal one-second boundary.

Change `ConnectionProbe` and its production loop call for connection sampling;
update the worker serializer and shell forwarding together if fields change.
The disabled probe allocates no state and reads no clock. Its latest observation
is retained even between reports, so the stopped record contains the most recent
loop state rather than only the last one-second sample.

If the Rayon helper's generated call shape changes, update `patch_threaded_worker_helper.mjs` and its staging assertion together. The patcher is intentionally narrow: it fails rather than silently rewriting an unrecognized helper.

The server-side executor uses the persistent pool only for owned packet preparation; a JavaScript task queue must not become a second world owner or reorder packet commits. Keep packet preparation on the shared permit, release completed results before unrelated awaits, and preserve the ordered encode queue when changing `join_scheduler::prepare_owned_initial_packet`.

The executor's only caller is packet preparation in `join_scheduler`. The injected-executor controls use
authored poll and timer-boundary order to distinguish permit delay from pool
delay and check acceptance, discard and unwind without elapsed-time thresholds.

## Configuration

The threaded build is selected by the `wasm-threads` Cargo feature in `web/worker/Cargo.toml` and is compiled with `-C target-feature=+atomics,+bulk-memory` plus `-Z build-std=panic_abort,std` in `stage_worker.sh`. The runtime pool cap is four workers. Health sampling runs at a one-second interval after ready. `runtimeMs` controls only how long the measurement harness observes the ready worker; it does not alter game scheduling.

The worker's optional Rayon dependency enables `web_spin_lock` for Wasm synchronization.

`?log=debug` on the standalone page, or SDK `logLevel: "debug"`, enables sampled phase timing. Example console record: `worldgen timing: phase=packet-lighting calls=7 items=7 sum_ms=48.200 max_ms=9.300`. Each record covers work since the preceding successful sample, not the whole session.

`viewRadius` is a nonnegative signed 32-bit integer. Normal shell launches always supply the shared integrated stream radius; for example, render distance nine launches a server radius of eleven. Older control callers may omit `viewRadius`, retaining the worker's compatibility default of eight. The Wasm `start_worker` export accepts the optional radius after its existing log-level argument, preserving earlier argument positions.

Cancellation is scoped to the active worker epoch and takes effect between columns.

The deployment serving the page must send `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`. Without cross-origin isolation the serial artifact is selected deliberately.

## Dependencies

The worker depends on `lodestone-shell` for the authoritative server entry point, `wasm-bindgen`/`web-sys` for the transferable ports, and optional `wasm-bindgen-rayon` plus Rayon for the atomics-enabled build. The staging hook requires the repository's pinned nightly, `rust-src`, `wasm-bindgen`, and Trunk's staging directory. The bootstrap uses worker `setTimeout` and `performance.now()` for health sampling; the measurement harness uses browser `Worker`, `MessageChannel`, `PerformanceObserver`, and `performance.now()` APIs.
