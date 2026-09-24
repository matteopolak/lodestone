# Backlog

## What it is

The tier definitions that order open work, and a pointer to where that work actually lives. **Open
issues are in [GitHub](https://github.com/matteopolak/lodestone/issues)**, organised as seven tier
epics with sub-issues; this file explains what each tier means, not what is in it.

[Tier 1](https://github.com/matteopolak/lodestone/issues/1) ·
[Tier 1½](https://github.com/matteopolak/lodestone/issues/2) ·
[Tier 2](https://github.com/matteopolak/lodestone/issues/3) ·
[Tier 3](https://github.com/matteopolak/lodestone/issues/4) ·
[Tier 4](https://github.com/matteopolak/lodestone/issues/5) ·
[Infrastructure](https://github.com/matteopolak/lodestone/issues/6) ·
[Architecture](https://github.com/matteopolak/lodestone/issues/7)

## The tiers

Ordered by how much each item changes what is on screen per unit of effort.

- **Tier 1 — needed before "a stranger could play survival for an hour."** The things whose absence
  is obvious within minutes of joining a world.
- **Tier 1½ — smaller, player-requested.** Polish an owner has actually asked for: HUD animation,
  feedback on a hit, the small motions that make the game feel alive rather than correct.
- **Tier 2 — expected by any real player.** Mostly the container screens: the furnace family, anvil,
  enchanting table, brewing, loom, smithing, stonecutter, grindstone, cartography, beacon, villager
  trading, horse inventory. Each is a `MenuKind` with its own slot layout, and a constant offset
  draws a plausible but transposed inventory that reads as an art bug rather than a wrong number.
- **Tier 3 — completeness.** Secure chat signing, online-mode auth end to end, server-provided
  resource packs, and the rest of the protocol surface a public server expects.
- **Tier 4 — the game simulation.** Being a *server* rather than a client: redstone, mob AI and
  pathfinding, villager economics, farming and breeding, spawning rules, block ticks, fluid flow,
  explosions, Anvil-format persistence, command execution. Plausibly larger than Tiers 1–3 combined
  and on a different axis entirely.
- **Infrastructure** — build, CI, tooling and the scanners.
- **Architecture** — decomposition and throughput work. Landing it ahead of the next feature batch
  is wanted rather than a detour: a handful of wiring files serialise nearly all parallel work, so
  decomposing them raises the ceiling on everything else.

## How to use it

**The tracker lags the tree.** Before starting an issue, `git log --oneline --grep '#<N>'` and read
the code it names — issues have been dispatched after the fix already landed. "Nothing exists for X"
is the least trustworthy claim you will find; grep for the capability rather than the name you
expected it to have.

**A player report outranks the tier order.** It is the only source of evidence no gate in this repo
can produce.

Three labels exist because they are this repo's recurring defect classes rather than because they
are generically useful: `island` (built, tested, and called by nothing), `stale-record` (a claim
that was true when written), and `vacuous-test` (a gate whose input cannot exercise the property).

See [`../CLAUDE.md`](../CLAUDE.md) for the working rules and [`meta/handoff.md`](./meta/handoff.md)
if you are dispatching work rather than writing it.
