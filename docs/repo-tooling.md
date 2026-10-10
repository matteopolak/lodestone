# Repo tooling: the task runner, CI, build caching, and the xtask scanners

## What it is

The tools that keep this workspace buildable and testable at scale: the `just` task runner giving every health check a canonical name, the GitHub Actions workflow that verifies pushes without loading the shared dev machine, the machine-level shared-target and `sccache` policy, and the `cargo xtask` static scanners (`islands`, `world-coverage`, `connectedness`, `check-ptr-const`, `wasm-check`) that catch defect classes no compiler check can see.

## How it works

### The task runner (`just`)

The root `Justfile` is a naming layer, not a build system: `xtask` owns anything that parses Rust or workspace structure, `just` owns the one-to-three-line canonical invocation, and `scripts/*` keeps script bodies at their existing paths (docs reference them by name). `just check`, `check-all`, `check-seam`, `test` and `check-comment-voice` are the five required health checks, and `just health` runs them in order. `just -n <recipe>` prints the expanded command with no side effects, which is how to verify a recipe stays faithful to the raw command it names.

- Cargo policy resolves normally. Locally `~/.cargo/config.toml` selects `/Volumes/CodexBuilds/targets/lodestone`, `rustc-wrapper = "sccache"` and eight cross-crate jobs; `~/.cargo/shared-target` is a compatibility symlink, not a second cache. Isolated builds may use an SSD target but must coordinate CPU and RAM. The `Justfile` asks `cargo metadata` for the target path only where a profiler needs a built binary. CI has no user config.
- `just run` and `just run-wasm` (trunk against `web/`'s separate Cargo workspace) are separate recipes because they share no invocation.
- Regeneration recipes (`regen-docs-index`, `regen-collision`, `regen-hardness`, ...) share one shape: a committed artifact derived from an authoritative source, a test asserting the committed file matches fresh output, and `LODESTONE_REGEN=1` on that test writing instead of asserting.

### CI (`.github/workflows/ci.yml`)

Runs on every push to `main` and every pull request, so heavy builds run on hosted runners. It is not a replacement for the `#[ignore]`d live and GPU gates (no GPU, vanilla jar or oracle server on a runner); CI proves the hermetic majority.

- Jobs run in parallel: a three-OS matrix for `cargo check --workspace --all-targets`; Linux jobs for all-features, the version seam, structural checks, the full test suite, the wasm32 tripwire, fuzz smoke and benchmark controls. macOS minutes cost roughly 10x and Windows 2x, so do not add a platform to a job that does not vary by platform.
- The wasm job keeps its full browser build log as a short-lived artifact on failure and installs the `wasm-bindgen` CLI matching the web lockfile (the worker hook invokes it directly; trunk's private copy is not on `PATH`).
- `cargo check` never links, so only the Linux `test` job sees unresolved symbols. Test/bench sites naming macOS-only `proc_pid_rusage` are gated per item with a non-Darwin panic arm, not a whole-file `cfg`.
- A test passing locally and failing on a runner can differ beyond OS: the Cranelift debug backend lacks one SSE intrinsic a font dependency's `simd` feature reaches on x86; a negative `sqrt` NaN sign differs between aarch64 and x86_64; and `cfg!` read inside the function under test resolves per machine.
- Linux installs `libasound2-dev`/`pkg-config` for `cpal` (the step is `if: runner.os == 'Linux'`). CI opts into sccache via `mozilla-actions/sccache-action` and `RUSTC_WRAPPER`/`SCCACHE_GHA_ENABLED`; local config selects no wrapper there.
- Gotcha: every job passes a `toolchain:` input that is not the compiler used. `rust-toolchain.toml`'s `channel` pin wins silently, and the input cannot be deleted (the action requires it). Read the `rustc --version` a job reports, never the YAML.

### Native release artifacts

The default `release` profile keeps full DWARF (`debug = 2`) for Samply/Instruments. Shipping builds use `cargo build --profile release-dist -p lodestone-shell --bin lodestone`, which inherits the optimizer settings (`opt-level = 3`, ThinLTO, one codegen unit) and adds `debug = 0` and `strip = "symbols"`. Keep an unstripped `release` build for profiling or symbolized crash diagnostics.

### Stale-safe private-index commits

Concurrent agents build commits in private indexes so they never stage each other's files. Publish with `scripts/private-index-commit.sh <recorded-head> <message> <path>...`:

- It fails before writing if the branch advanced, if the index is incomplete (would delete unselected files) or unchanged (empty commit), if the private index differs outside the named paths, or if a named path is missing.
- It publishes with `git update-ref` and the recorded old object as compare-and-swap, then resets only the selected paths in the shared index (an advanced branch otherwise reports them as staged). Unselected staged paths are preserved.
- Reconciliation happens only in the checkout that ran the helper. A commit published from another worktree leaves this checkout's files and index old, so the helper refuses any named path whose index entry differs from the recorded `HEAD`; three-way merge `HEAD` into the working file and reset that index entry first.
- A short-lived repository lock serializes publication and reconciliation; a helper waiting too long fails rather than publishing unreconciled.
- The pre-commit hook rejects `git commit -- <paths>` and `--only`, which build a temporary index from the working tree and can carry another agent's dirty hunks.
- `scripts/test-private-index-commit.sh` runs all controls (pathspec rejection, stale and unchanged index failures, a rebuilt index retaining a concurrent file, interleaved commits leaving the shared index at the later one).

### Shared target, caching, dev profiles

Local builds share the default target to reuse dependencies; Cargo's exclusive lock queues commands, and eight jobs plus eight front-end threads run once admitted. Independent targets do not share the lock. `sccache` wraps every local rustc. CI is separate, with an Actions-backed cache and runner-local targets.

- The build root is `/Volumes/T7/codex-builds/build-cache.sparsebundle`, a grow-on-demand APFS image (500 GiB maximum, not reserved) mounted at `/Volumes/CodexBuilds`. Targets are `targets/lodestone`, `targets/jai`, `targets/<project>-<lane>`; scratch is `scratch/<project>-<lane>`. APFS avoids ExFAT's Unix limits and AppleDouble sidecars that broke trunk's staged-file renames; keep trunk distributions on APFS too.
- After reconnecting T7: `hdiutil attach -nobrowse /Volumes/T7/codex-builds/build-cache.sparsebundle`. Stop builds and detach `/Volumes/CodexBuilds` before ejecting; never delete the image while mounted. To change the default, edit the user's Cargo config and pruning script together and verify `cargo metadata --no-deps --format-version 1`.
- Dev profiles use `debug = "line-tables-only"` and `opt-level = 1` for dependencies; override a hot crate with `--config 'profile.dev.package.<crate>.opt-level=0'`. A heavy vendored-C `-sys` crate rebuilds in every target (a rustc wrapper cannot cache C work); prefer removing it.
- A daily `cargo-sweep` LaunchAgent skips while Cargo or rustc runs, removes artifacts older than 21 days, caps the default target at 120 GB and skips an unmounted target. Isolated lane targets are not covered. Test-runtime memory (several GB RSS in a single test binary) is untouched by any of this.
- `just reclaim` is the safe disk reclaim (free space has fallen to 4.1 GiB, below which every shell call fails): it removes `target/debug/incremental` (the largest sink, measured 8-22 GB) unless a `rustc` is live, then per-crate `target/debug/build/*/` directories untouched for over a day. Never `rm -rf target/debug`, which kills every concurrent compile. The split varies by day, so measure first.

### The `xtask` static scanners

These complement `connectedness` (which only asks whether a clientbound packet reaches anything; see `docs/multi-protocol-seam.md`, `docs/packet-wiring.md`).

- **`cargo xtask islands`** parses with `syn` (three earlier hand-rolled scanners were each wrong about lifetimes) and reports functions with zero production call sites, fields with zero production readers, fields only ever assigned default-like values, and stray `#[allow(dead_code)]`. Resolution is by bare name, so common names (`new`, `tick`) hide each other. Production versus test is tracked by realm (a `tests/`, `benches` or `examples/` target is test by path; an external `#[cfg(test)] mod tests;` marks the declaration). Derive traits (`Encode`, `Decode`, `Serialize`, ...) are excluded from dead-field reports. The default-only finding is a syntax heuristic that misses intermediate bindings and chained accessors.
- **`cargo xtask world-coverage`** asks, per entity, block-entity and particle type, whether anything resolves real geometry. Its calibration case had a pose matrix, hitbox, render branch and draw counter yet drew nothing because the model-rig corpus had no entry. Buckets: drawn; stranded (the finding class); absent; and no reference rig (checked against the decompiled 26.2 renderer registrations), which keeps the report actionable. Every claim needs an anchor (a file and symbol that must still exist), and a rule resolving to zero subjects hard-fails.

## How to change it

- Never reintroduce a `CARGO_*` variable, hardcoded shared target dir or fixed `-j` in the Justfile; each defeats per-agent isolation or throttles idle/CI machines invisibly.
- Do not add a CI job running the `#[ignore]`d live/GPU gates.
- Regenerate `docs/README.md` with `cargo xtask docs-index` (or `LODESTONE_REGEN=1 cargo test -p xtask docs_index_matches_committed`) whenever a doc's H1 or `## What it is` summary changes, in the same commit.
- A skip must never look like a clean scan: `islands` and `world-coverage` hard-fail when a scan target is missing or many files fail to parse.
- A new `islands` false-positive exclusion needs a test planting the exact shape, named after the false positive.
- Prefer mechanical `world-coverage` rules (arm literals, suffix rules, AST-read variant lists) over hand lists, which go stale silently. Over-claiming is invisible in the output (a stranded subject reads as drawn); under-claiming only produces visible noise.
- `cargo xtask check-comment-voice` (`xtask/src/comment_voice.rs`, the fifth `just health` check, run in CI's `xtask-structural-checks`) fails comments written in the voice of their change: bare issue numbers, or phrases that refer to the change itself (patterns in the source). Exceptions live in `xtask/check-comment-voice.toml` with `owner` and `reason`; stale entries are reported. It also fails reference-implementation class names in comments and docs, but only under `CLASS_NAME_SCOPES` (in `comment_voice.rs`; add a path once it has been swept). The names come from `xtask/vanilla-class-names.txt`, regenerated by `scripts/gen-vanilla-class-names.py` from the cached reference jar; a name is exempt when this workspace's own code uses it as an identifier, so our types keep theirs. Method and field names are not detected. Nested `.worktrees/` checkouts are excluded.
- `cargo xtask check-mc-version` (`just check-mc-version`, `xtask/src/mc_version_lint.rs`) fails on a hard-coded `.cache/mc/<digit...>` path outside `xtask/check-mc-version.toml`. See [`mc-version-bump.md`](./mc-version-bump.md).

## Configuration

- `~/.cargo/config.toml`: local shared target, compiler wrapper, eight-job queue. `.cargo/config.toml`: repo-only compiler/profile settings.
- `~/.local/bin/cargo-shared-target-prune` and its LaunchAgent: age and size retention.
- `LODESTONE_REGEN=1`: switches drift-check tests from assert to write.
- `cargo xtask islands [--crate <name>]` and `cargo xtask world-coverage` scan the whole workspace from the current directory unless scoped.

## Dependencies

`casey/just`; `cargo`, `xtask`, `scripts/*`. CI: `dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `mozilla-actions/sccache-action`, `extractions/setup-just`. `sccache`, `cargo-sweep`, `syn`/`proc-macro2` (`visit`), `lodestone-data`/`lodestone-assets` (version-free deps of `world-coverage`), and the pinned 26.2 decompile under `.cache/` as its optional cross-check.
