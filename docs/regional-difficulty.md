# Regional Difficulty

## What it is

Vanilla's `DifficultyInstance`: a scalar (`effective_difficulty`, roughly `0.0`–`6.75`)
grown from world difficulty, elapsed world time, a chunk's inhabited time and the moon
phase. `crates/lodestone-server/src/regional_difficulty.rs` is the pure formula;
`WorldStateHandle::regional_difficulty_at` (`crates/lodestone-server/src/world_state.rs`)
is the one wired call site, feeding `crates/lodestone-server/src/lightning.rs`'s
skeleton-horse-trap chance.

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
* **The consumers the original issue named — zombie/skeleton spawned-with-gear
  chance, reinforcement, spawn-cap composition — do not exist anywhere in this tree.**
  There is no zombie AI, no spawn-equipment path and no reinforcement mechanic to wire
  this scalar into; building those is separate, unimplemented work. The one real
  consumer today is `crate::lightning`'s skeleton-horse-trap roll — and, as of
  `crate::tick::run_tick_loop_with_weather` calling `crate::lightning::tick_thunder_for_chunk`
  every tick, that roll runs in a live server rather than only in `lightning.rs`'s own
  unit tests (see `docs/lightning.md`).

## Configuration

None — pure arithmetic over values the caller already has (`WorldStateHandle`'s
difficulty and clock).

## Dependencies

`lodestone_model::Difficulty` only.
