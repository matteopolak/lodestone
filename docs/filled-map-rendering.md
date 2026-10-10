# Filled map rendering

## What it is

How a filled map and the icons on it get from `MAP_ITEM_DATA` to pixels, held in either hand in first person or hanging in an item frame.

## How it works

The chain, one hop per row:

| hop | where | what it does |
|---|---|---|
| wire | `lodestone_v26_2::adapter` (26.3 reuses it through its dialect) | decodes the packet into `ClientEvent::MapItemData`; decoration type ids resolve through the fixed `map_decoration_type` registry, so 26.3's five extra types decode |
| state | `lodestone_game::maps::MapStore` | keeps the 128x128 colour grid and the icon list per map id. `color_revision` bumps on a pixel change, `decoration_revision` when the icon list differs. An absent icon list means unchanged, an empty one clears |
| source | `Sim::map_source` / `map_picture` | answers by map id, or by item-frame entity through `EntityMapIds`. No id is no map; there is no "lowest id" fallback |
| ids | `MainHandItem::map_id`, `EntityDraw` item map ids | each hand's stack and each frame carry their own `minecraft:map_id` |
| draw | `gpu::maps` | picture quad with the map texture at group 1, then the icon mesh with the decoration sheet at group 1, same pipeline |

### Icons

`lodestone_render::map_item::map_decoration_mesh` places one 8x8-pixel quad per icon in the map's pixel space: wire x/y are half pixels from the map centre, the sprite is centred half a pixel left and down of that point, rotation is clockwise sixteenths of a turn, and the sprite's top texel row faces the lower map edge (so rotation 0 reads upside down and rotation 8 upright; a player facing north is rotation 8). Later icons sit further out, `0.5 + 0.1n` map pixels in front of the picture; the reference's thousandth-of-a-pixel steps do not survive the forward depth buffer, so these are larger.

`MAP_DECORATION_TYPES` maps a registry key path to its sprite id and whether an item frame draws it. The pixels are not listed anywhere: `lodestone_assets::map_decoration_atlas` stitches whatever the pack's `atlases/map_decorations.json` directory source resolves to, and `MapRenderCache` loads it on the first map draw. Item frames hide `player`, `red_marker`, `blue_marker`, `player_off_map` and `player_off_limits`; a held map shows every type.

### Held maps

The stance follows the stacks: the main hand's map with an empty off hand is two-handed (0.76 blocks wide, tilting flat as the view pitches level, tipping upright by 45 degrees down); any other map is one-handed on its own side (0.38 wide). A map in the off hand therefore draws, on the left. `held_map_pose` is the pose chain; the hand swing and equip dip feed it. Each hand keeps its own cached meshes.

### Item frames

Pictures batch per distinct map; every visible frame's icons merge into one mesh drawn after them with the map-surface depth variant. The mesh cache key includes `decoration_revision`.

## How to change it

- A new decoration type: append to `MAP_DECORATION_TYPES` in registry order. `tests/items/map_decoration_sheet_gate.rs` checks the table against the jar's registry report and the sprite directory both ways.
- Placement rules live in `map_decoration_mesh`; the pixel gates (`tests/gpu/held_map_decoration_pixels.rs`, `framed_map_icons_draw_where_the_rules_place_them...` in `tests/items/framed_map_pixels.rs`) compute the expected place again from each sprite's PNG in `tests/support/map_decoration_probe.rs`.
- Held poses: `held_map_pose` and its arithmetic test.

## Configuration

`LODESTONE_MAP_DISABLE_DEPTH*`, `LODESTONE_MAP_DISABLE_FRUSTUM_CULL`, `LODESTONE_MAP_DISABLE_BACKFACE_CULL`, `LODESTONE_MAP_LIFT_PROBE`: live diagnostics that remove one decision at a time; unset in normal runs.

## Gaps

- The server draws no frame markers: it has no item frames, so a framed map never gains a marker.
- Cartography zoom and lock are absent.
- A decoration's custom name label is not drawn.
- The paper `map_background` behind a held map and the player's arms around it are not drawn.
- Held maps use full-bright light instead of the hand's sampled light.

## Dependencies

`lodestone-assets` (decoration sheet), `lodestone-render` (`map_item`, model pipeline), `lodestone-game` (`MapStore`), `lodestone-ecs` (`SessionMaps`), the 26.2 adapter and 26.3 dialect for the wire.
