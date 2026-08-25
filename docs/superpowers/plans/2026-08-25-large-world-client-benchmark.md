# Large-world Client Benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reproduce the real-server 7–8 ms world and 4–5 ms HUD phases with an official large multiplayer world, attribute them, fix the dominant client bottleneck, and measure the result again.

**Architecture:** A safe installer provisions an untracked Hermitcraft Season 10 cache, a dedicated Java oracle hosts it, and the existing live runner gains a `megaworld` workload plus explicit F3-open/F3-closed arms. Existing per-frame CPU columns and GPU pass timestamps provide timing; the runner adds count summaries before any renderer change is selected.

**Tech Stack:** Python 3 standard library, Bash, Java 26.2 server, Apple `container`, Rust/winit/wgpu, Lodestone frame profiler.

---

### Task 1: Provision the pinned large world

**Files:**
- Create: `scripts/install-client-benchmark-world.py`
- Create: `scripts/test-install-client-benchmark-world.py`
- Create: `scripts/live-oracles/megaworld.sh`
- Modify: `docs/live-client-frame-benchmark.md`

- [ ] Write installer tests for ZIP path traversal, nested world-root discovery,
  malformed archives, and idempotent complete installs.
- [ ] Run `python3 scripts/test-install-client-benchmark-world.py` and verify the
  tests fail because the installer module does not exist.
- [ ] Implement streaming download, safe extraction into a unique temporary
  directory, `level.dat`/region validation, atomic cache installation, and
  server-file provisioning without touching other oracle worlds.
- [ ] Re-run the installer tests and verify they pass.
- [ ] Add a dedicated `:25590` game / `:25591` RCON oracle using the existing
  26.2 jar and the repository's foreground startup pattern.
- [ ] Run the installer against the official archive, record its byte count and
  SHA-256, then start the oracle and verify RCON responds.

### Task 2: Add megaworld choreography and overlay policy

**Files:**
- Modify: `crates/lodestone-shell/src/config.rs`
- Modify: `crates/lodestone-shell/src/app/benchmark.rs`
- Modify: `crates/lodestone-shell/src/app.rs`
- Modify: `crates/lodestone-shell/src/app/redraw.rs`

- [ ] Add failing configuration tests for `--benchmark megaworld` and
  `--benchmark-debug-overlay open|closed`, including rejection without a
  benchmark workload.
- [ ] Add a failing driver test proving megaworld uses the terrain flight
  choreography and stable `megaworld.*` labels.
- [ ] Run the focused config/driver tests and verify the new cases fail.
- [ ] Add `BenchmarkWorkload::Megaworld`, explicit overlay policy in
  `BenchmarkConfig`, CLI parsing/help, and exhaustive labels/intents.
- [ ] Apply the overlay policy only during benchmark runs, leaving ordinary F3
  behavior unchanged.
- [ ] Re-run focused tests and `cargo check -p lodestone-shell --no-default-features`.

### Task 3: Extend the live runner and summaries

**Files:**
- Modify: `scripts/client-frame-benchmark.py`
- Modify: `scripts/test-client-frame-benchmark.py`
- Modify: `justfile`

- [ ] Add failing Python tests for the megaworld oracle, authored-spawn player
  setup, overlay metadata, and parsing section/HUD count snapshots.
- [ ] Run the Python tests and verify the new cases fail.
- [ ] Add the oracle, pass overlay policy to the client, preserve it in JSONL,
  and run closed/open arms without relabelling segments.
- [ ] Parse and summarize `sections visited`, `chat lines`, `debug lines`, and
  `menu overlays` from the tracing stream; keep missing counts absent rather
  than zero.
- [ ] Add `just bench-client-megaworld` and a short smoke recipe.
- [ ] Re-run Python tests and the focused Rust benchmark tests.

### Task 4: Establish the large-world root cause

**Files:**
- Modify: `docs/client-frame-performance-2026-08-25.md`

- [ ] Build the release client once.
- [ ] Run a smoke trial for both overlay arms on the hardware built-in
  fullscreen display; fix workload/setup defects, not performance yet.
- [ ] Run three full trials per arm with the same map, position, route, render
  distance, display, and durations.
- [ ] Compare `world.prepare_buffers`, `world.terrain_cull_draw`,
  `world.other_draws`, `world.encoder_finish`, `world.queue_submit`, all six HUD
  subphases, section/HUD counts, frame percentiles, RSS, and GPU pass timestamps.
- [ ] Capture one Samply profile of the worse arm and state one root-cause
  hypothesis supported by both timing and counts.

### Task 5: Implement one measured optimization

**Files:**
- Modify: exact renderer/HUD files selected by Task 4 evidence
- Modify: corresponding subsystem documentation

- [ ] Write the smallest failing unit/integration control that represents the
  attributed hot path or allocation/submission count.
- [ ] Run it against the current implementation and verify the expected failure.
- [ ] Implement only the root-cause fix; do not bundle unrelated culling,
  caching, batching, or wgpu-option changes.
- [ ] Run the focused test, relevant pixel gates, shell no-feature seam, and
  benchmark smoke.

### Task 6: Repeat, verify, and report

**Files:**
- Modify: `docs/client-frame-performance-2026-08-25.md`
- Modify: `docs/live-client-frame-benchmark.md`
- Modify: `docs/README.md`

- [ ] Repeat three full same-arm trials and the CPU profile.
- [ ] Report before/after medians, trial spread, counts, CPU subphases, GPU
  passes, and any unchanged bottleneck; do not compare mismatched overlay arms.
- [ ] Run `python3` installer/runner tests, focused Rust tests, `just check`,
  `just check-all`, `just check-seam`, `just wasm-check`, and the honest
  foreground test scope required by touched systems.
- [ ] Regenerate the docs index, run `git diff --check`, and commit exact files
  only after the shared index count is zero.
