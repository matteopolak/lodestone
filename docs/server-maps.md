# Server-side maps

## What it is

The integrated server's map saved data: using an empty map creates a filled map, held maps are sampled from loaded terrain, and each holder is sent colour patches and decorations in `map_item_data` packets. Lives in `lodestone_server::maps`; the client side is [filled-map-rendering](./filled-map-rendering.md).

## How it works

- **Creation.** `ServerBound::UseItem` on an empty map (`server/map_tick.rs::use_empty_map`) allocates an id from `MapHandle::create_for_use`, centred on the 128<<scale grid (scale 0) and stamps `map_id` on the new stack.
- **Data.** `MapData` holds dimension, centre, scale, locked flag, 128x128 colour bytes (`colour<<2 | shade`), banners and per-holder markers. `MapStore` owns them by id; `MapHandle` is the shared handle in `WorldStateHandle::maps`.
- **Sampling.** `MapData::sample_terrain` runs per tick for a map in either hand: one in 16 columns per pass, plus the following column whenever one changed. It reads only resident column snapshots through `ChunkSource::resident_column`, so a source without that never produces colour, and it never generates chunks.
- **Sending.** Each connection owns a `MapSession`, ticked every 50 ms from `serve_play` (`tick_maps`). Every filled map in the inventory is tracked; dirty rectangles go out as patches, and decorations are resent every fifth check. Dropping the session removes the holder's marker and flushes.
- **Colours.** Per-block-state map colour is `lodestone_data::map_colors`, generated from the real 26.3 block-state registry by `MapColorOracle.java`; `tests/support/map_colors_jvm.txt` is the committed dump.
- **Persistence.** `<world>/data/minecraft/maps/<id>.dat` (gzip NBT, `{data, DataVersion}`) and `maps/last_id.dat`; the folder is attached from the first connection whose `PlayerDataStore` has a world directory. An in-memory world keeps maps in memory only. `map_id` also round-trips through item NBT (`item_nbt.rs`).

## How to change it

- New colour table: `just oracle-map-colors`, then `just regen-map-colors`; the tests assert the committed dump matches the generated table.
- Wire shape: `V770ServerProtocol::encode_map_item_data_body` (26.2 crate); other families would add `encode_map_item_data`.
- Item-frame markers: a frame holding a map reports it to every connection's `MapSession::tick`, which keeps a `frame` marker and sends the map to viewers who do not hold it; see [item-frames](./item-frames.md).
- Banner markers: `UseItemOn` with a filled map on a banner block goes through `server/map_tick.rs::filled_map_banner_click` before ordinary block use, toggling `MapData::toggle_banner`; the marker persists in the `banners` list.
- Persistence is native-only (`lodestone-anvil` is not linked on wasm); `read_saved`/`write_saved` are cfg-split.

## Configuration

None. Radius halves in ceiling dimensions; markers past 320 blocks are dropped unless tracking is unlimited.

## Dependencies

`lodestone-data` (map colours, block states), `lodestone-anvil` (`player_dat` NBT file IO), `lodestone-server` world state and chunk sources. The end-to-end gate is `crates/versions/26.2/tests/singleplayer_lan/singleplayer_map_stream.rs`.
