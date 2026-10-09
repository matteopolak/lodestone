# Continuous integration

## What it is

`.github/workflows/ci.yml` runs on every push to `main` and every pull request so a GitHub-hosted runner verifies the hermetic majority of the suite. It does not replace the live/GPU gates (they need a GPU, a fetched `client.jar` or an oracle and stay `#[ignore]`d; see [oracles and benchmarks](./oracles-and-benchmarks.md)). `.github/workflows/branch-check.yml` is a separate opt-in workflow for branches named `codex/ci/**` or manual `workflow_dispatch` (no inputs, so no caller-supplied shell): `just check`, the `lodestone-worldgen` and `lodestone-server` lib tests and `just wasm-check` on Ubuntu with read-only permissions, cancelling superseded runs.

## How it works

Eight parallel jobs so a failure names itself (`check-default` is a three-OS matrix, the rest single Ubuntu legs):

| job | command | why |
|---|---|---|
| `check-default` (ubuntu, macos, windows) | `just check` | baseline; the only per-platform job |
| `check-all-features` | `just check-all` | every feature combination compiles |
| `check-shell-no-default` | `just check-seam` | shell compiles with no protocol family |
| `xtask-structural-checks` | `check-isolation`, `check-deletable` per family, `check-comment-voice` | cheap structural scans |
| `wasm` | `just wasm-check` | wasm32 tripwire; `web/` is its own workspace nothing else builds |
| `fuzz` | `just fuzz-smoke 30` | [fuzzing](./fuzzing.md) |
| `bench-gate` | `just test-bench-gate`, `bench-record`, `bench-gate` | [benchmark gate](./benchmark-regression-gate.md) |
| `test` | `just test`, then a self-skip surfacing step | the only job that links every test binary and runs all `.wgsl` through naga |

`wasm-check` and `fuzz-smoke` are not in `just health`: health is a command an agent chooses to run, CI always runs.

**Platform matrix and cost.** One job wide on purpose: OS differences are `#[cfg]` code, paths and platform crates, which `cargo check --workspace --all-targets` covers; features, seam and wasm are OS-independent. Private-repo minutes bill Linux 1x, Windows 2x, macOS 10x (one clean run: Linux 110 s, macOS 291 s, Windows 214 s, about 3,450 billed seconds). macOS is the first leg to drop (dev machines are Apple Silicon); Windows has found Windows-only defects alone.

**What it does not cover.** `cargo check` never links, so no check leg sees an unresolved symbol: test/bench code declaring macOS-only `libSystem` symbols (such as `proc_pid_rusage`) in an `extern` block fails only at link, so each such site is gated per item with `#[cfg(target_os = "macos")]` and the other arm panics rather than returning zero. Only the Linux `test` job links (linking on macOS/Windows would cost most of the job at 2x/10x). Architecture also matters: the pinned x86 Cranelift backend cannot lower the 256-bit CRC path in `crc32fast`, so the dev profile has a package-scoped LLVM override (find the originating crate before widening it), and negative-input `sqrt` NaN sign bits differ between aarch64 and x86_64.

**The `toolchain:` input is inert.** Every job passes it to `dtolnay/rust-toolchain`, but the pinned nightly in `rust-toolchain.toml` always wins; the `check-default` step "Report the toolchain actually in use" prints the real compiler. Do not delete the input (the action requires it and exits 1 on empty).

**Caching.** `Swatinem/rust-cache` with a per-job `shared-key`; `CARGO_INCREMENTAL=0`; `CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_PROFILE_TEST_DEBUG=0` (a `lodestone-shell` debug test binary reaches 3.7 GB RSS; `bench-gate` also zeroes bench/release debug); jobs touching the main workspace `apt-get install libasound2-dev pkg-config` (cpal) guarded by `if: runner.os == 'Linux'` and `timeout-minutes: 10` (mirror hangs are a seen failure). `mozilla-actions/sccache-action` runs after `rust-cache` with `SCCACHE_GHA_ENABLED` and `RUSTC_WRAPPER` (local Cargo has no wrapper). Windows sets `rustc_wrapper: ""` via `matrix.include` because the `lodestone-shell` command line exceeds 32,767 characters once sccache respawns it (`os error 206`); never express that as a `${{ cond && '' || 'sccache' }}` ternary (empty string is falsy).

**`bench-gate`.** Baselines are committed JSON in `bench-baselines/`; the band is two-way; only deterministic counts are gated. `bench-results/` uploads as an artifact. Open risk: baselines were recorded on aarch64 macOS and the gate runs on x86_64 Linux; a disagreement with no code change means the metric is not machine-independent and should be corrected or dropped.

**`wasm`.** `just wasm-check` runs per-crate `cargo check --target wasm32-unknown-unknown`, grep-based confinement rules (no filesystem, socket or wall-clock in crates that must not), then `(cd web && trunk build)` (pinned prebuilt `trunk`). `web/Trunk.toml` stages the resource archive and `blocks.json` in a conditional `post_build` hook so a fresh runner without `.cache/` builds; the atomics worker rebuilds std, hence `rust-src` in the toolchain.

**Self-skipping tests.** `.cache/` and `vendor/` are gitignored, so tests needing a jar, decompiled sources or `minecraft-data` are `#[ignore]`d, while a fixed set of non-ignored tests (each a second anchor on a table already checked against a committed golden dump) self-skip with a loud `eprintln!`. `cargo test` hides that on pass, so the `test` job re-runs them by name with `--nocapture` and warns if the count differs. Names live in `ci.yml`'s step.

### Reproducing a failure locally

```bash
just check; just check-all; just check-seam; just test; just wasm-check; just fuzz-smoke 30
just test-bench-gate && just bench-record && just bench-gate
```

`xtask-structural-checks` uses raw commands (the `just xtask` passthrough adds `-q`): `cargo run -p xtask -- check-isolation`, `check-deletable <package>` per package under `crates/versions/` (the workflow finds them via `cargo metadata` and `jq`), `check-comment-voice`. A red macOS leg reproduces with `just check` on Apple Silicon; Windows has no local repro (push a branch). PR runs cancel superseded runs; pushes to `main` do not. For a CI-like environment without `.cache/`, `vendor/` or a GPU, use `git worktree add --detach /tmp/lodestone-ci-repro HEAD` and `cargo test --workspace --no-fail-fast` there; a self-skipping-test difference is usually `.cache/` presence after `cargo xtask fetch-assets`.

## How to change it

- Add a check: a job (checkout, apt step if it touches `lodestone-sound`, toolchain, `rust-cache` with its own `shared-key`, `sccache-action`, `just`, the recipe), one command per job; add the recipe to the `Justfile` first.
- Add a platform: runner in `check-default`'s `matrix.include`, keep `fail-fast: false`, guard non-portable steps with `if: runner.os == '...'`, own `shared-key` suffix.
- Add a protocol family: its folder name (`v1-8`, not `lodestone-v1-8`) in the `for family in ...` loop in `xtask-structural-checks`.
- A test newly needing `.cache/`, `vendor/` or a GPU: `#[ignore = "reason"]`, or follow `load_real_report()` in `xtask/src/lib.rs` (check `.exists()`, print why, return) and add its name to the surfacing step with the count bumped.
- Gated bench: name it in `bench-record`, add its baseline, raise `--min-compared` in the same commit. Never add a job running live/GPU/jar gates.

## Configuration

`.github/workflows/ci.yml` (toolchain input, cache keys, apt packages, env vars, commented inline); `rust-toolchain.toml` (the compiler actually used); committed `Cargo.lock` (cache key changes exactly with dependencies); `bench-baselines/*.json`.

## Dependencies

`dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `mozilla-actions/sccache-action` (pinned `v0.0.11`), `extractions/setup-just` (pinned SHA, `just-version: "1.58.0"`), `actions/checkout@v4`, `actions/upload-artifact@v4`. No self-hosted runners, secrets or third-party services.
