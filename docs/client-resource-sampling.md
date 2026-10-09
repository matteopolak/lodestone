# Client resource sampling

## What it is

`scripts/client-resource-sampler.py` collects bounded process-tree RSS and observed CPU intervals for explicitly identified native, browser or Java client processes. It retains raw samples, process lifetime identities, attribution gaps, sampling overhead and a summary, and never turns an unavailable GPU memory measurement into zero.

## How it works

- `ProcessTreeSampler` takes positive root PIDs plus a required note on how they were associated with the workload. Each snapshot runs `ps` for PID, parent, birth time, cumulative CPU time and RSS only (no arguments or environment).
- The first snapshot pins each root to PID and birth time. A missing root stays unassociated; a reused PID with a different birth time is excluded. Children use the same lifetime key and overlapping trees are deduplicated. Birth times have one-second resolution, so reuse within a second is indistinguishable.
- **RSS** is summed over selected processes and double-counts shared pages, so it is not unique physical RAM. `rss_bytes` needs a complete observed tree; `rss_observed_bytes` keeps partial sums; missing observations are `null`. A process limit, unavailable root or malformed snapshot marks the result incomplete.
- **CPU** is the delta of cumulative CPU time for identical lifetimes in consecutive snapshots, divided by the monotonic interval between snapshot midpoints (100% = one core; values can exceed it). It does not use `ps` rolling `%cpu`. `cpu_observed_interval_percent` may be partial; `cpu_complete` says whether every selected process had a comparable observation. New, vanished or reparented processes and short-lived children between snapshots leave gaps. The mean weights intervals by elapsed time.
- **Retired counters** (opt-in, macOS): cumulative instructions and cycles per process via `proc_pid_rusage` (`RUSAGE_INFO_V4`). These measure CPU work, not time, GPU work or per-column cost. Deltas need consecutive successful observations of the same start timestamp with nondecreasing counters; zero, unavailable or new-lifetime counters leave gaps, never zeros. Disabled collection makes no extra queries.
- Dedicated VRAM and resident GPU bytes are always `null` with an explanation (unified memory on Apple Silicon, no supported instrument elsewhere). An optional application allocation estimate is stored separately with type, bytes and source, and never substituted for RSS.

## How to change it

- Keep lifetime matching and missing-value handling in `ProcessTreeSampler.take_sample`. Add typed memory fields only when a real platform instrument supplies them, with its scope and method. Keep browser PID association explicit.
- Resource samples and frame measurements need matching workload intervals and clocks before a comparative claim; a run on a busy machine stays preliminary.
- Tests: `python3 scripts/test-client-resource-sampler.py` (timestamp/CPU arithmetic, PID reuse, absent metrics, limits, shared RSS, allocation estimates, counter ABI and gaps) and `python3 scripts/test-summarize-client-resources.py` (irregular intervals, excluded boundary work, clock errors). Neither runs a client.
- For a foreground harness: build `ProcessTreeSampler` right after launch, call `sample_if_due()` in the poll loop, finish with `write_report(path, stop_reason="client_exit")`. `take_sample()` samples immediately; both return a sample or `None`. Pass `gpu_allocation_estimate={"bytes": ..., "source": ...}` when the app has one. `client-frame-benchmark.py` already does this (saving `resources.json` with `--artifact-dir`) but does not align samples to frame segments.
- `scripts/summarize-client-resources.py` summarises a window given in the collector's monotonic clock (not video or client frame time). It includes whole intervals only (no proration), reports the actual covered fraction, leaves the source report unchanged and refuses overwrite.

## Configuration

```bash
python3 scripts/client-resource-sampler.py \
  --root-pid 12345 --association 'Java process PID returned by this controlled launcher' \
  --duration 60 --interval 1 --max-samples 600 --max-processes 64 \
  --output captures/java-resources.json

python3 scripts/summarize-client-resources.py --report captures/resources.json \
  --start 12000.25 --end 12010.25 --output captures/stationary-resources.json
```

- Several `--root-pid` flags allow associated browser renderer and GPU processes. A browser root can include unrelated tabs; use a dedicated session and name wrappers and profilers in the association note.
- Interval at least 0.1 s; duration at most 3600 s; hard bounds 10000 samples, 1024 processes per snapshot, 65536 retained observations. Defaults: 60 s, 1 s, 600 samples, 64 processes. Reaching a bound records truncation; output overwrite is refused.
- `snapshot_overhead_seconds` times the `ps` call; `sampler_overhead_seconds` adds selection and bookkeeping (including counter reads, excluding serialization). Peaks can miss bursts shorter than the interval.

## Dependencies

Python 3 standard library and Unix/macOS `ps` with `pid`, `ppid`, `lstart`, `time`, `rss`; optional counters use macOS `libproc` via `ctypes`.
