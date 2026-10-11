# Structure entities

## What it is

Entities a structure spawns while it generates: witch and black cat in swamp huts, chest minecarts in mineshafts, elder guardians in monuments, illagers and allays in mansions, drowned in ocean ruins, shulkers and elytra frames in end cities, and every entity embedded in a template (village folk and animals, bastion piglins and hoglins, igloo basement villagers, camp cushions, outpost allays). They spawn once, at generation, and are saved with their chunk afterwards.

## How it works

1. **Worldgen** (`terrain263/structures.rs::place_structures`) returns `StructurePlacement { writes, loot, entities }`. Per chunk it gathers coded piece entities (`StructurePiece::entities`, e.g. swamp hut, monument rooms), mineshaft carts (`mineshaft_blocks_for_chunk`), template entities (`StructureTemplate::placed_entities`) and data-marker spawns (`structure::entity::handle_marker`). An entity belongs to the chunk containing its block cell. Each `StructureEntity` is a full entity tag compound (`Pos`, `Rotation`, flags, `LootTable`...).
2. Template entity positions follow the template transform: `1 - v` under a mirror, quarter-turn rotation about the pivot, yaw = rotate + mirror - stored. Markers are filtered by the chunk box, and a handler may overwrite the marker block with air (mansion) or water (ocean ruin below sea level).
3. **Server**: `Terrain263ChunkSource::propose_generation_spawns` turns them into `GenerationSpawn { structure: Some(..) }` ahead of the creature packs, behind the same once-per-chunk `populated` ledger, and publishes the batch. The region source retains and dirty-marks the column, and a column with a pending batch is not saved, so a restart reloads from disk rather than regenerating.
4. `tick.rs::populate_resident_generation` classifies a structure candidate without terrain rules (`classify_generation_spawn`) and `materialize_generation` rebuilds it through `generation_population::structure_record` and `MobSim::restore_saved`, the load constructor, so persistence, cat variants, loot tables, item frames and cushions behave as after a reload. The spawn-mobs rule does not gate structure entities. Types the sim does not host are dropped.

Spawning is on the central mob handle on the tick thread, as natural population is; `entity_handoff` (moving entities between regions) is not involved.

## How to change it

- New coded entity: `Builder::spawn` (coded.rs) or `Canvas::spawn_elder` (monument.rs) with `mob_at_block`/`StructureEntity::new`.
- New marker: add an arm to `entity::handle_marker`.
- New hosted type: nothing here; `restore_saved` decides.
- Gotcha: a column regenerated a second time in one process spawns nothing (the ledger), so tests must read the spawn batch from the first `column()` call.

## Known gaps

- Armor stands (two in taiga armorer houses) are dropped: the sim has no body for them (ledger row `entities:armor_stand_body`).
- Spawn-time randomisation (e.g. villager professions, horse variants) is not applied; the mansion allay count uses a position-seeded roll rather than the reference stream.

## Configuration

None.

## Dependencies

`lodestone-worldgen` (`structure::entity`, `spawn_stage::GenerationSpawn`), `lodestone-server` (`generation_population`, `tick`, `entity_record::SavedEntity`, `MobSim::restore_saved`). Related: [block-entity-generation](./block-entity-generation.md), [worldgen-structures](./worldgen-structures.md), [worldgen-mob-generation-spawn](./worldgen-mob-generation-spawn.md).
