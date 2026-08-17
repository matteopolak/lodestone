# Potion effects in the inventory

## What it is

`EffectsInInventory` (`26.2`): the column of active-effect chips drawn beside
the player's own inventory screen — name, amplifier level, remaining time and
an icon per active effect. Ported by reusing the existing top-right HUD chip's
fold/tint/font machinery in `crates/lodestone-shell/src/effects.rs`, rather
than building a second effect-rendering pipeline next to it.

## How it works

### Layout: real 26.2 constants, read from the decompiled source

- `ICON_SIZE = 18` (the tint-swatch icon's side length).
- The per-chip background is a fixed `32`px square when compact, wider when
  there is room for text.
- `x0 = leftPos + imageWidth + 2` — the column starts two pixels to the right
  of the container panel's own right edge, in the panel's local coordinate
  space.
- `available_width = screen_width - x0`; the column shows nothing at all when
  `available_width < 32` (`canSeeEffects`).
- `max_width = available_width >= 120 ? available_width - 7 : 32`
  (`extractRenderState`'s own branch).
- `y_step = 33` for five or fewer active effects; above that,
  `132 / (count - 1)`, so a crowded column still fits between the panel's top
  and bottom rather than overflowing past it.

### The stale claim this doc exists partly to correct

**`EffectsInInventory` does not reposition the container panel to make
room.** That behaviour belongs to the *older* `EffectRenderingInventoryScreen`
lineage, which some descriptions of "potion effects in the inventory" still
carry forward from an earlier Minecraft version. In the real 26.2 source,
`InventoryScreen`'s own `leftPos` comes from the ordinary centred (or
recipe-book-shifted) layout, completely untouched by whether any effect is
active — `EffectsInInventory` only ever decides whether there is *already*
enough free canvas beside the panel to draw into, never whether to make more.
This is exactly the kind of stale claim CLAUDE.md's evidence-standards
section exists to catch: it reads as a reasonable simplification right up
until you diff it against the real jar source, which is what settled it here.
The port implements the real (non-repositioning) behaviour, not the
described one.

### The draw

`effects::inventory_geometry` builds the column's quads: a translucent
background rect per chip (alpha `0.6` for an ambient effect, `0.85`
otherwise), an `18×18` tint swatch standing in for the icon, and up to two
lines of bitmap text (name, then remaining time in grey). `EffectsRenderer::
render_in_inventory` uploads and draws it in its own composited pass, called
from `redraw.rs` immediately after the container/recipe-book draw call, only
when the open menu's `MenuKind` is `Player` (a chest or furnace screen
resolves a different `MenuKind` and gets no column) and only when
`self.effects` exists.

The icon is a flat tint swatch, not the real `mob_effect/*` sprite — the same
disclosed simplification `effects.rs`'s top-right HUD chip already makes for
the same reason (no per-effect sprite atlas is loaded), extended here rather
than duplicated with a different look.

### Coordinate space

`panel_x`/`panel_y`/`panel_width` are the container panel's own `leftPos`/
`topPos`/`imageWidth` in the **logical** GUI canvas — the same space
`container::layout::panel_origin_with_scale` produces, and the same space
`geometry`'s top-right HUD chips already operate in. NDC is
resolution-independent, so handing `inventory_geometry` the logical canvas
(rather than the true framebuffer size) is what makes the column scale with
the container panel at any GUI scale or DPI, matching every slot and label
inside the panel itself. The recipe-book panel shift
(`recipe_book_panel_shift`) is folded into `panel_x` before this call, so an
open recipe book pushes the effect column right along with the panel it sits
beside.

## How to change it

- Layout constants live as named `const`s at the top of `effects.rs`
  (`INV_ICON_SIZE`, `INV_BACKGROUND`, `INV_Y_STEP`, `INV_CROWDED_SPAN`) —
  re-derive any of them from the real 26.2 `EffectsInInventory` source rather
  than guessing a round number; see CLAUDE.md's own warning about predicting
  plausible round numbers instead of re-deriving the arithmetic.
- To draw the real per-effect sprite instead of a tint swatch, both this
  column and the HUD's own top-right chips need the same new atlas entry —
  do it once, in `effects.rs`'s shared tint/icon resolution, not twice.
- If a future screen other than the player's own inventory should show this
  column (there is none in vanilla today), the gate to change is the
  `MenuKind::Player` check in `redraw.rs`, not anything in `effects.rs`
  itself — `inventory_geometry` takes a panel origin and width as plain
  arguments and has no opinion about which screen is open.

## Configuration

None — no flags or env vars gate this; it draws whenever the local player has
at least one active effect and there is room beside the panel.

## Dependencies

- `crates/lodestone-shell/src/effects.rs` — layout, geometry, and
  `EffectsRenderer::render_in_inventory`.
- `crates/lodestone-shell/src/app/redraw.rs` — the call site, gated on
  `MenuKind::Player`.
- `crate::container::{slot_layout, panel_origin_with_scale,
  recipe_book_panel_shift}` — the panel geometry this column positions itself
  relative to.
- `Sim::active_effects()` — the live effect set this column reads each frame.

## Verification

```bash
cargo test -p lodestone-shell --lib --no-fail-fast -- effects::
```

`effects.rs`'s own `inventory_geometry_covers_only_the_column_beside_the_panel`
test is the load-bearing one: it rasterises the emitted quads in software
(no GPU device needed) and asserts coverage lands *only* in the column beside
the panel, none over the panel itself. Watched with real numbers at the time
this landed: empty column = 0 px, two active effects = 1786 px in the column,
0 px leaked onto the panel — the control that proves the column never paints
over the slots it sits beside.
