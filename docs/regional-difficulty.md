# Regional Difficulty

## What it is

Vanilla's `DifficultyInstance`: a scalar (`effective_difficulty`, roughly `0.0`–`6.75`)
grown from world difficulty, elapsed world time, a chunk's inhabited time and the moon
phase. `crates/lodestone-server/src/regional_difficulty.rs` is the pure formula. There
is no single wired call site — `crate::tick::run_tick_loop` resolves a fresh
`DifficultyInstance` inline, once per tick, from `WorldState::time()`/`difficulty()`,
and feeds its two derived quantities to every real consumer that tick:
`crate::lightning`'s skeleton-horse-trap chance (`effective_difficulty()` directly) and
`MobSim::set_spawn_difficulty`/`set_spawn_monsters_enabled` (`special_multiplier()`/
`is_hard()`, feeding the zombie-family gear roll, door-breaking roll and reinforcement
roll below).

This is a different thing from **world difficulty** (peaceful/easy/normal/hard, the
`/difficulty` setting) — that already exists as `WorldStateHandle::difficulty`.
Regional difficulty is a *derived* per-query scalar, not a setting.

## How it works

`DifficultyInstance::new(base, total_game_time, local_game_time, moon_brightness)`
computes `calculate_difficulty`, transcribed clause by clause from
`DifficultyInstance.calculateDifficulty` — see the module's own doc comment for the
full clause table. In short: Peaceful is always `0.0`; otherwise a base scale of
`0.75` grows by up to `0.25` as `total_game_time` passes a 72,000-tick offset (capped
at 1,440,000 ticks), plus a local term from `local_game_time` (capped at 3,600,000
ticks, weighted `1.0` on Hard and `0.75` otherwise) and a moon-phase term **clamped by
the global term, not by `1.0`** — the one clause easy to get backwards. Easy halves the
local term. The total is multiplied by the difficulty's ordinal (Peaceful 0 … Hard 3).

`moon_brightness_for_day_time` reproduces `DimensionType.MOON_BRIGHTNESS_PER_PHASE`
indexed by `(day_time / 24000) % 8` — day 0 is a full moon.

## How to change it

The formula itself (`calculate_difficulty`) should not need touching; it is a direct,
tested port. What is missing:

* **Chunk inhabited time is not tracked anywhere in this crate** — `chunk_nbt.rs`'s
  `InhabitedTime` field is a hardcoded `Nbt::Long(0)`. Vanilla increments it once per
  natural-spawn cycle (`ServerChunkCache.tickSpawningChunk`'s
  `chunk.incrementInhabitedTime`). Until that lands, every caller passes
  `local_game_time = 0`, which only ever *understates* the result.
* **Every consumer the original issue named is now real and wired**, except spawn-cap
  composition (see below): zombie/skeleton spawned-with-gear chance
  (`lodestone_entity::spawn_equipment`, fed `special_multiplier`/`is_hard` from
  `crate::tick::run_tick_loop`), the zombie family's door-breaking coin flip (same
  feed), and zombie reinforcement-calling-for-backup (`MobSim::attack_from_player`'s
  `Zombie.hurtServer` roll, gated on `is_hard` and the `spawn_mobs` game rule — see
  `docs/mob-species-spawning.md` for all three). `crate::lightning`'s skeleton-horse-trap
  roll remains the one consumer that reads `effective_difficulty()` directly rather than
  through `special_multiplier()`/`is_hard()`.
* **Spawn-cap composition is not a real vanilla mechanic to model.** Checked against
  `.cache/mc/26.2/src/net/minecraft/world/level/NaturalSpawner.java`: the only
  `getCurrentDifficultyAt` reads in that file are inside `finalizeSpawn` (the gear/
  door/reinforcement roll this doc already covers) — the mob-cap arithmetic itself
  (`MobCategory`'s `70`-per-`289`-chunks-style constants) is not difficulty-scaled at
  all in vanilla. The original issue's "spawn-cap composition" phrase does not name a
  real, missing formula.
* **The "peaceful never starves below half a heart" premise in the original issue is
  also not quite right, and it needed no regional-difficulty input either way.**
  `.cache/mc/26.2/src/net/minecraft/world/food/FoodData.java`'s `tick` shows Peaceful's
  real protection is upstream of the starve-damage floor: the depletion branch's own
  `difficulty != PEACEFUL` guard means `foodLevel` never *reaches* zero on Peaceful in
  the first place, so the starvation arm never fires at all — not a floor at some
  nonzero health. (The starvation *arm*'s own difficulty gate, `health > 10.0 ||
  HARD || (health > 1.0 && NORMAL)`, actually *includes* Peaceful in its first clause,
  so a Peaceful player above 10 health can still take starve damage down to exactly
  10 — five hearts, not one-half.) `crate::food::FoodData::tick` already implements
  both halves correctly and is wired into production via `PlayerVitals::tick_food`
  (`crate::server::serve_play`) — see `crate::food`'s own module doc.

## Configuration

None — pure arithmetic over values the caller already has (`WorldStateHandle`'s
difficulty and clock).

## Dependencies

`lodestone_model::Difficulty` only.
