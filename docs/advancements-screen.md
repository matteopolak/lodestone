# Advancements screen

## What it is

Vanilla's `AdvancementsScreen` (issue #167), reached from the pause menu's
Advancements button: five tabs, the real 26.2 advancement tree, connector lines,
frames, icons, a tiled per-tab background, panning, and the full hover tooltip.
The tree *shape* comes off the data pack; the *progress* comes off the wire, so
completed advancements really do draw their obtained frames.

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

### Where the progress comes from, and which direction the join runs

`UPDATE_ADVANCEMENTS` decodes into `ClientEvent::AdvancementsUpdated`, which
`route` sends to **`session`** (`lodestone_ecs::session::apply_advancements` →
`SessionAdvancements`). There is deliberately **no `net.rs` `forward` arm** — the
event is `shell: false`, so adding one would put a second writer on state that
already has one. `Sim::advancements()` clones the store and
`AdvancementProgress::from_store` reduces it to a per-id snapshot.

**The store carries no positions, so the join is one-directional.** Ids are looked
up *from* `ADVANCEMENTS` *into* the store, never the reverse: rebuilding the forest
from the store's own `parent` links would produce a second, unpositioned tree that
disagrees with the one being drawn. `AdvancementProgress::from_store` iterates
`ADVANCEMENTS` for exactly that reason.

The snapshot is refreshed every frame while the screen is open and at 4 Hz
otherwise (`PROGRESS_POLL`), because cloning a 126-entry store of owned criterion
names sixty times a second buys nothing a player can see.

An empty store draws what it drew before the wire landed: everything unobtained,
no readouts. That is the true state of a fresh world.

### What the progress reaches

| surface | rule |
|---|---|
| frame sprite | `*_frame_obtained` when `AdvancementProgress::isDone` |
| `x/y` readout | completed **requirement groups** over declared groups — an AND-of-ORs count, *not* obtained criteria |
| hidden widgets | `!isHidden() \|\| isDone()`, via `is_visible`, consulted by both the layout and `draw_plan` so a widget and its connector agree |
| hover title bar | split into an obtained and an unobtained half at the progress fraction |
| completion toast | `AdvancementToastQueue` → `HudFrame::advancement_toast` |

### The toast's seed is the whole design

Vanilla fires `AdvancementToast` from `onUpdateAdvancementProgress`, which the
server only calls on a change. We see a *snapshot*, and the join packet's `reset`
batch carries everything already earned — so a naive "obtained now, not last
frame" test fires sixty toasts at once on entering a long-played world.
`AdvancementToastQueue::observe` adopts the first non-empty observation silently
and only toasts later transitions.

## How to change it

**A new progress-derived surface** goes through `AdvancementProgress`, not
through a second read of `SessionAdvancements`: one snapshot per frame is what
keeps the layout, the draw and the toast from disagreeing about the same id.

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
- **The fade is ticked from `advancements_hover`**, not from a screen tick hook —
  that is the one per-frame call which already knows whether anything is hovered,
  so ticking it anywhere else would walk the layout twice. Framerate-dependent, as
  vanilla's own `+0.06`/`-0.12` per frame is.
- **`box_obtained`/`box_unobtained`/`title_box` are nine-slice sprites and the
  container atlas stretches them.** `ContainerBackground::sprite_quad` samples the
  whole sprite; the HUD's `GuiAtlas` honours `.mcmeta` nine-slicing but the
  container path does not. The tooltip's borders are therefore slightly stretched.
  Fixing it means teaching the container atlas nine-slicing, which the tab sprites
  would want too.
- **The toast's multi-line branch is not modelled.** Vanilla alternates heading and
  wrapped title every 1500 ms when the title exceeds 125 px; all 126 shipped titles
  fit, so the alternation is unreachable with the vanilla data pack and a longer
  title degrades to its first line.

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
