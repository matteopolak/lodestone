# World spawn

## What it is

The server-side search that decides where a fresh player appears in a new world —
`crates/lodestone-server/src/world_spawn.rs`, a port of vanilla's
`MinecraftServer.setInitialSpawn` plus `PlayerSpawnFinder.getLevelRespawnPos`. It
also holds the per-player bed respawn point (`RespawnPoint`) and the set-time
legality check applied before one is accepted.

## How it works

`find_initial_spawn(source)` runs once per join, in `server.rs`'s
`ConfigurationFinished` arm, and its result is threaded onward for the whole
session:

```text
ConfigurationFinished
  └─ find_initial_spawn(source)          -> WorldSpawn { pos, yaw, pitch }
       ├─ begin_play_at(view_radius, pos) -> login + set_default_spawn_position
       │                                     + player_position teleport
       │                                     + chunk_cache_center from pos
       └─ serve_play(.., world_spawn, ..) -> apply_client_command's respawn arm
                                             teleports back here on death
```

The search itself is two nested pieces:

1. **`spiral_chunk_offsets`** — vanilla's 121-iteration ±5-chunk spiral, kept as
   an explicit sequence so the traversal *order* is a named, testable fact. The
   first candidate is the origin, then `(1,0)`, then `(1,1)`; it is not "the
   nearest land chunk".
2. **`get_level_respawn_pos(column, lx, lz)`** — scans a column from the top down
   and answers with the feet Y, or `None`. Two tests, both jar-dumped:

   | vanilla expression | here |
   |---|---|
   | `!blockState.getFluidState().isEmpty()` → abort the column | `spawn_has_fluid_state` |
   | `Block.isFaceFull(getCollisionShape(…), UP)` → stand here | `spawn_face_full_up` |

   Both resolve a block-state *string* into `lodestone-data`'s per-state bitsets
   via `spawn_state_id`, which tries the exact canonical state first and falls
   back to the block's default state.

When every chunk in the box is invalid — a fully-ocean box — the result is
`(8, GENERATOR_SPAWN_HEIGHT, 8)`, i.e. `(8, 64, 8)`.

## How to change it, and the gotchas

**Do not use `is_air_or_fluid` (or any "is this block solid" helper) as the
standability test.** That is what this function used to do, and it is why the
owner reported spawning in the air: the generator places `short_grass`,
`dandelion`, `poppy` and snow layers, all of which have **no collision** and all
of which `is_air_or_fluid` calls ground. Measured `face_full_up`:

| block | `face_full_up` | consequence |
|---|---|---|
| `short_grass`, `dandelion`, `poppy`, `snow` | `false` | vanilla scans past them |
| `grass_block`, `stone`, `oak_log`, `oak_leaves` | `true` | vanilla stands on them |

Leaves being `true` is not a bug to fix: vanilla genuinely spawns players on
treetops, and "correcting" it would be the divergence. If a treetop spawn needs
addressing, it needs addressing the way vanilla would — by picking a better spawn
*chunk*, not by disagreeing about what a floor is.

**The fallback Y is `64`, from `ChunkGenerator.getSpawnHeight`, and it is not a
surface query.** It used to be `origin.min_y + 1`, which is `y = -63` — inside the
bedrock floor, under an ocean. That arm is not rare: measured against the real
generator, **two of four probe seeds take it**, because without vanilla's climate
sampler the search centres on chunk `(0, 0)` and an all-ocean ±5 box is common.
`64` is two blocks above the generator's sea level of 62, so a pathological world
drops the player into open water instead of burying them.

**A hermetic fixture cannot exercise either defect.** Every one of this module's
original eleven tests was green against columns its own test code wrote.
`real_generator_spawn_is_always_standable_or_the_documented_fallback` is the
world-species gate: it runs `find_initial_spawn` against
`overworld_chunk_source` for four seeds and requires **both** arms to be
exercised as a precondition. It is `#[ignore]`d because it composes real columns
(~1.5 s per seed in release):

```bash
cargo test --release -p lodestone-server --lib real_generator_spawn -- --ignored --nocapture
```

**The base-name/exact-state join is load-bearing in both directions.** Keying only
by base name reports `oak_leaves[waterlogged=true]` (state 253) as dry, because
its default state is `waterlogged=false`; keying only by exact state fails to
resolve bare `minecraft:water`, because the generator emits fluids without their
`level` property. `spawn_state_resolution_agrees_with_the_census_for_every_surface_state`
gates every state of every surface block, not just the defaults — it found the
waterlogged case on its first run.

**Known gaps, in the order they matter:**

- **No climate sampler.** Vanilla's first step is
  `chunkSource.randomState().sampler().findSpawnPosition()`, which picks a spawn
  *chunk* from climate noise; we centre on `(0, 0)`. Seed `1234`'s first valid
  chunk is at Chebyshev **r=15** (842 columns), so simply widening the box is not
  affordable on the join critical path — the sampler samples noise rather than
  composing columns, which is the whole reason vanilla can do this cheaply.
- **`RespawnPoint` is stored, never used.** A death resolves against the *world*
  spawn, so a player with a bed still respawns at spawn. Vanilla's full path needs
  the placement teleport and the ticket search of `PlayerSpawnFinder.findSpawn`
  (see `docs/plans/world-state.md` unit P2).
- **No player-state persistence at all.** `find_initial_spawn` gives a *fresh*
  spawn; "rejoin puts me back where I was" is a separate feature.

## Configuration

None. `GENERATOR_SPAWN_HEIGHT` is a jar-sourced constant, not a tunable.

## Dependencies

- `lodestone-data::snow_support` — the jar-dumped `face_full_up` /
  `has_fluid_state` bitsets, and `block_states` for names and properties.
- `lodestone-server::chunk` — `ChunkColumn`/`ChunkSource`, and `is_air_or_fluid`
  for the bed-obstruction check (deliberately *not* for standability).
- `crates/lodestone-server/src/server.rs` — the only caller, plus the
  `world_spawn` value it threads into `serve_play` for the respawn teleport.
