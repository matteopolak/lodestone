# Structure chests

## What it is

The server-side pass that fills a generated structure's chests with rolled loot
(issue #337): shipwrecks, ocean ruins and igloos place real blocks since the
worldgen `structure` S2 unit, and every one of them arrived with an **empty**
chest — or, for an ocean ruin, no chest at all. `crates/lodestone-server/src/structure_loot.rs`
is vanilla's `TemplateStructurePiece.postProcess` data-marker pass plus the three
`handleDataMarker` overrides it dispatches to.

## How it works

```text
OverworldChunkSource::column(cx, cz)
  -> attach_structures
       -> generator.structure_references(cx, cz)   which structures reach this chunk
       -> generator.structure_starts(origin)       their pieces, positions, rotations
       -> structure_loot::chests_for_chunk
            -> embedded template bytes             the markers, which the parsed
                                                   template deliberately drops
            -> template::transform(...)            marker -> world position
            -> marker_loot_table(structure, meta)  the chest's table
            -> LootTableSet::roll + shuffle        27 slots, scattered
  -> ChunkColumn::set_block_entities
```

Four decisions carry the weight:

- **The markers come from the raw `.nbt` bytes, not from the parsed
  `StructureTemplate`.** That parser drops each block's `nbt` compound and the
  `BlockIgnoreProcessor` drops the marker block itself — both exactly as vanilla
  does, since a data marker is not meant to be a block in the finished world. The
  `metadata` string ("supply_chest", "chest", …) only exists in the file. The bytes
  are already embedded in `lodestone-server` (`assets/structure/`, via `build.rs`),
  so re-reading them costs a gunzip per piece and no I/O.
- **The starts come from `structure_references`, not from
  `structure_starts(cx, cz)`.** The latter is the starts whose *origin* is this
  column, and a shipwreck's chest is routinely in a neighbouring chunk.
  `references` is vanilla's own "which structures reach here", already narrowed to
  the chunk box. Using the origin list loses every chest that crosses a border,
  which is most of them.
- **The roll is seeded from the chest position and nothing else.**
  `OverworldChunkSource` regenerates an unedited column on *every* request, so a
  chest whose contents came from a per-connection RNG would hold different loot
  each time the column was streamed. The world seed is not mixed in and does not
  need to be: it already decides where the structure lands, so two seeds never ask
  for a roll at the same coordinates.
- **A chest is a real block entity now.** `BlockEntity::Container { id, slots }`
  (27 slots, `menu_name` `minecraft:generic_9x3`) replaced the `Opaque` arm for
  `minecraft:chest`, `minecraft:trapped_chest` and `minecraft:barrel`. Generated
  chests live in the `ChunkColumn`, not the live `BlockEntityRegistry`, so
  `apply_use_item_on` **hydrates** one into the registry on the first click via
  `ChunkSource::block_entity` — after which the registry copy is authoritative and
  a regeneration cannot refill it.

### Marker → table, from the jar

| structure | marker | table | chest is |
|---|---|---|---|
| `shipwreck`, `shipwreck_beached` | `map_chest` | `chests/shipwreck_map` | already in the template, one block **below** the marker |
| | `treasure_chest` | `chests/shipwreck_treasure` | ditto |
| | `supply_chest` | `chests/shipwreck_supply` | ditto |
| `igloo` | `chest` | `chests/igloo_chest` | already in `igloo/bottom`, one block **below** |
| `ocean_ruin_cold`, `ocean_ruin_warm` | `chest` | `chests/underwater_ruin_{small,big}` | **created** at the marker position |

Sources: `ShipwreckPieces.MARKERS_TO_LOOT` (`:69-71`), `IglooPieces.handleDataMarker`
(`:111`), `OceanRuinPieces.handleDataMarker` (`:318`). "big" is derived from the
template id containing `/big_`, which is how `OceanRuinPieces` names its large
family.

## How to change it

To support another structure's chests, add its `(structure id, marker)` pair to
`marker_loot_table`, and — if the chest is *created* by the marker rather than
already present in the template — to `marker_places_chest`. Both come straight
from that structure's own `handleDataMarker`.

### Gotchas

- **Shipwreck and igloo markers sit one block above the chest**
  (`position.below()` in both), while an ocean ruin's marker *is* the chest
  position. Getting this off by one puts loot in a block of air where nothing can
  reach it, and no test that merely counts rolls would notice — which is why
  `a_generated_shipwreck_arrives_with_a_filled_chest` asserts the block at the
  position is a chest *and* that the entity rolled something.
- The four structure-chest tables are bundled under `loot.rs`'s
  `DECORATION_ONLY_UNSUPPORTED` allowlist (see `docs/loot-tables.md`): they use
  `enchant_randomly`/`exploration_map`/`set_name`, so a shipwreck's map chest holds
  a plain map rather than a target-marked one. The items and counts are vanilla's.
- A marker whose table is not bundled yields an **empty** chest, not a missing one
  — the same "no such table" tolerance `block_drops::drop_block_loot` has.
- `drowned` markers in the big ocean-ruin templates are read and ignored: there is
  no structure-spawn path for a mob yet.

## Configuration

None. The templates are already embedded (`assets/structure/`), the loot tables are
already embedded (`assets/loot_table/`), and the pass runs unconditionally inside
`OverworldChunkSource::attach_structures`.

## Dependencies

- `crate::loot` for the tables and the roll, `crate::block_drops::bundled_tables`
  for the process-wide `LootTableSet`.
- `lodestone-worldgen`'s `structure` module for `StructureStart`/`PiecePlacement`
  and `template::transform`.
- `flate2` for the gzip wrapper Mojang ships templates with.
