# Block action latency

## What it is

An opt-in shell trace records block-breaking milestones shared by the native and browser clients. It separates legitimate mining duration from the subsequent acknowledgement, world-state observation, terrain handoff and presentation submission.

## How it works

`BlockActionTrace` is an interaction resource. `Sim::begin_attack_live` attaches an optional `TraceId` to the existing `AttackPresses` entry, so a dropped or rejected press cannot shift another attempt's input time. `drive_mining` records a start only when the predictor actually emits `StartDestroy`; it records completion from `take_destroyed`, using the completion action's sequence. Instant breaks complete on START, while progressive breaks complete on STOP. A hold or plugin intent that starts another attempt has no delivered-press timestamp.

The egress milestone is the attempt to hand the filtered action queue to the connection. It does not prove that bytes were written to a socket. Cumulative acknowledgements are compared to the completion sequence using `PredictionSequence`; an earlier progressive START acknowledgement cannot settle its STOP. An acknowledgement alone does not imply acceptance.

A changed-coordinate notification triggers a sample of the current shared block state. This is labelled `authoritative_state_observed`, because the notification carries coordinates rather than an immutable state captured from a packet. Observation remains active after the ordinary prediction ledger has retired an acknowledged entry. AIR and non-AIR observations are reported separately; local restoration is also explicit.

The trace uses the normal section-invalidation rule to register the currently loaded affected `SectionKey`s. Each authoritative observation or local restoration replaces that set and clears its previous settlement and present timestamps. Successful `upload_meshed` outcomes, including `Unchanged`, and explicit renderer removals for resident empty sections satisfy matching keys. After the normal drains, an already-empty section can also settle when the scheduler has current empty-snapshot evidence, no pending removal, and the renderer confirms absence. `empty_renderer_settlements` counts this path separately. A failed upload, an unrelated section, world AIR alone or a column unload does not qualify. Once every current affected key is settled, `current_mesh_settled_ms` is recorded and the following `frame.present` records presentation submission. These milestones do not prove visibility of the edited face, completion by the browser compositor, or a causal lighting change.

Records end after completion acknowledgement, state observation and the following present submission. Abort, local input rejection, overlapping outstanding completions at one position, expiry, capacity eviction and session resets produce reports with missing milestones left as `None`. Repeated outstanding positions are ambiguous because a block-coordinate update has no attempt identity.

The resource keeps at most 32 active attempts and 32 undrained reports. The oldest undrained report is dropped on overflow, with a cumulative `dropped_reports` count in later reports. Active attempts expire after 120 seconds, including an exceptionally long unfinished dig; expiry is a diagnostic outcome and does not alter mining. Disabled hooks perform no clock reads, allocations, action scans or world reads.

## How to change it

The state model and output fields live in `sim::block_action_trace`. Keep interaction milestones beside the predictor's actual actions and predicted write in `drive_mining`; keep egress, acknowledgement and shared-state sampling at their existing simulation boundaries. Renderer hooks must remain after successful production uploads/removals and after presentation submission. Reset outstanding records whenever a session or dimension is replaced or disconnected.

Change section coverage through `dirty_sections_for_blocks`, which is also used by normal authoritative invalidation. A single cell affects at most eight sections. Do not add a packet consumer or infer a block identity from nearby item drops, pickups or lighting. Exact transport timing would require a semantic callback after the driver successfully writes the action; it cannot be derived from this queue-drain trace.

The focused model tests use arithmetic timestamps: input 101 ms, start 137 ms, completion 1137 ms, prediction 1140 ms, egress 1144 ms, acknowledgement 1181 ms, state observation 1187 ms, mesh handoff 1212 ms and present submission 1221 ms. Their expected intervals are 36 ms input delay, 1000 ms mining, 3 ms prediction delay, 37 ms egress-to-acknowledgement, 25 ms state-to-mesh and 9 ms mesh-to-present. They also exercise earlier START acknowledgement, unrelated section handoff, sequence rollover, overlap, capacity and expiry. Model tests are not a live latency measurement.

## Configuration

Native clients enable the trace with `LODESTONE_BLOCK_ACTION_TRACE=1`; any nonempty value except `0` enables it. Reports use the `lodestone_block_action_trace` tracing target at INFO level.

Browser clients start disabled. The SDK mount option `traceBlockActions: true` enables tracing, and the returned handle's `setBlockActionTrace(bool)` changes it afterward. The underlying Rust control is `BrowserControl::set_block_action_trace_enabled`. The runner applies a changed setting before processing the next frame's input, drains reports after redraw while enabled, and flushes once when disabled.

Reports arrive through the existing `onProgress` callback every 50 ms with `type` and `phase` set to `block-action-trace`; `message` contains the trace row. The browser export queue holds 32 rows, drops its oldest row on overflow, and includes cumulative `browser_dropped_reports` in later rows. This count is separate from the trace resource's `dropped_reports`. Disabling emits unfinished attempts with outcome `trace-disabled`. Enabling general debug logging does not enable this trace. Timestamps are milliseconds relative to trace activation; `None` means that milestone was not observed.

```javascript
const session = await sdk.mount({
  canvas, resourcePack, blocksJson,
  traceBlockActions: true,
  onProgress(event) {
    if (event.phase === "block-action-trace") console.log(event.message);
  },
});
session.setBlockActionTrace(false);
```

Hosts using the render worker can set `traceBlockActions` on its `mount` message or send `{ kind: "input", input: { type: "setBlockActionTrace", enabled: true } }` after readiness. Trace progress is forwarded as the worker's ordinary `{ kind: "progress", event }` message.

The standalone page enables the same trace with `?trace-block-actions=1` and prints report messages to the JavaScript console independently of its general logging level. With `?probe=1`, the optional panel can toggle the trace and retains the latest 32 report messages and their receipt times in `#lodestone-responsiveness-report`, together with a dropped-row count. A new world join resets this bounded history. Worker errors appear in both the console and the probe status, including after the boot overlay is removed.

## Dependencies

The trace uses `crate::platform::Instant` for the shared native/browser clock, the shell interaction resources, `PredictionSequence` serial ordering, the shared `ChunkWorld`, production section invalidation and renderer handoff/presentation seams. It has no additional crate or external service dependency.
