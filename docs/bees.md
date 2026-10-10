# Bees and hives

## What it is

Bees find blooms, carry nectar to a hive, live inside it, and tend crops on the way. The behaviour is `lodestone_entity::ai::bee`; the hive is a block entity (`lodestone_server::beehive`), and the bridge between them is `lodestone_server::mobs::bees` plus one pass in `tick::run_tick_loop`.

## How it works

**Bee side.** `BeeState` (on `NavigatingMob`, active after `enable_bee`) holds the hive and bloom cells, nectar, the cooldowns and the blacklist. `BeeState::tick` runs the clocks every tick. The nine bee goals read it; the world answers questions through `PathWorld` (`attracts_bees`, `bee_growth`, `hive_at`, `hives_within`, `is_loaded`).

- A bee wants home when it carries nectar, has searched 3600 ticks without any, or `Sky::bees_stay_in_hive` says so (any rain, or the overworld night, day time 12542 to 23460), unless it is stung, hunting, mid-pollination, in its 400-tick re-entry timer or the hive burns.
- Bloom search: Manhattan radius 5 around the bee, nearest first, only blooms it can path to; failures are remembered 600 ticks. 30% of checks are skipped, so a bee must pass near a bloom.
- Pollination: the bee steers straight at a point 0.6 above the bloom and hops between hover points. After 400 ticks each tick has a 1 in 5 chance of ending, which sets nectar. Rain stops it.
- Crops: with nectar and a valid hive, every ~15 ticks the cells 1 and 2 below the bee gain one growth step (crop age +1, berries on cave vines), at most ten per nectar.

**Entering and leaving.** The goal sets `BeeState::entering`. `MobSim::tick_with_terrain` drains it per mob; `resolve_hive_entries` refuses a hive that is gone or holds three bees, otherwise saves the bee as entity NBT (`saved_mob`), removes it, and queues a `HiveEntry`. `run_tick_loop` hands entries to `BlockEntityRegistry::enter_hive`, ticks every hive (`tick_hives`: a bee leaves once `ticks_in_hive` exceeds its minimum stay, 2400 with nectar and 600 without, and only when it is not night or raining and the hive face is clear), and calls `MobSim::release_bee`. A bee delivering nectar raises the block's `honey_level` by 1 (2 on a 1 in 100 roll, never past 5). `release_bee` places the bee at the hive face, ages it down by its time inside and gives it back the hive as home. A broken hive (`server::block_actions`) releases everyone at once.

**Saved state.** Bee fields use the vanilla names: `HasNectar`, `HasStung`, `hive_pos`, `flower_pos`, `TicksSincePollination`, `CannotEnterHiveTicks`, `CropsGrownSincePollination` (`sim_persistence_state`). The hive saves `bees` (each `entity_data`, `ticks_in_hive`, `min_ticks_in_hive`) and `flower_pos`; world generation's hives load as typed hives.

## How to change it

- New bloom or crop rule: the `minecraft:bee_attractive` / `minecraft:bee_growables` tags drive it; special cases are in `attracts_bees` and `grown_state`.
- Timers and radii are constants at the top of `bee.rs` and `beehive.rs`.
- Hives are found through `MobSim::set_hives` (block cell to occupant count), refreshed every tick from the registry.

Gotchas: wasm has no entity persistence, so there `resolve_hive_entries` does nothing and bees never enter. Not modelled: smoke sedation, the roll animation, bees turning on the player when their hive is broken.

## Configuration

None; weather and time reach the bees through `MobSim::set_environment` and `set_day_time`.

## Dependencies

`lodestone-entity` pathfinding and goals, `lodestone-data` block tags and states, `crate::entity_storage` (bee records), `crate::block_entities`.
