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
cargo build --release --bin lodestone --features live
```

- **`cargo build` is NOT a health check.** It skips test targets, so a crate whose lib compiles and
  whose lib-test does not reports green. Always `--all-targets`.
- **`cargo test -p <crate>` is not one either — it fail-fasts.** It aborts at the *first* failing
  test binary, so everything alphabetically later is never run and never reported. This has misled
  twice: a stale `block_updates` failure hid the new `hardness` gate entirely, and what looked like
  "a red test" in `lodestone-v770` was really **three red binaries and 14 failing tests**, masked
  because `serverbound_change_game_mode` sorts first. **Use `--no-fail-fast` when assessing crate
  health.**
- **The binary is `lodestone`, not `lodestone-shell`** — the `[[bin]]` name differs from the crate.
- **`--features live` is mandatory for multiplayer and fails silently without it.** The client still
  starts, renders the demo world, and reports a plausible `chunks=169` while whispering
  `no version family compiled in for protocol 776` into the log.
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
  **Never `git add -A`. Never `git reset --hard`, `git checkout .`, or `git stash`.** A blanket
  stage has clobbered in-flight work three times and destroyed a `lib.rs` edit once.
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
