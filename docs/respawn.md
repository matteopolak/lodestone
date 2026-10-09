# Respawn points

## What it is

Where a player reappears after dying or leaving the End: the bed or respawn anchor they set, in whichever dimension it stands, or the world spawn when there is none. One resolver, `respawn::plan`, serves both cases, and the point is saved with the player.

## How it works

`world_spawn::RespawnPoint` holds a block position, dimension, yaw, pitch and a `forced` flag, but no kind: the block at the position decides the rule at respawn time, so a broken bed just stops working. Setters are a bed click (overworld only), a charged anchor click in the Nether (see [respawn-anchor.md](respawn-anchor.md)), and `/spawnpoint` (forced).

`respawn::plan` is pure over `ChunkSource`s. It finds the point's dimension via the home dimension, the viewed dimension, or a sibling (`ChunkSource::sibling`), then:

| block at the point | outcome |
|---|---|
| charged anchor (or forced), Nether | stand beside it; a death respawn spends one charge unless forced |
| bed, overworld | stand beside it (`world_spawn::resolve_bed_respawn`) |
| other, point forced | stand in the cell if it and the cell above are free of solids and fluids |
| other | missing: world spawn in the home dimension, point cleared |

A dimension the protocol cannot reach falls back quietly to world spawn and keeps the point.

`connection_travel::perform_respawn` sends the result. A missing block sends game event 0 first (the client reports the bed or anchor gone), then clears the point. A death landing at home sends an ordinary respawn packet; any other landing sends a dimension change, which makes the client drop its chunks. A spent charge is published to the viewing dimension's block feed and the depletion sound goes only to the respawning player. If the landing is outside the built stream it returns `DimensionReset { target, route }`.

Both serve loops consume that reset alike: choose the source from `route` (`Home`, `Current`, `Sibling`), prepare the ticket transfer, `reset_stream`, `reset_player`, and stage `connection_travel::Destination`.

The End exit uses the same resolver with `consume` off: nothing is spent and no sound plays, but a missing block still sends event 0 and clears the point.

`respawn::store` writes a `respawn` compound into the player file's preserved fields (`pos` int array, `dimension`, `yaw`, `pitch`, `forced` byte only when true), the reference layout, so files round-trip. `respawn::load` reads it at join; a malformed record or unhosted dimension loads as no point. The native loop stores every iteration (a cheap compare); the browser loop keeps the point for the session only.

## How to change it

- New respawn block: add a `Found` variant and a branch in `respawn::resolve_in`.
- New dimension: update `Dimension::from_key`, `respawn_anchor::works_in` and `respawn::bed_works_in`.
- A new route must be handled in both loops' reset consumers; `perform_respawn` infers what is in view from the dimensions of `home` and `current`.
- A charge spent in another dimension is not broadcast to that dimension's viewers.
- Not modelled: the world-border check on the stand-up cell, and the facing rotation sent with the point.

## Configuration

None. `respawn::RESPAWN_FIELD` names the saved compound.

## Dependencies

`world_spawn`, `respawn_anchor`, `connection_travel`, `player_data`, `lodestone_core::Nbt`, and the protocol's `supports_dimension_change` and respawn, dimension-change and game-event encoders.
