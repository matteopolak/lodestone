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

Six jobs — eight legs, because `check-default` is a three-OS matrix — running in
parallel so a failure names itself instead of hiding behind three green jobs and
one red one:

| job | runner | command | why it's separate |
|---|---|---|---|
| `check-default` | **`ubuntu` + `macos` + `windows`** | `just check` (`cargo check --workspace --all-targets`) | the baseline health check, and the only per-platform job |
| `wasm` | `ubuntu` | `just wasm-check` (`cargo xtask wasm-check`) | the wasm32 tripwire: 19 per-crate wasm builds, ~20 grep-based confinement rules, and a real `trunk build` of `web/` |

and, on `ubuntu-latest` only:

| job | command | why it's separate |
|---|---|---|
| `check-all-features` | `just check-all` (`cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench`) | proves every feature combination compiles; the `--exclude` is structural, not a workaround (below) |
| `check-shell-no-default` | `just check-seam` (`cargo check -p lodestone-shell --no-default-features`) | the version-seam check — no protocol family is enabled by default, and this is the only thing that proves the shell still compiles with **none** |
| `xtask-structural-checks` | `cargo run -p xtask -- check-isolation`, then `check-deletable` for each of `v47`/`v340`/`v735`/`v770` | dependency-direction and folder-deletability checks; cheap (xtask has almost no dependencies) and catches a class of break nothing else does |
| `test` | `just test` (`cargo test --workspace --no-fail-fast`) | the only thing that compiles doctests and runs all 22 `.wgsl` shaders through naga; see "What cannot run" below for what it deliberately does not exercise |

### Which platform catches what, and what each one costs

The matrix is deliberately **one job wide, not six**. What differs between macOS,
Linux and Windows is `#[cfg]`-selected code, path handling and platform crates —
so `cargo check --workspace --all-targets` is the whole per-platform payload.
Every other job varies an axis that is *not* the OS, and running it three times
would measure the same thing three times:

| job | axis it varies | so it runs on |
|---|---|---|
| `check-default` | **the OS** | all three |
| `check-all-features` | features | Linux |
| `check-shell-no-default` | the version seam | Linux |
| `xtask-structural-checks` | the dependency graph | Linux |
| `test` | runtime behaviour | Linux |
| `wasm` | the *target* (wasm32, from any host) | Linux |

**Cost, because it is a real constraint on a repo with this much churn.** GitHub
bills private-repo runner minutes with a per-OS multiplier: **Linux 1x, Windows
2x, macOS 10x**. The matrix's cost is therefore not "3x one job". Using the
measured wall times from run `31337815809` (see "Verification status"):

| leg | wall | multiplier | billed-equivalent |
|---|---|---|---|
| `ubuntu-latest` | 98s | 1x | 98s |
| `macos-latest` | 100s | 10x | 1,000s |
| `windows-latest` | 531s | 2x | 1,062s |
| **total** | | | **~2,160s ≈ 36 min** |

against the ~1.6 minutes the single Linux leg used to cost — roughly a **22x**
increase for this job, on a workflow that runs on every push to `main` and every
PR.

Two things follow, and the second is not what you would guess:

- **macOS is the least informative leg per unit cost.** The dev machine is an
  Apple Silicon Mac, so every local `just health` already proves that platform;
  the leg exists because the request named it, not because it earns its 10x. If
  runner spend needs cutting, drop it first.
- **Windows is now the most expensive leg in absolute billed terms** (1,062s vs
  macOS' 1,000s), purely because it runs without `sccache`. It is also the *only*
  leg that has ever found something — a Windows-only defect on its first run — so
  it is the one to keep. If its cost becomes a problem, the fix is to get a
  compiler cache working there, not to drop the leg.

No extra trigger gating was needed for this, and the obvious idea to add some
would have been a no-op: the `on:` block is already `push: branches: [main]` plus
`pull_request`, so a push to any non-default branch **never** starts this workflow
in the first place. There is no "every branch push" case to exclude.

### What the matrix does not cover

**`cargo check` never links.** It stops after type-checking and emits no
executable, so no `check` leg — on any OS — can see an unresolved symbol. This is
not a theoretical gap; it is how the one real cross-platform break in this
repository hid:

Five test and bench targets declare `proc_pid_rusage` in an `unsafe extern "C"`
block to read `ri_instructions` (`CLAUDE.md` prefers instructions-retired over
wall clock, at 0.16–0.21% reproducibility against ~10.8%). That symbol lives in
Darwin's `libSystem` and exists on neither Linux nor Windows. An `extern`
declaration of a missing symbol **compiles fine everywhere and fails at link**,
so all three `check` jobs were green while the Linux `test` job died on
`rust-lld: error: undefined symbol: proc_pid_rusage` — which it had been doing on
every run, in `lodestone-shell`'s `client_chunk_cycles` test binary. The five
sites are now `#[cfg(target_os = "macos")]`-gated with explicit non-Darwin arms
(below).

The residual gap, stated plainly: on Linux the `test` job links, so that class is
covered there. **On macOS and Windows nothing in this workflow links.** The check
that would close it is `cargo test --workspace --no-run` per platform, and it was
deliberately **not** added: it builds every test binary in a ~290-crate
workspace, which is the 10–35-minute shape the `test` job already has, and at
Windows' 2x and macOS' 10x that is a large recurring cost to catch a class whose
only known instance was Darwin-only FFI — something the Linux `test` job catches
by construction. If a *Windows*-only `extern` ever lands, nothing here will see
it; a Windows `cargo test --no-run` on push-to-`main` only is the cheapest way to
buy that, and it is an explicit deferral rather than an oversight.

### The `#[cfg]` gates this required

Five files, all test or bench targets — no library code needed changing. Each got
the same narrow treatment: `#[cfg(target_os = "macos")]` on the two constants,
the `extern "C"` block and the real reader, plus a `#[cfg(not(target_os =
"macos"))]` reader that **panics with `unimplemented!`**:

- `crates/lodestone-shell/tests/client_chunk_cycles.rs` — `Counters::read` and
  `assert_counters_are_real`
- `crates/lodestone-server/tests/explosion_cost_profile.rs` — `instructions_now`
- `crates/lodestone-server/tests/join_parallel_efficiency.rs` — `rusage_now`
- `crates/protocol/v770/tests/chunk_encode_cycles.rs` — `instructions_retired`
- `crates/lodestone-worldgen/benches/generation.rs` — `instructions_retired`
  (a **bench** target, which `--all-targets` checks and `cargo test` links, so it
  is a real cross-platform break and not only a `cargo bench` concern)

Two decisions worth keeping, because the cheaper alternatives are both wrong:

- **The non-Darwin arm panics; it does not return zero.** Every one of these
  readers feeds a before/after difference or a ratio, so a counter that silently
  reads `0` would report a cost of zero instructions — a number that looks like a
  result and is not one. That is the *precondition* species of vacuous test in
  `CLAUDE.md`, and `client_chunk_cycles.rs`'s own doc comment already warns about
  exactly this failure mode for the Intel/Rosetta case. Every test that reaches
  one of these is `#[ignore]`d, so nothing on Linux or Windows calls it; running
  one explicitly with `--ignored` on those hosts is what should fail loudly.
- **Gated per item, not per file.** A file-level `#![cfg(target_os = "macos")]`
  is fewer lines and would have been wrong twice over.
  `chunk_encode_cycles.rs`'s `encode_chunk_still_returns_a_send` is **not**
  `#[ignore]`d and is not a measurement, so a file-level gate would have silently
  dropped a real test from the Linux and Windows suites. And the surrounding
  harness code in the two `lodestone-server` tests exercises a lot of server API;
  keeping it compiling on all three platforms is most of the value of having
  those platforms in CI at all.

### The `toolchain:` input is inert, and the job now says so

`docs/ci.md` claimed for a while that "every job pins the toolchain to `1.95.0`
(matching `rust-toolchain.toml` and the dev machine)". **Both halves were wrong.**
`rust-toolchain.toml` pins `channel = "nightly-2026-08-07"` (worldgen needs
`portable_simd`), and cargo resolves that file over any rustup default — so the
`toolchain: "1.95.0"` input installs a toolchain that is then never used. rustup
says so in the run's own log:

```
info: note that the toolchain 'nightly-2026-08-07-x86_64-unknown-linux-gnu'
      is currently in use (overridden by …/lodestone/rust-toolchain.toml)
```

This is benign in effect — every job has always compiled with the same pinned
nightly as the dev machine, which is what you want — but it wastes a toolchain
download per job and the file was asserting the opposite of what happened. The
`check-default` matrix now has a `Report the toolchain actually in use` step that
prints `rustc --version`/`cargo --version` and fails if there is none, so each leg
states its compiler instead of a comment claiming one.

**That follow-up — "just delete the pointless `toolchain:` input from the other
jobs" — was attempted and is NOT possible. Do not try it.** The input is *inert*,
but it is not *optional*: `dtolnay/rust-toolchain@master`'s own `action.yml`
declares `toolchain` as `required: true` and, because GitHub does not enforce
`required` inputs itself, opens with an explicit guard that exits 1 —

```yaml
if [[ -z $toolchain ]]; then
  # GitHub does not enforce `required: true` inputs itself.
  echo "'toolchain' is a required input" >&2
  exit 1
```

— so removing the line does not remove a dead pin, it **fails every job at the
install step**. "Inert" and "removable" are different properties, and the value
being unused by the compiler said nothing about whether the action tolerates its
absence. Read the action, not the effect.

The three real options, none of which is a cleanup:

- **give it a truthful value** (`nightly-2026-08-07`) — duplicates the pin into
  seven places with nothing checking the copies against `rust-toolchain.toml`,
  which is the drift this repo pays for repeatedly;
- **drop the action entirely** and let cargo auto-install from
  `rust-toolchain.toml` (which already declares `components` *and*
  `targets = ["wasm32-unknown-unknown"]`, so the `wasm` job's `targets:` input is
  redundant too) — plausibly correct and the least duplication, but it changes
  what six passing jobs do and can only be validated by a run;
- **leave it**, which is the current state. It costs one unused toolchain download
  per job and nothing else, now that no document claims the value is the compiler.

**Five of the six jobs call `just` recipes** (`docs/task-runner.md`) rather
than retyping the raw `cargo` invocation, so this file, `CLAUDE.md`, and
`ci.yml` cannot silently drift apart the way three independently-maintained
copies of the same command eventually do. Each of those five jobs installs a
pinned `just` (`extractions/setup-just`, pinned to a commit SHA, not a
floating major tag) right before its recipe-calling step. All three legs of the
`check-default` matrix run the same `just check`, so adding platforms added no
new command to keep in sync — which is most of why the recipe layer was worth
having. `xtask-structural-
checks` is the one exception, left calling `cargo run -p xtask` directly: the
Justfile's generic `xtask *args` passthrough always adds `-q`, which is not
byte-identical to this job's pre-existing invocation, so wrapping it would
change what the job runs rather than merely naming it. Neither
`LODESTONE_TARGET_DIR` nor `LODESTONE_JOBS` is set anywhere in this workflow,
so every recipe here expands to the same bare command it always ran — a
private `--target-dir` is a local-agent convention (`docs/build-caching.md`),
not something CI opts into, and `-j` stays at cargo's own default rather than
a hardcoded value that would throttle the runner.

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

Every job passes `toolchain: "1.95.0"` to `dtolnay/rust-toolchain`, and **that
value is not the compiler any job uses** — cargo resolves `rust-toolchain.toml`'s
`channel = "nightly-2026-08-07"` over it, so 1.95.0 is installed and then never
invoked. The input cannot simply be deleted either; see "The `toolchain:` input is
inert, and the job now says so" above for the action's own `required: true` guard
and the three real options. The `check-default` matrix prints `rustc --version` so
each leg states its actual compiler rather than inheriting a claim from this file.

Every job caches `~/.cargo` and
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

In the `check-default` matrix that step carries `if: runner.os == 'Linux'`, which
is a correctness requirement and not just a saving: `apt-get` does not exist on
the macOS or Windows runners, so an ungated step would fail those legs outright.
Neither needs it — `cpal` gates `alsa-sys` to Linux in its own manifest, and
reaches CoreAudio and WASAPI through SDK frameworks that need no dev package — so
`alsa-sys` is not in the dependency graph on those two legs at all.

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

Run the exact failing job's command from `.github/workflows/ci.yml`. Five of
the six jobs literally run a `just` recipe (see
[`docs/task-runner.md`](./task-runner.md)) as their `run:` step — there is no
translation to do, the job's own command **is** the recipe below. The sixth
(`xtask-structural-checks`) still runs raw `cargo`/`xtask` invocations. None
of the six need anything beyond the toolchain pin and (for anything
workspace-wide) `libasound2-dev`/`pkg-config`.

**If the red leg was `check-default (macos-latest)`, `just check` on the dev
machine already reproduces it** — same OS, same architecture, same pinned
nightly. A red `windows-latest` leg is the one with no local reproduction: there
is no Windows host here, so read the run's log and reason from it, or push a
branch and open a PR to iterate (a PR run cancels its own superseded runs, so
iterating there is cheap; pushes to `main` deliberately do not).

```bash
# whichever job went red
just check       # cargo check --workspace --all-targets
just check-all    # cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench
just check-seam   # cargo check -p lodestone-shell --no-default-features
cargo run -p xtask -- check-isolation
cargo run -p xtask -- check-deletable v770   # or v47 / v340 / v735
just test         # cargo test --workspace --no-fail-fast
```

The `xtask-structural-checks` job's two commands have no dedicated recipe and
are not wrapped through `just xtask` either: the generic `xtask *args`
passthrough always adds `-q`, which would make the recipe a different command
from what this job has always run, not just a renamed one. Reproduce that job
with the raw `cargo run -p xtask -- …` lines above (`just xtask
check-isolation` and `just xtask check-deletable v770` are close but add
`-q`, so prefer the raw form when you specifically need this job's exact
output).

Locally, `just` picks up whatever `LODESTONE_TARGET_DIR`/`LODESTONE_JOBS` you
have set in your shell (per-agent private target dir, `-j 4` for multi-agent
courtesy — see `docs/build-caching.md`); CI sets neither, so a recipe run in
CI is the bare command with no `--target-dir` override and no `-j`.

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

### The three-OS matrix

**All three legs are green on a real runner**, as of run `31337815809`. Measured
wall times from that run, which are also the cost basis below:

| leg | status | wall |
|---|---|---|
| `check-default (ubuntu-latest)` | ✅ pass | 98s |
| `check-default (macos-latest)` | ✅ pass | 100s |
| `check-default (windows-latest)` | ✅ pass | 531s |
| the non-Darwin `#[cfg]` arm | ✅ proven **both directions** — locally by inverting the predicate (below), and on the runners by the Linux legs type-checking it and the Linux `test` job linking it | — |

It took three runs to get there, and each red was informative rather than noise:

1. **Run `31336089874`** — macOS green, Linux red. The gate's non-Darwin arm did
   not compile: two files keep the transcription-size assertion outside the
   reader, so `RUSAGE_INFO_V4_SIZE` was still referenced from ungated code
   (`E0425`). `check-all` and `wasm` went red with it, from the same cause.
2. **Run `31336570162`** — Linux, macOS and `check-all` green; Windows red on
   `sccache` failing to spawn `rustc` (`os error 206`), which is an upstream
   limitation and not our code.
3. **Run `31337815809`** — all three green with `RUSTC_WRAPPER` empty on Windows.

Windows is **5.4x slower than the other two legs** and that is expected, not a
new problem: it is the one leg with no compiler cache, for the reason in the
`sccache` section above.

**Read the first run rather than assuming this table.** A matrix that certifies
platforms nobody has built is worse than no matrix, and the Windows row above is
exactly that until a run replaces it.

### The `wasm` job failed on gitignored assets, and its own reporter hid why

Two defects, and the second is the reason the first was expensive. Worth reading in
that order, because the shape recurs.

**The reporter first.** Run `31337815809`'s `wasm` job failed at
`lodestone-web (trunk build)` and printed, in total:

```
      │ error from build pipeline
      │ 2026-08-09T21:50:37.386150Z ERROR error from build pipeline
```

That names no file, no cause, and no next step. `xtask`'s `wasm-check` was pushing
the captured build through an **anchored** filter — `line.starts_with("error")`,
plus a few `contains`, capped at 8 lines — and `trunk` prefixes every line with an
RFC-3339 timestamp and a level, so **nothing it writes starts with `error`**. The
`Caused by:` chain, which is where the answer was, is *indented on the lines after*
the headline and so was invisible to a per-line filter regardless. This is
`CLAUDE.md`'s "a shell pipeline will destroy the evidence you are about to reason
from" living inside one of our own tools: the transform that made output readable
invented a silence.

The fix is mechanical rather than a better regex, in both `xtask`'s port and
`scripts/wasm-check.sh`:

- every build writes its **full** output to `target/wasm-check/<name>.log` and the
  console prints a summary **of that file**, naming the path;
- the verdict is the process's **own exit status**, never a property of its output;
- matching happens on **ANSI-stripped** text (CI sets `CARGO_TERM_COLOR=always`
  globally, so a coloured `error:` does not start with `e`), and the child is also
  asked for `NO_COLOR`;
- a matched line brings its **indented continuation lines** with it;
- when **nothing** matches, the tail is printed **verbatim**. A filter that can
  yield an empty summary turns a failing build into a silent one, and output that
  prints nothing must read as a failure to run, never as an absence of findings.

`xtask`'s three `diagnostic_selection_*` tests gate this against `trunk`'s real
output, captured verbatim from a reproducing run; one of them carries the anchored
predicate as a **control** and requires it to miss the cause, so the test cannot
quietly stop measuring anything.

**Then the actual failure.** `web/index.html` carried two
`data-trunk rel="copy-file"` links pointing into `../.cache/mc/26.2/` —
`client.jar` (39 MB) and `generated/reports/blocks.json` (6.8 MB). `.cache/` is
gitignored, so **no runner has ever had it**, and a `copy-file` link is a hard
build-time dependency: `trunk` failed in **0.33s**, before compiling anything, with
`error getting canonical path for "…/client.jar"` / `No such file or directory`.
Every contributor's first `trunk build` failed the same way.

It reproduces exactly in a throwaway `git worktree add --detach`, which by
construction has no gitignored files — a cheaper and more faithful runner
stand-in than editing paths by hand, and worth reaching for whenever a failure
smells like "present on my machine".

The fix moves the two files from a mandatory `copy-file` link to a **conditional
`post_build` hook** in `web/Trunk.toml`, which stages them only if they exist and
otherwise prints one named line per absent file and exits 0. Nothing is lost,
because the *runtime* already handled their absence: the page reports
`ASSET LOAD FAILED` and draws nothing, deliberately, so a synthetic stand-in can
never be mistaken for a working session. The failure simply moved to where it can
be told apart from a broken build — the build cannot distinguish "this developer
has not populated `.cache/` yet" from "the browser bundle is broken", and the page
can. Verified both directions: assets present → both land in `dist/` at their exact
byte sizes; assets absent → exit 0 with the named notice.

### `sccache` cannot wrap `rustc` on Windows here

The Windows leg's first run failed with `sccache: error: failed to spawn Command
{…}` and **`os error 206`** — `ERROR_FILENAME_EXCED_RANGE`, "The filename or
extension is too long". The command in question is the `lodestone-shell` `rustc`
invocation, which carries several hundred
`-L dependency=D:\a\lodestone\lodestone\target\debug\build\…\out` flags and
exceeds Windows' 32,767-character command-line limit. Cargo can spawn that
command; `sccache` re-spawning it cannot.

So the Windows leg sets `RUSTC_WRAPPER: ""` — the escape hatch this file already
documented — and skips `sccache-action` entirely. Two things make that work and
are worth keeping:

- An environment variable beats a config file in Cargo's precedence, which
  matters because the **committed** `.cargo/config.toml` has an unconditional
  `build.rustc-wrapper = "/opt/homebrew/bin/sccache"` — a path that exists on no
  runner at all. The empty env var disables the wrapper rather than leaving cargo
  pointed at a missing binary.
- The per-leg value comes from a `matrix.include` entry, **not** from
  `${{ matrix.os == 'windows-latest' && '' || 'sccache' }}`. That expression looks
  right and is a trap: an empty string is falsy in a GitHub expression, so the
  `&&` yields it, the `||` then fires anyway, and all three legs get `sccache`.

The cost is that the Windows leg builds without a compiler cache. Given it is the
2x runner and not the 10x one, that was the cheaper trade than debugging an
upstream limitation.

### Checking the other arm without a Linux host

A `#[cfg]` gate is only tested if **both** arms compile, and the arm this machine
cannot select is the one that breaks. Cross-compiling is not the way to check it
here: `cargo check --target x86_64-unknown-linux-gnu` from macOS dies in
`aws-lc-sys`'s build script for want of a cross C toolchain, which is a red for a
reason unrelated to the gate.

What works, costs nothing, and needs no new target is to **temporarily invert the
predicate**. Replace `target_os = "macos"` with `target_os = "linux"` across the
gated files and run `just check` on this Mac: the host now selects the
`not(...)` arm, whose token stream is identical to the non-Darwin arm the runners
compile. Follow `CLAUDE.md`'s neuter protocol — back the files up to the
scratchpad with an `md5` manifest first, keep the window inside a single shell
invocation, and restore by `cp` with an `md5` check plus a count of residual
flipped predicates, never with `git checkout`.

**This was not a formality: it caught two real breaks the first CI run had not
yet reached.** Both were the same shape — a `RUSAGE_INFO_V4_SIZE` reference
*outside* the region gated with the constant, in
`lodestone-worldgen/benches/generation.rs`'s `assert_instruction_counter_is_real`
and inline in `chunk_encode_cycles.rs`'s
`encode_cost_per_column_instructions_retired` — because those two files keep the
size assertion somewhere other than inside the reader, unlike the other three.
The generic lesson: when gating a constant, grep for **every** use of it, not just
the function you were editing; the compiler only tells you about the arm it is
currently building.

The workflow YAML is statically validated (`actionlint`, zero findings,
including its embedded `shellcheck` pass over every `run:` block, re-checked
after the `sccache` steps and the `pull_request`-only `cancel-in-progress`
fix were added). The `alsa-sys`/`libasound2-dev` finding was confirmed
empirically in an isolated container.

### Local, pre-`e0dc99f` figures (superseded)

An earlier pass measured all four `CLAUDE.md` commands plus
`check-isolation`/`check-deletable` in a `git worktree` at committed `HEAD`,
before the dev machine's build-infrastructure rework (`e0dc99f`: private
per-agent `--target-dir`s, `sccache` live, `[profile.dev] debug =
"line-tables-only"`, lower `opt-level` for deps). Those numbers (40s/19s/54s
for the three checks, 7s/2s for the xtask commands) are **not reproduced
here** because they no longer describe the current build — see the
post-`e0dc99f` table below instead. The one exception is `cargo test
--workspace`, which that first pass could not complete: it was killed by an
external `SIGTERM` (exit 143) at 1166s — the dev machine's own `target/`
cleanup, not a test failure — after 189 test binaries had already reported
zero failures. That run was redone to completion below.

### Post-`e0dc99f` figures, with a private target dir

Re-measured in a fresh `git worktree` at `origin/main` (`e0dc99f`, plus this
workflow's own commit `cffb86a`), using the now-standard per-agent build
isolation — **`--target-dir` as a flag, never `CARGO_TARGET_DIR` as an env
var**, because `sccache` hashes `CARGO_*` env vars into its cache key and the
env-var form measured 0% hits against the flag form's 78-94% (see
`docs/build-caching.md`) — and `-j 4` to bound rustc parallelism on a
10-core/16 GB machine shared with other agents:

| command | exit | wall time |
|---|---|---|
| `cargo test --workspace --no-fail-fast -j 4 --target-dir <private>` | **101** | 999s |

**This is a real result, not an infrastructure artifact, and it is not
green.** Exit 101 is a genuine `cargo test` failure report (`error: 1 target
failed: -p lodestone-shell --lib`), not a signal/exit-code fluke — confirmed
by grepping for `FAILED`/`panicked` rather than trusting the exit code alone:

- `app::tests::pressing_play_reaches_a_running_integrated_server` panicked:
  `the client never logged in to the integrated server; errors: []`.
- `sim::tests::extract_particles_does_not_hold_the_world_guard_across_the_per_particle_work`
  panicked: a wall-clock timing ratio landed at `3.58x` against a `3x` bound.

`lodestone-shell --lib` otherwise reported 950 passed, 46 ignored — these are
the only 2 failures in the entire workspace run, and every other crate's
test binary (including all of `lodestone-render`'s pixel gates and
`lodestone-server`'s worldgen suite, both of which never got a confirmed
green run in the earlier interrupted attempt) passed clean this time.

Both failures have real circumstantial evidence of being **timing/load
sensitive rather than logic regressions** — a login-wait deadline and a
wall-clock growth-ratio assertion, both run on the shared dev machine
alongside other agents' concurrent work rather than on a dedicated runner —
but circumstantial evidence is not the same as confirmation, and this file
does not own `lodestone-shell`'s tests. **Flagged as a separate task
(`task_5cfe780f`) rather than fixed or dismissed here**: whether they are
flaky (re-run in isolation) or a real regression is for whoever owns
`lodestone-shell` to determine, not for this CI doc to guess at. Until that
lands, a real (rare) flake in this specific pair on a GitHub-hosted runner —
which has no other agents contending for its CPU — is the most likely
outcome, but "most likely" is not "confirmed," and the `test` job should be
watched, not assumed green, until a CI run actually reports on it (the first
run's `test` job never got the chance — see below).

### The CI run itself (`30925426704`, the first ever)

| job | result | time |
|---|---|---|
| `check: --workspace --all-targets` | ✅ pass | 3m17s |
| `check: --all-features (allocbench excluded)` | ✅ pass | 2m48s |
| `check: lodestone-shell --no-default-features (version seam)` | ✅ pass | 2m39s |
| `xtask: check-isolation / check-deletable` | ✅ pass | 41s |
| `test: --workspace --no-fail-fast` | ❌ **cancelled**, not failed | 34m51s |

**Finding, not papered over: `test` was cancelled by this workflow's own
`concurrency` setting, not by a test failure.** The annotation is explicit —
"Canceling since a higher priority waiting request for
ci-CI-refs/heads/main exists" — because a second push to `main` landed while
the first run's 35-minute `test` job was still going, and the original
`cancel-in-progress: true` (scoped to the ref, not the event type) killed it.
On a busy shared trunk where pushes arrive every few minutes, a `test` job
that takes over half an hour will *never* get to report a real result under
that policy — it will always find a newer push waiting. **Fixed in this same
change**: `cancel-in-progress` is now `${{ github.event_name == 'pull_request'
}}`, so a PR still cancels its own superseded runs (the scenario the setting
exists for) but a push to `main` always runs `test` to completion. This was
caught by reading the actual run, exactly per "read the finished run and
report what happened" — `actionlint` cannot see this class of problem, since
the YAML is valid either way.

**`sccache` engagement, confirmed from the run's own logs, not assumed**: every
job's `Post Run mozilla-actions/sccache-action` step printed real
`--show-stats` output with nonzero `compile_requests` and 0 `cache_errors` —
e.g. `check-default`: 1330 compile requests, 810 executed, 167 hits / 639
misses (21%). The wrapper is genuinely intercepting `rustc`, not silently
bypassed. **One real nuance worth flagging rather than hiding**: the same
job's JSON stats reported **434 `cache_write_errors`** against only 205
successful `cache_writes` — a majority of attempted writes to the GitHub-Actions-
cache-backed `sccache` server failed, distinct from (and not counted in) the
`0 errors` the summary annotation shows, which only covers a different
counter (`cache_errors`). This is plausibly first-run contention: five jobs
writing to the same brand-new GHA cache namespace simultaneously is a known
rough edge for `sccache`'s GHA backend. It did not fail the build (writes
failing just means less is cached for next time, not a build error) and low
hit rates are expected on a cold cache regardless — but if hit rates stay low
on the *second* CI run too, this write-error count is where to look first,
not the hit-rate percentage alone.

Read wall-clock figures in both tables as machine/runner-dependent
snapshots, not a CI budget: `lodestone-server`'s worldgen tests
(`chunk::tests::parallel_generation_is_deterministic_and_matches_serial`,
several `worldgen_data::tests::*`) are individually CPU-heavy, and a GitHub
runner's core count and clock speed — and how warm its `sccache`/`rust-cache`
state is — will shift every number here run to run.

### The just-recipe conversion's first CI run (`30946172105`)

Item 3 of the `just` design (#432) landed: the four health-check jobs now
install a pinned `just` (`extractions/setup-just`, commit SHA, not a floating
tag) and run `just check`/`check-all`/`check-seam`/`test` instead of a
hand-copied `cargo` line. Read from the run itself, not assumed:

| job | result | time |
|---|---|---|
| `check: just check` | ✅ pass | 1m9s |
| `check: just check-all (allocbench excluded)` | ✅ pass | 1m24s |
| `check: just check-seam (version seam)` | ✅ pass | 58s |
| `xtask: check-isolation / check-deletable` | ❌ **fail** — pre-existing, unrelated | 22s |
| `test: just test` | ❌ **fail** — pre-existing, unrelated | 10m45s |

Confirmed from the job logs, not inferred: the `just test` step's own printed
command line was `cargo test --workspace --no-fail-fast  --target-dir
target` — no `-j` (the flag was empty, as it should be with `LODESTONE_JOBS`
unset) and the plain default `target` dir (no `LODESTONE_TARGET_DIR` reached
the runner). The env block logged immediately above that line lists only the
pre-existing `CARGO_TERM_COLOR`/`CARGO_INCREMENTAL`/`CARGO_PROFILE_*_DEBUG`/
`SCCACHE_*`/`RUSTC_WRAPPER` vars this workflow already set — no
`CARGO_TARGET_DIR` anywhere. The conversion itself changed nothing about what
ran.

**Both failures are pre-existing and unrelated to this conversion — verified
by reproducing each at committed HEAD in an isolated `git worktree`, before
either failure could be blamed on `just`, `extractions/setup-just`, or this
file's edits:**

- `xtask-structural-checks` (a job this change did not touch — it still calls
  raw `cargo run -p xtask` directly, see "How it works" above) failed on
  `check-isolation`: `lodestone-fuzz` depends directly on all four version
  crates (`lodestone-v47`/`v340`/`v735`/`v770`), which is exactly the
  shared-crate-depends-on-a-version-crate violation that check exists to
  catch. Real, and not caused by anything in this change.
- `test` failed on a single test, `measure_light_recompute_cost`
  (`crates/lodestone-world/tests/memory.rs:286`), a wall-clock-bound
  performance assertion (`column light recompute unexpectedly slow: 54.899
  ms`) — the same species of timing-sensitive test as the
  `sim::tests::extract_particles_…` flake logged above. It failed identically
  in a clean worktree at committed HEAD *before* this file was edited, so it
  is pre-existing, not a regression from the `just` conversion. The two
  chunk-streaming tests that were flagged as known-red elsewhere in this
  session (`dig_and_place_persist_through_forget_and_reload`,
  `real_client_view_follows_player_across_chunk_boundaries`) both **passed**
  in this run — that claim of red has gone stale; whoever owns them should
  re-check before assuming it still holds.

**A second run on the very next push (`30947402590`, this doc's own commit
`054e482`) confirms the pattern rather than the specific tests.** `check`,
`check-all`, and `check-seam` passed again; `xtask-structural-checks` failed
on the same `lodestone-fuzz` isolation violation. But `test` failed on three
*different* tests this time —
`app::tests::the_mouse_path_resolves_the_default_attack_and_use_buttons`,
`menu::key_binds::tests::every_control_has_a_row_and_every_row_but_the_footer_scrolls_into_view`,
`menu::key_binds::tests::six_categories_carry_all_twenty_seven_actions` — all
in `lodestone-shell`, all about a keybinds count (27 vs. an in-flight bump to
29 for two new verbs) that a same-day commit (`d9e5a9a`, landed *after* this
push) fixed. Two pushes, two unrelated `test` failures, neither touching
anything this conversion changed: on a trunk this fast-moving, "which test is
red" is not a stable signal — "check/check-all/check-seam pass, install-just
+ recipe-dispatch never causes the failure" is the thing actually being
verified here, and it held both times, from the recipe's own logged command
line (`cargo test --workspace --no-fail-fast  --target-dir target`, no `-j`,
no custom target dir) rather than from the pass/fail count.

## How to extend it

- **Add a check**: add a job, following the existing pattern (checkout →
  apt step if the job touches `lodestone-sound` → toolchain → rust-cache with
  its own `shared-key` → sccache-action → install `just` (pinned
  `extractions/setup-just` commit SHA, `just-version: "1.58.0"`) → the
  recipe). Keep one command per job; a job that runs three commands in
  sequence hides which one failed behind a single red X. If the check has no
  `just` recipe yet, add one to the `Justfile` first (`docs/task-runner.md`)
  rather than calling raw `cargo` here — a job that bypasses `just` is exactly
  the drift this conversion exists to close off.
- **Add a platform**: add the runner label to `check-default`'s `matrix.os`.
  Keep `fail-fast: false` — with the default `true`, the first leg to go red
  cancels the others, so you learn one thing per run instead of three and the
  cancelled legs report as neither pass nor fail. Any step that is not portable
  needs an `if: runner.os == '…'` guard (the `apt-get` audio step is the existing
  example, and an ungated `apt-get` fails a macOS or Windows leg outright), and
  give the leg its own `shared-key` suffix so `Swatinem/rust-cache` does not
  thrash one entry between OSes. Do **not** duplicate the other five jobs onto
  the new platform without saying which axis that job varies — see "Which
  platform catches what" above.
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
- `rust-toolchain.toml` — **the toolchain every job actually compiles with**, and
  it wins over the workflow's `toolchain:` input rather than agreeing with it. The
  previous version of this entry said the two should be kept in sync manually and
  that "a mismatch would fail CI immediately on the next push, which is the
  mismatch announcing itself". **That is false, and demonstrably so: they have
  mismatched (1.95.0 vs `nightly-2026-08-07`) across every green run in this
  workflow's history.** An override is silent by design — it is the resolution
  rule, not an error — so nothing announces it and there is nothing to keep in
  sync. Change the compiler here; the workflow input is inert either way.
- `Cargo.lock` — committed, so `Swatinem/rust-cache`'s cache key (derived
  from it) changes exactly when dependencies do.

## Dependencies

- [`dtolnay/rust-toolchain`](https://github.com/dtolnay/rust-toolchain) —
  installs the pinned toolchain.
- [`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) — caches
  `~/.cargo` and `target/` between runs.
- [`mozilla-actions/sccache-action`](https://github.com/mozilla-actions/sccache-action)
  (pinned `@v0.0.11`) — installs `sccache` and a GitHub-Actions-cache-backed
  server; see "A pending coordination point: sccache" above.
- [`extractions/setup-just`](https://github.com/extractions/setup-just)
  (pinned to a commit SHA, `just-version: "1.58.0"`) — installs `just` in the
  four jobs that call a recipe, so the job's own `run:` line can be the
  recipe name (`docs/task-runner.md`) instead of a hand-copied `cargo`
  invocation.
- `actions/checkout@v4` — standard checkout.
- No self-hosted runners, no repository secrets, and no third-party service
  beyond GitHub Actions itself and the actions above.
