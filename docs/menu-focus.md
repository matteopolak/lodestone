# Screen focus, tab traversal, event dispatch — and `EditBox`

## What it is

Two modules, the third child of the menu-framework epic (#392/#395):

- `crates/lodestone-shell/src/menu/focus.rs` — vanilla's screen input layer:
  `GuiEventListener` / `ContainerEventHandler` dispatch, `ComponentPath`,
  `FocusNavigationEvent`, the `gui/navigation/Screen{Axis,Direction,Position,Rectangle}`
  geometry arrow navigation compares, and `Screen.keyPressed`'s ordering.
- `crates/lodestone-shell/src/menu/edit_box.rs` — `EditBox`: a single-line text
  field with a caret, a selection, horizontal scrolling and a length cap. The
  first widget that genuinely needs the above.

Their consumer is `Screen::ServerEdit`'s name and address fields, **converted**
rather than added alongside: `menu::nav::EditForm` now holds two real `EditBox`es
and a `FocusSet` instead of two `String`s and a `FormField` enum.

`docs/ui-framework.md` remains the epic's plan of record; corrections to it live
here. [`menu-widgets.md`](./menu-widgets.md) (#393) is the leaf, and
[`menu-layout.md`](./menu-layout.md) (#394) places leaves.

## How it works

### The two orderings that are the whole point

**1. `Screen.keyPressed`: Escape, then the focused child, *then* navigation**
(`Screen.java:120-150`).

```text
1. event.isEscape() && shouldCloseOnEsc()  -> onClose(), return true
2. super.keyPressed(event)                 -> the FOCUSED CHILD ONLY
3. only if that returned false:            -> 258 Tab / 262..265 arrows
                                              become focus navigation
```

Step 2 is `ContainerEventHandler.keyPressed`, which is
`getFocused() != null && getFocused().keyPressed(event)` — it **never iterates
children**. `FocusSet::screen_key_pressed` is this, transcribed.

The consequence is the one that makes an `EditBox` drop into an arrow-navigated
screen without fighting it: `EditBox.keyPressed` handles 262/263 (Left/Right) and
268/269 (Home/End) and **declines** 264/265 (Down/Up) — `EditBox.java:279-284`
lists them in the `default:` group. So horizontal arrows move the caret and never
reach step 3; vertical arrows fall through and move focus. **There is no rule
anywhere saying "a text field swallows arrows"; it falls out of the ordering.**

**2. Tab does not wrap in `handleTabNavigation` — the wrap is in `Screen`.**
This contradicts the obvious reading. `ContainerEventHandler`'s tab walk runs off
the end of the sorted child list and returns `null`; there is no modular
arithmetic anywhere in it. The wrap is a **retry** one layer up
(`Screen.java:139-143`):

```java
ComponentPath focusPath = super.nextFocusPath(navigationEvent);
if (focusPath == null && navigationEvent instanceof TabNavigation) {
    this.clearFocus();                                 // forget where we were
    focusPath = super.nextFocusPath(navigationEvent);   // restart from the end
}
```

Two things a `(i + 1) % n` gets wrong:

- **Arrow navigation does not wrap at all.** The retry is gated on
  `instanceof TabNavigation`. Arrow off the edge and focus stays put.
- **The wrap clears focus first**, so a widget that refuses focus when already
  focused (every `Widget` — `takes_focus` is `isActive() && !isFocused()`) becomes
  eligible again. On a one-focusable-child screen, Tab therefore re-lands on the
  same child rather than doing nothing.

### Tab order is insertion order until something says otherwise

`handleTabNavigation` sorts `children()` by `getTabOrderGroup()`
(`TabOrderedElement.java:4-6`, default `0`) with `Collections.sort`, which is
**stable**. So an all-default screen tabs in the order widgets were added.
`FocusSet::next_focus_path` uses `slice::sort_by_key`, Rust's stable sort. Since
nothing in this shell overrides the group yet, the shipped behaviour cannot prove
the sort is stable — `tab_order_group_overrides_insertion_order_but_ties_keep_it`
does, with a hand-written group.

### Arrow navigation is geometric, and it is two passes

`nextFocusPathInDirection` (`ContainerEventHandler.java:186-233`):

1. **Strict.** Keep children that overlap the focused rect *in the orthogonal
   axis* and lie after it along the travel axis; sort by leading edge, then by the
   orthogonal negative edge. Column-to-column, row-to-row.
2. **Vague** (`nextFocusPathVaguelyInDirection`, `:235-269`). If nothing
   qualified, drop the overlap requirement and take the nearest by **squared
   distance** between the focused rect's leading-edge centre and each candidate's
   trailing-edge centre.

Ship only the strict pass and focus dies at the end of a column instead of hopping
to the next one — which reads as "Tab works, arrows are broken".

`ScreenRectangle::bound_in_direction` is **inclusive** on the positive side
(`position + length - 1`), and every comparison in the strict pass is against that
value. A port using `x + width` there makes abutting widgets look overlapped.

### The three registries

| added with | drawn | gets events | narrated |
|---|---|---|---|
| `addRenderableWidget` | yes | yes | yes |
| `addWidget` | **no** | yes | yes |
| `addRenderableOnly` | yes | **no** | no |

Dispatch reads `children()`, which only `addWidget` appends to (and
`addRenderableWidget` through it). A widget in the wrong list is unit-testable,
correct, registered and **never clickable** — or invisible — with nothing failing
loudly. `Registry` is an explicit enum for that reason, and
`registries_are_not_interchangeable` asserts a render-only widget receives no
click, with the interactive registration as its executed control.

`getChildAt` is **first match in `children()` order**, not topmost by z
(`:28-36`). Two overlapping widgets means the older one wins every click, forever;
vanilla simply does not overlap them.

### Identity is an index, not a reference

Vanilla's `ComponentPath` holds the components and `applyFocus` walks *them*. Rust
cannot hold a back-reference into a screen's storage while mutating it, so:

- A caller hands its children to `FocusSet` through the `FocusChildren` trait, an
  `id -> &dyn FocusTarget` lookup. Every path is a path of **ids**.
- `FocusTarget::current_focus_path` has to be *told* its own id to build a
  `ComponentPath::Leaf`.
- `applyFocus`'s recursion lives on `FocusSet` / `FocusTarget::apply_focus`
  rather than on the path.
- A path returned by `FocusSet` is **relative to it**: vanilla wraps every
  returned path as `ComponentPath.path(this, child)`, and here the `FocusSet` *is*
  `this`, so the head of a returned path is already a child id.

`ComponentPath::Path` exists but nothing nests yet; #396's scrolling list is what
will construct one.

### `EditBox`

Ported field for field. The parts worth knowing:

- `maxLength` defaults to **32**, `DEFAULT_TEXT_COLOR` is `-2039584`,
  `textColorUneditable` is `-9408400`, and the uneditable colour is keyed on
  **`isEditable`, not `active`** (`EditBox.java:411`). A field can be active
  (clickable, focusable) and uneditable.
- `insertText`'s budget is `maxLength - value.length() - (start - end)` with
  `start <= end`, so the third term *adds* the selection length back. Reading it as
  a subtraction makes a full field impossible to overtype.
- `scrollTo` computes `lastPos` from the **old** `displayPos`, may then change
  `displayPos`, and still compares against the stale `lastPos`. Reproduced, not
  corrected.
- `getWordPosition`'s forward walk lands *past* the run of spaces after the word;
  the backward walk skips trailing spaces first.
- A live selection beats a word: `deleteWords` deletes the selection instead
  (`:172-176`), so Ctrl+Backspace over a selection does not also eat the word
  before it.

## How to change it

- **`textX`/`textY` are methods, not cached fields.** Vanilla caches them and
  calls `updateTextPosition()` from six places (`setX`, `setY`, `setValue`,
  `setBordered`, `setCentered`, `onValueChange`); a seventh that forgets draws at a
  stale offset. Computing on demand deletes the bug class.
- **A `false` from `EditBox::handle_key` is load-bearing.** It is what lets
  Up/Down out to focus navigation. Consuming a key "to be safe" breaks Tab
  traversal in a way no unit test of `edit_box.rs` can see.
- **The edit-shortcut modifier is Cmd on macOS, not Ctrl.**
  `InputQuirks.EDIT_SHORTCUT_KEY_MODIFIER` is `8` (SUPER) on OSX and `2` (CONTROL)
  elsewhere, and *every* `isCut`/`isCopy`/`isPaste`/`isSelectAll` goes through it
  (`InputWithModifiers.java:66-84`). Hardcoding Ctrl ships a client where Cmd+V
  does nothing on the platform this repo is developed on.
- **`FocusSet` and the children it dispatches to must live in *different*
  structs.** `FocusSet`'s methods take `&mut dyn FocusChildren`; a `FocusSet` in
  the same struct as its children could not be called at all, because
  `&mut self.focus` and `&mut self` are not disjoint. That is why `EditForm` has a
  `FormFields` sub-struct. Vanilla has the same shape (a `Screen` holds both) and
  no such rule.
- **`FocusSet::default` is hand-written**, like `Widget`'s: a derived one would
  give `close_on_esc = false` and every screen built from `..Default::default()`
  would silently swallow Escape.

## Widgets now outlive a frame — the one architectural change

Every other menu screen rebuilds its rows, labels included, in
`render::frame_for`, every frame. That is fine for a button, whose whole state is
derivable, and **impossible for a text field**: rebuilding one resets the caret,
the selection and the scroll offset sixty times a second. Vanilla has exactly this
consequence — `Screen.rebuildWidgets` calls `clearFocus()`
(`Screen.java:342-347`), so a rebuilt screen has no focus by construction.

So `EditForm`'s two `EditBox`es are the first *widget* state in the shell rather
than derived state, and #394's note that `OptionsSubScreen`'s
build→reposition order "becomes the right one once a widget holds state" is where
it lands.

**What it cost.** `render::frame_for` takes `&MenuNav`, so it cannot reposition the
originals. Instead:

- each `MenuRow` carries a **clone** of the live box (`MenuRow::edit`);
- `render::build`'s `draw_edit_box` moves the clone into that frame's rect and
  then *asks* it for the text, caret and selection — `repositionElements`' order,
  applied to a copy;
- the originals are seeded once, in `EditForm::adding`, from
  `render::field_row_rects(SEED_CANVAS)` — the same `row_rect` the draw uses, not
  a restated width.

The seed matters for two things and only two: the **relative y order** of the
fields (arrow navigation is geometric, so both boxes at `(0, 0)` would make
Up/Down silently stop working) and the **width** `displayPos` scrolls against.
`row_rect` centres the stack vertically and clamps to `ROW_W`, so the seed is
correct at every canvas that is not pathologically narrow.

A `&mut MenuNav` per frame would let the originals be repositioned in place and
delete the clone. That is one signature change in `app.rs`, which this change does
not own.

## Deliberate gaps, each with the reason

- **Left/Right/Home/End and every modifier are implemented and unreachable.**
  `app.rs::menus::WindowApp::menu_key_for` translates exactly seven winit keys into `MenuKey`: Up, Down,
  Enter, Escape, Tab, Backspace, Delete. `EditBox::handle_key` handles the rest and
  is unit-tested on them; four `KeyCode` arms in `app.rs` would light them up, with
  no change needed in `focus.rs` or `edit_box.rs`. Adding `MenuKey` variants
  pre-emptively would break the exhaustive `match key` in every other `key_*` arm
  for no gain.
- **A click is routed as a row index, not a position.** `MenuNav::click` /
  `hover` take a `usize`, so a click focuses the right *field* but cannot place the
  caret at the clicked character. `EditForm::click_at` and
  `FocusSet::mouse_clicked` implement the real thing and are tested; they need an
  `app.rs` that passes logical `(x, y)`.
- **No clipboard.** `isCopy`/`isCut`/`isPaste` return `true` in vanilla *and*
  touch `Minecraft.keyboardHandler`. There is no clipboard seam here, so
  `handle_key` declines all three rather than consuming a keystroke it cannot
  honour — in particular **Ctrl+X does not delete**, so nothing is lost to a
  clipboard that was never written. Select-all needs no clipboard and works.
- **The caret does not blink.** `TextCursorUtils.isCursorVisible` is a 300 ms
  interval on `Util.getMillis() - focusedTime`, and no `MenuFrame` carries a clock.
  `edit_box::is_cursor_visible` is the pure predicate; `EditBox::show_cursor(None)`
  means "always on", which is what the shell passes and what the pre-#395 form
  caret already did.
- **No narration.** `Registry` records the `narratables` membership for fidelity;
  nothing in this shell speaks, so a `NarratableEntry` port would reach zero pixels
  and zero audio.
- **No tooltip.** #393 deferred `WidgetTooltipHolder` to "whatever knows how long
  the cursor has rested". Nothing here tracks that either — `FocusSet` sees clicks
  and keys, not hover dwell time — so it is still deferred.

## Measurement is a fixed advance

`displayPos`, the visible substring and the caret's x all depend on
`Font.plainSubstrByWidth`/`Font.width` — a **proportional** measure. `EditBox` has
no `Font`: it is pure data, and threading one through `FocusTarget::key_pressed`
would put the renderer inside the input layer. It carries an `advance` instead,
defaulting to `MENU_TEXT_ADVANCE = 12.0`.

**12, not vanilla's 6**, and that is the point: `render` draws menu body text at
`TEXT_SCALE = 2.0`, so a character occupies 12 logical pixels of the row it is
measured against. `displayPos` exists to keep the caret inside the *drawn* rect, so
the number that matters is the one the draw uses. A box measuring at 6 while
drawing at 12 would scroll half a field too late.

`render::text_px` and `render::clip` already make the same fixed-advance
approximation for every other menu string (only `clip_measured` consults the real
font), so this is not a new class of error — it means a long value can differ from
vanilla by a glyph at the right edge.

## Indices are `char`s, not UTF-16 code units

Every position in vanilla is a Java `String` index. Ours are `char` indices, so
`Util.offsetByCodepoints` becomes a plain `±1`, `insertText`'s high-surrogate guard
has nothing to split, and **`maxLength` counts an astral character as one, not
two**. Nothing in this shell sends an `EditBox` value to a server as a
length-capped protocol field, so the difference is unobservable — but it would
matter for a sign- or book-editing screen.

## Two corrections to the written record

Both were confidently held and are false against the 26.2 jar.

1. **`docs/ui-framework.md` and #393 say the sprite predicate is `isFocused()`;
   for a *button* it is `isHoveredOrFocused()`.** #393 measured this and the
   shipped behaviour was already right, because the shell's single row cursor
   carried both facts in one flag. **#395 splits them**: `Widget::hovered` is now
   its own field, `Widget::focused` is keyboard focus alone, and
   `Widget::is_hovered_or_focused` is the join. Keep it an `||` — dropping either
   side compiles, passes every existing test that sets only `focused`, and changes
   how every button in the client highlights.

2. **`EditBox` does not share that predicate.** `EditBox.java:407` is
   `SPRITES.get(this.isActive(), this.isFocused())` where
   `AbstractButton.java:43-53` is
   `SPRITES.get(this.active, this.isHoveredOrFocused())`. **Both** arguments
   differ: `isActive()` (`visible && active`) rather than the raw field, and
   **`isFocused()` alone** — hovering a text field does *not* draw its highlighted
   sprite. Applying #393's "join them with `||`" correction here would be wrong.
   `the_sprite_is_the_two_argument_collapse_and_keys_on_focus_alone` asserts both,
   with a button under the same flags as its control.

`EditBox` does route through `WidgetSprites`, via the two-argument collapse
(`widget/text_field`, `widget/text_field_highlighted`) — so #393's record of the
mechanism was already right, and only `Checkbox` and `AbstractSliderButton` bypass
it.

## Behaviour that deliberately changed on `Screen::ServerEdit`

The old `key_edit` was a flat `match key`. Every key now goes through
`EditForm::handle_key`, i.e. vanilla's order, and four things move:

| key | before | now |
|---|---|---|
| Backspace | dropped the **last** character | deletes before the caret |
| Delete | did nothing | deletes forward at the caret |
| Tab | toggled the field | real tab traversal (and its wrap) |
| Up / Down | toggled the field (wrapping) | geometric focus navigation, **no wrap** |

The last is the only one a user could call a regression, and it is vanilla: Down
from the bottom field stays put, and Tab is what cycles.

## How it is proved

- **`focus.rs`'s own tests** (26): the exact Tab sequence including the wrap, the
  `tabOrderGroup` sort with insertion-order ties, an inactive child skipped with the
  re-enabled walk as its control, arrow navigation asserted to be *geometric* with
  the Tab order from the same layout as its control, the vague pass reached with a
  premise assertion that the strict pass cannot be what found it, arrows proved not
  to wrap where Tab does, `getChildAt`'s first-match with the reversed
  registration order as its control, the three registries with the interactive
  registration watched flipping every assertion, and Escape proved to be answered
  *before* the focused child.
- **`edit_box.rs`'s own tests** (14): the caret through typing, arrows, Home/End,
  Backspace/Delete and word-wise motion; the selection replaced by what comes next;
  the full-selection insert budget; `displayPos` scrolling in a 4-character-wide
  box; `textY`'s Java integer truncation; and the sprite/colour predicates with
  their controls.
- **`nav.rs`'s screen-level tests**: the exact focused-field sequence with the
  wrap; the focused field swallowing Backspace/Delete/Left while declining Up/Down/
  Tab; the field ids proved to be the row indices `app.rs` reports (the same guard
  shape as `the_settings_rows_are_in_the_order_click_assumes`, and the same #391 bug
  it protects); and the seed geometry proved to be the layout the draw uses,
  including the premise arrow navigation rests on (the address field is strictly
  below the name field, in the same column).
- **`render.rs`'s pixel gate**,
  `the_edit_box_draws_its_text_and_its_caret_inside_its_own_rect`: on the real
  screen, through `frame_for` + `geometry`, with every bound derived from the widget
  and none restated. Its control is executed — an empty focused field paints only
  its caret, and its *vertical extent* is asserted to be a bar rather than a line of
  glyphs, so "the band is in the right place" is a checked premise and not an
  assumption. Then typing must paint >8× as many vertices in the band, the leftmost
  pixel must be the box's own `text_x` (a draw using the row's `PAD` of 6 instead of
  `BORDER_INSET` of 4 fails here), the rightmost must be the caret's right edge from
  the box's own `draw_state`, and one Backspace must retreat that edge by **about
  one advance** — not by nothing (a frozen caret) and not by the whole field (a
  re-laid-out one). Failure output is a bounding box, never a fraction.
- The measuring window is strict on `y` and inclusive on `x`, and the asymmetry is
  the point: the field's background and its outline's left/right edges sit
  `BORDER_INSET` outside the band horizontally, so an inclusive `x` still excludes
  them while keeping the caret's own left edge; its outline's **bottom** edge,
  though, lands *inside* the band vertically while spanning the full field width, so
  only a strict `y` keeps it out. An inclusive one would report a box the width of
  the whole field whatever the value was — a control that fires while measuring
  something unrelated.

## Configuration

None of its own. Sprite art comes from the pack through
`resources::load_menu_gui_atlas()`; `gui_scale` (`config.rs`) sets the logical
canvas via `render::logical_canvas`. `EDIT_SHORTCUT_MODIFIER` resolves from
`target_os` at compile time.

## Dependencies

- `focus.rs`: `menu::widget` (for `Widget`'s `FocusTarget` impl) and
  `menu::layout::ipx` (the `f32` → integer-pixel boundary). Nothing else.
- `edit_box.rs`: `menu::widget` and `menu::focus`.
- `menu::nav::EditForm` and `menu::render`'s `draw_edit_box` are the consumers.

## See also

- [Menu UI framework](./ui-framework.md) — the epic's plan of record.
- [Menu widgets](./menu-widgets.md) — the leaf (#393), and the `hovered`/`focused`
  field this change split.
- [Menu layout containers](./menu-layout.md) — the containers (#394), and the note
  that predicted this switch of two-phase timing.
- [Main menu](./main-menu.md) — the server list screen this form belongs to.
