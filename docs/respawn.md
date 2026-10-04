# Respawn points

## What it is

Where a player reappears after dying or leaving the End: the bed or respawn anchor they set, in whichever dimension it stands, or the world spawn when there is none. One resolver (`respawn::plan`) serves both the death respawn and the End exit, and the point is saved with the player.

## How it works

**The point.** `world_spawn::RespawnPoint` holds a block position, a dimension, yaw and pitch, and a `forced` flag. It records no kind: the block standing at the position decides the rule at respawn time, so a bed that was broken or replaced simply stops working. Setters: a bed click (`RespawnPoint::block`, overworld only), a charged anchor click in the Nether (see [respawn-anchor.md](respawn-anchor.md)), and `/spawnpoint` (`RespawnPoint::forced`).

**Resolution** (`respawn::plan`, pure over `ChunkSource`s, tested without a connection). The point's dimension is looked up in three places, in order: the connection's home dimension, the dimension it is viewing, or a sibling reached through `ChunkSource::sibling`. In that dimension the block decides:

| block at the point | outcome |
|---|---|
| anchor with charge (or forced), in the Nether | stand beside it; a death respawn spends one charge unless the point is forced |
| bed, in the overworld | stand beside it (`world_spawn::resolve_bed_respawn`) |
| anything else, point forced | stand in the cell if it and the cell above are free of solids and fluids |
| anything else | missing: world spawn in the home dimension, and the point is cleared |

A point in a dimension the protocol cannot reach (no dimension-change packet, or no sibling) falls back to the world spawn quietly and is kept.

**Sending it** (`connection_travel::perform_respawn`). A missing block sends game event 0 first (the client prints that the bed or anchor is gone or obstructed), then clears the point. A death landing at home sends an ordinary respawn packet; any other landing sends a dimension change, which makes the client drop its chunks. A spent anchor charge is published to the viewing dimension's block feed and neighbours are notified so a comparator reading it follows. The depletion sound goes only to the respawning player. When the landing is outside the stream already built, the function returns a `DimensionReset { target, route }`.

**The loops.** Both serve loops consume the reset the same way: pick the destination source from `route` (`Home`, `Current`, or `Sibling`), prepare the ticket transfer, re-centre the stream with `reset_stream`, `reset_player`, and stage `connection_travel::Destination` (`Home`, none, or `Dimension`). The native loop also loads and saves the point; the browser (`wasm32`) loop keeps it for the session only.

**End exit.** Same resolver with `consume` off: nothing is spent and no sound plays, but a missing block still sends event 0 and clears the point, and a Nether anchor is a valid destination.

**Persistence.** `respawn::store` writes the point into the player file's preserved fields as the `respawn` compound (`pos` int array, `dimension` string, `yaw` and `pitch` floats, `forced` byte written only when true), the layout the reference game uses, so such a file round-trips. `respawn::load` reads it at join; a malformed record or an unhosted dimension loads as no point. The native loop calls `store` every iteration, which is a cheap compare and only changes the preserved fields when the point changed.

## How to change it

- A new respawn block: add a `Found` variant and a branch in `respawn::resolve_in`.
- A new dimension: `Dimension::from_key` and `respawn_anchor::works_in` / `respawn::bed_works_in` decide what it accepts.
- Gotcha: `perform_respawn` decides what is in view from the dimensions of `home` and `current`; a new route has to be handled in both loops' reset consumers.
- Gotcha: a charge spent in another dimension is not broadcast to that dimension's viewers.

## Not modelled

- The world-border check on the stand-up cell, and the facing rotation sent with the point.
- The browser loop does not persist the point.

## Configuration

None. `respawn::RESPAWN_FIELD` names the saved compound.

## Dependencies

`world_spawn` (point type, bed rules), `respawn_anchor` (anchor rules and stand-up search), `connection_travel` (frames, reset, sibling staging), `player_data` (preserved fields), `lodestone_core::Nbt`, and the protocol's `supports_dimension_change`, `encode_respawn_with_teleport_id`, `encode_dimension_change_with_teleport_id` and `encode_game_event`.
