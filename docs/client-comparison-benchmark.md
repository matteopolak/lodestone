# Client comparison benchmark

## What it is

`scripts/client-comparison/` runs Java Edition 26.3 (vanilla and an optimised Fabric modset) and Lodestone against the same fixed-seed world, pose and frame cap, reporting frame rate, frame-time percentiles, CPU, RSS, GPU utilisation and package power over the measured window only. `bench-suite.py` alternates the arms over several idle-gated rounds.

## How it works

Everything heavy (jars, libraries, mods, the world fixture, run output) lives outside the repo under `$LODESTONE_COMPARISON_ROOT`.

| script | role |
|---|---|
| `prepare-26.3.py` | downloads and hash-verifies the 26.3 client and server, assets, Fabric loader and mods; writes `manifest.json` and per-modset folders |
| `make-fixture.py` | boots the dedicated server on seed `-4172144997902289642`, pre-generates chunks around the bench pose, saves `world-fixture/` |
| `compile-adapter.sh` | builds the two adapter jars from `adapter/` (one needs Sodium, one stubs it) |
| `run-java-263.py` | one Java trial: local server on a fixture copy, `--offlineDeveloperMode` client, adapter pins the pose and logs every present |
| `run-lodestone-263.py` | one Lodestone trial on the same server and fixture, using the showcase benchmark segments |
| `bench-suite.py` | rotated rounds of all arms with samplers into `runs/<prefix>-suite.json` |
| `rcon.py` | minimal RCON client |

Per trial, `bench-suite.py`:
1. waits for a 1-minute load average under `--max-load` and no compiler or linker above 30% CPU for three consecutive 10 s checks (`--no-idle-wait` skips; a timeout is recorded `idle: false`);
2. starts system-wide samplers: `ioreg` accelerator "Device Utilization %" every 250 ms and `powermetrics` (CPU/GPU mW, GPU residency, frequency) every 500 ms;
3. runs the arm's driver, which also records process-tree CPU and RSS via [client-resource-sampling](client-resource-sampling.md);
4. cuts every series to the measured window: the adapter's `recording_start`/`recording_end` markers for Java, `showcase.stationary` to `showcase.complete` transitions in the client log for Lodestone.

Frame figures come from each arm's own present log (frames produced, not shown). GPU utilisation and power are system-wide; compare arms only within one suite on an otherwise idle machine, with nobody using the desktop. A covered Lodestone window presents nothing (benchmark mode ignores focus, not occlusion); Java runs with `pauseOnLostFocus:false`. A trial with no frames reruns up to `--retries` (default 2) as `<trial>-retryN`, keeping the failed directory. If a Lodestone attempt skipped every frame as paced, its window was never visible and the suite exits, since Java would still draw into a hidden window and neither number would mean anything.

## How to change it

- New arm: a branch in `bench-suite.py::run_trial` and `window_bounds`, with a start/end marker that maps to the unix clock.
- New mod or version: edit `MODS` in `prepare-26.3.py` (Modrinth version ids), rerun it, and `compile-adapter.sh` if mixin targets moved.
- `--fps N` reaches both drivers (260 means unlimited for Java): cap (for example 120) for equal-work efficiency, uncapped for throughput.
- `adapter/` is a tool-read harness like `oracle-java/`.

## Configuration

`LODESTONE_COMPARISON_ROOT` (default `/Volumes/LodestoneScratch/java-26.3`); `LODESTONE_COMPARISON_JAVA` (default the launcher's bundled `java-runtime-epsilon`). `powermetrics` needs a passwordless sudo rule for `/usr/bin/powermetrics` only, otherwise the power columns are empty. Lodestone needs `--lodestone <release binary>` and vanilla assets at `.cache/benchmarks/vanilla-26.3-assets`.

```bash
python3 scripts/client-comparison/bench-suite.py --prefix uncapped --rounds 3 --lodestone target/release/lodestone
```

## Dependencies

Python 3 standard library, macOS `ioreg` and `powermetrics`, Java 25, and network access for `prepare-26.3.py` (Mojang, Fabric meta, Modrinth). Video composition is separate: [client-comparison-video](client-comparison-video.md).
