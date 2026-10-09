# Block action latency

## What it is

An opt-in shell trace records block-breaking milestones shared by the native and browser clients. It separates legitimate mining duration from acknowledgement, world-state observation, terrain handoff and presentation submission.

## How it works

- `BlockActionTrace` is an interaction resource. `Sim::begin_attack_live` attaches an optional `TraceId` to the `AttackPresses` entry, so a dropped press cannot shift another attempt's input time. `drive_mining` records a start only when the predictor emits `StartDestroy`, and a completion from `take_destroyed` using the completion action's sequence (instant breaks complete on START, progressive ones on STOP). Holds and plugin intents that start another attempt have no input timestamp.
- Egress is the attempt to hand the filtered action queue to the connection, not proof of a socket write. Cumulative acknowledgements are compared to the completion sequence with `PredictionSequence`; an earlier START acknowledgement cannot settle its STOP, and an acknowledgement alone does not imply acceptance.
- A changed-coordinate notification samples the current shared block state, labelled `authoritative_state_observed` (the notification carries coordinates, not a captured state). Observation continues after the prediction ledger retires the entry. AIR and non-AIR observations and local restoration are reported separately.
- The affected `SectionKey`s come from the normal section-invalidation rule; each observation or restoration replaces the set and clears its settlement and present stamps. Successful `upload_meshed` outcomes (including `Unchanged`) and renderer removals of resident empty sections satisfy keys. An already-empty section also settles with current empty-snapshot evidence, no pending removal and renderer-confirmed absence (counted separately in `empty_renderer_settlements`). Failed uploads, unrelated sections, world AIR alone and column unloads do not qualify. When all keys settle, `current_mesh_settled_ms` is recorded, then the next `frame.present` is the presentation stamp. None of this proves visibility, compositor completion or a lighting cause.
- Records end after completion acknowledgement, state observation and the next present. Abort, local rejection, overlapping outstanding completions at one position (ambiguous, since updates carry no attempt identity), expiry, eviction and session reset emit reports with missing milestones as `None`.
- Limits: 32 active attempts, 32 undrained reports (oldest dropped, counted in cumulative `dropped_reports`), 120 s expiry (diagnostic only). Disabled hooks do no clock reads, allocations, scans or world reads.

## How to change it

- State model and output fields live in `sim::block_action_trace`. Keep interaction milestones beside the predictor's actions in `drive_mining`, and egress, acknowledgement and state sampling at their existing simulation boundaries. Renderer hooks stay after successful uploads and removals and after presentation. Reset outstanding records on session or dimension replacement and disconnect.
- Section coverage comes from `dirty_sections_for_blocks` (one cell touches at most eight sections). Do not add a packet consumer or infer block identity from drops, pickups or lighting. Exact transport timing needs a callback after the driver's successful write; the queue-drain trace cannot supply it.
- Model tests use arithmetic stamps (input 101 ms, start 137, completion 1137, prediction 1140, egress 1144, ack 1181, state 1187, mesh handoff 1212, present 1221, giving intervals 36, 1000, 3, 37, 25, 9 ms) and cover early START ack, unrelated sections, sequence rollover, overlap, capacity and expiry. They are not a live latency measurement.

## Configuration

- **Native:** `LODESTONE_BLOCK_ACTION_TRACE=1` (any nonempty value except `0`). Reports go to tracing target `lodestone_block_action_trace` at INFO.
- **Browser:** disabled by default. SDK mount option `traceBlockActions: true`, or `session.setBlockActionTrace(bool)` later (Rust: `BrowserControl::set_block_action_trace_enabled`). The runner applies changes before the next frame's input, drains after redraw, and flushes once on disable. Reports arrive through `onProgress` every 50 ms with `type` and `phase` `block-action-trace` and the row in `message`. The export queue holds 32 rows, drops the oldest, and reports cumulative `browser_dropped_reports` separately from `dropped_reports`. Disabling emits unfinished attempts as `trace-disabled`. General debug logging does not enable it. Timestamps are milliseconds since activation.

  ```javascript
  const session = await sdk.mount({
    canvas, resourcePack, blocksJson, traceBlockActions: true,
    onProgress(event) { if (event.phase === "block-action-trace") console.log(event.message); },
  });
  session.setBlockActionTrace(false);
  ```
- **Render worker hosts:** `traceBlockActions` on the `mount` message, or after readiness `{ kind: "input", input: { type: "setBlockActionTrace", enabled: true } }`; progress comes back as `{ kind: "progress", event }`.
- **Standalone page:** `?trace-block-actions=1` prints reports to the console. With `?probe=1` the panel toggles the trace and keeps the latest 32 messages with receipt times in `#lodestone-responsiveness-report` (history resets on a new world join). Worker errors appear in the console and the probe status.

## Dependencies

`crate::platform::Instant`, the shell interaction resources, `PredictionSequence`, the shared `ChunkWorld`, section invalidation and the renderer handoff and presentation seams. No extra crates or services.
