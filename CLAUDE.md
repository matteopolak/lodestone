# Lodestone — working rules

A from-scratch Minecraft client in Rust. Active scope is **v770 only** (protocol 776 / MC 26.2).

This file is the short, durable set of rules. The long-form record lives in
[`DESIGN.md`](./DESIGN.md) (architecture, plus a §12 validation log of ~20 beliefs that were
confidently held and empirically false) and [`HANDOFF.md`](./HANDOFF.md) (what is open and why).
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

The general rule: the transform that makes output readable is also the transform that can invent a
green. When a conclusion depends on what was *not* in the output, re-run without the filter.

**Four species of vacuous test.** Two cannot be found by reading the test — the source is exemplary
and the flaw is a property of what it was pointed at:

| species | flaw lives in | readable? |
|---|---|---|
| assertion | the assert | yes |
| precondition | the setup (skip instead of fail) | yes |
| duration | test lifetime vs system counters | **no** |
| **world** | **the input data** | **no** |

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
- **Never put a double quote inside a WGSL shader — not even in a comment.** The shaders live in
  Rust `r"…"` raw strings, so a `"` terminates the string early and rustc then parses the rest of
  your *prose* as code: `error: prefix 'yet' is unknown`, pointing at English. The errors look
  nothing like the cause. Use backticks in shader comments. This has now bitten twice, the second
  time immediately after being warned about it.

## Live-server hazards

- **Offline mode derives the account UUID from the username**, ignoring the UUID the client sends.
  Every test sharing a name shares one persisted player file — and a **dead player is held on the
  death screen, which sends no chunks**: a silent, total chunk blackout while join, keep-alives and
  entity movement all continue perfectly. Use `lodestone-testsupport`'s `unique_username`.
- **Vanilla's RCON client performs exactly one `read()` per request** and closes the socket unless
  `pktsize == read - 4`. **Write the entire frame in one call.**
- **A freshly summoned entity is not selector-visible until the next server tick.** Poll; never
  assert immediately. `Invulnerable:1b` also makes an entity un-targetable — use `NoAI:1b` for a
  stationary lure.
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

## Documentation

Keep [`docs/`](./docs/README.md) current: one doc per subsystem, `kebab-case`, named after the
feature rather than the file. Each should cover what it is, how it works, **how to change it and the
gotchas**, configuration, and dependencies. Update `docs/README.md` as the index.

Write down *why*, and especially write down what was measured. The most valuable thing in this repo
is not the code — it is the record of beliefs that were confidently held and turned out to be false.
