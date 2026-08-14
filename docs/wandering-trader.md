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

**The spawn cycle now exists**, as `MobSim::run_wandering_trader_spawn_cycle`
in `crates/lodestone-server/src/mobs/mod.rs` (not `mob_spawn.rs` — the timer
and the position search both need `ChunkWorld`/`self.players`, which that
version-free crate deliberately does not depend on, so the cycle lives
alongside `run_patrol_spawn_cycle`, the closest existing precedent, instead).
It ports vanilla's own 1200-tick poll, 24000-tick base delay and 25%→75%
climbing spawn chance exactly (`WanderingTraderSpawner.tick`/`spawn`), plus
its 48-block, 10-attempt position search
(`WanderingTraderSpawner.findSpawnPositionNear`) against the same live,
player-following terrain snapshot `run_patrol_spawn_cycle` already uses.
`crate::tick::run_tick_loop` calls it once per tick, gated on the
`spawn_wandering_traders` game rule
(`GameRules::spawn_wandering_traders`/`WorldState::spawn_wandering_traders`,
both new in this pass), right after the patrol-spawn call.

**Still simplified**, disclosed in `run_wandering_trader_spawn_cycle`'s own
doc comment: no `PoiTypes.MEETING` search (always searches around a random
online player's own position, never a village meeting point),
no `BiomeTags.WITHOUT_WANDERING_TRADER_SPAWNS` exclusion, no
`hasEnoughSpace` collision check before spawning, and no
`setDespawnDelay`/`setWanderTarget`/`setHomeTo` afterwards (this sim has no
despawn-delay or home-position fields to set them on). **No persistence**:
vanilla's timer/delay/chance state survives a server restart
(`WanderingTraderData`); this sim's equivalent fields live on `MobSim` and
reset with it, since this crate has no save/load for `MobSim` at all yet.

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
- **`spawn_wandering_trader` itself still does not gate on anything** — no
  biome check, no POI search, no despawn timer (`trader.setDespawnDelay(48000)`
  / `setWanderTarget`/`setHomeTo` are all unimplemented). Its caller,
  `run_wandering_trader_spawn_cycle`, adds the timer/delay/chance and the
  position search around it, but not the biome/despawn/wander pieces — those
  still need `hasEnoughSpace`, a biome tag lookup and per-trader
  despawn/home state this sim does not have anywhere yet.
- **The timer state is not `mob_spawn.rs`, on reflection.** That crate is
  deliberately version-free and has no `ChunkWorld`/player-position
  dependency at all (see its own module doc); the trader cycle needs both
  for the position search, so it lives on `MobSim` in `mobs/mod.rs` instead,
  next to `run_patrol_spawn_cycle`'s identical shape (`patrol_next_tick`/
  `patrol_rng` fields, a `world: &ChunkWorld` parameter, one call per tick
  from `run_tick_loop`). Follow that precedent, not this doc's old plan.

## Configuration

No feature flags or env vars.

## Dependencies

- `MobSim::try_leash`/`LeashHolder::Mob` (`docs/leashing.md`) — the escort
  tether.
- `lodestone_entity::attribute::default_attributes` — the new
  `"wandering_trader"` `TypeSpec`.
- `.cache/mc/26.2/src/net/minecraft/world/entity/npc/wanderingtrader/{WanderingTrader,WanderingTraderSpawner}.java`,
  `npc/villager/Villager.java` (attribute inheritance).
