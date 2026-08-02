# Menu layout containers

## What it is

`crates/lodestone-shell/src/menu/layout.rs` — vanilla's
`net/minecraft/client/gui/layouts/` package as arithmetic: `GridLayout`,
`LinearLayout`, `FrameLayout`, `HeaderAndFooterLayout`, `SpacerElement`, the
`LayoutSettings` cell model and Mojang's `Divisor`. Its consumers are the title
screen's button column and the pause screen's grid, which since this change are
**built and arranged** rather than stored as tables of hand-derived offsets.

This is issue #394, the second child of the menu-framework epic #392. #393
([`menu-widgets.md`](./menu-widgets.md)) landed the leaf and the `LayoutElement`
seam; the plan of record for the rest is [`ui-framework.md`](./ui-framework.md).

## How it works

Three phases, and they are vanilla's:

1. **Build.** A container owns `Box<dyn LayoutElement>` children, each wrapped
   with a `LayoutSettings` — four paddings and an alignment per axis in
   `0.0..=1.0`.
2. **Arrange.** One `arrange_elements()` call walks the tree *bottom-up* (nested
   containers size themselves first, or every one of them would measure 0×0),
   sizes each container from its children, and writes absolute positions into the
   leaves.
3. **Visit.** `visit_widgets` hands the leaves to the screen. This is the only
   route from a tree to a draw, which is why a `SpacerElement` — whose
   `visit_widgets` is a no-op — takes part in every measurement and is never
   drawn.

### The alignment model is padding-aware

`AbstractChildWrapper::setX` (`AbstractLayout.java:73-78`) is the whole of it:

```text
offset = lerp(xAlignment, paddingLeft, availableSpace - child.width - paddingRight)
```

Not `(available - width) / 2`. With `paddingLeft = 10`, `paddingRight = 0`, a
20 px child in 100 px and `xAlignment = 0.5`, vanilla gives **45** where both
naive readings give 40.

`setY` is the same expression with **`Math.round` where `setX` truncates**
(`:83` vs `:76`). The asymmetry is real and worth one pixel: a 20 px child
centred in a 25 px cell lands at `x = 2, y = 3`. It is reproduced, not tidied —
and note `Math.round(float)` is `floor(v + 0.5)`, which is *not* Rust's
`f32::round` (they disagree at every negative half, and a layout sees negatives
as soon as a child is wider than its cell).

### `GridLayout` is the only algorithm

- **Row and column counts are derived**, never declared: `max(lastOccupiedRow)` /
  `max(lastOccupiedColumn)` over the children (`GridLayout.java:27-33`). A child
  at row 3 in an otherwise empty grid creates rows 0..3 with heights
  `[0, 0, 0, h]`.
- **A spanning cell splits its size with a `Divisor`**, Mojang's Bresenham-style
  integer splitter: 7 over 3 is `2, 2, 3`, and the parts always sum back, so a
  span is never a pixel wider than the columns it covers. The share is `max`ed
  into each row/column, so a span only *grows* one that is smaller
  (`:43`, `:50`).
- **`RowHelper` wraps** when the next span would overflow the row, abandoning the
  rest of the current row and jumping the index to the next row boundary with
  `Mth.roundToward` — so a 1-wide child added after a wrapped 2-wide one lands on
  the row *after* it, not beside it. Every helper child spans exactly one row.
- **`LinearLayout` wraps a one-row or one-column `GridLayout`** and delegates
  `arrangeElements` entirely (`LinearLayout.java:56-59`). `spacing()` maps to
  `columnSpacing` when horizontal and `rowSpacing` when vertical; setting the
  other one is a silent no-op on a single-row grid.
- **`FrameLayout`'s children default to `align(0.5, 0.5)`** (`FrameLayout.java:14`)
  while `GridLayout`'s and `EqualSpacingLayout`'s default to top-left. Its size is
  `max(minWidth/minHeight, largest padded child)` and every child is aligned
  independently in the *whole* box.
- **`HeaderAndFooterLayout`** pins the header at `(0, 0)` sized
  `screen.width × headerHeight`, the footer at `y = screen.height - footerHeight`,
  and the content at `min(headerHeight + 30, screen.height - footerHeight -
  contentHeight)` — it *prefers* a 30 px gap and clamps **upward** so content can
  never overlap the footer. `Math.min` reads like a maximum until you remember y
  grows downward.

### Which two-phase timing this follows, and why

Vanilla has two, and they are not interchangeable:

| screen | order |
|---|---|
| `PauseScreen.createPauseMenu` (`:180-182`) | build → `arrangeElements()` → `FrameLayout.alignInRectangle` → `visitWidgets` |
| `OptionsSubScreen.init` (`:28-34`) | build → `visitWidgets` → `repositionElements()` → `arrangeElements()` |

The second exists so a **resize repositions existing widgets** instead of
rebuilding them, which matters when a widget holds state (focus, a scroll offset,
an `EditBox`'s text).

This port follows **`PauseScreen`'s**, for three reasons:

1. It is what the screen being converted actually does.
2. There is nothing to reposition. `frame_for` and `pause_frame` rebuild every
   `MenuRow` — including a fresh `label.to_string()` — every frame, so no widget
   survives a frame, let alone a resize.
3. The resize problem is already solved better, by accident of `Slot`. Arranging
   is canvas-**independent**: only the final `alignInRectangle` depends on the
   screen size, and that is exactly what `Origin::PauseGrid` applies at draw time.
   So the tree is arranged **once per process** (a `OnceLock`) and a resize costs
   nothing at all — no rebuild *and* no re-arrange.

When #395 gives widgets persistent focus, `OptionsSubScreen`'s order becomes the
right one for screens that own state, and this note is the reason the choice was
not arbitrary.

**#395 landed and that is what happened.** `Screen::ServerEdit`'s two `EditBox`es
hold a caret, a selection and a scroll offset, so they cannot be rebuilt per frame;
`render::build`'s `draw_edit_box` therefore *repositions* them — on a per-frame
clone, because `frame_for` takes `&MenuNav` — instead of arranging a fresh tree.
See [`menu-focus.md`](./menu-focus.md).

## What consumes it

`menu/render.rs`:

- **`title_menu_column()`** — a vertical `LinearLayout` (spacing 4) of three
  200×20 rows, a nested centred horizontal row of three 20×20 icon buttons, and a
  nested horizontal row of two 98×20 buttons.
- **`pause_menu_grid_with()`** — vanilla's `PauseScreen.createPauseMenu` as a real
  two-column `GridLayout` with `padding(4, 4, 4, 0)` as the live cell baseline, a
  204-wide spanning first cell carrying `paddingTop(50)`, the Advancements/Statistics
  pair, a nested `alignHorizontallyCenter` icon row, and the full-width Options and
  Disconnect rows.
- `title_slot` / `pause_slot` read the arranged leaves by index; `Origin::PauseGrid`
  applies `align_in_dimension` to `pause_grid_size()` — the grid's own measured
  size — instead of restating `212`×`166`.

**Pixels did not move.** That is the whole proof, and it is asserted rather than
eyeballed: `the_title_screen_rects_are_vanillas_own` and
`the_pause_screen_rects_are_vanillas_own` already held the two hand-derived rect
tables, so what used to be `pause_slot`'s *implementation* is now its
*expectation*. Two independent derivations of the same arithmetic — one by hand
from the Java, one by running a port of it — which is the only shape of gate that
can catch a port that is self-consistently wrong.

### The title screen is a re-expression, not a port

**Vanilla's `TitleScreen` uses no layout class at all** — it hand-centres on
`this.width / 2 - 100` and steps `topPos` by 24 — and #392's plan is explicit
that a hand-arithmetic screen is legitimate vanilla. Do not read
`title_menu_column` as evidence otherwise.

What makes the re-expression faithful is that the two are *numerically identical*,
and the reasons are worth writing down because they are not coincidences:

- `spacing = 24` on 20 px buttons is a 4 px `rowSpacing`, so rows land on
  `0, 24, 48, 72, 96` either way.
- The column's width is `max(200, 68, 200) = 200`, so centring it on `width / 2`
  *is* `width / 2 - 100`.
- `getHorizontalPosition(n, 3, 20)` is `width/2 - 34 + (n-1)*24`
  (`TitleScreen.java:170-173`); a 68 px row centred in the 200 px column sits at
  `lerp(0.5, 0, 132) = 66`, and `100 - 66 == 34`. The 34 is `totalWidth / 2` and
  the 66 is `(200 - 68) / 2`.
- `98 + 4 + 98 == 200`, so the half-width pair fills the column exactly and its
  children are at `+0` and `+102`.

If that equality ever stops holding, the rect table fails and names the button.

## How to change it

- **Read the `arrangeElements` body, not a screen that uses it.** `CLAUDE.md`'s
  record-vs-summary trap; `PauseScreen` alone cannot tell you what `xAlignment`
  does, because with `padding(4, 4, 4, 0)` a full-width cell's `leastOffset` and
  `mostOffset` are *both* 4 and alignment cannot move it.
- **Every offset is an integer.** The internals are `i32` and the `f32`
  `LayoutElement` seam converts at the boundary (`layout::ipx`). Vanilla's
  layouts are `int`-only and the truncations are load-bearing.
- **`default_cell_setting()` is the live baseline; `new_cell_settings()` is a
  copy of it.** Mutating the former changes what every *subsequent* cell
  inherits. Ours are `Copy`, so a child snapshots its settings when added; vanilla
  can also alias the live object into a cell, and a late mutation there would move
  already-added children. That is the one deliberate deviation, and it is
  unobservable across the whole client — but only for a reason narrower than the
  obvious one, so it was grepped rather than assumed (the first version of this
  note claimed something false):
  - The aliasing has exactly one path: `RowHelper`'s short `addChild` forms, which
    pass `defaultCellSetting()` itself (`GridLayout.java:202`). Every other
    `addChild` passes a `copy()`.
  - Of the ~75 `defaultCellSetting`/`defaultChildLayoutSetting` call sites in the
    client, the only one on a `RowHelper` is `RealmsResetWorldScreen.java:158`,
    and it runs before that helper's first add.
  - One screen *does* mutate a live baseline mid-build: `DisconnectedScreen` sets
    `padding(10)`, adds the title and reason, then sets `padding(2)` for the
    buttons after (`:44-47`). It is a `LinearLayout`, whose `addChild` copies, so
    its first two children keep padding 10 in vanilla exactly as they do here.
  So "no screen mutates the baseline after an add" is the **wrong** claim, and
  "no screen mutates a baseline that was aliased into a cell" is the right one.
- **`add_child` returns nothing**, where vanilla hands the child back so the
  screen can keep a reference *and* the layout can own it. Read results back with
  `visit_widgets`, whose order is insertion order in vanilla too. This is the one
  place the ownership model forces a different shape; if a screen ever needs to
  reach in and set `active` on one widget, that wants an index handle, not a
  `Rc<RefCell<_>>`.
- **`visit_widgets` is required on `LayoutElement`, `arrange_elements` is
  defaulted.** Vanilla makes `visitWidgets` abstract — which is why
  `SpacerElement` writes an explicit empty body — and a defaulted no-op would let
  a future element type silently never reach a screen. `arrange_elements` is
  defaulted to a no-op because vanilla's equivalent is the `child instanceof
  Layout` test in `Layout.arrangeElements`, and a leaf's arrange is a no-op either
  way.
- **`HeaderAndFooterLayout` now has a screen consumer, and it needed no changes.**
  It landed here with none — a knowing exception to this repo's island rule — and
  #397's `Screen::WorldSelect` is what closed that: `world_select_layout` builds
  vanilla's `SelectWorldScreen(this, 8+9+8+20+4, 60)` with all three bands and its
  rects reach pixels. See [`world-select.md`](./world-select.md). One thing that
  port surfaced and this note did not say: this is the **only canvas-dependent
  container here** (it pins the footer to `screen.height` and centres both bands in
  `screen.width`), so unlike the title column and the pause grid its arranged rects
  are not reusable at another size — a consumer must either re-arrange per canvas
  or, as #397 does, convert them to `Origin`-relative offsets *and gate that the
  offsets really are canvas-invariant*. It is still true that you should not point
  it at `Screen::Settings` or `Screen::ServerList` as they stand (that is #55/#396's
  work): those use this shell's own centred row stack, so converting them is a
  deliberate *visual* change.
- **`EqualSpacingLayout` and `CommonLayouts` are not ported.** One user each in
  the whole client tree; porting them now would be an island.
- **Do not arrange a container screen with any of this.** Zero of `screens/
  inventory/`'s 59 files reference a layout class; slot geometry comes from the
  *menu* classes. See #392's boundary section.

## How it is proved

- **`layout.rs`'s own tests** are per-mechanism, each with the vanilla line cited
  and, where an absence is claimed, a control: padding-aware alignment (with both
  naive answers asserted *not* to occur), the `setX`/`setY` rounding asymmetry, the
  `FrameLayout`-vs-`GridLayout` alignment defaults, derived grid dimensions,
  `Divisor`'s exact sequences plus a sum-preservation sweep, span growth only where
  a column is smaller, `LinearLayout` in both orientations (with an unspaced
  control), `RowHelper`'s wrap, the bottom-up recursion (asserting the nested layout
  measures 0 *before* arranging), translation, `align_in_dimension`'s truncation,
  both branches of `HeaderAndFooterLayout`'s clamp, a spacer that occupies space and
  reaches no widget list, and the live-vs-copy cell settings.
- **`the_pause_grid_size_is_the_arranged_layouts_own`** compares the arranged
  grid's measured size with the hand-derived `PAUSE_GRID_W`×`PAUSE_GRID_H`. Those
  constants stay hand-derived on purpose: an expected value must originate outside
  the code under test.
- **`a_changed_cell_padding_moves_every_pause_rect`** is #394's negative control,
  executed rather than described: `pause_menu_grid_with` is called with
  `MENU_PADDING_TOP - 10` and every one of the nine rects must move up 10 px, with
  the grid's own height shrinking by 10 and nothing moving horizontally. An arrange
  pass that silently no-opped fails all of it.
- **`every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks`** (#393's
  anti-island gate) is extended rather than duplicated: each of the 18 buttons is now
  drawn at its **own** slot, and the sprite stream's destination bounding box is
  compared with the rect the layout placed it in. It also asserts the 18 positions are
  distinct, so "drew in the right place" cannot be satisfied by everything landing in
  one place. That is the assertion that says the containers reach pixels.
- `tests/menu_button_pixels.rs` is unchanged and still measures vanilla's real art at
  vanilla's rects through `title_slot`, on a GPU with the real jar.

## Configuration

None of its own. The canvas the arranged block is aligned in comes from
`render::logical_canvas` (and therefore `gui_scale` in `config.rs`).

## Dependencies

- `menu/widget.rs` — `Widget` and the `LayoutElement` seam (which this change gave
  `visit_widgets` and `arrange_elements`).
- `menu/render.rs` — the two tree builders, `Slot`/`Origin`, and the draw.
- The 26.2 jar at `.cache/mc/26.2/client-src` — behavioural reference only.

## See also

- [Menu UI framework](./ui-framework.md) — the epic's plan of record.
- [Menu widgets](./menu-widgets.md) — the leaf, the disabled path, and the three
  things the written record had wrong about `WidgetSprites`.
- [Main menu](./main-menu.md), [Pause menu](./pause-menu.md) — the two converted
  screens.
