# Continuous integration

## What it is

`.github/workflows/ci.yml` runs on every push to `main` and every pull request, so an agent can
push and let a GitHub-hosted runner verify the codebase instead of every agent running heavy
`cargo` builds on the one shared dev machine. It is **not** a replacement for the live/GPU
gates — those still need a real GPU adapter, a fetched vanilla `client.jar`, or a running
Minecraft oracle, none of which exist on a hosted runner, and stay exactly as `#[ignore]`d as
they are locally. CI proves the hermetic majority of the suite on every push; the rest stays a
local, explicit, opt-in run (`docs/oracles-and-benchmarks.md`).

## How it works

### The jobs

Eight jobs, ten legs — `check-default` is a three-OS matrix, everything else is one leg —
running in parallel so a failure names itself instead of hiding behind several green jobs and
one red one:

| job | runner(s) | command | why it exists |
|---|---|---|---|
| `check-default` | ubuntu + macos + windows | `just check` | the baseline health check, and the only per-platform job |
| `check-all-features` | ubuntu | `just check-all` | every feature combination compiles (`--exclude lodestone-allocbench` is structural, below) |
| `check-shell-no-default` | ubuntu | `just check-seam` | the version-seam check — no protocol family is on by default, and this is the only proof the shell still compiles with none |
| `xtask-structural-checks` | ubuntu | `cargo run -p xtask -- check-isolation`, `check-deletable` per family, `check-comment-voice` | dependency-direction, folder-deletability, and comment-voice — three cheap, `xtask`-dependency-only checks with no platform/feature axis worth their own job |
| `wasm` | ubuntu | `just wasm-check` | the wasm32 tripwire: nothing else builds `web/`'s own separate Cargo workspace |
| `fuzz` | ubuntu | `just fuzz-smoke 30` | the cargo-fuzz tripwire; see `docs/fuzzing.md`'s own CI section, not duplicated here |
| `bench-gate` | ubuntu | `just test-bench-gate`, `just bench-record`, `just bench-gate` | the benchmark regression gate; see `docs/benchmark-regression-gate.md`, not duplicated here |
| `test` | ubuntu | `just test`, then a self-skip surfacing step | the only job that links every test/bench binary and runs all `.wgsl` shaders through naga |

`check`, `check-all`, `check-seam`, `test`, and `check-comment-voice` are `CLAUDE.md`'s five
canonical health checks — four as their own job, `check-comment-voice` folded into
`xtask-structural-checks` because it is a cheap text scan with no per-platform or per-feature
axis worth a job of its own. `wasm-check` and `fuzz-smoke` are deliberately **not** part of
`just health`: health is a command an agent chooses to run many times a session, so it cannot
answer "nothing runs this automatically" — CI can, and does, on every push and PR. The same
reasoning applies to the benchmark suite: it is a real, working set of `criterion` files that
nothing invoked on any schedule before `bench-gate` existed.

### Which platform catches what, and the runner-cost arithmetic

The matrix is deliberately **one job wide, not several**. What differs between macOS, Linux
and Windows is `#[cfg]`-selected code, path handling, and platform crates, so `cargo check
--workspace --all-targets` is the whole per-platform payload. Every other job varies an axis
that is not the OS — features, the version seam, the dependency graph, runtime behaviour, or
the wasm32 target — so running any of them three times would measure the same thing three
times.

**Cost is a real constraint here, not a formality.** GitHub bills private-repo runner minutes
with a per-OS multiplier: Linux 1x, Windows 2x, macOS 10x. Measured wall times from run
`33813152822` (a clean three-leg pass):

| leg | wall | multiplier | billed-equivalent |
|---|---|---|---|
| `ubuntu-latest` | 110s | 1x | 110s |
| `macos-latest` | 291s | 10x | 2,910s |
| `windows-latest` | 214s | 2x | 428s |
| **total** | | | **≈3,448s ≈ 57 min** |

against the ~110s a single Linux leg would cost alone — roughly a **31x** increase for this one
job, on a workflow that runs on every push to `main` and every PR. Wall times drift run to run
with runner contention and cache warmth; re-measure from a recent run before quoting a specific
multiplier, but the shape — macOS costs an order of magnitude more per leg than it tells you —
does not change. macOS is the least informative leg per unit cost, since the dev machines here
are Apple Silicon and every local `just health` already proves that platform; if runner spend
ever needs cutting, it is the first leg to drop. Windows carries no compiler cache (below) and
is the one leg that has, historically, found a Windows-only defect on its own — the reason to
keep it is not its billed cost.

No extra trigger gating was needed to control this: `on:` is already `push: branches: [main]`
plus `pull_request`, so a push to any non-default branch never starts this workflow at all.

### What the matrix does not cover

**`cargo check` never links.** It stops after type-checking and emits no executable, so no
`check` leg on any OS can see an unresolved symbol. This is not theoretical: a handful of
test/bench-only sites (`crates/lodestone-shell/tests/session/client_chunk_cycles.rs`,
`crates/lodestone-server/tests/explosion_cost_profile.rs`,
`crates/lodestone-server/tests/join_parallel_efficiency.rs`, and two more under
`crates/versions/26.2` and `crates/lodestone-worldgen`) declare `proc_pid_rusage` in an
`unsafe extern "C"` block to read instructions-retired counters — a macOS-only `libSystem`
symbol. An `extern` declaration of a missing symbol compiles fine everywhere and fails only at
**link** time, so every `check` job stayed green while the Linux `test` job died on
`rust-lld: error: undefined symbol: proc_pid_rusage`. Each site is now gated per-item
(`#[cfg(target_os = "macos")]` on the constant, the `extern` block, and the real reader; a
`#[cfg(not(target_os = "macos"))]` arm that **panics** rather than returning zero, since every
caller feeds a before/after difference and a silent zero would report a real-looking cost of
nothing) — gated per item rather than per file, because some sibling functions in the same
files are ordinary tests that must still compile and run on every platform.

The residual gap, stated plainly: on Linux the `test` job links, so that class is covered
there. **On macOS and Windows nothing in this workflow links.** The check that would close it
is `cargo test --workspace --no-run` per platform, deliberately not added — it builds every
test binary in a workspace this large, which is most of the `test` job's own 10–35-minute
shape, at Windows' 2x and macOS' 10x multiplier, to catch a class whose only known instance so
far was Darwin-only FFI that the Linux `test` job already catches by construction. A
Windows-only `extern` would still be invisible to this workflow; that is an explicit deferral,
not an oversight.

A related fact worth stating once rather than rediscovering: a test passing on a dev machine
and failing on a hosted runner can differ on axes other than the OS name — architecture (a
codegen backend without a lowering for one SSE intrinsic an x86-only dependency feature
reaches; a negative-input `sqrt`'s NaN sign bit differing between aarch64 and x86_64) and
`cfg!` read from inside the function under test rather than passed as a parameter. None of
these show up in `cargo check` or in a wasm confinement scan.

### The `toolchain:` input is inert, and that is not the same as removable

Every job passes a `toolchain:` input to `dtolnay/rust-toolchain`, and that value is **not**
the compiler any job actually uses: `rust-toolchain.toml` pins `channel =
"nightly-2026-08-07"` (worldgen needs `portable_simd`), and cargo resolves that file over any
rustup default unconditionally and silently. The `check-default` matrix's own "Report the
toolchain actually in use" step prints `rustc --version`/`cargo --version` for this reason —
a job that names one compiler and silently runs another should at least say which one won.

Deleting the input is not the fix: `dtolnay/rust-toolchain@master`'s own `action.yml` declares
`toolchain` a required input and, since GitHub does not enforce `required` inputs itself,
opens with an explicit guard that exits 1 if the value is empty. Removing the line fails every
job at the install step rather than tidying anything. The three real options, none of them a
cleanup:

- **give it a truthful value** (`nightly-2026-08-07`) — duplicates the pin into every job with
  nothing checking the copies against `rust-toolchain.toml`;
- **drop the action entirely** and let cargo auto-install from `rust-toolchain.toml` (which
  already declares `components` and `targets = ["wasm32-unknown-unknown"]`, so the `wasm`
  job's `targets:` input is redundant too) — the least duplication, but changes what every
  passing job does and can only be validated by a real run;
- **leave it**, the current state — one wasted toolchain download per job, and nothing else,
  now that no comment claims the value is the compiler.

### Toolchain and caching

Every job caches `~/.cargo` and `target/` via `Swatinem/rust-cache` with a per-job
`shared-key` so a job that varies features or profile does not thrash a cache entry shared
with the default-features build. `CARGO_INCREMENTAL=0` — a CI build is never resumed by the
same job, so incremental bookkeeping only adds overhead. `CARGO_PROFILE_DEV_DEBUG=0` /
`CARGO_PROFILE_TEST_DEBUG=0` drop DWARF debug info from dev/test profiles (link time and
binary size only, not codegen or `debug_assertions`): nothing in CI symbolizes a crash dump,
and the workspace's own measurement is a `lodestone-shell` debug test binary reaching **3.7 GB
RSS**, so shedding debug info is free memory headroom on a standard hosted runner. The
`bench-gate` job separately zeroes `CARGO_PROFILE_BENCH_DEBUG`/`CARGO_PROFILE_RELEASE_DEBUG`
for the same reason, scoped there because a duration-symbolizing use of that DWARF would be a
reason *not* to drop it in a job that published timings — `bench-gate` reads only counts.

Every job that touches the main workspace installs `libasound2-dev pkg-config` via `apt-get`
before the toolchain step (`cpal`'s Linux audio backend needs them; macOS/Windows reach
CoreAudio/WASAPI through SDK frameworks and need nothing). That step carries `if: runner.os ==
'Linux'` in the matrix (`apt-get` does not exist on the other two runners) and a
`timeout-minutes: 10` bound everywhere else — a hung mirror inside `apt-get update` is a real,
previously-observed failure mode for this step specifically, and bounding it turns an
unbounded network wait into a fast, honest failure instead of eating the job's whole budget.

### sccache in CI, and what it is actually worth

The repo-wide, unconditional `build.rustc-wrapper` line in `.cargo/config.toml` means every
job here needs a real `sccache` binary on `PATH` before its first `cargo` invocation, or the
very first compile hard-errors rather than falling back — so every job (including
`xtask-structural-checks`, which skips the `alsa-sys` apt step) runs
`mozilla-actions/sccache-action@v0.0.11` right after `Swatinem/rust-cache`, with
`SCCACHE_GHA_ENABLED`/`RUSTC_WRAPPER` set at the workflow's top-level `env:` (both required by
the action's own README; it installs the binary and a cache server but sets neither variable
itself). Setting `RUSTC_WRAPPER` as a workflow env var also sidesteps a portability question:
an environment variable always wins over a config-file value in Cargo's precedence, so this
resolves through `PATH` to whatever the action just installed regardless of the absolute,
dev-machine-specific path `.cargo/config.toml` hardcodes. The documented escape hatch for a job
or runner that genuinely cannot run the action is overriding `RUSTC_WRAPPER: ""` for that job.

**Windows is that case, and it is a measured limitation rather than a preference.** The
`lodestone-shell` `rustc` invocation on that leg carries several hundred `-L
dependency=D:\...` flags, past Windows' 32,767-character command-line limit; `sccache`
re-spawns `rustc` itself and failed with `os error 206` (`ERROR_FILENAME_EXCED_RANGE`) while
cargo alone can spawn the same command. `check-default`'s matrix therefore sets
`rustc_wrapper: ""` for `windows-latest` via `matrix.include`, deliberately **not** via
`${{ matrix.os == 'windows-latest' && '' || 'sccache' }}` — an empty string is falsy in a
GitHub expression, so that ternary's `||` fires anyway and every leg would get `sccache`
regardless of the condition.

**Measured on both sides, and the two numbers disagree sharply on purpose.** In CI, `sccache`
is genuinely earning its place: the `wasm` job's own `Post Run` step on a recent run reported
**86% — 102 hits, 14 misses**, and every job's summary shows real nonzero compile requests
with a meaningful hit rate. Measured on the shared dev machine, across a five-agent day, the
contribution is close to zero: 5,635 compile requests, 1 cache hit — a **0.16%** hit rate — and
sccache refuses incremental compilation outright rather than falling through. Disabling
incremental compilation does not close that gap: two isolated builds of `lodestone-time` with
`CARGO_INCREMENTAL=0` into two empty target directories — identical inputs, so the second
should hit if the cache functions at all — produced 8 requests, 2 cacheable, 5 non-cacheable,
and **zero** hits. `docs/repo-tooling.md` carries the full local-side measurement and the
untested suspects for why it misses; the point worth keeping here is that "sccache works" and
"sccache is worth it locally" are different claims, verified independently, and only the first
one is true on the dev machine today.

### The `bench-gate` job

Three steps in order: `just test-bench-gate` (the gate's own control suite — a planted
regression it must catch and a healthy fixture it must pass, run first because it is seconds
of pure Python and should fail fast), `just bench-record` (runs the hermetic, count-producing
subset of the benchmark suite in `criterion`'s `--test` mode), and `just bench-gate` (compares
the fresh counts against committed baselines). The `bench-results/` measurement log is
uploaded as a job artifact on every run, success or failure, since it is gitignored and
otherwise vanishes with the runner.

Full design and rationale live in `docs/benchmark-regression-gate.md`; the facts that matter
for CI specifically:

- Baselines are committed JSON under `bench-baselines/`, one file per bench, keyed by
  `(scene, metric)`.
- The tolerance band is **two-way**: an unexplained *improvement* fails the gate too, because
  the most common cause of a count improving is a benchmark that quietly stopped doing the
  work it used to measure.
- Only deterministic **counts** are gated — draw-list sizes, quad counts, structural byte
  totals — never a duration. `ALLOWED_UNITS` in `scripts/bench-gate.py` refuses to write a
  baseline whose unit measures time, so a timing cannot re-enter through this door even by
  accident; this repo has already paid once for a committed duration acting as a wall-clock
  ceiling on healthy code under load.

**A known, currently-unresolved risk**: the committed baselines were first recorded on
`aarch64` macOS, and `bench-gate` runs on an `x86_64` Linux hosted runner. A gated metric is
only a valid baseline if it is a pure function of committed code plus a committed fixture —
architecture-independent by construction — but that has not yet been *proven* by a completed
CI run at the time of writing. If the first run disagrees, the diagnostic question is not
"did the code regress" but "is this metric actually machine-independent": check out the exact
commit that produced the baseline and re-run `just bench-record && just bench-gate` on both an
aarch64 macOS machine and (nested in a throwaway checkout, or reasoning from the CI log) the
Linux shape — a disagreement with **no code change in between** is a baseline-scope bug in the
metric's determinism, not a regression, and the fix is to correct or drop that metric, not to
chase a code change that was never made. A regression, by contrast, always has a diff between
the baseline's commit and the one being gated.

### The `wasm` job

Two things run under `just wasm-check`: 20 per-crate `cargo check --target
wasm32-unknown-unknown` builds and 34 grep-based confinement rules (asserting that crates with
no business touching a filesystem, a socket, or wall-clock time on wasm32 do not), then a real
`(cd web && trunk build)` of `web/` — its own separate Cargo workspace with its own lockfile,
outside the root `members` glob, so nothing in `check`/`check-all` has ever covered it. `trunk`
is installed from a pinned prebuilt release tarball rather than built from source. `web/`'s own
`Trunk.toml` stages the gitignored vanilla `client.jar`/`blocks.json` through a conditional
`post_build` hook rather than a mandatory `data-trunk rel="copy-file"` link, so the build
itself does not require `.cache/` to be populated on a fresh runner — see `web/Trunk.toml` for
the mechanism.

**As of this writing, the job is red, and the cause is a real compile error rather than a
tooling or caching problem**: `lodestone-shell` (and, downstream, the `trunk build` of
`lodestone-web`) fails with `E0425` against names on the worldgen-override surface
(`world_dir`, `overworld_chunk_source_override`, `GeneratorOverride`) that do not currently
resolve — the shape of an in-flight edit elsewhere in the tree rather than a wasm-specific
defect, and it is being fixed outside this doc. Do not read a future green run as evidence this
was already fixed by anything described here; check the job's own log.

`wasm-check` is deliberately **not** part of `just health`. `health` is four (now five, with
`check-comment-voice`) full or near-full workspace builds already, run many times a session by
every agent; adding a ~20-crate wasm build plus a `trunk build` would tax the command everyone
runs to catch a regression that only lands on the browser surface. More decisively, a command
someone *chooses* to run cannot answer "nothing runs this automatically" — CI can, on every
push and PR, which is the whole reason this job exists: for a long time nothing ran
`wasm-check` at all, not `health`, not CI, and the first person to try the browser build found
several confinement breaks waiting.

### The `fuzz` job

Runs `just fuzz-smoke 30` — a bounded, thirty-second-per-target `cargo-fuzz` pass over every
target's committed seed corpus under ASan, gating that each target still builds and links,
still reaches real decode code, replays its full seed corpus deterministically, and does not
crash within seconds of those seeds. Long campaigns stay a human's job. Full rationale,
measured runs, and the seed-corpus provenance live in `docs/fuzzing.md`; not duplicated here
because that doc already has its own CI section naming this exact job.

### Surfacing the tests that self-skip, and the ignored-test inventory

`.cache/` and `vendor/` are both gitignored, so no vanilla `client.jar`, no decompiled sources,
and no `minecraft-data` checkout exist on a fresh runner, and a hosted runner has no GPU
adapter and no running Minecraft oracle. Tests needing any of those are `#[ignore]`d for
exactly that reason and do not execute here at all — the count drifts constantly (a mechanical
`grep -rc '#\[ignore' crates/ xtask/` reports **1207** as of this writing; re-run it rather than
trusting this number, the same way the number this replaced had already gone stale twice).

Eleven **non-`#[ignore]`d** tests read the same gitignored data and, by an existing convention,
self-skip with a loud `eprintln!` rather than a silent pass when it is absent — `cargo test`
hides that `eprintln!` on a pass, so the `test` job re-runs exactly those eleven by name with
`--nocapture` afterward and writes a count into the job summary, warning if it is ever not
exactly 11. This is pure visibility: those eleven already ran, and passed, as part of the full
suite above. **Why it is safe for each of them to degrade rather than fail**: every one is a
*second, independent* anchor on top of a table already checked against a **committed** golden
dump elsewhere in the same crate — `xtask`'s tests cross-check generated code against a
Mojang report also checked, deterministically, against a committed fixture; `lodestone-data`'s
tests cross-check generated tables against a Mojang report also checked against committed
`tests/support/*_jvm.txt` dumps. Losing the Mojang-report cross-check on a runner with no jar
loses a second opinion, never the only one. The exact eleven names are listed inline in
`ci.yml`'s own step, which is the place to keep them in sync — this doc explains why the
pattern is safe, not which tests currently hold it.

## How to reproduce a CI failure locally

Every job's `run:` step **is** the command to reproduce — five of the eight jobs are literally
a `just` recipe, so there is no translation to do:

```bash
just check          # check-default
just check-all       # check-all-features
just check-seam       # check-shell-no-default
just test             # test
just wasm-check        # wasm
just fuzz-smoke 30      # fuzz
just test-bench-gate && just bench-record && just bench-gate   # bench-gate
```

`xtask-structural-checks` is not wrapped through `just` at all: the generic `xtask *args`
Justfile passthrough always adds `-q`, which would not be byte-identical to this job's
existing invocation, so reproduce it with the raw commands instead —
`cargo run -p xtask -- check-isolation`, `cargo run -p xtask -- check-deletable <family>` for
each of `v1-8`/`v1-9`/`v1-14`/`v26-2`, and `cargo run -p xtask -- check-comment-voice`.

**If the red leg was `check-default (macos-latest)`, `just check` on an Apple Silicon dev
machine already reproduces it** — same OS, same architecture, same pinned nightly. A red
`windows-latest` leg has no local reproduction on this project's dev machines: read the run's
log, or push a branch and open a PR to iterate (a PR run cancels its own superseded runs;
pushes to `main` deliberately do not, so `main` always gets a completed `test` result even on a
fast-moving trunk).

To reproduce the CI environment more exactly — no `.cache/`, no `vendor/`, no GPU — use a
throwaway `git worktree` rather than moving anything in the shared checkout:

```bash
git worktree add --detach /tmp/lodestone-ci-repro HEAD
cd /tmp/lodestone-ci-repro   # gitignored dirs do not exist here
cargo test --workspace --no-fail-fast
```

If `test` is red locally but was green in CI (or the reverse), the likely cause for the eleven
self-skipping tests specifically is local `.cache/`/`vendor/` presence, not a flake: a dev
checkout that has run `cargo xtask fetch-assets` runs those eleven with full coverage, while
CI always runs them with reduced coverage. That is an expected coverage difference, not a
regression.

## How to extend it

- **Add a check**: add a job following the existing pattern (checkout → apt step if the job
  touches `lodestone-sound` → toolchain → `Swatinem/rust-cache` with its own `shared-key` →
  `sccache-action` → install `just` → the recipe). One command per job — a job running several
  commands in sequence hides which one failed behind a single red X. Add the recipe to the
  `Justfile` first if it does not exist; a job that calls raw `cargo` instead of a recipe is
  exactly the drift `just` exists to prevent.
- **Add a platform**: add the runner to `check-default`'s `matrix.include`. Keep
  `fail-fast: false` — the default cancels the other legs on the first red one, so you would
  learn one thing per run instead of three. Guard any non-portable step with
  `if: runner.os == '…'` and give the leg its own `shared-key` suffix.
- **Add a protocol family** to `xtask-structural-checks`'s deletability loop: add its folder
  name (`v1-8`, not `lodestone-v1-8`) to the `for family in ...` list.
- **A new test starts needing `.cache/`/`vendor`/a GPU**: `#[ignore]` it with a reason string
  (the existing house style — grep any `#[ignore = "..."]` for examples), or, if it should
  degrade gracefully instead, follow the `load_real_report()` pattern in `xtask/src/lib.rs`:
  check `.exists()`, `eprintln!("skipping <test>: <path> is absent")`, return early — never
  skip without printing why. If you do this, add the test's exact name to the `test` job's
  self-skip surfacing step and bump the expected count, or its `::warning::` fires on every
  run.
- **Add a bench to the gated set**: name its target in the `bench-record` recipe and add its
  deterministic metrics to a new `bench-baselines/<bench>.json`; raise `--min-compared` in the
  same commit, or the new entries can silently stop being compared. Full detail in
  `docs/benchmark-regression-gate.md`.
- **Do not add a job that runs the `#[ignore]`d live/GPU/jar gates.** There is no jar, GPU, or
  Minecraft oracle on a hosted runner; such a job would either fail every run or need its
  assertions loosened, which is exactly the class of change this project's evidence standards
  forbid.
- **`docs/README.md` drift**: this file is indexed there. If its H1 or `## What it is`
  paragraph changes, regenerate with `cargo xtask docs-index` (or `just regen-docs-index`) and
  commit the result — `cargo test -p xtask` fails loudly on drift.

## Configuration

- `.github/workflows/ci.yml` — the workflow itself; every tunable (toolchain version, cache
  keys, apt packages, env vars) lives inline with a comment explaining why.
- `rust-toolchain.toml` — the toolchain every job actually compiles with; wins over the
  workflow's `toolchain:` input silently rather than agreeing with it (above).
- `Cargo.lock` — committed, so `Swatinem/rust-cache`'s cache key changes exactly when
  dependencies do.
- `bench-baselines/*.json` — committed count baselines the `bench-gate` job compares against;
  see `docs/benchmark-regression-gate.md` for who is allowed to move a value and how.

## Dependencies

- [`dtolnay/rust-toolchain`](https://github.com/dtolnay/rust-toolchain) — installs the pinned
  toolchain (inert everywhere but the `fuzz` job, above).
- [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) — caches `~/.cargo` and
  `target/` between runs.
- [`mozilla-actions/sccache-action`](https://github.com/mozilla-actions/sccache-action)
  (pinned `@v0.0.11`) — installs `sccache` and a GitHub-Actions-cache-backed server.
- [`extractions/setup-just`](https://github.com/extractions/setup-just) (pinned to a commit
  SHA, `just-version: "1.58.0"`) — installs `just` in every job that calls a recipe.
- `actions/checkout@v4`, `actions/upload-artifact@v4` — standard checkout and artifact upload
  (the latter for `fuzz`'s crash artifacts and `bench-gate`'s measurement log).
- No self-hosted runners, no repository secrets, and no third-party service beyond GitHub
  Actions itself and the actions above.
