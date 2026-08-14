# Wandering trader (issue #240)

## What it is

The entity-spawn slice of the wandering trader: `MobSim::spawn_wandering_trader`
spawns a real `minecraft:wandering_trader` with 1–2 `minecraft:trader_llama`
escorts leashed to it, ported from the entity-creation half of vanilla
`WanderingTraderSpawner.spawn`
(`.cache/mc/26.2/src/net/minecraft/world/entity/npc/wanderingtrader/WanderingTraderSpawner.java`).
**This is a partial implementation** — see "What is not implemented" below,
which is most of the issue's own scope.

## How it works

`spawn_wandering_trader(pos)` spawns the trader via the normal
`spawn_species` path (so it gets the same fallback goal set — wander and
look around — any unrostered species gets), then spawns two llamas at fixed
`±2` block offsets and leashes each to the trader via
`LeashHolder::Mob(trader_id)` — the same leashing mechanism `docs/leashing.md`
covers, reused rather than duplicated. `crates/lodestone-entity/src/attribute.rs`
gained a `"wandering_trader"` `TypeSpec` (`movement_speed` 0.5, inherited from
`Villager.createAttributes()` since `WanderingTrader` declares no override of
its own).

## What is not implemented

**The spawn cycle itself — when and where one appears.** Vanilla's
`WanderingTraderSpawner` is a `CustomSpawner` with a 1200-tick poll, a
24000-tick base delay, a 25%→75% climbing spawn chance, a 48-block
player-anchored search for a `PoiTypes.MEETING` point (falling back to the
player's own position), and a `BiomeTags.WITHOUT_WANDERING_TRADER_SPAWNS`
exclusion. None of this exists anywhere in this crate. It belongs beside
`crate::mob_spawn`'s existing per-species natural-spawn cap/timer engine — a
file outside this pass's ownership. See this session's broker note
(wandering trader spawn cycle) for the exact call shape a caller there needs
(`spawn_wandering_trader(pos) -> (i32, Vec<i32>)`, already built and tested).

**Wares.** `WanderingTrader.updateTrades` builds its offer list from
`TradeSets.WANDERING_TRADER_{BUYING,UNCOMMON,COMMON}`. This crate has no
merchant-offer/trade-table model at all — not for villagers, not for this.
A spawned trader here has no wares and nothing to trade. This is a real,
separate feature (a trade-table data model plus a trading-screen protocol
path), not a small gap.

**The "drink invisibility" behaviour is not damage-triggered, and the issue
body's framing of it is wrong.** Reading `WanderingTrader.registerGoals`
directly: it installs two `UseItemGoal`s — one drinks an invisibility potion
when `level().isDarkOutside() && !isInvisible()`, the other drinks milk when
`level().isBrightOutside() && isInvisible()`. This is a **day/night cycle**
behaviour (the trader hides at night, reappears by day), not a reaction to
taking damage. Nothing in `WanderingTrader.java` drinks anything in response
to an attack. Implementing the real mechanism needs: a generic
"use-an-item-under-a-predicate" goal (nothing like `UseItemGoal` exists in
`lodestone_entity::ai::goals` yet), and a way for a goal to read the world's
time-of-day — `MobController`/`NavigatingMob`'s perception seam carries
positions and player state today, not a clock. Both are real, separate
additions.

**Simplified escort placement.** Vanilla's `tryToSpawnLlamaFor` searches up
to 10 candidate positions within 4 blocks per attempt and can fail to find
space (so "2 attempts" is not a guarantee of 2 llamas). This always places
both at fixed `±2` offsets with no obstruction check, since `MobSim` has no
per-cell space query the way vanilla's `BlockGetter` does.

## How to change it, and the gotchas

- **Do not derive the invisibility behaviour from "on taking damage"** — the
  real trigger is `isDarkOutside()`/`isBrightOutside()`, verified against
  `WanderingTrader.registerGoals` directly rather than assumed from the
  issue text. A damage-triggered version would look plausible and be wrong.
- **The llama escort reuses `LeashHolder::Mob`, not a new field.** Any
  future "escort" concept (an iron golem following a village, say) should
  reach for the same leash mechanism before inventing a parallel one.
- **`spawn_wandering_trader` does not gate on anything** — no biome check,
  no POI search, no despawn timer (`trader.setDespawnDelay(48000)` /
  `setWanderTarget`/`setHomeTo` are all unimplemented). A caller wiring the
  real spawn cycle needs to add those, most of which belong in
  `mob_spawn.rs` alongside the natural-spawn cap engine rather than in this
  function.

## Configuration

No feature flags or env vars.

## Dependencies

- `MobSim::try_leash`/`LeashHolder::Mob` (`docs/leashing.md`) — the escort
  tether.
- `lodestone_entity::attribute::default_attributes` — the new
  `"wandering_trader"` `TypeSpec`.
- `.cache/mc/26.2/src/net/minecraft/world/entity/npc/wanderingtrader/{WanderingTrader,WanderingTraderSpawner}.java`,
  `npc/villager/Villager.java` (attribute inheritance).
