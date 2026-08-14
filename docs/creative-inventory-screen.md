# Creative inventory screen

## What it is

Vanilla's `CreativeModeInventoryScreen` (issue #158): the 14-tab strip, the
scrollable 5×9 item grid, the search tab, and the inventory tab. Contents come
from `crates/lodestone-shell/src/container/creative_items.rs` — a
hand-transcription of the decompiled `CreativeModeTabs.java`, 1725 items over 14
tabs, every id cross-checked against `lodestone-data`'s real item registry.

## How it works

Two files, split the way every other screen in this crate is: geometry and hit
test in `container/`, the per-frame wiring in `app/`.

```
container/creative_items.rs   the transcribed table (CREATIVE_TABS)
            │
container/creative.rs         creative_layout   -> rects, one expression for draw and click
                              creative_hit_test -> CreativeHit
                              creative_geometry -> ContainerGeometry
            │
app/creative_screen.rs        creative_screen_open()   is it up?
                              handle_creative_click()  clicks
                              scroll_creative_screen() wheel
                              drag_creative_scroll()   scrollbar thumb
                              creative_panel_geometry() the frame's geometry
            │
app/lifecycle.rs              its own MouseInput / MouseWheel / CursorMoved arms,
                              plus KeyGate::creative_search for the search box
app/redraw.rs                 ContainerRenderer::render_geometry_scaled
```

### Why it is not a `MenuKind`

Vanilla's screen is backed by `ItemPickerMenu`, a **client-only**
`AbstractContainerMenu` with no server container, and `lodestone-game`'s
`menu.rs` says plainly that `MenuKind` must not grow. So this screen owns its own
layout rather than extending `slot_layout`.

Everything *below* that seam is shared, which is the point:
`creative_geometry` returns a `ContainerGeometry` and
`ContainerRenderer::render_geometry_scaled` draws it — the same pipelines, bind
groups and four-pass order the ordinary container screen uses. No new shader, no
new bind group, and the same guarantee about stack counts landing over their
icons rather than under them. `render_geometry_scaled` was split out of
`render_with_icons_scaled` for exactly this; the only difference between the two
callers is where the geometry came from.

### The creative signal

`Sim::has_infinite_materials` — `Abilities.instabuild` off `PLAYER_ABILITIES`,
the same field the anvil and enchanting screens already gate on. **Not**
`GameMode::Creative`: `ServerGameMode` is an ECS component with no shell reader,
and vanilla itself opens this screen off `player.hasInfiniteMaterials()` rather
than the game-mode enum. So `instabuild` is the right signal, not a stand-in —
with the same consequence vanilla has, that a server granting `instabuild` in
another mode gets this screen.

The screen **replaces** the player inventory screen rather than overlaying it
(`redraw` short-circuits its container block when `creative_screen_open()`), and
it never applies to a *server-opened* container: a chest is a chest in creative
too.

### Backgrounds are loose textures

The tab buttons and the scrollbar thumb are `gui/sprites/**`, so they ride
`ContainerBackground`'s atlas through `GUI_SPRITES` (see `all_gui_sprites`). The
three panel sheets are **not**: `textures/gui/container/creative_inventory/{tab_items,tab_item_search,tab_inventory}.png`
are loose art with no `.mcmeta` and no `GuiScaling`, exactly like
`container/inventory.png`. They get their own `ResourceLocation`s and
`ContainerBackground::creative_quad`, which blits the top-left 195×136 window of
a 256×256 sheet.

A jar-less run resolves none of it and falls back to flat fills, the same
degradation the ordinary container screen has.

## Three deliberate departures from vanilla

1. **Clicking a grid cell gives the item into the selected hotbar slot** instead
   of picking it up onto the cursor. The cursor stack lives on a real `Menu` and
   the creative grid has none, so the click sends
   `ClientAction::SetCreativeModeSlot { slot: 36 + selected, .. }` directly —
   one gesture instead of two, identical wire traffic. This is also the first
   shell producer that action has ever had; it was encoded by every protocol
   family and constructed by nothing (the `ClientAction::SetFlying` shape).
2. **The `hotbar` tab is empty.** Vanilla fills it from saved hotbars on disk
   (`HotbarManager`); this client has no store for them. The tab draws its
   background, strip and the live hotbar row like any other, and its grid is
   honestly blank.
3. **Clicks on the hotbar row and the inventory tab's slots are consumed and
   ignored.** Moving a stack between real inventory slots needs the cursor
   semantics point 1 explains away. They are consumed rather than passed through
   because the screen underneath is not the one on screen.

Search matches a **case-insensitive substring of the item id's path**, not the
localized display name, and tag queries (`#minecraft:logs`) are not modelled.

## Gotchas

- **Vanilla's own tab hit rect and tab blit rect disagree by 4 px.** `getTabY`
  returns `-32`/`+imageHeight`; the blit uses `-28`/`+imageHeight - 4`. The art
  tucks under the panel edge and that strip is deliberately not clickable.
  `CreativeLayout` carries both (`tabs` and `tab_hits`) for that reason — do not
  collapse them.
- **`aligned_right` is derived from the column**, not stored: in 26.2
  `alignedRight()` is called on exactly the four tabs in columns 5 and 6. Re-check
  it against a newer `CreativeModeTabs.java` if the strip grows an eighth column.
- **The scrollbar thumb is 15 px tall but travels `112 - 17`**, so it stops 2 px
  short of the track's bottom. Vanilla's arithmetic, kept literal.
- **`row_count` is `ceil(n/9) - 5`**, the number of rows the grid can scroll
  *past*, so a tab with 45 or fewer items reports zero and must not be divided
  by. A short final page keeps trailing empty cells rather than shrinking the
  grid.
- The layout is built from the same `(gui_scale, width, height)` triple in both
  the draw and the hit test, through one function. A layout built at a different
  scale than the frame was drawn with silently mis-resolves every click — the
  warning `container/layout.rs` already carries.
- **`CreativeLayout::grid` must be empty on the inventory tab, and this is now
  enforced at `creative_layout` rather than left to each consumer to
  remember.** The generic 45-cell item-picker grid sits on the same
  `GRID_X0`/`SLOT` pitch this panel reuses for the survival layout
  (`inventory_tab_slots`), so a populated `grid` geometrically overlaps the
  player's own armour/main/hotbar rects at that tab. The item-draw loop
  already skipped `grid` there (`kind != CreativeTabKind::Inventory`), but
  `creative_hit_test` checked it unconditionally and checked it *before*
  `layout.inventory` — so a cursor over the armour wells resolved to a
  mismatched `CreativeHit::Grid` cell instead of the real
  `CreativeHit::Inventory` slot, and the hover highlight was drawn at that
  wrong grid rect: a "slot" appearing where it should not, overlapping the
  armour area. Fixed by making `grid` itself empty on this tab (mirroring how
  `inventory`/`destroy` are already conditioned), so every consumer —
  present or future — inherits the fix rather than needing its own
  `kind != CreativeTabKind::Inventory` guard.

## Configuration

None of its own. It follows the global GUI scale (`MenuNav::gui_scale`) and the
language table for tab titles, with `en_us` fallbacks in
`app/creative_screen.rs::fallback_tab_title` for a pack-less run.

## Dependencies

- `container/creative_items.rs` — the item table.
- `container/{builder,background,geometry,renderer}.rs` — vertex streams, panel
  art, geometry type, draw passes.
- `lodestone-game` — `Menu` (the player's own inventory, for the hotbar row and
  the inventory tab).
- `lodestone-model` — `ClientAction::SetCreativeModeSlot`.
