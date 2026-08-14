# Dropped-item physics: swept collision against real block shapes

## What it is

How a dropped item entity comes to rest on the server. It used to be a
solid/air boolean per cell with the rest height hardcoded to the top of the
block, which meant an item settled a full block too high on any grassy surface
and half a block too high on a slab. It is now
`lodestone_physics::collision::collide` — vanilla's own `Entity.collide` sweep —
against the real per-block-state collision shapes from `lodestone-data`.

## How it works

`MobSim::tick_with_terrain` takes a **block-state name** oracle,
`&dyn Fn(i32, i32, i32) -> String`, and the item pass wraps it in
`mobs::ItemCollision`, a `CollisionView`. Per cell that resolves a name to a
state id and the state id to a `&'static [Aabb]` in rodata:

```
block-state name ──► block_state_id (O(1) exact-string map)
                └──► block_states::state_id (fallback: span default)
                                            └──► collision_shapes::collision_boxes(id)
```

`settle_item` then does what `ItemMotion::tick` cannot: it recovers the movement
that function applied by translating, replays it as a sweep, and writes the
resolved position back.

```
before ─► ItemMotion::tick (gravity, translate, drag, bounce) ─► attempted delta
       └─► collide(view, attempted, item box, on_ground, step 0.0) ─► resolved
       └─► position = before + resolved; zero each eaten velocity component
```

The item box is `0.25 × 0.25` with **`step_height` 0.0** — `ItemEntity` never
overrides `maxUpStep()`, so a dropped item cannot climb a slab it slides into.

### Why the boolean could not be patched

Read out of the generated shape table rather than predicted — and the table is a
dump from the real 26.2 server, so it is an outside source:

| block state (bare name) | true collision top | the boolean's answer |
|---|---|---|
| `short_grass`, `tall_grass`, `snow[layers=1]` | **0.0** (no boxes at all) | 1.0 |
| `oak_slab` | 0.5 | 1.0 |
| `enchanting_table` | 0.75 | 1.0 |
| `soul_sand`, `mud`, `chest` | 0.875 | 1.0 |
| `dirt_path` | 0.9375 | 1.0 |
| `oak_fence` | **1.5** (uncapped) | 1.0 — *too low* |

The grass row is the one with the visible symptom, because almost any grassy
surface has a plant on it: almost every dropped item floated. The fence row
matters because it fails in the opposite direction, so a gate that only checked
for floating would miss it. `oak_leaves` is **1.0** and was rejected as a test
input for exactly that reason — "leaves are see-through so surely not a full
cube" is the wrong intuition and would have produced a gate that passes either
way.

## How to change it

- **Do not resolve a state id here with `mobs::block_state_id_or_default`.** That
  helper returns the block's *lowest* state id, and its own doc comment says it
  "is not a substitute for `block_state_id` where the properties matter
  (collision shapes, path types)". It was used here for one iteration and a bare
  `minecraft:oak_slab` resolved to a full cube, so the fix reproduced the bug it
  removes. `block_states::state_id` consults `span.default` — vanilla's real
  `defaultBlockState()` — and is the correct fallback.
- **The ordering inside the tick is deliberately unchanged.** Gravity and drag
  still run inside `ItemMotion::tick` (in `lodestone-entity`) *before* the
  collision. Vanilla's `ItemEntity.tick` collides between them, so its friction
  reads the post-move `onGround`. Matching that means changing a crate this one
  does not own; keeping the order fixed is what makes the pre-existing settling
  gates still meaningful rather than merely still green.
- **Per-block friction is still not wired.** `block_friction` keeps
  `DEFAULT_BLOCK_FRICTION`, so an item slides on ice as it does on stone.
- **Adding a surface to the gates means checking it is discriminating first.**
  Evaluate the full-cube hypothesis at the candidate — if its collision top is
  `1.0`, the arm measures only that the code runs.
  `the_surface_fixtures_resolve_to_the_shapes_the_gates_assume` asserts exactly
  that, and also that the *bare* name still resolves to the state the height was
  read from.

## Configuration

| knob | where | value |
|---|---|---|
| `ITEM_DIMENSIONS` | `mobs/mod.rs` | `0.25 × 0.25`, step height `0.0` |
| `VOID_DESPAWN_DEPTH` | `mobs/mod.rs` | 64 blocks below `min_y` |
| `ITEM_GRAVITY` / `ITEM_AIR_DRAG` | `lodestone-entity` | `0.04` / `0.98` |

## Cost, and why it is a counter

Swept collision against real shapes is strictly more work per item than one
boolean lookup, and nothing bounds how many items sit on a floor.
`MobSim::items_settled_probe_count` reports the cells the last tick's settling
pass asked for, and `run_tick_loop`'s existing "Can't keep up!" warning carries
it as `item_settle_probes` — so an item-driven overrun is visible in the log
rather than inferred.

**Measured: 36 probes per item per tick, at both 1 item and 64 (2,304 total) —
exactly linear.** So 200 items on a floor is ~7,200 probes in one tick's
settling pass, each a `String` from the oracle plus a name→id lookup plus an
O(1) rodata index.

`the_settling_sweep_costs_a_constant_number_of_probes_per_item` asserts both
halves, and the second is not redundant: linearity alone is satisfied by a pass
that became uniformly ten times more expensive, which is why there is an
absolute bound as well. This repo has already shipped a latency defect whose
per-unit cost was fine and whose single unserviced window was not — see
[`server-view-streaming.md`](server-view-streaming.md).

## Dependencies

- `lodestone-physics` — `CollisionView`, `collision::collide`,
  `EntityDimensions`. Already a dependency of `lodestone-server` for melee
  knockback, so this added no graph.
- `lodestone-data` — `block_states::state_id`, `collision_shapes::collision_boxes`.
  Already a dependency for the path-type and hardness censuses.
- `lodestone-entity` — `ItemMotion`, unchanged by this.

## The gates

All in `crates/lodestone-server/tests/item_settling.rs`.

| gate | what it would catch |
|---|---|
| `an_item_rests_on_the_real_collision_shape_of_each_surface` | the full-cube rest height, on four surfaces at once |
| `the_surface_fixtures_resolve_to_the_shapes_the_gates_assume` | a bare name resolving to a different default state, or a candidate that stopped discriminating |
| `a_thrown_item_stops_against_a_wall_instead_of_passing_through_it` | horizontal collision not being resolved at all |
| `the_settling_sweep_costs_a_constant_number_of_probes_per_item` | a superlinear settling pass, or an order-of-magnitude per-item rise |

Both behavioural gates were run against deliberate neuters and observed to fail:

- **full-cube neuter** (`ItemCollision` emitting a unit cube for every non-air
  cell): all **4 of 4** surfaces failed, each landing exactly on the full-cube
  hypothesis of `66` against true answers of `65`, `65.5`, `65.9375` and `66.5`.
  The gate collects every arm rather than asserting inside the loop precisely so
  a control run demonstrates all four instead of stopping at the first.
- **vertical-only neuter** (restoring `attempted` for x and z): the thrown item
  reached `x = 5.554`, straight through a two-cell-thick wall whose near face is
  at `3.0`.
