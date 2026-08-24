# Design: Live client frame-performance investigation

## What it is

This work adds a reproducible, Java-backed client benchmark for answering where
Lodestone spends frame time in real play, applies one measurement-selected client
optimization, and repeats the identical workload to establish the before/after
effect. It covers both terrain streaming and a deliberately dense showcase of
specialized render paths so a healthy terrain pass cannot hide expensive block
entities, entities, text, maps, or particles.

The investigation is client-only. Server tick performance is explicitly deferred;
the Java server is an input oracle whose job is to supply representative world and
entity state.

## How it works

### Workloads

Both workloads run the release client at 2560 x 1440, render distance 24, VSync
disabled, and an unlimited frame-rate cap. Each run records its git SHA, machine,
profile, resolution, render distance, workload, segment, and trial so results from
different conditions cannot be compared accidentally.

The **terrain** workload joins the existing normal-terrain 26.2 Java oracle. After a
20-second warm-up it records 30 seconds while stationary, then 60 seconds of
deterministic creative flight through previously unloaded terrain. The two segments
separate steady-state render cost from decode, meshing, upload, frontier remesh, and
world-stream churn.

The **showcase** workload joins the existing flat creative 26.2 Java oracle. Its
fixture composes the repository's current screenshot scenes, which already exercise
signs, player heads, patterned banners, text/item/block displays, block entities,
equipped armour stands, mobs, and campfire smoke. A benchmark-only scene extension
adds mapped item frames, more repeated instances, translucent geometry, and sustained
particle emitters. The client records fixed viewpoints and a slow deterministic
camera orbit so both per-object setup and changing visibility are represented.

The checked-in fixture describes commands and camera/movement intent; generated
world data and raw profiles remain local artifacts. A public downloadable map may be
used after the controlled trials as an external validation workload, but it is not a
regression gate because map URLs, licences, conversion behavior, and spawn state are
not stable enough to define a repeatable comparison.

### Automation

An opt-in shell benchmark driver owns only experiment choreography: warm-up,
stationary/moving segment boundaries, deterministic movement or camera intent,
metadata emission, and clean exit. It uses the production connection, simulation,
meshing, and rendering paths. It does not replace the player with a render-only
camera, skip packet handling, or bypass the normal frame loop.

The driver is disabled by default and activated by explicit CLI options. Normal
interactive play remains byte-for-byte on the existing path after argument parsing.
The runner launches the correct Java oracle, prepares the showcase fixture through
RCON, launches the release client, and fails closed when a server, GPU timing feature,
expected segment, or result file is missing.

### Measurement flow

Each segment is run three times in the foreground on an otherwise idle machine. The
existing `LODESTONE_FRAME_PROFILE_DUMP` CSV is the frame-level source of truth and the
existing wgpu timestamp queries separate CPU command construction from GPU execution.
The summarizer reports:

- frame-time median, p95, and p99;
- frames above 16.67 ms, 25 ms, and 33.3 ms;
- the existing CPU frame phases and world/HUD sub-phases;
- measured GPU passes, without treating diagnostic bracket spans as totals;
- sections visited/drawn/culled, draw calls, quads, and resident mesh bytes;
- process RSS at segment boundaries and its growth during the run.

A `samply` sampling profile is recorded for the worst baseline segment. Sampling is
used to attribute CPU cost to call stacks rather than to guess from subsystem size.
Allocation tracing is added only if the sample profile, RSS growth, or frame spikes
implicate allocation. Finer GPU pass timestamps are added only if the existing
timestamps demonstrate that the relevant segment is GPU-bound but are too coarse to
identify the pass.

### Root-cause and optimization rule

No renderer change is chosen before the baseline is complete. The largest actionable
cost is traced through its producer and consumers, compared with similar working
paths, and stated as one falsifiable hypothesis. Candidate ideas such as extra
culling, caching, batching, instancing, buffer layout changes, wgpu feature changes,
or hot-path allocation removal are evaluated only against that evidence.

The first implementation pass makes one focused change addressing the confirmed root
cause. It includes a regression test or benchmark control that fails against the old
behavior, then reruns the same three-trial protocol. A win must exceed the measured
run-to-run noise and must not trade an improvement in one segment for an unexplained
regression in the other. If no single change clears that bar, the result is reported
honestly and the evidence still determines the ranked follow-up plan.

## Data flow

```text
Java 26.2 oracle + checked-in scene commands
  -> production protocol/client world
  -> normal simulation, meshing, upload, culling, and render passes
  -> per-frame CPU CSV + GPU timestamps + renderer counts + RSS
  -> workload summarizer + same-machine JSONL history
  -> worst-segment samply profile
  -> one root-cause hypothesis
  -> regression control + focused optimization
  -> identical post-change trials and ratio report
```

Raw per-frame CSV and sampling profiles are local experiment artifacts because they
are large and machine-specific. Compact metadata and summaries live under
`bench-results/`; the interpretation, limitations, before/after ratios, and next
bottlenecks live in the feature documentation.

## Error handling

The benchmark runner exits non-zero if the selected oracle is unavailable, the
client fails to join, the expected warm-up or measurement segments do not complete,
the CSV lacks required columns, a segment contains no frames, or the process exits
without its completion marker. Unsupported GPU timestamps are reported as missing
GPU evidence rather than zero milliseconds.

Interrupted runs are never appended as successful trials. The summarizer rejects
mixed resolution, render distance, build profile, machine, or workload metadata.
Busy-machine evidence and run-to-run spread are printed beside the ratios; a duration
from a contended run is not silently attributed to the code change.

## Testing

- Parser tests cover every benchmark CLI option, incompatible combinations, and the
  unchanged default interactive configuration.
- Pure driver tests cover warm-up/measurement transitions, deterministic intent,
  completion, and early-exit behavior without a window or server.
- Fixture tests assert the showcase contains every required content category,
  including mapped item frames and sustained particles.
- Summarizer tests use small CSV fixtures to verify percentiles, missed-frame counts,
  skipped-field handling, metadata mismatch rejection, and incomplete-run rejection.
- The live benchmark has a short smoke mode that proves Java server -> real client ->
  GPU frame -> result file -> clean exit before expensive trials are attempted.
- Focused crate tests, the shell suite with `--no-fail-fast`, the version seam, wasm
  check when configuration or target-gated code changes, and the repository health
  commands verify correctness after the optimization.
- The optimization's control is observed failing before production code changes and
  passing afterward. The live before/after benchmark is the performance acceptance
  test; functional tests remain the correctness acceptance test.

## How to change it

Experiment choreography belongs in a small benchmark-driver module rather than in
renderer subsystems. Java scene commands belong beside the existing screenshot scene
fixtures and should reuse them instead of copying their content. Result parsing and
statistics belong in a standalone script with fixture tests so changing presentation
cannot perturb the client being measured.

Add a new workload by defining its server preparation, deterministic path, required
content controls, and metadata identity. Do not compare it with an existing workload
under the same scene name. Add finer instrumentation at the narrowest boundary that
the current evidence cannot distinguish, and account for the observer cost rather
than hiding it in another phase.

Renderer optimizations stay in the subsystem that owns the confirmed cost. Do not
turn benchmark-only shortcuts into production branches, restore Metal's retracted
multi-draw assumption, change the global allocator without live allocation evidence,
or report a synthetic demo-world gain as a real-world frame-time win.

## Configuration

The runner supplies explicit values for resolution, render distance, VSync, frame
limit, workload, trial count, warm-up, and measurement durations. The initial fixed
protocol is Minecraft 26.2 / protocol 776 because it is the only family Lodestone can
host and the repository's maintained live render target.

The existing `LODESTONE_FRAME_PROFILE_DUMP` environment variable selects the raw CSV
path. New benchmark CLI options are opt-in and never read from `options.json`, so a
persisted player preference cannot silently alter a recorded experiment. Local
server assets remain under `.cache/mc/`; benchmark outputs use `bench-results/` for
compact summaries and a temporary directory for raw traces.

## Dependencies

- `lodestone-shell`'s production frame loop, `FrameProfiler`, render statistics, and
  `GpuQueryTimer`.
- The existing `frame_profile` and `render_submit` benchmarks for synthetic controls
  and same-machine history conventions.
- The 26.2 normal-terrain and creative Java oracle launchers under
  `scripts/live-oracles/` plus their local server jar and Apple `container` runtime.
- Existing screenshot scene commands under `scripts/screenshot-scenes/` and RCON
  helpers for deterministic showcase construction.
- `samply` for CPU sampling; macOS allocation tooling only when allocation evidence
  warrants it.
- The bundled Python runtime or system Python standard library for result aggregation;
  no network service is required for the repeatable benchmark itself.
