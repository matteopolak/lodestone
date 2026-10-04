# Performance comparison handoff

## Current objective

Build a fair, repeatable comparison of Lodestone against optimized Java gameplay and rendering on equivalent worlds and actions. Include native Lodestone and the browser build, compare visual and resource settings at the same physical resolution, and report frame-time distributions, successful presentations, CPU, memory, and GPU memory when it can be measured honestly. Produce a normal-speed split-screen video with Java on top and Lodestone below, with provenance and raw results suitable for sharing on X.

The goal tracker currently marks this objective `blocked`; this handoff does not change that status. The reported 1,000-FPS-at-5K result is a motivation, not a verified Lodestone target or an equivalent comparison.

## Repository state

- Latest commit: `8e36db43a9c842fd5c877d69bee908c5029a0cbb` (`Reduce terrain origin binding overhead`), pushed to `origin/main`.
- At handoff, `HEAD` and `origin/main` both resolved to that commit.
- The working tree contains extensive unrelated, uncommitted 26.3 migration and other work. Preserve it. Do not stage broad paths, reset, stash, clean, or commit unrelated changes.
- `docs/README.md` is currently dirty and fails `cargo run -q -p xtask -- docs-index --check`. It reflects concurrent documentation work. Do not publish links to uncommitted documents; regenerate and verify the complete index only when the related documentation is ready to land.
- The last check showed about 92 GiB free on the internal disk and 397 GiB on `/Volumes/CodexBuilds`. Recheck before large captures or builds.

Read `AGENTS.md`, `CLAUDE.md`, and relevant subsystem docs before editing. The repository uses a shared checkout and shared Cargo target. Use foreground builds, keep memory/disk bounded, and commit only exact owned file paths through `scripts/private-index-commit.sh` with a private `GIT_INDEX_FILE`. Never amend or force-push.

## Latest implementation and evidence

The latest change replaces per-draw dynamic terrain-origin binding with an origin instance stream using the existing padded arena. It preserves draw order, geometry, fade math, uploads, and legacy non-terrain rendering; it falls back to the existing uniform path when device limits require it. It also adds counters for real terrain camera/origin bindings and indexed draws, plus pixel-level controls.

Two accepted, reverse-order native A/B pairs used the same frozen scene (`139d2b4a…`), 1280×720 framebuffer, 329 loaded columns, and 170 handoffs/s cap. Whole-process retired instructions per successful handoff were approximately 11.213M for baseline and 9.689M for candidate (about 13.6% lower). Throughput remained capped at 170/s. In the reverse-order pair, p99 handoff interval was 7.866 ms baseline and 8.170 ms candidate. This supports reduced CPU instruction work, **not** an FPS win; handoffs are not displayed-frame cadence. The candidate recorded 2 terrain camera binds, 2 origin-stream binds, and 241 indexed draws per sampled frame.

Evidence limits:

- The scene is sparse/sky-heavy and does not establish performance on dense terrain, foliage, water, or 5K output.
- This is not yet a matched Java comparison. Java’s compatible optimization-mod stack and equivalent client settings still need runtime verification.
- No browser runtime comparison or comparison video is complete.
- GPU timestamp samples have shown reversed/invalid intervals. Do not use them as GPU duration until the query/readback association is independently validated. Unified-memory GPU residency also remains unavailable in the current capture.
- The Wasm check proves compilation/confinement, not browser runtime behavior.

## Verification and artifacts

For the committed source snapshot:

- Release native build passed.
- Render terrain unit/layout tests passed; the ignored GPU pixel test passed for opaque, translucent, and fluid paths. Its forced-zero-origin negative control produced 2,685 mismatching pixels per family, bounding box `[4,13,89,48]`.
- The shell’s ignored shared-encoder GPU test passed; frame profiler tests passed; Python benchmark-reader tests passed 54/54.
- `just wasm-check` passed, including 38 confinement rules and the Trunk build. It did not exercise the browser at runtime.
- `git show --check` passed for the published commit. `just health` was not run.

Private capture and audit artifacts are under `.cache/` and `/Volumes/CodexBuilds/scratch/lodestone-java-sodium-262-20261003/`. The candidate native binary is:

`/Volumes/CodexBuilds/scratch/lodestone-java-sodium-262-20261003/native-terrain-instance-e17/lodestone`

SHA-256: `20d7352850217e00b99cd07ec5794f23feb06908220def5c1c4ceeeee1900141`.

The encoder-only comparison binary is at `.../native-primary-encoder-e17/lodestone`. It is not the unmodified main baseline; use a fresh immutable binary from the relevant commit for future controls. A source manifest for the prior candidate is `.cache/terrain-instance-source-manifest.json`.

## Next steps

1. Recheck `HEAD`, `origin/main`, disk, memory, active Cargo processes, and dirty paths. Keep the 26.3 work untouched.
2. Establish an immutable release binary from the current pushed commit and record its SHA, compiler/configuration, and source revision. Preserve the previous binaries and captures as controls.
3. Improve the workload before drawing conclusions: use a verified ground-level world with terrain, trees/foliage, water, and a reproducible camera/action plan. Add a dense built scene and a controlled movement segment. Keep a separate 5K run; do not mix it with 1280×720 results.
4. Run repeated alternating-order native A/B trials on a quiet host. Validate the image/world identity, physical framebuffer size, settings, presentation mode, and continuous target residency/settlement independently. Record displayed FPS separately from redraw starts or queue handoffs.
5. Configure a verified Java release with compatible optimization mods. Freeze its exact game/mod versions, JVM arguments, pack, world snapshot, settings, resolution, warmup, and action timeline. Capture matching raw data for each arm.
6. Run the same workload on the browser build and measure end-to-end frame and resource behavior; a successful `wasm-check` alone is not runtime evidence.
7. Profile measured bottlenecks, then batch changes and compare against the immutable baseline. Keep optimizations that reduce measured cost without rendering/correctness regressions; do not add caches without evidence they help.
8. Record normal-speed video with Java above Lodestone. Keep overlays and UI treatment identical, include settings and metric definitions, and retain raw files plus a concise provenance manifest alongside the export.
9. Update the generated docs index when the concurrent documentation work is ready, rerun affected checks, commit exact owned files, and push additively.

## Benchmark interpretation

- Distinguish game FPS, successful surface submissions, redraw attempts, queue completion, and GPU execution; they are different boundaries.
- Compare frame-time percentiles and long stalls, not only averages or peak FPS.
- Report CPU instruction/cycle counts with the exact interval and process/thread scope. Report RSS separately from GPU allocations/residency.
- Record physical pixels, not only logical window size. Keep VSync/presentation policy explicit.
- Use the real vanilla texture pack for visual matching in Lodestone’s comparison build; do not use the normal user-facing alternate pack for this comparison.
- Any screenshot/video is only a visual record; retain machine-readable logs, world hashes, version/settings manifests, and raw samples for claims.
