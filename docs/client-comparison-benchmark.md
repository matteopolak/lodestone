# Client comparison benchmark

## What it is

`scripts/client-comparison/` runs Java Edition 26.3 (vanilla and an optimized Fabric modset) and
Lodestone against the same fixed-seed world, pose and frame cap, and reports frame rate,
frame-time percentiles, CPU, RSS, GPU utilisation and package power over the measured window only.
`bench-suite.py` alternates the arms over several idle-gated rounds.

## How it works

Everything heavy (game jars, libraries, mods, the world fixture, run output) lives outside the
repo under `$LODESTONE_COMPARISON_ROOT`. The scripts only read and write there.

| script | role |
|---|---|
| `prepare-26.3.py` | downloads and hash-verifies the 26.3 client/server, assets, Fabric loader and mods; writes `manifest.json` and per-modset folders |
| `make-fixture.py` | boots the dedicated server on seed `-4172144997902289642`, pre-generates chunks around the bench pose, saves `world-fixture/` |
| `compile-adapter.sh` | builds the two bench adapter jars from `adapter/` (one needs Sodium, one stubs it out) |
| `run-java-263.py` | one Java trial: local server on a copy of the fixture, `--offlineDeveloperMode` client, adapter pins the pose and logs every present |
| `run-lodestone-263.py` | one Lodestone trial against the same server/fixture, using the showcase benchmark segments |
| `bench-suite.py` | rounds of all arms in rotated order, with samplers, into `runs/<prefix>-suite.json` |
| `rcon.py` | minimal RCON client used to drive the server |

Per trial, `bench-suite.py`:

1. waits until the 1-minute load average is below `--max-load` and no compiler/linker is above
   30% CPU for three consecutive 10 s checks (`--no-idle-wait` skips this; a timed-out wait is
   recorded as `idle: false`);
2. starts two system-wide samplers — `ioreg` accelerator "Device Utilization %" every 250 ms, and
   `powermetrics` (CPU/GPU mW, GPU active residency, frequency) every 500 ms;
3. runs the arm's driver, which also records process-tree CPU/RSS via
   [`client-resource-sampler.py`](client-resource-sampling.md);
4. cuts every series to the measured window — the adapter's `recording_start`/`recording_end`
   markers for Java, the `showcase.stationary` → `showcase.complete` segment transitions in the
   client log for Lodestone.

Frame figures come from each arm's own present log, so they count frames *produced*, not frames a
display showed. GPU utilisation and power are system-wide, not per-process; run on an otherwise
idle machine and compare arms only within one suite.

Nobody may use the desktop during a suite. A covered Lodestone window presents nothing (benchmark
mode ignores focus but not occlusion), and Java is run with `pauseOnLostFocus:false` so a focus
change cannot open the pause screen mid-run. A trial that presents no frames is rerun up to
`--retries` times (default 2) as `<trial>-retryN`; the failed attempt's directory is kept.

## How to change it

- New arm: add a branch in `bench-suite.py::run_trial` and `window_bounds`; the arm must log a
  start/end marker that can be put on the unix clock.
- New mod or version: edit `MODS` in `prepare-26.3.py` (Modrinth version ids) and rerun it, then
  `compile-adapter.sh` if the adapter's mixin targets moved.
- Frame-rate cap: `--fps N` reaches both drivers; 260 means unlimited on the Java side. Use a cap
  (e.g. 120) for an equal-work efficiency comparison, uncapped for throughput.
- The adapter sources under `adapter/` are a tool-read harness, like `oracle-java/`.

## Configuration

| variable | default |
|---|---|
| `LODESTONE_COMPARISON_ROOT` | `/Volumes/LodestoneScratch/java-26.3` |
| `LODESTONE_COMPARISON_JAVA` | the launcher's bundled `java-runtime-epsilon` |

`powermetrics` needs a passwordless sudo rule for `/usr/bin/powermetrics` only; without it the
power columns are empty and the rest still runs. Lodestone needs `--lodestone <release binary>`
and the vanilla assets at `.cache/benchmarks/vanilla-26.3-assets`.

```bash
python3 scripts/client-comparison/bench-suite.py --prefix uncapped --rounds 3 --lodestone target/release/lodestone
```

## Dependencies

Python 3 standard library, macOS `ioreg` and `powermetrics`, a Java 25 runtime, network access for
`prepare-26.3.py` (Mojang, Fabric meta, Modrinth). Video composition is separate:
[`client-comparison-video.md`](client-comparison-video.md).
