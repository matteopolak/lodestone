# The multiplayer server list

## What it is

`Screen::ServerList` — vanilla's `JoinMultiplayerScreen` plus its
`ServerSelectionList` — at vanilla's geometry: a `HeaderAndFooterLayout` title,
36 px list rows with a 32×32 favicon, wrapped MOTD and a status column, and seven
footer buttons three of which are inactive when there is nothing selected.

This is issue **#396**, the fourth child of the menu-framework epic #392, and it
is a **fidelity pass**: the model (`menu/servers.rs`) and the pinger
(`menu/status.rs`) already existed and are unchanged in substance. What changed is
the presentation, plus the mouse actions vanilla puts on the favicon.

| file | what it owns |
|---|---|
| `menu/servers.rs` | `ServerEntry`/`ServerList`, the on-disk JSON, and `swap` |
| `menu/status.rs` | the probe, the cache, and **which sprite a state resolves to** |
| `menu/render.rs` | the layout (`server_list_layout` …), the frame (`server_list_frame`) and the draw (`draw_server_entry`) |
| `menu/nav.rs` | `ServerListButton`, the two cursors, and what a click on a quadrant does |
| `menu/widget.rs` | `over_right_half` / `over_top_left_quarter` / `over_bottom_left_quarter` |

## How it works

### The screen is a real `HeaderAndFooterLayout`

`server_list_layout(width, height)` builds `HeaderAndFooterLayout(this, 33, 60)`
with the title in the header, a `SpacerElement` sized to `getContentHeight()` in
the contents, and `LinearLayout.vertical().spacing(4)` in the footer holding two
`horizontal().spacing(4)` rows — 3 × 100 px then 4 × 74 px. It is arranged once at
a reference canvas (`SERVER_LIST_REF_CANVAS`), and each footer button's rect is
then expressed as a `Slot` from `Origin::ScreenBottom`.

That is sound only because the arrangement is canvas-independent once expressed
that way, and it is asserted rather than assumed
(`the_server_list_slots_do_not_depend_on_the_reference_canvas`):

- both footer rows measure **308** (`3*100 + 2*4` and `4*74 + 3*4`), so the column
  is 308 at any width and neither row is offset inside it;
- the content band always begins at the **header height**, because the list is
  sized to `getContentHeight()` exactly, which makes
  `min(headerHeight + 30, height - footerHeight - contentHeight)` always pick the
  second term.

### The rows are not `Slot`s, and that is deliberate

`getRowLeft()` is `getX() + this.width / 2 - getRowWidth() / 2` — **two separate
integer divisions** (`AbstractSelectionList.java:372-374`). At an odd canvas width
that is not `(width - 305) / 2`, and it is not `anchor + dx` for any anchor
either, so a `Slot` cannot express it. `row_rect` therefore answers a row carrying
a `MenuRow::entry` from `server_row_rect(view.index, width)` instead, before it
looks at `slot`.

Answering it *inside `row_rect`* is the point: that function is also `app.rs`'s
hit-test, so the draw and the click cannot disagree about where a row is.

The row geometry, all from the jar:

```
row       = (floor(w/2) - 152, 33 + 2 + index*36, 305, 36)
content   = row inset by CONTENT_PADDING (2) a side  ->  (…, …, 301, 32)
favicon   = content origin, 32x32
name      = contentX + 32 + 3, contentY + 1                       (white)
motd      = same x, contentY + 12 + 9*line, at most 2 lines       (-8355712)
motd wrap = contentWidth - 32 - 2 = 267
statusIcon= contentRight - 10 - 5, contentY, 10x8   (not centred vertically)
statusText= right-aligned at statusIconX - width - 5, contentY + 1
```

### Which sprite a row shows

`status.rs` owns the whole mapping, and nothing else picks a sprite:

| state | sprite | how it is reached |
|---|---|---|
| `Initial` | `ping_1` | never probed (`StatusSlot::Idle`) |
| `Pinging` | `pinging_1..5`, animated | probe in flight |
| `Successful` | `ping_5..1` by latency | answered, protocol matches ours |
| `Incompatible` | `incompatible` | answered, protocol differs |
| `Unreachable` | `unreachable` | probe failed |

- The latency buckets are `< 150 / < 300 / < 600 / < 1000 / else` and run
  **downward** — five bars for a fast server.
- The pinging animation is `(millis / 100 + index * 2) & 7` folded with
  `if idx > 4 { idx = 8 - idx }`, so it **ping-pongs** rather than sawtooths, and
  the `index * 2` puts adjacent rows out of phase. Its clock is
  `StatusCache::millis`, which is where it lives so that `frame_for` — which
  already takes a `&StatusCache` — needs no new parameter and `app.rs` needs no
  change.
- `Incompatible` is decided by `ServerStatus::protocol != Some(STATUS_PROTOCOL)`,
  and an **absent** protocol counts as a mismatch. That is vanilla: its
  `serverData.protocol` is a primitive `int` that is still `0` when the status
  omitted one.

The MOTD column carries the *state's* text, not only the MOTD, because vanilla's
pinger overwrites `serverData.motd` per state: `Pinging...` while in flight, and
the red failure reason when a probe fails (with the status column blanked). The
one state that is ours is `Idle`, which shows the address — vanilla has no text
for it because it never lasts longer than a frame.

### Two cursors on one screen

`MenuNav::server` is the list **selection** (a 1 px row outline plus a black
interior, `AbstractSelectionList.extractItem`). `MenuNav::list_button` is which
footer button the **mouse** is over, and it reaches the draw as
`MenuFrame::selected` so `draw_widget` picks `widget/button_highlighted`. They are
separate because both are visible at once, and because they are drawn completely
differently.

**Hover never moves the selection.** Only a click does (and the keyboard, via
`MenuKey`). Vanilla reaches `AbstractSelectionList.setSelected` from `setFocused`
(`AbstractSelectionList.java:298-311`) and from the click paths, and from nowhere
else — so `MenuNav::hover_list` deliberately does nothing at all for a server row
except clear `list_button`.

This was wrong at first: `hover_list` set `MenuNav::server`, on the reasoning that
the mouse and keyboard should drive one cursor rather than two. A player reported it
immediately — the row outline followed the mouse, so a server could not stay
selected while the cursor travelled down to the Join button. That reasoning *is*
right for a screen of buttons, which is why `MenuNav::hover` still moves the row
cursor on the title, pause and settings screens; it is wrong for a selection list,
where the selection is a persistent choice rather than a highlight.
`hovering_a_server_row_does_not_move_the_selection` is the gate, with clicks as its
control so that "hover is inert" cannot pass on a screen where nothing works.

A hovered row's icon overlay is a third thing again, and it depends on the mouse
*position* rather than any row index — see below. That is also why nothing has to be
recorded for a hovered row: both hover visuals (the icon scrim and the quadrant
sprite) are derived in `render.rs` from `MenuFrame::cursor` bounds-tested against the
row rect being drawn, so a `hovered` row index here would have had no consumer. Note
`WorldSelectNav::hovered` *does* exist, because on that screen a hovered row must not
pull focus out of the search field.

### The favicon's quadrants

Vanilla puts three actions on the 32 px icon of a hovered row
(`ServerSelectionList.java:364-395,490-515`): the right half joins, the top-left
moves the row up (`index > 0`), the bottom-left moves it down (not the last row).
All three sprites blit into the **same** rect, with the highlighted variant for
whichever quadrant holds the cursor.

That needs a mouse position, and every other screen here resolves the mouse to a
row index long before a frame exists. So:

1. `app.rs`'s `menu_row_at` already converts physical pixels to the logical canvas.
   It now also calls `MenuNav::set_menu_cursor(x, y, w, h)` there — **before** the
   hit-test, so a cursor over the backdrop still updates it, and covering hover and
   click at once with no change at either call site.
2. `frame_for` copies the position onto `MenuFrame::cursor`, and
   `draw_server_entry` does the quadrant test against the rect it is about to draw
   into.
3. `MenuNav::click` reads the same position back through
   `render::server_entry_icon_rect`, so the quadrant that *acts* is the quadrant
   that was *highlighted*.

The predicates themselves live in `menu/widget.rs` (vanilla puts them on
`SelectableEntry`, the components layer) for exactly that reason: two copies of
`rel_x >= size / 2` is how a highlight ends up one quadrant from what a click
does, and no gate on the highlight alone can see it.

### The footer buttons

`ServerListButton` is the seven, in the order vanilla adds them, which is also the
order `server_list_footer_slot` reads out of the arranged layout and the order the
rows appear in after the entries. `ServerListButton::enabled(has_selection)` is
`onSelectedChange`, with two deviations that are named in its own doc comment:

- **vanilla's selection starts null** even with a non-empty list, so its three
  conditional buttons are inactive until the player clicks or arrows onto a row.
  This shell's keyboard cursor always points at a row when there is one, so
  `has_selection` is `!list.is_empty()` and the disabled path is reached by an
  *empty* list;
- **Edit/Delete are also inactive for a LAN row** in vanilla. There is no LAN
  discovery here, so every row is the equivalent of an `OnlineServerEntry` and the
  two conditions collapse. If LAN rows land, that is the function to split.

**Direct Connection is present and inactive**, because it opens a second address
form this shell does not have. That is the epic's rule — a vanilla control that
cannot be honoured is greyed out in its right place, not omitted.

### Refresh is its own verb

`MenuAction::RefreshList`, not `Reprobe(None)`. `StatusCache::refresh` *skips* any
address it already has a result for, which is right for "make sure everything has
been probed" and would make a Refresh button do nothing at all.
`StatusCache::refresh_all` discards the cached results first — which is what
vanilla does by throwing the whole screen away and building a new one
(`JoinMultiplayerScreen.java:167-169`).

F5 is `MenuKey::Refresh`, a variant of its own rather than a reuse of
`Char('r')`: on the edit form a `Char` is text, so mapping a function key onto one
would type an `r` into the address field. It maps to `focus::KEY_F5` (294) at the
`KeyEvent` boundary, so a focused `EditBox` is offered it first and declines it,
exactly as `Screen.keyPressed`'s ordering requires.

## How to change it

- **The expected rects live in the test, not in the code.**
  `the_server_list_rects_are_vanillas_own` holds the hand-derivation from the Java
  at 854×480; `server_list_footer_slot` computes the same numbers from an arranged
  layout. Two independent derivations of one arithmetic is the only shape of gate
  that catches a port that is self-consistently wrong, so do not "simplify" either
  side into the other.
- **`ServerListButton::width()` is not what the draw uses** — the arranged
  layout's width is. The method exists so a test can assert the two agree, which
  is what would catch a footer built with its two rows swapped.
- **Do not restate the icon rect.** `render::server_entry_icon_rect` is public for
  the click path; add callers rather than arithmetic.
- **A new row state means a new arm in `StatusSlot::state` *and* in
  `status_sprite`**, and `each_state_resolves_to_its_own_sprite` will fail until
  the sprite is distinct from the other four.
- **The status text and the MOTD are different columns with different rules.**
  A failure goes in the MOTD (red, `-65536`); an incompatible version goes in the
  status column (`ChatFormatting.RED`, `0xFF5555`). Vanilla's own split.

## Deliberately not done

Each of these is a scope cut, not an oversight:

- **No scrolling and no scrollbar.** `server_row_visible` reproduces
  `extractListItems`' visibility test, which stands in for vanilla's scissor: a row
  that would overflow into the footer is skipped rather than half-drawn. There is
  no scroll offset, so on a short canvas a long list is truncated. `row_rect` still
  reports a rect for a skipped row, so a click there selects it and nothing else.
  `AbstractSelectionList`'s scissored viewport, scrollbar and mouse-wheel handling
  are shared with `Screen::WorldSelect` and want doing once.
- **No LAN discovery**, so no `LANHeader` row and no `NetworkServerEntry`. That is
  also why Edit/Delete need no entry-type test.
- **No double-click to join.** `app.rs` reports one click at a time with no
  interval. Joining is one click away on the icon's right half, and one keypress
  away with Enter or Join Server.
- **No tooltips.** The status icon and the player-count text both carry one in
  vanilla. Nothing in this shell tracks hover *dwell time* — the same gap
  `menu-widgets.md` and `menu-focus.md` record — so a tooltip would reach zero
  pixels.
- **No delete confirmation.** Vanilla opens a `ConfirmScreen`; Delete here removes
  immediately, as it always has.
- **No `Shift`+Up/Down reorder.** Vanilla's `OnlineServerEntry.keyPressed` has it,
  but `MenuKey` carries no modifiers. The mouse path is implemented.
- **The favicon is still a 16×16 mosaic of quads**, not a sampled texture — a
  per-server runtime texture needs its own upload path and bind group. The
  *fallback* icon is a real texture (`misc/unknown_server`, through
  `resources::MENU_TEXTURES`), because that one is a pack asset.
- **The list background is the screen's flat fill.** Vanilla tiles
  `menu_list_background.png` across the band and draws no per-row fill for an
  unselected row, so an unselected row correctly paints only its content; the band
  texture itself is one of the 89 loose `textures/gui/` PNGs `resources.rs`
  documents as invisible to `GuiAtlas`'s glob.
- **The MOTD wrap is greedy on whitespace**, where `Font.split` also breaks inside
  an over-long word. It takes a ~44-character unbroken run in a 267 px column to
  notice.

## How it is proved

- **Row and footer geometry** against the hand-derived table above, plus
  `server_row_left(856)` versus the naive centring to pin the two-integer-divisions
  reading.
- **Canvas independence** by re-arranging at three canvases and requiring
  identical slots — *and* requiring each slot to resolve onto that canvas' own
  arrangement, which is what makes the two derivations independent rather than
  merely equal. Even widths only: `Origin::ScreenBottom`'s x is `width * 0.5`
  unrounded while `FrameLayout` truncates, so an odd logical width differs by half
  a pixel (the same limit `Screen::WorldSelect`'s footer has).
- **Status sprite by identity**, through `GuiAtlas` UV regions at the status
  icon's own rect, with a control asserting the detector can tell `ping_3` from
  `ping_5`. Four states are asserted to resolve to four *distinct* sprites.
- **The hover overlay by position**: the icon-dim quad's bounding box must be at
  the hovered row's icon rect and must **move** when the cursor moves to another
  row, with no-cursor and cursor-on-backdrop as the controls that prove the
  detector fires.
- **The highlighted quadrant by atlas region**, because all three sprites share one
  destination rect: some UV inside `join_highlighted` and none inside `join`, and
  the other two arrows simultaneously plain. Row 0's move-up arrow must be absent
  entirely, with its move-down arrow as the control.
- **The disabled footer per button**, by the joint test of destination rect *and*
  sampled region, with the expected sprite produced by `WidgetSprites::get` and
  never spelled out. The control is executed: adding a server must flip all three.
- **The row indices `click` assumes are the ones the frame builds**
  (`the_server_list_rows_are_in_the_order_click_assumes`) — the same guard shape as
  the settings screen's, protecting against the same #391 bug.
- **The quadrant predicates** are proved to partition the icon exactly: every one
  of the 1024 points inside it belongs to exactly one of the three, and no point
  outside belongs to any.

## Configuration

None of its own. `gui_scale` (`config.rs`) sets the logical canvas through
`render::logical_canvas`; `LODESTONE_DATA_DIR` moves `servers.json` (see
`menu/servers.rs`). `status::STATUS_PROTOCOL` is the protocol advertised in the
status handshake and therefore the one an incompatible row is compared against.

## Dependencies

- `lodestone-net` — `server_status`, which decodes the MOTD, player counts,
  favicon, protocol and round-trip.
- `lodestone-render` — `GuiAtlas` for the `server_list/*` sprites; the fallback
  favicon arrives through `resources::MENU_TEXTURES`' extras.
- `menu/{widget,layout,focus}.rs` — #393/#394/#395: the button contract and the
  disabled path, `HeaderAndFooterLayout`/`LinearLayout`, and the `KeyEvent`
  boundary F5 crosses.
- The 26.2 jar at `.cache/mc/26.2/client-src` — behavioural reference only, never
  transliterated.

## See also

- [Menu UI framework](./ui-framework.md) — the epic's plan of record.
- [Menu widgets](./menu-widgets.md) — the disabled path this screen is the first
  list consumer of.
- [Menu layout containers](./menu-layout.md) — `HeaderAndFooterLayout`, which had
  no screen consumer until this one.
- [Menu focus and `EditBox`](./menu-focus.md) — the edit form this screen opens.
- [Main menu](./main-menu.md) — the screen state machine and the persisted list.
