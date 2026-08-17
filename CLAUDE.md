# Lodestone — working rules

A from-scratch Minecraft client in Rust, plus an integrated server.

**This file is the rules. [`DESIGN.md`](./DESIGN.md) §12 is the evidence.** Every rule here was paid for by
an incident; §12's validation log holds the measurement, sha and count behind each one (§12.19 onward is the
operational record that used to live here). Read §12 when a rule looks like it cannot possibly matter. Open
work is in [GitHub issues](https://github.com/matteopolak/lodestone/issues), with tiers and per-item traps
in [`docs/backlog.md`](./docs/backlog.md); [`HANDOFF.md`](./HANDOFF.md) is for an agent *orchestrating* this
repo rather than writing code in it; subsystem detail in [`docs/`](./docs/README.md).

**Four client protocol families exist**, each a workspace member under `crates/protocol/` behind a
`lodestone-registry` feature: `v47` (1.8.9), `v340` (1.12.2), `v735`, `v770` (protocol 776 / MC 26.2).
`v340` alone is ~18k lines, so "v770 only" is wrong. Three load-bearing consequences:

- **No family is enabled by default** in `lodestone-registry`; the shell's default `live` feature turns on
  `v770` and nothing else, so a legacy family is invisible to every command below unless you name its feature.
- **Only `v770` implements `ServerProtocol`**, so 26.2 is the only version we can *host*. Joining and hosting
  are different sets, and `lodestone-registry` keeps `Family` and `ServerFamily` as two tables for exactly
  that reason — read its doc before assuming a family you can join is one you can serve.
- **`v735` speaks protocol 754** (1.16.5). The folder name is not the protocol number, unlike the other
  three. Never derive the protocol from the folder — ask `VersionAdapter::supports`.

New gameplay work targets `v770` unless an issue says otherwise; that is a default, not a scope.

---

## Build and test

`just` (see [`docs/task-runner.md`](./docs/task-runner.md)) is the canonical *command* layer — one name
per raw invocation, so the recipe is never the only record of what it runs:

```bash
just check       # cargo check --workspace --all-targets     -- the health check
just check-all    # cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench
just check-seam   # cargo check -p lodestone-shell --no-default-features   -- the version seam still holds
just test         # cargo test --workspace --no-fail-fast
just health       # all four of the above, in order
just run          # cargo run --release -p lodestone-shell --bin lodestone   -- launch the game
just run-wasm     # cd web && trunk serve --release   -- launch the BROWSER build on :8080
```

**PGO is opt-in and stays off by default** — the only measurement so far is 14.6% fewer instructions
retired on a worldgen probe, which is a proxy rather than frame or tick time, and the build-side cost is
unmeasured. It is four recipes rather than a `--pgo` flag because the justfile's header forbids a recipe
that parses an argument and branches on it (the same reason `run-wasm` is its own name), and because the
cycle is inherently interactive — an instrumented binary only learns a useful profile from a
representative workload, which for a game means playing it:

```bash
just pgo-instrument   # RUSTFLAGS=-Cprofile-generate … cargo run --release   -- then play, then quit
just pgo-merge        # xcrun llvm-profdata merge   -- fold the .profraw files into one .profdata
just run-pgo          # RUSTFLAGS=-Cprofile-use … cargo run --release   -- play the optimised build
just build-pgo        # the same, build only
just pgo-probe        # ./scripts/pgo-probe.sh   -- reproduce the docs/pgo-experiment.md comparison
```

All of them use a private target dir (`LODESTONE_PGO_DIR`, default `target/pgo`), because cargo keys its
cache on `RUSTFLAGS` and sharing `target/` would cost every other build on the machine a cold rebuild.

`just run-wasm` is the browser surface, not a flag on `just run`: `web/` is its own workspace with its
own `Cargo.lock`, and trunk -- not cargo -- drives the build, so the two share no invocation to
parameterise. `--release` is mandatory there for a reason unlike the native one's: a debug wasm build
makes single-threaded worldgen ~10x slower, which **blows the singleplayer probe's own 30 s deadline and
so presents as a failure rather than as slowness**. Joining a real server additionally needs
`just run-relay` up, because a browser cannot open a raw TCP socket.

All four *checks* are required, and each catches a class the others structurally cannot:

- **`cargo build` is NOT a health check.** It skips test targets, so a crate whose lib compiles and
  whose lib-test does not reports green. Always `--all-targets`.
- **`--all-targets` alone misses non-default features.** `live_inventory.rs` sat broken behind the
  `live-inventory` feature for a whole session — invisible to the first command, caught immediately by
  the second. The `--exclude` is not a workaround: `lodestone-allocbench` has a deliberate
  `compile_error!` when more than one allocator feature is on, because each installs its own
  `#[global_allocator]`, so plain `--all-features` **structurally cannot pass** and chasing it is wasted
  time. With that one crate excluded, the whole workspace is clean under `--all-features`.
- **No `cargo check` sees a doctest, at any feature setting.** A doc example that no longer builds is
  invisible to every check in this list: the `lodestone-data` extraction (#361) passed all three green,
  then failed `cargo test --workspace` on one stale `use` inside a `///` block. Prose that *mentions* an
  old crate is usually fine; it is the fenced code that rots. **After any crate rename or module move,
  grep the moved code for the old crate path and run `cargo test` — not just `check`.**
- **`cargo test -p <crate>` is not one either — it fail-fasts.** It aborts at the *first* failing test
  binary, so everything alphabetically later is never run and never reported: what looked like "a red
  test" in `lodestone-v770` was really **three red binaries and 14 failing tests**, masked because
  `serverbound_change_game_mode` sorts first. **Use `--no-fail-fast` when assessing crate health.**
- **A targeted `--test <binary>` run is a *narrower* filter than `-p` fail-fast, and it hides the same
  class of thing.** Adding a `ClientEvent` variant changes the directive **sequence**, so every
  choreography test asserting an exact `vec![Directive…]` is a silent caller. **No `cargo check` can see
  this** — the break is a runtime `assert_eq!`. When you add an event or change what an adapter emits,
  grep for the **packet id**, not for the event, and run the crate with `--no-fail-fast`.
- **A second instance of that class, with a different trigger: widening a *system's* signature breaks
  every hand-rolled schedule, at runtime, and `--lib` cannot see it either.** Adding
  `ResMut<AudioEngine>`/`Res<FrameClock>` to `drive_mining` compiled clean and passed
  `cargo test -p lodestone-shell --lib` at the documented 1729/0 — while **three integration binaries**
  panicked with *"Resource does not exist"*, because each builds its own `GameTick` schedule rather than
  the production one and therefore inserts only the resources the system needed *before*. So the
  requirement is data, checked when the schedule runs, and **three separate instruments miss it**:
  `cargo check` (not a type error), `--lib` (a different binary), and `-p` fail-fast (aborts before
  reaching them alphabetically). Only `cargo test -p <crate> --no-fail-fast` across **every** target
  found it. The habit: when you add a resource to a system, **grep for every harness that constructs a
  schedule containing it** — a hermetic harness is a second, silent implementation of production's
  wiring, and it is exactly the shared-construction-path blindness this file already records for
  `render::frame_for`, one layer down.
- **`cargo check -p lodestone-shell --no-default-features` is now a required health check.** With `live`
  on by default nothing else proves the shell compiles with **no** version family — the entire point of
  the version seam, and the only thing stopping a hardcoded `v770` dependency creeping into shell code.
  Its failure mode is architectural rather than a broken test, so nothing else will catch it.
- **There is a fifth class, and none of the four sees it: `just wasm-check`.** It lives in CI rather than in
  `just health` (a full workspace build is too slow to fold into the command everyone runs), so **all four
  above can be green while `wasm32-unknown-unknown` is broken** — measured: a shutdown-signal fix gated a
  portable type behind `cfg(not(target_arch = "wasm32"))` while four use sites stayed unconditional, so
  `lodestone-server` compiled natively and not for wasm, and `main` shipped red for a whole agent's session.
  Run it after any `cfg` change, any dependency edit, and any module move.

  **And a green wasm compile carries almost no information about whether the browser runs.** The hazard table
  in `scripts/wasm-check.sh`'s header is now *measured* — each call compiled to a `cdylib` and executed in a
  wasm VM — and it corrects what this repo believed: **`std::fs::*` returns `Err(Unsupported)` and does NOT
  trap**, while `Instant::now()`, **`SystemTime::now()`** and `thread::spawn` all trap outright. So the
  filesystem family is *degradation*-class and the clocks are *crash*-class; treating them as one family is
  what let `SystemTime::now()` — named in no hazard list in this repo until now — sit live in five production
  sites, including the chat-caret blink, which runs every frame. **Reaching exit 0 for wasm32 is when you
  start looking, not when you stop.** `docs/browser-shell-port.md` carries the census and the open work.

  **And a confinement guard only covers the crate it names — the browser reaches about fifteen.** Measured:
  wasm32 was exit 0, all three `lodestone-shell` guard rules PASSed, and the tab still died on *"time not
  implemented on this platform"* three crates down — `Sim::build → Particles::new → ParticleEngine::new →
  from_entropy → SystemTime::now`, in `lodestone-particle`, which is not in the guard's list at all. **Every
  crate the browser links wants the clock rules**; a green guard means "the crates I remembered to name are
  clean". Same shape as the docs-index gate scanning three directories and not the fourth. Also refined by
  execution: `thread::spawn`/`sleep` trap, but **`Builder::spawn` and `available_parallelism` return `Err`** —
  so those two are degradation-class, and two of the four sites previously assumed fatal never were.

  **But classify the *call site*, not the API — an `.expect()` makes a degrading call exactly as fatal as a
  trapping one.** `net.rs` spawned an OS thread and unwrapped it, so `Builder::spawn`'s graceful `Err` killed
  the tab anyway, and it never appeared in a census that had (correctly) filed that call as degradation-class.
  A hazard census keyed on the function is blind to how the result is handled; read the handling.

  **And the `.expect()` can live in `std`, where no grep of ours will ever see it: `std::thread::scope`
  traps.** Measured the same way as the rest of this table — compiled to a `cdylib` and executed in a wasm
  VM — `Scope::spawn` reaches `Builder::spawn`'s `Err` through an internal `.expect()`, so it is
  **crash**-class despite being built on a degradation-class primitive, and under this workspace's
  `panic = "abort"` release profile that is unrecoverable. It froze the browser tab on world creation:
  `chunk.rs`'s two offload wrappers fanned out over `thread::scope` on wasm32, while `portal.rs` had
  already independently discovered and gated the identical hazard for its own call site — the same fix
  found twice, unshared, because nothing mechanical connected them. So a census that classifies by
  *our* call sites is not enough either: **a safe-looking `std` wrapper over a degrading primitive
  inherits none of its gracefulness**, and the only durable answer is a `wasm-check` rule, since the
  second discovery of a hazard is evidence the first one never became a guard.

  **And a rule written in prose is not a rule.** `lodestone-server` already carried this exact one in a doc
  comment — *"this crate must not call `std::time::Instant::now()` anywhere … because the crate links into a
  wasm32 bundle where that compiles and then panics at runtime"* — and **four sites violated it**. The rule was
  right, and it was unenforced text. `wasm-check.sh` now bans the clock paths mechanically in five crates (17
  confinement rules, all passing). Whenever the type system cannot express a constraint, make it *checkable*
  and check it; a comment stating an invariant is documentation of intent, not a guard.

**There is no way to wait for a background run. Run every build and test in the FOREGROUND.** This is the
most repeated operational failure in this repo by a wide margin — **six agents lost runs to it in a single
day**, each having read a rule forbidding it, because the rule was filed under memory pressure and the
instinct that leads there is *correct*: they wanted to report a real test count rather than omit one.

The mechanism is unforgiving. An agent that stops to wait is **marked complete by the harness** and its
wake-up is **discarded**. The run itself keeps going and finishes fine; nobody ever hears the result. It
costs the whole tail of a session, and the work is usually already done.

So the honest options are only these:

- **Run it in the foreground and let it take the time.** `cargo test -p lodestone-shell --lib` is ~7
  minutes, the full `lodestone-server` suite 14–29. **That is not a hang** — see the wedged-lock rule
  above, and sample the *children* rather than the parents.
- **Narrow the run** — `--lib` plus the specific binaries you touched is an acceptable substitute **if you
  say so explicitly**.
- **Report without it**, naming exactly what you did not run.

A partial, honest report beats a complete one nobody receives. And note the two ways a foreground run still
lies: **a filter matching nothing prints `0 passed; N filtered out` and exits 0**, and **a truncated run
reads as a real total** — a "1700/0 across 63 binaries" report was a ~13-minute cut-off of a ~29-minute
suite against 1972 real test attributes. **Say when a run did not finish.**

Smaller facts, each of which has cost someone an hour:

- **The binary is `lodestone`, not `lodestone-shell`** — the `[[bin]]` name differs from the crate.
- **`live` is now a default feature, and `cargo run --release` launches the game.**
  `--no-default-features` is the way to reproduce the version-free build.
- `default-members` makes a bare `cargo run`/`build`/`test` target `lodestone-shell` only; every command
  above says `--workspace` for that reason. Live and GPU gates are `#[ignore]`d. Run them explicitly:
  `-- --ignored --nocapture`.
- **A test total gathered while another agent is mid-edit is a sample, not a measurement.** The invariant
  is *zero failures and zero non-compiling targets*, never the absolute count. **A *timing* is worse — it
  will be attributed to the wrong cause** (a debug-vs-release story was pure machine load). Prefer a
  **counter over a duration**; a ratio only helps when both arms are measured **concurrently**. Re-run a
  timing-shaped failure **alone** before calling it a regression. (§12.19)

Oracles (not part of repo state — recreate them):

```bash
./scripts/live-oracles/creative.sh   # :25570 game, :25571 RCON — flat/creative/peaceful
./scripts/live-oracles/terrain.sh    # :25580 — normal terrain, for light gates
./scripts/live-oracles/survival.sh   # survival, normal terrain
```

---

## Repo hazards

**Single shared checkout, no per-agent worktrees. Multiple agents edit concurrently.** Everything here
follows from that. The incidents, with shas and counts, are §12.20–§12.37.

**Never `git add -A`. Never `git reset --hard`, `git checkout .`, `git stash`, or `git clean` (in any
form, including `-n`-then-`-f`).** In full, never any of these:

| never run | because |
|---|---|
| `git add -A`, or `git add <dir>` | sweeps up other agents' files. **Stage explicit *file* paths, never a directory** |
| `git reset --hard`, `git checkout .`, `git checkout -- <path>` | discards the working tree for that path — no diff, no reflog. **`git checkout -- <path>` is the same command as `git checkout .`, narrowed, and is banned too**; there is no safe pathspec for it here |
| `git stash`, `git pull --rebase`, `--autostash` | `--autostash` stashes the whole shared tree *for* you, silently. A live `stash@{0}: autostash` is someone else's safety net — **do not `stash drop` or `stash pop` it** |
| `git clean` | deletes **untracked** files — new crates, new `docs/*.md`, new oracle dumps, in no commit and no reflog. **No legitimate use here** |
| `git commit --amend`, `git push --force` | rewrites a commit others built on and absorbs their staged work. If your last commit was wrong, **land a follow-up commit** |
| `cargo fmt`, `rustfmt` | rewrites files you do not own, and the *cleanup* is the damage — reversing it against `HEAD` cannot distinguish new content from collateral formatting. **Format the lines you wrote, by hand** |
| `git reset`, after a pathspec commit | the source of every stale-index incident here. It exists only to clean up after the private-index route, and the pathspec form leaves nothing to clean up |

To move to a newer commit, use a throwaway `git worktree add --detach`, which touches nothing here; prefer
`git worktree remove` over leaving worktrees around.

**A worktree is right for *verification* and wrong for *long work*, because its base goes stale fast enough
to make the result unmergeable.** Measured: two agents each ran a crate-wide comment sweep in an isolated
worktree; by the time they finished, `main` was **122 commits** ahead of their base, and one of them had been
editing a flat `mobs.rs` that no longer exists on `main` at all — it had been split into `mobs/` hours
earlier. Both reported a green build and a passing suite, honestly, *against their own stale tree*: one
measured 1322 tests where `main` had 1466, and its single "pre-existing" failure passed on `main`, because
the gate it tripped had been fixed by work its base predated. **A green worktree proves nothing about
`main`**, and neither does a test count from one.

The recovery that works, and is worth doing before discarding anything:

```
mb=$(git merge-base <branch> main)
comm -23 <(git diff --name-only $mb..<branch> -- <dir> | sort) \
         <(git diff --name-only $mb..main   -- <dir> | sort) > safe.txt
git diff $mb..<branch> -- $(cat safe.txt) > recover.patch
git apply --check recover.patch     # refuses rather than half-applying
```

Files the branch touched **and** `main` has since changed are the conflict set; everything else applies
cleanly. That salvaged 132 of 172 files in one case (`--check` exit 0, then check/seam/suite all green
before committing). Redo the overlap on current `main` rather than resolving it. And note the tell that the
base is stale is not a conflict — it is a **file that exists on the branch and not on `main`**, which a
merge would silently resurrect.

**Commit with the pathspec form: `git commit -m "…" -- <your paths>`. This is the standard here, not a
fallback.** It commits exactly those paths and **ignores the index entirely** — the only property that
makes it safe — and leaves the index clean, so **do not run `git reset` after it.**

- Put `-m` **before** the `--`, or git parses the message as a pathspec and silently commits nothing. **It
  cannot introduce an untracked file either — it fails by committing nothing**, so anything that creates a
  file needs an explicit `git add <files>` first.
- **Read your own sha in the same shell invocation as the commit, and `git show --stat` it.** A no-op commit
  does not look like a failure: a later `git rev-parse HEAD` prints **another agent's** sha.
- **It commits *working-tree* content, so a path you name carries whatever is in it** — an accepted cost, not
  a blocker. **Name only paths in your own assigned cluster** and `git diff -- <path>` before naming it.
  **Do not block on another agent**; only a **mid-keystroke** file is worth waiting on.
- **But the cost is not only mis-attribution: carrying *half* of someone's cross-file change breaks `main`.**
  Measured — a `tick.rs` commit swept in a concurrent agent's dispenser hunk whose *other* half lived in
  `redstone_dispenser.rs` and `hopper.rs`, files the committer did not own. One half landed, the tree went
  red, and the honest fix (committing the pair) is exactly the thing the ownership rule forbids; it took the
  other agent committing minutes later to restore green. So **after committing a contended shared file,
  check the tree still compiles at HEAD** — not just that your own paths are intact — and if you have broken
  it this way, say so immediately rather than reaching for the other agent's files. The trap is that every
  individual rule was followed: the paths were disclosed, the index was clean, the sha was read back.
- **The index is shared: never leave work staged** — another agent's commit in the gap harvests it under
  their message, and one shell invocation is not an atomic transaction. `git add` "to see the diff" is the
  most expensive way to look; `git diff -- <paths>` touches nothing.
- **Check `git diff --cached` is empty immediately before every commit** — **a count, not an eyeball, and a
  verdict that depends on the count**; an unconditional `echo "(clean)"` is its own vacuous control.
- Staging **hunks** (`git add -p`, or a filtered `git diff` applied with `git apply --cached`) is how you
  commit into a file someone else is editing — but it stops *you* shipping their lines, not them yours.
- **And hunk staging followed by a pathspec commit is self-defeating, because the pathspec form ignores the
  index — the very property that makes it safe everywhere else.** Measured: an agent carefully isolated its
  command-block hunks from a concurrent TNT feature in four shared files, then committed with
  `git commit -m … -- <paths>`, which took the *working tree* and swept the TNT hunks in regardless. The
  other agent's next commit then reverted the command-block hunks by reconstructing those files from a stale
  base, and a third commit had to restore them. **If you have staged hunks, commit them through the index**
  — the `GIT_INDEX_FILE` + `commit-tree` route below — and never with a pathspec. Choose one mechanism per
  commit: pathspec *or* index, never both.
- **The reciprocal, from the other agent in that same collision: a private-index reconstruction must be built
  from *current* `HEAD`, not from a snapshot you captured earlier.** Between capturing its base and committing,
  the other feature landed; the reconstruction was therefore a tree in which that feature did not exist, and
  committing it **reverted a landed feature** in four files. The compare-and-swap in `update-ref` did not help,
  because it protects the *parent*, not the tree. **The tell is `git show --stat` reporting pure deletions for
  files you were adding to** — check that before believing a reconstruction, and recover with a follow-up
  commit, never an amend. Both agents here disclosed the incident from their own side, which is the only
  reason it was untangled rather than silently kept.
- **Third instance, same shape, different cause — and this one is a *shell* failure, not a git one. The step
  that establishes your base can fail silently and `set -e` will not fire.** A `git checkout --detach` inside
  a throwaway worktree failed, the script continued against a **stale `main`**, and the commit built on that
  parent **discarded five other agents' commits** when `update-ref` moved the ref. The CAS again protected the
  parent and not the tree, so nothing objected. All five were recovered from the object store and merged back.
  The rule that generalises: **read the exit status of every step that establishes your base, not only the
  steps that write** — a `checkout`, a `read-tree`, a `worktree add`. `set -e` does not cover a command whose
  status is consumed by a conditional, a pipeline or a function call, and the failure presents as a correct
  commit against the wrong world. Sanity-check the parent you are about to build on (`git rev-parse HEAD` in
  the same invocation, compared against the shared checkout's) before `commit-tree`, exactly as you already
  sanity-check the tree's file count.

**`GIT_INDEX_FILE` + `commit-tree` is the escape hatch, and its only use is partial-file granularity.**
Two traps: **`git write-tree` against a missing index writes the EMPTY tree, silently, and that commit
deletes the entire repository**, and **the compare-and-swap in `git update-ref <new> <old>` protects the
*parent*, not the tree you built**. So: `read-tree → add → write-tree → commit-tree → update-ref` in **one
invocation** (**shell state does not persist between tool calls**), **read the tree and commit it in one
step**, use **a literal nonce** rather than `$$` or `$RANDOM`, **sanity-check the tree against a plausible
file-count floor before moving the ref**, and run `update-ref` **first** — a refresh before it stages a
reversal of the commit you just made. (§12.27–§12.28 carry the exact sequence.)

**And clean up after it, because the route leaves the SHARED index staging the exact inverse of what you just
committed — which means the next agent's plain `git commit` deletes your work.** Reported independently by two
agents in one session; one measured `git diff --cached --stat` at **68 insertions, 1,250 deletions**, precisely
the reverse of its own commit. This is why the ban table says `git reset` "exists only to clean up after the
private-index route": after `commit-tree`, `git reset -- <your paths>` (or `git read-tree HEAD` for a
content-only refresh) **is** the sanctioned step, and skipping it is the hazard. Two things about doing it:
**verify the reversal set is exactly your own files first** — the identical symptom appears when you are about
to discard someone else's staged work — and **confirm the count goes to zero afterwards**, a count with a
verdict depending on the count. The trap is that a clean `git status` for *your* files is exactly what an
inverse-staged index looks like.

Editing, and reading the tree:

- **Never rewrite a shared file wholesale — edit the lines you mean.** No git command is involved, so no
  ban above catches it: a full new copy silently discards every concurrent edit. **Re-read a shared file
  immediately before writing to it**, not at the start of your task. `sim.rs`, `app.rs`, `gpu.rs`,
  `server.rs` and `docs/README.md` are the usual victims. **It happens: an agent rewrote `server.rs`
  wholesale and silently dropped another's `ticks_after_place` call**, with nothing red and no conflict.
- **So protect your own edits with a marker check, not with vigilance.** The victim above caught it only
  because it had a mechanical check that **grepped for a distinctive symbol from its own change and required
  a non-zero count** — a count with a verdict depending on the count, exactly as for `git diff --cached`.
  After landing work in a contended file, re-grep for one symbol per edit; zero matches where you expect one
  is the *only* signal you will get, because a wholesale rewrite leaves a clean tree, a green build, and no
  diff to read.
- **A red test here may be someone else's *deliberate* neuter, and no diff can tell you** — a diff and a
  test run are **two observations at two different moments**. **Before reporting a red `main`, re-run at
  the committed sha in an isolated worktree.** When *you* neuter something, keep the window short and
  restore by `cp` from a scratchpad backup **with an md5 check** — never `git checkout`.
- **But "pre-existing" means "not caused by my change"; it does NOT mean "not a bug", and conflating the two
  is how real defects survive a whole session.** Three instances in one day, each disclaimed by several
  agents in turn, each real: two `lightning::tests` failures (genuine test defects — an RNG search space too
  small, then a fixture bug it unmasked), and **~29 end-to-end `lodestone-v770` failures** that turned out to
  be a live regression from inserting `SetCompression` into the login directive sequence. Every agent
  correctly proved the failure was not theirs — the worktree check above works — and then moved on, so the
  correct half of the procedure licensed skipping the rest.

  **The discriminator is one command: re-run a single failing test alone, single-threaded.** A contention or
  resource artefact needs load to reproduce; a real defect fails alone. The v770 case failed alone in
  **0.00 s** with an immediate assertion failure, which is unambiguous — nothing about it was environmental,
  and a count that moves with load (42 under load, 29 quiet) is *not* evidence of flakiness when the
  individual failures are deterministic. So: **disclaim ownership if you like, but run the one command before
  calling a failure environmental**, and say which you established — "not mine" and "not a bug" are two
  claims and the second needs its own evidence.
- **A clean log for *your* files, in a build someone else broke, is not evidence your files are clean —
  unless you prove the compiler got that far.** The control is cheap: **plant a deliberate type error in your
  own lib file, and a second inside your own crate's test file, then check which ones come back.** Both cases
  were measured, and **they answer differently, so one control does not cover both**:

  | broken | your crate's diagnostics | verdict |
  |---|---|---|
  | **a sibling crate** (`lodestone-shell` failing, you are in `lodestone-server`) | still emitted — compiled and warned normally | a *sibling*'s failure is **not** a short-circuit; keep working |
  | **a crate you depend on** (`lodestone-server` failing, you are in `lodestone-shell`) | **nothing at all** — a planted error in the agent's own lib file was never reported, zero diagnostics for its crate | wait it out; "my files look clean" carries **no** information |
  | **your own lib** | **test-file errors vanish entirely** — only the lib error is reported | a clean log for your *tests* means nothing until your lib compiles |

  The second row is the trap: a test target depends on its lib, so a broken lib **hides every error in its own
  crate's test files**, and removing the lib error makes the test error appear alone. So read a green test
  target as evidence only once the lib is green. Restore by `cp` from an md5-checked backup as above. Use this
  instead of blocking — only a **mid-keystroke** file is worth waiting on.
- **The scratchpad directory is shared too**, per-*session*, with none of git's protections. **Use
  uniquely-named files**, write them with the file tools rather than shell heredocs, and **re-read
  anything you are about to reason from** — a `#[path]` harness compiles whatever is on disk right then.
- **`docs/README.md` drift is red-`main`-shaped, and reverting it makes it worse.** Check `git status`:
  **uncommitted** drift belongs to a mid-flight author; only **committed** drift is yours to fix, and then
  **regenerate and commit `docs/README.md` alone** (`cargo xtask docs-index`, or `LODESTONE_REGEN=1 cargo
  test -p xtask docs_index_matches_committed`) — correct and expected, not a foreign-line violation.
- **`rtk` is not a transparent proxy. Do not trust it for evidence — use `/usr/bin/grep` and the real
  `cargo`/`git`.** It **strips the matched pattern and everything before it on the line**, and has
  reported **exit 0 while its own output said 7 failed** — unpredictable per subcommand, which is worse
  than uniform. **Re-read every exit code from a captured file with a program, not from a pipeline.**
- **An error whose path contains `/scratchpad/` or a `wt-` prefix is not about your code** — ignore it and
  re-run. **Never point `CARGO_TARGET_DIR` at the shared `target/` from a throwaway worktree**: it bakes
  that path into build-script output and poisons everyone else's build until someone runs
  `cargo clean -p <crate>`. **A no-op result from a repair step is not evidence the thing you repaired was
  healthy** — check whether someone else already fixed it first. (§12.37)

The machine:

- **The disk fills, and *which* subdirectory is to blame changes between measurements. Measure, never recall.**
  Three readings of `target/debug` now exist and they do not agree on even the ordering, so any rule naming a
  culprit is a rule that will be wrong on its next reading. Six times in one session `target/`
  reached 100–118 GB against a volume with ~30 GB usable, free space hit zero, and **every `Bash` call then
  failed before running** because the harness could not write its own output file — which reads as a dead tool,
  not as a full disk. **An earlier version of this rule blamed `target/debug/incremental` and prescribed
  `CARGO_INCREMENTAL=0`; that was measured wrong.** At 113 GB of `target/`, `build` was **101 GB** and
  `incremental` **4.2 GB** — 24× apart, so deleting the cache bought back only ~4 GB each time, which is exactly
  why it kept recurring. **That ratio is not a constant, and treating it as one is the next version of the same
  mistake**: re-measured later the same day at 80 GB of `target/debug`, the split was `build` **46 GB** and
  `incremental` **34 GB** — 1.35× apart. **A third reading then inverted the ordering outright**: at 101 GB of
  `target/` (86 GB of it `debug`), `build` was **35 GB** against `incremental`'s **51 GB**. So the successor
  claim — that `build` is *reliably* the largest and `incremental` merely non-negligible — was itself wrong, and
  wrong in the same way as the original: it generalised a ratio from the readings taken so far. The three splits
  are 24× one way, 1.35× one way, and 1.46× **the other**. Nothing about this ratio is stable, so
  **measure the split before choosing what to delete** and quote the reading you just took, never one from this
  file. This toolchain puts intermediates at
  `target/debug/build/<pkg>/<hash>/out/*.rcgu.o` and **never GCs the stale hash directories** — 2,150 of them
  under `lodestone-shell` alone, one holding 16,900 objects. `CARGO_INCREMENTAL=0` does not touch them.

  So the reclaim that works is **`rm -rf target/debug`** (72 GB in one go, measured), and it is safe when no
  cargo/rustc is running: it is all regenerable, and **keeping `target/release` means `just run --release` still
  starts immediately** rather than costing a full release rebuild. `build/` regrows fast — back to 9.6 GB within
  the hour — so treat this as periodic maintenance, not a one-off. Still prefer `cargo check -p <crate>` while
  iterating and `--all-targets` once at the end; that reduces churn even though it is not the main driver.
  **Never reach for `git clean` to reclaim space** — it deletes untracked files, which here means other
  agents' new crates, docs and oracle dumps, in no commit and no reflog. And **do not purge `target/` while
  another agent is mid-compile** — it happened twice in one session and cost an agent its workspace test run.
  **Its signature is worth memorising: a flood of `E0463 can't find crate` affecting every crate uniformly**
  (247 of them in one run) means rlibs were deleted underneath a live build, not that anything is broken. It
  looks exactly like a compile break, and **a compile break defeats `--no-fail-fast`**, so the whole suite
  reports nothing; the re-run is green. A second independent measurement put `target/debug/build` at **69 GB
  across 11,272 hash directories all created in one day**, 27 GB under one crate in 1,614 of them — test
  binaries living in build-script `out/` dirs that cargo never collects. Whether that is aggravated by a
  worktree ever pointing `CARGO_TARGET_DIR` at the shared `target/` (the poisoning this file warns about
  elsewhere) is unestablished and worth checking before anyone optimises further.
- **Docker is fair game to stop and prune when no live gate needs it**; the oracles are not repo state and
  `scripts/live-oracles/{creative,survival,terrain}.sh` recreates them. Quitting Docker Desktop reclaims
  the VM reservation, the largest single win; restart it before any `#[ignore]`d live-oracle gate. Prune
  images and build cache freely, but **think before pruning volumes** — the only Docker object holding data
  nothing recreates. Docker's `name=` filter is a *substring* match, so name targets explicitly.
- **Do not kill Bitwarden.** It hosts the **ssh-agent that authenticates GitHub**, so killing it to
  reclaim memory breaks every push in the session, including other agents'.
- **`-j` bounds rustc, not test binaries** — single test binaries here measure 4.8–5.2 GB RSS against
  16 GB total, and unbounded test memory force-rebooted the machine. When many agents are live: **pass
  `-- --test-threads=2` to `cargo test`**, **prefer `cargo check` when you only need to know it
  compiles**, and **never run two cargo commands concurrently or background one and start another**.
- **"Idle cargo processes and zero `rustc`" is NOT a wedged lock — a `cargo test` run has no `rustc` in it.**
  Measured: five cargo processes, all at **0.0% CPU**, oldest 22 minutes, zero `rustc` — reported by an agent
  as the documented lock-wedge and very nearly acted on by killing them. They were healthy. The parents idle
  *because they are waiting on their children*, and the children were **five test binaries burning 770% CPU
  between them**. Killing on that reading destroys live agents' work and looks justified doing it.

  **Sample the children, not the parents**: `ps -Ao pid,etime,%cpu,command | grep "[t]arget/debug"`, and read
  a **CPU sum across two samples**. Non-zero and moving means progress, however idle the parent looks. A real
  wedge needs *no* cargo child doing work either — and before killing anything, prefer waiting, because a
  cold `cargo check -p lodestone-shell` measures ~13 minutes under Cranelift and a full server suite runs
  longer. The generalisable form is the one this file already applies to counters: **a process's own CPU
  figure says nothing about the tree beneath it.**
- **`Pages free` is NOT headroom** and a threshold on it is actively harmful — it stalled an agent that
  obeyed one. Real pressure is **non-zero `used` in `sysctl -n vm.swapusage`**, **`Swapouts` climbing in
  `vm_stat`**, and `memory_pressure`'s own free percentage; compressor *growth* across readings, never one
  absolute value. **Load average is the worst proxy of all.** And **"wait" must never mean arming a
  background monitor** — an agent that stops to wait is marked complete by the harness and its notification
  discarded, the most repeated operational failure in this repo. Re-read `vm_stat` a bounded number of
  times **inside one shell invocation**, or run the cheaper command.

---

## The two rules that matter most

### 1. Nothing is done until something on screen changes

The dominant defect class here is the **island**: a subsystem that is individually built, individually
tested, and reaches **zero pixels** because nothing calls it. Nine confirmed instances. The tree is
green, the counters look plausible, and the screen is wrong.

A crate's own test suite is a **closed loop** — it can be entirely green while the crate is dead code.
Only a gate that asserts *coverage inside the subject's screen rect*, plus a negative control that must
fail the same assertion, can see an island.

Ask of every piece of work: **what actually consumes this?** Treat "nothing" as a defect report, not a
status update. Assign work end-to-end, from data through to draw, rather than by crate.

**Every terminal `_ =>` arm in an event router is an island factory, and there are three.** A system can
be correct, registered in the right set and order, and unit-tested green, and still never run, because
`SharedState::apply` only forwards events the switch lists — and a hermetic test that calls the system
directly passes either way.

| router | carries | missed instance |
|---|---|---|
| `ingest::handles_event` | per-entity ECS state | `EntityDamaged`/`EntityHurtAnimation`, air supply |
| `session::handles_event` | local-player session scalars | — |
| `net.rs`'s `forward` | the shell's own `ClientEvent` stream | `BLOCK_EVENT`, so chest lids could never animate |

**`ingest` vs `session` is a real fork and guessing it wrong has cost work twice** — `apply` consults
*both*, so an arm in the wrong one compiles, tests green, and never runs. **Per-entity state is `ingest`,
local-player scalars are `session`**, and block/world events are neither, travelling the shell stream.
When a decoded packet reaches no pixels, grep its variant in *every* router before blaming the decode.

**Islands come in both directions.** `ClientAction::SetFlying` was encoded by four adapters with **zero
producers** outside `crates/protocol/`, so the server kicked us with `multiplayer.disconnect.flying`.
**Ask what *sends* a serverbound action, not only what consumes a clientbound one.** (§12.38)

**And the reported layer is almost never the broken one — so trace the whole chain before fixing where you
were told.** Six instances in a single day, each filed as one thing and actually another:

| reported as | actually |
|---|---|
| tame wolf never reaches the wire | it *is* on the wire; **no ECS component folds it**, so the draw site cannot receive it |
| leashing unimplemented | implemented and pulling the mob; **no `SET_ENTITY_LINK` encoder exists**, so the rope is invisible |
| lightning absent | strike selection fine and the **client consumer already live**; zero server-side producer |
| server login has no compression | the codec was correct and the server's `SetCompression` arm **existed but was unreachable** |
| nine-slice measures the declared size | **fixed already**; the doc comment above it still described the bug |
| Create New World misses the canvas stamp | it reaches the stamp; the screen had **no arm in the hover switch** |

The pattern is that a chain of five or six hops is complete except for one, and whoever filed the issue
inferred the broken hop from the symptom rather than walking it. Two consequences. **Write the task as
"trace action → server → wire → pixels and say which link you verified", not as "implement X"** — the
agents given that framing found the real hop; the ones given a layer went to the named layer. And
**"the code exists" is never sufficient evidence to close a feature issue**: ask what *consumes* it, and if
the answer is nothing, the issue is more accurately open than closed.

The cheap mechanical tell is an **`#[allow(dead_code)]` nobody removed**. Treat removing one as the signal
the island actually closed — and if the attribute is still needed afterwards, the wiring did not take.

**A whole corpus can stop one layer above the defect, and then no test in it can see a dead draw branch.**
Measured: the menu tab widget's `draw_tab` had **never once run** since the day it landed. `draw()` checked
`MenuRow::tab` *nested inside* `if row.slot.is_some()`, and a tab row never carries a slot — its rect comes
from a separate `row_rect` arm keyed on `row.tab`. So every tab row fell through to the un-slotted "centred
stack" path and drew as a plain button, on **both** consumer screens. Not one gate caught it, and each gate
was individually well written: they build a `MenuFrame` and assert on *it*, and never ask `draw`/`geometry`
to rasterise it. The frame was always correct; the branch that consumes the frame was dead.

So the audit question is not "is this tested?" but **"does any gate in this subsystem reach the layer that
actually emits geometry?"** If every test in a corpus stops at the model, the entire renderer below it is
unguarded by construction — the same shape as a corpus that shares one fixture value or one construction
path, one level lower. Note also the honest form of the neuter here: the new rasterising gates **failed
before the fix and passed after**, and that failing run *is* the control. A control you observed fail is
worth more than one you constructed afterwards.

**A defaulted trait method plus a wrapper impl is an island generator, and it compiles silently.** Measured:
gating the block-entity scan on residency added `ChunkSource::is_column_resident` with a `true` default, and
neither `Arc<S>` nor `DimensionalSource<S>` forwarded it. Production always wraps `ChunkStore` in
`DimensionalSource`, so every call took the default and **the entire fix was a no-op in production** while its
own tests — which construct the inner type directly — passed. Nothing is red, because supplying a default is
exactly what makes a wrapper compile without forwarding.

So when you add a method to a trait, **grep for every `impl <Trait> for` in the workspace and check each
forwards**, not just the implementor you had in mind; wrappers, newtypes and blanket impls are the ones that
silently inherit. Prefer no default at all when the honest answer is per-implementor — a compile error naming
each unforwarded wrapper is worth more than a default that is right for one of them. And note the *test* here
could not see it: a gate that builds the concrete type bypasses the wrapper production always uses, which is
the shared-construction-path blindness under a different hat.

### 2. Re-verify before routing around "X doesn't exist yet"

Staleness is the most common defect in the written record — **seven instances in one session**. Every
stale claim was *true and evidenced when written*, which is exactly why it survives review: nothing
about it looks wrong on inspection. **A file path in this document is a claim like any other; verify it
before relying on it.**

**A predecessor's "blocked on X" is a claim like any other, and it decays faster than a doc — because it is
never revisited by anyone.** Four in one day, each stated confidently by a careful agent and each false:

- *"`OwnerHurtByTargetGoal` needs `server.rs`, which is off-limits"* — `apply_attack` already threaded
  `PlayerIdentity` through and `MobSim::tick` already resolved mob-hits-player. **Zero `server.rs` edits
  were needed.**
- *"`TITLE`/`CRAFT_PROGRESS_BAR`/`TAB_COMPLETE` have no canonical `ClientEvent` target"* — **all three
  already existed**, produced by v770 under different names (`TitleText`, `ContainerData`,
  `CommandSuggestionsReceived`), each with a live consumer. Four passes stopped at that wall and a fifth
  walked through it in one grep.
- *"`Stab` has no reference in the 26.2 source beyond an unrelated enum value"* — it is
  `Minecraft.startAttack()` gating on `DataComponents.PIERCING_WEAPON`, and seven shipped items carry it.
- *"secure chat is entirely absent client-side"* — landed across eight commits in six prior sessions.

The mechanism is worth naming: a blocker is written at the moment of **greatest ignorance** about the
neighbouring subsystem — you stopped precisely because you could not see the thing — and it is then quoted
forward as a finding. **A search that failed is evidence about the search, not about the tree.** So when you
inherit a blocker, spend the one grep before inheriting it, and **search for the capability, not the name
you expected it to have**: three of the four above were found by looking for what the packet *does* rather
than what the porting family calls it. When you *write* one, say what you searched for, so the next reader
can tell a genuine absence from a missed synonym.

**And the freshest-looking claim of all is a *sibling's* just-measured report — which is exactly how a stale
comment gets laundered into a fresh measurement.** Measured twice in one day. An agent closing the
world-admin decode family reported, in its own final summary, that **"no permission/op model exists anywhere
in `lodestone-server`"**; that went into a brief as the first thing to build. `crate::access` had landed
**nine days earlier**. The agent had not invented it: it asked "why is each of these packets stranded?" and
answered from **the decode sites' own doc comments**, every one of which still said "no permission model" —
so a stale comment became a report, and the report became a task. The real defect was the exact inverse of
the brief: the model existed and six handlers were never gated on it, each still carrying the comment that
concealed it.

Two things follow. **A subagent's report is a claim like any other, and it decays *faster* than a doc
precisely because nothing marks it as second-hand** — a doc at least looks like it has an age. Before
building on one, spend the same grep you would spend on an issue body. And **when a comment states an
absence, it is evidence about the moment it was written, not about the tree** — so when you wire something up,
grep for comments asserting it does not exist and delete them in the same commit, because the next reader's
"blocked on X" will be quoting yours.

**The highest-decay content in this repo is a doc's own status annotation** — "Landed", "still open", "not
implemented", "blocked by". A drifted *citation* eventually fails visibly, because the path stops resolving
or the symbol stops existing; a wrong **"Landed: no"** stays perfectly plausible forever, and nothing about
reading it suggests checking. Four instances surfaced in one day, all as **by-products of a citation sweep**
rather than by anyone reviewing the claims:

- a plan's blocker-4 prose said the Configuration-state `RESOURCE_PACK_PUSH`/`POP` arms did not exist; there
  are four such arms, one carrying a comment saying it was added for that blocker
- `bevy-migration.md`'s Stage 1 "Landed" bullet asserted a type "did not die" that is deleted tree-wide, and
  its Stage 5 "Moves:" list names fields the struct no longer holds
- a `GuiScaling::geometry` doc described the bug its own call site had already been fixed for
- three doc comments said the enderman gaze cone *widens* with range; the formula narrows it

Two things follow. **When you land something a plan or roadmap doc tracks, update that doc's status line in
the same commit** — it is the one piece of prose guaranteed to be wrong otherwise, and the tracker does not
cover it because these live in `docs/`, not in issues. And **treat a status annotation as unverified until
you check the tree**, exactly as with a file path: a doc claiming work is outstanding is the cheapest way to
send an agent at a problem that no longer exists, which has now happened repeatedly.

- **Zero hits in the file a stale note names is not evidence a feature is unwired** — **grep for the
  producer across the whole tree, not for the consumer in one named file.**
- **Read the record definition, not a summary of the call site.** Vanilla's
  `DepthStencilState(…, 1.0F, 10.0F)` was transcribed as "constant 1.0, slope 10.0" — backwards.
- **A field declared on a *base* record is inherited by every variant, and reading it on only the variant
  you cared about silently drops it everywhere else.** Measured: `"transformation"` is a field of every
  `ItemModel.Unbaked`, not of `SpecialModelWrapper.Unbaked` alone — `bake(context, transformation)` threads
  an accumulated matrix down and each node composes `Transformation.compose(parent, this.transformation)`.
  Our parser read it only on `special` nodes, so `shield.json`'s `scale [1.0, -1.0, -1.0]` — vanilla's
  `ShieldSpecialRenderer` flip, hoisted out of code into data on the enclosing `minecraft:condition` node —
  was dropped, and **every shield rendered back-to-front**. **14 of the 91 `special` nodes in the 26.2 jar
  inherit rather than carry**, so the sample that "worked" was 85%.

  Three things generalise. **The accumulated value is a chain, not an `Option`** — model it as the
  root-to-node composition, because an `Option` cannot represent a pack that nests two. **A wrong transform
  is invisible at the draw site and disguises itself as a texture or blending bug**: here the symptoms were
  "a dyed and a plain shield are bit-identical" and "a loom pattern moves 0 pixels", because the two base
  textures differ in **exactly 200 texels, all inside the front-face UV rect**, and every mask is **0/264
  opaque** over the back face. Five pipeline layers were instrumented and all five were genuinely correct —
  **the defect was an input that never entered the chain**. So when every layer verifies and the output is
  still wrong, stop instrumenting downstream and go find what was dropped upstream. And **when a parser
  reads an inherited field, grep the source data for how many instances rely on inheritance** before
  believing the sample you tested.
- **A hand-rolled Rust lexer will be wrong about lifetimes** — `&'static str` opened a "char literal"
  flag that never closed, silently disabling comment detection in three scanners.
- **A grep for a *field name* finds every struct that has one — read what the literal belongs to before
  building a diagnosis on it.** A `mip_level_count: 1` in `lodestone-render`'s `block.rs` was quoted as
  proof the block atlas had no mip chain; it is `DepthBuffer::new`'s depth texture, and the atlas has had
  a real per-sprite chain all along (`AtlasBuilder::with_mip_levels` → `GpuAtlas::upload_mips`). The tell
  is that a field-name hit carries no subsystem: name the enclosing symbol in the claim, and if you
  cannot, you have not read enough to make it. The agent sent at that diagnosis correctly refused it and
  found the real bugs — **no gutter reserved for the stitched sprites, and the gutter extruded only at
  mip level 0** — which is worth its own note: *isolating mip **generation** per sprite does not isolate
  **sampling***, because a bilinear tap at a sprite's own UV edge still reaches its neighbour. Two
  independent guards, and having the first is what makes the second's absence invisible.

**Prefer `cargo xtask connectedness` over any hand-derived coverage number**; the hand-derived version
has been wrong four times in four different ways. **But first check the instrument runs at all** — it was
found *unable to execute*, bailing with `duplicate play serverbound decode arm`, so every figure quoted
while it was broken was hand-derived after all. The cause is worth knowing because it is invisible at
runtime: `server_protocol.rs` had **two** `State::Play` decode arms for the same packet id, an old
`Ignored` stub shadowed by a real arm added later and never deleted. Rust takes the first satisfied guard,
so behaviour was correct and only the scanner choked. **Diagnostic: grep `server_protocol.rs` for a
repeated `if packet_id == play::serverbound::` guard.** A dead duplicate arm is also its own hazard — the
next reader edits the unreachable one.

**And an ordinary refactor can blind an instrument while every test stays green — so `SKIPPED` must never be
reachable for a subject that exists.** Splitting v770's 7,876-line `adapter.rs` into a `src/adapter/`
directory module was verified thoroughly (796/796 tests before and after, at one pinned sha) and
nonetheless made `connectedness` report v770 as **SKIPPED**: the scan hardcoded a search for a *flat*
`src/adapter.rs`. The one family the tool exists to report on vanished with no error, and the run still
exited 0. After the fix it reports 141/141. Two rules follow. **Re-run the *instruments* after a module
move, not just the tests** — this repo already says to re-run `cargo test` after a rename because no
`cargo check` sees a doctest; scanners are the same class and worse, because they fail *quietly*. And
**treat a skip as a failure unless the subject is genuinely absent**: a tool whose primary subject can go
missing without a non-zero exit is reporting a false negative, which is the same defect as a guard whose
detector errored.

Five instruments were found broken in a single day — five clock rules, the xtask rule table, this scanner's
duplicate-arm bail, this scanner's module-shape blindness, and `conformance`'s unconditional workspace
gate. **Every one reported success or silently declined to run.** None was concealing a real defect, which
is the good outcome and also the reason nobody noticed. Budget for auditing the tools, not only the code.

Know its scope, because outside it the instrument is *silent* rather than wrong (§12.40):

- It answers **"is this clientbound packet reaching anything"** and nothing else — not Rust call graphs,
  where it returns byte-identical output before and after a fix. For a crate-internal island, grep for
  constructors tree-wide plus a test that drives the *registry* rather than the type.
- **A low number does not mean the work is unwritten — in every legacy family, a chunk of it was already
  written, tested, and simply never dispatched.** The scan counts adapter match arms, so a fully round-tripped
  codec with no arm reads identically to code that does not exist. Measured across all three: v47 had
  `HeldItemSlot` and a clientbound abilities decoder sitting unused, v340 had `OPEN_WINDOW`/`CLOSE_WINDOW`/
  `SET_SLOT`/`WINDOW_ITEMS`, and v735 had `UpdateHealth`/`Respawn`/`SpawnPosition`/`HeldItemSlot`/
  `CloseWindow` — all with passing tests in the crate's own suite. The count is not wrong; it is answering
  "is this reaching anything", and the honest answer was no. But it makes a five-minute wiring job and a
  from-scratch port look the same. **Before writing any decoder, grep the crate for an existing struct and
  codec for that packet** — and note the dual hazard: writing a second decoder beside a working one is worse
  than doing nothing, because now both look plausible to the next reader.
- **Its granularity is the packet id, so a packet that multiplexes an enum hides an unconnected arm behind
  its connected siblings.** Measured: `PLAYER_ACTION`'s `SWAP_ITEM_WITH_OFFHAND` ordinal — the F key — fell
  through to `ServerBound::Ignored`, while the packet id counted as **connected** on the strength of its
  other ordinals, so the instrument reported health for a feature that did nothing. Any packet carrying a
  discriminant (`PLAYER_ACTION`, `PLAYER_COMMAND`, `INTERACT`, `CUSTOM_CLICK_ACTION`) needs its arms
  enumerated by hand; "the packet is connected" is a claim about the id, not about the behaviour you care
  about. The same reasoning applies to a `MetadataField` index shared across classes.
- **It cannot see a fully-connected wire carrying the wrong value.** #323: `SET_TIME` decodes and really
  does darken the sky, every link green, while the value is wall-clock elapsed-since-join and `tick.rs`'s
  real counter never reaches the encoder. Only a gate whose expected value originates **outside** our own
  producer can see that.
- **It also cannot see a render/instance struct field that no code ever *reads*, and that is its own island
  species.** `creeper_swelling` reaches the shader, is computed for real by the extract step, and was
  nonetheless dead: `prepare_entities` resolved every entity through a path whose swell is a hard `0.0`, so
  four functions between them had **zero production callers** and no creeper ever swelled. The field's own
  doc comment named "two consumers downstream, both in `gpu.rs`" and grep returned nothing.

  **The obvious detector is the wrong one, measured.** "Every assignment of this field is the same constant"
  reads it as 17 constants plus 1 computed — a *healthy* ratio — precisely because the extract step does
  assign a real value. The durable query is the **dual: a field with zero production readers.** Before the
  fix that field had 0 production reads and 4 test reads; after, 2. Test reads are what make the naive
  version lie, so count production and test call sites separately and report both. This is not landed as an
  xtask: a trustworthy version needs real parsing, and a hand-rolled Rust lexer will be wrong about
  lifetimes (three scanners here already were). Grep is the interim instrument — and asking "what reads
  this?" is the habit, not just the tool.

  **A third species is upstream of both, and it is the cheapest to find once you know the shape: a value
  the decoder reads off the wire and *discards at the decode site*.** Two in one day. The recipe-unlock
  toast's crafting-station icon was `let _category = …` in `read_recipe_display` — the walk over the
  trailing `SlotDisplay` was already written and correct, and its result was dropped on the floor. v47's
  `SPAWN_ENTITY_LIVING` decodes each mob's metadata entries (it must, to consume the trailing bytes) and
  then discards every one, so **no 1.8 mob carries baby, colour, sheared or on-fire state at all**.

  Neither existing instrument sees it. `connectedness` reports the packet as decoded *and* connected,
  because it is — the packet reaches a real fold, just without that field. The zero-production-readers
  query needs a field to ask about, and here **no field was ever declared**, so there is nothing to count
  reads of. The tell is lexical and greppable: **`let _`, a discarded tuple element, or a loop that parses
  and does not bind**, sitting in a decoder. Both instances read as deliberate — a `let _` is exactly how
  you *correctly* consume bytes you do not model — which is why they survive review. So when a feature is
  missing and the packet demonstrably arrives, **grep the decode path for a discard before suspecting the
  consumer**; and when you write one, say in the comment whether the value is unmodelled *by decision* or
  merely not carried yet, because those read identically six months later.

  **And the sibling species is now the most common defect in this repo: a correct function fed a constant
  by its *producer*.** Five in one day, each reported as a broken feature and each actually a supplying
  path that never learned the real value: `creeper_swelling` (`prepare_entities` resolved every entity
  through a path whose swell is a hard `0.0`), `ThirdPersonBodyState` (`swim_amount` hardcoded, so remote
  players swim and your own body does not), the anvil rename box, `NearbyEntity::living` (`collision_rule`
  defaulting to `Always`, so a team's `never` rule was invisible and players pushed each other), and
  `Bundle`'s empty sound lists. In every case the consumer was already correct and well tested. So when a
  feature "does not work" and the function that implements it looks right, **stop reading the consumer and
  go find who constructs its input** — and the count that finds it is not "how many sites assign this
  field" but **"how many production sites assign it something other than the default"**. Zero is the tell.
- **And a table keyed on "does this need behaviour?" is the wrong key for "does this need to exist?" — one
  membership test, two questions.** Measured: `block_entity_for_item` creates a `BlockEntityRegistry` entry
  only for the dozen block-entity types this server *simulates* (furnaces, hoppers, containers…), and both
  persistence and chunk serving source their block-entity lists from that live registry. A skull needs a
  record purely so the client will *draw* it — its render is block-entity-driven, like a chest's, not derived
  from block state — so it entered no registry, was never written to disk, and was served in a chunk packet
  carrying the right block state and an empty block-entity list. **~49 vanilla types are in this class and
  only ~12 were covered.**

  The symptom is the tell and it points at the wrong crate: a later `BLOCK_UPDATE` lets the client synthesize
  the record itself off the state, so the block **appears the moment you interact with it** — which reads as a
  client render bug, and was reported as one. **A thing that is invisible until touched is a missing
  server-side record, not a draw bug.** The repair also generalises: synthesizing the gap at *load* time, not
  just at placement, self-heals every already-broken save instead of only fixing new placements.
- **Do not quote a coverage number from memory or from a doc — run it and quote that.** Legacy families
  are thin, and *decode* and *connectedness* are different axes; five issue bodies inherited one
  wrong-axis figure. Serverbound decode lives in `crates/protocol/v770/src/server_protocol.rs`, **not**
  `lodestone-server`, and a variant decoding into `server.rs`'s `ServerBound::Ignored => {}` is stranded
  exactly as a clientbound packet would be — a **two-file join**, not a one-file scan.

---

## Evidence standards

**A derived constant can be tuned to cancel the bug you are about to fix, so the fix breaks it.** Measured:
a container's player-inventory row sat one pixel low because `main_y` used `18 + rows*18 + 14` where
`ChestMenu.inventoryTop` is `+ 13`. Correcting it turned a *different* gate red — `SlotLayout::height` was
**derived** from `main_y` through a shared `+ 82`, and that `82` had been implicitly chosen against the
*wrong* `main_y` so the panel height still came out right. Two errors cancelling, each invisible while the
other stood.

Vanilla settles it: `ContainerScreen`'s `imageHeight = 114 + rows*18` is an **independent** constant, and
the background asset's own geometry — a `rows*18 + 17` top blit plus a fixed `96` bottom blit — confirms
the two numbers are genuinely one pixel apart rather than derivable from each other.

So: **derive each ported constant from its own outside source, never from a sibling you also ported**, and
when a fix turns an unrelated gate red, **check whether that gate was calibrated against the bug** before
assuming the fix is wrong. The tell is a magic offset joining two quantities vanilla defines separately.
This is the arithmetic form of the closed loop `decode(encode(x)) == x` already warns about: two values that
agree only because they share one mistake.

**A doc that transcribes vanilla *correctly* is not evidence the code does — the transcription is the plan, and
nothing here checks the plan against the implementation.** `docs/fluid-rendering.md` carried
`FluidRenderer.shouldRenderFace` verbatim, **including** its `isFaceOccludedBySelf` conjunct, the entire time
that `mesh_fluids` implemented only the *first* conjunct. The doc was right, the code was half-right, and the
gap was invisible for as long as nobody diffed one against the other. **A conjunction is the dangerous shape**:
implementing one clause yields code that behaves correctly in most scenes, and — the reason it survives review
— the *sibling* it does implement is genuinely correct, so the call site looks finished. So when porting from a
transcription, **enumerate the clauses and point at the line implementing each**; treat a transcribed formula
as a checklist, not as a citation. Two instances in one day: this, and the falling-water fix, which was the same
mistake (a *self*-cell question answered from neighbours) in the same file.

**An expected value must originate outside the code under test.** `decode(encode(x)) == x` is satisfied
by two symmetric misunderstandings — hermetic chunk fixtures generated with our own encoder passed
throughout, then a live gate produced 49 × "unexpected end of input". Use captured server bytes, a JVM
oracle, or a hand-decoded spec example. Note that a self-authored JVM oracle validates *the behaviour
you chose to model*, so agreement across ports sharing an author is weak evidence.

**And when a fix and a gate disagree, the gate is as likely to hold the bug — a derivation written in a
doc comment is not evidence the derivation is right.** Measured: fixing mob knockback turned
`combat_live.rs` red, expecting a mob to fly *toward* its attacker. The gate looked rigorous — its doc
carried a full derivation, term by term — and the derivation's own first line said `dx = target.x -
attacker.x`, **labelled "attacker→target"**. Vanilla's `dealDefaultKnockback` uses
`source.getSourcePosition().x() - this.getX()`, the other way round. So the *test's expected value*
reproduced the exact sign error the code fix had just removed, and the label made it read as checked.
The reciprocal claim was wrong too: the gate's premise that a non-sprinting hit's knockback power is zero
contradicts `hurtServer`, which applies the flat `0.4` unconditionally and sprint-gates only the *bonus*.

Two habits. **Check a derivation's convention labels against the source, not its arithmetic against
itself** — the arithmetic was internally consistent and that is exactly why it survived. And **when a
change turns a gate red, derive the expectation afresh from the outside source before touching either
side**; "the newer code is right" and "the test is right" are both assumptions, and here the tell that
settled it was a *third* gate (`mob_attack.rs`) that had been correct all along and disagreed with the
first.

The cheapest instance of that symmetry, and it recurs in every packet: **two adjacent same-typed fields
transpose without a trace.** `TAKE_ITEM_ENTITY` is three VarInts, the first two being the item entity and its
collector — swap them and the round-trip through our own encode/decode is *byte-perfect*, while the client
lerps the **player toward the item**. So for any packet, assert against a byte string the *other* side already
decodes, and choose **pairwise-distinct** field values (`11, 1, 4`, never `1, 1, 4`) so a transposition cannot
survive. A field's value being distinct from its neighbours' is part of the fixture's job.

**Two adjacent `bool`s are the worst case of this, not an exception to it.** They coincide half the time by
chance, so a fixture that happens to set them equal cannot see a transposition *at all* — and unlike a numeric
field there is no "obviously wrong" value to notice downstream. Measured while formatting two debug toggles
into one `format!`: setting them deliberately **different** is what makes the arm able to fail. Applies to any
adjacent same-typed pair in one expression, not just on a wire.

**And the transposition can come from the record itself: a packet's declaration order is not its wire order.**
`ClientboundSetExperiencePacket` is constructed at its `doTick` call site as `(progress, total, level)` and
declares its fields in that same order — but its `write` emits progress, **level**, total. Transcribing the
constructor, or the field list, silently swaps two adjacent VarInts: wire-legal, survives every round trip,
and puts the wrong number on the XP bar. **Port from `write`/`read`, never from the constructor or the field
declaration** — for a record whose fields are all the same type, those are three different orders that all
look authoritative.

**A nested `catch_unwind` did not catch under Cranelift, and the failure looked like a broken production
invariant.** Measured: `random_tick_section_counters`'s tripwire control wraps a deliberately-corrupting
`tick_chunk` call in `std::panic::catch_unwind`, *inside* libtest's own harness catch. Under Cranelift the
panic escaped that inner catch and libtest reported a bare `FAILED` before the message-content checks ran;
the identical test passes under `CARGO_PROFILE_DEV_CODEGEN_BACKEND=llvm`. It cost real time because the
panic message named a counter desync and a plausible culprit (three new `_with_entities` mutation paths)
had landed the same day — **production was innocent**. The fix was `#[should_panic(expected = …)]`, using
libtest's outer catch, which is reliable.

So: **prefer `#[should_panic]` to a hand-rolled `catch_unwind` in a test**, and treat a panic-catching
boundary as backend-sensitive rather than a given. The workspace has 14 `catch_unwind` sites and two of them
are **production panic isolation** — `lodestone-ecs`'s `async_task`/`handle` job boundary and
`lodestone-fuzz`'s panic-to-finding conversion. Neither is verified under Cranelift; if either silently
stops catching, an isolated job kills the process and the fuzzer aborts instead of reporting a finding.

**A `const` has no stable address, and switching codegen backend is what makes that bite.** Measured the
day Cranelift became the debug backend: `ai::roster::FALLBACK` was a `const`, and `is_fallback` compared
pointers against it. A `const` is *inlined at every use site*, so it may occupy as many addresses as it has
uses — LLVM happened to fold them to one, Cranelift did not, and two `lodestone-entity` tests began failing
**deterministically**, reading a real unclaimed species as claimed. The fix is one word: `static`, which
does have a single address.

Two things generalise. **Pointer identity is only meaningful on a `static`** — if you find yourself
comparing `&CONST` addresses, that is the bug, not the backend. And **a codegen-backend change is a
behaviour change for anything resting on unspecified details**, so a green suite under LLVM is not evidence
under Cranelift; the failures it surfaces are latent bugs it *revealed*, not bugs it *caused*, and they
should be fixed rather than worked around. Release still builds with LLVM, so this class can differ between
`just run` and a debug test run — which is precisely the shape that reads as "flaky".

**Vanilla's `Mth.cos`/`Mth.sin` are a quantized lookup table, not `f32::cos`/`f32::sin`, and substituting the
standard library diverges exactly at the poles.** Measured: a fishing bobber cast straight down (`pitch == 90.0`)
launched *upward*, because at the pole the library's result lands on the wrong side of zero by float noise while
the table does not. Use `lodestone_physics::mth`. The general shape is that **a port's numeric substitution is
invisible in the middle of the range and wrong at the edge**, so a fixture sitting at a cardinal angle, an axis,
or a zero crossing is testing something a mid-range fixture cannot — and those are precisely the inputs a person
writing a test reaches for as "simple".

**And sampling only the destination cell after a move tunnels through thin geometry.** The same fix: the bobber's
settling code read the single cell below its *already-moved* position, so a fast fall could cross a one-block
floor within a tick and land on neither side of that sample, dropping into the void. **Scan every integer cell
the movement crossed**, not the endpoint. Any per-tick "am I on the ground / in water / inside a block" check
written against a post-move position has this bug latent in it, and it only shows at high speed.

**When a control lands between the two hypotheses, reframe what it asserts — never fit a threshold to it.**
Measured on the boat water-mask gate: the raft control was *"a raft must show water changing through its
hull"*, which is not a property a raft has — it has no cavity. Water paints over **9.2%** of a raft's
silhouette, against a **masked** boat's 5.8% and an **unmasked** boat's 41.2%, so the raft sits nearer the
masked boat and **any threshold placed there would have been fitted to the answer**. Restated as the exact
thing that is true — `"oak_raft"` and `"raft"` must render **byte-identically, 0 px** — it became
falsifiable, with the boat's own A/B (3,933 px) as the positive control that the comparison can see a mask
at all. **A control whose number sits between your two hypotheses is measuring the wrong claim**; the fix is
a different assertion, not a tuned constant.

**And a fixture can hide the exact condition the feature exists for.** The same gate stood the boat *on* the
water (`feet.y = SURFACE_Y`), so its own opaque hull hid everything the mask would hide — masked and
unmasked were pixel-identical and the A/B was **0**. The mask is for a **partially submerged** hull. The
fix was to derive the feet height from the patch plane (bob `0.375` plus half the `0.1875` plate) so the
surface meets the patch, **derived and asserted at runtime rather than tuned until pixels moved** — and
rebuilding the fixture is what surfaced a real production bug (`prepare_entities` submitted the depth-only
patch **before** the hull, depth-rejecting the hull's own below-waterline planks; vanilla's
`AbstractBoatRenderer.submit` calls `submitModel` *then* `submitTypeAdditions`). **Ask what state the
feature is for, and check the fixture is actually in it.**

**Assertions of an absence need a control proving the detector works.** "No corrective teleport", "no
trailing bytes", "zero unresolved" are only as good as the evidence the mechanism *would* have fired.
Run the control and observe it fail; do not describe what it would do.

**A sentinel that makes the assertion's *negative* form true is unfalsifiable, and `unwrap_or(f32::NAN)` is the
one to watch for.** Every comparison against `NAN` is false, so `!=` against it is always **true** — an
inequality assertion fed a NaN sentinel therefore passes for an **absent** value, which is precisely the case
it existed to catch. Measured: a font-advance gate asserting a glyph's advance was *not* the wrong number
passed with the glyph missing entirely; 13 of 16 gates fired under the neuter and this was one of the three
that did not. **Compare the `Option`, do not unwrap to a sentinel** — and when a neuter leaves a gate green,
that gate is the finding, not a rounding error in the run.

**And a control can only demonstrate as many arms as it is allowed to report.** An `assert!` *inside* a `for`
loop aborts on the first failure, so running the neuter proves exactly **one** arm and the rest stay
*arguments* rather than observations — you learn that some case failed, not that all four did. **Collect the
mismatches and assert on the collection.** Restructured that way, a full-cube neuter over the item-settling
gate failed **4 of 4** arms, each landing exactly on the wrong hypothesis's value (66.0 against true 65 /
65.5 / 65.9375 / 66.5); with the assert inside the loop, three of those four numbers would never have been
printed. Same reasoning as making failure output print a bounding box: the gate has to be able to *say* what
it measured.

**A control's premise can be false before the feature under test ever existed** — and it fails in the
*safe*-looking direction, because the control fires and what it measures is unrelated. **Before believing
a control, ask what else already paints here**, and derive layout from the same expression the draw uses
rather than restating a constant. (§12.41)

**And a gate's *invariant* can be false, derived from a geometric argument whose unstated assumption holds
only at one point in the frame.** Measured: a terrain-hole gate asserted that any sky pixel sandwiched
between two terrain pixels in a screen column is a bug, reasoning that a column's rays share one horizontal
ground track and so cross terrain monotonically. `Camera::basis`'s `up` carries a `cos(yaw)·sin(pitch)`
term, so that is true **only at the image's centre column** — off-centre, bearing drift lets one row graze
past a convex corner into real sky while a lower row reacquires ground, making terrain→sky→terrain legal.
**213 of the 215 pixels the gate reported were real sky.**

Three things generalise. **A screen-space invariant asserted over a whole frame is usually a centre-of-frame
truth**, so state which pixel the derivation was written for and ask what the term you dropped does at the
edges. **Replace an invariant you cannot defend with a per-sample oracle** — here, casting each flagged
pixel's own ray against real block data, transcribed independently of the renderer — which localises rather
than aggregates. And **the deliberate-defect control is what proves the new invariant did not simply become
permissive**: it must still classify 100% of its pixels as genuine bugs. A weakened premise that also
silences the control has not been fixed, it has been switched off.

**Measure by location, never by frame average.** A gate reporting only a fraction cannot tell a
uniform-but-wrong frame from a localised blob. Ask *where*, not *what*, and **make failure output print a
bounding box** — that diagnosed two premise-false controls in one step.

**But a bounding box is an instrument too, and this one was broken for as long as it existed.**
`text_colour.rs`'s `opaque_ink` cast raw NDC floats — clip space, `[-1, 1]` — straight to `i32`, so every
on-screen vertex truncated to `0` and **every failure in that file printed `(0, 0, 0, 0)`** regardless of
where the ink actually was. The presence/absence assertions were unaffected, since they read only `rgb`,
which is exactly why nobody noticed: the gate's *verdict* was right and only its *diagnosis* was empty. So
a box that is degenerate, constant, or suspiciously round across unrelated failures is a broken transform,
not a measurement — **check the box localises before reasoning from a gate that prints one**, the same way
you check a guard's detector actually ran.

**And a probe that samples *vertices* is blind to any quad larger than the probe.** `band_coverage` in the menu
render tests counts vertices falling inside a rect, so a quad that **encloses** the rect contributes none and
reads as zero coverage. Measured, not hypothesised: a canvas-wide tint painted straight through one gate's
probe rect and that gate still reported **0**. The failure direction is the dangerous one — a new full-screen
element is exactly what such a probe cannot see, so it certifies "nothing paints here" at the moment something
started painting everywhere. When a coverage check is point- or vertex-sampled, **say so in its name or its
doc**, and prefer testing the rasterised result (or the quad's own rect against the probe) when the thing you
are guarding against might be *bigger* than the window you are looking through.

**Three things already paint into a hermetic pixel gate's frame, and each one silently defeats a detector.**
Measured while building a flat-terrain hole detector, and each cost a wrong reading before it was found:

| hazard | symptom | fix |
|---|---|---|
| `RenderState` draws an **unconditional first-person bare arm** whenever no third-person body is reported | an *identical* 29-pixel "hole" at a fixed screen rect **regardless of camera yaw or pitch** | install a dummy `set_third_person_body_source` |
| the real background is **`SkyFrame::clear_color`** — a time-of-day and eye-height resolved fog colour under a sky disc — **not** `SKY_COLOR` | a hardcoded sky constant makes the detector blind: a control with a real, deliberately-removed section found **zero** pixels | diff against a rendered no-terrain reference frame |
| a hermetic `World`'s light defaults to `LightData::Missing`, which `SectionLight::sky_at` resolves to **0** | everything renders dark; `SkyDefault::Full` bridges an **absent neighbour** only, never a present-but-unlit section | `LightData::Uniform(15)` |

The invariance under camera angle in the first row is the generalisable tell: **a "world-space" defect that does
not move when the camera does is screen-space, and something else is painting it.**

**And a harness can submit geometry and readback nothing, with every counter reporting health.** The same gate
measured **zero pixel difference between a terrain world and an empty one** while `RenderStats` simultaneously
reported 59 sections drawn, 59 draw calls and 15,104 quads. So a non-zero draw counter is not evidence anything
reached the frame — **only the control firing is**, and when it does not fire the paired "pass" is *vacuous, not
exculpatory*. If you must leave such a harness in the tree, **banner it at the top**: its doc comment will
otherwise read as rigorous and authoritative to the next reader, which is worse than no harness at all.

**A gamma/linear blend mismatch has a signature you can measure rather than argue about: the divergence is
large against a dark background and ~0 near white** — the fixed points at black and white are where the two
spaces agree. Measured on the HUD's per-player tab-list background, where the constant (`0x20FFFFFF`) matched
`Options.getBackgroundColor` exactly and was never the bug: vanilla blends directly on raw gamma bytes,
`hud.wgsl` writes the same raw bytes uncorrected, and the render target is an `Srgb`-format view (native
`wgpu-core` picks one by default), so the hardware blends in **linear** light. **Before adjusting a colour
constant, sweep the background from black to white and look at where the error goes** — if it vanishes at both
ends, the constant is right and the space is wrong.

**Validate the instrument before optimising the system — a wrong counter does not merely mislead about
magnitude, it can invert the conclusion.** Measured: `vram_bytes` was computed from `stats.total_quads`, which
is accumulated **inside the terrain draw loops, after the cull** — a per-frame *drawn* quantity wearing a
*residency* label. Turning the camera 180° from the same eye moved the reported figure 26% (1,853,568 →
1,365,552 B) while true residency was byte-identical at 5,777,856. It was also pricing every live-vanilla quad
at the packed path's 72 B against a real `ModelVertex` quad's 152 B, so it under-reported ~2.1× **on top of**
the cull factor: real mesh VRAM at RD 8 is ~67 MB live where that line printed under 32 MB.

The conclusion drawn from it — *"we barely use any VRAM, so retain more"* — was therefore backwards twice: the
arena **already** retains everything (a pooled suballocator whose blocks are never released), and usage was
double what was shown. The prescribed fix would also have been actively harmful: there is no client-side
view-radius eviction to add hysteresis to, so retaining past the server's unload signal would have broken the
invariant behind *"never collide with terrain you cannot see"*. **So when a reported number looks wrong, the
number is a hypothesis too.** The cheapest discriminator is usually an input that cannot physically affect the
quantity — a pure camera rotation cannot change residency, so any movement in a residency counter under
rotation alone localises the bug to the accounting before you read a line of the subsystem.

**And a rate counter placed upstream of a throttle counts intentions, not events — so "N per second" is not
a flood until you check which side of the transform the counter sits on.** Measured: a log showing ~20
movement actions per 500 ms was read (by me, into a brief) as the client flooding the wire while standing
still. `send_move_action` really does push one `ClientAction::Move` per tick — but v770's
`select_move_packet` is already a tested port of `LocalPlayer.sendPosition`'s dirty-tracking, so on an idle
tick it emits **nothing**, and the count was of queued actions upstream of it.

The follow-on is the part worth carrying, because the "obvious" fix is a regression: **gating the producer
starves the consumer's counter.** That port's 20-tick `positionReminder` only advances when it is *invoked*,
exactly mirroring vanilla calling `sendPosition()` every real tick — so skipping pushes at the producer
would silently kill the periodic resync while changing the packet rate not at all. **When the dirty-tracking
lives downstream, the throttle belongs downstream too**, next to the "last actually sent" state it needs.
(The real gap the same investigation found is elsewhere: `v47`/`v340`/`v735` encode a move packet
unconditionally on every call, with no tracker of their own, so a legacy-family session genuinely does send
20/s while idle.)

**A throughput measurement structurally cannot see a latency defect, and the symptom will send you to the
wrong instrument.** Measured while diagnosing a keep-alive timeout: the server kicked its own client because
crossing a chunk boundary awaited generation *and* encode of a whole 33-column strip (361 on a jump) inside
**one** `select!` arm, so nothing was read or written for the duration. **`spawn_blocking` had already moved
the work off the core thread — offloading does not shorten a suspension point.** Per-column cost was ≈14.8 ms
and entirely healthy; **the defect was the number of columns per suspension point**, and streaming the strip
changed throughput not at all. So when the report is "it's slow" or "it disconnects", ask **how much work sits
inside one unserviced window** before optimising the work itself, and prefer *shipping an instrument* (a stall
watch that names the arm) over taking a wall-clock figure on a busy machine — a duration gathered while other
agents build gets attributed to the wrong cause. Two further traps in the same incident, neither guessable
from the aggregate: tokio's default `MissedTickBehavior::Burst` fires missed ticks **back to back with no
delay**, so a stall spanning two intervals writes a challenge and finds it unanswered in the same instant
(zero grace — use `Delay`), and a timeout denominated in **wall clock** rather than *serviced* time measures
something vanilla does not, whose reads happen on a thread that never blocks on worldgen. (§12.165)

**A shell pipeline will destroy the evidence you are about to reason from.** `| head` read as absence;
`| grep | tail` reported exit 0 because that is `tail`'s status, one command from a commit on a red tree;
`| tail` with no `-f` buffers until EOF, so a healthy build looked hung and was killed; and **zsh does not
word-split an unquoted `$var`**, so an audit whose whole job was to prove a commit had no foreign lines
returned green by measuring nothing. So:

- **Let cargo write its own output to a file and check its real exit status**, then filter the file; never
  put a buffering filter between a long build and your only view of it. **This includes not trusting a
  *wrapper's* summary of the run**: a task-completion notification reported **"exit code 0"** for a workspace
  test run whose real status was **101**, and only reading cargo's own status out of the log caught it. The
  harness around a build is one more transform that can invent a green.
- **Write the paths out, or `set -- a b c` and use `"$@"`.**
- **Treat an audit that prints nothing as a failure to run, never as a pass.** Sharpened by measurement:
  **a guard whose detector *errored* has measured nothing, and "no findings" must never share a value with
  "could not look".** Five of `wasm-check.sh`'s clock-ban rules printed a grep error and then reported
  **PASS** for weeks. The mechanism is worth memorising because it is generic: the rule table is
  `|`-separated and those five patterns spelled a BRE alternation `\(Instant\|SystemTime\)`, so **the field
  separator appeared inside the pattern** and `read` truncated it mid-escape; grep then exited **2**, and the
  `|| true` that exists to swallow grep's *no-match* exit **1** swallowed the error identically. So: read
  grep's status and treat `>= 2` as a hard failure printing its stderr, validate each table row's field
  count, and prefer literal substrings over regex metacharacters in any table whose separator is itself a
  metacharacter. The other twelve rules were correct **by accident**, not by construction — nothing required
  a pattern to be separator-free.

  **And a parity test that pins a list or a count instead of comparing the two sources goes stale in
  silence.** The `cargo xtask wasm-check` that CI actually runs carried **9 of 17** rules — every
  `lodestone-shell` rule and all five clock rules absent, and `lodestone-shell` missing from its wasm compile
  list — because its parity test hard-coded nine labels and stayed green as the script grew. **Parity gates
  must parse both sources and diff them**, never assert against a transcribed snapshot.

  The reusable control: **plant a violating line in each named crate and require the rule to fail and name
  the file**, then restore by `cp` from an md5-checked backup. Done for all 22 rules (the five alternations
  split one-per-hazard), 22/22 fired in both implementations, and the run now prints
  `rules that actually ran: N/22` with a verdict on the count. No real violation was hiding — the guards were
  decorative, not concealing.
- **Do not build a control out of a shell pipeline here. Count with a program that reads the file.** A
  `diff | grep -c '^<'` control reported **0** where the truth was about **15,000**.

The general rule: the transform that makes output readable is also the transform that can invent a green.
When a conclusion depends on what was *not* in the output, re-run without the filter.

**Five species of vacuous test.** Two cannot be found by reading the test — the source is exemplary and
the flaw is a property of what it was pointed at:

| species | flaw lives in | readable? |
|---|---|---|
| assertion | the assert | yes |
| precondition | the setup (skip instead of fail) | yes |
| **magnitude** | the assert's *predicate*, not its subject | yes, if you ask "how much?" |
| duration | test lifetime vs system counters | **no** |
| **world** | **the input data** | **no** |
| **self-reference** | the gate's *own* text satisfying the pattern it searches for | **no** |

*self-reference* is the newest and the sneakiest, because the gate is a good idea implemented into a
circle. Several gates here defend a wiring line by grepping a file's **literal source text** — the
`chat_opts` detector is the pattern, and it works. One written the same way was placed **inside the file
it greps**, using `include_str!` on its own path: its own assertion string matched, so it passed even with
the real line deleted. **A source-grep gate must live in a different file from the one it greps**, and its
neuter is not optional — delete the line it defends and watch it go red, because that is the only thing
that distinguishes it from a gate matching itself.

*magnitude* is the one that reads as rigorous: **predict the value, do not merely assert the sign of the
change** — compute *both* the correct and the suspected-wrong hypothesis from outside constants and require
the measurement to land on one.

Three corollaries, all paid for (§12.160):

- **A ranking metric that scores an input highly can be selecting the *least* discriminating one, and the
  score will look honest.** Choosing oracle chunks for a light survey by "most partial sky cells" put six
  chunks at exactly **3584 = 14 × 256** — fourteen individually-*uniform* layers, i.e. **open ocean**, which
  a purely vertical propagator gets entirely right. The discriminating criterion was **lateral** variation
  (differs from the `+x`/`+z` neighbour), on which ocean scores 0. So the corollary below applies to your
  *selection procedure*, not only to the input you end up with: **ask what the wrong hypothesis would score
  on your ranking**, because a plausible metric can rank coincident inputs first. Related: derive a
  comparison-count floor from a measurement rather than a guess — vanilla only materialises a `DataLayer`
  where light is non-trivial (**2–7 sky and 0–10 block sections per full chunk out of 26**), so a `> 500_000`
  cell floor was impossible against a real ceiling of 126,976.
- **An input where both hypotheses coincide is not a test.** The XP curve yields 37 at level 15 under *both*
  the inclusive and the exclusive reading of its threshold, so a gate at 15 passes either way; the
  discriminating levels are 16 and 31. Before writing the assertion, evaluate the wrong hypothesis at your
  chosen input and **pick an input where the two answers differ** — otherwise you have measured that the
  code runs.

  **And a whole fixture corpus can share one coincidence.** `join_view_rings` yields ring *offsets*
  `(dx, dz)`, and the join loop passed them to the encoder as *absolute* coordinates — so every streamed
  square was centred on chunk `(0, 0)`. It survived because **every existing join gate spawns the player at
  chunk `(0, 0)`**, the single input where "offset" and "absolute" are the same number. No individual test was
  badly written; the corpus had one blind spot and all of them inherited it. So ask the coincidence question of
  **the fixture set**, not only of the input in front of you: if every gate in a subsystem shares a spawn
  point, an origin, a seed or an identity value, that shared value is exactly where a whole class of bug lives
  unobserved. (The same shape as the docs-index gate scanning three directories and not the fourth.)

  **The shared thing need not be a value — a corpus can share one *construction path*, and that variant is
  harder to see because no input looks suspicious.** The in-world settings screen had no header/footer bars,
  no hover tooltips and painted the panorama over the paused world, while the identical page reached from the
  main menu was correct. Nothing was a regression and no assertion was wrong: `render::frame_for` stamps four
  canvas facts (`gui_scale`, `panorama_speed`, `list`, `cursor`) and returns `None` for overlay screens by
  design, and the in-world path built its frame **raw at the draw site**, reaching none of them. Every existing
  render gate obtained its frame through `frame_for`, so the whole corpus was blind to any caller that did not.
  Measured on the same page and canvas: `frame_for` → chrome rect `(0, 33, 320, 174)`, raw → `None`.

  Two habits follow. **Ask what constructs the fixture, not just what is in it** — if every gate in a subsystem
  reaches the subject through one factory, a second caller of that factory's *output type* is unguarded by
  construction. And **when one field of a shared stamp is missing at a second call site, audit every field in
  the stamp**: the reported symptom here was the bars, and auditing the other three found the missing tooltips
  and the panorama-over-world — the latter meaning half of an *earlier* fix had silently never applied,
  because routing the frame to the overlay path changed the load op and not the frame's own backdrop
  declaration.

  **Two discriminating requirements can be mutually exclusive, and folding them into one test silently voids
  one of them.** Wiring the item-pickup packet needed *both* an ordering claim (the take must reach the wire
  before the entity's removal) and a **partial** pickup (so the `amount` field is not merely the stack size) —
  but a partial pickup leaves the entity alive by construction, so **there is no removal left to order
  against**. One test for both reported *"the item entity was never removed, so the ordering claim is moot"*.
  Split it: a **full** pickup for ordering, a **partial** one for `amount`. The neuter then proved the split
  was necessary rather than merely tidy — forcing `amount` to the banked count failed the partial arm while
  the ordering arm **stayed green**. So when a gate needs two properties, check whether the input each one
  demands is the same input; if not, that is two gates.
- **Do not predict the plausible round number.** Four gates in one unit failed on first run for this alone —
  "200 blocks" was really 241, "regeneration fills the bar" was 15.5, "regeneration repeats" happens once.
  Re-derive the arithmetic in a separate script rather than reaching for the figure that sounds right; a
  round number is a guess wearing a prediction's clothes, and it fails in the direction that looks like a
  code bug.

  **And a round number chosen as an *input* is worse than one chosen as an expectation, because it can make
  every arm of a gate coincide at once — concealing a real bug rather than merely failing.** Measured on the
  end portal: the animation gate advanced `GameTime` by `200.0`, and that made the per-layer UV shift an
  exact integer **for every layer simultaneously**, so the two sampled phases looked identical. That is
  precisely the symptom of the real defect sitting underneath it — implicit-derivative mip selection had made
  the swirl blind to `GameTime` entirely — and the round delta reproduced that symptom for an innocent
  reason. Re-deriving the delta as `0.37` separated the two and exposed the shader bug. So when a fixture
  supplies a *step*, a *delta*, a *seed* or an *interval*, ask what it divides evenly into; the whole point of
  picking an input is to make the arms differ, and a round one is the input most likely to make them agree.

And the reciprocal of *world*: **a new subsystem silently breaks every test that assumed its absence.** A
drowning-cadence gate began failing when hunger landed, because a hurt well-fed player now regenerates and
the next health packet is a *heal*. When you add a system, grep for gates whose premise was "this does not
exist yet". *world* is the one you cannot read: both instances were verified against
the one scene, or the one `ServerProtocol`, **that structurally cannot exercise the change**. So the audit
question is not "is this test integration-level?" but **"which implementation does this test's transport
actually resolve to, and is it the one production uses?"** — a test double *complete enough to pass* is the
most dangerous kind. Also ask: **does any server-side counter accumulate past this gate's lifetime?** and
**does the input actually contain the structure the code under test exists to handle?** (§12.43)

**And the corpus can state its own blind spot in plain words, where it reads as an explanation rather than
as a defect report.** Measured: every drop/pickup gate in `serve_play.rs` passes `&NoEntities` — a
permanently-empty `EntitySource` — and asserts against `mobs.with(|sim| sim.snapshots())`, never against
what a client receives. Its own `FakeProtocol::encode_add_entity` carries a doc comment saying those
encoders are *"inert for every other test in this file … never called"*. That sentence is **true**, it was
written deliberately, and it is exactly the finding: an encoder no gate ever reaches is an unguarded wire,
and the browser's singleplayer path really did construct the server with `NoEntities`, so a mined block
rolled its loot, spawned a real item, and **never sent `ADD_ENTITY`**. Nothing was red anywhere.

So when a fixture's own docs explain that some path is not exercised, **read it as scope, not as
reassurance** — and grep the corpus for that phrasing (`never called`, `inert`, `unused in these tests`) as
its own audit. **The same holds for production comments admitting an unported feature**: the boss bar drew
a flat tinted rect for as long as it existed, above a comment reading *"Not ported: vanilla's
per-colour/overlay sprite art"* — accurate, deliberate, and tracked by nothing. Add `not ported`,
`approximation`, `placeholder` and `for now` to that grep. Note the gap there began a layer *up*: the model
had already folded the bar's colour into an approximate RGB tint and dropped the overlay style, so no
amount of care at the draw site could have recovered them — **when a draw looks unfixable, check whether
the model threw the information away first.** The habit that catches this class: for any packet you believe production sends, ask **which
gate asserts it reached the wire**, not which gate asserts the state that should have produced it.

**The worst instance so far shipped a totally silent hang, and every gate in the corpus used a fresh world.**
The owner's saved worlds served **0 chunks in 240 s — no error, no disconnect, no panic**. The cause was a
**self-deadlock**: `run_tick_loop` holds the scheduled-tick queue mutex across its whole tick section, that
section calls `world.column`, and for a *saved* chunk that reaches `ScheduledTickHandle::restore` → back into
`with` → `Mutex::lock`. **`std::sync::Mutex` is not reentrant**, so the tick thread parked forever and the
join wedged before its first chunk batch.

Two things to carry:

- **A lock held across a call into a subsystem that can call back into you is a self-deadlock waiting for the
  right input.** Grep what the guarded section calls, transitively, before widening a critical section. The
  fix here was to stage the loaded ticks behind a second mutex and merge them inside `with`, with a fixed
  lock order.
- **The discriminating input was "a saved chunk carrying a pending tick"** — `load` returns early when the
  chunk is not on disk and `restore` returns early when it has no ticks, so *only* saved content with pending
  ticks reaches the re-entry. Every singleplayer gate created a **fresh** world, so the whole corpus was blind
  by construction; a fresh persistent directory was **not** enough either (18 chunks arrived fine). Note the
  controls that localised it: in-memory 23 chunks, fresh-on-disk 18 chunks, the owner's save 0.

And the diagnostic worth remembering: a wedged process yields nothing to a log or a test, but **`sample` on
the hung pid printed the whole re-entrant stack**. Reach for it before theorising about a hang.

**The sharpest form of that reciprocal: a gate that uses an *unimplemented* thing as its negative stand-in
goes vacuous the moment someone implements it.** Measured — modelling `minecraft:custom_data` and
`minecraft:repair_cost` silently voided **six** gates at once: three used `custom_data` as their "an
unmodeled component halts the decode" stand-in, and three more had a **captured server fixture** whose
bytes happened to carry `repair_cost`. All six went red, which is the *only* reason it was noticed; had
they been written to skip rather than fail, the whole set would have gone quietly green and stayed that
way. So when a test's premise is "feature X does not exist", that premise has an expiry date nothing
tracks. Two things make it survivable: **name the stand-in once** in a shared constant so implementing
something breaks one line rather than six, **pick one that is genuinely expensive** to implement (so the
expiry is far off), and give it **a control that fails by name** when the stand-in stops standing in.
Note the second half is unreadable from the test source — a captured fixture's bytes are opaque, so
"which of my fixtures contain a thing I might implement next?" is a question only a grep of the capture
can answer.

That last question is the whole of the third *world* instance, and it is worth reading as a template because
the test source is exemplary. `water_seam_convergence.rs` fills **two whole columns** with water, so every
rim cell's neighbour is another full column and the corner-height helper's own `edge_a >= 1.0` arm returned
1.0 before the missing rule was ever reached. Nothing in the corpus had **an isolated column with air beside
it** — the one input the bug needs. A companion lesson from the same fix: both halves (the `hasSameAbove`
short-circuit *and* the averaging helper) were individually implemented and individually correct, and the
defect lived in the **composition**, which had no name and therefore nothing to point a test at. When a bug
turns out to be a seam between two correct functions, **extract the composition as a named symbol** so a gate
has a subject.

**A gate that compares two things you control cannot tell you that a third thing exists.** The docs-index
drift gate scanned three directories and not `docs/plans/`, so six documents were invisible and nothing
failed. Ask of any drift or parity gate: what is *in scope*, and how would I find out if something fell
outside? This is `decode(encode(x)) == x` wearing different clothes.

**A test that performs an OS-level side effect is a defect with a user-visible symptom that no health
check here can see — the suite passes.** A unit test was opening `login.live.com` in the owner's browser
on every `cargo test -p lodestone-shell`. **Fork on `#[cfg(test)]` rather than early-returning on
`cfg!(test)`**, so the interception is *assertable* instead of a silent skip, and **grep for the effect,
not the feature** (`Command::new("open")` / `xdg-open` / `cmd /C start`) — which found a second latent
instance. Fixtures should use RFC 2606 `.invalid` hostnames as a second layer. (§12.44)

**And the effect class is wider than launching a UI: a test can *destroy* state that lives outside the
repo, and that one has no visible symptom at all.** Caught mid-change rather than after: wiring
account resolution into the shell's join meant an **existing** `net.rs` unit test that called the
connect entry point would have opened the keychain, POSTed to Microsoft and **rotated the owner's
refresh token** — invalidating the credential his real client holds, while the suite reported green
and the damage sat in a keychain no `git status` covers. The browser case at least *announced* itself.

Two things generalise. **When you thread a resolver into a function tests already call, enumerate what
it touches outside the process before you wire it** — the new call site is not where the hazard is
introduced, the *pre-existing* caller is, and it will not appear in your diff. And a credential store,
a token endpoint that rotates on use, a shared cache and the real filesystem are all this class, so
the grep is for the **effect** — keychain access, a token refresh, a `.cache` write — not for the
feature that happens to use it.

---

## Rendering constraints

- **The model shader is at wgpu's 4-bind-group floor.** Its default `max_bind_groups` is 4 and the
  shader already spends all four (camera / atlas / palette / anim). A 5-group shader compiles and
  validates on an M5 (which reports 8) and **fails on any 4-group adapter** — a startup crash for other
  people and never for us. Fog was folded into the group-0 camera uniform for this reason. **Check the
  limit, not the adapter.**
- **Depth is `[0,1]` DirectX-style, not vanilla's reversed-Z.** Every ported depth comparison and bias
  flips sign: vanilla's `GREATER_THAN_OR_EQUAL` is our `LessEqual`, and a positive vanilla depth bias is
  negative here.
- **`Surface::get_default_config` is correct natively and wrong in a browser, and the difference is
  structural rather than a preference.** It takes `get_capabilities().formats[0]`; native `wgpu-core`
  sorts sRGB formats first, so that is already `Bgra8UnormSrgb`, while the WebGPU backend **never lists
  an sRGB format at all** — a browser canvas structurally cannot be configured with one. Measured live:
  `navigator.gpu.getPreferredCanvasFormat()` returns `"bgra8unorm"`. Linear shader output then reaches
  the compositor with no EOTF applied and **the whole image, menus included, comes out dark**. The fix
  is an sRGB *view* over the swapchain (`config.view_formats`, plus an explicit format on every acquired
  frame's `TextureViewDescriptor`), with the target reporting that **view** format, since every pipeline
  is built against it.

  Two things generalise past this one call. **A defaulting helper that reads a platform-supplied
  ordering is a `cfg`-free way to get different behaviour per target** — nothing in the source looks
  conditional, so it will not appear in any wasm hazard census, all four native checks stay green, and
  `wasm-check` passes. And the honest gate here is a **decision-level** one — assert which format the
  chooser picks given a web-shaped capability list and again given a native-shaped one — because you
  cannot predict an exact composited byte through `ALPHA_BLENDING` on this backend anyway (see below).
- **The GUI winding invariant is negative, not positive.** `sign(det(gui_ortho * gui_item_pose))` must
  **equal** `sign(det(Camera::view_projection()))`, and that sign is negative because `glam`'s DirectX RH
  perspective is itself negative. Coding to "positive determinant" ships an inside-out block that still
  looks plausibly isometric in a screenshot. Derive the front-facing sign from a real camera; do not
  assert a polarity.
- **Vanilla is not colour-managed.** Tint *and* shade multiply in **gamma** space
  (`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`). Doing it in linear pulls every shade factor
  toward 1.0 and washes the image out.
- **You cannot predict an exact composited byte through `ALPHA_BLENDING` on this backend.** Measured
  while gating banner pattern layers (#174): on Metal, with an `Rgba8UnormSrgb` target, the *effective*
  blend alpha is a real, repeatable, **non-trivial** function of the raw fragment alpha byte — not the
  identity, not `linear_to_srgb(a)`, and not any single power law. An exact-byte prediction from the
  textbook blend formula therefore cannot be made to hold. This does **not** license a direction-only
  assertion, which is the *magnitude* species above. **Predict exactly what you can, bracket the rest,
  and make at least one assertion that fails under the wrong pipeline.** The shape that worked:

  | alpha | assertion |
  |---|---|
  | full | submission order alone decides the winner — byte-identical, no tolerance |
  | low + high | composite sits >40/255 from the wrong-pipeline hypothesis at both ends |
  | across | movement is monotonic between them |
  | mid | differs from **both** anchors by >15/255 |

  The last row is load-bearing, and only a control could have shown it: the ordinary `EntityPipeline`
  passed every assertion **except** the mid-alpha anchor distance, measuring `d_mid_to_src == 0` exactly.
  The monotonic inequalities alone are satisfied by a hard discard-then-overwrite, so a gate without the
  mid-anchor check proves nothing about blending.
- **Shaders live in `.wgsl` files. Never inline one in Rust again** —
  `crates/lodestone-render/src/shaders/` and `crates/lodestone-shell/src/shaders/`, via `include_str!`,
  still compile-time. See [`docs/shaders.md`](./docs/shaders.md). "Just for a quick test" is not an
  exception: `no_wgsl_is_inlined_in_rust_sources` fails on any `@vertex`/`@fragment` under a crate's
  `src/`. A `"` in a `.wgsl` comment is now legal and inert. Note **`cargo check` has never compiled a
  shader at any feature setting**; `cargo test --workspace` runs all 22 through naga in ~0.02s. (§12.45)

## Live-server hazards

- **Offline mode derives the account UUID from the username**, ignoring the UUID the client sends.
  Every test sharing a name shares one persisted player file — and a **dead player is held on the
  death screen, which sends no chunks**: a silent, total chunk blackout while join, keep-alives and
  entity movement all continue perfectly. Use `lodestone-testsupport`'s `unique_username`.
- **Vanilla's RCON client performs exactly one `read()` per request** and closes the socket unless
  `pktsize == read - 4`. **Write the entire frame in one call.**
- **A freshly summoned entity is not selector-visible until the next server tick.** Poll; never
  assert immediately. `Invulnerable:1b` also makes an entity un-targetable — use `NoAI:1b` for a
  stationary lure, **but `NoAI:1b` halts gravity too, not just AI.** Measured while building a
  fall-damage oracle: a `NoAI` subject does not fall at all, so it is the wrong lure for anything
  involving motion, and a test that used it would read "no fall damage" as a code defect. Use it for
  a stationary *target*, never for a subject you intend to drop.
- **`minecraft:generic` is itself `bypasses_armor`-tagged**, so it is the wrong damage type for testing
  armour reduction — it reduces nothing by design. Caught mid-oracle when a fully-armoured subject took
  full damage and the armour maths looked broken. `minecraft:mob_attack` is a reducible type.
- **`tick step N` does not advance entity physics; only `tick sprint N` does** — and a
  `tick sprint 1` used for registration silently consumes a tick.

## Data sources, in order

1. **Mojang's own generator** (`packets.json`, `registries.json`, `blocks.json`) — authoritative.

   **But `registries.json` is authoritative about registry *contents*, not about which registries are
   *sent to the client*, and issue #275's body makes exactly that mistake.** It names `registries.json`
   as the verification source for the Configuration-phase `registry_data` set — and that file **omits
   `dimension_type` and `world_clock`**, so following it literally builds a set missing the registry the
   client needs most. The real list is **`RegistryDataLoader.SYNCHRONIZED_REGISTRIES` (29 entries)** in
   the jar. Two different questions, one file, and the wrong answer looks complete.

   The reason this survives review is worth keeping too: **our own client is deliberately tolerant of a
   short registry set**, so a wrong list produces no error here — only against a real vanilla client. An
   authoritative source answering a *neighbouring* question is harder to catch than a stale one, because
   nothing about it looks out of date.
2. **Decompiled source** under `.cache/mc/26.2/{src,client-src}` — reference for behaviour only,
   never transliterated. 26.2 ships de-obfuscated, so names are real.
3. **minecraft-data** — bootstrap and cross-check for **1.8–1.21.11 only**; it has no 26.x data, and
   was measured **92.29% covered and stale** for 26.2 collision shapes.

**Prefer interrogating the real jar over any community dataset.** `blocks.json` has no collision geometry
and no `destroySpeed`. Per-block-state tables come from booting the real server headlessly
(`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();`) and walking `Block.BLOCK_STATE_REGISTRY` —
see `crates/lodestone-data/tests/{collision_shapes,hardness}.rs` for the generate-or-assert +
`LODESTONE_REGEN=1` pattern, and the dump programs for the crate you are working in. Both are
`#[ignore]`d, so `just regen-collision` / `just regen-hardness` is how you run them.

**`oracle-java/` is NOT at the repo root, and this document said otherwise until it cost an agent time.**
It is **per-crate** — `crates/lodestone-{data,render,physics,canonical}/oracle-java/` — so a root-relative
path in a brief or a doc resolves to nothing. Find the one belonging to your crate.

**There is no Java runtime on the *host*, and that does not block a JVM oracle — the oracles run in a
container.** `java -version` reports *"Unable to locate a Java Runtime"*, and the first version of this
section wrongly concluded from that that *"every instruction to verify against a JVM oracle is currently
unexecutable"*. That was wrong, it was propagated into four agent briefs, and at least one of them
correctly refused it. **Read [`docs/oracle-runtimes.md`](./docs/oracle-runtimes.md) before repeating the
claim:** every oracle path runs its real vanilla server or JVM oracle under **Apple `container`**, not
Docker — *"Docker is gone from every one of these paths — there is no `LODESTONE_ORACLE_RUNTIME` switch and
no fallback"*. So the JVM comes from the image and the host needs no `java`. `scripts/worldgen-oracle/run.sh`
(temurin-25) and `scripts/live-oracles/*.sh` are the entry points; `container list` tells you what is up.

**There is also a vanilla-authored oracle world already on disk**, which several units need no new fixture
to start against: `.cache/mc/survival/world`, seed **-195764831**, ~89 region files — 14,499 overworld
chunks carrying full `structures.starts`/`References` NBT (mineshaft 29, ocean_ruin 14, trial_chambers 8,
ruined_portal 7, shipwreck 7, and one each of monument/village/trail_ruins/buried_treasure) plus **2,444
Nether chunks** (wastes 487, crimson 327, soul_sand 255, basalt 172, and **warped_forest 0** — a
world-species limit to assert, not to discover later). `chunk_nbt_vanilla_oracle.rs` is the precedent for
reading it. The End has no block oracle anywhere, so End work gates on record definitions plus arithmetic
until someone generates one.

**That world's `players/data` is a second, separate oracle in the same tree, and it is easy to miss.** 247
vanilla-written player files, **12 with non-zero XP**, each carrying a real `(XpTotal, XpLevel, XpP)` triple —
vanilla's own answer to "what level and bar does this total give". It settled the XP curve's carry
re-expression where no *total* could: `total_points_for_level` is identical under both the inclusive and the
exclusive reading of every level seam (315 at 15, 352 at 16, 1395 at 30, 1507 at 31), so the corollary about
picking a discriminating input **had no discriminating input to pick** — only real triples separate the
hypotheses, and total 15 (level 1, bar 8/9, where a bare `progress - 1.0` gives level 2) is the one that does.
Read with a `gzip`+`struct` parser sharing no code with the repo, then **committed as a table with
provenance**, so the gate does not depend on `.cache` being present. It also confirmed NBT types the record
alone had not stated: `XpLevel` Int, `XpP` Float, `XpTotal` Int, plus an unmodelled `XpSeed` Int. Expect other
player-scoped facts (hunger, air, inventory shape, ender chest) to be answerable the same way.

**26.2 no longer stores the world seed in `level.dat`** — it is in
`world/data/minecraft/world_gen_settings.dat`. Reading `level.dat` and finding no seed is not evidence the
world lacks one.

Two consequences of all this, and the second is the one that matters:

- The **committed dumps and the on-disk oracle world are outside sources you can use right now**, with no
  container start at all. A generate-or-assert gate against a committed dump is as good as it ever was.
- **Verify the runtime before promising it, and name your outside source either way.** `container list`
  costs nothing; asserting availability from memory has now been wrong in both directions in one day. When
  a container genuinely is not available, that is **never** licence to compare our output against our own —
  that is the closed loop the whole evidence section exists to forbid. The alternatives that keep the
  expected-value-from-outside rule intact: the decompiled 26.2 source read as a *record definition* and
  hand-expanded; the on-disk vanilla world above; or a **cross-arm invariant** whose expectation comes from
  geometry or arithmetic rather than from either implementation. Measured while wiring server light
  (§12.117): a seam survey comparing isolated against exact 3×3 computation is a legitimate outside
  expectation, because the two arms are independent constructions of the same physical rule.

**Never hand-count an entity metadata index. Run `EntityDataIndexOracle.java`.** It dumps every
`EntityDataAccessor` sorted by index, so collisions land on adjacent lines; its first run found **two
shipped bugs**, and **every sheep in the game was rendering its default colour** while the decoder
reported a clean parse. Indices are reused across classes and **the guard you need depends on which
classes collide**: index 8 is `LivingEntity.DATA_LIVING_ENTITY_FLAGS` **and** `AbstractArrow.ID_FLAGS` —
living vs **non**-living, so `entity_census::is_living` is right; index 15 is `Mob`'s flags **and**
`ArmorStand.DATA_CLIENT_FLAGS`, and an armour stand *is* a `LivingEntity`, so `is_living` would report
**every decorative armour stand with arms as an aggressive mob** — that one needs `entity_census::is_mob`.
Check the dump, then pick the census column that separates the *actual* claimants; **assuming the previous
collision's guard generalises is how the armour-stand bug would have shipped.** (§12.47)

**Third instance, and this time no existing census column worked.** Index 8 has **five** `INT` claimants —
the experience orb's value, `PrimedTnt.DATA_FUSE_ID`, `FishingHook.DATA_HOOKED_ENTITY`,
`VehicleEntity.DATA_ID_HURT`, and a display entity's interpolation delay — and **neither `is_living` nor
`is_mob` separates them**, because an orb is neither and neither is a primed TNT. It needed a *new* class
(`MetadataClass::ExperienceOrb`); ungated, **a lit TNT draws an orb sprite**, its fuse read as an XP value. So
the rule is stronger than "pick the right column": **when the claimants share no census axis, adding a class is
the fix, and the guard's premise must itself be asserted against the committed jar dump** rather than assumed
from the two known cases. Also worth carrying: an entity whose renderer is a **sprite** rather than a cuboid
rig must stay **absent** from the model corpus — a `model_for_type` entry would hand the mob pass a rig for an
entity that has none, so those "no model, no texture" assertions are load-bearing and must not be inverted
when the entity starts drawing.

**The same collision exists in NBT, keyed by field *name* rather than by index, and it silently rewrites
the world.** `Age` is a `Short` on `minecraft:item` (ticks alive) and an **`Int`** on a mob (breeding age,
negative for a baby); `Health` is a `Float` on a mob and a constant `Short` on an item. A round-trip that
decides which fields to carry through by consulting a **static name list** therefore excludes a field it
failed to decode — so a loaded sheep lost its negative `Age` and **every baby in the world silently became
an adult**, with a clean parse and no error. The rule: **exclude a field only if the decode actually
consumed it**, never because its name appears in a modelled-field table. Any name-keyed schema shared
across entity or block-entity types has this shape; the type is part of the key. (§12.158)

## Documentation

Keep [`docs/`](./docs/README.md) current: one doc per subsystem, `kebab-case`, named after the feature
rather than the file. Each should cover what it is, how it works, **how to change it and the gotchas**,
configuration, and dependencies.

**`docs/README.md` is now generated — do not hand-edit it.** `cargo xtask docs-index` produces it from
every doc's own H1 plus its `## What it is` summary paragraph, and `cargo test -p xtask` fails loudly if
the committed file drifts (`LODESTONE_REGEN=1` to refresh). To change how your doc appears, **edit your
doc's H1 and summary paragraph**, then regenerate. A doc with no usable summary makes the generator fail
loudly naming the file. Do not create a fourth scanned directory lazily, and **if you regenerate, commit
the generator change and the index together and immediately** — a regenerated index left in the working
tree was swept into an unrelated agent's pathspec commit within minutes. (§12.48)

Write down *why*, and especially write down what was measured. **The most valuable thing in this repo is
not the code — it is the record of beliefs that were confidently held and turned out to be false.** That
record is [`DESIGN.md`](./DESIGN.md) §12: the rule goes here, the measurement goes there.

### Cite symbols, never line numbers, and stop cross-referencing issues

**Never write a line number for code in this repo.** Not `server.rs:5189`, not `entities.rs:1398`, not
in a doc, a comment, a commit message or an issue. **Name the symbol path instead** —
`lodestone_server::commands::ServerCommands::run`, `Density::is_xz_pure`,
`ViewTracker::build_batch`. A symbol survives every edit above it; a line number is wrong the next time
anyone inserts a function, and a *plausible* wrong line number is worse than none because it sends the
reader somewhere real. Maintaining these has been pure cost: the stale claims that repeatedly waste time
here are almost all drifted citations rather than wrong ideas, and a decomposition invalidates thousands
at once.

**Do not cross-reference issue numbers from code comments or docs.** `(issue #415)`, `#520's own doc`,
`the trap #275 names` — all of it goes. A comment should say what the code does and why, in terms a
reader of *that file* can check. Issue numbers are tracker bookkeeping with a short half-life: they get
closed, superseded, renumbered, and split, and none of that reaches the comment. Put the reasoning in the
comment and let the tracker keep its own history. **Commit messages and issue comments may name an issue**
— that is where cross-references belong.

Two things this does **not** cover:

- **Vanilla record definitions get cited in `docs/` only — never in a `.rs` file.** The owner's
  decision, on legal advice grounds: prose under `docs/` may name `FireBlock.tick`,
  `LivingEntityRenderer`'s `yRot`, `Mth.clampedLerp` and the rest, because the decompile under
  `.cache/mc/26.2/` is a pinned external source and citing it is how the next reader re-verifies a
  port. **Source comments must not.** Drop the `:NNN` in docs too; a class-and-method name is just as
  findable and does not rot when the cache is re-extracted.

  This costs something real and the cost is worth naming, because the mitigation is what keeps the
  rule survivable: the in-code citation is what let a mislabelled knockback derivation be caught by
  reading the method it named. So when you remove one, **do not just delete it** — say in the comment
  *what rule the code implements*, in terms a reader of that file can check, and point at the `docs/`
  page that carries the citation. "Two knockback impulses, the flat one unconditional and the sprint
  bonus gated — see `docs/combat.md`" is as verifiable as a symbol and names nothing.
- **Measurements keep their numbers.** §12 exists for figures — allocation counts, instruction counts,
  hit rates, byte totals, md5s. Those are evidence, not citations, and they are the point of the record.

Prose is still wanted; it is the *pointers* that were the tax. When you touch a file, delete the line
numbers and issue references you pass and leave the reasoning.
