# Continuous integration

## What it is

`.github/workflows/ci.yml` runs on every push to `main` and every pull
request. It exists so an agent can push and let a GitHub-hosted runner verify
the four commands in `CLAUDE.md`'s "Build and test" section, instead of every
agent running heavy `cargo` builds on the one shared dev laptop — with ten
agents on ten cores, local verification was slower than the work it was
checking.

It is **not** a replacement for the live/GPU gates. Those still need a real
GPU adapter, a fetched vanilla `client.jar`, or a running Docker Minecraft
oracle — none of which exist on a hosted runner — and stay exactly as
`#[ignore]`d as they are today. CI proves the hermetic 90%+ of the suite on
every push; the rest is still a local, explicit, opt-in run.

## How it works

Five jobs, all on `ubuntu-latest`, running in parallel so a failure names
itself instead of hiding behind three green jobs and one red one:

| job | command | why it's separate |
|---|---|---|
| `check-default` | `cargo check --workspace --all-targets` | the baseline health check |
| `check-all-features` | `cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench` | proves every feature combination compiles; the `--exclude` is structural, not a workaround (below) |
| `check-shell-no-default` | `cargo check -p lodestone-shell --no-default-features` | the version-seam check — no protocol family is enabled by default, and this is the only thing that proves the shell still compiles with **none** |
| `xtask-structural-checks` | `cargo run -p xtask -- check-isolation`, then `check-deletable` for each of `v47`/`v340`/`v735`/`v770` | dependency-direction and folder-deletability checks; cheap (xtask has almost no dependencies) and catches a class of break nothing else does |
| `test` | `cargo test --workspace --no-fail-fast` | the only thing that compiles doctests and runs all 22 `.wgsl` shaders through naga; see "What cannot run" below for what it deliberately does not exercise |

These are exactly the commands named in `CLAUDE.md`. **Do not "simplify" the
`--exclude lodestone-allocbench` away.** That crate has a deliberate
`compile_error!()` when more than one allocator feature is on — each installs
its own `#[global_allocator]` — so plain `--all-features` structurally cannot
pass for it. The `--exclude` is not covering up a bug; the bug it would be
"fixing" is the point of the crate.

`test` uses `--no-fail-fast` deliberately: plain `cargo test` stops at the
first failing test **binary**, so a break in an alphabetically-later crate is
invisible behind an earlier, unrelated failure. `CLAUDE.md` records this
having hidden three failing binaries and 14 failing tests behind one earlier
failure in a real session — `--no-fail-fast` is load-bearing, not a style
choice.

### Toolchain and caching

Every job pins the toolchain to `1.95.0` (matching `rust-toolchain.toml` and
the dev machine) via `dtolnay/rust-toolchain`, and caches `~/.cargo` and
`target/` via `Swatinem/rust-cache` with a `shared-key` per job so the
`--all-features` cache doesn't thrash against the default-features one (they
build a materially different `target/`). `CARGO_INCREMENTAL=0` because a CI
build is never resumed by the same job — incremental bookkeeping only adds
overhead. `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0` drop
DWARF debug info from dev/test profiles: nothing in CI symbolizes a crash
dump, and a debug `lodestone-shell` test binary has been measured locally at
**3.7 GB RSS**, so shedding debug info (which affects link time and binary
size, not codegen or `debug_assertions`) is free memory headroom on a standard
hosted runner.

### A real system-dependency gotcha, found empirically

`lodestone-sound` pulls in `alsa-sys` (via `cpal`) for Linux audio. Verified
in a bare `rust:1.95.0-bookworm` container: **the build fails** with
`pkg-config exited with status code 1` / `Package alsa was not found` unless
`libasound2-dev` (and `pkg-config`) are installed first — confirmed the
opposite too: installing them fixes it. Every job that touches the workspace
(everything except `xtask-structural-checks`, whose own dependency graph —
`anyhow`, `serde_json`, `sha1`, `zip`, `tempfile` — never reaches
`lodestone-sound`) installs `libasound2-dev pkg-config` via `apt-get` as its
first step, before the toolchain is even set up.

Everything else in the dependency tree that looked risky turned out fine on
inspection: `xkbcommon-dl`, `x11-dl` and `x11rb` (winit's Linux backends) are
either pure-Rust or `dlopen`-based, so they need no system dev packages at
build time; `aws-lc-sys` (via `rustls`) needs `cc`/`cmake`/`pkg-config`, all
of which are part of GitHub's documented `ubuntu-latest` runner image, so no
extra step was added for it — but if this workflow ever moves to a
self-hosted or minimal runner image, that assumption needs re-checking the
same way the alsa one was.

### `sccache` via `mozilla-actions/sccache-action`

Every job — including `xtask-structural-checks`, which skips the `alsa-sys`
apt step above — runs `mozilla-actions/sccache-action@v0.0.11` right after
`Swatinem/rust-cache` and before its first `cargo` invocation. This is not
optional once the dev machine's `.cargo/config.toml` gains a repo-wide
`build.rustc-wrapper` pointing at `sccache` (a build-infrastructure change
landing alongside this workflow, see `docs/build-caching.md`): a
config-level `rustc-wrapper` is unconditional, so a runner with no `sccache`
binary on `PATH` hard-errors on the very first compile — verified
empirically by another agent, not assumed. The two top-level env vars
(`SCCACHE_GHA_ENABLED: "true"`, `RUSTC_WRAPPER: "sccache"`) are what the
action's own README documents as required for Rust projects; the action
installs the binary and a GitHub-Actions-cache-backed server, but does not
set either variable for you.

Setting `RUSTC_WRAPPER` as a workflow env var also sidesteps a portability
question this file can't fully answer from CI's side: whatever absolute
path `.cargo/config.toml` ends up hardcoding (the dev machine's is a Homebrew
path, meaningless on a Linux runner) is irrelevant here, because Cargo's
config precedence has an environment variable always win over a config-file
value — this workflow's `RUSTC_WRAPPER: "sccache"` resolves through `PATH` to
whatever the action just installed, regardless of what the repo's config
file says. The same mechanism is the escape hatch if a future job or a
different runner image genuinely can't run the action: override
`RUSTC_WRAPPER: ""` for that job specifically, which disables the wrapper
without touching `.cargo/config.toml`.

### Surfacing the tests that self-skip

Eleven **non-`#[ignore]`d** tests read gitignored data (`.cache/mc/26.2` or
`vendor/minecraft-data`) and, by the codebase's own existing convention,
self-skip with a loud `eprintln!` — never a silent pass — when that data is
absent, because each one anchors against a **committed** dump or table
either way (see "What cannot run" below for the full list and why each is
safe to degrade rather than fail). `cargo test` hides that `eprintln!` output
on a pass, so after the main `cargo test --workspace --no-fail-fast` step,
the `test` job re-runs just those 11 tests by exact name with `--nocapture`
and writes a count into the job's step summary. This is pure visibility —
those 11 tests already ran (and passed) as part of the full suite above; this
step does not change what ran, it changes whether a human looking at the run
can tell that 11 of them ran with reduced coverage rather than full coverage.
If the observed count is ever not exactly 11, the step emits a
`::warning::` — either the runner unexpectedly has `.cache`/`vendor` (unlikely
on a fresh checkout) or one of the 11 stopped self-skipping, which is
interesting but not a failure (the workspace run already exited 0).

## What cannot run here, and why

`.cache/` and `vendor/` are both gitignored (`.gitignore` lines for
`/vendor/` and `/.cache/`) — no vanilla `client.jar`, no decompiled sources,
no `minecraft-data` checkout exists on a fresh runner — and a hosted runner
has no GPU adapter and no Docker Minecraft oracle listening on `:25565`/etc.
**219 tests are `#[ignore]`d** for exactly these reasons (live oracle, GPU
adapter, or vanilla jar) and none of them run in this workflow. This was
verified by reading every file that references `.cache/mc`, `GpuContext::new`,
`wgpu::Instance::new`, or `request_adapter` across the workspace and checking
each `#[test]`/`#[tokio::test]` function individually — not by trusting the
count — including cases that looked risky at first glance (see below).

Run them yourself with `-- --ignored --nocapture`, against a real jar
(`cargo xtask fetch-assets`), a GPU, and/or the oracles in
`scripts/live-oracles/`.

### The 11 that self-skip instead of being `#[ignore]`d

These are the **only** non-ignored tests in the workspace that read
gitignored data. Each is intentional, pre-existing, and documented in its own
doc comment as "skipped (not failed) when absent" — not something this CI
change introduced or weakened:

- `xtask` (`xtask/src/lib.rs`, `mod tests`), all keyed off a shared
  `load_real_report()`/inline `.exists()` check against
  `.cache/mc/26.2/generated/reports/{packets,registries}.json` or
  `vendor/minecraft-data/data/pc/1.8/protocol.json`:
  `packet_id_check_accepts_pristine_and_rejects_corrupted_file`,
  `parses_real_packet_report_counts`,
  `generated_identifiers_are_unique_per_state_and_bound`,
  `generated_lookup_helpers_round_trip`, `codegen_is_deterministic`,
  `parses_real_registry_report_counts_for_dispatch_blockers`,
  `registry_codegen_is_deterministic_and_standalone_rust`,
  `parses_real_minecraft_data_report_for_protocol_47`.
- `lodestone-data` (`tests/tools.rs`), cross-checking the crate's committed,
  generated tables against Mojang's own report rather than against the JVM
  dump the tables were generated from:
  `block_registry_order_agrees_with_mojangs_registries_report`,
  `block_tag_membership_agrees_with_the_vanilla_datapack`,
  `dump_agrees_with_mojangs_own_components_report`.

Why these are safe to degrade rather than fail: every one of them is a
**second, independent anchor** on top of a table that is already checked
against a *committed* golden dump elsewhere in the same crate (e.g.
`lodestone-data`'s many `*_jvm.txt` fixtures under `tests/support/`, which
**are** committed and **are** exercised on every `cargo test`, ignored or
not). Losing the Mojang-report cross-check on a runner with no jar loses a
second opinion, not the only opinion.

### Classes that looked risky but checked out as already-safe

Two patterns are common enough in this codebase to be worth naming, so a
future agent doesn't have to re-derive them:

- **A file containing GPU/jar code is not the same as a test needing it.**
  Files like `container_screen.rs`, `container_labels.rs`,
  `menu_panorama_pixels.rs`, `world_mesher_bench.rs`,
  `entity_diffuse_two_lights_pixels.rs` and `thrown_and_held_item_pixels.rs`
  each contain both `#[ignore]`d GPU tests *and* non-ignored, genuinely
  hermetic ones (pure layout math, CPU-only mesh-build budgets, camera-matrix
  algebra) — a file-level grep for `GpuContext::new`/`wgpu::Instance::new`
  over-reports; the check has to be per-function. All 26 initially-flagged
  functions were confirmed hermetic by reading their bodies.
- **A `#[test]`/`#[tokio::test]` named "live" is not necessarily a live
  oracle.** `protocol/v770/tests/combat_live.rs`'s two `#[tokio::test]`s
  ("a real client attacks a live mob...") run a real `lodestone-client`
  against a real `V770ServerProtocol`, but over `lodestone_net::memory_pair()`
  — an in-process duplex byte stream, not a socket — so there is no Docker
  oracle, no network, and no jar involved despite the name. They are
  correctly non-`#[ignore]`d and run in CI.
- **A committed fixture defeats the "reads `.cache`" grep.** Several
  `lodestone-data` "drift guard" tests (`shade_brightness.rs`,
  `sound_types.rs`, and others) document `.cache/mc/26.2` in their module
  doc comment as *provenance* for a fixture, but the fixture itself is
  `include_str!`'d from a **committed** `tests/support/*_jvm.txt` file, not
  read from `.cache/` at test time. Grep for the actual `Path`/`join` call,
  not for the string `.cache/mc` anywhere in the file.

## How to reproduce a CI failure locally

Run the exact failing job's command from `.github/workflows/ci.yml`. All five
are plain `cargo`/`xtask` invocations with no hidden environment beyond the
toolchain pin and (for anything workspace-wide) `libasound2-dev`/`pkg-config`:

```bash
# whichever job went red
cargo check --workspace --all-targets
cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench
cargo check -p lodestone-shell --no-default-features
cargo run -p xtask -- check-isolation
cargo run -p xtask -- check-deletable v770   # or v47 / v340 / v735
cargo test --workspace --no-fail-fast
```

If `test` is red locally but was green in CI (or vice versa), the likely
cause is **local `.cache/`/`vendor/` presence**: a dev checkout that has run
`cargo xtask fetch-assets` has real vanilla data, so the 11 self-skipping
tests above run with *full* coverage locally and reduced coverage in CI —
that is an expected difference in coverage, not a flaky test, and the
opposite direction (red in CI, green locally) is the one worth investigating.

To reproduce the CI environment more exactly (no `.cache/`, no `vendor/`, no
GPU), use a throwaway `git worktree` rather than moving anything in the
shared checkout — see `CLAUDE.md`'s repo-hazards section on why nothing here
should ever rename or delete `.cache/` in place:

```bash
git worktree add --detach /tmp/lodestone-ci-repro HEAD
cd /tmp/lodestone-ci-repro   # gitignored dirs do not exist here
cargo test --workspace --no-fail-fast
```

## The `sccache` coordination point (resolved)

See [`docs/build-caching.md`](./build-caching.md) for the full evaluation.
The dev machine's build infrastructure is being reworked to per-agent private
`--target-dir`s plus a shared `sccache` (measured there at a 94.28% warm hit
rate and a 3.4x CPU reduction on a workspace check) fronted by a repo-wide,
**unconditional** `build.rustc-wrapper` line in `.cargo/config.toml`. A
config-level wrapper pointing at a missing binary is a hard error on first
compile, not a slow fallback — verified empirically by another agent — so
this workflow could not simply ignore that change once it lands.

The owner's decision: every job here runs
`mozilla-actions/sccache-action@v0.0.11` right before its first `cargo`
invocation, with `SCCACHE_GHA_ENABLED: "true"` and `RUSTC_WRAPPER: "sccache"`
set at the workflow's top-level `env:` (both required by the action's own
README for Rust projects — it installs the binary and a cache server, but
sets neither variable itself). This also sidesteps a portability question
CI can't otherwise answer: whatever absolute path the dev machine's
`.cargo/config.toml` hardcodes is irrelevant here, because an environment
variable always wins over a config-file value in Cargo's config precedence,
so `RUSTC_WRAPPER: "sccache"` resolves through `PATH` to whatever the action
just installed regardless of what the repo's config file says. The
documented escape hatch, if a future job or runner genuinely can't run the
action, is overriding `RUSTC_WRAPPER: ""` for that job specifically.

The alternative the owner passed on — a top-level `RUSTC_WRAPPER: ""` for
every job, forfeiting the cache in CI entirely — was simpler and has no
external action dependency, but the owner chose to get the caching benefit
in CI too rather than opt out of it.

**This part is not yet proven, and says so on purpose**: a workflow that has
never run is its own kind of island. `actionlint` cannot execute the action,
check that `SCCACHE_GHA_ENABLED`/`RUSTC_WRAPPER` are sufficient, or catch a
runner-side incompatibility between the action and whatever `.cargo/config.toml`
ends up containing. The first real CI run — after this lands on `origin` — is
what actually proves the action, the toolchain pin, the `libasound2-dev` step,
and the 11 loud-skip tests all work together, and it should be read, not
assumed clean because `actionlint` was.

Separately: the dev machine's build infrastructure changes described above
(per-agent private `--target-dir`s, the shared `sccache`, and likely
`[profile.dev] debug = "line-tables-only"`) had not landed yet when the wall
times below were measured. **Every wall-time figure below predates that
change** and describes the old, single-shared-`target/`, no-`sccache`
world — re-measure after the infrastructure lands rather than trusting these
numbers as a baseline going forward.

## Verification status

The workflow YAML is statically validated (`actionlint`, zero findings,
including its embedded `shellcheck` pass over every `run:` block, re-checked
after the `sccache` steps were added). The `alsa-sys`/`libasound2-dev`
finding was confirmed empirically in an isolated container (Docker was later
shut down as part of the infrastructure rework, so it was not re-verified a
second time — the original finding stands).

All four `CLAUDE.md` commands plus `check-isolation` and `check-deletable`
for every protocol family were run for real, in an isolated `git worktree`
at committed `HEAD` (not the shared checkout, which had unrelated in-flight
work from other agents mid-edit — including, at the time, a genuine compile
error in an unfinished `menu/language.rs` that does not exist at `HEAD`).
Machine load: contended early on (ten-plus concurrent agents), quieter by
the time these ran. All real exit codes, not inferred from a pipeline:

| command | exit | wall time |
|---|---|---|
| `cargo check --workspace --all-targets` | 0 | 40s |
| `cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench` | 0 | 19s |
| `cargo check -p lodestone-shell --no-default-features` | 0 | 54s |
| `cargo run -p xtask -- check-isolation` | 0 | 7s |
| `cargo run -p xtask -- check-deletable v47/v340/v735/v770` | 0 (all four) | 2s total |
| `cargo test --workspace --no-fail-fast` | **143** (SIGTERM) | 1166s (interrupted) |

**The `test` row is not a real result and should not be read as one.** Exit
143 is `128 + SIGTERM` — something outside this process sent a termination
signal (almost certainly the dev machine's own infrastructure work clearing
its shared `target/`, which was happening concurrently; this worktree's
`target/` was a separate directory, but a broad process-level kill would not
care about that). It was not a test failure: grepping the full captured
output for `FAILED`, `panicked`, or any `N failed` with `N > 0` finds
**nothing** — 189 test binaries had already reported `test result: ok` (zero
failures) across every crate from the start of the alphabetical run through
most of `lodestone-shell`'s `lib`/`sim` unit tests, `lodestone-render`'s full
pixel-gate suite, and partway into `lodestone-server`'s unit tests, when the
signal arrived. What did **not** get its own confirmed green run before the
interruption: the remainder of `lodestone-server`, and everything
alphabetically after it (`lodestone-sound`, `lodestone-testsupport`,
`lodestone-world`, `lodestone-worldgen`, `lodestone-worldgen-parity`, the four
`protocol/*` crates, `xtask`, and `crates/plugins/*`) — including the doctest
compile pass and the 22-shader `wgsl_valid` run this job exists partly to
provide, and the 11-test self-skip inventory this file documents above (those
were separately confirmed by name — see the `xtask`/`lodestone-data` rows
implied by "Surfacing the tests that self-skip" — but not as part of this
particular `--workspace` invocation). This should be re-run to completion
once the build infrastructure work has landed, both because that is the
honest thing to do and because the outcome will be measured in the *new*
environment anyway.

Read the wall-clock figures above (including the incomplete 1166s) as
"before the infrastructure rework," not as a CI budget: most of the suite
that did run was fast, but `lodestone-server`'s worldgen tests
(`chunk::tests::parallel_generation_is_deterministic_and_matches_serial`,
several `worldgen_data::tests::*`) are individually CPU-heavy and, run
serially as `cargo test` does by default within one binary, would have
dominated whatever the final total turned out to be. A GitHub runner's exact
core count and clock speed will shift this further; none of this should be
read as a tight CI budget.

## How to extend it

- **Add a check**: add a job, following the existing pattern (checkout →
  apt step if the job touches `lodestone-sound` → toolchain → rust-cache with
  its own `shared-key` → the command). Keep one command per job; a job that
  runs three commands in sequence hides which one failed behind a single red
  X.
- **Add a new protocol family to `xtask-structural-checks`**: add its folder
  name to the `for family in ...` loop. `check-deletable` accepts a package
  name, folder name, or path — folder name (`v47`, not `lodestone-v47`) is
  what the existing loop uses for readability.
- **A new test starts needing `.cache/`/`vendor`/a GPU**: either `#[ignore]`
  it with a reason string (the existing convention — grep any
  `#[ignore = "..."]` in the workspace for the house style), or, if it should
  degrade gracefully instead of being skipped entirely, follow the
  `load_real_report()` pattern in `xtask/src/lib.rs`: check for the file with
  `.exists()`, `eprintln!("skipping <test>: <path> is absent")`, and return
  `Ok(())`/return early — **never** skip without printing why. If you do this,
  add the test's exact name to the `test` job's "Surface self-skipped..."
  step and bump the expected count, or the `::warning::` will fire on every
  run.
- **Do not add a job that runs the `#[ignore]`d gates.** There is no jar, no
  GPU, and no Docker oracle on a hosted runner, so such a job would either
  fail every single run (useless) or need its assertions loosened to pass
  (exactly the kind of "weakened check" this repo's whole culture exists to
  avoid — see `CLAUDE.md`'s "Evidence standards"). If self-hosted GPU runners
  or a jar-caching strategy are ever set up, that is a bigger design decision
  than this file covers — raise it as its own issue.
- **`docs/README.md` drift**: this file is indexed there. If you retitle it
  or change its `## What it is` paragraph, regenerate the index —
  `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed` — and
  commit the result; `cargo test -p xtask` fails loudly on drift.

## Configuration

- `.github/workflows/ci.yml` — the workflow itself; every tunable
  (toolchain version, cache keys, apt packages, env vars) lives inline with a
  comment explaining why.
- `rust-toolchain.toml` — the toolchain version CI pins to; keep the two in
  sync manually (there is no automated check that they agree — a mismatch
  would fail CI immediately on the next push, which is the mismatch
  announcing itself).
- `Cargo.lock` — committed, so `Swatinem/rust-cache`'s cache key (derived
  from it) changes exactly when dependencies do.

## Dependencies

- [`dtolnay/rust-toolchain`](https://github.com/dtolnay/rust-toolchain) —
  installs the pinned toolchain.
- [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) — caches
  `~/.cargo` and `target/` between runs.
- `actions/checkout@v4` — standard checkout.
- No self-hosted runners, no repository secrets, and no third-party service
  beyond GitHub Actions itself and the two actions above.
