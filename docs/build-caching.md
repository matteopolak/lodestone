# Build caching (`sccache`), dev profiles, and multi-agent build contention

## What it is

The measured design for how up to eleven agents build concurrently in one
shared checkout on a 10-core / 16 GB machine: a repo-level `sccache`
compiler-cache wrapper (**active in `.cargo/config.toml` since 2026-08-04**),
per-agent private target dirs via the `--target-dir` flag, trimmed dev
profiles in the root `Cargo.toml`, and a cleanup-on-finish policy. This doc
is the record of what was measured, what was decided from it, and the honest
limits of both.

All numbers were measured 2026-08-04 on the dev machine (M-series, 10 cores,
16 GB), the decisive ones on a cleared box (load ~3–15, stated per
experiment; `docs/worldgen-surface-perf.md` documents 3× wall-clock outliers
under load, and a leg measured mid-stampede at load 93 reproduced exactly
that). `sccache --show-stats` hit/miss counts are load-independent and are
the numbers to trust over any wall time.

## The design, in one block

Every agent brief carries this:

```
Build in a PRIVATE target dir, never the repo's `target/`:

1. Choose your dir once at task start: /tmp/lt-<issue>-<4 random chars>
   (example: /tmp/lt-427-k3f9). Write the literal path in every command —
   shell variables do not survive between tool calls.
2. Add `--target-dir` and `-j 4` to EVERY cargo command:

   cargo check --workspace --all-targets -j 4 --target-dir /tmp/lt-427-k3f9
   cargo test -p <crate> --no-fail-fast -j 4 --target-dir /tmp/lt-427-k3f9

   - The --target-dir FLAG form is mandatory. NEVER export CARGO_TARGET_DIR
     as an env var: sccache hashes CARGO_* env vars into its cache keys, and
     the env-var form measured 0% cache hits where the flag form measured
     78-94%.
   - -j 4 bounds rustc parallelism. Without the shared-target lock there is
     no accidental admission control left, and the machine is 10 cores and
     16 GB shared by everyone.
3. sccache is active via .cargo/config.toml — set nothing. Cold dep
   compiles hit the shared cache automatically.
4. Before finishing, delete your dir — and only yours, by its literal name:

   rm -rf /tmp/lt-427-k3f9

   Never glob /tmp/lt-*.

The `cargo xtask` alias has no --target-dir; use the expanded form:
   cargo run -q -p xtask -j 4 --target-dir /tmp/lt-427-k3f9 -- docs-index --check
```

**The Justfile (`docs/task-runner.md`) now bakes both of the above.** `just`'s
`xtask *args` recipe already runs the expanded `cargo run -q -p xtask
--target-dir … --` form — the alias's missing `--target-dir` is exactly why
that recipe exists — so `LODESTONE_TARGET_DIR=/tmp/lt-427-k3f9 just xtask
docs-index --check` is the same invocation as the hand-expanded one above,
without retyping it. Every other cargo recipe in the Justfile
(`check`/`check-all`/`check-seam`/`test`/`health`) reads the same
`LODESTONE_TARGET_DIR` env var and an optional `LODESTONE_JOBS` for `-j`, e.g.
`LODESTONE_TARGET_DIR=/tmp/lt-427-k3f9 LODESTONE_JOBS=4 just health`. `just`
interpolates the variable into the command line *before* cargo runs, so cargo
still only ever sees the flag form — the env var read by `just` is not the
one cargo sees. This is a convenience, not a new mechanism: the raw commands
above remain correct and are exactly what each recipe expands to (verify with
`just -n <recipe>`).

Why each piece is shaped that way is the rest of this doc.

## Why: the lock, not the compiler, was the bottleneck

All agents used to share one `target/`; cargo serialises concurrent builds on
an exclusive build-dir lock (`Blocking waiting for file lock on build
directory`). Observed on one day: a `cargo test -p lodestone-v770` at
**42m35s elapsed, 0.0% CPU** — pure lock-wait; two more cargos blocked 11 and
18 minutes at 0.0% CPU; a five-crate check taking 10m44s. sccache does not
touch the lock. **Private target dirs dodge it; sccache is what makes them
affordable**, because the dependency graph then comes from cache instead of
being recompiled per agent.

## Measured: sccache

- Graph (all-features): 593 packages — 560 registry deps, 33 workspace
  members; 37 proc-macro crates (ours: `lodestone-macros`), 91 build-script
  crates (ours: `lodestone-server`).
- ~87% of rustc invocations are cacheable; warm workspace-check hit rate
  **94.28%** (643/682). Non-cacheable: 68 `crate-type` (proc-macro dylibs +
  build-script executables — every *user* of `lodestone-macros` caches
  fine), 31 `incremental` (workspace crates), 8 misc.
- **A retracted figure, kept as a warning.** This bullet used to end *"C/C++
  inside `aws-lc-sys` etc. hit 99.6%."* **Distrust it.** `.cargo/config.toml`
  sets `rustc-wrapper` and **nothing else** — no `CC`, no `CXX` — so `sccache`
  never saw a C compilation from a build script, and a hit rate over
  compilations it never observed cannot mean what the sentence says. The figure
  is not merely stale; it contradicts the "what this does **not** cover" section
  below, which was written from the opposite direction after `aws-lc-sys` was
  measured being rebuilt from scratch in 25 target directories at once.
  Retracted rather than corrected because no re-measurement has been done, and
  guessing at what it *did* measure would just relocate the error.
- Cold worktree render-subtree check: **36.4s / 32.4s-user uncached vs
  16.4s / 8.1s-user warm** (86.5% hits) — 2.2× wall, 4× less CPU.
- Build-mode warm restore of a render-tests dir: **28.6s** wall, vs 39.1s
  cold-uncached under the same profile; priming overhead in build mode was
  ~zero (38.7s vs 39.1s).
- Edit-check loop: **parity** (wrapper 0.76/0.36/0.42s vs none
  0.50/0.36/0.35s). Workspace crates compile incrementally; sccache marks
  them non-cacheable and passes them through, so incremental compilation is
  NOT lost. Do not set `CARGO_INCREMENTAL=0`.
- **Trap 1 (env var):** `CARGO_TARGET_DIR` as an env var → 0% hits on
  byte-identical rebuilds. The `--target-dir` flag → 78–94%. sccache hashes
  `CARGO_*` env vars into keys; registry deps otherwise share across
  checkouts/worktrees because their sources live at one stable path.
- **Trap 2 (flip cost):** cargo hashes the wrapper into every fingerprint —
  adding or removing `rustc-wrapper` forces a full rebuild per target dir
  (verified: a green dir recompiled from `libc` up). The 2026-08-04 flip was
  free only because the 59 GB shared `target/` was being wiped anyway.
- Wrapper legs make `/usr/bin/time`'s user-time meaningless: rustc work
  moves into the sccache daemon, outside the measured process tree. Compare
  wall, or the daemon's own stats.

### The limit of all of the above: it is a *rustc* measurement (issue #446)

Everything in this section measures `rustc` invocations, because `sccache` is
installed as `[build] rustc-wrapper`. It says **nothing** about what a build
script does with a C toolchain, and that gap has a name: `aws-lc-sys`, which
vendored ~1,500 C translation units, was compiled by its own `builder/` rather
than by `rustc`, and was therefore rebuilt **from scratch in every target
directory**. Twenty-five concurrent copies were counted across per-agent
`/tmp/lt-*` dirs, and 137 GB of accumulated per-agent target dirs was half of
issue #446. Under that load a multi-minute build is indistinguishable from a
hang, which is exactly what the owner reported.

So the per-agent `--target-dir` policy above is a straight **N× multiplier** for
any `-sys` crate with a heavy build script, with no cache to offset it. Two
consequences worth keeping:

- **Delete your per-agent target dir when you finish.** Nothing does this
  automatically. It is the mitigation that actually recovered 81 GB.
- **Prefer deleting a heavy `-sys` dependency over caching it.** `aws-lc-sys` was
  removed outright — see [`tls-crypto-provider.md`](./tls-crypto-provider.md) —
  which deletes the whole class rather than making it cheaper.

One honest tension, flagged rather than resolved: the bullet above reports
*"C/C++ inside `aws-lc-sys` etc. hit 99.6%"*, which reads as though the C side
*was* cached. That figure came from `sccache --show-stats`' own C/C++ counters
and was never traced back to `aws-lc-sys`'s build script specifically, and
`.cargo/config.toml` sets only `rustc-wrapper` — it does not set `CC`, so
nothing routes the `cc` crate's invocations through sccache. Whichever reading is
right, it is now **moot for this repo**: the crate is gone from `Cargo.lock`
entirely. Do not cite that 99.6% as evidence that a future `-sys` crate will be
cached; measure it.

## Measured: dev profiles (landed in root `Cargo.toml`)

Render-tests build (`cargo test -p lodestone-render --no-run`, 214-unit
subtree, cold private dirs, wrapperless, back-to-back):

| profile | wall | user CPU | dir size |
|---|---|---|---|
| previous (debug=2 everywhere) | 28.0s | 124.5s | 2,263 MB |
| + `debug="line-tables-only"`, deps `debug=false` | 24.5s | 103.6s | 1,605 MB |
| + deps `opt-level=1` (landed set) | 39.1s | 214.7s | 1,233 MB |

Runtime, prebuilt binaries, same box:

- Render suite (633 tests): **29.5s → 21.2s (1.39×)** from the deps knobs.
- Worldgen suite: 11.4s → 4.1s.
- **The 144-chunk sweep** —
  `worldgen_data::tests::served_columns_never_carry_an_unported_badlands_variant`
  (`crates/lodestone-server/src/worldgen_data.rs`, a 12×12 chunk loop, the
  strongest worldgen gate in the repo): **203.42s → 27.16s (7.5×)**, and an
  isolation leg (`opt-level=2` on `lodestone-worldgen` alone, nothing else)
  measured **27.13s** — the entire win is that one line. Two days earlier
  this gate was 700.57s; memoisation (`6509a97`) plus this profile puts it
  at 27s, i.e. runnable on a whim.
- Deps `opt-level=1` costs 2.07× CPU on a *cold* dep compile — paid once
  machine-wide, then served from cache (warm restore = 28.6s, the same wall
  as the old profile's cold build).
- `split-debuginfo = "unpacked"` was considered and dropped: measured as
  already the macOS dev default (`cargo build -v` shows it with no config).
- `debug-assertions` untouched everywhere — correctness gates depend on it —
  and `opt-level` does not interact with it.

**Honest limit 1: `opt-level = 2` makes `lodestone-worldgen`'s own
incremental edit loop slower per edit.** Not measured. If you are iterating
*inside* the crate and it bites, override locally:
`--config 'profile.dev.package.lodestone-worldgen.opt-level=0'` — that
changes only your invocation, nobody else's keys.

## Disk math and the agent ceiling

- Free after the 59 GB shared-`target/` wipe: **153 GiB**.
- Measured per-agent dirs (landed profile): worldgen 420 MB, render-tests
  1,233 MB, server+worldgen 1,275 MB. Ten typical agents ≈ 13 GB — trivial.
- **Honest limit 2: a full-workspace `--all-targets` dir was NOT measured**
  under the new profile. Estimate: ~11 GB (the 59 GB dir was weeks of
  multi-config accretion; one old-profile config ≈ 20 GB × the measured
  0.545 size ratio). Rule of thumb: keep full-workspace dirs to **~5
  concurrent**; scoped `-p`-cluster dirs need no rationing.
- sccache cache: 94–129 MB measured per config/mode pair; `SCCACHE_CACHE_SIZE
  = "10G"` is >3× headroom over the ~1–3 GB estimated steady state.
- **The binding constraint is memory at test runtime, not disk, and nothing
  in this design touches it.** A `lodestone-shell` test binary was killed at
  **4,823 MB RSS** the same day this landed (free memory ~102 MB, compressor
  5.6 GB) — that is test content, not debuginfo. Bounding concurrent *test
  execution*, not just compilation, is the open problem.

## Cleanup-on-finish, not a slot pool

A pool of reusable per-slot dirs was considered and rejected: its entire
per-task saving is the warm restore, measured at ~29s wall for a
render-scale dir. Against that, a pool costs slot coordination, permanent
disk, and staleness accretion — the 59 GB shared dir is what accretion looks
like. Agents keep one dir across all steps of a task and delete it at the
end. Orphans are visible to the orchestrator as `ls -d /tmp/lt-*` (audit by
listing; never clean by glob — an active agent's dir looks identical to an
orphan).

## CI

The config-level wrapper means **every builder needs the `sccache` binary**,
CI runners included — a missing binary is a hard error on every cargo call.
CI satisfies this with **`mozilla-actions/sccache-action@v0.0.11`** (exact
org: `mozilla-actions`), which is complementary to `Swatinem/rust-cache`
(one caches compilation units, the other `~/.cargo` and `target/`). The
escape hatch for any environment without the binary, verified working:
`RUSTC_WRAPPER=""` in the environment cleanly overrides the config and
disables the wrapper. See `docs/ci.md` for the workflow itself.

## Configuration

- `.cargo/config.toml` — `[build] rustc-wrapper = "/opt/homebrew/bin/sccache"`,
  `[env] SCCACHE_CACHE_SIZE = "10G"` (applies on server start; does not
  override a pre-existing env var).
- Root `Cargo.toml` — `[profile.dev]` and the two package-override sections;
  comments there carry the per-knob numbers.
- `SCCACHE_DIR` — unset; defaults to `~/Library/Caches/Mozilla.sccache`.
- `sccache --show-stats` / `--zero-stats` — the only trustworthy hit-rate
  instrument; never infer cache behavior from wall time.

## What was NOT measured (route around at your peril)

- Full-workspace `--all-targets` dir size under the new profile (estimate
  above), and N-agent steady-state throughput on private dirs.
- Deps `opt-level=1` runtime effect outside the render/worldgen suites (the
  1.39× is one data point; shell/server suites are plausible but unmeasured).
- Worldgen's incremental edit-loop cost at `opt-level=2` (limit 1 above).
- Test-runtime RSS (limit 2 above) — unaddressed by design.
- A bare `touch` no longer dirties cargo 1.95 (content-hash freshness) —
  relevant when constructing edit-loop experiments; a `touch`-based A/B
  measures a no-op.

## Dependencies

- `/opt/homebrew/bin/sccache` 0.17.0 (Homebrew) locally;
  `mozilla-actions/sccache-action@v0.0.11` in CI.
- Interacts with: `docs/ci.md`, `CLAUDE.md` *Build and test* (the three
  health checks are the cache-priming workload), cargo 1.95 content-hash
  freshness.
