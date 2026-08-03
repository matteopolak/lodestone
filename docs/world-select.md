# World select, with creation disabled

## What it is

`Screen::WorldSelect` — vanilla's `SelectWorldScreen`, reached from the title
screen's **Singleplayer** button. A title, a search field, a world list, and six
footer buttons: Play Selected World, Create New World, Edit, Delete, Re-Create,
Back. **Four of the six are present and disabled, Create New World among them**,
and the list holds **exactly one** world.

This is issue #397, the fifth child of the menu-framework epic #392, and the first
consumer of #394's `HeaderAndFooterLayout` — which landed as a knowing exception to
this repo's island rule precisely so this screen could be the thing that wires it
to pixels.

**Issue #287 then gave it a world and made Play Selected World live**, so this is
now the screen a real session starts from. The launch chain itself is documented in
[`singleplayer.md`](./singleplayer.md); what follows is the screen.

Two files:

- `crates/lodestone-shell/src/menu/world_select.rs` — the screen's *input* half:
  which widgets exist, which are active, where focus is, what a key or a click
  means.
- `crates/lodestone-shell/src/menu/render.rs` — the geometry (`world_select_slot`,
  `world_select_search_slot`, `world_list_row_rect`) and the draw
  (`frame_for`'s `Screen::WorldSelect` arm), beside the other two vanilla screens'.

## How it works

### The layout is vanilla's, arranged rather than tabulated

`world_select_layout(width, height)` builds
`HeaderAndFooterLayout(this, 8 + 9 + 8 + 20 + 4, 60)` — the literal from
`SelectWorldScreen.java:31` — and fills all three bands:

| band | vanilla | ours |
|---|---|---|
| header | `LinearLayout.vertical().spacing(4)`, children centred: a title `StringWidget`, then a nested `LinearLayout.horizontal().spacing(4)` holding the 200×20 search `EditBox` | same, with the title cell **zero-width** (below) |
| contents | the `WorldSelectionList`, sized to `layout.getContentHeight()` | a `SpacerElement` of the same size |
| footer | `GridLayout().columnSpacing(8).rowSpacing(4)` over a **4-column** `RowHelper`: Play (span 2), Create (span 2), then Edit / Delete / Re-Create / Back at `.width(71)` | same |

The arithmetic that falls out, hand-derived from the Java at 854×480 *before*
running the port and then found to agree with it exactly:

```
header column 200x33, centred in 854x49  -> (327, 8)
  title  -> text centre 427, top 8
  search -> (327, 21, 200, 20)      <- 21, not the 22 in the EditBox constructor
footer grid 308x44, centred in 854x60 pinned at y 420 -> (273, 428)
  columns [71, 71, 71, 71] + 3*8 = 308;  rows [20, 20] + 4 = 44
  Play (273,428,150,20)  Create (431,428,150,20)
  Edit (273,452,71,20)   Delete (352,452,71,20)
  ReCreate (431,452,71,20)  Back (510,452,71,20)
content band top = min(49 + 30, 480 - 60 - 371) = 49
list row 0 = (292, 51, 270, 36);  its content box = (294, 53, 266, 32)
```

Three of those are worth keeping:

- **The search box's declared `x`/`y` are dead.** `new EditBox(font, width/2 - 100,
  22, 200, 20, …)` (`:55`) is immediately overwritten by the header layout, which
  puts it at y **21**. Transcribing 22 would be off by one and look right.
- **The four footer columns are all 71 because *Play* says so.** Its 150 px spanning
  two columns with an 8 px gutter splits `Divisor(142, 2)` = 71/71; the four 71 px
  buttons can then only match it. So `.width(71)` on row 2 is not what sizes the
  grid — it is what stops row 2 *growing* it.
- **The content band starts flush under the header, not 30 px below it.**
  `HeaderAndFooterLayout` prefers `headerHeight + 30` and clamps *upward* to
  `height - footerHeight - contentHeight`; because vanilla sizes the list to
  `getContentHeight()` exactly, the clamp always wins and the answer is the header
  height. `Math.min` reads like a maximum until you remember y grows downward.

### `HeaderAndFooterLayout` needed no changes — but it is canvas-dependent

#394's port was complete and correct as it stood; nothing in `layout.rs` changed
for this screen. What did need thought is that this is the **first
canvas-dependent container** to reach a slot: it pins the footer to
`screen.height` and centres both bands in `screen.width`, so unlike
`title_block`/`pause_block` its arranged rects are not reusable at another size.

The resolution is that every rect *is* canvas-independent once expressed relative
to the right `Origin` — the header column is always 200 wide, the footer grid
always 308, the content band always starts at the header height. So the tree is
arranged once at `WORLD_SELECT_REF_CANVAS` (854×480) and the rects are converted to
`Origin::ScreenTop`/`Origin::ScreenBottom` offsets.

**That is a claim, so it is a gate**:
`the_world_select_slots_do_not_depend_on_the_reference_canvas` re-arranges at
320×240 (the real floor `calculate_gui_scale` can produce), 854×480 and 1920×1080
and requires every slot to be identical. Without it this screen would be correct at
one window size and wrong at every other, which no screenshot at the development
size would ever show.

### The title cell is deliberately zero-width

Vanilla's `StringWidget(title, font)` is `font.width(title)` wide; there is no font
at arrange time here. It does not matter, because the cell is
`alignHorizontallyCenter`ed in the 200 px column, so a `w`-wide title lands at
`colX + (200 - w) / 2` and its **centre** is `colX + 100` for every `w` under 200.
A zero-width cell puts the leaf rect exactly on that centre, and
`world_select_title_label` draws from it with `Align::Centre`. The arranged
position is therefore the real one rather than an approximation of it.

### What is disabled, and why

`active = false` is the whole mechanism (see [`menu-widgets.md`](./menu-widgets.md)).
Vanilla disables Play, Edit, Delete and Re-Create together whenever nothing is
selected (`updateButtonStatus(null)`, `SelectWorldScreen.java:159-166`), which is
where three of our four disabled buttons come from — but **not Play**: since #287
there is always a selection, so Play is active and Edit/Delete/Re-Create are off for
their own reasons (below).

**The one deviation is Create New World**, which vanilla leaves active: its press is
`CreateWorldScreen.openFresh` (`:87`), and `CreateWorldScreen` (828 lines) plus
`WorldCreationUiState` (326) are issue **#190**. `EditWorldScreen` (210) is the
same story for Edit, and Re-Create routes through `CreateWorldScreen` too. None of
them is ported, and naming them here is the whole of the stub.

Rendering Create greyed rather than omitting it is the point of the issue: a
missing row changes the footer grid's shape, so the screen would read as a
*different* screen rather than as vanilla with a feature unavailable.

`WorldSelectNav::update_button_status` is vanilla's method collapsed to a constant,
because the selection is one. Vanilla's non-null branch reads `primaryActionActive()`,
`canEdit()`, `canRecreate()`, `canDelete()` (`LevelSummary.java:189-211`, overridden
by `SymlinkLevelSummary`/`CorruptedLevelSummary` at `:273-347`) plus a
`requiresFileFixing()` tooltip. Only the first is ported, and as a constant `true`:
`BUNDLED_WORLD` is always the selection and is always playable, while the other three
ask about a *file* — there is none, and Edit/Delete/Re-Create have no screen to open
anyway. The rest stays unported rather than modelled-and-unused, because an enum
whose variants nothing constructs is the island `CLAUDE.md` names as this repo's
dominant defect. Those line numbers are the lookup for whoever adds world storage.

No tooltip either. `WidgetTooltipHolder` is what makes "disabled with an
explanation" honest and #393 deferred it to "whatever knows how long the cursor has
rested"; #395's input layer sees clicks and keys, not hover dwell time, so it is
still deferred. See [`menu-focus.md`](./menu-focus.md)'s deliberate-gaps list.

### The one row, and why vanilla has nothing to copy

There is still no singleplayer world *storage* in this client — no
`LevelStorageSource`, no save directory, no save format. What #287 added is the
**server**: `lodestone_server::IntegratedServer` runs in-process over an in-memory
duplex, so a world can be played without ever being written. The one row is
`BUNDLED_WORLD` — a fixed seed for the bundled overworld generator, regenerated
identically on every launch and never persisted. This module still ships **no
`LevelSummary` equivalent**; a fabricated *save* would read as a working one, and the
row's label says "generated, not saved" instead.

One row rather than a list because one is the honest count: with no storage there is
nothing to enumerate and a second row would have to be invented.

**Vanilla has no empty-list rendering for this screen**, which contradicts the
obvious guess. It is worth keeping even now that the list is not empty, because it is
why the one row looks the way it does.
`WorldSelectionList.handleNewLevels` (`WorldSelectionList.java:167-183`) switches on
the list type, and for `SINGLEPLAYER` an empty result calls
`CreateWorldScreen.openFresh` — it *leaves the screen*. `NoWorldsEntry`
(`:379-397`) exists but only the Realms `UPLOAD_WORLD` branch reaches it, and
`LoadingHeader` (`:354-377`) is only up while a `CompletableFuture` is pending.
Neither is a state a vanilla player sees on `SelectWorldScreen` with no worlds.

So the choice here is deliberate rather than transcribed: the screen draws
**`NoWorldsEntry`'s geometry** — one entry, a `StringWidget` centred in row 0's
content box — carrying `BUNDLED_WORLD.label`. `WorldListEntry`'s own geometry (a
32×32 icon plus three text lines off a `LevelSummary`, `:494-502`) is the one we
*cannot* draw, because there is no `LevelSummary` to fill it from.

The centring is exact rather than approximate, and the derivation is short enough to
keep: `contentXMiddle` is `rowLeft + 2 + 133` and `rowLeft` is `floor(w/2) - 135`,
so the halves cancel and the centre is the screen's own. The text's top is
`contentYMiddle - 4`, where the `4` is Java's `9 / 2` and not 4.5.

The label's **length** is a constraint: `NoWorldsEntry`'s `StringWidget` has no
`maxWidth`, so nothing clips it and a longer string would overhang the 266 px row —
44 characters at the jar-less fixed advance.
`the_world_list_row_label_fits_the_row_it_is_centred_in` pins that.

`AbstractSelectionList`'s scrolling and per-entry hit-testing are still **not**
ported. With exactly one world, that world is always the selection
(`WorldSelectNav::selected`), so nothing needs clicking to select — which is the one
deliberate deviation from vanilla in this screen's *behaviour*, and it is what makes
Play Selected World active. #396 is the issue that needs a real list, for the server
screen.

### Hover is not focus — the first screen where that matters

Every other menu screen has a single row cursor that both the keyboard and
`MenuNav::hover` move, so one flag carried both facts. Here they must be separate:
merging them would make *moving the mouse across the footer* steal keyboard focus
out of the search field, so typing would land nowhere.

So `MenuFrame` gained `hovered: Option<usize>`, `None` on every other screen (their
pixels are unchanged), and `draw_widget` sets `Widget::hovered` from it.
`MenuFrame::selected` is the **focused** row on this screen. Vanilla joins the two
only where the sprite is picked — `isHoveredOrFocused()` — and that `||` stays in
`Widget::is_hovered_or_focused`; the jar-less fallback fill now asks the same
question, so it cannot disagree with the sprite path about which button is lit.

### Input

Every key goes through `WorldSelectNav::handle_key`, i.e. vanilla's
`Screen.keyPressed` order: Escape, then the focused widget, then Tab and the arrows
as focus navigation, then the screen's own meaning. Consequences, all of them
vanilla's rather than rules written here:

- The search field consumes Backspace/Delete and the horizontal arrows and
  **declines** Up/Down and Tab (`EditBox.java:279-284`), so those reach traversal.
- Tab cycles between the only two active widgets — the field and Back — skipping
  the five inactive buttons, because `nextFocusPath` returns null for an inactive
  widget. The wrap is `clearFocus()`-then-retry, not `(i + 1) % n`; arrows do not
  wrap at all.
- Enter on a focused button is `AbstractButton.keyPressed` (`:61-71`). Our `Widget`
  is data with no press callback, so the screen applies it one layer up, after the
  focus layer declines. Same observable behaviour; **Space does not** press a
  button, because `app.rs` delivers it as `MenuKey::Char(' ')` and it goes into the
  field instead.
- `setInitialFocus(this.searchBox)` (`:147-152`) — the explicit overload, for
  `EditForm::adding`'s reason: the no-argument one is gated on a last-input-type
  this shell does not track, and without it the first keystroke would go nowhere.

**A click has its own arm** (`MenuNav::click`), which is the third screen to need
one. `MenuNav::click` translating a click into `hover` + `Enter` has caused two live
bugs on cursorless screens — #391's GUI-scale row toggling view bobbing, and
clicking a `ServerEdit` field submitting the form — and this screen has no row
cursor at all: a click on the field focuses it, a click on Back presses it, and a
click on one of the five disabled buttons does **nothing, including not moving
focus** (which is what would let the next Enter press it).

## What consumes it

The title screen's Singleplayer button. `MainButton::Singleplayer` now calls
`UiState::open_world_select()` instead of returning `MenuAction::Singleplayer`,
which is vanilla's own wiring — `TitleScreen` opens `SelectWorldScreen`; nothing in
vanilla starts a world straight off the title.

**One consequence to know about**: `app.rs`'s staged `begin_singleplayer` failure —
the honest "the integrated server is not wired yet" error screen — is no longer
reachable from the menu, because the only button that produced it now opens this
screen and Play Selected World is inactive. `MenuAction::Singleplayer` and
`app.rs`'s arm for it are kept because they are the seam #287 lands on; it becomes
reachable again the moment this list has a world.

## How to change it

- **Read the widget's `active`, not `WorldSelectButton::enabled()`.** The enum is the
  *initial* value `update_button_status` writes onto the widgets, exactly as vanilla
  computes one and assigns `button.active`. The flag on the widget is the live fact,
  and it is what the sprite and focus traversal already key on. This was a real
  defect and not a hypothetical one: `click_row` asked the enum, and
  `a_click_on_a_disabled_button_does_nothing_at_all`'s control — enable one button
  and watch the click land — is what caught it.
- **The row indices are the focus ids.** Row 0 is the search field and rows 1..6 are
  the footer buttons, which is what `app.rs`'s hit-test reports and what
  `FocusSet` dispatches on. Two tests hold the coupling —
  `the_world_select_rows_are_in_the_order_click_assumes` (nav) and
  `the_world_select_frame_is_the_screen_vanilla_draws` (render) — because reordering
  the `rows` vector in another file would otherwise rebind the mouse to the wrong
  control. That is #391's exact shape.
- **`WorldSelectNav` holds widget state, so it cannot be rebuilt per frame.** The
  search box owns a caret, a selection and a scroll offset; the frame carries a
  *clone* and `draw_edit_box` repositions the clone, which is
  `OptionsSubScreen`'s reposition-don't-rebuild order. See
  [`menu-focus.md`](./menu-focus.md) — the same shape as `EditForm`, including the
  reason `FocusSet` and its children live in different structs.
- **A slotted row can be a field.** `build`'s loop checks `MenuRow::edit` before
  falling through to `draw_widget`, because a text field is not a button with text
  in it: it has its own sprite set and its own predicate (`isFocused()` alone, not
  `isHoveredOrFocused()`).
- **Do not add a `LevelSummary` model until something constructs one.** See above.
- **Adding a widget means the leaf counts too.** `WorldSelectBlock::at` asserts the
  header has 2 leaves and the footer has `WORLD_SELECT_BUTTONS.len()`; a tree that
  no longer describes the screen fails loudly there instead of silently shifting
  every rect by one.

## How it is proved

- `world_select.rs`'s own tests: the five-disabled/one-enabled split with Create
  asserted present; the row-index ↔ focus-id mapping in both directions; every
  widget reachable through the `FocusChildren` seam with a control for an unknown
  id; the exact Tab sequence **with a control that enables Create and watches the
  walk reach it**, so "two stops" is not satisfied by a traversal that can only find
  two things; typing plus Up/Down leaving the field (which is also the premise
  assertion that the seeded bounds put Back below the box and overlapping it in x);
  Escape and Enter-from-Back; a click that focuses without pressing; a click on each
  disabled button doing nothing with the enabled-button control executed; and hover
  never moving focus.
- `render.rs`: `the_world_select_rects_are_vanillas_own` holds the hand-derived table
  above as the *expectation* while the values come out of the arranged tree — two
  independent derivations of the same arithmetic, which is the only shape of gate
  that catches a port that is self-consistently wrong.
  `the_world_select_slots_do_not_depend_on_the_reference_canvas` is what makes
  arranging a canvas-dependent container once legitimate.
- `every_world_select_button_draws_the_sprite_the_widget_layer_picks` is the
  anti-island gate, modelled on #393's: the expected sprite id comes from
  `WidgetSprites::get` and is never spelled out, the measurement is *which atlas
  region the frame's own UVs sample*, the destination bounding box is compared with
  the slot the layout placed the button in, and each of the twelve (button, focused)
  cases runs its own control — flipping `active` must move the sample. Its premises
  are checked too: the screen must still carry a mix of enabled and disabled
  buttons, and the six rects must be distinct.
- `a_disabled_world_select_label_lands_on_vanillas_grey` predicts `-6250336` rather
  than asserting "greyer", with the enabled label as the executed control.
- `the_world_list_draws_its_one_row_inside_row_zeros_content_rect` is the list
  assertion, so "the list has a world" is distinguishable from "the list failed to
  draw" — since #287 it is also the pixel half of Play Selected World being honest.
  It reports a **bounding box**, checks the extent is a line of
  text rather than a fill, and runs two controls: the row *below* must be empty, and
  the same band on the **title screen** must be empty — because per `CLAUDE.md` a
  control's premise can be false before the feature exists, and the question "what
  else already paints here?" has to be asked. (Answer: the logo reaches y 94, but it
  is on the *sprite* stream, and this measures colour.)
- `the_search_box_draws_as_a_field_inside_its_own_slot` uses the synthetic pack as
  the discriminator: it carries `widget/button*` and no `widget/text_field*`, so a
  field emits **no** sprite quads where a button emits nine. The control is the same
  row with its `EditBox` removed, watched emitting them.
- `hovering_a_world_select_button_lights_it_without_moving_focus` asserts both facts
  reach the draw, that a hovered *disabled* button still samples
  `widget/button_disabled` (`WidgetSprites`' three-argument collapse), and that the
  click hover would have preceded does nothing.

## Configuration

None of its own. Sprite art comes from the pack via
`resources::load_menu_gui_atlas()`; `gui_scale` (`config.rs`) sets the logical
canvas through `render::logical_canvas`.

## Dependencies

- `menu/widget.rs` — `Widget`, `WidgetSprites`, the disabled path.
- `menu/layout.rs` — `HeaderAndFooterLayout`, `LinearLayout`, `GridLayout`,
  `SpacerElement`, `Divisor`.
- `menu/focus.rs`, `menu/edit_box.rs` — the focus layer and the search field.
- `menu/render.rs` — the geometry, the frame and the draw.
- `menu/nav.rs`, `menu.rs` — the title-screen button, the key/click arms, the
  `Screen` variant.
- The 26.2 jar at `.cache/mc/26.2/client-src` — behavioural reference only.

## See also

- [Menu UI framework](./ui-framework.md) — the epic's plan of record, and the
  boundary this must not cross.
- [Menu widgets](./menu-widgets.md), [Menu layout containers](./menu-layout.md),
  [Menu focus and `EditBox`](./menu-focus.md) — #393, #394 and #395, whose
  machinery this screen is the consumer of.
- [Main menu](./main-menu.md) — the title screen this hangs off, and the server
  list beside it.
