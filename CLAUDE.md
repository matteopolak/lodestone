# Lodestone — working rules

A from-scratch Minecraft client in Rust, plus an integrated server.

**Four client protocol families exist**, each a workspace member under `crates/protocol/` behind a
`lodestone-registry` feature: `v47` (1.8.9), `v340` (1.12.2), `v735`, `v770` (protocol 776 / MC
26.2). This file used to say "v770 only" and that was wrong for long enough to mislead — `v340`
alone is ~18k lines with a canonical id:meta bridge and live-oracle tests.

Three things that line hid, all load-bearing:

- **No family is enabled by default** in `lodestone-registry`; the shell's default `live` feature
  turns on `v770` and nothing else. So a legacy family is invisible to every command in *Build and
  test* below unless you name its feature.
- **Only `v770` implements `ServerProtocol`**, so 26.2 is the only version we can *host*. Joining
  and hosting are different sets, and `lodestone-registry` keeps `Family` and `ServerFamily` as two
  tables for exactly that reason — read its doc before assuming a family you can join is a family
  you can serve.
- **`v735` speaks protocol 754** (1.16.5). The folder name is not the protocol number, unlike the
  other three. Never derive the protocol from the folder — ask `VersionAdapter::supports`.

New gameplay work targets `v770` unless an issue says otherwise; that is a default, not a scope.

This file is the short, durable set of rules. The long-form record lives in
[`DESIGN.md`](./DESIGN.md) (architecture, plus a §12 validation log of ~20 beliefs that were
confidently held and empirically false). What is open lives in
[GitHub issues](https://github.com/matteopolak/lodestone/issues), with the tier definitions and
per-item traps in [`docs/backlog.md`](./docs/backlog.md). [`HANDOFF.md`](./HANDOFF.md) is the
workflow for an agent *orchestrating* this repo rather than writing code in it.
Per-subsystem detail goes in [`docs/`](./docs/README.md).

---

## Build and test

```bash
cargo check --workspace --all-targets     # the health check
cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench
cargo check -p lodestone-shell --no-default-features   # the version seam still holds
cargo run --release                       # launch the game
```

- **`cargo build` is NOT a health check.** It skips test targets, so a crate whose lib compiles and
  whose lib-test does not reports green. Always `--all-targets`.
- **`--all-targets` alone misses non-default features.** `live_inventory.rs` sat broken behind the
  `live-inventory` feature for a whole session — invisible to the first command, caught immediately
  by the second. The `--exclude` is not a workaround: `lodestone-allocbench` has a deliberate
  `compile_error!` when more than one allocator feature is on, because each installs its own
  `#[global_allocator]`, so plain `--all-features` **structurally cannot pass** and chasing it is
  wasted time. With that one crate excluded, the whole workspace is clean under `--all-features`.
- **No `cargo check` sees a doctest, at any feature setting.** `check --all-targets` does not compile
  them, so a doc example that no longer builds is invisible to every check in this list. The
  `lodestone-data` extraction (#361) passed all three checks green and then failed
  `cargo test --workspace` on a single doctest still importing `lodestone_v770::path_types` — 338
  test binaries clean, one stale `use` line in a `///` block. Prose that *mentions* the old crate is
  usually correct ("lives here rather than in `lodestone-v770`"); it is the fenced code that rots.
  **After any crate rename or module move, grep the moved code for the old crate path and run
  `cargo test` — not just `check`.**
- **`cargo test -p <crate>` is not one either — it fail-fasts.** It aborts at the *first* failing
  test binary, so everything alphabetically later is never run and never reported. This has misled
  twice: a stale `block_updates` failure hid the new `hardness` gate entirely, and what looked like
  "a red test" in `lodestone-v770` was really **three red binaries and 14 failing tests**, masked
  because `serverbound_change_game_mode` sorts first. **Use `--no-fail-fast` when assessing crate
  health.**
- **A targeted `--test <binary>` run is a *narrower* filter than `-p` fail-fast, and it hides the
  same class of thing.** Adding a `ClientEvent` variant changes the directive **sequence**, not just
  the type, and every choreography test that asserts an exact `vec![Directive…]` is a silent caller
  of it. `ClientEvent::BiomeVisuals` (#96) was tested with
  `cargo test -p lodestone-v770 --test registry_data`, which passes, and broke
  `join_flow::full_login_sequence_produces_expected_directives`, which was never run — `main` was red
  for one commit. **No `cargo check` can see this**: the break is a runtime `assert_eq!`, so all
  three required checks stayed green, *including at the commit's own sha in a clean detached
  worktree*, which is otherwise the strongest verification available here. When you add an event or
  change what an adapter emits, grep for the **packet id**, not for the event, and run the crate with
  `--no-fail-fast`.
- **The binary is `lodestone`, not `lodestone-shell`** — the `[[bin]]` name differs from the crate.
- **`live` is now a default feature, and `cargo run --release` launches the game.** It used to need
  `--features live`, and forgetting it failed *silently*: the client still started, still rendered,
  and reported a plausible `chunks=169` while whispering `no version family compiled in for protocol
  776` into the log. That trap is deleted rather than documented — but the flag still exists, so
  `--no-default-features` is the way to reproduce the version-free build.
- **`cargo check -p lodestone-shell --no-default-features` is now a required health check.** With
  `live` on by default, an ordinary build no longer proves the shell compiles with **no** version
  family — which is the entire point of the version seam. This is the only thing stopping a
  hardcoded `v770` dependency creeping into shell code, and its failure mode is architectural
  rather than a broken test, so nothing else will catch it.
- `default-members` makes a bare `cargo run`/`build`/`test` target `lodestone-shell` only. Every
  command above says `--workspace` explicitly for that reason; a health check that loses the flag
  silently narrows to one crate.
- Live and GPU gates are `#[ignore]`d. Run them explicitly: `-- --ignored --nocapture`.
- A test total gathered while another agent is mid-edit is a **sample, not a measurement**. The
  invariant is *zero failures and zero non-compiling targets*, never the absolute count.

Oracles (not part of repo state — recreate them):

```bash
./scripts/live-oracles/creative.sh   # :25570 game, :25571 RCON — flat/creative/peaceful
./scripts/live-oracles/terrain.sh    # :25580 — normal terrain, for light gates
./scripts/live-oracles/survival.sh   # survival, normal terrain
```

## Repo hazards

- **Single shared checkout, no per-agent worktrees.** Multiple agents edit concurrently.
  **Never `git add -A`. Never `git reset --hard`, `git checkout .`, `git stash`, or `git clean`
  (in any form, including `-n`-then-`-f`).** A blanket stage has clobbered in-flight work three
  times and destroyed a `lib.rs` edit once.
- **Never rewrite a shared file wholesale — edit the lines you mean.** This is a *fourth* way to
  clobber, and no git command is involved, so none of the rules above catch it: writing a full new
  copy of a file silently discards every concurrent edit in it, and the loser finds out only when
  their own change stops existing. An agent overwrote `sim.rs` this way and destroyed three edits
  another agent had already made there; that agent recovered by re-routing its work through
  `resources.rs` and `app.rs`, but nothing warned either of them. `sim.rs`, `app.rs`, `gpu.rs` and
  `docs/README.md` are the usual victims because everyone needs a line in them. Prefer a targeted
  edit over a rewrite, and **re-read a shared file immediately before writing to it** — not at the
  start of your task, which may be an hour of other agents' commits ago.
- **Never run `cargo fmt` (or `rustfmt`) in this checkout.** It rewrites files you do not own, and
  the damage is not the reformatting — it is that your diff becomes inseparable from everyone
  else's, so the *cleanup* is what destroys work. An agent ran `cargo fmt` on `sim.rs`, then tried
  to strip the reformatting by reversing hunks against `HEAD`; the reversal deleted another agent's
  concurrent `particle_atlas`/`particle_sheet_atlas` additions, because new content added since
  `HEAD` is indistinguishable from "collateral formatting" when you diff against `HEAD`. It was
  caught only by a build error naming a method that had stopped existing, and re-applying the patch
  forward recovered it. Format the lines you wrote, by hand.
- **When a shared file already holds someone else's work, stage your hunks, not the file.**
  `git add -p`, or `git diff -- <file> | …` filtered and applied with `git apply --cached`, then
  read `git diff --cached` to confirm the commit contains no foreign lines. This is the working
  practice that let one agent commit into `gpu.rs`, `gpu/stats.rs`, `resources.rs` and
  `docs/README.md` while three other agents held in-flight edits in all four.
- **A red test in this checkout may be someone else's *deliberate* neuter, and no diff can tell you.**
  Every control in this file works by breaking something on purpose and watching a test fail — so at
  any moment another agent's two-minute neuter window looks exactly like a real regression. It
  happened: one agent reported "two `entity::tests::*projectile*` lib tests are red on committed
  `main`", and they were the exact pair another agent's `arrow_NEUTERED` experiment produced. `main`
  was green throughout.
  **The `git diff HEAD` substitute does not save you here**, which is the part worth internalising,
  because that substitute is otherwise excellent (see the entry below). The neuter lived in
  `lodestone-assets` while the failures surfaced in `lodestone-render`, and — more fundamentally —
  a clean diff and a test run are **two observations at two different moments**. Emptiness at 19:31
  says nothing about the tree at 19:33.
  So: before reporting a red `main`, re-run at the **committed sha in an isolated worktree**, which is
  the only observation that excludes concurrent edits by construction. And when *you* neuter
  something, keep the window as short as possible and restore by `cp` from a scratchpad backup with an
  md5 check — never `git checkout`.
- **The scratchpad directory is shared between agents too, so the md5 check above is load-bearing.**
  The path is per-*session*, and every agent in a session gets the same one — so a
  `scratch/probe.rs` or `msg.txt` is exactly as contended as a file in the checkout, with none of the
  git-level protections and no diff to show you what happened. Observed: an agent wrote two scripts
  by heredoc and **read back different content than it wrote**, and found a `msg.txt` it had never
  created already sitting there. That nearly had it classify its hunks against the shared *index*
  instead of `HEAD`, which is the one mistake that ships another agent's lines.
  **Use uniquely-named files** (include the issue number or a nonce), write them with the file tools
  rather than shell heredocs, and re-read anything you are about to reason from. A `#[path]` harness
  is the common case here: it compiles whatever is on disk at that instant, so a clean run proves
  nothing about the file you thought you wrote. This is the same "two observations at two different
  moments" failure as the entry above, one directory over.
- **Never leave a stale blob in the shared index.** A `docs/README.md` blob sat staged at `7b506a8`
  while `HEAD` had `3432cb3`; committing the index would have **deleted** a newer agent's index
  bullet. Refreshing one path with `git reset -- <path>` sets that index entry back to `HEAD` and
  leaves the working tree untouched, which is the safe cleanup — but the real fix is never staging in
  the first place (see the pathspec-commit entry).
  **This is the most frequently observed hazard in the file: five instances in one session**, every one a
  *reversal of a commit that had just landed*, armed for the next agent's `git commit` to ship under
  their message. Twice on `container.rs` (632 lines, then 268), three times on `gpu.rs` (115, 59 and 290
  deletions). Every affected agent had used `GIT_INDEX_FILE` correctly and none had run a bare
  `git add` — truthfully.

  **The cause is the cleanup step itself, in the wrong order.** `git reset -- <paths>` sets the *shared*
  index entry to whatever `HEAD` is **at that instant**, creating an entry where there was none. Run it
  *before* `git update-ref`, and it pins the pre-commit blob; `update-ref` then moves `HEAD` forward and
  that entry becomes a staged reversal of the commit you just made. A deletion-only staged diff
  (`0` insertions, N deletions) is the signature.

  So the order is not stylistic:

  ```
  TREE=$(GIT_INDEX_FILE=$priv git write-tree)
  NEW=$(git commit-tree "$TREE" -p "$OLD" -F msg)
  git update-ref refs/heads/main "$NEW" "$OLD"   # HEAD moves FIRST
  git reset -- <paths>                           # then refresh, against the NEW HEAD
  ```
- **`git write-tree` against a missing index writes the EMPTY tree, silently — and that commit
  deletes the entire repository.** This is the worst outcome available from the escape hatch and it
  has already reached `refs/heads/main` once, for a few seconds, before its author caught it in
  `git show --stat` and reverted with a compare-and-swap.

  The trigger is mundane: **shell state does not persist between tool calls.** A private-index path
  built with a `$$` nonce in one invocation is an *empty string* in the next, so `GIT_INDEX_FILE=""`
  and `write-tree` has nothing to write. No error, no warning — a valid commit object whose tree
  contains nothing.

  Three defences, and use all three because each catches a different slip:
  1. **One invocation** for `read-tree` → `add` → `write-tree` → `commit-tree` → `update-ref`. Not
     "one per step, carefully ordered" — the variables do not survive.
  2. **A literal nonce**, not `$$` or `$RANDOM`: `idx-fog-7f3a`, chosen by you and typed out.
  3. **Sanity-check the tree before moving the ref.** `git ls-tree -r "$TREE" --name-only | grep -c ""`
     against a plausible floor is one line and it makes this class impossible:
     ```
     n=$(git ls-tree -r "$TREE" --name-only | grep -c "")
     [ "$n" -gt 1000 ] || { echo "ABORT: tree has only $n files"; exit 1; }
     ```
  And always `git show --stat` your own commit afterwards. That is what caught it.

  And still check `git diff --cached` is empty immediately *before* every commit, because another agent
  may have left one: a count, not an eyeball, and a verdict that depends on the count — an unconditional
  `echo "(clean)"` after the check is its own vacuous control, and that mistake was also made here.
- **The index is shared too: never leave work staged.** Hunk-staging (above) stops *you* shipping
  someone else's lines; it does nothing to stop *them* shipping yours. `git add` writes to the one
  index every agent shares, so any other agent's `git commit` in the gap — however narrow — harvests
  whatever you have staged into **their** commit, under their message. This happened to a whole
  26-file change: the `registry_data` ingest for #288 was staged, verified, and then committed by
  another agent as `a19e5e4 feat(shell): chests reach pixels`. Nothing was lost and nothing foreign
  was shipped, but the change set has no commit that describes it, and a reviewer reading `a19e5e4`
  is misled about what it contains. The same gap cost that work three re-stagings, because a
  concurrent broad `git add` also reset the index for `docs/` twice mid-flight, and a
  `git diff --cached` read one command later was already describing a different index.
  **Use the pathspec form: `git commit -m "…" -- <your paths>`. This is the standard here, not a
  fallback.** It commits exactly those paths and **ignores the index entirely**, which is the only
  property that makes it safe.

  Measured in a throwaway worktree, because the whole point is that it needs no cleanup step:

  | | result |
  |---|---|
  | commit created | yes, `HEAD` moved |
  | contents | only the named path |
  | **index afterwards** | **clean — no `git reset` needed** |
  | working tree | untouched |
  | another file's edits | survived on disk, excluded from the commit |

  That third row is the important one. **`git reset -- <paths>` is the source of every stale-index
  incident in this file** — nine in one session — and it only exists to clean up after the private-index
  route. The pathspec form leaves nothing to clean up, so **do not run `git reset` after it.** Adding
  that step back is how the hazard returns.

  Argument order matters: `git commit -m "msg" -- <paths>`. Put `-m` *before* the `--` or git parses
  the message as a pathspec and silently commits nothing — a probe written the wrong way round here
  reported "index clean" from a commit that never happened, which is a vacuous control on top of a
  no-op.
  "Stage, verify and commit in one shell invocation" was tried and is **not sufficient** — a single
  invocation is not an atomic transaction. An agent staged six files, asserted
  `git diff --cached --name-only` matched exactly, and then its plain `git commit` swept in **14
  files** belonging to another agent who had run `git add` in the window between the assert and the
  commit. One of those files was captured **mid-keystroke**, so `main` was briefly red from a commit
  whose author never touched the broken file. Review-then-commit cannot be made race-free while the
  index is shared; the fix is not to look harder but to stop consulting the index at all.
  `git add` "to see the diff" is the most expensive way to look — `git diff -- <paths>` shows the
  same thing and touches nothing.

  **The pathspec form commits *working-tree* content, so a path you name carries whatever is in it.**
  It defeats the index race, not the shared checkout. **That is an accepted cost, not a blocker** —
  the repo owner's call: shipping a few of another agent's lines under your message is far cheaper than
  agents stalling on each other, and it is recoverable by reading the diff. So:

  - **Name only paths in your own assigned cluster.** That is what actually prevents this, and it is
    why ownership is assigned per agent up front.
  - `git diff -- <path>` before naming it, so you *know* what is going in. If a foreign edit is there,
    say so in the commit message rather than abandoning the commit.
  - **Do not block on it.** Waiting for another agent to finish is usually the wrong trade, and
    splitting your change to avoid a shared file is worse — it produces two half-commits neither of
    which reaches pixels.
  - The one case still worth avoiding: a file that is **mid-keystroke** rather than merely modified. If
    it does not compile and you did not break it, wait a beat, do not commit it.

  Only reach for the temp-index route below when you need **partial-file** granularity — committing two
  hunks out of a file whose remaining hunks belong to someone else. That is a real need (it happened
  once here) and it is the only thing the pathspec form cannot express.
- **Never `git pull --rebase`, and never `--autostash`.** The `git stash` ban above is easy to keep
  when you type it; `--autostash` runs one *for* you, on the whole shared tree, silently. An agent ran
  `git pull --rebase --autostash`, the rebase aborted, and it was left with a spurious **staged
  deletion of another agent's brand-new test file** — content intact but the index claiming a removal,
  which the next commit would have shipped. It repaired the index entry by hand. There is also a live
  `stash@{0}: autostash` entry holding a full-tree snapshot, left in place deliberately as someone
  else's safety net: **do not `stash drop` or `stash pop` it.** If you need to move to a newer commit,
  do it in a throwaway `git worktree add --detach`, which touches nothing here.
- **`GIT_INDEX_FILE` + `commit-tree` is the escape hatch, and it has its own trap: a stale tree.**
  When you need partial-file granularity that a pathspec commit cannot express, build the commit in a
  **private** index so the shared one is never touched. But the ref compare-and-swap in
  `git update-ref <new> <old>` protects the *parent*, **not the tree you built**. An agent read a tree,
  two commits landed while it worked, and committing that stale tree onto the fresh parent **reverted
  2,173 lines** of another agent's chest and metadata fixes. It was caught immediately in
  `git show --stat` and repaired, but the lesson is: **read the tree and commit it in one step**, and
  always `git show --stat` your own commit afterwards to confirm it contains only additions you
  intended and no deletions you did not.
- **`git clean` is the worst of the git-level mistakes, because it destroys what nothing can
  recover.** The others discard *modifications* to tracked files, which at least existed in a commit
  once.
  `git clean` deletes **untracked** files — which in this repo means whole new crates, new
  `docs/*.md`, new oracle dumps and new test files, none of which are in any commit or reflog.
  It has already cost real work: an agent ran it while others were mid-flight and destroyed
  `docs/autonomous-navigation.md` outright, plus `crates/plugins/lodestone-autopilot`'s manifest
  and source, leaving only the `LICENSE` behind and the workspace unloadable. The author had to
  rewrite it from nothing. There is **no legitimate use** for it here: build output is already
  gitignored, and "tidying up" a shared checkout is not a thing any single agent has the standing
  to do.
- **Stage explicit *file* paths, never a directory.** `git add docs/` is the same mistake as
  `git add -A`, just narrower — it sweeps up whatever else happens to be in there. This bit me
  personally: `53850ce` swept another agent's then-unfinished `docs/block-break-timing.md` into a
  render commit. Nothing was lost, but the commit contains 169 lines its author never wrote, and a
  reviewer reading that diff would be misled about what the change was. `git add <file>` or
  `git add -p`, always.
- **Read `git diff --cached` before every commit.** Explicit file paths are necessary but not
  sufficient: a *shared* file can already contain someone else's in-flight edit. `0b95b4e` staged
  `docs/README.md` by exact path and still captured another agent's index line pointing at a doc
  that commit did not include — shipping a broken link. Review the staged diff, not just the file
  list.
- **`rtk` is not a transparent proxy. Do not trust it for evidence — use `/usr/bin/grep` and the
  real `cargo`/`git`.** It is a token-saving filter, and its filtering silently destroys exactly the
  output a search exists to produce. Verified here directly, on one file, one pattern:

  | | output for `ambient_occlusion_at` in `mesher.rs` |
  |---|---|
  | `rtk grep -n` | `usize, y: usize, z: usize) -> bool {` |
  | `/usr/bin/grep -n` | `fn ambient_occlusion_at(&self, x: usize, y: usize, z: usize) -> bool {` |

  **It strips the matched pattern and everything before it on the line** — it deletes the one thing
  you searched for, so you cannot tell a real match from a near-miss, and a symbol looks absent when
  it is present. This is the `| head` trap with no visible pipe: rule 2's whole class of "X doesn't
  exist yet" mistakes can now be manufactured by the search tool itself.

  Also observed by agents, each nearly producing a wrong conclusion: `rtk proxy cargo test` reporting
  **exit 0 while its own output said 7 failed**, and rewriting `-p lodestone-render` into a run that
  executed `lodestone-physics`' tests; and `rtk proxy git diff HEAD -- $LONG_VAR` returning **zero
  hunks while the content plainly differed**, which nearly had an agent conclude its work was already
  committed (single literal paths worked). Exit-code preservation *is* fine for `cargo check`
  failures — measured 101 both through `rtk proxy` and through `~/.cargo/bin/cargo` — so the failure
  is not uniform, which is worse than if it were: it is unpredictable per subcommand.

  Practical rule: `rtk` for reading something you already believe, the real binary for anything a
  conclusion rests on. **Re-read every exit code from a captured file with a program, not from a
  pipeline.**
- **This machine is shared with an unrelated project.** Docker holds images and volumes belonging to
  other work (`mht-*`, postgres, valkey, seaweedfs). **Never run `docker system prune`,
  `docker volume prune`, or `docker builder prune`.** Name every target explicitly; note Docker's
  `name=` filter is a *substring* match. Lodestone containers are `lodestone-*`; prefer `--rm`.

---

## The two rules that matter most

### 1. Nothing is done until something on screen changes

The dominant defect class here is the **island**: a subsystem that is individually built,
individually tested, and reaches **zero pixels** because nothing calls it. Nine confirmed instances.
The tree is green, the counters look plausible, and the screen is wrong.

A crate's own test suite is a **closed loop** — it can be entirely green while the crate is dead
code. Only a gate that asserts *coverage inside the subject's screen rect*, plus a negative control
that must fail the same assertion, can see an island.

Ask of every piece of work: **what actually consumes this?** Treat "nothing" as a defect report, not
a status update. Assign work end-to-end, from data through to draw, rather than by crate.

**One specific island factory: `ingest::handles_event`'s routing switch.** A system can be correct,
registered in the right set, in the right order, and unit-tested green — and still never run in
production, because `SharedState::apply` only forwards events the switch lists. A hermetic test that
calls the system directly passes either way, so nothing catches it. This has now hidden working code
**twice in one session** (`EntityDamaged`/`EntityHurtAnimation`, then air supply). When adding an
ingest system, the switch is the first thing to check, not the last.

**Generalise it: every terminal `_ =>` arm in an event router is an island factory, and there are
three.** A `_ => {}` that silently discards is indistinguishable, at the call site, from one that has
nothing left to handle.

| router | carries | missed instance |
|---|---|---|
| `ingest::handles_event` | per-entity ECS state | `EntityDamaged`/`EntityHurtAnimation`, air supply |
| `session::handles_event` | local-player session scalars | — (but see below) |
| `net.rs`'s `forward` | the shell's own `ClientEvent` stream | `BLOCK_EVENT`, so chest lids could never animate |

**`ingest` vs `session` is a real fork and guessing it wrong has cost work twice.** `SharedState::apply`
consults *both*, so an arm added to the wrong one compiles, tests green as a unit, and never runs.
`DimensionTypeChanged` is claimed by `session`, and so is `AbilitiesChanged` — for which both the issue
and the dispatch briefing said `ingest`, where an arm would have produced a fold that never fires.
The rule of thumb that has held: **per-entity state is `ingest`, local-player scalars are `session`**,
and block/world events are neither, travelling the shell stream instead — the chest work needed no
`handles_event` arm at all.

So when a decoded packet reaches no pixels, grep its variant in *every* router before concluding the
decode is wrong, and check the sibling router before adding an arm to the one you thought of first.

**Islands come in both directions.** All of the above are *inbound*. `ClientAction::SetFlying` was the
mirror image: encoded by four protocol adapters with **zero producers** anywhere outside
`crates/protocol/`, so flight was applied locally and the server kicked us with
`multiplayer.disconnect.flying`. Ask what *sends* a serverbound action, not only what consumes a
clientbound one.

### 2. Re-verify before routing around "X doesn't exist yet"

Staleness is the most common defect in the written record — **seven instances in one session**.
Every stale claim was *true and evidenced when written*, which is exactly why it survives review:
nothing about it looks wrong on inspection.

Two specific traps, both of which have already cost real work:

- **Zero hits in the file a stale note names is not evidence a feature is unwired.** A note said the
  shell didn't consume the chat resolver, citing `chat.rs:88`. Grepping `chat.rs` returned nothing —
  correctly, because the consumer is one layer up in `sim.rs`, at ingest. **Grep for the producer
  across the whole tree, not for the consumer in one named file.**
- **Read the record definition, not a summary of the call site.** `HANDOFF.md` transcribed vanilla's
  `DepthStencilState(…, 1.0F, 10.0F)` as "constant 1.0, slope 10.0". The record is
  `(depthTest, writeDepth, depthBiasScaleFactor, depthBiasConstant)` — i.e. slope 1.0, constant
  10.0. Backwards.

Prefer `cargo xtask connectedness` over any hand-derived coverage number; the hand-derived version
has been wrong four times in four different ways.

**But know what it measures, because it is silent rather than wrong outside that scope — and it is
narrower than its name suggests.** Measured twice today, each time by an agent I had pointed at it
wrongly:

- It reports **clientbound decode → event wiring, for `v770` only.** `xtask/src/lib.rs:2846` is a hard
  `if family != "v770" { continue; }`, so v47/v340/v735 are **silently never measured** — and the report's
  own header string claims it takes "denominators from each family", which is false. Do not read a green
  connectedness number as saying anything whatever about a legacy family.
  It does **not** measure serverbound decode. Its `53/69` "serverbound encoded" figure is bare token
  presence in the *client* adapter — no arm, no body, no direction check — which is why it is an *encode*
  number: the client adapter only ever names a serverbound id while building a `Directive::Send`.
- **Serverbound decode does not live in `lodestone-server` at all** —
  `/usr/bin/grep -rn "serverbound::" crates/lodestone-server/src/` returns **zero hits**. It is in
  `crates/protocol/v770/src/server_protocol.rs:880`, as `State::Play if packet_id ==
  play::serverbound::NAME =>` arms. **This entry previously said `lodestone-server` and quoted
  "5/69 → 8/69"; both were wrong** — there are **10 Play arms**, and `docs/roadmap/protocol.md`'s
  "completely zero" is stale too. Two hand-counted figures in two documents, both stale within a day,
  which is the argument for automating the axis rather than re-counting it.
  Note the count alone is not connectedness: a variant that decodes and lands only in `server.rs`'s
  `ServerBound::Ignored => {}` group is stranded exactly as a clientbound packet would be, so the
  serverbound axis is a **two-file join** across crates, not a one-file scan.
- It does not measure **Rust call graphs** either. Pointed at a *crate-internal* island — an
  implemented type nothing in the workspace constructs — it returns **byte-identical output before and
  after the fix**, which reads as "no change" rather than "not applicable". The agent closing
  `projectile.rs`/`item_entity.rs`'s missing tick drivers hit this and correctly reported the identical
  output as meaningless rather than quoting it.

So: right instrument for "is this clientbound packet reaching anything", wrong one for everything else.
For a crate-internal island, grep for constructors tree-wide plus a test that drives the *registry*
rather than the type. For server decode, grep the packet ids.

---

## Evidence standards

**An expected value must originate outside the code under test.** `decode(encode(x)) == x` is
satisfied by two symmetric misunderstandings — hermetic chunk fixtures generated with our own
encoder passed throughout, then a live gate produced 49 × "unexpected end of input". Use captured
server bytes, a JVM oracle, or a hand-decoded spec example. Note that a self-authored JVM oracle
validates *the behaviour you chose to model*, so agreement across ports sharing an author is weak
evidence.

**Assertions of an absence need a control proving the detector works.** "No corrective teleport",
"no trailing bytes", "zero unresolved" are only as good as the evidence the mechanism *would* have
fired. Run the control and observe it fail; do not describe what it would do.

**A control's premise can be false before the feature under test ever existed.** This is subtler
than a wrong assertion and it fails in the *safe*-looking direction: the control fires, so the gate
looks rigorous, and what it actually measures is unrelated. Two instances while wiring the sky:

- A control asserted that a sky-less frame "clears uniformly to `SKY_COLOR`". It failed at 3.5%. The
  offenders were at `x221..255 y180..255` in dark browns — the **first-person bare arm**, which the
  hand pass draws whenever `third_person_body_drawn` is false, i.e. always, in first person, with
  nothing installed. The premise had been false since long before the sky existed.
- A HUD gate's rect hardcoded the *with-hotbar* `cluster_top`. `sprite_vitals` stacks upward from a
  **moving** anchor (pulled up only `if frame.hotbar`, again only `if frame.xp`), so the gate
  measured ~20 logical pixels above a row that was drawing perfectly and reported 0 px — a dead
  wiring chain that was not dead.

So: before believing a control, ask **what else already paints here**, and derive layout from the
same expression the draw uses rather than restating a constant. And per *measure by location, never
by frame average* below — both were diagnosed in one step by printing a **bounding box** instead of
a percentage. A gate that reports only a fraction cannot tell a uniform-but-wrong frame from a
localised blob; make failure output say *where*.

**A shell pipeline will destroy the evidence you are about to reason from.** Two instances in one
session, both of which produced a confident wrong conclusion:

- **`| head` read as absence.** `grep -rn -A4 0.085 …/world/entity/ | head -24` was flooded by
  `DropChances.java` and showed no hit in `Player.java`, so the swim-descent constants were declared
  unverifiable and an agent was told to distrust them. They are real, at `Player.java:1408`. A
  truncated search is not a negative result — `grep -c`, or narrow the path, before concluding a
  thing does not exist.
- **`| grep | tail` swallowed a non-zero exit.** `cargo test --workspace | grep … | tail -30`
  reported "exit code 0" because that is `tail`'s status, while cargo's own last line was
  `error: 1 target failed:` — and the grep pattern then cut the target name off. This came within
  one command of a commit on a red tree. **Let cargo write its own output to a file and check its
  real exit status**; filter the file afterwards.

- **zsh does not word-split an unquoted `$var`, so a path list in a variable is *one* argument.**
  An audit built as `P="a.rs b.rs …"; git diff --numstat -- $P` printed **nothing** and its companion
  `git diff -- $P | grep -E "<foreign markers>"` printed **none** — both correct answers about an
  empty diff, because git was handed a single nonexistent path with spaces in it. The check whose
  entire job was "prove this commit contains no other agent's lines" returned a green by measuring
  nothing, one command before the commit. Caught only because the empty `numstat` was *also*
  surprising. **Write the paths out, or `set -- a b c` and use `"$@"`** — and treat an audit that
  prints nothing as a failure to run, never as a pass.

The general rule: the transform that makes output readable is also the transform that can invent a
green. When a conclusion depends on what was *not* in the output, re-run without the filter.

**And `rtk` rewrites pipelines, so this reaches controls that have nothing to do with cargo.** A
zero-deletion control on a regenerated data table ran `diff | grep -c '^<'` and reported **0**. The
true figure was about **15,000**; it surfaced only as 20,251 deletions in `git diff --cached`, and
the control had to be redone as a semantic parse (43 statics carrying over with all 30,360 literals
byte-identical). The generator emits one line per tick where the committed file is reflowed to four,
so a line-oriented control was the wrong instrument even before the pipeline ate the count. **Do not
build a control out of a shell pipeline here.** Count with a program that reads the file.

**Five species of vacuous test.** Two cannot be found by reading the test — the source is exemplary
and the flaw is a property of what it was pointed at:

| species | flaw lives in | readable? |
|---|---|---|
| assertion | the assert | yes |
| precondition | the setup (skip instead of fail) | yes |
| **magnitude** | the assert's *predicate*, not its subject | yes, if you ask "how much?" |
| duration | test lifetime vs system counters | **no** |
| **world** | **the input data** | **no** |

The *magnitude* species is new and it is subtle because everything else about the gate is right. The
hurt-overlay gate asserted that silhouette pixels **"moved toward vanilla's overlay red"** and
reported 3440/3440, with a working negative control. It measured **direction, not magnitude** — and
the shader was rendering ~70% red where vanilla renders ~30%, a predicate satisfied identically by
both. Wiring genuinely proven, strength never under test, and a player saw it immediately.

The repair generalises: **predict the value, do not merely assert the sign of the change.** Compute
*both* the correct and the suspected-wrong hypothesis from constants that originate outside the code,
and require the measurement to land on the right one. Here vanilla's overlay green is 0, so the blend
is a pure scaling in gamma space and green retention is `0.698` if right and `0.302` if inverted —
measured `0.6969`, control `0.3057`. A ratio needs no knowledge of the subject's own colours.

The *world* species is the live one here. A colour fix was verified against `--headless` and
measured byte-identical, concluding it was inert. There are two meshers: `--headless` renders
through `mesh_simple`, whose `ao` is corner-occlusion only, while `face_shade`'s per-face constants
live in `mesh_models`, which is what live terrain uses. **The change was verified against the one
scene in the tree that structurally cannot exercise it.**

Audit questions: *does any server-side counter accumulate past this gate's lifetime?* and *does the
input actually contain the structure the code under test exists to handle?*

**Measure by location, never by frame average.** Averaging a frame once gave G/R ≈ 1.13 and read as
"global gamma"; clustering by *location* revealed two spatially distinct populations, which a global
transform cannot produce. Ask *where*, not *what*.

---

## Rendering constraints

- **The model shader is at wgpu's 4-bind-group floor.** Its default `max_bind_groups` is 4 and the
  shader already spends all four (camera / atlas / palette / anim). A 5-group shader compiles and
  validates on an M5 (which reports 8) and **fails on any 4-group adapter** — a startup crash for
  other people and never for us. Fog was folded into the group-0 camera uniform for this reason.
  **Check the limit, not the adapter.**
- **Depth is `[0,1]` DirectX-style, not vanilla's reversed-Z.** Every ported depth comparison and
  bias flips sign: vanilla's `GREATER_THAN_OR_EQUAL` is our `LessEqual`, and a positive vanilla
  depth bias is negative here.
- **The GUI winding invariant is negative, not positive.**
  `sign(det(gui_ortho * gui_item_pose))` must **equal** `sign(det(Camera::view_projection()))`, and
  that sign is negative because `glam`'s DirectX RH perspective is itself negative. Coding to
  "positive determinant" ships an inside-out block that still looks plausibly isometric in a
  screenshot. Derive the front-facing sign from a real camera; do not assert a polarity.
- **Vanilla is not colour-managed.** Tint *and* shade multiply in **gamma** space
  (`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`). Doing it in linear pulls every shade
  factor toward 1.0 and washes the image out.
- **Shaders live in `.wgsl` files. Never inline one in Rust again.**
  `crates/lodestone-render/src/shaders/` and `crates/lodestone-shell/src/shaders/`, pulled in with
  `include_str!` — still compile-time, still a `&'static str`, no runtime asset loading. See
  [`docs/shaders.md`](./docs/shaders.md). "Just for a quick test" is not an exception:
  `no_wgsl_is_inlined_in_rust_sources` fails on any `@vertex`/`@fragment` under a crate's `src/`.
  The rule this replaces was *never put a double quote inside a shader, not even in a comment* —
  because a `"` terminated the enclosing Rust raw string and rustc then parsed the remaining WGSL
  and your *prose* as code: `error: prefix 'yet' is unknown`, pointing at English. The errors
  looked nothing like the cause, and it bit **four times**, twice inside comments that were
  themselves warning about the trap. Deleting the trap beat remembering it.
  Two things worth keeping from that history. First, **a `"` in a `.wgsl` comment is now legal and
  inert** — measured, not assumed: one put into `sky_disc.wgsl`'s comment left the suite green,
  while the same `"` in *code* position failed with `expected expression, found "\""`. Write shader
  comments normally. Second, **`cargo check` has never compiled a shader at any feature setting**,
  so before `wgsl_valid` a WGSL syntax error could reach `main` with all three required checks
  green — the only thing that read the WGSL was `create_shader_module`, inside an `#[ignore]`d GPU
  gate. `cargo test --workspace` now runs all 22 shaders through naga's front end in ~0.02s with no
  adapter.

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
2. **Decompiled source** under `.cache/mc/26.2/{src,client-src}` — reference for behaviour only,
   never transliterated. 26.2 ships de-obfuscated, so names are real.
3. **minecraft-data** — bootstrap and cross-check for **1.8–1.21.11 only**; it has no 26.x data, and
   was measured **92.29% covered and stale** for 26.2 collision shapes.

**Prefer interrogating the real jar over any community dataset.** `blocks.json` has no collision
geometry and no `destroySpeed`. Per-block-state tables come from booting the real server headlessly
(`SharedConstants.tryDetectVersion(); Bootstrap.bootStrap();`) and walking
`Block.BLOCK_STATE_REGISTRY` — see `crates/protocol/v770/tests/{collision_shapes,hardness}.rs` for
the generate-or-assert + `LODESTONE_REGEN=1` pattern, and `oracle-java/` for the dump programs.

**Never hand-count an entity metadata index. Run `EntityDataIndexOracle.java`.** It dumps every
`EntityDataAccessor` in the game sorted by index, so collisions land on adjacent lines. The first
time it was run it immediately found **two shipped bugs**: `Sheep.DATA_WOOL_ID` and
`Horse.DATA_ID_TYPE_VARIANT` were each off by one, both hand counts having missed
`AgeableMob.AGE_LOCKED`. **Every sheep in the game was rendering its default colour** while the
decoder reported a clean parse — invisible precisely because the tests encode with the same
constants they decode with, which is the `decode(encode(x))` trap in its most expensive form, and
because every sheep pixel gate builds its `EntityDraw` *downstream* of the wire.

Indices are reused across classes and **the guard you need depends on which classes collide**:
- Index 8 is `LivingEntity.DATA_LIVING_ENTITY_FLAGS` **and** `AbstractArrow.ID_FLAGS`, both `BYTE`,
  with the arrow's crit bit `0x01` bit-identical to "using item" — living vs **non**-living, so
  `entity_census::is_living` is the right guard.
- Index 15 is `Mob`'s flags (aggressive `0x04`) **and** `ArmorStand.DATA_CLIENT_FLAGS`, whose `0x04`
  is `CLIENT_FLAG_SHOW_ARMS` — and an armour stand *is* a `LivingEntity`, so `is_living` would report
  **every decorative armour stand with arms as an aggressive mob**. That collision is living vs
  living and needs `entity_census::is_mob`. `Display` also claims 15 as a `BYTE`.

So: check the oracle dump for the index, then pick the census column that separates the *actual*
claimants. Assuming the previous collision's guard generalises is how the armour-stand bug would
have shipped.

## Documentation

Keep [`docs/`](./docs/README.md) current: one doc per subsystem, `kebab-case`, named after the
feature rather than the file. Each should cover what it is, how it works, **how to change it and the
gotchas**, configuration, and dependencies. Update `docs/README.md` as the index.

Write down *why*, and especially write down what was measured. The most valuable thing in this repo
is not the code — it is the record of beliefs that were confidently held and turned out to be false.
