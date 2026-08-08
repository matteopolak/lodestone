# World select, and the singleplayer save list

## What it is

`Screen::WorldSelect` — vanilla's `SelectWorldScreen`, reached from the title
screen's **Singleplayer** button. A title, a search field, a world list, and six
footer buttons: Play Selected World, Create New World, Edit, Delete, Re-Create,
Back. The list holds **one row per world in `saves/`**, and **three of the six
buttons are present and disabled**: Edit and Re-Create have no screen to open, and
Delete has no confirmation step.

This is issue #397, the fifth child of the menu-framework epic #392, and the first
consumer of #394's `HeaderAndFooterLayout` — which landed as a knowing exception to
this repo's island rule precisely so this screen could be the thing that wires it
to pixels.

**Issue #287 then gave it a world and made Play Selected World live**, and issue
#468's second reading turned its one hardcoded row into a real save list, so this
is now the screen a real session starts from and the screen that creates one. The
launch chain is documented in [`singleplayer.md`](./singleplayer.md) and the
directory layout in [`world-save-load.md`](./world-save-load.md); what follows is
the screen.

Three files:

- `crates/lodestone-shell/src/saves.rs` — this client's `LevelStorageSource`:
  enumeration, `WorldSummary` (vanilla's `LevelSummary`), name sanitising and
  creation. **Read its module doc before changing anything here**; it is the spec.
- `crates/lodestone-shell/src/menu/world_select.rs` — the screen's *input* half:
  which widgets exist, which are active, where focus is, what a key or a click
  means.
- `crates/lodestone-shell/src/menu/render.rs` — the geometry (`world_select_slot`,
  `world_select_search_slot`, `world_list_row_rect`) and the draw
  (`frame_for`'s `Screen::WorldSelect` arm, plus `draw_world_entry`), beside the
  other two vanilla screens'.

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
selected (`updateButtonStatus(null)`, `SelectWorldScreen.java:159-166`), and this
screen now really does reach that state — an empty `saves/`, or a search matching
nothing — so all four go grey together exactly as vanilla's do. With a playable
selection, Play comes back and the other three stay off for their own reasons: no
`EditWorldScreen` (210 lines), no re-create flow, and no confirmation step for a
Delete that cannot be undone.

**Create New World was the one deviation** — vanilla leaves it active and #397 left
it greyed, pending `CreateWorldScreen` (828 lines) plus `WorldCreationUiState`
(326). Issue **#190** built it and this button has been live since.

Rendering a still-unavailable button greyed rather than omitting it is the point of
the issue: a missing row changes the footer grid's shape, so the screen would read
as a *different* screen rather than as vanilla with a feature unavailable.

`WorldSelectNav::update_button_status` **used to be vanilla's method collapsed to a
constant**, because the selection was one. Since the save list it asks a real
summary. Vanilla's non-null branch reads `primaryActionActive()`, `canEdit()`,
`canRecreate()`, `canDelete()` (`LevelSummary.java:189-211`, overridden by
`SymlinkLevelSummary`/`CorruptedLevelSummary` at `:273-347`) plus a
`requiresFileFixing()` tooltip; all four are now ported onto
`saves::WorldSummary` and `&&`-ed with `WorldSelectButton::enabled()`, which is the
*client-level ceiling*:

| button | ceiling | per-selection |
|---|---|---|
| Play Selected World | live | `can_play()` — `false` for a world whose `level.dat` will not decode |
| Create New World | live | always |
| Edit | **never** | no `EditWorldScreen` (210 lines) is ported |
| Delete | **never** | no confirmation step exists — see `saves.rs` on why a cheap one was rejected |
| Re-Create | **never** | routes through `CreateWorldScreen` |
| Back | live | always |

So a greyed Play now means "nothing selected" — an empty `saves/`, or a search that
matches nothing — which is a state vanilla's `updateButtonStatus(null)` exists for
and this screen previously could not reach. The tooltip stays unported (below).

The three `WorldSummary` predicates that answer "not disabled" in vanilla —
`locked`, `requiresManualConversion`, `isCompatible` — are deliberately **not**
modelled: this client writes no `session.lock` and has no `DataFixer`, so each
would be a constant claiming a check happened. `readable` is the one real failure
mode, and it maps to `CorruptedLevelSummary`.

No tooltip either. `WidgetTooltipHolder` is what makes "disabled with an
explanation" honest and #393 deferred it to "whatever knows how long the cursor has
rested"; #395's input layer sees clicks and keys, not hover dwell time, so it is
still deferred. See [`menu-focus.md`](./menu-focus.md)'s deliberate-gaps list.

### The list: N rows, `WorldListEntry`'s geometry

The rows come from `saves::list_worlds_in`, one `MenuRow` each, in
`saves::WorldSummary::cmp_for_list` order — `LevelSummary.compareTo` verbatim, i.e.
last played **descending**, ties broken by folder name ascending. Each carries
`WorldListEntry`'s three text lines (`WorldSelectionList.java:555-570`):

| line | y inside the content box | content | colour |
|---|---|---|---|
| 1 | `+1` | the display name (`LevelName`, or the folder when it is empty) | white |
| 2 | `+9+3` | `folder (YYYY-MM-DD HH:MM UTC)` | `-8355712` grey |
| 3 | `+9+9+3` | `Survival, Cheats, Version: 26.2` | `-8355712` grey |

all inset by `getTextX() = getContentX() + 32 + 3`. **The 32 px icon column is
reserved and drawn empty**: vanilla blits `FaviconTexture.forWorld` from the
`icon.png` the client writes on quit, and this client writes none, so there is
nothing to blit — and the column still has to exist, because all three lines' x is
measured from its far edge. Drawing a placeholder square would be inventing a
texture vanilla has no equivalent of.

The date format is a **deliberate deviation**: vanilla uses
`Util.localizedDateFormatter(FormatStyle.SHORT)`, i.e. the user's locale and the
system time zone, and this shell has neither a locale table nor a tz database.
Guessing a locale format would be wrong in a way nobody could test; an ISO
timestamp labelled `UTC` says what it is. `saves::format_epoch_millis_utc` is
`civil_from_days`, gated against values produced by Python's own `datetime`.

Two more deviations, both worth knowing before "fixing" them:

**The empty list does not leave the screen.** `WorldSelectionList.handleNewLevels`
(`WorldSelectionList.java:167-183`) switches on the list type, and for
`SINGLEPLAYER` an empty result calls `CreateWorldScreen.openFresh` — real vanilla
*replaces* the world list with the creation screen when you have no worlds.
`NoWorldsEntry` (`:379-397`) exists but only the Realms `UPLOAD_WORLD` branch
reaches it. This shell draws `NoWorldsEntry` anyway, with
`world_select::NO_WORLDS_LABEL`, for two reasons: opening a different screen from a
screen's *first frame* makes the world list unreachable on a fresh install, and
Escape from that creation screen would return the player to a screen they never
saw.

`NoWorldsEntry`'s centring is exact rather than approximate, and the derivation is
short enough to keep: `contentXMiddle` is `rowLeft + 2 + 133` and `rowLeft` is
`floor(w/2) - 135`, so the halves cancel and the centre is the screen's own. The
text's top is `contentYMiddle - 4`, where the `4` is Java's `9 / 2` and not 4.5.
Its **length** is a constraint: the `StringWidget` has no `maxWidth`, so nothing
clips it and a longer string would overhang the 266 px row — 44 characters at the
jar-less fixed advance. `the_world_list_row_label_fits_the_row_it_is_centred_in`
pins that. A world *row*'s three lines are clipped, matching
`StringWidget.setMaxWidth` (`:418`), and
`a_long_world_name_is_clipped_to_its_row_rather_than_overhanging_it` is the gate.

**The list does not scroll.** `AbstractSelectionList`'s scroll model is not ported
here — #396 ported it for the *server* list, and this screen has no equivalent yet.
So `world_list_row_visible` rejects any row that would fall outside the content
band, and `row_rect` then reports **no rect at all** for it: not drawn, and not
clickable, which is the same gate `server_row_visible` applies and for the same
reason (`row_rect` is also `app.rs`'s hit-test). `world_list_max_rows()` caps the
list at what the reference canvas can show — 10 — so a player with more worlds than
that cannot reach the rest, and at a canvas shorter than 480 the last few of those
ten are not drawn either. Both are the same missing feature, filed as
[#541](https://github.com/matteopolak/lodestone/issues/541), and both are stated here
rather than discovered.

One behavioural deviation follows from it: **row 0 is selected on open**, where
vanilla starts with `updateButtonStatus(null)` and waits for a click. With no
keyboard selection model, requiring a click would leave a keyboard-only player
unable to play at all; row 0 is the most recently played world, which is both the
likely intent and the state this screen already had when it had one row. Focus
landing on a row also selects it, which is `AbstractSelectionList.nextFocusPath`'s
own `setSelected`.

### The row ids are not the row order

The frame's `rows` run **search field, then the six footer buttons, then the world
rows** (`world_select::SEARCH_FIELD`, `FIRST_BUTTON_ROW`, `FIRST_WORLD_ROW`) — even
though the worlds draw *above* the buttons and Tab visits them *before* the buttons.
Three different orderings, deliberately:

- the **ids** are indices into `frame.rows` and into `FocusSet`, so they must be
  stable; putting the worlds between the search field and the buttons would renumber
  all six buttons whenever the world count changed, and `app.rs`'s hit-test would be
  reading last frame's numbering. That is #391's shape at list scale: every click one
  control off.
- the **tab order** is registration order, which `WorldSelectNav::rebuild` sets to
  header → contents → footer, exactly as `layout.visitWidgets`
  (`SelectWorldScreen.java:76`) walks it.
- the **screen order** is geometry, which `row_rect` answers.

`tab_visits_the_list_between_the_search_field_and_the_footer` is the gate that fails
if the first two are ever collapsed into one.

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
click on one of the disabled buttons does **nothing, including not moving
focus** (which is what would let the next Enter press it). A click on a **world
row** is its own case again: it *selects* and does not launch, because
`WorldListEntry.mouseClicked` (`:571-583`) only joins on a double-click or on a
click inside the 32×32 icon — and because launching on a single click would make Play
Selected World unreachable, since you could never point at a world without opening
it.

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

- `world_select.rs`'s own tests, over a **stated** fixture — three worlds, one of
  which is deliberately corrupt, asserted as a precondition, because the `world`
  species of vacuous test lives in the input data and is unreadable from the test
  source. They cover: the three id bands not overlapping in either direction; every
  widget reachable through the `FocusChildren` seam with a control for an unknown id;
  the exact Tab sequence, and its own control — the *same* walk with an empty list
  must visit only the footer, so the list rows appearing has to be caused by there
  being rows; typing plus Up/Down leaving the field; Escape and Enter-from-Back; a
  click that selects a row without launching, with the control that Play then opens
  *that* row rather than always row 0; a corrupt world's row listed, inactive and
  never a tab stop; the filter matching name **and** folder, keeping the selected
  *world* rather than its row index across a filter change, and greying Play when it
  matches nothing; and hover never moving focus or selection.
- `saves.rs`'s own tests: enumeration over a root that really contains a tie on
  last-played, an empty `LevelName`, an undecodable `level.dat`, a directory with no
  `level.dat` and a stray *file*; the `FileUtil` name rules including the ` (N)`
  counter and a 400-character non-ASCII name that byte-slicing would panic on; and
  `creating_a_world_writes_the_typed_name_and_a_fresh_directory`, which asserts the
  second Create makes a **different** directory — the defect the save list exists to
  fix, asserted directly rather than inferred.
- `nav.rs`'s `creating_two_worlds_lists_both_and_play_opens_the_selected_one` is the
  owner's report as an assertion, driven through the real screen flow (title → list →
  create → list → Play), so it fails if any hop is unwired rather than only if
  `saves.rs` is wrong.
- `render.rs`'s `every_world_in_the_list_draws_inside_its_own_row_band` measures
  **each** row's own content rect. The single-row gate it replaces could not tell
  "the list drew" from "row 0 drew", which is exactly the failure a hardcoded row
  has. It asserts by *location* that ink starts past the 32 px icon column, that the
  selection outline is present on row 0 and absent on row 1, and — as controls — that
  the band after the last world is empty and that every band it measures is empty on
  the title screen.
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
