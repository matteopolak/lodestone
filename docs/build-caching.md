# Build caching (`sccache`) and multi-agent build contention

## What it is

A measured evaluation of `sccache` (Mozilla's compiler cache) for this repo's
specific problem: up to eleven agents building concurrently in one shared
checkout on a 10-core / 16 GB machine. Verdict, numbers, the adoption playbook,
and the traps found on the way. `sccache 0.17.0` is installed at
`/opt/homebrew/bin/sccache` (Homebrew). **It is deliberately not yet active in
`.cargo/config.toml`** — a commented, ready-to-flip block is there; read
"How to turn it on repo-wide" below before uncommenting it.

All numbers were measured 2026-08-04 on the dev machine (M-series, 10 cores,
16 GB), mostly during a coordinated freeze with load averages stated inline.
`docs/worldgen-surface-perf.md` documents a >10% spread on this machine with
loaded outliers reaching 3×; treat every wall-clock figure accordingly. The
`sccache --show-stats` hit/miss counts are load-independent and are the
numbers to trust.

## The verdict, in order of what actually matters

1. **The dominant cost of concurrent builds is cargo's exclusive lock on
   `target/`, and sccache does not touch it.** All agents share one
   `target/`; cargo serialises concurrent builds on
   `Blocking waiting for file lock on build directory`. Observed during the
   load-91 incident and after: a `cargo test -p lodestone-v770` at **42m35s
   elapsed, 0.0% CPU** (pure lock-wait, killed with owner clearance), three
   runs at ~15 min zero CPU, and a five-crate `cargo check -p
   lodestone-server --lib` taking **10m44s** that opens with the lock-wait
   line. The exact wait-vs-compile split *within* one contended build was not
   instrumented, but 0% CPU over 42 minutes is that split for the worst victim.
2. **sccache's real value here is making private (per-agent / worktree)
   target dirs affordable**, which is the thing that dodges the lock. A cold
   private build of the `lodestone-render` subtree (95 units): **36.4s / 32.4s
   user CPU** with no cache vs **16.4s / 8.1s user** with a warm cache
   (86.5% hits) — 2.2× wall, **4× less CPU**, back-to-back at load 5–8.
   CPU is the scarce currency on a 10-core box with eleven builders; wall
   ratios on an idle machine understate the contended benefit.
3. **It does roughly nothing for the warm shared-`target/` loop** — cargo
   already reuses those artifacts — **and nothing for the edit-check loop**
   (see incremental, below). It is a cold-build accelerator, and this repo
   manufactures cold builds constantly: `CLAUDE.md`-mandated detached-worktree
   re-verification, post-incident cleans (the 13 GiB stale-path rebuild),
   fingerprint stampedes, CI.

## Measured facts (the ones decisions rest on)

### The graph is ~87% cacheable, and the proc-macro fear is misplaced

Resolved graph (all-features): 593 packages — 560 registry deps, 33 workspace
members; 37 proc-macro crates (ours: `lodestone-macros`), 91 with build
scripts (ours: `lodestone-server`). On a real `cargo check --workspace`
(~790 rustc invocations), sccache classified **107 non-cacheable**:

| reason | count | what it is |
|---|---|---|
| `crate-type` | 68 | proc-macro dylibs and build-script *executables* |
| `incremental` | 31 | workspace crates (compiled incrementally) |
| `missing input` / other | 8 | misc |

Everything else — 682 units, including every crate that merely *uses*
`lodestone-macros`/serde-derive, the compiled output of build-script crates,
and the C code in `aws-lc-sys` etc. (C/C++ hit rate 99.6%) — is cacheable.
Warm-cache re-run: **94.28% hits (643/682)**. "sccache can't cache proc
macros" is true only of the 37 macro crates' own tiny dylib compiles.

### Incremental compilation is NOT lost

sccache marks incremental compiles non-cacheable and passes them through to
real rustc unchanged; cargo only compiles *workspace* crates incrementally and
registry deps are never incremental. So the trade the internet warns about
mostly doesn't exist at default settings: deps get cached, workspace crates
keep incremental. Measured edit-loop (append comment to
`lodestone-render/src/lib.rs`, `cargo check -p lodestone-render`, private
worktree): wrapper 0.76/0.36/0.42s vs no wrapper 0.50/0.36/0.35s — parity
within noise, ~0.2s first-call server-spawn overhead. No-op workspace check:
0.31s vs 0.29s. Do not set `CARGO_INCREMENTAL=0`; nothing here needs it.

### The `CARGO_TARGET_DIR` env var silently poisons every cache key

sccache hashes `CARGO_*` environment variables into the compile key.
Measured: byte-identical rebuild with `CARGO_TARGET_DIR` set to a different
path → **0% hits**; same rebuild using the `--target-dir` *flag* → **78%
hits** (all registry deps; misses were the workspace-path-dependent units);
identical rerun, same path → 100%. **Always use `--target-dir <dir>`, never
the env var**, for private build dirs, or every agent gets a private cache
that shares nothing. Registry deps hit across *different* checkouts and
worktrees because their sources live at one stable path
(`~/.cargo/registry`); workspace crates re-key per checkout path (and are
incremental anyway).

### Turning the wrapper on/off invalidates every fingerprint

Cargo hashes the wrapper into each unit's fingerprint. Measured: a green
no-wrapper target dir immediately recompiles from `libc` up when the same
command runs with `RUSTC_WRAPPER` set. Consequences:

- Landing (or reverting) `build.rustc-wrapper` in `.cargo/config.toml` forces
  **one full rebuild of the shared 54 GiB `target/` for every agent at once**.
  It must be scheduled, not slipped in.
- Never ad-hoc-run the wrapper against the shared `target/` — you flip
  everyone's fingerprints twice (on and back off). Private `--target-dir`
  only, until the repo-wide flip.

### Costs

- First-ever (cold-cache) build pays ~**+25%** wall for cache writes
  (24.9s → 31.2s on the workspace-check workload).
- Disk: one check-mode config cached = **129 MB** compressed. `SCCACHE_CACHE_SIZE`
  is set to **20G** in the commented block: enough for check+build+test modes
  across the three health-check feature configs with LRU headroom, and small
  against the ~99 GiB free (the 54 GiB shared `target/` is the actual disk
  problem). Default location `~/Library/Caches/Mozilla.sccache`.

## How to use it today (opt-in, safe now)

For exactly the builds that are currently painful — detached-worktree
verification, or a single-crate check while the shared `target/` lock is held
by someone else:

```bash
RUSTC_WRAPPER=/opt/homebrew/bin/sccache \
  cargo check -p <crate> --target-dir /tmp/<unique-private-dir>
```

`RUSTC_WRAPPER` as an env var is safe (it is not a `CARGO_*` var and is not
hashed into keys). `--target-dir` as a flag is required (see above). The
first agent to do this per graph pays the +25% priming; everyone after gets
the 2–4×. This never touches the shared `target/` or anyone's fingerprints.
The old worktree scar — `CARGO_TARGET_DIR` pointed at the *shared* target
from a throwaway worktree, baking dead paths into build-script output (435
phantom errors, 13 GiB rebuild) — does not apply: private dir, flag form,
and the worktree dies with its own target dir.

## How to turn it on repo-wide (the playbook)

Uncommenting the block in `.cargo/config.toml` is correct only inside a
coordinated quiet window, because of the fingerprint stampede:

1. Freeze agent builds (orchestrator).
2. Uncomment the `[build] rustc-wrapper` + `[env]` block in
   `.cargo/config.toml`.
3. Prime: run the three health checks from *Build and test* in `CLAUDE.md`
   once each. This is the stampede, absorbed while nobody is waiting; the
   cache warms for all three feature configs.
4. **CI**: `docs/ci.md` §"A pending coordination point" — the workflow
   assumes no wrapper. A runner without the binary hard-errors on every
   `cargo` call. Either install sccache in the workflow (e.g.
   `mozilla/sccache-action`, complementary to `Swatinem/rust-cache`) or
   override the wrapper off in the workflow env before this lands. Do not
   land the flip without the CI owner.
5. Thaw.

## What was NOT measured (route around at your peril)

- A green full-workspace A/B: the tree had a broken `lodestone-shell` lib
  mid-edit, so the three workspace-check legs each fail-fast-truncated at the
  same point (~350 units started). The legs are mutually comparable but
  understate full-graph absolutes.
- Build-mode (codegen) hit rates and cache size at scale — check-mode only.
- Steady-state throughput with N agents on private dirs (the memory question:
  private dirs bypass the lock's accidental admission control; if several
  agents cold-build simultaneously, cap with `-j` per invocation. With Docker
  down (~7 GB reclaimed) a repo-wide `[build] jobs` cap is *not* recommended;
  revisit at ~`jobs = 6` if the oracles return and memory pressure resumes).
- `[profile.dev] debug = "line-tables-only"`: likely the biggest unmeasured
  win for the *other* half of the pain (54 GiB target, multi-GB debug test
  binaries, link time). It is also a full-stampede change, so if adopted it
  should land in the same quiet window as the wrapper flip, one stampede for
  both. Not landed here because it is unmeasured.

## Configuration

- `.cargo/config.toml` — commented `[build] rustc-wrapper` +
  `[env] SCCACHE_CACHE_SIZE = "20G"` block (the flip switch).
- `SCCACHE_DIR` — unset; defaults to `~/Library/Caches/Mozilla.sccache`.
- `sccache --show-stats` / `--zero-stats` — the only trustworthy hit-rate
  instrument; never infer cache behavior from wall time.

## Dependencies

- `/opt/homebrew/bin/sccache` 0.17.0 (Homebrew; `brew upgrade sccache` is the
  owner's call — a version change re-keys nothing but changes behavior).
- Interacts with: `docs/ci.md` (CI must be coordinated before the flip),
  `CLAUDE.md` *Build and test* (health checks are the priming workload),
  cargo 1.95's content-hash freshness (a bare `touch` no longer dirties a
  crate — relevant when constructing edit-loop experiments).
