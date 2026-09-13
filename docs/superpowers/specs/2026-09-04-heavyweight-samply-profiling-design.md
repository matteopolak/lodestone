# Heavyweight Samply Profiling Design

## What it is

This design defines deterministic, locally profiled heavyweight scenes for finding
where a populated world spends CPU time. It adds two complementary release-built
paths: a headless integrated-server path for simulation and scheduled work, and a
shipped-client path for packet ingestion, meshing, GPU submission, and presentation.

The output is evidence for choosing the next investigation, not a cross-machine
performance contest. A capture is compared with the immediately relevant local
capture only after confirming that both runs exercised the same scenario witnesses.
There are deliberately no duration thresholds, committed timing baselines, or
pass/fail claims about one machine's speed.

## Goals and boundaries

The design must provide the following independently selectable workloads and one
mixed workload:

- dense, mixed block-state palettes;
- opaque, cutout, and translucent terrain;
- static and changing light sources;
- liquid surfaces and liquid-update work;
- sign text;
- block-entity rendering;
- large, varied entity populations; and
- scheduled server work.

Every workload must use an ordinary production route. A server workload writes
through the same world mutation, notification, scheduling, and tick mechanisms as
gameplay. A client workload is authored as server commands, accepted by the local
server, transmitted as normal packets, and consumed by the usual client session,
mesher, and frame loop. Direct world-container writes, hand-built entity draw lists,
and one-shot renderer sources remain useful unit-test tools, but are not scenario
construction mechanisms.

The design does not add a throughput gate, a benchmark baseline, a new renderer
feature, an asset pack, a networking oracle, or an artificial command path available
only to profiling. It also does not claim that a CPU capture measures GPU execution;
the existing GPU timestamp readings remain the instrument for that separate question.

## How it works

### Local, profiler-first workflow

The normal loop is:

1. Build the selected path in release mode. Release builds already carry the debug
   information needed for symbol attribution.
2. Run a short readiness pass that prints a scenario witness and fails if any
   requested subsystem was absent.
3. Record one steady-state or one intentionally changing segment with `samply`.
4. Load the saved capture in Samply when an interactive call tree is useful, then run
   `scripts/profile-cost-table.py` for a symbol-keyed inclusive and self-time table.
5. Select the highest attributable cost, make a focused change, and re-record the
   same scenario on the same machine. Compare witnesses before interpreting any
   timing difference.

The existing cost-table script weights samples by `threadCPUDelta` whenever the
capture supplies it. That avoids assigning blocked acquisition or presentation time
to the function at the top of a sampled stack. A fallback to sample-count weighting
is printed prominently by the script and must be retained as a lower-confidence
result, never silently treated as equivalent.

Capture files and their symbol sidecars are local artifacts. The scenario output
records enough metadata to identify an input, but it is not appended to the normal
cross-run benchmark history and is never used as a CI duration comparison.

### Deterministic scene specification

`HeavySceneSpec` is the proposed pure data model. It takes a named scenario, an
explicit seed, dimensions, density controls, and a camera plan, then produces a
deterministic sequence of ordinary setup commands and optional phase actions. It must
not read wall-clock time, enumerate the host filesystem, use a random global source,
or choose state names from an unordered map.

The specification is split into small builders so a workload remains meaningful on
its own:

| Builder | Owns | Required witness |
|---|---|---|
| `PaletteScene` | valid named block states and their per-section placement pattern | requested sections, distinct requested states per section, non-air cells |
| `TransparencyScene` | solid, cutout, and translucent arrangements visible from the camera plan | requested cells per category and visible-layer submissions |
| `LightScene` | emitters plus optional mutation waves | emitter count, changed cells, relight and remesh work |
| `LiquidScene` | source volumes, barriers, drains, and optional update waves | liquid cells, liquid meshes, scheduled updates when requested |
| `SignScene` | positions, text spans, colours, and glow state | sign count and prepared sign-text vertices |
| `BlockEntityScene` | supported state-backed block entities and their payloads | records received and block-entity draws |
| `EntityScene` | tagged, uniquely identified entity templates, positions, and metadata | spawned, extracted, and drawn entities by family |
| `ScheduledWorkScene` | production scheduled operations with deterministic due times | enqueued, executed, and remaining scheduled items |

`MixedScene` composes those builders without inventing a second construction route.
Its placement plan reserves non-overlapping volumes for each class, then deliberately
overlaps only the combinations whose rendering interaction is the subject, such as
text in front of translucent terrain. Every builder receives the same coordinate
frame and camera plan, making the mixed scene reproducible without duplicating its
placement arithmetic.

The emitted scene remains data, not an executable fixture hidden in a test module.
For the live path, a generated command file is the input consumed by the existing
RCON executor. For the headless path, the same specification calls the production
server-facing setup operations directly. The generator may be a checked-in helper,
but its generated scene must be deterministic and reviewable.

### Scenario catalogue

`--scenario` selects one of `palette`, `transparency`, `light`, `liquid`, `sign`,
`block-entity`, `entity`, `scheduled`, or `mixed`.

- `palette` fills each subject section with a nonuniform, repeated pattern of valid
  states. Distinct-state pressure is measured per section rather than across the
  whole world, because a diverse world made of individually uniform sections does
  not stress a section palette.
- `transparency` uses separate known-solid, known-cutout, and known-translucent
  materials. It places them in the camera frustum and on different depth planes so
  a scene cannot pass merely because all transparent geometry was culled.
- `light` has a static arm for shaded terrain and an update arm that changes sources
  after readiness. The update arm is necessary to profile client relight and remesh
  work; a scene fully installed before joining only proves the final lighting state.
- `liquid` has a static arm for liquid geometry and a scheduled-update arm for source,
  drain, and barrier changes. The latter waits for the server tick path rather than
  declaring a source block itself to be a liquid-simulation workload.
- `sign` varies text length, colour, glow, side, and placement while keeping boards
  within the camera plan. It measures prepared text geometry, not merely records
  stored in the world.
- `block-entity` uses only block entities whose state and payload are understood by
  the running client. It groups model families without replacing their normal
  frame-scoped sources.
- `entity` creates a stable grid of tagged, unique entities from already-supported
  families. It varies models, equipment, poses, colours, and display transforms while
  using frozen behaviour where movement would add nondeterministic simulation work.
- `scheduled` creates a bounded, deterministic due-work population through the real
  queues. It reports execution and remaining work separately so an empty queue does
  not masquerade as a fast queue.
- `mixed` combines all of the above at a documented density, then uses a fixed
  stationary view and a fixed orbit to distinguish preparation from view-dependent
  submission.

No scenario owns a performance target. Counts are readiness witnesses, not a score to
optimise toward.

### Release-built paths and command surface

The integrated-server path is a new native release executable, tentatively named
`heavy-scene-server`, owned beside the server's other executable targets. Its command
surface is:

```text
heavy-scene-server --scenario <name> --seed <u64> --scale <n>
                   --phase <ready|steady|mutate> --output <path>
                   [--ticks <n>] [--camera-plan <name>]
```

It starts the integrated server without a window, installs the scenario through
ordinary server APIs, drives the requested production ticks, and emits one structured
readiness record before the requested phase begins. `steady` measures already-settled
work; `mutate` applies the specification's phase actions and then advances the bounded
tick interval. `--ticks` is required for a scheduled or liquid-update phase so the
caller cannot accidentally profile setup only.

The client/GPU path uses the shipped `lodestone` release binary and extends the existing
live frame-benchmark runner rather than creating a second client launcher:

```text
cargo build --release -p lodestone-shell --bin lodestone
samply record --save-only --unstable-presymbolicate -o heavy-client.json.gz -- \
  ./target/release/lodestone --benchmark heavyweight \
  --heavy-scenario mixed --heavy-seed 1 --heavy-scale 1
python3 scripts/profile-cost-table.py heavy-client.json.gz
```

The corresponding runner command accepts `--workload heavyweight`, forwards the scene
selection and seed, starts the existing local creative server, submits the generated
ordinary commands through its existing RCON client, and preserves the present
warmup/stationary/moving lifecycle. Post-join phase actions are issued only at a named
segment boundary and are logged with that boundary. The client remains the shipped
binary, so the capture includes its real packet, session, meshing, render, and window
paths.

Both paths support `--output <path>` for newline-delimited JSON. They must not offer a
`--direct-renderer`, `--synthetic-packet`, or `--skip-witness` escape hatch. `--smoke`
may reduce density and duration, but still requires every selected witness to be
nonzero.

### Output and metadata

Each run writes a versioned record containing:

- executable kind, release profile, revision, platform, architecture, process id, and
  scenario specification hash;
- scenario name, seed, scale, requested phase, camera plan, and exact fixed-view or
  orbit segment;
- requested and installed counts for every builder;
- production-path counts, including world notifications, chunks or sections reached,
  scheduled-work enqueue/execute/remaining counts, and server tick counts where
  applicable;
- client-path counts, including received section work, meshed/uploaded sections, solid
  and translucent submissions, liquid geometry, sign-text vertices, block-entity
  draws, entity extraction/draw counts, and relight/remesh work; and
- status, elapsed setup and warmup annotations, and a precise failure reason when a
  phase did not start.

The profiler's existing CSV can continue to hold frame-phase measurements. The
heavyweight record supplies the missing workload witnesses, particularly the render
counts not currently present in the CSV. A consumer must join records by run id and
scenario hash, not by timestamp alone.

### Readiness and anti-vacuity rules

A scenario is ready only if its requested counts and the corresponding production
counts agree with the scenario's declared minimum shape. Examples:

- a palette scene needs nonzero meshed subject sections and the requested per-section
  distinct-state floor;
- a transparency scene needs nonzero visible submissions for each selected category;
- a light-update scene needs nonzero changed light cells and remesh submissions after
  its mutation boundary;
- a liquid-update scene needs both due work and executed work, not only liquid cells;
- a sign scene needs nonzero prepared sign-text vertices;
- a block-entity scene needs nonzero received records and draws;
- an entity scene needs matching unique spawned, extracted, and drawn counts by family;
  and
- a scheduled scene needs nonzero enqueue and execution counts.

Mixed readiness is the conjunction of its selected builders, not a single total. A
total draw count cannot prove that a sign, liquid, or block-entity path was present.
Every witness has a negative control in its focused test: remove or neutralize the
producer, observe the witness fail, then restore it. This makes an absence detector
credible rather than merely descriptive.

The scenario generator also reports the camera-space bounds of every workload. A
visible-workload check must establish that a representative subject is inside the
stationary view or orbit before profiling begins; a population outside culling range
is an unloaded benchmark, not a light workload.

### Failure handling

Missing release binaries, a missing local server asset, RCON refusal, an unavailable
GPU adapter, a failed fullscreen confirmation, missing Samply, an absent symbol
sidecar, and a readiness mismatch all fail with a nonzero exit status and a named
reason. None is reported as a successful empty run.

Scene setup remains bounded. Large clear/fill operations are partitioned below the
server command limit, cleanup selects only the scenario tag, and every generated entity
has a unique identity and position. A setup timeout prints the command/action index and
the partial witness; it does not continue into measurement.

The client runner distinguishes setup, join, warmup, and measured-segment failure. A
post-join mutation failure invalidates that trial instead of reusing its static arm.
If a capture lacks `threadCPUDelta`, cost-table output may still be inspected, but its
fallback banner and weighting mode must be included in the run metadata.

### macOS and worker threads

The live path keeps the current macOS display rules: select the hardware-built-in
display through the platform display identifier, require confirmed fullscreen, and
record the physical framebuffer. Monitor names and a generic primary-display flag are
not evidence that the intended display was used. Samply can record a same-user process
on macOS without a separate privilege step; a missing recorder is still a hard
precondition failure.

The main thread is not the whole workload. Native meshing, server work, and asset or
network helpers may consume CPU on worker threads while the window thread appears
quiet. Implementations must give profile-relevant worker groups stable, role-bearing
thread names and include the observed names in the readiness record. Run
`profile-cost-table.py` once for the main thread and once for every non-idle worker
role named by the record. Do not merge function indices or sample tables across
threads: attribution remains thread-local, and a main-thread table is not evidence
about worker cost.

## Staged implementation

1. Define `HeavySceneSpec`, deterministic generation, scenario hashes, the structured
   record schema, and pure generator tests. Add one tiny smoke form of every focused
   builder before raising density.
2. Add the headless integrated-server executable with palette, entity, and scheduled
   scenarios. Prove setup goes through normal mutations and ticks with a deliberate
   producer-removal control for each witness.
3. Add the live workload selection to the existing runner and shipped client command
   surface. Route generated scene commands through the current RCON validation path,
   then surface client-side witness counters in the frame output.
4. Add focused live scenarios for transparency, light updates, liquid updates, signs,
   and block entities. Keep static and mutation phases separate in the output.
5. Add `mixed`, its fixed camera plan, worker-role metadata, Samply invocation
   documentation, and a manual capture/readback rehearsal on macOS.
6. Only after the witnesses and captures are trustworthy, use profiles to choose
   implementation work. Do not convert a local observation into a timing gate.

## Testing and connectedness requirements

The generator gets deterministic unit tests: identical inputs produce byte-identical
commands and hashes, a changed seed changes only documented placement choices, and
invalid state or coordinate input fails before any server action. Structural tests check
per-section palette diversity, command-size partitioning, unique entity ids, cleanup
scope, and one required representative of every selected category.

Each focused scenario needs an integration control through its ordinary consumer:

- server scenarios prove mutations reach notifications, queues, and ticks;
- live scenarios prove accepted commands reach received world/entity records and their
  named mesh or draw witnesses; and
- a negative control removes a real producer and observes the associated witness fall
  to zero or below its declared minimum.

The mixed test confirms every selected focused witness independently; it must not use a
large total as a proxy. GPU-backed checks are explicitly opt-in and fail closed when
requested without an adapter or required local assets. The headless path remains
adapter-free so it can validate server-side production work independently.

Before landing an implementation stage, run its narrow crate tests, the relevant
all-target release compile, the comment-voice guard, and `cargo xtask islands` for each
changed crate. For packet or registry routing changes, also run the applicable
connectedness check. Tests must identify the live consuming call site or witness; a
builder tested only by parsing its own emitted data is a construction island.

## How to change it

Add a workload by extending `HeavySceneSpec` with a small builder, a declared witness
contract, a focused negative control, and a mixed-scene reservation rule. Do not add a
new direct renderer or a separate packet encoder for convenience. If a new workload
needs an asynchronous phase, make the phase boundary explicit in both paths and add its
action result to the record.

Add a witness at the actual consumption boundary, not merely where data is constructed.
For a renderer, that means geometry prepared or submitted; for simulation, it means
scheduled work executed or a normal world update consumed. Update the record schema,
its compatibility reader, and the corresponding focused control together.

If the capture format changes, retain the existing cost-table parser controls and
revalidate them before relying on a table. Keep all duration interpretation local to a
machine and an identical scenario hash.

## Configuration

- `--scenario`, `--seed`, `--scale`, `--phase`, `--ticks`, and `--camera-plan` select a
  deterministic workload and must be written to every record.
- `--output` selects a local JSONL destination. Captures, sidecars, screenshots, and
  records are local evidence and are not committed.
- `--benchmark heavyweight`, `--heavy-scenario`, `--heavy-seed`, and `--heavy-scale`
  are the proposed shipped-client controls. Existing warmup, stationary, and moving
  options remain the time segmentation controls.
- `samply record --save-only --unstable-presymbolicate` creates a local capture and
  symbol sidecar. `scripts/profile-cost-table.py <capture>` reads the capture; its
  `--thread` option selects the explicit worker-role pass.
- The live path continues to use the local creative-server configuration and the
  existing frame-profile dump setting. The headless path does not require a window or
  GPU adapter.

## Dependencies

- The integrated server's normal world mutation, notification, scheduled-work, and
  tick interfaces.
- The shipped client's normal session, packet ingestion, simulation, meshing,
  block-entity, entity, and renderer interfaces.
- The existing local creative-server launcher and RCON command executor for the live
  path.
- Release DWARF information, Samply, and `scripts/profile-cost-table.py` for local CPU
  attribution.
- On macOS, the platform display-identifier integration and a usable native GPU adapter
  for the live path.
