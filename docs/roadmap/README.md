# Roadmap

## What it is

The plan to 1:1 parity with Minecraft 26.2, client and server, with a plugin framework deep enough to host a port of any Java plugin. Open work lives in the [project tracker](https://github.com/matteopolak/lodestone/issues) and the [project board](https://github.com/users/matteopolak/projects/7); this directory holds the durable decomposition (why work splits this way, what unblocks what, the traps per area). [`../backlog.md`](../backlog.md) is the per-item trap record and tier definitions; where it and the tracker disagree, the tracker is newer.

## What "1:1 parity" means

Parity is a claim about observable behaviour and is only worth making if falsifiable.

1. **An expected value must originate outside the code under test.** `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings; live decoding must reject incompatible bytes, including truncation. Measure against a JVM oracle, captured server bytes, generated registry reports or a hand-decoded spec example, never against ourselves.
2. **Nothing is done until something on screen changes** (server-side: a real client observes it). The dominant defect is the island: built and tested but called by nothing. A green crate suite cannot see one.
3. **A self-authored oracle validates only the behaviour you chose to model.** Agreement between two ports with one author is weak evidence; where it is all there is, the issue says so.

## The tracks

| track | scope |
|---|---|
| Tier 1 | before "a stranger could play survival for an hour" |
| Tier 1½ | smaller, player-requested |
| Tier 2 | expected by any real player |
| Tier 3 | completeness: auth, chat signing, options, accessibility |
| Tier 4 | being a server: the game simulation (plausibly larger than Tiers 1-3 combined) |
| Infrastructure | repo health, test integrity, the written record |
| Architecture | the bevy ECS substrate and plugin API |
| Plugin framework | plugin capability parity with Bukkit/Paper/Fabric |
| Benchmarks | measuring expensive operations and keeping them measured |

Tiers 1-3 are the client; the plugin framework and benchmarks run in parallel to both.

## Area decompositions

- [server-simulation](./server-simulation.md): chunk lifecycle, persistence, block behaviour, redstone, world state, tick loop.
- [server-entities](./server-entities.md): mob AI, spawning, breeding, villagers, raids, gameplay mechanics.
- [client-rendering](./client-rendering.md): block entities, sky and weather, lighting, GUI screens, audio, entity layers.
- [client-simulation](./client-simulation.md): unmodelled movement modes, riding, combat, vitals, prediction, input.
- [plugin-framework](./plugin-framework.md): the capability audit and port-feasibility analysis.
- [protocol](./protocol.md): packet coverage, registries, chat signing, robustness, multi-version.
- [benchmarks](./benchmarks.md): what is measured, the harness, and regression catching without flakes.

## Invariants every implementation inherits

Rationale is in [`../../CLAUDE.md`](../../CLAUDE.md) and [architecture](../architecture.md).

- `EcsHandle` is not reentrant: holding its write guard across a call that locks again deadlocks silently. Plugin-facing entry points must make that state unrepresentable.
- The model shader is at wgpu's 4-bind-group floor; a fifth group crashes startup on adapters reporting 4. Check the limit, not the adapter.
- Depth is reversed-Z `[0,1]` (clear to `lodestone_render::DEPTH_CLEAR`, `0.0`), so ported comparisons and biases transcribe with no sign flip. Tint and shade multiply in gamma space.
- Before recording a missing capability, grep the whole tree for the producer, not just one consumer file.
- A shell pipeline can destroy the evidence you reason from (`| head` hid a real constant; `| grep | tail` reported success while cargo returned 101). Let cargo write its own output and check its exit status.
- Four species of vacuous test exist; two cannot be found by reading the test: the duration species (test lifetime vs system counters) and the world species (input data lacks the structure the code handles).

## Scale

The server track alone is a multi-year effort at hobby pace, and "port any Java plugin" needs its own audit rather than an assertion. The decomposition does not make the work small; it makes each unit independently checkable and attaches the traps to the item. The estimate is a roadmap rather than a wish because of existing foundations that are bit-exact against JVM oracles: worldgen (noise router, density, carvers, surface, aquifer, ore features), collision shapes for all 32,366 block states, hardness, entity dimensions, block physics constants, a `path_types.rs` dumped from the pathfinding-node evaluator, and player movement.
