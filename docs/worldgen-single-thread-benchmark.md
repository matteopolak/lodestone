# Strict single-thread world-generation benchmark

## What it is

`strict_single_thread_worldgen` is an ignored production benchmark for the
Overworld. Its `production_request` metric measures one genuinely serial
generation worker through the server's request/session boundary, including the
full terrain, structure, feature, and ordered mutable stages.

## How it works

The test requires `LODESTONE_WORLDGEN_WORKERS=1`, which must be set before the
process-wide dispatcher is initialized, and the profiler runs the test binary
with `--test-threads=1`. It first runs a fixed `#[inline(never)]` calibration
kernel, then measures one cold request and immediately repeats that coordinate
as the retained-target negative control. It then generates a contiguous set of
new targets through the production batch boundary. Request timing stops before
result extraction, cloning, or assertions. A separate fresh
`OverworldChunkSource::column` phase measures direct generator work, while light
and packet encoding run afterward as a separate phase. On macOS the report uses
process resource counters for retired instructions and cycles, and asserts both
deltas are nonzero; elapsed time is diagnostic context. Source construction and
request-session initialization are reported independently. On macOS the
production phase also reports current physical footprint and process peak
footprint after all requests complete; compare separate fresh processes with
the same target set when assessing batch-width memory cost. The process peak
includes source construction and is not an exact count of retained region bytes.
The `first_output` line measures from the start of sustained generation to the
first completed callback in cohort mode, or the first returned batch otherwise.
It does not include packet encoding, meshing, or presentation.

After measurement, `output_checksum` hashes canonical block IDs, every 3D biome
cell, and the three client heightmaps separately. These non-cryptographic checksums
use the same Rust toolchain for before/after comparisons and ignore palette layout.
They do not cover structure sidecars, block entities, or lighting, so they complement
rather than replace focused parity checks. A changed-block control verifies that
the block checksum responds to a mutation.

The counter reads occur once immediately before and once immediately after each
measured closure; no PMU call or event allocation is placed in the generation
loop. The fixed `pmu_calibration` kernel proves that the selected xctrace or
process counter path observes work, while `retained_target_control` proves that
an immediate repeat takes the existing-target path instead of regenerating.
The hardware summary preserves every vector exposed by the selected xctrace
template. If the template adds cache-miss or memory-traffic events, the TOC
names them in the summary (or pass an explicit comma-separated `--events` list
when a template uses multi-word names); unavailable events are reported as
unavailable rather than inferred from the software counters.

The opt-in `worldgen-stage-pmu` feature adds calibrated retired-instruction and
cycle scopes to outermost worldgen stages. It reports terrain-prefix, features,
finalization, and session-remainder totals, followed by each stage's IPC. The
scopes are installed only by this ignored harness, subtract one measured PMU
read from each interval, and do not enable the ordinary generation counters.
`stage_region_pmu` assigns each stage interval to its deepest active region
and asserts that the assigned totals equal the stage totals. This distinguishes
terrain work during admission from feature, top-layer, and output interning
during mutable completion without summing overlapping region scopes.
The session remainder includes production admission, materialization, ledger,
publication, and diagnostic observer overhead; compare it with the uninstrumented
`production_request` total rather than treating it as a generator stage.
The same feature reports lifecycle regions for admission, replay-context
preparation, requested-target mutable advancement, sparse-padding completion,
packet-snapshot finalization, shaped-prefix import, checkpoint capture and
export, session hydration, and ledger publication. A zero region row without
an installed scope does not prove that the corresponding work is absent.
The packet-snapshot region also reports its output snapshot, neighbour
construction, final packet boundary, state-machine reconstruction, and
post-wavefront settlement advancement as nested scopes. The latter includes
resume-output, FEATURES commit, and top-layer commit scopes.
FEATURES commit separates source transactions, snapshot and sidecars, stage
publication, and foreign-winner settlement.
Admission overlaps the terrain-prefix stage counters. Prefix import is nested
inside mutable-target advancement, so those two rows are inclusive and must not
be added together. `direct_transition_mirror` measures the successful direct
feature result's local-write replay, including canonical-winner bookkeeping;
it is an upper bound on work a direct-only path could remove. The checkpoint
and publication rows isolate their named operations from the session remainder.
These diagnostics are absent from the default build; packet light and encoding
remain the separate `light_encode` metric.
With `lodestone-worldgen/gen-counters`, `gen_work` also counts structure piece
bounding-box checks and reached pieces, separating placement traversal from
height probing.

The production prefix carries its sixteen surface biomes as `BiomeRef` values with
their snow-temperature results. Surface and top-layer consumers keep that typed
handoff through the final output; resource-name parsing and string callbacks remain
only at configuration or compatibility boundaries. Surface measurements therefore
include the typed callback contract used by production generation.

## How to change it

Keep the cold target and contiguous sustained coordinates distinct. A benchmark
that requests completed targets measures retention lookup rather than generation.
If the request lifecycle changes, keep the calls through
`ChunkSource::request_generation` and `ChunkSource::request_generation_batch`
so the benchmark cannot silently bypass production stage commits. Add a separate
phase when a new post-generation cost needs attribution; do not fold it into the
generation number. Keep extraction and correctness checks after each measured
request. The `production_request`, `session_initialization`, `fresh_column`,
`retained_target_control`, `light_encode`, and `pmu_calibration` metric labels
are part of the output schema; every request line also reports its actual
`batch_size` and `layout`.

The `generator_leases` line is a diagnostic boundary for the staged worldgen
store. `batch_opens` must not exceed the number of production admission groups;
an admission whose shaped products are already retained does not open another lease.
`opens` also includes narrow nested structure reads, so it is intentionally
larger and must not be interpreted as a count of production batches.

Batch width is a first-order control for dependency-halo reuse. On `437a559ad`,
the same seven-column line with one native worker measured 11.43 columns/s,
1.892 billion instructions/column, and 372.6 million cycles/column at width 2;
16.39 columns/s, 1.316 billion instructions/column, and 264.3 million
cycles/column at width 4; and 20.30 columns/s, 1.055 billion instructions/column,
and 214.2 million cycles/column at width 7. The reduction is repeated
terrain-prefix and admission work, not a change in generated output. These are
diagnostic controls: increasing a browser window also delays the next emitted
column until the batch future completes, so the browser scheduler keeps its
separate first-emit and memory contract until browser measurements justify a
change.

Follow-up native one-worker line controls on the clean production path reached
24.25 columns/s at width 16 (875 million instructions and 177.5 million cycles
per column) and 24.65 columns/s at width 32 (831 million instructions and
174.4 million cycles per column). The plateau shows that larger cohorts reduce
halo overhead but do not approach the 200-column target; the remaining work is
inside the terrain and mutable stages rather than the scheduler window alone.

Topology must also match when comparing results. A production-only fresh
64-column square with batch width 64 measured 401 million instructions and
105.5 million cycles per output, versus 866 million and 179.3 million for a
16-column line with batch width 16 using the same seed and one worker. The
square amortizes its dependency perimeter over more outputs; this is a
different workload, not an optimization result. Its snapshot-finalization
region alone consumed 17.7 million cycles per output, making column-copy and
region-wide bookkeeping costs important alongside density evaluation.

## Configuration

Run the ignored test with `LODESTONE_WORLDGEN_WORKERS=1`. Optional variables are
`LODESTONE_WORLDGEN_BENCH_SEED` (default `42`) and
`LODESTONE_WORLDGEN_BENCH_COLUMNS` (default `8`).
`LODESTONE_WORLDGEN_BENCH_BATCH` defaults to `2`, matching the minimum
one-worker batch control. Set `LODESTONE_WORLDGEN_BENCH_COHORT=1` to drive the
production streaming cohort boundary with the same width and coordinates. For
a bounded debug smoke run,
set the column count to `1`; release runs should be performed on an otherwise
quiet machine. The dispatcher override is a benchmark control, not a production
default. `LODESTONE_WORLDGEN_BENCH_LAYOUT=square` arranges a perfect-square
column count as a compact region; the default line layout stresses dependency
halo turnover. `ring` first generates the centre as a separate measured
request, then takes the next outward-ring coordinates in join-stream order.
Its sustained metric isolates the production window after first-chunk delivery.
Report the layout because the shapes answer
different questions. The hardware profiler accepts positional `seed`, `columns`,
`batch_size`, and `layout` arguments and launches this test binary directly; it
does not profile the raw generator example. Set
`LODESTONE_WORLDGEN_BENCH_COMPARE_BATCH` to run the same coordinates on a fresh
source with another batch width after the measured phase. It prints all three
output checksums, the exact block-difference count, and at most 32 differing
cells. Comparison work is excluded from the reported production rate.
Set
`LODESTONE_WORLDGEN_PROFILE_DRY_RUN=1` to print the resolved command without
requiring macOS Instruments.
`LODESTONE_WORLDGEN_XCTRACE_TEMPLATE` selects the Instruments template and
`LODESTONE_WORLDGEN_XCTRACE_EVENTS` optionally supplies its comma-separated
counter names to the summary when the TOC uses ambiguous multi-word labels.
For the stage decomposition, add the server feature
`worldgen-stage-pmu`; it requires macOS retired-instruction counters and is
diagnostic-only. The same seed, coordinates, worker count, and batch size must
be used for the uninstrumented total and the PMU run.
The sustained-batch throughput and PMU lines are emitted only after every
request has returned a column; a failed batch is a diagnostic, not a rate.

`scripts/profile-worldgen-hardware.sh` exports the xctrace TOC and the target
stdout to `scripts/summarize-xctrace-counters.py`. The summary combines
process-wide retired instructions/cycles (and any extra hardware events) with
the benchmark's phase-labeled deltas, so `production_request`, `fresh_column`,
`light_encode`, the fixed calibration, and the retained-target negative control
remain distinguishable. Use a template that actually exposes the desired event
before interpreting an extra column; software cache and representation counters
are not hardware-cache measurements.

## Dependencies

The benchmark uses `lodestone-server`'s public production source and session
request APIs, the 26.2 server protocol encoder, and macOS `proc_pid_rusage`
when available. The profiler uses `xcrun xctrace` and the focused script tests
use only the Python standard library. It has no persistence or network
dependency.
