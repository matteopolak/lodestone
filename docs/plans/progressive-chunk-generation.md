# Progressive chunk generation ("mip levels" for worldgen)

## What it is

An unimplemented design for serving distant chunk columns at a reduced generation stage (shape, caves, surface, structures; no ores, vegetation or top layer) and upgrading them to full generation as the player approaches. It lets the server stream a larger view radius without fully generating columns the player barely sees.

Two owner constraints: a player must not modify a chunk that is not fully generated, and a column that is already generated or modified is always sent whole, so a far-away player-built structure stays visible.

## How it would work

**The generator seam already exists.** `OverworldGenerator::column` memoises its stages in `StagedStore`: structure starts/refs, `pre_ore` (fill, surface, structure pieces, carvers), `post_ore`, `vegetation_stage`, `top_layer_stage`, then `spawn_stage`. Structure pieces are placed inside `pre_ore`, so a shaped column already contains villages and monuments. The seam has no public name yet.

**Two tiers plus a flag.**

```
GenStage::Shaped = structures, fill, surface, carve
GenStage::Full   = everything, including spawn stage
modified         = already tracked by RegionChunkSource's edit map and dirty set
served(column)   = max(stage_already_generated, stage_requested_by_band)
```

- No code path downgrades Full to Shaped.
- A Shaped column lacks ores, trees, snow/ice and generation-time mob spawns.
- Light and heightmap are computed at encode from the column's real blocks (`compute_served_light`), so a Shaped column is never black.

**Bands.** Per connection, Chebyshev distance up to `R_full` is Full and beyond that is Shaped. `R_full` has a floor of 8 so the ticked area, mob simulation, reach and explosions all sit inside Full. The band is computed in `ViewTracker`, independent of tickets.

**Upgrade on the wire** is a second `LEVEL_CHUNK_WITH_LIGHT` for the resident column. `World::load` replaces the chunk and the shell re-meshes on `ChunkLoaded`, as `resend_column_for_light` already exercises. Upgrades go through `ColumnPipeline` as stage-tagged jobs, so pacing and priority are inherited.

**Persistence.** Shaped columns are never persisted, because they can never be dirty. A column on disk is served Full at any distance (disk wins over the generator).

**Edit authority.** Enforced at `ChunkSource::set_block` in `ChunkStore`:
- Player dig/place/use arms check the stage first and re-sync the client's block on refusal.
- Any other write to a non-Full column is dropped, counted and warn-logged.
- The check runs inside the store's existing cache lock and must not re-enter the store.

**Boundary hazard.** A tree in a Full chunk spills canopy into a Shaped neighbour whose own pass never ran, so canopies clip along the band edge until the neighbour upgrades. Test fixtures must be forests at the boundary, since an ocean fixture passes vacuously.

## Staged plan

0. Measure first (go/no-go): the shaped/full cost ratio is unknown after the worldgen rewrite, and the old estimate (shaped about 20% of full) predates it. If shaped is at least about 50% of full, spend on client mesh LOD instead.
1. Give the generator seam a public name and a shaped constructor.
2. Add the stage to the server column and make the store stage-aware (never overwrite a higher stage).
3. Edit authority as above.
4. Banded streaming and upgrade/band-cross enqueueing in `ViewTracker`.
5. Experiment with `R_full` in {8, 12, 16, ...} to choose the default by horizon pop and frame cost.
6. Raise `MAX_RENDER_DISTANCE`.

## Limits

- This targets render distance 64, not 512. At 512 columns are about 1.05M; storage, meshing and bandwidth dominate long before generation. Beyond about 64 needs client LOD meshing (see `distant-terrain-lod.md`).
- Measured anchor: 31.1 KiB per packed server column; extrapolations scale linearly.
- Constants derived at small radii (`STORE_RETENTION`, pipeline window) need re-deriving for banded joins.
- Rejected: full per-status pipeline, delta upgrades, ticket-driven stages, client-side far generation, auto-upgrade-on-edit.

## Dependencies

`lodestone-worldgen` (`StagedStore`, stages), `lodestone-server` (`ChunkStore`, `RegionChunkSource`, `ColumnPipeline`, `ViewTracker`), `lodestone-world` (`World::load`).
