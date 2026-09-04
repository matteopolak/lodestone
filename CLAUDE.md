# Lodestone — working rules

A from-scratch Minecraft Java Edition client in Rust (2024 edition) + wgpu, plus an integrated server.
The **library is the product**: the playable game is a thin shell over `lodestone-client`, and anything
the game can do a bot can do headlessly.

Architecture and rationale live in [`docs/architecture.md`](./docs/architecture.md); per-subsystem
detail is in [`docs/`](./docs/README.md). Open work is in
[GitHub issues](https://github.com/matteopolak/lodestone/issues), with tiers in
[`docs/backlog.md`](./docs/backlog.md).

---

## Protocol families

Ten client protocol families exist, each a workspace member under `crates/versions/` behind a
`lodestone-registry` feature: `v1-7` (1.7.6-1.7.10), `v1-8` (1.8.8-1.8.9), `v1-9` (1.9.4-1.12.2),
`v1-13` (1.13.2), `v1-14` (1.14.4-1.16.5), `v1-17` (1.17.1-1.18.2), `v1-19` (1.19.4), `v1-20-6`
(1.20.5-1.20.6), `v1-21-11` (1.21.11), `v26-2` (protocol 776 / MC 26.2). Together they cover every
Minecraft release from 1.7.10 up, in the joining direction only.
Each folder is named for the *era-start* Minecraft version it covers (e.g. `crates/versions/1.8`), which
is neither its package/feature suffix (`lodestone-v1-8`, feature `v1-8`) nor a protocol number — ask
`VersionAdapter::supports`, never the folder or the feature name.

- **No family is enabled by default** in `lodestone-registry`. The shell's default `live` feature turns
  on `v26-2` and nothing else, so a legacy family is invisible to every command below unless you name
  its feature.
- **Only `v26-2` implements `ServerProtocol`**, so 26.2 is the only version we can *host*. Joining and
  hosting are different sets; `lodestone-registry` keeps `Family` and `ServerFamily` as two tables.
- **A family may speak several protocols**: `v1-9` serves 110/210/316/340 and `v1-14` serves
  498/578/754. No folder, package or feature name is a protocol number.

New gameplay work targets `v26-2` unless an issue says otherwise.

---

## Build and test

`just` (see [`docs/repo-tooling.md`](./docs/repo-tooling.md)) is the canonical command layer — one recipe
per raw invocation, so the recipe is never the only record of what it runs.

```bash
just check        # cargo check --workspace --all-targets
just check-all    # ... --all-features --all-targets --exclude lodestone-allocbench
just check-seam   # cargo check -p lodestone-shell --no-default-features
just test         # cargo test --workspace --no-fail-fast
just check-comment-voice  # cargo xtask check-comment-voice: no issue refs/change-voice in comments
just health       # all five of the above, in order
just wasm-check   # wasm32 compile + confinement guards (CI runs this; `health` does not)
just run          # launch the game
just run-wasm     # launch the BROWSER build on :8080
```

All five health checks are required, because each catches a class the others cannot:

- **`cargo build` is not a health check** — it skips test targets. Always `--all-targets`.
- **`--all-targets` alone misses non-default features**, hence `check-all`. The `--exclude` is not a
  workaround: `lodestone-allocbench` has a deliberate `compile_error!` when more than one allocator
  feature is on, so plain `--all-features` structurally cannot pass for it.
- **No `cargo check` sees a doctest.** After any crate rename or module move, run `cargo test`.
- **`--no-fail-fast` is not optional.** Plain `cargo test` aborts at the first failing test *binary*,
  hiding every alphabetically-later one.
- **`check-seam` is architectural**: nothing else proves the shell compiles with no version family,
  which is the whole point of the version seam.
- **`check-comment-voice` is a lint, not a compiler check**: nothing else catches a comment written in
  the voice of the change that introduced it, or a bare issue reference standing in for the substance
  it pointed at (`xtask/src/comment_voice.rs`). Exceptions are recorded in
  `xtask/check-comment-voice.toml`, never silently skipped.
- **`wasm-check` lives in CI, not in `health`** — the other five can be green while `wasm32` is broken.
  A green wasm compile still does not prove the browser runs: `std::fs` returns `Err(Unsupported)`, but
  `Instant::now`, `SystemTime::now`, `thread::spawn` and `thread::scope` all trap. Run it after any
  `cfg` change, dependency edit or module move.

**Run every build and test in the FOREGROUND.** There is no way to wait for a background run: an agent
that stops to wait is marked complete by the harness and its wake-up is discarded. `cargo test -p
lodestone-shell --lib` is ~7 minutes and the full `lodestone-server` suite 14–29; that is not a hang.
Narrowing a run is fine if you say so; reporting without it is fine if you name what you skipped. Say
when a run did not finish — a truncated run reads as a real total, and a filter matching nothing prints
`0 passed; N filtered out` and exits 0.

Smaller facts:

- **The binary is `lodestone`, not `lodestone-shell`** — the `[[bin]]` name differs from the crate.
- `default-members` makes a bare `cargo run`/`build`/`test` target `lodestone-shell` only; every command
  above says `--workspace` for that reason.
- Live and GPU gates are `#[ignore]`d: `-- --ignored --nocapture`.
- Live oracles are not repo state — recreate them with `just oracle-creative` / `oracle-terrain` /
  `oracle-survival`. They run under Apple `container`, not Docker
  ([`docs/oracles-and-benchmarks.md`](./docs/oracles-and-benchmarks.md)); the host needs no `java`.
- Test *counts* and *timings* gathered while other agents build are samples, not measurements. The
  invariant is zero failures, never an absolute number.
- **PGO is opt-in and off by default** (`just pgo-instrument` / `pgo-merge` / `run-pgo`). See
  [`docs/oracles-and-benchmarks.md`](./docs/oracles-and-benchmarks.md).

---

## Repo hazards

**Single shared checkout, no per-agent worktrees. Multiple agents edit concurrently.** Everything here
follows from that.

| never run | because |
|---|---|
| `git add -A`, `git add <dir>` | sweeps up other agents' files. Stage explicit *file* paths |
| `git reset --hard`, `git checkout .`, `git checkout -- <path>` | discards the working tree — no diff, no reflog |
| `git stash`, `git pull --rebase`, `--autostash` | stashes the whole shared tree, silently |
| `git clean` | deletes untracked files — new crates, docs, oracle dumps, in no commit and no reflog |
| `git commit --amend`, `git push --force` | rewrites a commit others built on. Land a follow-up instead |
| `cargo fmt`, `rustfmt` | rewrites files you do not own. Format the lines you wrote, by hand |

**Commit with the pathspec form: `git commit -m "…" -- <your file paths>`.** It ignores the index
entirely, which is the property that makes it safe. Put `-m` before the `--`, or git parses the message
as a pathspec and commits nothing. It cannot introduce an untracked file, so `git add <files>` anything
new first. Read your sha back in the same shell invocation and `git show --stat` it — a no-op commit
does not look like a failure. Name only paths in your own cluster, and check `git diff --cached` is
empty (count it, do not eyeball it) before committing: the index is shared, so never leave work staged.

**Never rewrite a shared file wholesale — edit the lines you mean, and re-read the file immediately
before writing to it.** No git command is involved, so no ban above catches it: a full new copy silently
discards every concurrent edit. `docs/README.md` is the usual victim.

**A red test may be someone else's in-flight edit.** Before reporting a red `main`, re-run at the
committed sha in an isolated worktree (`git worktree add --detach`; prefer `git worktree remove` after).
A worktree is right for verification and wrong for long work — its base goes stale fast enough to make
the result unmergeable, and a green worktree proves nothing about `main`.

**Machine hygiene.** `target/` reaches 100+ GB against a volume with ~30 GB usable, and every `Bash`
call then fails before running. Measure the split before choosing what to delete (the `build` vs
`incremental` ratio is not stable); `rm -rf target/debug` is the reclaim that works, and is safe when no
cargo/rustc is running. Do not purge `target/` while another agent is mid-compile — its signature is a
flood of `E0463 can't find crate` affecting every crate uniformly. Do not kill Bitwarden (it hosts the
ssh-agent that authenticates GitHub). Idle cargo processes with zero `rustc` are **not** a wedged lock —
sample the children (`ps -Ao pid,etime,%cpu,command | grep "[t]arget/debug"`), not the parents.

---

## Coding practices

- **Nothing is done until something on screen changes.** The dominant defect here is the *island*: a
  subsystem individually built, individually tested, and reaching zero pixels because nothing calls it.
  A crate's own test suite is a closed loop — it can be entirely green while the crate is dead code. Ask
  of every piece of work: **what actually consumes this?** and treat "nothing" as a defect report. Two
  mechanical forms of the question: for a clientbound packet, `cargo xtask connectedness`; for a field a
  draw site reads, count the *production* call sites that assign it something other than the default,
  and treat zero as the defect. `cargo xtask islands` and `world-coverage` cover the crate-internal and
  registry-subject cases.
- **The reported layer is almost never the broken one.** Trace the whole chain — action → server → wire
  → ECS → pixels — and say which link you verified. "The code exists" is not evidence a feature works.
- **Delete dead code.** Wire it or remove it; there is no third state. An `#[allow(dead_code)]` nobody
  removed is the cheapest tell that a wiring never landed.
- **Re-verify before routing around "X doesn't exist yet."** A predecessor's blocker is written at the
  moment of greatest ignorance about the neighbouring subsystem, and a search that failed is evidence
  about the search, not about the tree. Search for the *capability*, not the name you expected it to
  have. The same applies to a comment asserting an absence, and to a doc's status annotation.
- **An expected value must originate outside the code under test.** `decode(encode(x)) == x` is
  satisfied by two symmetric misunderstandings. Use captured server bytes, a JVM oracle, a committed
  dump, or arithmetic that is independent of both arms. Derive each ported constant from its own outside
  source, never from a sibling you also ported.
- **Assertions of an absence need a control proving the detector works.** Run the control and observe it
  fail; do not describe what it would do. Treat an audit that prints nothing as a failure to run, and a
  skip as a failure unless the subject is genuinely absent.
- **Predict the value, do not merely assert the direction of a change** — and pick inputs where the
  right and wrong hypotheses differ. A round number chosen as an input is the one most likely to make
  every arm of a gate coincide.
- **Measure by location, not by frame average**, for anything pixel-shaped, and make failure output
  print a bounding box.
- **Validate the instrument before optimising the system.** When a reported number looks wrong, the
  number is a hypothesis too; the cheapest discriminator is an input that cannot physically affect the
  quantity.
- **Shaders live in `.wgsl` files** under a crate's `src/shaders/`, via `include_str!`. Never inline one
  in Rust — `no_wgsl_is_inlined_in_rust_sources` fails on any `@vertex`/`@fragment` under `src/`. Note
  `cargo check` never compiles a shader; `cargo test --workspace` runs them all through naga.
- **Pointer identity is only meaningful on a `static`.** A `const` is inlined per use site and has no
  stable address. `cargo xtask check-ptr-const` enforces this.
- **Whenever the type system cannot express a constraint, make it checkable and check it.** A comment
  stating an invariant is documentation of intent, not a guard.

### Rendering and data-source constraints

Renderer invariants that are expensive to rediscover — bind-group budget, reversed-Z depth, gamma-space
tint and shade, the GUI winding sign, resource-pack reload re-attachment — are in
[`docs/architecture.md`](./docs/architecture.md) and the per-subsystem render docs.

Data sources, in order of authority:

1. **Mojang's own generator** (`packets.json`, `registries.json`, `blocks.json`). Authoritative about
   registry *contents*; not about which registries are *sent* to the client. Dynamic (datapack)
   registries are ordered **alphabetically by resource location**, not by their bootstrap class.
2. **Decompiled source** under `.cache/mc/26.2/{src,client-src}` — behavioural reference only, never
   transliterated. 26.2 ships de-obfuscated. Port from a packet's `write`/`read`, never from its
   constructor or field declaration; those are three different orders that all look authoritative.
3. **minecraft-data** — bootstrap and cross-check for **1.8–1.21.11 only**; no 26.x data.

Prefer interrogating the real jar over any community dataset: per-block-state tables come from booting
the real server headlessly and walking `Block.BLOCK_STATE_REGISTRY` (see
`crates/lodestone-data/tests/`, and the `just regen-*` recipes). `oracle-java/` is **per-crate**, not at
the repo root. A vanilla-authored oracle world sits at `.cache/mc/survival/world`. Entity metadata
indices come from `EntityDataIndexOracle.java` — never hand-count one.

---

## Documentation practices

Keep [`docs/`](./docs/README.md) current: **one doc per subsystem**, `kebab-case`, named after the
feature rather than the file. Each covers what it is, how it works, how to change it and the gotchas,
configuration, and dependencies — at the level of "a developer new to this codebase but experienced in
general", not a line-by-line account of every function.

Docs are for durable facts: architecture, measured constants, data-model shapes, oracle provenance, and
gotchas that are still live. They are not a changelog and not an incident log. Prefer deleting a stale
section to annotating it.

- **`docs/README.md` is generated — do not hand-edit it.** `cargo xtask docs-index` (or `just
  regen-docs-index`) builds it from every doc's own H1 plus its `## What it is` summary paragraph, and
  `cargo test -p xtask` fails if the committed file drifts. To change how your doc appears, edit its H1
  and summary. A doc with no usable summary makes the generator fail loudly, naming the file.
- **Cite symbols, never line numbers.** `lodestone_server::commands::ServerCommands::run`, not
  `server.rs:5189`. A symbol survives every edit above it; a plausible wrong line number is worse than
  none. Measurements keep their numbers — those are evidence, not citations.
- **Do not cross-reference issue numbers from code comments or docs.** Say what the code does and why,
  in terms a reader of that file can check. Commit messages and issue comments may name an issue.
- **Never name vanilla code anywhere — not in `.rs`, not in `docs/`** (owner's decision; this is a
  clean-room implementation). No class, method, field or `.java` file names, no `net.minecraft` paths.
  This supersedes the earlier rule that allowed such citations under `docs/`.
  **Describe the rule instead, in terms a reader can check**: "two knockback impulses, the flat one
  unconditional and the sprint bonus gated" rather than a method name. Behaviour, constants and
  measurements are all still welcome — it is the *identifiers* that go. Our own types keep their
  names even where they coincide (`BlockPos`, `ItemStack`, `ResourceLocation` are ours).
  **Scoped to Mojang's code only** (owner's ruling): third-party APIs we interoperate with keep their
  names — Bukkit/Paper especially, since a compatibility surface cannot be documented without naming
  it — as do the JDK, our own `oracle-java/` harnesses, and any path a tool or test actually reads.

**AI-ingested artifacts are size-capped.** `CLAUDE.md` is auto-loaded into every agent's context, so it
is budgeted like code: a pre-commit hook (`.githooks/pre-commit`, installed by `just install-hooks`)
fails a commit that pushes it past its cap. Keep additions to a sentence or two, and put the long form
in `docs/`. [`docs/meta/handoff.md`](./docs/meta/handoff.md) is the orchestrator's handbook — read it on
demand if you are dispatching subagents rather than writing code; it is deliberately *not* auto-loaded.
