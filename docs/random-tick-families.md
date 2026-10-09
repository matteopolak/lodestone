# Random-tick behavior families

## What it is

The random-tick scheduler picks positions and delegates each eligible block to a behavior family. Grass spreading, lava ignition, gravity and redstone live in separate modules; position selection and event plumbing are shared.

## How it works

`random_tick.rs` owns section eligibility, the position LCG, typed state predicates and dispatch. `random_tick/{grass,lava,gravity,redstone}.rs` hold the handlers; crop, sapling and leaf transitions are in `growth_tick.rs`. `lodestone_server::is_randomly_ticking_id(StateId)` classifies a block family without going back to text.

Everything is keyed by `lodestone_data::block_states::StateId`; text appears only at fixture or wire boundaries.

`ChunkColumn` derives ticking and reaction metadata together, once per palette state, so section counters answer the eligibility gate without rescanning. Keep both metadata vectors in the same pass if the classification changes.

Redstone fan-out is deterministic: centers and directions are enumerated with `UPDATE_ORDER`.

## How to change it

- Put behavior in the family module that owns it; keep position draws in the parent scheduler.
- Preserve the number and order of behavior RNG calls when editing a handler.
- Mutations must return `RandomTickEvent`s so the tick loop can persist and broadcast them.
- Update the family's draw-pattern or event-order tests with the behavior.

## Configuration

`RandomTickScheduler::new(position_seed, behavior_seed)` takes independent seeds; `tick_chunk` takes the per-section `tick_speed` (`DEFAULT_RANDOM_TICK_SPEED` is `3`).

## Dependencies

`ChunkColumn` (state and mutation), `ChunkSource` (neighbouring resident columns), `growth_tick`, `fire`, `gravity_tick`, and the redstone family modules.
