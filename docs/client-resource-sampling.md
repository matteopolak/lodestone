# Client resource sampling

## What it is

`scripts/client-resource-sampler.py` collects bounded process-tree RSS and observed CPU intervals
for explicitly identified native, browser, or optimized Java client processes. It retains raw
samples, process lifetime identities, attribution gaps, sampling overhead, and a summary without
turning unavailable GPU memory measurements into zero.

## How it works

`ProcessTreeSampler` accepts positive root PIDs and a required explanation of how they were
associated with the workload. Each snapshot runs `ps` with only PID, parent PID, birth time,
cumulative CPU time and RSS fields. It never requests process arguments, environment, or credentials.
The first snapshot pins each root to its PID and birth time. A missing root at that point stays
unassociated; a reused PID with a different birth time is excluded. Children use the same lifetime
key, and overlapping root trees are deduplicated. `ps` birth times have calendar-second resolution,
so reuse within the same second cannot be distinguished. Snapshots are observations, not atomic
system-wide transactions.

RSS is summed over the selected processes. Shared pages can appear in each process's RSS, so this
is not unique physical RAM consumption. `rss_bytes` requires every selected metric and a complete
observed tree; `rss_observed_bytes` separately retains available partial sums. Empty/missing
observations use `null`. A process limit, unavailable root, or malformed snapshot marks the result
incomplete.

CPU comes from cumulative process CPU time deltas for identical lifetimes observed in consecutive
snapshots. Divide those deltas by the actual monotonic interval between snapshot midpoints; 100%
means one core and values can exceed 100%. The collector does not use `ps` rolling `%cpu`.
`cpu_observed_interval_percent` can be partial; `cpu_complete` records whether all selected
processes had comparable observations without disappearing lifetimes. Initial/new processes have
no interval baseline. Disappeared, reparented, or exited processes lack a final measurement, and
short-lived children between snapshots can be missed. CPU timestamp resolution also quantizes
short intervals. Summary peaks retain these observation limits rather than claiming full CPU
accounting. The mean observed CPU percentage weights comparable intervals by their actual elapsed
time; it remains partial when an interval lacks some process attribution.

Opt-in macOS retired counters use `proc_pid_rusage` with `RUSAGE_INFO_V4` for cumulative
instructions and CPU cycles across every thread of each selected process. These are CPU work,
not elapsed time, GPU work, or instructions per generated column. Deltas require consecutive
successful observations with identical process start timestamps and nondecreasing counters.
Zero/unavailable counters, access errors, new lifetimes, and missing observations leave gaps;
they are not reported as zero work. The first observation supplies only a baseline. Summaries
retain partial totals and the number of complete intervals. Disabled collection makes no
additional process queries.

The report always leaves dedicated VRAM and resident GPU bytes `null` with an explanation. On
Apple Silicon the host has unified memory, and `ps` cannot isolate resident GPU memory. On other
Unix systems this collector also lacks a supported VRAM instrument. An optional application
allocation estimate is stored separately with its type, byte count and explicit source. It is
never substituted for measured residency or RSS.

## Configuration

Use PIDs identified by the launcher or by a verified browser process association:

```bash
python3 scripts/client-resource-sampler.py \
  --root-pid 12345 --association 'Java process PID returned by this controlled launcher' \
  --duration 60 --interval 1 --max-samples 600 --max-processes 64 \
  --output captures/java-resources.json
```

The PID is an example, not a discovery mechanism. Multiple `--root-pid` flags allow explicitly
associated browser renderer/GPU processes. A browser root can include unrelated tabs and a shared
GPU process; describe the selected scope honestly and use a dedicated browser session for a
controlled comparison. Sampling a launcher wrapper also includes its selected descendants;
wrappers and profilers must be named in the association note.

The interval is at least 0.1 seconds. The CLI duration is positive and at most 3600 seconds; hard
bounds are 10000 samples and 1024 selected processes per snapshot, with their product limited
to 65536 retained process observations. Defaults are 60 seconds, one second, 600 samples, and
64 processes. Only the previous snapshot is retained for lifetime matching outside the bounded
sample storage. The report refuses overwrite. Reaching a sample/process
bound records truncation. Snapshot errors, missing/exited identities and roots are retained.
The CLI stops when the duration ends, the sample bound is exhausted, or no selected roots remain.
`snapshot_overhead_seconds` times the `ps` call; `sampler_overhead_seconds` also includes selection
and sample bookkeeping, including optional counter reads but excluding report serialization.
Peaks are observations at the sampling rate and can miss short bursts.

For a foreground harness, load the script as a module, instantiate `ProcessTreeSampler` immediately
after launch with the known PID, call `sample_if_due()` inside the existing poll loop, and finish
with `write_report(path, stop_reason="client_exit")`. `take_sample()` collects immediately.
Both return a sample dictionary, or `None` if not due/the bound is exhausted. Optional
`gpu_allocation_estimate={"bytes": 1234, "source": "<actual allocation counter>"}` can be passed
to either collector method when the application provides such an estimate. Injected snapshot
and monotonic clock callables support deterministic tests.

Add `--retired-counters` on macOS, or pass `retired_reader=mac_retired_counters` to the
programmatic collector. Unsupported systems retain explicit counter errors and `null` totals.
Counters cover only the sampled interval, not the process's entire launch; query overhead and
missing final observations remain visible in the report.

`client-frame-benchmark.py` consumes this collector in its existing foreground
trial loop. It samples immediately after launch and on observed exit, retains
`resources.json` with `--artifact-dir`, and records the whole-launch summary beside
frame results. It does not align resource samples to individual frame segments.

## How to change it

Keep lifetime matching and missing-value handling in `ProcessTreeSampler.take_sample`. Extend
typed memory fields only when an actual platform instrument supplies them, retaining its scope
and method. Keep browser PID association explicit. Resource samples and frame measurements need
matching workload intervals and clocks before forming a comparative claim; a preliminary run on
a busy machine remains preliminary.

Run `python3 scripts/test-client-resource-sampler.py`. Synthetic controls check timestamp/CPU
arithmetic, PID reuse, absent metrics, process/sample limits, shared RSS accounting and separate
GPU allocation estimates, plus counter ABI, interval arithmetic and missing-counter controls.
They do not run clients or claim measured game resources.

## Dependencies

Python 3 standard library and Unix/macOS `ps` supporting `pid`, `ppid`, `lstart`, `time` and `rss`.
Optional retired counters use the macOS `libproc` system library through `ctypes`.
No third-party packages, GPU drivers, downloads, or process command-line inspection are required.
