# Golem construction (issue #239)

## What it is

Block-pattern detection and spawn for the snow golem and the iron golem —
given a just-placed carved pumpkin or jack o'lantern, does a valid
`snow_block`/`iron_block` structure now exist around it, and if so, spawn
the golem. Lives in `crates/lodestone-server/src/mobs/mod.rs` as
`MobSim::try_construct_golem` (the pattern-matching internals it calls into
are `mobs/golem.rs`, split out of the same file since this doc was written), ported from vanilla
`CarvedPumpkinBlock.trySpawnGolem`
(`.cache/mc/26.2/src/net/minecraft/world/level/block/CarvedPumpkinBlock.java`).

## How it works

Vanilla's own matcher (`BlockPattern`/`BlockPatternBuilder`) is a small
general-purpose 3D pattern engine: a pattern is a grid of predicates in local
`(right, down, forward)` axes, and `find` brute-forces every position in a
`dist × dist × dist` cube from the placed block and every one of the 24 valid
`(forwards, up)` axis pairs until one orientation matches every cell. This
crate ports that engine directly (`GolemPatternMatch`, `find_golem_pattern`,
`vec3i_cross`) rather than special-casing "upright only", because **a golem
really can be built lying on its side against a wall in vanilla** — the
sideways case is real behaviour, not a generalisation invented for this port,
and `golem_tests::an_iron_golem_built_sideways_against_a_wall_still_matches`
is the control that would fail if the search were narrowed to `up = +Y`.

Two patterns, read verbatim from `CarvedPumpkinBlock.getOrCreateSnowGolemFull`/
`getOrCreateIronGolemFull`:

| golem | shape | blocks consumed |
|---|---|---|
| snow | one column: pumpkin over two snow blocks | 3 |
| iron | a T: three iron blocks in a row, one more centred below, pumpkin on top | 5 |

Snow is tried first and returns on a match, exactly mirroring vanilla's early
`return` in `trySpawnGolem` (the two shapes cannot both match the same
placement, but the order is still part of the port).

**`MobSim` has no block-write authority** — its own world is a read-only
`PathWorld` reference — so `try_construct_golem` is a pure detection query
over a caller-supplied `&dyn Fn(i32, i32, i32) -> String` oracle (the same
idiom `tick_with_terrain` already uses), and its `GolemConstruction.consumed`
field is a *report* of which cells to clear, not an action. The entity spawn
itself does happen inside `MobSim` (through `spawn_species`, so the golem
gets the same goal set and category any other spawn of its species would),
but clearing the pattern blocks and firing the break-particle level event is
the caller's job.

`species_shape`/`combat_defaults` already resolve real attributes for both
species (`lodestone-entity`'s `attribute.rs` gained an `"iron_golem"`
`TypeSpec` in this pass — `max_health` 100, `knockback_resistance` 1.0,
`step_height` 1.0 — matching `IronGolem.createAttributes()`; `"snow_golem"`
already existed).

## How to change it, and the gotchas

- **The server.rs hook is not wired yet.** Detection and spawn are complete
  and tested (`crates/lodestone-server/src/mobs/golem.rs`'s `golem_tests` module),
  but nothing in `server.rs`'s block-placement path calls
  `try_construct_golem` — that file is outside this pass's ownership. See
  the broker note this session left (golem construction server hook) for the
  exact anchor (`apply_use_item_on`'s generic placement branch, right after
  its own `propagate_placement` call) and proposed patch.
- **The iron golem's village-POI-count gate does not apply here, and the
  issue that named it was wrong to cite it for this mechanism.** Reading
  `CarvedPumpkinBlock.trySpawnGolem` directly: the player-built path never
  consults POI count at all — it calls `setPlayerCreated(true)` instead,
  which only suppresses the golem attacking the player who angered it. The
  POI-gated spawn is vanilla's *separate* natural village-golem-spawning
  system, which does not exist anywhere in this tree yet and is a different
  issue's scope.
- **`setPlayerCreated(true)` is not modelled.** This sim has no per-golem
  flag and no player-directed-hostility suppression for a neutral mob to
  begin with, so a player-built iron golem here behaves identically to a
  (not yet implemented) village-spawned one. Disclosed in
  `try_construct_golem`'s own doc comment.
- **Adding a third golem shape** (vanilla also has the copper golem): add a
  `GolemCell` pattern constant and a new arm in `try_construct_golem`,
  following the snow/iron precedent — the matcher itself
  (`find_golem_pattern`) is already generic over the pattern.
- **The consumed-blocks report includes the pumpkin.** A caller that clears
  only "the blocks I expect to be iron/snow" and skips the pumpkin will
  leave it floating with nothing under it.

## Configuration

No feature flags or env vars.

## Dependencies

- `lodestone_entity::attribute::default_attributes` — `"iron_golem"`'s new
  `TypeSpec` entry.
- `.cache/mc/26.2/src/net/minecraft/world/level/block/CarvedPumpkinBlock.java`,
  `.../state/pattern/{BlockPattern,BlockPatternBuilder}.java` — the ported
  pattern-matching engine and both golem shapes.
- `crates/lodestone-server/src/mobs/mod.rs`'s `spawn_species` — the entity spawn
  itself, so a constructed golem gets the same goal set as any other.
