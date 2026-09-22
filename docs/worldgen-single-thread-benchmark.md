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
request-session initialization are reported independently.

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
The session remainder includes production admission, materialization, ledger,
publication, and diagnostic observer overhead; compare it with the uninstrumented
`production_request` total rather than treating it as a generator stage.
The same feature reports four non-overlapping lifecycle regions: replay-context
preparation, requested-target mutable advancement, sparse-padding completion, and
packet-snapshot finalization. These regions explain the session remainder without
changing the default build; packet light and encoding remain the separate
`light_encode` metric.

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

## Configuration

Run the ignored test with `LODESTONE_WORLDGEN_WORKERS=1`. Optional variables are
`LODESTONE_WORLDGEN_BENCH_SEED` (default `42`) and
`LODESTONE_WORLDGEN_BENCH_COLUMNS` (default `8`).
`LODESTONE_WORLDGEN_BENCH_BATCH` defaults to `2`, matching the minimum
one-worker join window. For a bounded debug smoke run,
set the column count to `1`; release runs should be performed on an otherwise
quiet machine. The dispatcher override is a benchmark control, not a production
default. `LODESTONE_WORLDGEN_BENCH_LAYOUT=square` arranges a perfect-square
column count as a compact region; the default line layout stresses dependency
halo turnover. Report the selected layout because the two answer different
questions. The hardware profiler accepts positional `seed`, `columns`,
`batch_size`, and `layout` arguments and launches this test binary directly; it
does not profile the raw generator example. Set
`LODESTONE_WORLDGEN_PROFILE_DRY_RUN=1` to print the resolved command without
requiring macOS Instruments.
`LODESTONE_WORLDGEN_XCTRACE_TEMPLATE` selects the Instruments template and
`LODESTONE_WORLDGEN_XCTRACE_EVENTS` optionally supplies its comma-separated
counter names to the summary when the TOC uses ambiguous multi-word labels.
For the stage decomposition, add the server feature
`worldgen-stage-pmu`; it requires macOS retired-instruction counters and is
diagnostic-only. The same seed, coordinates, worker count, and batch size must
be used for the uninstrumented total and the PMU run.

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
