# Lodestone handoff — 2026-09-13

## Current state

`main` is clean and synchronized with `origin/main` at
`04f3c7ca6a9f2f30275938fe329aed93a77e6579` before this handoff commit. The last integrated pass
restored shell/server compilation, landed the dimension-rendering and 256-chunk-distance work,
repaired split test modules, removed all warnings observed by the final all-target checks, and landed
the current world-generation lifecycle model.

Do not assume that a clean tree means world generation is finished. Nether's bounded streaming
comparison passes, but the Overworld still has a real mismatch at the third streamed target. The full
million-chunk goal and the open GitHub backlog remain incomplete.

## Verified immediately before handoff

- `cargo check -p lodestone-shell --all-targets` passes without warnings.
- `cargo check -p lodestone-server --all-targets` passes without warnings.
- `cargo check -p lodestone-worldgen-parity --all-targets` passes.
- `cargo xtask docs-index --check` passes.
- The v26 streaming unit target passes 17 tests with one external-oracle test ignored.
- The external Nether stream comparison for chunks `(380,380)` through `(381,380)` passes 2/2.
- The external Overworld stream comparison passes `(0,0)` and `(1,0)`, then fails at `(2,0)` with
  terrain and heightmap differences.
- `HEAD` contains 10,029 tracked paths and the root `Cargo.toml`. This guard matters because an empty
  tree was accidentally published earlier in the session; it was repaired additively and history was
  not rewritten.

The complete `just health` and `just wasm-check` suites were not rerun after the final integration.
Run them before claiming repository-wide health. Some test suites take many minutes; keep Cargo in the
foreground and wait for a real completion line.

## First task: finish Overworld streaming parity

The relevant commit is `6a90ee8ac` (`fix(worldgen): replay target-scoped feature lifecycles`). Start
with these files:

- `crates/lodestone-worldgen-parity/src/lifecycle.rs`
- `crates/lodestone-worldgen-parity/tests/overworld_tuff_lifecycle.rs`
- `crates/versions/26.2/tests/streaming_worldgen_parity.rs`
- `docs/worldgen-decoration.md`
- `docs/worldgen-stages.md`
- `scripts/worldgen-oracle/LargeParityOracle.java`
- `scripts/worldgen-oracle/stream-parity.sh`

The current lifecycle distinction is intentional:

- A target's own FEATURES body is one authenticated completion against its radius-one resident
  context.
- A later source completion may mutate a previously requested target. Those spills are retained and
  source completions are globally deduplicated.
- Nether uses the externally observed x-major, z-fast completion wavefront. Do not restore the old
  model that treated all nine neighbours as one target FEATURES body.

The Nether discriminator is target `(381,380)`: its direct FEATURES body has exactly 24 crimson-root
positions, while the east source `(382,380)` later places the boundary root at local `(15,77,5)` before
the final packet snapshot. Focused Nether lifecycle and cross-target tests encode this separation.

For the Overworld, keep iteration small. Run the live comparator over `(0,0)` through `(2,0)`, stop on
the first mismatch, and inspect the target-2 terrain/heightmap difference. Do not start another
multi-hour baseline dump or write gigabytes of packets. Compare the external server and Lodestone in
one streaming run, ignore lighting until block/biome/heightmap parity is stable, and remove temporary
tracing before committing.

Use:

```bash
LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS=1 \
  bash scripts/worldgen-oracle/stream-parity.sh \
  --dimension overworld --cx 0 2 --cz 0 0

LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS=1 \
  bash scripts/worldgen-oracle/stream-parity.sh \
  --dimension nether --cx 380 381 --cz 380 380

cargo test -p lodestone-v26-2 --test streaming_worldgen_parity --no-fail-fast
cargo test -p lodestone-worldgen-parity --all-targets --no-fail-fast
```

After `(0,0)..(2,0)` passes, expand by a few thousand chunks per run and stop at the first mismatch.
The ultimate tracked goal is identical packet-relevant output over the full requested
domain, later including a separate lighting pass. The requested domains also include Nether and End;
outer End islands/cities need samples away from the central island.

## Other remaining work

GitHub had 37 open issues when queried for this handoff. Treat that list as live state and audit it
again with `gh issue list`; the tracker can lag the tree. Notable current items are:

- Full worldgen parity run. This is the immediate continuation described above.
- Structure, Nether-decoration, and Overworld configured-feature gaps. Verify each
  against the current tree before implementing or closing it.
- Browser worker separation and a 20 Hz integrated simulation. World generation must
  not starve server ticks or the browser main thread.
- Camera lag and dynamic dimension rendering appear substantially implemented by
  `f9a8ec1b0`, `ca5e9f6c4`, `fc11a229f`, and `2abe48207`; audit production consumers and close/update the
  issues with evidence if complete.
- Block-placement parity.
- First-person offhand/use-state animation.
- Differential fuzzing remains a larger ongoing effort.
- Hosted-protocol, plugin/Paper compatibility, region-executor, and typed-ID issues remain open. Work
  them by file cluster, and close stale issues only after checking production wiring and tests.

The user's standing priorities are worldgen parity, worldgen throughput/memory, and continuously
closing genuinely completed GitHub issues. After correctness, profile generation with `samply`; reduce
allocations, string work, repeated scans, and tick-loop stalls. Do not accept a separate WASM tick loop:
native and browser builds must consume the same simulation implementation.

## Architecture and coding direction

- Keep the dimension-specific worldgen order explicit through the typed stage schedule documented in
  `docs/worldgen-stages.md`. A reader should be able to compare stage order without reconstructing it
  from callbacks.
- Far chunks should resume through the same stage cursor and may stop at a compact preliminary level;
  do not invent a disconnected shaped-to-full pipeline. The render-distance slider now reaches 256,
  but the far-chunk compact representation and incremental stage resumption are not complete.
- Avoid strings in hot paths. Prefer generated enums and typed properties. Truly arbitrary plugin data
  may use interned identifiers or `serde_json::Value`, but known JSON formats should deserialize into
  typed Serde structures.
- Do not globally replace every hash table with `FxHashMap`; use it only where measurements show a
  significant benefit.
- Keep world generation off the simulation-critical path with a persistent worker pool/work-stealing
  design. Do not repeatedly create scoped operating-system threads.

## Repository workflow and hazards

Read `AGENTS.md`, `CLAUDE.md`, and `docs/meta/handoff.md` before editing. This repository uses one shared
checkout and one shared Cargo target at `~/.cargo/shared-target`.

- Never use `git add -A`, broad directory staging, `git stash`, `git clean`, `git reset --hard`,
  `git checkout --`, `cargo fmt`, force-push, or amend.
- Commit exact files through `scripts/private-index-commit.sh` using a private `GIT_INDEX_FILE` built
  from the recorded `HEAD`.
- Before publishing, verify the candidate tree has more than 10,000 paths and contains `Cargo.toml`.
- Keep the shared index empty and push additive commits to `main`; the user explicitly authorized
  pushes to `main`.
- `docs/README.md` is generated. Run `cargo xtask docs-index`; never hand-edit it.
- Run Cargo in the foreground with the configured shared target. Do not create temporary target dirs.
- Do not launch the game unless the user asks. Live play evidence still requires the user at the
  keyboard.

Before handing back, the minimum repository check is:

```bash
just check
just check-all
just check-seam
just test
just check-comment-voice
just wasm-check
git status --porcelain=v1
git rev-parse HEAD
git rev-parse origin/main
```

The final status output must be empty, and local `HEAD` must equal `origin/main`.
