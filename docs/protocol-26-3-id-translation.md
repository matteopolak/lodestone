# 26.3 game-data ID translation

## What it is

`lodestone-v26-3::id_translation` maps the canonical 26.2 block-state and item IDs to their 26.3 wire IDs. It lets a future 26.3 adapter reuse internal game data without sending 26.2 registry numbers to a 26.3 peer.

## How it works

The 26.2 and 26.3 server reports are joined by block name plus state properties, or by item name. The generator compresses consecutive mappings into small forward and reverse runs. Typed `WireBlockStateId` and `WireItemId` values validate the 26.3 range; reverse translation returns an error for a 26.3-only entry because the canonical data model cannot represent it yet. The module is available to an adapter, but Play and registry exchange are still gated.

Old state property shapes did not change, but many numeric IDs did. For example, `minecraft:water[level=0]` maps from canonical `86` to wire `89`, while `minecraft:stone` stays `1`. The two directions use separate run tables because some existing entries change order.

## How to change it

Regenerate after caching both official server reports:

```sh
python3 crates/versions/26.3/tools/gen_id_translation.py
python3 crates/versions/26.3/tools/gen_id_translation.py --check
```

The generator requires every canonical entry to exist in 26.3, checks both ID spaces for duplicates and gaps, and writes `src/generated/id_translation.rs`. Update the fixed test witnesses from the reports independently of the generator. When a 26.3 adapter is implemented, translate at each wire boundary, including chunk palettes, block updates, entity metadata, and item-bearing packets. Do not change the canonical `lodestone-data` IDs to make a 26.3 packet fit.

## Configuration

There are no runtime flags. The generator reads `.cache/mc/26.2/generated/reports/{blocks,registries}.json` and the corresponding 26.3 reports. It only writes the table in the 26.3 crate.

## Dependencies

The module uses `lodestone-data` for canonical `StateId` and `Item`. The official server reports are generation inputs; no report JSON or runtime string map is linked into the game.
