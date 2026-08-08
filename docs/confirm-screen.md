# Confirmation screen

## What it is

`Screen::Confirm` — vanilla's `ConfirmScreen`: a question, a warning naming the
thing at risk, and two buttons. It is the gate an **irreversible** action passes
through, and the only thing that opens it today is the world list's Delete button
(issue [#540](https://github.com/matteopolak/lodestone/issues/540)).

Two files:

- `crates/lodestone-shell/src/menu/confirm.rs` — the arranged block, the
  placements, the input half (`ConfirmNav`) and the frame builder.
- `crates/lodestone-shell/src/menu.rs` — the `Screen::Confirm` variant and its two
  edges, `open_confirm`/`close_confirm`.

The filesystem half lives in `crates/lodestone-shell/src/saves.rs`
(`delete_world_in`) and is called from exactly one place,
`MenuNav::apply_confirm`'s affirmative arm.

## Why it is a screen and not a mode on the world list

This is the whole design, and it is worth reading before the mechanics.

`saves.rs` carried the argument for a release with no screen to discharge it:
**arming the Delete button and treating a second press of the same button as
confirmation is deletable-by-double-click.** A player who double-clicks, or whose
mouse chatters, loses a world — and for an operation with no undo that is worse
than no Delete at all. The cheap version is not "less good", it is unshippable.

What makes the real thing safe is **geometry plus focus**, not a timer:

- the affirmative control is a *different control on a different screen*, and its
  rect does not overlap the Delete button's. Vanilla centres this block in the
  canvas (`FrameLayout.centerInRectangle`, `ConfirmScreen.java:59-62`) while
  `SelectWorldScreen` pins Delete into a footer band, so at 854×480 the two are
  **177 px apart**. A second click where the first one landed hits nothing.
  `the_confirmation_cannot_be_fired_by_a_second_click_where_delete_was` asserts
  that as a rect relation, with both rects resolved through the same slot
  machinery the draw and `app.rs`'s hit-test read — a restated constant could be
  right while the drawn rect was not — and with the overlap detector itself shown
  to detect an overlap.
- **nothing is focused when the screen opens.** `ConfirmScreen.init` (`:45-56`)
  calls no `setInitialFocus`, unlike `SelectWorldScreen.java:147`, so Enter
  immediately after opening presses nothing. Reproducing that is both faithful
  *and* the safe direction: a held Enter cannot roll through the confirmation from
  the world list into a deletion.
  `enter_immediately_after_opening_the_confirmation_does_nothing` is the gate, with
  "one Tab, then Enter answers" as its control so it cannot pass for a screen whose
  Enter is simply dead.

## How it works

`confirm_rects(width, height)` builds vanilla's own tree —
`LinearLayout.vertical().spacing(8)` holding a title `StringWidget`, the message,
and a `LinearLayout.horizontal().spacing(4)` of two `Button.DEFAULT_WIDTH` buttons
with `paddingTop(16)` (`ConfirmScreen.java:47-53`) — arranges it, then
`align_in_rectangle`s it at `(0.5, 0.5)`. Every leaf is read back as a
`ConfirmPlacement`, which `Origin::Confirm` resolves.

The arithmetic that falls out, hand-derived from the Java at 854×480 *before*
running the port and then found to agree with it:

```
block width  = 2 * 150 + 4                     = 304   (the button row sets it;
                                                        both text cells are 0-wide)
block height = 9 + 8 + 9 + 8 + (16 + 20)       = 70
centred      -> ((854-304)/2, (480-70)/2)      = (275, 205)
buttons y    = 205 + 9 + 8 + 9 + 8 + 16        = 255
Delete (yes) = (275, 255, 150, 20)
Cancel (no)  = (429, 255, 150, 20)
```

`ConfirmNav` is the input half: two `Widget`s, a `FocusSet`, the title and message
strings, and a `ConfirmRequest` saying what is being confirmed. It holds no
filesystem anything — `MenuNav` owns the saves root, so `MenuNav::apply_confirm` is
what acts on a `ConfirmOutcome::Yes`, exactly as it is what acts on
`CreateWorldOutcome::Create`.

Escape is the **negative answer**, not a bare unwind: `ConfirmScreen.keyPressed`
(`:97-104`) runs `callback.accept(false)` and `shouldCloseOnEsc()` is `false`
precisely so that the callback runs. `MenuNav::key_confirm` therefore intercepts it
before `UiState::on_escape`; the `Screen::Confirm` arm in `on_escape` exists to keep
that match exhaustive and does the same observable thing (no world is deleted
either way).

Both answers `close_confirm` **and re-read the world list** —
`WorldSelectionList.deleteWorld`'s callback calls `returnToScreen()` outside its own
`if (result)` (`WorldSelectionList.java:624-631`) — so the screen the player lands
on reflects the disk rather than what was enumerated before the confirmation
opened. That matters even for a cancel: another process may have removed the world.

## Two deliberate deviations

**The message is one clipped line, not a `MultiLineTextWidget`.** Vanilla wraps it
to `width - 50` over up to 15 rows (`:67-69`), which makes the block's *height* —
and therefore the buttons' `y` — a function of the font. There is no font at arrange
time here (the same reason every title cell in this tree is zero-width), so a
wrap-dependent height would put the buttons somewhere the hit-test could not
predict. The block reserves one 9 px line instead, and `ConfirmNav::delete_world`
clips the **interpolated world name** — never the `will be lost forever!` half,
which is the part that says the action is irreversible — until the whole sentence
measures inside the block, using `render::text_px`, the same fixed advance the
jar-less draw measures with.

**No `setDelay`/`delayTicker`.** Vanilla's is not used by the world-delete path at
all: `WorldSelectionList.deleteWorld` (`:619-637`) builds a plain `ConfirmScreen`,
and `setDelay` exists for the backup/experimental-world confirmations. It also could
not work here — `frame_for` is a pure function of the UI state with no tick input —
so porting the field would be a constant claiming a delay happened.

## How to change it

- **A second kind of confirmation is a `ConfirmRequest` variant, not a second
  screen.** `MenuNav::apply_confirm`'s `match` on it is exhaustive, so opening a
  confirmation and forgetting to act on the affirmative answer is a compile error
  rather than a silently harmless Yes. The next two candidates are the world
  list's Edit (an `EditWorldScreen` reset) and Re-Create.
- **A second *entry point* also needs a return fork.** `close_confirm` always goes
  back to the world list, because that is the only screen that opens it — the same
  shape as `open_social_from_pause`. `UiState::settings_return` is the pattern to
  copy when there are two.
- **Do not give the block a variable height.** See the first deviation: the "cannot
  be double-clicked" property is a statement about two arranged rects, and a block
  whose height depends on the font makes the affirmative button's position
  unpredictable from outside the draw.
- **`selected` must be `usize::MAX` when nothing is focused**, not `0`.
  `draw_widget` feeds `MenuFrame::selected` to `Widget::focused`, and
  `isHoveredOrFocused()` would then draw the affirmative button as the one the
  keyboard is on.
- Vanilla's affirmative label here is **"Delete"** (`selectWorld.deleteButton`),
  not `gui.yes`. The wording is part of the safety: a button saying `Yes` answers a
  question the player may not have read.

## Configuration

None of its own. Sprite art comes from the pack via
`resources::load_menu_gui_atlas()`; `gui_scale` sets the logical canvas through
`render::logical_canvas`.

## Dependencies

- `menu/layout.rs` — `LinearLayout`, `LayoutSettings`, `align_in_rectangle`.
- `menu/widget.rs` — `Widget`, the button sprites.
- `menu/focus.rs` — `FocusSet` and the Tab/arrow traversal.
- `menu/render.rs` — `Origin::Confirm` and the frame model.
- `saves.rs` — `delete_world_in`, reached only through `MenuNav::apply_confirm`.
- The 26.2 jar at `.cache/mc/26.2/client-src` — behavioural reference only.

## See also

- [World select](./world-select.md) — the screen that opens this one, and the
  selection-model change that made a corrupt world deletable.
- [Menu widgets](./menu-widgets.md), [Menu layout containers](./menu-layout.md),
  [Menu focus and `EditBox`](./menu-focus.md).
