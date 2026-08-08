# Advancements screen

## What it is

Vanilla's `AdvancementsScreen` (issue #167), reached from the pause menu's
Advancements button: five tabs, the real 26.2 advancement tree, connector lines,
frames, icons, a tiled per-tab background, panning, and a hover title. Built off
the data pack rather than the wire — **everything draws, nothing is obtained.**

## How it works

Three modules under `crates/lodestone-shell/src/menu/`, plus the wiring in
`app/`:

```
menu/advancement_data.rs   126 advancements over 5 roots, generated from the
                           26.2 data pack's own JSONs
        │
menu/advancement_tree.rs   layout_tree() — vanilla's TreeNodePosition, ported
        │                  (the tidy-tree algorithm; x = depth, y = tidy row)
menu/advancements.rs       advancements_layout  -> rects, one expression for
                                                   draw and click
                           advancements_hit_test -> AdvancementsHit
                           advancements_geometry -> ContainerGeometry
        │
app/advancements_screen.rs clicks, viewport pan, wheel, hover resolution,
                           advancements_panel_geometry()
app/lifecycle.rs           its own CursorMoved / MouseInput / MouseWheel arms
app/redraw.rs              ContainerRenderer::render_geometry_scaled, as an
                           overlay over the paused world
```

`Screen::Advancements` lives in `menu.rs`; the selected tab and per-tab scroll
live on `MenuNav` (`nav.advancements()` / `advancements_mut()`) beside every other
menu screen's state, and are reset on every entry from the pause menu — vanilla's
`AdvancementTab`s are rebuilt per screen, so its scroll does not survive a close
either.

### There is no `x`/`y` in the JSON, and this is the load-bearing fact

The issue's own trap says positions come from the datapack's `x`/`y`. **26.2's
advancement JSON has neither.** `DisplayInfo`'s `x`/`y` are computed server-side by
`net/minecraft/advancements/TreeNodePosition.java` and only ever appear on the
wire. So a client building this screen from disk has to run the algorithm itself,
and there is nothing on disk to verify a position against — which is why
`advancement_tree.rs` is a term-for-term port rather than a plausible auto-layout.
The only available correctness argument is that it *is* vanilla's procedure; the
tests assert the algorithm's own guarantees (one row of separation at every depth,
a parent inside its children's span) rather than literal coordinates.

### Why it draws through `ContainerRenderer`

Not through `menu::render::MenuFrame`. This screen is sprite-and-item-icon work at
arbitrary positions — item icons, a GUI sprite atlas, loose panel art, the dim
gradient — which is exactly what the container path already has and what the
row/label frame system does not. `ContainerRenderer::render_geometry_scaled` takes
a prebuilt `ContainerGeometry`; the creative inventory screen (#158) uses the same
seam.

Consequently `menu::render::owns_frame` deliberately **excludes**
`Screen::Advancements` and `frame_for` returns `None` for it. That `None` would
mean "invisible" without the block in `redraw`, which is the trap in-world Settings
and the command block editor were each caught by — the two halves must stay
together.

`nav::routes_menu_input` does include it, so Escape reaches
`UiState::on_escape` → `close_advancements`.

### Why nothing is obtained

Nothing in this workspace decodes `UPDATE_ADVANCEMENTS`:

- `crates/protocol/v770` carries the packet **id** and nothing else — no decode
  arm, no `ClientEvent` variant, nothing in `net.rs`'s `forward`.
- The integrated server *does* have a real `AdvancementManager` with per-player
  progress, and `server.rs` already calls the encode seam on join and on every
  dirty tick. But `ServerProtocol::encode_update_advancements`'s trait default is
  `ServerDirective::None` and `V770ServerProtocol` does not override it, so even
  singleplayer against our own server sends nothing.

So every widget draws `*_frame_unobtained`. This is the **true** state, not a
placeholder — a freshly created vanilla world's own screen looks the same — and
the same trade the Statistics screen made (#188).

## How to change it

**When the decode lands**, the shape to add is a progress source on
`AdvancementsState`: `obtained: HashSet<&'static str>` plus a completed-criteria
count per id. `advancement_frame_sprite(frame, obtained)` and
`progress_text(done, total)` already take those two arguments, and
`Advancement::requirement_count` is already the denominator, so the draw needs no
restructuring — only `draw_plan` passing the real flag instead of `false`, and the
title box gaining its progress line.

**To regenerate the data table**, walk the data pack's advancement JSONs again
(everything outside `recipes/` that has a `display` block). Verify every `icon`
against `lodestone-data`'s generated `ITEM_NAMES` and every translation key against
`client.jar`'s `en_us.json` before committing — both were checked with zero
mismatches for the current table, and a wrong icon degrades silently to an empty
frame while a wrong key draws the raw key.

## Gotchas

- **There is no scissor.** Vanilla scissors the 234×113 viewport;
  `render_geometry_scaled` cannot, so clipping is done on the CPU:
  `advancements_layout` drops any widget wholly outside the viewport and
  `draw_plan` clamps every connector segment and background tile into it (the
  tile's UV window is narrowed by the same fraction, which is what makes it look
  like a scissor). A widget straddling the edge therefore draws slightly past the
  frame. That is the one visible divergence.
- **Vanilla's own frame rect and hit rect disagree by 3 px.** The frame blits at
  `x + 3` and `isMouseOver` tests from `x`. Kept, not reconciled.
- **`AdvancementTabType.ABOVE`'s `max` is 8, not the number of tabs.** The
  "rightmost" tab sprite is chosen at index `max - 1`, so with five roots no tab
  ever draws the `right` variant. Vanilla's behaviour; it reads fine because the
  middle sprite is symmetric. The other three tab types (`BELOW`, `LEFT`, `RIGHT`)
  are unreachable at five roots and deliberately unported.
- **The per-tab scroll is centred lazily**, on first read — vanilla's `centered`
  latch. That is why even the *draw* path needs `&mut AdvancementsState`, and why
  `redraw` resolves the hover before it splits its field borrows.
- **The hover fade is not animated.** Vanilla ramps the viewport dim to `0.3` over
  a few ticks; this snaps to the ceiling, because the screen has no per-frame tick
  hook. Only the single-line title box is drawn — the multi-line description panel
  needs vanilla's `findOptimalLines` splitter, a text-layout job of its own.

## Configuration

None of its own. Follows the global GUI scale and the language table, with the
data pack's own `en_us` values as the fallback for a pack-less run.

## Dependencies

- `container/{builder,background,geometry,renderer}.rs` — vertex streams, the
  atlas that stitches `advancements/**` plus `window.png` and the five tile
  textures, the geometry type, the draw passes.
- `menu/nav.rs` — `PauseButton::Advancements` (now enabled) and the screen's
  persisted tab/scroll.
- `lodestone-data` — the outside source every icon id was verified against.
