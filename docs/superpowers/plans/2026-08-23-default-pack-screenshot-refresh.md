# Default-pack Screenshot and Coverage Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make documentation captures deterministic against the built-in vanilla resource pack, regenerate all five images, and refresh current protocol coverage figures.

**Architecture:** Keep the production resource pipeline intact and isolate only the capture process by clearing its process-wide selected-pack order before `Sim` construction. Drive all coverage numbers from `cargo xtask connectedness`, keeping historical dated measurements intact.

**Tech Stack:** Rust integration tests, Lodestone resource manager, RCON screenshot harness, `xtask`, wgpu headless rendering, Markdown documentation.

---

### Task 1: Pin the capture process to vanilla resources

**Files:**
- Modify: `crates/lodestone-shell/tests/capture_screenshots.rs`

- [x] Add a hermetic test that seeds `resources::set_selected_packs` with a fake custom pack, calls the capture configuration boundary, and expects `resources::selected_packs()` to be empty.
- [x] Run `cargo test -p lodestone-shell --test capture_screenshots capture_configuration_uses_only_the_builtin_pack` and confirm it fails because the selection remains populated.
- [x] Make the capture configuration boundary call `resources::set_selected_packs(Vec::new())` before returning the live configuration, with a comment that this does not persist the change.
- [x] Re-run the focused test and confirm it passes.

### Task 2: Refresh current coverage snapshots

**Files:**
- Modify: `README.md`
- Modify: `docs/roadmap/protocol.md`

- [x] Run `cargo xtask connectedness` and capture its complete successful output.
- [x] Replace the README join/host table counts with that output: legacy clientbound and serverbound-encoded counts, plus v770's current encoded/decoded/connected values.
- [x] Replace only the leading current measured-coverage block in `docs/roadmap/protocol.md`; preserve explicitly dated historical measurements below it.
- [x] Re-run `cargo xtask connectedness` and compare each numerator and denominator with both documentation snapshots.

### Task 3: Regenerate every image

**Files:**
- Modify: `docs/images/01-text-displays.png`
- Modify: `docs/images/02-signs.png`
- Modify: `docs/images/03-block-entities.png`
- Modify: `docs/images/04-entities.png`
- Modify: `docs/images/05-hud.png`
- Modify: `docs/screenshots.md`

- [x] Update `docs/screenshots.md` to state that committed captures always clear local selected packs and use the built-in jar.
- [x] Start or confirm the flat creative oracle from `scripts/live-oracles/creative.sh`.
- [x] Run `just screenshots` with no scene filter and require five successful writes.
- [x] Inspect all five PNGs at original resolution, checking vanilla 16x textures and each scene's intended content.

### Task 4: Verify and deliver

**Files:**
- Modify if generated: `docs/README.md`
- Modify: `xtask/src/lib.rs`
- Modify: `scripts/wasm-check.sh`
- Modify: `docs/ci.md`

- [x] Run the docs-index check or regenerate `docs/README.md` if the new design/plan entries require it.
- [x] Run the focused capture and scene tests.
- [x] Run `just health` in the foreground; classify the one parallel resource-pack global-state failure by rerunning it alone, single-threaded (it passed).
- [x] Add a regression test and scrub inherited `NO_COLOR` in both wasm runners after the environment exposed Trunk's boolean-parser failure.
- [x] Run `just wasm-check` and the reference shell guard with `NO_COLOR=1`; confirm every compile/confinement guard and the Trunk build pass.
- [x] Confirm the working tree contains only the explicit capture, documentation, wasm-runner follow-up, and four changed image files (`02-signs.png` re-rendered byte-identically).
- [ ] Commit explicit file paths, verify the resulting commit stat, push `main`, and confirm `HEAD == origin/main` with a clean index.
