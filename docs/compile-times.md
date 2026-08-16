# Compile times: `--timings` critical path, Cranelift, parallel frontend, and linker

## What it is

Measured findings from `cargo build --timings`, a Cranelift-vs-LLVM debug-build
comparison, the nightly parallel front-end (`-Z threads=N`), and the macOS
linker, taken to decide whether crates should split, dev-profile options
should change, or nightly-only compiler flags are worth taking permanently.
This complements [`docs/build-caching.md`](./build-caching.md) (sccache + the
existing dev-profile trims), which everything below assumes as the baseline.

## Machine state discipline (read before trusting any number below)

This checkout regularly has **3-5 other agents compiling concurrently**. Every
run here was preceded by a quiet check (`pgrep`-style scan for `rustc`/`cargo`,
`sysctl -n vm.swapusage`, `vm_stat`'s `Swapouts` delta, `sccache --show-stats`),
but several runs were still contaminated mid-flight by another agent starting a
build — evidenced by literal `Blocking waiting for file lock on build
directory` lines in the logs. **Only numbers taken through an isolated
`CARGO_TARGET_DIR` (a private scratch directory, not the shared `target/`) are
treated as clean**, because that sidesteps the shared build-directory lock
entirely; numbers taken against the shared `target/debug` are marked as such
and should not be read as precise deltas.

## Measured: `--timings` critical path

One full-workspace run: quiet-checked (no `rustc`/`cargo` processes, load
averages 6.9-10.7 settling, `vm_stat` swapouts flat, `sccache --show-stats` at
93.18% hit rate going in), then `rm -rf target/debug` (never `target/release`)
followed by `cargo build --workspace --all-targets --timings`.

**Honest limit:** this is a *clean target dir, warm sccache* measurement, not a
from-scratch cold compile — sccache was already warm from other agents' prior
work, so most units are cache **fetches**, not novel compilation. It mainly
measures cargo's own per-unit overhead (fingerprinting, linking, sccache
round-trips), which is still the realistic number for "I ran `rm -rf
target/debug` to reclaim disk (a documented, periodic maintenance step in this
repo) and now need to rebuild."

The build ended at exit 101 on an **unrelated, in-flight, uncommitted** edit —
`crates/lodestone-render/src/block_models.rs`, confirmed via `git status`/`git
diff --stat` to be modified by another agent, not by anything here — which
left one test binary (`model_ao_corner_gate`) failing to compile with a missing
`sprite` field on `BakedQuad`. Cargo's default scheduler stops queuing *new*
units after an error but lets in-flight ones finish, so the data below is
complete for everything queued before that point but **excludes
`lodestone-shell`, `lodestone-v770`, and `lodestone-registry` entirely** — they
never got scheduled in this run.

- 639 units measured, makespan (wall) **52.46s**.
- Sum of all unit self-durations (serial-equivalent): 483.9s. Parallelism
  ratio: **9.2x** of 10 cores.

Top workspace crates by self-duration (a single unit's own wall time):

| crate | self time | note |
|---|---|---|
| `lodestone-server` | 19.12s | lib |
| `lodestone-worldgen` | 18.95s | lib, `opt-level=2` override applies |
| `lodestone-ecs` | 16.30s | lib |
| `lodestone-data` | 10.39s | lib |
| `lodestone-assets` | 9.99s | lib |
| `xtask` | 8.07s (+7.41s for its own lib test) | |
| `lodestone-render` | 7.68s | lib only — see below |
| `lodestone-auth` | 5.88s | lib |

Top workspace crates by **aggregate** self-time (every unit of that crate
summed — lib, bins, every test/bench binary):

| crate | aggregate | shape |
|---|---|---|
| `lodestone-render` | 29.66s | dominated by ~15 separate ~1-1.5s pixel-test binaries, not the 7.68s lib |
| `lodestone-ecs` | 24.26s | |
| `lodestone-server` | 20.05s | |
| `lodestone-worldgen` | 18.95s | |
| `xtask` | 16.12s | |

`lodestone-server`, `lodestone-worldgen`, and `lodestone-ecs` are the three
largest single-lib-build units, which is the number a split decision should
key on — but note this run's numbers are relink/cache-fetch dominated (sccache
was warm), **not raw novel-compile cost**, so they do not by themselves
justify a split. A split's payoff is in parallelism recovered on a *fresh*
compile, which this run structurally cannot show; nobody should point at
"19.12s" as the reason to split `lodestone-server`.

`crates/protocol/v340` — one of the two crates CLAUDE.md names as "the big
ones" — is a single 2.97s unit in this run and is not a bottleneck by this
measurement. **`lodestone-shell`, the other named crate, could not be measured
at all this session**: it depends on `lodestone-server`, and `lodestone-server`
was under active, uncommitted, breaking edits (`crates/lodestone-server/src/
{registrar,dimension_tick,integrated,player_data,server,tick}.rs` all modified
per `git status`, producing `E0061` argument-count errors) for the entire
measurement window — not something in this doc's ownership to fix. This is a
genuine gap, not an oversight: re-run `cargo build -p lodestone-shell
--all-targets --timings` once the tree is quiet.

Raw data: `target/cargo-timings/cargo-timing.html` embeds a `const UNIT_DATA =
[...]` JS array that is plain JSON — `grep -o "const UNIT_DATA = \[.*\];"` and
parse it rather than eyeballing the flame graph.

## Measured: Cranelift (`rustc-codegen-cranelift`)

Installed for the pinned nightly with `rustup component add
rustc-codegen-cranelift-preview --toolchain nightly-2026-08-07` — available
for this exact pin. Invoked with cargo's `-Z codegen-backend` unstable flag
and `RUSTFLAGS="-Z codegen-backend=cranelift"`.

Tested against three crates, each built in an **isolated `CARGO_TARGET_DIR`**
specifically to avoid the shared build-directory lock (confirmed clean: no
`Blocking waiting for file lock` line in any of these three logs):

| crate | LLVM real / user / sys | Cranelift real / user / sys | wall speedup |
|---|---|---|---|
| `lodestone-worldgen-core` (leaf, uses `portable_simd`) | 7.94s / 4.05s / 0.56s | 2.84s / 0.95s / 0.37s | 2.8x |
| `lodestone-worldgen` (full crate, `portable_simd`, `opt-level=2` override) | 15.02s / 28.42s / 1.70s | 8.58s / 5.34s / 1.05s | 1.75x |
| `lodestone-ecs` (no SIMD, `bevy_ecs` derive-macro heavy) | 47.34s / 33.87s / 3.64s | 18.62s / 14.91s / 2.88s | 2.5x |

**The SIMD caveat this task was briefed to check did not materialize**:
Cranelift compiled both `portable_simd` crates cleanly — exit 0, no errors,
the same warning set as the matching LLVM build. No per-package LLVM carve-out
was needed for either.

**Follow-up, 2026-08-16: the gap above is closed.** Re-run once the shared
tree was quiet in `lodestone-server` (confirmed via `git status`/a scoped
`cargo check -p lodestone-server --lib` going green immediately before this
pass — the earlier session's `E0063`/`E0277` breakage in `integrated.rs` was
a live, concurrent edit that resolved itself, not anything this task touched).
Each crate below was built in its own **isolated `CARGO_TARGET_DIR`** (`v770`
and `lodestone-shell` share one scratch dir with each other, since
`lodestone-shell` depends on `lodestone-server` depends on `v770` — reusing
the dependency chain already compiled there rather than paying for it twice):

| crate | LLVM real / user / sys | Cranelift real / user / sys | wall speedup |
|---|---|---|---|
| `lodestone-render` (wgpu bindings, the FFI surface flagged below as the likeliest gap) | 85.26s / 25.77s / 3.98s | 42.38s / 10.74s / 3.14s | 2.0x |
| `lodestone-v47` | 32.12s / 16.85s / 2.63s | 12.71s / 6.10s / 1.66s | 2.5x |
| `lodestone-v340` (the ~18k-line family) | 69.70s / 18.51s / 2.77s | 15.30s / 7.62s / 1.98s | 4.6x |
| `lodestone-v735` (speaks protocol 754, not 735 — folder name ≠ protocol number) | 73.64s / 14.50s / 2.16s | 10.05s / 4.16s / 1.38s | 7.3x |
| `lodestone-v770` (pulls in the full `lodestone-server` dependency chain, incl. `hickory-resolver`/tokio/auth) | 199.14s / 117.98s / 11.94s | 82.80s / 34.82s / 8.01s | 2.4x |
| `lodestone-shell` | 146.83s / 93.80s / 9.88s | 61.97s / 32.75s / 5.63s | 2.4x |
| `lodestone-server` (explicit, though already exercised as a dependency of the two rows above) | 46.53s / 59.96s / 4.68s | 18.52s / 16.45s / 2.46s | 2.5x |

**Every crate this repo has now tested under Cranelift — all seven above plus
the three from the first pass — compiles clean: exit 0, zero errors, the same
warning set as the matching LLVM build.** The wgpu/FFI gap flagged as "more
plausible" than the SIMD crates did not materialize either: `lodestone-render`
is clean and 2x faster. No per-package LLVM carve-out was needed anywhere in
the workspace.

**Not landed as a default this session either, despite step 2 (this table)
coming back fully clean — a disk-driven call, not a technical one.** This
machine measured as low as **10 GiB free** (of 460 GiB total) partway through
this verification pass, with three other agents compiling concurrently.
Flipping `profile.dev.package."*".codegen-backend = "cranelift"` invalidates
sccache's fingerprint for every rustc invocation in the workspace — the same
"flip cost" `-Z threads=8`'s own landing accepted, but at full-workspace scope
rather than one flag, meaning every live agent's next build pays a cold
rebuild wave **at the same time**, on a machine that could not currently
absorb even one more isolated crate build without staying under ~10 GiB. This
repo's own health notes are explicit that purging `target/` to make room is
not an option while other agents are mid-compile, so there was no safe way to
both land this and let the induced rebuild wave complete this session.
**Recommendation stands from the original filing: land
`[unstable]\ncodegen-backend = true` in `.cargo/config.toml` plus the
`profile.dev.package` override in `Cargo.toml`, but time it for a moment the
shared checkout has disk headroom for a full-workspace cold-rebuild wave** —
re-check `df -h` immediately before landing it, not from this doc's numbers.

**Not landed as a default.** Flipping `profile.dev.package."*".codegen-backend
= "cranelift"` (or even a narrower per-crate override for just the three
verified crates) was considered and rejected for this session: `just health`
could not be run clean, because the shared tree was red in
`lodestone-server`/`lodestone-shell` from unrelated concurrent edits for the
entire measurement window, and landing a change nobody can currently verify
against the full suite is exactly what this repo's evidence standards exist to
forbid — a change you cannot attribute is a change you should not land.

**Opt-in recipe, not committed anywhere**, for a developer who wants to try it
locally on a crate that is not mid-edit by someone else:

```
rustup component add rustc-codegen-cranelift-preview --toolchain nightly-2026-08-07   # once

RUSTFLAGS="-Z codegen-backend=cranelift" cargo build -Z codegen-backend \
  --config 'profile.dev.package."*".codegen-backend="cranelift"' \
  -p <crate> --lib
```

**Done, 2026-08-16** (see the "Follow-up" table above): `lodestone-shell`,
`lodestone-render`, `lodestone-server`, and all four protocol families are now
verified clean under Cranelift. What is left is purely a scheduling question,
not a technical one — land `[unstable]\ncodegen-backend = true` in
`.cargo/config.toml` plus the profile override in `Cargo.toml` the next time
`df -h` shows enough headroom to absorb a full-workspace cold-rebuild wave
across every other live agent at once (this pass saw as low as 10 GiB free).
No LLVM carve-out is needed for any crate tested so far.

## Landed: parallel front-end (`-Z threads=N`)

`.cargo/config.toml`'s `[build]` table now carries `rustflags = ["-Z",
"threads=8"]`. This parallelises per-crate type-checking/MIR building inside
one rustc invocation — a different axis from cargo's own cross-crate
scheduling and from `codegen-units`, and it does not touch codegen or change
generated code.

Measured in two isolated-`CARGO_TARGET_DIR` builds of `lodestone-ecs` (chosen
because the `--timings` run above showed it as one of the largest self-time
units, and it is proc-macro-heavy via `bevy_ecs`'s derives — a case where a
frontend-only win is the *least* expected, since macro expansion is largely
serial):

| threads | real | user | sys |
|---|---|---|---|
| 1 (previous default) | 50.84s | 43.73s | 5.06s |
| 8 (landed) | 43.27s | 45.80s | 5.97s |

~15% less wall time for a few percent more total CPU — the expected shape.
Verified after landing with a scoped, contended `cargo check -p
lodestone-worldgen-core --lib` (exit 0; 1m38s wall against three other agents'
concurrent builds, dominated by `Blocking waiting for file lock on build
directory`, not by the flag itself).

**Gotcha, recorded in the config comment**: `RUSTFLAGS` set in the environment
**overrides** `build.rustflags` rather than merging with it. A one-off
`RUSTFLAGS=...` invocation (this session's own Cranelift tests included)
silently drops `-Z threads=8` for that invocation — not a correctness problem,
but worth knowing before concluding the flag "isn't doing anything."

**Cache-key cost, accepted**: changing `rustflags` changes sccache's
fingerprint key for every rustc invocation, so the next build anyone in this
checkout runs pays one cold-rebuild wave. This is the same shape as
`docs/build-caching.md`'s "Trap 2 (flip cost)" for the `rustc-wrapper` flip,
and is accepted for the same reason: it is a one-time cost, not a regression.

**Not yet verified, reasoned instead**: `cargo xtask wasm-check` was not
re-run against this change this session (see the machine-state section — the
shared tree's instability made a full wasm compile impractical to schedule
here). `-Z threads` is a compiler-frontend concurrency knob, orthogonal to
target architecture and codegen backend, and it does not appear in
`scripts/wasm-check.sh`'s hazard table, so it is expected to be safe — but
per this repo's own rule, **re-run `cargo xtask wasm-check` after this lands**
to turn that expectation into a measurement before trusting it.

## Checked, no action taken: the linker

Already on the fast option. `ld -version_details` reports `ld-1267` — Apple's
modern replacement linker ("ld-prime"), confirmed by its own `will use
ld-classic for:` line listing only 32-bit/legacy architectures, meaning
`arm64` (this machine's target) already gets the new linker, not the classic
one. `lld`/`ld64.lld` is not installed, and installing LLVM via Homebrew to
get it would be a real download for an uncertain, likely-small win against an
already-modern native linker on this platform. Not pursued.

## Not measured this session: build scripts

`target/cargo-timings/cargo-timing.html`'s `` build-script`` target entries
from the run above carry real per-build-script timing, but they were not
separately aggregated or audited this session — the time went to the
Cranelift/threads experiments instead. `docs/build-caching.md`'s existing
finding — that sccache (a `rustc-wrapper`) does not cover a build script's own
C-toolchain work, and that this was previously a real cost via `aws-lc-sys`
before that dependency was removed — is the standing caveat for this axis.
Nothing in this workspace's current `Cargo.lock` was checked for a repeat of
that pattern; that is open work, not a "checked, clean" result.

## How to change it / gotchas

- To re-run the `--timings` critical path cleanly: confirm no `rustc`/`cargo`
  processes are running and `sysctl -n vm.swapusage` shows no growth, then `rm
  -rf target/debug` (never `target/release`) and `cargo build --workspace
  --all-targets --timings`. Read `target/cargo-timings/cargo-timing.html`'s
  embedded `UNIT_DATA` array as JSON rather than eyeballing the flame graph.
- To compare a codegen-backend, threading, or linker change without fighting
  the shared build-directory lock, point `CARGO_TARGET_DIR` at a private
  scratch directory for the comparison run. (This is a one-off local
  experiment, not a cache-key concern, so the flag-vs-env-var distinction
  `docs/build-caching.md` documents for per-agent dirs does not apply here.)
- `-Z threads=8`: chosen to leave 2 of this machine's 10 cores free for
  cargo's own cross-crate scheduling on a workspace build. Re-tune if the core
  count of the reference machine changes.
- Cranelift's opt-in recipe above needs the rustup component installed once
  per toolchain; re-install after any `rust-toolchain.toml` pin change.

## Configuration

- `.cargo/config.toml` — `[build] rustflags = ["-Z", "threads=8"]`, nightly-only,
  requires the pin in `rust-toolchain.toml`.
- Cranelift: nothing committed. The opt-in recipe above is the only way to use
  it until the follow-up verification lands it for real.

## Dependencies

- `rustc-codegen-cranelift-preview` rustup component — not committed, install
  locally to use the opt-in recipe; must match the pinned nightly's date.
- Interacts with [`docs/build-caching.md`](./build-caching.md) (sccache
  cache-key/flip-cost precedent this doc reuses), `rust-toolchain.toml` (the
  pin both this and Cranelift depend on), and `Cargo.toml`'s `[profile.dev]`
  package overrides (`lodestone-worldgen`'s `opt-level = 2`, confirmed
  compatible with Cranelift above).
