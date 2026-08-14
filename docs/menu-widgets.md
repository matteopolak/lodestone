# Menu widgets, and the disabled render path

## What it is

`crates/lodestone-shell/src/menu/widget.rs` — one type, `Widget`, that owns a menu
control's bounds, message and state (`active` / `visible` / `focused`) and answers
the two questions each screen used to answer for itself: **which sprite is my
background** and **what colour is my label**. With it comes `WidgetSprites`,
vanilla's four-state sprite record, and therefore the **disabled render path** —
the thing the settings tree needs so an unsupported option can be present
and greyed out rather than absent.

This is issue #393, the first child of the menu-framework epic #392. The plan of
record for the rest is [`ui-framework.md`](./ui-framework.md); this doc is the
part that exists.

## How it works

### `active = false` is the whole disabled API

There is **no disabled widget type** in vanilla, and inventing one is the mistake
this port exists to prevent. Setting `active = false` does exactly three things:

| effect | vanilla | ours |
|---|---|---|
| the background sprite changes | `WidgetSprites.get(active, hoveredOrFocused)` | `Widget::background_sprite` |
| the label goes grey `-6250336` | `AbstractWidget.WithInactiveMessage` | `Widget::message_colour` |
| keyboard focus skips it | `nextFocusPath` returns `null` when `!isActive()` | `Widget::takes_focus`, and `MenuNav`'s `step_enabled` |

The third is the one that is easy to miss: an inactive widget is **unreachable by
keyboard**, not merely unclickable. `MenuNav::key_main`/`key_paused` already
implement that half for the two converted screens — see
[`main-menu.md`](./main-menu.md).

Vanilla disables its own controls for exactly our reason, which is why these are
patterns to copy rather than design: the narrator button
(`OptionsSubScreen`'s constructor), the anisotropy slider
(`VideoSettingsScreen.tick`), Multiplayer for a banned account
(`TitleScreen.createNormalMenuOptions`) and telemetry (`OptionsScreen`'s constructor). The last two
carry a tooltip saying *why*, which is what makes "unsupported, greyed out" read
as honest rather than broken.

### `WidgetSprites` and its collapsing constructors

`WidgetSprites` is a four-field record — `enabled`, `disabled`, `enabled_focused`,
`disabled_focused` — with three collapsing constructors lifted from vanilla's
`WidgetSprites` record itself. The collapse is the interesting part: it is how a widget
declares it has *no* disabled art without needing a second type.

| vanilla arity | ours | result |
|---|---|---|
| 1 | `WidgetSprites::uniform` | all four the same |
| 2 | `WidgetSprites::focusable` | `(sprite, sprite, focused, focused)` — `EditBox`'s form |
| 3 | `WidgetSprites::with_disabled` | `(enabled, disabled, focused, disabled)` — `AbstractButton`'s form |

Read the fourth field of the 3-argument form again: `disabled_focused` is the
**disabled** sprite. That is why a greyed-out button under the cursor still looks
greyed out, and it is the single rule a hand-rolled highlight is most likely to get
wrong.

`BUTTON_SPRITES` is `AbstractButton.SPRITES`:
`widget/button`, `widget/button_disabled`, `widget/button_highlighted` through the
3-argument form.

### Who consumes it

`menu/render.rs`'s `draw_widget` — the sole draw path for every row carrying a
`MenuRow::slot`, which is the title screen, the pause menu, the death screen and
the account screen's action row. It builds a `Widget` from the row's
`enabled`/`selected` and then *asks* it; no sprite id and no label colour is
decided in `render.rs` any more.

The rect still comes from `row_rect`, deliberately: that function is also
`app.rs`'s hit-test, so it stays the one definition of where a row is. `Widget`
implements the `LayoutElement` seam (read size, write position) that the layout
work's containers attach through — see [`menu-layout.md`](./menu-layout.md), which added
`visit_widgets` to that trait once there was a container to walk, and which feeds
the title and pause rects into `row_rect` from an arranged tree rather than a
table.

**Pixels did not move.** That is the point — the conversion is a de-duplication,
not a visual change, and `tests/menu_button_pixels.rs` is unchanged and still
measures vanilla's real art at vanilla's rects.

## How to change it

- **Add state to `Widget`, not to a screen.** The reason the type exists is that
  the fourth screen must not write the blit a fourth time.
- **Do not build disabled art for `Checkbox`, `EditBox` or
  `AbstractSliderButton`.** Verified against the 26.2 jar rather than taken on
  trust: `Checkbox.java` and `AbstractSliderButton.java` never mention
  `WidgetSprites` and pick between a plain and a `_highlighted` sprite by hand
  (`Checkbox.extractContents`, `AbstractSliderButton.getSprite`/`AbstractSliderButton.getHandleSprite`);
  `EditBox.SPRITES`
  uses the **two**-argument constructor, which collapses `disabled` onto
  `enabled`. All three rely on the grey label plus blocked input. Construct them
  with `Widget::new` (no sprites), not `Widget::button`.
- **`button_disabled`'s nine-slice border is 1 where its siblings' are 3.**
  Measured directly against the atlas. Nothing in `widget.rs` encodes a border; `GuiAtlas` reads each
  from the sibling `.png.mcmeta`. A hardcoded rect would silently sample the wrong
  slice — which is also why the gates resolve UV windows through `GuiAtlas` at test
  time instead of restating coordinates.
- **The 89 `container/*.png` textures are invisible to `GuiAtlas` and that is
  correct.** It globs `gui/sprites/**`; `load_gui_atlas`'s own doc comment (`crates/lodestone-shell/src/resources.rs`) documents the gap and
  `container.rs::background::ContainerBackground` works around it on purpose, because vanilla blits
  hand-placed sub-rects of those 256×256 sheets and `GuiScaling` has no variant for
  an arbitrary sub-rect. Do not widen the glob.

### Three things the jar says that the written record got wrong

Found while porting, all three by reading the class rather than a summary of it —
`CLAUDE.md` rule 2. Each was plausible and each would have shipped something
subtly wrong.

1. **The sprite's second argument is `isHoveredOrFocused()`, not `isFocused()`.**
   Both the original issue's body and `ui-framework.md` say focused.
   `AbstractButton.extractDefaultSprite` passes
   `SPRITES.get(this.active, … this.isHoveredOrFocused())`,
   and that is `isHovered() || isFocused()`
   (`AbstractWidget.isHoveredOrFocused`). Our single row cursor is moved by *both* the
   keyboard and `MenuNav::hover`, so one flag is the faithful model and the shipped
   behaviour was already right — but a later change that splits hover from focus must join
   them with an `||` here, not drop one.

   **A later change did split them**, and this is now the state of it: `Widget::hovered` is
   its own field, `Widget::focused` is keyboard focus alone, and
   `Widget::is_hovered_or_focused` is the `||`. Two things follow —
   `Widget::takes_focus` reads `focused` only (hover is not focus, so hovering a
   widget must not make Tab skip it), and `EditBox` does **not** share the
   predicate: `EditBox.extractWidgetRenderState` passes `isFocused()`, so hovering a text field
   draws its plain sprite. See [`menu-focus.md`](./menu-focus.md).
2. **The two `get` arguments are not the same predicate.** `AbstractButton` passes
   the raw `active` field; `EditBox.extractWidgetRenderState` passes `isActive()` (i.e.
   `visible && active`). `WithInactiveMessage.getMessage()` also keys on `active`
   (`AbstractWidget.WithInactiveMessage.getMessage`). `Widget::background_sprite` and
   `Widget::message_colour` follow the button, because an invisible widget is not
   drawn at all (`AbstractWidget.extractRenderState`) so the distinction is unobservable
   for it.
3. **`EditBox` does use `WidgetSprites`.** "No disabled sprite at all for
   `Checkbox`, `EditBox`, `AbstractSliderButton`" is true about the *art* and
   misleading about the mechanism — `EditBox` routes through the record, just with
   the 2-argument collapse. Only `Checkbox` and `AbstractSliderButton` bypass it
   entirely.

### Deferred on purpose

**No tooltip.** `WidgetTooltipHolder` is what makes "disabled with an explanation"
honest, and it belongs on this type eventually — but nothing in this shell draws a
hover tooltip, so a `tooltip` field would reach zero pixels. It lands with the
screen-level input layer, which is what knows how long the cursor has
rested. Adding the field now would be an island; `CLAUDE.md`'s dominant defect
class, sixteen confirmed.

**That input layer landed and it is still deferred**, for the same reason narrowed: that layer
turned out to see clicks and keypresses, not hover *dwell time*, so it does not
know how long the cursor has rested either. See
[`menu-focus.md`](./menu-focus.md)'s deliberate-gaps list.

## How it is proved

- `menu/widget.rs`'s own tests cover the record (all four states of a set with
  four *distinct* members, so `get` cannot be silently ignoring an argument),
  the defaults (`active`/`visible` true — a `derive(Default)` would grey out every
  widget built from `..Default::default()`), and the focus/click gating.
- **The grey is derived, not transcribed.** `-6250336` is lifted verbatim from
  `AbstractWidget.WithInactiveMessage.defaultInactiveMessage` and `argb_to_rgba` must unpack it onto
  `INACTIVE_LABEL`, with a *different* vanilla colour (`EditBox`'s
  `DEFAULT_TEXT_COLOR`, `-2039584`) as the control. Without that the array would
  only ever agree with itself, which is the `decode(encode(x)) == x` trap.
- **`every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks`**
  (`menu/render.rs`) is the anti-island gate. It walks all nine title and nine
  pause buttons at their real `enabled()` values, both focused and not, and asserts
  which *atlas region* the frame's own UVs sample — with the expected id produced by
  `WidgetSprites::get`, never spelled out. Each case runs its own control: flipping
  `active` must move the sample off that region. Its premise is checked too (both
  screens must still carry a mix of enabled and disabled buttons, or the gate is
  vacuous).
- `tests/menu_button_pixels.rs` remains the end-to-end pixel gate, on a GPU with
  the real jar: the bevel discriminates real art from a flat fill (4.29 vs 1.0
  measured in a *linear* readback), `widget/button_disabled` is both ~2.5× darker
  *and* bevel-free, a hovered disabled button must still show a black border where
  the highlighted sprite's is white, and `detach_gui` is the executed negative
  control.

## Configuration

None of its own. Sprite art comes from the pack via
`resources::load_menu_gui_atlas()`; `gui_scale` (`config.rs`) sets the logical
canvas the bounds are in, through `render::logical_canvas`.

## Dependencies

- Nothing beyond `core` for the type itself — it is pure data and arithmetic.
- `menu/render.rs` resolves the sprite ids through `lodestone_render::GuiAtlas`,
  which applies the `.mcmeta` nine-slice from `lodestone_assets::gui`.
- `menu/nav.rs` owns which buttons are `enabled()` and the keyboard step-over.

## See also

- [Menu UI framework](./ui-framework.md) — the epic's plan, the settings census,
  and the HUD/container boundary this must not cross.
- [Main menu](./main-menu.md), [Pause menu](./pause-menu.md) — the two converted
  screens.
