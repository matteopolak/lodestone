# The task runner (`just`)

## What it is

A [`Justfile`](../Justfile) at the repo root, run with `casey/just` (installed
at `/opt/homebrew/bin/just`, 1.58.0+). It gives every health check, `xtask`
invocation, and `LODESTONE_REGEN` regeneration a short, memorable name —
`just check`, `just xtask docs-index`, `just regen-hardness` — so an agent
does not have to reconstruct the exact flag set from CLAUDE.md by hand every
time, and so a fidelity check has one file to diff against instead of a
half-remembered command.

It is a thin *naming* layer, not a build system of its own. `just` is not
installed in CI (items 3/4 of the design that produced this file are
deliberately out of scope — see "How to change it" below) — CI still runs the
raw commands directly from `.github/workflows/ci.yml`.

## How it works

**`just` is the canonical-*command* layer, not a script host.** Three roles,
and the boundary between them is the whole point of this file existing:

| role | owns | example |
|---|---|---|
| `xtask` | anything that parses Rust/workspace structure, generates a committed artifact, or needs its own test | `docs-index`, `check-isolation`, `check-deletable`, `gen-packet-ids` |
| `just` | the canonical *invocation* — one to three lines, no logic | `just check` → `cargo check --workspace --all-targets …` |
| `scripts/*` | the script *body*, at its current path | `scripts/wasm-check.sh`, `scripts/worldgen-region-sweep.sh` |

**No script body moved into the Justfile.** `wasm-check`, `wasm-size`, and
`worldgen-sweep` are one-line delegations (`./scripts/wasm-check.sh`, etc.).
This was a deliberate constraint, not laziness: roughly 30 docs already
reference `scripts/…` paths by name (`docs/bevy-migration.md`,
`docs/chunk-world-resource.md`, `docs/session-components.md`, …), and every
one of those links stays correct precisely because the script never moved. A
Justfile that inlined the bodies would have required editing all ~30; the
delegating form needed 8.

### The four health checks

`check`, `check-all`, `check-seam`, `test` are exactly CLAUDE.md's "Build and
test" section and exactly the first four jobs in `.github/workflows/ci.yml`
(the fifth, `xtask-structural-checks`, has no dedicated recipe — reach it via
`just xtask check-isolation` / `just xtask check-deletable <family>`).
`just health` runs all four in order. **Byte-for-byte fidelity between the
recipe and CLAUDE.md's raw command is a property you can check yourself**:
`just -n <recipe>` prints the expanded command without running it (note: to
stdout on some `just` builds, stderr on others — capture both, `2>&1`, when
diffing programmatically).

### The target-dir design

Every cargo-invoking recipe passes `--target-dir {{tdir}}`, where:

```just
tdir := env("LODESTONE_TARGET_DIR", "target")
```

`just` interpolates `{{tdir}}` into the command line **before** cargo ever
runs, so cargo always sees the `--target-dir` *flag* — never an environment
variable. This is load-bearing, not stylistic: `docs/build-caching.md`
measured the flag form at 78–94% sccache cache hits and the `CARGO_TARGET_DIR`
env-var form at ~0%, because sccache hashes `CARGO_*` env vars into its cache
keys. `LODESTONE_TARGET_DIR` is deliberately *not* `CARGO_`-prefixed for
exactly that reason — sccache never sees it, only the flag it produces.

Default is plain `target`, so an agent or CI job that sets nothing gets
today's behaviour unchanged. Set `LODESTONE_TARGET_DIR=/tmp/lt-<issue>-<nonce>`
per CLAUDE.md's per-agent private-target-dir convention to get an isolated
build.

`-j` works the same way, via `jobs := env("LODESTONE_JOBS", "")`. It defaults
to **empty**, not `4` — a hardcoded `-j 4` baked into the Justfile would
silently throttle CI runners and any run on an otherwise-idle machine, which
is exactly the kind of thing this file's own gotchas section (below) warns an
editor not to reintroduce. Set `LODESTONE_JOBS=4` yourself for the local
multi-agent courtesy CLAUDE.md asks for.

### The `xtask` alias problem

`.cargo/config.toml` defines `xtask = "run --quiet --package xtask --"` so
`cargo xtask <command>` works as shorthand. That alias has no way to carry
`--target-dir` — cargo aliases splice fixed tokens in front of the args you
type, they cannot inject a flag mid-invocation — so `docs/build-caching.md`'s
agent playbook has always told agents to hand-expand it: `cargo run -q -p
xtask --target-dir <dir> -- <command>`. `just xtask *args` bakes exactly that
expansion, so `just xtask docs-index --check` (optionally with
`LODESTONE_TARGET_DIR` set) replaces the hand-expansion instead of leaving it
as a step every agent re-derives.

### Regeneration recipes

`regen-docs-index`, `regen-collision`, `regen-hardness` all follow the same
"generate offline, drift-check online" pattern documented at length in
CLAUDE.md's Documentation section: a committed artifact is derived from
something authoritative (the doc tree, a physics oracle dump, a JVM hardness
dump), a test asserts the committed file matches a fresh regeneration, and
`LODESTONE_REGEN=1` on the same test writes the fresh output back instead of
asserting.

| recipe | test it drives | ignored? |
|---|---|---|
| `regen-docs-index` | `cargo xtask docs-index` directly (no test needed — `cargo test -p xtask` has its own `docs_index_matches_committed`, not `#[ignore]`d, which is why plain `cargo test -p xtask` catches drift with no `LODESTONE_REGEN` flag at all) | n/a |
| `regen-collision` | `crates/lodestone-data/tests/collision_shapes.rs::committed_table_matches_dump` | yes |
| `regen-hardness` | `crates/lodestone-data/tests/hardness.rs::committed_table_matches_dump` | yes |

The collision-shape and hardness tests are `#[ignore]`d because they need an
external artifact (a physics-oracle dump / a JVM dump) that is not always
present; running them via `just regen-collision`/`just regen-hardness` is
exactly `LODESTONE_REGEN=1 cargo test … -- --ignored --nocapture` and nothing
more.

**A stale path worth flagging while you're here**: CLAUDE.md's own "Data
sources" section cites these two tests as living at
`crates/protocol/v770/tests/{collision_shapes,hardness}.rs`. They do not —
they live at `crates/lodestone-data/tests/{collision_shapes,hardness}.rs`
(confirmed by reading the files directly; `crates/protocol/v770/tests/` has
no `collision_shapes.rs` or `hardness.rs` at all, only
`block_hardness_seam.rs`, which is a different test). This Justfile's own
`regen-collision`/`regen-hardness` recipes point at the real location. Fixing
CLAUDE.md's citation is out of scope for this change (it is not one of the
files this change touches) but the next editor of that section should
correct it.

## How to change it, and the gotchas

- **Adding a new canonical command**: add a recipe, one to three lines, no
  script logic. If what you're adding needs more than that, it belongs in
  `xtask` (if it parses Rust/workspace structure or needs a test) or a new
  script under `scripts/` (if it's a shell pipeline) — write the recipe as a
  delegation to it, the same way `wasm-check` delegates to
  `scripts/wasm-check.sh`.
- **Never reintroduce a `CARGO_*`-prefixed variable anywhere in this file.**
  That is the exact env-var form measured at ~0% sccache hits. If a future
  edit needs a new cargo-affecting variable, name it `LODESTONE_*` and pass it
  as a flag inside a recipe body, the same way `tdir`/`jobs` are handled.
- **Never add `set export`.** It would push every `just` variable
  (`tdir`, `jobs`, and anything added later) into the environment for every
  child process, including cargo — which reintroduces the env-var path this
  design exists to avoid, silently, the next time someone adds a variable
  without checking. `{{interpolation}}` inside a recipe body is the only
  sanctioned way a variable reaches a command line here.
- **Never hardcode a shared target dir or a fixed `-j`.** Both regressions are
  invisible in a diff that only looks at "does this recipe work" — they
  silently break the *multi-agent* property (private dirs, no throttling of
  idle/CI runs), which nothing in a single test run will catch.
- **Verify fidelity, don't assume it**: `just -n <recipe>` prints the expanded
  command with no side effects. Diff it against the raw command in CLAUDE.md
  or `.github/workflows/ci.yml` after any edit to a health-check recipe.
- **This file only names commands that already exist elsewhere.** If you're
  tempted to add logic — a loop, a conditional pipeline, argument parsing
  beyond a bare `*args` passthrough — that's a sign the thing you're adding
  belongs in a script or in `xtask`, not here.

### What deliberately has no recipe here

- **`scripts/profile-cost-table.py`** — 372 lines with its own `argparse`; a
  tool, not a task. Its entry point is documented in
  `docs/roadmap/benchmarks.md`, not here.
- **`crates/lodestone-allocbench/bench.sh`** — stays crate-local, no root
  recipe. It `cd`s into its own directory and writes `bin/` there; hoisting it
  to the root would put an `--all-features`-adjacent foot-gun (the crate has a
  deliberate `compile_error!` when more than one allocator feature is on) one
  tab-completion away from every agent working anywhere else in the repo.
- **The oracle scripts** (`scripts/live-oracles/*.sh`, `scripts/live-oracles/
  rcon-op.py`, `scripts/worldgen-oracle/run.sh`) — untouched. This is a
  Docker→Apple-`container` migration in flight; adding a recipe that wraps a
  script mid-rewrite would either point at the wrong runtime or need
  rewriting again the moment that migration lands. Revisit once it soaks.
- **CI** (`.github/workflows/ci.yml`) — not converted to call `just` yet, on
  purpose: this file needs to soak locally first. `docs/ci.md`'s
  "How to reproduce" section names both forms side by side in the meantime.

## Configuration

| variable | default | effect |
|---|---|---|
| `LODESTONE_TARGET_DIR` | `target` | value of every cargo recipe's `--target-dir` flag |
| `LODESTONE_JOBS` | *(empty)* | value of every cargo recipe's `-j` flag; empty means cargo's own default (no `-j` passed at all) |

Neither is read by cargo directly — `just` substitutes them into the command
line as literal flag values, so cargo itself only ever sees
`--target-dir <path>` / `-j <n>`, never an environment variable.

## Dependencies

- `casey/just` 1.58.0+ (the `env()` function and `if`/`else` variable
  expressions used for `jflag` need a reasonably current `just`; both are
  present in 1.58.0, which is what this was built and verified against).
- `cargo`, `xtask`, and every script under `scripts/` this file delegates to
  — this file adds no new runtime dependency beyond what those already had.
- `docs/build-caching.md` for the full sccache/target-dir measurement this
  design is built on; read that before changing the target-dir or `-j`
  handling here.
