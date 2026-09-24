# Roadmap

The plan to 1:1 parity with Minecraft 26.2 — **client and server** — with a plugin
framework deep enough to host a port of any Java plugin.

**Open work lives in the [project tracker](https://github.com/matteopolak/lodestone/issues)**,
organised by epic and collected in the [Lodestone project board](https://github.com/users/matteopolak/projects/7).
This directory holds the durable *decomposition*: why the work splits the way it does,
what unblocks what, and the traps attached to each area. The tracker answers what is
open; these docs answer what to build, in what order, and how to verify it.

[`../backlog.md`](../backlog.md) remains the per-item trap record and the source of the
tier definitions. When it and the tracker disagree, the tracker is newer.

---

## What "1:1 parity" means here

Parity is a claim about **observable behaviour**, and it is only worth making if it is
falsifiable. Three standards, all of which this repo already applies:

1. **An expected value must originate outside the code under test.** `decode(encode(x)) == x`
   is satisfied by two symmetric misunderstandings. A fixture suite that uses the encoder
   under test is insufficient: live decoding must reject incompatible bytes, including
   truncation rather than accepting a self-consistent encoding. Parity is measured against
   a JVM oracle, captured server bytes, generated
   registry reports, or a hand-decoded spec example. Never against ourselves.
2. **Nothing is done until something on screen changes** (or, server-side, until a real
   client observes it). The dominant defect class here is the **island**: built, tested,
   reaching zero pixels because nothing calls it. Eleven confirmed. A crate's green test
   suite is a closed loop and cannot see one.
3. **A self-authored oracle validates the behaviour you chose to model.** Agreement
   between two ports sharing an author is weak evidence. Where that is the only available
   evidence, the issue says so.

## The tracks

| track | scope |
|---|---|
| **Tier 1** | before "a stranger could play survival for an hour" |
| **Tier 1½** | smaller, player-requested |
| **Tier 2** | expected by any real player |
| **Tier 3** | completeness — auth, chat signing, options, accessibility |
| **Tier 4** | **being a server**: the game simulation |
| **Infrastructure** | repo health, test integrity, the written record |
| **Architecture** | the bevy ECS substrate and plugin *API* |
| **Plugin framework** | plugin *capability* parity with Bukkit/Paper/Fabric |
| **Benchmarks** | measuring the expensive operations, and keeping them measured |

Tiers 1–3 are the client. Tier 4 is the server and is **plausibly larger than Tiers 1–3
combined** — a different axis, not a further step along the same one. The plugin
framework and benchmarks are orthogonal to both and can proceed in parallel.

## Area decompositions

- [`server-simulation.md`](./server-simulation.md) — chunk lifecycle, persistence, block
  behaviour, redstone, world state, the tick loop and server plumbing.
- [`server-entities.md`](./server-entities.md) — mob AI and pathfinding, spawning,
  breeding, villagers, raids, and the gameplay mechanics that turn a world into a game.
- [`client-rendering.md`](./client-rendering.md) — block entity renderers, sky and
  weather, smooth lighting, the remaining GUI screens, audio, entity render layers.
- [`client-simulation.md`](./client-simulation.md) — the movement modes not yet modelled,
  riding, combat, vitals, prediction and reconciliation, input.
- [`plugin-framework.md`](./plugin-framework.md) — the capability audit against the real
  Java plugin surface, and the port-feasibility analysis that makes the claim checkable.
- [`protocol.md`](./protocol.md) — measured packet coverage both directions, registries,
  chat signing, robustness, and the multi-version question.
- [`benchmarks.md`](./benchmarks.md) — what is measured, the harness, and how a
  regression is caught without turning CI into a flake generator.

## Invariants every implementation inherits

These are correctness constraints, not style notes. Their full rationale is recorded in
[`../../CLAUDE.md`](../../CLAUDE.md).

- **`EcsHandle` is not reentrant.** Holding its write guard across a call that takes the
  lock again deadlocks *silently* — no panic, no log line. Every plugin-facing entry point
  must make that state unrepresentable, so the first action cannot stall the client. This
  is a correctness requirement, not ergonomics.
- **The model shader is at wgpu's 4-bind-group floor.** A fifth group validates on an
  adapter reporting 8 and is a startup crash on any adapter reporting 4. Check the
  *limit*, not the adapter.
- **Depth is reversed-Z `[0,1]`**, so a ported comparison and
  bias transcribe with **no** sign flip and a depth attachment clears to
  `lodestone_render::DEPTH_CLEAR` (`0.0`). Tint and shade multiply in *gamma* space.
- **Keep the written record current.** Grep for the producer across the whole tree, not
  only for the consumer in one named file, before recording a missing capability.
- **A shell pipeline will destroy the evidence you are about to reason from.** `| head`
  read as absence hid a real constant; `| grep | tail` reported success while cargo
  returned 101. Let cargo write its own output and check its real exit status.
- **Four species of vacuous test**, two of which cannot be found by reading the test: the
  *duration* species (test lifetime vs system counters) and the *world* species (the
  input data lacks the structure the code exists to handle).

## Scale, honestly

This is a large body of work — the server track alone is a multi-year effort at hobby
pace, and "port any Java plugin" is a capability claim that needs its own audit rather
than an assertion. The value of the decomposition is not that it makes the work small;
it is that every unit is independently checkable, and that the traps are attached to the
item rather than rediscovered.

The foundations that already exist are unusually strong for a project at this stage, and
they are the reason the estimate is a roadmap rather than a wish: worldgen (noise router,
density, carvers, surface, aquifer, ore features), collision shapes for all 32,366 block
states, hardness, entity dimensions, block physics constants, a `path_types.rs` dumped from
  the pathfinding-node evaluator, and player movement — all **bit-exact against JVM oracles**.
