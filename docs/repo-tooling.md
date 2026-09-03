# Repo tooling: the task runner, CI, build caching, and the xtask scanners

## What it is

The tools that keep this workspace buildable and testable at scale: the `just` task runner
that gives every health check a short canonical name, the GitHub Actions CI workflow that
verifies pushes without contending for the shared dev machine, the `sccache`/private-target-dir
build-caching design that lets many agents build concurrently in one checkout, and the
`cargo xtask` static scanners (`islands`, `world-coverage`, and their siblings `connectedness`,
`check-ptr-const`, `wasm-check`) that catch classes of defect no compiler check can see.

## How it works

### The task runner (`just`)

The root `Justfile` is a thin **naming** layer, not a build system: `xtask` owns anything that
parses Rust/workspace structure or needs its own test, `just` owns the one-to-three-line
canonical invocation, and `scripts/*` owns any script body at its existing path (kept there
deliberately — dozens of docs already reference `scripts/…` paths by name, so leaving script
bodies in place is what keeps those links correct). `just check`/`check-all`/`check-seam`/`test`
, plus `check-comment-voice`, are exactly the five commands this project's own working rules
require, and `just health` runs all five in order; `just -n <recipe>` prints a recipe's expanded
command with no side effects,
which is how to verify a recipe stays byte-for-byte faithful to the raw command it names.

Every cargo-invoking recipe passes `--target-dir {{tdir}}` and `-j {{jobs}}`, both sourced from
`LODESTONE_TARGET_DIR`/`LODESTONE_JOBS` environment variables that `just` interpolates into the
command line **before** cargo runs — so cargo only ever sees the flag form, never the
environment variable. This is load-bearing, not stylistic: the flag form measures 78–94%
`sccache` hits where the equivalent `CARGO_TARGET_DIR` environment variable measures near 0%,
because `sccache` hashes `CARGO_*` env vars into its own cache keys. `LODESTONE_JOBS` defaults
to empty (cargo's own default), never a hardcoded number, so a recipe never silently throttles a
CI runner or an otherwise-idle machine. `just run` (native) and `just run-wasm` (the browser
target, driven by `trunk` against `web/`'s own separate Cargo workspace) are deliberately
separate recipes rather than one parameterized command, because they share no invocation to
parameterize — `trunk` takes different flags entirely and `web/` never touches the shared
`target/` lock. Regeneration recipes (`regen-docs-index`, `regen-collision`, `regen-hardness`,
...) all follow the same generate-offline/drift-check-online shape: a committed artifact is
derived from an authoritative source, a test asserts the committed file matches a fresh
regeneration, and `LODESTONE_REGEN=1` on that same test writes the fresh output back instead of
asserting.

### CI (`.github/workflows/ci.yml`)

Runs on every push to `main` and every pull request, so an agent can push and let a hosted
runner verify the five canonical health checks instead of running heavy builds on the one
shared dev machine. It is **not** a replacement for the live/GPU gates, which still need a real
GPU adapter, a fetched vanilla jar, or a running oracle server — none of which exist on a hosted
runner — and stay exactly as `#[ignore]`d as they are locally; CI proves the hermetic majority of
the suite on every push, the rest stays a local, explicit, opt-in run.

Six jobs run in parallel, each varying exactly one axis rather than repeating another job's
question on a different platform: a three-OS matrix (`ubuntu`/`macos`/`windows`) for the
baseline `cargo check --workspace --all-targets`, since only the *platform* axis can change that
check's outcome; and, Linux-only, one job each for all-features, the version-seam
(`--no-default-features`, proving no protocol family is required), the `xtask` structural
checks (dependency-direction and folder-deletability), the full test suite, and a wasm32
tripwire. GitHub bills non-Linux runner minutes at a real multiplier (roughly 10x for macOS, 2x
for Windows), so the three-OS matrix costs an order of magnitude more than its Linux-only
siblings for one job's worth of coverage — worth remembering before adding a platform to a job
that does not vary by platform.

**`cargo check` never links**, so no `check` job on any OS can see an unresolved symbol; only
the Linux `test` job actually links every test/bench binary. A handful of test/bench-only sites
name a macOS-only libSystem symbol (`proc_pid_rusage`, used for instructions-retired
measurements) in an unconditional `extern "C"` block, which compiles fine everywhere and fails
only at link time — gated per-item with an explicit non-Darwin panic arm, not a whole-file
`cfg`, since some sibling functions in the same file are not measurements and must still compile
and run on every platform. More generally, **a test passing here and failing on a hosted runner
can differ on axes other than the OS name**: the codegen backend (Cranelift, this workspace's
debug backend, lacks a lowering for one SSE intrinsic that a font-rasterizing dependency's
`simd` feature reaches on x86 only), float semantics (a negative-input `sqrt`'s NaN sign bit
differs between aarch64 and x86_64), and `cfg!` read from inside the function under test rather
than passed as a parameter (silently resolving differently depending on which machine runs the
test). None of these show up in `cargo check` or a wasm hazard census.

CI installs `libasound2-dev`/`pkg-config` on Linux before the toolchain step, since `cpal`'s
Linux audio backend needs them at build time and every other backend needs no system package at
all — the step is `if: runner.os == 'Linux'` because `apt-get` does not exist on the other two
runners. A repo-wide, unconditional `sccache` wrapper in `.cargo/config.toml` means every job,
including CI's, needs the `sccache` binary on `PATH` or the very first compile hard-errors; CI
satisfies this with `mozilla-actions/sccache-action`, setting `RUSTC_WRAPPER`/
`SCCACHE_GHA_ENABLED` as workflow environment variables (an environment variable always beats a
config-file value in Cargo's precedence, so this correctly overrides whatever absolute,
dev-machine-specific path the committed config file names).

**A subtle, load-bearing gotcha**: every job passes a `toolchain:` input to the toolchain-install
action, and that value is **not** the compiler any job actually uses — `rust-toolchain.toml`'s
own `channel` pin wins over it unconditionally, silently, with no error. The input cannot simply
be deleted either, because the underlying action declares it a required input and hard-errors at
the install step without one; the real compiler used is whatever `rust-toolchain.toml` names,
and any doc or comment claiming the two are "kept in sync" is describing something Cargo's own
config-precedence rules make impossible to verify by inspection — read the actual `rustc
--version` a job reports, never a `toolchain:` value in the YAML.

### Build caching (`sccache`, private target dirs, trimmed dev profiles)

Up to a dozen agents build concurrently in one shared checkout on one machine. Before this
design, every agent shared one `target/`, and cargo serializes concurrent builds on an exclusive
build-directory lock — a `cargo test` has been measured at 42+ minutes elapsed and 0% CPU, pure
lock-wait. **Private per-agent target directories dodge the lock; a shared `sccache` compiler
cache is what makes that affordable**, since the dependency graph then comes from cache instead
of being recompiled once per agent. The measured warm hit rate on a full workspace check is
above 90%; the flag-vs-env-var distinction above is what makes that possible. Trimmed dev
profiles (`debug = "line-tables-only"` for the workspace, `opt-level = 1` for third-party
dependencies) cut both wall time and per-agent `target/` size substantially, at the cost of a
slower incremental edit loop for the one or two crates where `opt-level = 1` bites hardest —
override it locally for just that package if it does (`--config
'profile.dev.package.<crate>.opt-level=0'`).

**This measurement is scoped to `rustc` invocations only, and that scope has a real blind
spot.** `sccache` is installed as `[build] rustc-wrapper`, which says nothing about what a build
script does with a C toolchain — a heavy vendored-C `-sys` crate is rebuilt from scratch in
*every* per-agent target directory, with no cache to offset it, which is a straight multiplier on
disk and time for exactly that class of dependency. The two practical mitigations: delete a
per-agent target directory as soon as a task finishes (nothing does this automatically), and
prefer removing a heavy `-sys` dependency outright over trying to cache it (see
`docs/accounts-and-join.md`'s TLS crypto provider section for one such removal). The binding
constraint this design does not touch at all is test-runtime memory — a single test binary has
been observed using several gigabytes of RSS, which is unrelated to anything caching or profile
tuning can fix.

### The `xtask` static scanners

Two general-purpose scanners complement the packet-specific `connectedness` (see
`docs/multi-protocol-seam.md` and `docs/packet-wiring.md`) by answering questions it structurally
cannot: `connectedness` only ever asks "does this clientbound packet reach anything."

- **`cargo xtask islands`** parses every source file in a crate with `syn` (never a hand-rolled
  lexer — three earlier scanners in this repo were each independently wrong about lifetimes) and
  reports functions/methods with zero production call sites, struct fields with zero production
  readers, fields whose every production assignment is a default-like value, and stray
  `#[allow(dead_code)]` sites. Resolution is by bare name, not by type — few false positives (a
  name genuinely written nowhere is a strong signal), but two unrelated items sharing a common
  name (`new`, `tick`) hide each other. "Production" versus "test" is tracked by realm, not just
  textual `#[cfg(test)]`, because a Cargo target under `tests/`/`benches`/`examples/` is Test
  realm by path alone, and an external `#[cfg(test)] mod tests;` puts the attribute on the
  *declaration* rather than inside the file it names. Known-derive traits (`Encode`, `Decode`,
  `Serialize`, ...) are excluded from the dead-field report, since macro-generated code reads
  and writes every field without this scanner ever expanding the macro. A default-only-field
  finding is a heuristic over literal syntax, not runtime behaviour, and does not see a field
  grown only through an intermediate binding or a chained accessor.
- **`cargo xtask world-coverage`** answers, for every entity type, block-entity type, and
  particle type the game registry names: does anything resolve real geometry for it? Its
  calibration case is a subject that had a ported pose matrix, a hitbox entry, a dedicated
  render-path branch, and its own draw counter, and drew nothing, because the type had no entry
  in the actual model-rig corpus — an island invisible to both `connectedness` (no packet
  involved) and a plain code read (an earlier audit read the code and missed it). Findings sort
  into four buckets — **drawn**, **stranded** (something in the draw surface names the subject
  and nothing renders it — the finding class), **absent** (a real, cheap-to-see gap; nothing
  names it at all), and **no vanilla rig** (nothing draws it here because nothing draws it in
  vanilla either, checked against the decompiled 26.2 renderer registration classes) — and the
  fourth bucket is what keeps the report actionable instead of restating the registry. **Every
  claim needs an anchor**: a rule names a file and symbol that must still exist, so a renamed or
  deleted renderer fails the run rather than silently vouching for nothing, and a rule resolving
  to zero subjects is also a hard failure, since an empty claim must never read the same as
  legitimate full coverage.

## How to change it, and the gotchas

- **Never reintroduce a `CARGO_*`-prefixed variable, a hardcoded shared target dir, or a fixed
  `-j`, anywhere in the Justfile.** Each defeats a specific measured property (cache hits,
  per-agent isolation, no throttling of idle/CI machines) invisibly — a recipe that "still
  works" gives no signal that one of these regressed.
- **Do not add a job to CI that runs the `#[ignore]`d live/GPU gates.** There is no jar, GPU, or
  oracle server on a hosted runner; such a job would either fail every run or need its
  assertions loosened, which is the class of change this project's evidence standards forbid.
- **`docs/README.md` drift**: regenerate it (`LODESTONE_REGEN=1 cargo test -p xtask
  docs_index_matches_committed`) whenever a doc's H1 or `## What it is` summary changes, and
  commit the regenerated index in the same commit as the doc change.
- **A skip must never look like a clean scan.** Both `islands` and `world-coverage` hard-fail
  (rather than silently reporting a shorter result) when a scan target is missing entirely or a
  large fraction of files fail to parse — mirroring the incident where a module-layout change
  made `connectedness` report a whole protocol family `SKIPPED` while still exiting 0.
- **Adding a new false-positive exclusion or reference shape to `islands`**: pair it with a test
  that plants the exact shape and asserts it stops being flagged, named after the false positive
  rather than the mechanism.
- **Adding a renderer to `world-coverage`'s claim tables**: prefer a mechanical rule (arm
  literals, suffix rule, variant list read from the AST) over a hand-maintained explicit list —
  a mechanical rule tracks the underlying table for free; an explicit one goes stale silently.
- **Over-claiming in `world-coverage` is invisible in the output** — a claim rule that is too
  broad turns a stranded subject into a falsely-drawn one, and nothing in the report shows it.
  Under-claiming merely produces noise you can see and correct.
- **`cargo xtask check-comment-voice`** (`xtask/src/comment_voice.rs`) fails on a comment or doc
  comment written in the voice of the change that introduced it: a bare `#123`-shaped issue
  reference, or a word-bounded, case-insensitive "this change"/"this commit"/"this patch"/"before
  this change"/"this PR". Both rot the same way -- they read as authoritative long after they stop
  being accurate, because both were true only at the moment they were written. It is the fifth
  `just health` check and runs in CI's `xtask-structural-checks` job. Exceptions are recorded in
  `xtask/check-comment-voice.toml`, each with an `owner` and a `reason`; a stale entry (matching
  zero hits) is reported, not silently ignored, which is what makes shrinking the allowlist
  file-by-file tractable.

## Configuration

- `LODESTONE_TARGET_DIR` / `LODESTONE_JOBS` — per-agent private build directory and `-j` bound,
  read by `just` and spliced in as flags; unset means today's shared-`target/`, no-`-j` behavior.
- `LODESTONE_REGEN=1` — switches any generate-offline/drift-check-online test from assert to
  write, used throughout the regeneration recipes and the `docs-index` generator.
- `cargo xtask islands [--crate <name>]`, `cargo xtask world-coverage` — no environment
  variables of their own; both scan the whole workspace from the current directory unless scoped.
- `.cargo/config.toml`'s `[build] rustc-wrapper` — the repo-wide `sccache` pin; an environment
  variable (`RUSTC_WRAPPER=""`) is the sanctioned escape hatch for an environment without the
  binary, including one CI leg that cannot practically wrap `rustc` at all.

## Dependencies

- `casey/just` for the task runner; `cargo`, `xtask`, and `scripts/*` for everything it names.
- `dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `mozilla-actions/sccache-action`,
  `extractions/setup-just` in CI.
- `sccache` (native, native profile) as the compiler-cache wrapper; `syn`/`proc-macro2` (with
  the `visit` feature) for both AST-walking `xtask` scanners; `lodestone-data`/`lodestone-assets`
  as plain, version-free dependencies of `world-coverage` for the real registry populations and
  rig corpus, and the pinned 26.2 decompile under `.cache/` as `world-coverage`'s optional
  vanilla-oracle cross-check.
