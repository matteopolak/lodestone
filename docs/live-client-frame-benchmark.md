# Live client frame benchmark

## What it is

The live client frame benchmark measures Lodestone’s real fullscreen client while it is joined to a Java 26.2 server. It records frame intervals and CPU/GPU phase timings for a normal-terrain workload and a dense render showcase, then summarizes stationary and moving segments separately.

This is the reproducible workload used for client performance investigations. The synthetic `frame_profile` criterion bench remains an instrumentation control; it is not a substitute for these live measurements.

## How it works

`scripts/client-frame-benchmark.py` starts the matching Java oracle and does not return until every client child has exited. The runner temporarily launches Java with `view-distance=25`, which lets Lodestone’s render distance 24 request receive its one-column meshing buffer ring. It restores the persistent `server.properties` immediately after startup, so later oracle launches keep their previous configuration.

Each trial gets a new `LODESTONE_DATA_DIR` and offline username. This prevents an old player file, persisted graphics option, or selected Microsoft account from changing the workload. The client launches borderless fullscreen with render distance 24, no frame cap, `wgpu::PresentMode::AutoNoVsync`, and deterministic input. Focus loss still releases the cursor but does not pause or background-throttle a benchmark.

On macOS the benchmark does not trust display names or primary-monitor status. Winit can call every attached panel `Monitor #…`, and either external monitor can be the desktop primary. The shell maps each winit monitor to its CoreGraphics display ID and accepts only the display for which `CGDisplayIsBuiltin` is true, then enables winit’s game-fullscreen presentation so the menu bar and Dock do not cover the run. The startup log records the native ID, panel bounds, fullscreen state, and physical drawable size. The runner fails closed unless it sees both the hardware-built-in selection marker and `fullscreen=true`; it parses and records the actual framebuffer because laptop panel modes differ. On this MacBook the measured drawable is 3024×1898 physical pixels (the notch-safe fullscreen content region) inside the built-in panel’s 3024×1964 bounds.

The benchmark clock begins only after `SessionPhase::Connected`:

1. `warmup` settles the joined world. Terrain also enables creative flight.
2. `stationary` holds a fixed view for 30 seconds by default.
3. `moving` flies forward through terrain or makes one time-integrated 360-degree showcase orbit for 60 seconds by default.
4. `complete` logs a completion marker and exits cleanly.

`LODESTONE_FRAME_PROFILE_DUMP` writes one CSV row per finalized frame. `frame_interval_ms` is start-to-start wall time, and `segment` is captured when the frame begins so a transition cannot relabel the previous frame. Empty phase cells mean the phase did not run; the summarizer never averages them as zero.

The showcase is `scripts/benchmark-scenes/showcase.txt`. It resets its own 64×32×64 plot and includes 24 signs, 16 player heads, 16 patterned banners, 16 mapped item frames, 12 equipped armour stands, 24 sheep plus other mobs, text/item/block displays, block entities, translucent blocks, and repeating particle command blocks. Summoned entities carry the `lodestone_benchmark` tag so a rerun removes exactly its own entities.

## Running it

Build the client once, then run the short end-to-end gate:

```bash
cargo build --release -p lodestone-shell --bin lodestone
just bench-client-smoke
```

The smoke run uses one showcase trial with 2 seconds warmup, 2 seconds stationary, and 3 seconds moving. Full comparable runs use three trials:

```bash
just bench-client-terrain
just bench-client-showcase
```

Record one non-comparable CPU sample after the ordinary trials identify the worse workload:

```bash
python3 scripts/client-frame-benchmark.py --workload showcase --samply
```

The profile is saved under `bench-results/profiles/`. `--samply` runs one full session and does not append its timing to the comparable JSONL history because sampling changes the workload.

The RSS printed for a `--samply` run belongs to the Samply parent process, not
the profiled client child, and is therefore not a client-memory measurement.
Use the three ordinary trials for RSS comparisons.

The runner must stay in the foreground. It uses `subprocess.Popen.poll()` only so it can sample the child’s RSS and enforce a failure deadline; it does not detach the benchmark or rely on a later wake-up. Client stdout and stderr go directly to a file rather than an unread pipe.

## Reading the results

The runner prints, for each measured segment:

- frame count and p50/p95/p99 frame interval using observed nearest-rank percentiles;
- counts over the 16.67 ms and 33.3 ms budgets;
- means for every CPU phase/subphase that actually ran;
- client RSS at the first sample, peak sample, and last sample;
- min/median/max trial spread for p50 frame interval.

Comparable records are appended to `bench-results/live_frame_profile.jsonl`, one object per trial and measured segment. Each object includes the git SHA, OS/machine, architecture, release profile, scene, segment, trial, binary, render distance, parsed fullscreen framebuffer, durations, RSS, percentiles, budget misses, and phase means.

The raw temporary CSV has this leading schema:

```text
frame,frame_interval_ms,segment,setup,sim_tick,mesh_upload,acquire,...
```

It continues with the world encode and HUD subphase columns documented in `docs/frame-profiling.md`. A skipped phase is an empty cell, not `0.0000`.

The CSV’s frame interval includes everything that delayed the next redraw, including CPU work, GPU/back-pressure visible at acquire or present, compositor scheduling, and OS noise. The phase columns are CPU wall-clock spans. Wgpu timestamp queries are useful live controls but are not currently emitted as row-correlated CSV columns; do not subtract CPU phase means from frame interval and label the remainder “GPU”. Use the counts beside timings, a Samply CPU profile, and—when the evidence points GPU-side—an Xcode/Metal capture or a purpose-built timestamp dump.

## How to change it

Change workload choreography in `crates/lodestone-shell/src/app/benchmark.rs`. Keep it a pure state machine: window, GPU, network, and filesystem operations belong in the caller. If segment boundaries or input change, update its explicit-`Instant` unit tests first.

Change benchmark policy or live wiring in `crates/lodestone-shell/src/app.rs` and `app/redraw.rs`, `app/session.rs`, or `app/lifecycle.rs`. The macOS hardware selector depends on `winit::platform::macos::MonitorHandleExtMacOS` and the target-only `objc2-core-graphics` dependency; do not replace it with monitor-name matching. Ordinary play must continue through the option-driven branches. Run the focused policy tests and the version-free seam after edits:

```bash
cargo test -p lodestone-shell --lib policy_ -- --nocapture
cargo test -p lodestone-shell --lib app::benchmark -- --nocapture
cargo check -p lodestone-shell --no-default-features
```

Change the dense Java scene by editing `scripts/benchmark-scenes/showcase.txt`. Commands must be valid standalone RCON commands; comments start with `#`. Keep summoned entities tagged, and preserve the structural coverage gate:

```bash
cargo test -p lodestone-shell --test frame_benchmark_showcase_fixture -- --nocapture
```

Change summary math, validation, child management, or output fields in `scripts/client-frame-benchmark.py`, then run:

```bash
python3 scripts/test-client-frame-benchmark.py
```

Do not weaken completion, hardware-built-in fullscreen, physical-size parsing, workload-label, or non-empty-segment validation to make a failed run recordable. A partial run is diagnostic evidence, not a comparable benchmark trial.

## Configuration

The client’s opt-in flags are:

```text
--benchmark terrain|showcase
--benchmark-warmup SECONDS
--benchmark-stationary SECONDS
--benchmark-moving SECONDS
```

`--benchmark` forces a live windowed connection. Defaults are 20/30/60 seconds. The Python runner additionally accepts `--trials N` (default `3`), `--smoke`, `--samply`, and `--binary PATH`. `--smoke` forces one 2/2/3-second trial; `--samply` forces one sampled full trial.

Canonical server endpoints are terrain `127.0.0.1:25580` with RCON `:25581`, and showcase `127.0.0.1:25570` with RCON `:25571`. Both use the local RCON password already defined by the oracle scripts. The client process receives a temporary `LODESTONE_DATA_DIR`, `LODESTONE_FRAME_PROFILE_DUMP`, and `RUST_LOG=frame_profile=info,frame_benchmark=info,warn`.

## Dependencies

- A release Lodestone binary, normally `target/release/lodestone`.
- Apple’s `container` runtime and the existing oracle worlds under `.cache/mc/terrain` and `.cache/mc/creative`.
- The Java 26.2 `server.jar` already used by `scripts/live-oracles/terrain.sh` and `creative.sh`.
- Python 3 standard library only for ordinary runs.
- `samply` on `PATH` only when `--samply` is requested.
- On macOS, CoreGraphics through the target-only `objc2-core-graphics` crate for authoritative built-in-panel selection.
- A native GPU/window session. GPU timestamp availability depends on the selected adapter’s `TIMESTAMP_QUERY` feature.
