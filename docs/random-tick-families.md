# Random-tick behavior families

## What it is

The random-tick scheduler selects positions and delegates each eligible block to a behavior family. The family modules keep grass spreading, lava ignition, gravity decisions, and redstone propagation separate from the shared position-selection and event plumbing.

## How it works

`random_tick.rs` owns section eligibility, the position LCG, typed state predicates, and dispatch. `lodestone_server::is_randomly_ticking_id(StateId)` exposes the block-family classification without converting state IDs back to text. `random_tick/grass.rs`, `lava.rs`, `gravity.rs`, and `redstone.rs` provide the behavior-specific handlers; crop, sapling, and leaf transitions are implemented by typed helpers in `growth_tick.rs`. Runtime reads, writes, and `RandomTickEvent` fields use `lodestone_data::block_states::StateId`; text remains only at explicit fixture or wire boundaries. The scheduler still visits sections and positions in the same order; each handler uses the scheduler's behavior RNG and appends events in its existing mutation order.

`ChunkColumn` derives ticking and reaction metadata together, once per palette state. Section counters then answer the eligibility gate without rescanning a column on each tick. Keep both metadata vectors in the same pass if the classification changes.

Redstone notification fan-out remains deterministic: centers and directions are enumerated with `UPDATE_ORDER`. Cross-column reads and writes continue to use the resident-column view owned by the redstone family.

## How to change it

Add behavior-specific logic to the family module that owns it. Keep position draws in the parent scheduler and preserve the number and order of behavior RNG calls when changing a handler. New mutations must return `RandomTickEvent`s so the tick loop can persist them and notify connected clients. Update the family module's focused draw-pattern or event-order tests when behavior changes.

## Configuration

`RandomTickScheduler::new` receives independent `position_seed` and `behavior_seed` values. `RandomTickScheduler::tick_chunk` receives the per-section `tick_speed`; `DEFAULT_RANDOM_TICK_SPEED` is `3`.

## Dependencies

The families depend on `ChunkColumn` for state and mutation, `ChunkSource` for resident neighboring columns, `growth_tick` for crop/sapling/leaf predicates, `fire` for ignition state and scheduled fire work, `gravity_tick` for landing decisions, and the redstone family modules for signal and notification rules.
