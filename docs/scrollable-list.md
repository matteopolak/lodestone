# The scrollable list primitive

## What it is

`menu::widget::ScrollList` is the shared substrate for every list-shaped menu
screen: a **pixel** scroll offset, a scrollbar, and a `hovered`/`selected` pair that
are two genuinely separate pieces of state. It is a port of vanilla's
`AbstractScrollArea` + `AbstractSelectionList` scroll model, and it exists because
the multiplayer list and the account list had each grown their own — with different
bugs.

It owns no entries. A screen keeps its rows in whatever shape suits it and tells the
list only how many there are, so adopting it never means restructuring a model.

## How it works

Four fields, each one a vanilla field:

| `ScrollList` | vanilla |
|---|---|
| `scroll` | `AbstractScrollArea.scrollAmount` (`AbstractScrollArea.java:18`) |
| `selected` | `AbstractSelectionList.selected` (`AbstractSelectionList.java:40`) |
| `hovered` | `AbstractSelectionList.hovered` (`:41`) |
| `dragging` | `AbstractScrollArea.scrolling` (`:19`) |

Every method names the line it ports. The load-bearing ones:

| method | vanilla |
|---|---|
| `content_height` | `Σ heights + 4` (`AbstractSelectionList.java:198-206`) |
| `max_scroll` | `max(0, contentHeight - height)` (`AbstractScrollArea.java:84-86`) |
| `set_scroll` | `Mth.clamp(v, 0, maxScrollAmount())` (`:67-69`) |
| `scroll_rate` | `defaultEntryHeight / 2` (`AbstractSelectionList.java:44` → `:145-147`) |
| `mouse_scrolled` | `scrollAmount - scrollY * scrollRate()` (`AbstractScrollArea.java:34`) |
| `row_top` | `repositionEntries`' running `y` (`AbstractSelectionList.java:143-152`) |
| `row_visible` | `extractListItems`' overlap test (`:346-352`) |
| `scroll_to_entry` | `scrollToEntry` (`:251-261`) |
| `scroller_height` | `clamp((int)(h²/content), 32, h-8)` (`AbstractScrollArea.java:96-98`) |
| `scrollbar_y` | `scrollBarY()` (`:104-108`) |
| `scrollbar_x` | `getRowRight() + scrollbarWidth() + 2` (`AbstractSelectionList.java:289-291`) |
| `drag_to` | `mouseDragged` (`AbstractScrollArea.java:38-56`) |

### The offset is pixels, and that was the bug

Both of this shell's lists stored a **row index** — `MenuNav::server_scroll: usize`
and `accounts::State::scroll: usize` — so one wheel notch jumped a whole 36 px
entry. Vanilla's is a `double` in pixels and one notch is `defaultEntryHeight / 2`,
which for a 36 px server row is exactly **18 px**.

That is not a smoothing problem to be eased over. A row counter structurally cannot
hold 18, and no amount of animation on top of it could produce the intermediate
state, because the information is not there. The fix is the representation.

`a_row_index_implementation_fails_the_notch_assertion` keeps the old model
**executable** as the negative control, so the predicted-value assertion is a
control and not a description of one. Asserting merely that "the offset changed" is
satisfied identically by both implementations — the documented *magnitude* species
of vacuous test.

### There is no scroll animation in 26.2

Checked, not assumed, because "smooth" invites one:
`smoothScroll` / `scrollAnimation` / `targetScroll` appear **nowhere** in
`client/gui`, and `setScrollAmount` is an immediate `Mth.clamp` with no target, no
velocity and no per-frame approach. Smoothness in vanilla is entirely a consequence
of the offset being pixel-granular. **Do not add easing.** It would be invention,
and it would desynchronise the draw from the hit-test, which read the same `row_top`.

### Hover is not selection

Two fields, and nothing copies between them. `hovered` is recomputed from the mouse
at the top of every extract (`AbstractSelectionList.java:210`) and is only ever
*read*, as a boolean argument to the entry's draw (`:360`). `selected` moves on a
click, on a keyboard arrow, or through `setFocused` (`:299-311`) — never on a hover.

`set_hovered` therefore has **no code path to `selected`**. That is what makes the
account-screen bug structurally impossible rather than a remembered rule: `hover`
there wrote `highlighted` *and* `focus`, and `highlighted` is what Select and Remove
act on, so moving the mouse silently re-aimed them.

Closing that opens the mirror-image gap, so both halves belong together: a click
*does* select in vanilla. On the accounts screen a click arrives as `hover` + `Enter`
(`menu/nav.rs:1724-1725`), so `Enter` moves `highlighted` too.

### Clipping: why the offset could not be pixels before

`set_scissor_rect` appears **nowhere** in this workspace, and for months every list
doc in `docs/` recorded the same consequence: "this pipeline has no scissor", so a
row that straddled the band's edge would paint over the header or footer and had to
be **skipped whole**. That is why the offset was row-quantized in the first place.

`menu::render`'s `Quads` now clips on the CPU:

| primitive | how |
|---|---|
| `rect` (and `outline`, `mosaic`) | rect intersection in pixel space |
| `sprite` | `dst` **and** UV cropped together, both axes |
| `text` | emitted to a scratch buffer, then cut in NDC |

`Quads::with_clip` is the scoped entry point — vanilla's
`enableScissor`/`disableScissor` (`AbstractSelectionList.java:242-249`, `:212-214`).

A GPU scissor was rejected deliberately: the whole menu draws in **one**
`"menu-pass"` with four `pass.draw` calls over two vertex streams, so a scissor would
need `MenuGeometry` to record range breaks and the pass to replay them in order, and
the ordering between the streams is already load-bearing (labels are on the colour
stream and must land *on* their button sprite). CPU clipping costs nothing at draw
time and — the deciding reason — **also clips text**, which nothing cheaper does:
glyphs bottom out in `ColourStream::rect` in `hud/item_icon.rs` as one flat quad per
horizontal ink run, so they are not addressable as sprites.

The sprite crop generalises the horizontal-only UV crop the XP bar already uses
(`hud.rs:1302-1312`). Cropping one axis only would **squash** a favicon instead of
cutting it, which is worth naming because it still looks like a picture.

## How to change it, and the gotchas

- **Keep every formula a citation.** A convenience with no vanilla counterpart
  belongs in the screen, not here.
- **The integer truncations are load-bearing.** `scroller_height` casts to `int`
  before clamping and `scrollbar_y` does
  `(int)scrollAmount * (height - scrollerHeight()) / maxScrollAmount()` in *integer*
  arithmetic. The `floor`s are that arithmetic, not defensive rounding — dropping
  them moves the thumb by up to a pixel and no test would say why.
- **`scroll_rate` uses integer division.** A 25 px entry gives 12, not 12.5.
- **`Mth.clamp`'s bounds can cross.** On a very short band `scroller_height`'s upper
  bound (`height - 8`) is below its lower one (32), and vanilla resolves that to the
  **upper** bound. `max` then `min`, in that order.
- **`visible_range` inverts `row_visible`'s algebra; do not adjust it by hand.** The
  `ceil(d/row_h) - 1` is not interchangeable with a `floor`: when a row's bottom edge
  lands exactly on the band's top it is visible by the `>=`, and `floor` drops it.
  A first version used `floor` plus a compensating `+1` and over-reported the last
  row by one at rest. `visible_range_agrees_with_row_visible_at_every_offset` sweeps
  the whole span and is what caught it.
- **`resize` every frame, before reading geometry.** It is
  `updateSizeAndPosition` (`AbstractSelectionList.java:186-195`) and it ends in the
  re-clamp; a list holding a stale `height` reports a stale `max_scroll`, which is
  how a shrunk window ends up scrolled past its own content and draws empty.
- **`set_selected`'s `keyboard` flag is not cosmetic.** Vanilla scrolls into view
  when the entry is clipped **or** when the last input was the keyboard (`:58`). A
  click passes `false`, which is what stops a click on a partly-visible row from
  yanking the list.
- **Row heights may be uniform or per-entry** — `ScrollList::new_variable` (and
  `resize_variable`) take the heights, and `row_offset`/`row_height` are what
  every other method consults. This is `AbstractSelectionList`'s own
  `addEntry(entry, height)` (`:122-129`), whose `repositionEntries` advances a
  running `y` by each child's height (`:143-152`). **Uniform is the degenerate
  case of the same arithmetic, not a second implementation**, and
  `an_explicit_equal_height_list_is_indistinguishable_from_a_uniform_one` holds
  it to that: it sweeps both modes across the whole span and compares every
  observable, so a prefix-sum off-by-one cannot hide behind a spot check.
- **`row_h` is `defaultEntryHeight`, and once heights are explicit it is *not*
  "the height of a row".** It is only what `scroll_rate` is defined against
  (`:44`). Deriving the rate from `heights.first()` would make the wheel speed
  depend on which entry happens to be first — a settings list declaring 25 px
  scrolls **12** px per notch even when its first row is a 20 px header.
  `the_scroll_rate_ignores_the_entry_heights_entirely` pins it, and
  `one_notch_on_a_variable_list_is_half_the_declared_default_height` separates 12
  from the two rivals (10 = rate taken from the first entry, 20 = a row-index
  model landing on that row's height).
- **`visible_range` is closed-form only when uniform.** A prefix sum has no
  multiply to invert, so the variable case walks — as vanilla's own
  `repositionEntries` does. Both modes are held to agreeing with `row_visible`
  entry by entry across the whole span.
- **A uniform `resize` on a variable list drops to uniform on purpose.** Keeping
  a stale height table behind a new `len` is worse than a wrong count, because
  `row_offset` would keep answering *plausibly*. `resize_variable` is the call
  that keeps them.
- **A non-finite or negative height contributes 0 rather than propagating.** A
  `NaN` in a running sum poisons the entire tail, which would put every *later*
  row off-screen — one bad row must stay one bad row.
- **Nesting `with_clip` replaces rather than intersects**, matching
  `enableScissor`'s absolute bounds. No list nests one today.

## Configuration

None at runtime. The compile-time constants are vanilla's, in `menu::widget`:

| constant | value | vanilla |
|---|---|---|
| `SCROLLBAR_WIDTH` | 6 | `AbstractScrollArea.java:13` |
| `SCROLLBAR_MIN_HEIGHT` | 32 | `:14` |
| `SCROLLBAR_HEIGHT_INSET` | 8 | `:97` |
| `LIST_CONTENT_PADDING` | 2 | `AbstractSelectionList.java:435` |
| `SCROLLER_SPRITE` | `widget/scroller` | `AbstractScrollArea.java:15` |
| `SCROLLER_BACKGROUND_SPRITE` | `widget/scroller_background` | `:16` |

The jar-less scrollbar fallback is **not** a citation. 26.2 draws those two sprites
and nothing else, so there is no vanilla colour for a run with no atlas; the fallback
reuses this shell's own `ROW_OFF`/`LABEL` palette. Do not relabel those as "vanilla's
scrollbar colours".

## Current state, and what is left

| report | state |
|---|---|
| scrollbar on the server list | **done** — `render::draw_scrollbar` |
| hover must not focus (accounts) | **done** — `accounts::AccountsNav::hover` |
| both screens share one primitive | **done** — `widget::ListSpec`, below |
| pixel-granular ("smooth") scrolling | **done on the server list and the accounts screen**; the other five still hold a `first: usize` |

### The generic hook: `widget::ListSpec`

`ScrollList` was already shared *arithmetic*. What was not shared was the two ends
that reach a player: the scrollbar draw called `server_scroll_list` **by name**
(`render/draw.rs`), and `app`'s wheel arm was gated on `Screen::ServerList`. So a
screen could adopt `ScrollList`, be correct, be green, and show nothing.

`ListSpec` is a screen's **canvas-independent declaration** of its list — `row_h`,
`top`, `footer_h`, `len`, optional per-entry `heights`, `scroll`, and a `band`
(`RowBand::Centred` or `RowBand::Inset`, see below) — and
`ListSpec::model(canvas_height)` turns it into the live `ScrollList`. There is now
exactly one declaration point and two readers:

| | |
|---|---|
| declares | `MenuNav::active_list(&UiState)` — one arm per screen, each delegating to that screen's own `*_list_spec` |
| stamped onto the frame by | `render::frame_for`'s tail, beside `gui_scale`, so a screen cannot forget to tell the draw |
| read for pixels by | `render/draw.rs` → `draw_scrollbar`, plus `Quads::with_clip` for the rows |
| read for input by | `MenuNav::scroll_active_list`, called from **one** `MouseWheel` arm in `app/lifecycle.rs` |

Each screen's spec derives its band and pitch from the same constants its draw uses:
`render::server_list_spec` and `render::accounts_list_spec`. `server_scroll_model` is
now a thin wrapper over `server_list_spec` rather than the reverse, so the band is
stated once.

**To give a screen a scrollbar and a wheel, add an `active_list` arm.** The
prerequisite is that its offset is stored in **pixels**: a `first: usize` reports an
offset that is always a multiple of the row height, which is exactly the snap-to-row
stepping this work removed. Convert the field first — that is the work, not the arm.

### The accounts screen, converted

`accounts::State::scroll` is an `f32` pixel offset. Three changes went together,
because none of them is observable alone:

- `accounts_row_top(index, scroll)` takes a pixel offset and `index` is the row's
  position in the **full** list. It was the rendered-window position, which is only
  expressible at whole-row offsets.
- `accounts_idle_frame` emits **every** logical row instead of
  `rows[scroll..scroll + VISIBLE_ROWS]`, and `measure::row_rect` gates on
  `accounts_row_visible` so a click cannot land on a row that scrolled away.
- `accounts_row_visible` is a **partial**-overlap test now, and `draw_account_entry`
  is wrapped in `Quads::with_clip`. At a pixel offset a straddling row is the normal
  case; rejecting it would drop a row at every intermediate position, which is worse
  than the stepping it replaced.

`VISIBLE_ROWS`'s hardcoded 5 is gone, and the number it stood for is now *measured*:
at `MIN_SCALED_HEIGHT` the derived band is `240 - 60 - 33 = 147` px, which
`visible_range` resolves to exactly five 36 px rows. A taller canvas legitimately
shows more, which is the residual gap that constant documented.

`scroll_to_show` delegates to `ScrollList::scroll_to_entry` rather than restating a
clamp, and uses `MIN_SCALED_HEIGHT` for `scroll_server_to_show`'s reason: a keyboard
press has no canvas, and the smallest supported one can only under-use a taller
window, never scroll a row off screen.

The **"Showing 1-5 of 9" counter was removed**. It was this screen's stand-in for a
scrollbar; there is a real one now, and vanilla has no such label on any selection
list. Re-adding it would mean re-deriving a "shown" count that no longer exists —
which rows are visible is the real canvas's answer, not the frame's.

#### Gates, with both controls executed and observed

| gate | asserts |
|---|---|
| `the_accounts_screen_draws_the_same_scrollbar_the_server_list_does` | the thumb's rect carries `LABEL`; the 8 px gutter between rows and bar carries **nothing**; a non-scrolling list draws no bar |
| `one_notch_on_the_accounts_list_is_half_a_row_through_the_generic_router` | **18 px**, separated from 36 (row-index) and 147 (page); 54 after three notches, not a multiple of 36; both clamps |
| `the_wheel_router_declines_a_screen_with_no_list` | `false` on the title screen, `true` on accounts from the same nav |
| `an_account_row_straddling_the_band_is_clipped_not_drawn_over_the_footer` | zero coverage between the band bottom and the button row, with a straddling row proven present |
| `hover_takes_a_logical_row_and_the_offset_is_pixels` | offset 145 — **not** a multiple of 36, so a row index could not express it |

- **De-generalising the hook** (stamping `list` only for `Screen::ServerList`) was
  observed to fail both accounts pixel gates at `frame_for must stamp the accounts
  screen's ListSpec`.
- **Removing the accounts arm from `scroll_active_list`** — the pre-change state, where
  `app/` had two wheel arms and neither was this screen's — was observed to fail the
  notch gate at `the router must report that the accounts list moved`.

One near-miss worth recording, because it is the *false-premise control* species: the
clip gate first measured the whole strip from the band bottom to the canvas bottom and
read **0.352 covered**, which looks exactly like a broken clip. It was the four footer
buttons, which legitimately own that band — 20 px of 60 is 0.333. Before believing a
coverage-is-zero control, ask what else already paints in the rect. The gate now
measures band-bottom to the *button row's* own arranged `y`.

## Adoption audit — the other list screens

Measured, not guessed. **Every list screen in this tree now draws a scrollbar and
responds to the wheel** — the server list, accounts, statistics, key binds,
language, social interactions and every `OptionsList` settings page — through one
`MouseWheel` arm rather than one arm each. No screen holds a `first: usize` entry
index any more, which was the thing stopping a screen reporting a pixel offset to
`active_list`.

Each screen's notch is `floor(row_h / 2)`, predicted from its **own** row height
and then measured. The row height differing per screen is the point, and it is the
mistake each gate is built to catch: a screen that borrowed `options::WIDGET_H`
(20) when its real pitch is 18 reports **10** where it should report **9**, and a
screen that borrowed it when the real pitch is `DEFAULT_ITEM_HEIGHT` (25) reports
10 where it should report **12**. Both are plausible-looking wrong answers, so
every gate names the wrong value as an explicitly excluded hypothesis rather than
asserting only that the offset moved.

| screen | offset | hover vs selection | verdict |
|---|---|---|---|
| `stats.rs` | **`scroll: f32`** | none at all | **ADOPTED** (#445). `ROW_H` 20 → notch **10 px**, predicted and measured; row-index (20) and page (`LIST_WINDOW_PX`) both asserted excluded |
| `key_binds.rs` | **`scroll: f32`** | hover sets the cursor | **ADOPTED** (#445). `ROW_H` 20 → notch **10 px**. Control observed: with `scroll_by`/`scroll_to_cursor` snapped to whole rows, both notch gates fail |
| `language.rs` | **`scroll: f32`** into a *filtered* list | hover sets the cursor | **ADOPTED** (#445). `ROW_H` **18** → notch **9 px**. `active_list` reports the post-filter count, so the search box shortens the bar. 10 is asserted excluded — it is `floor(WIDGET_H / 2)` |
| `social.rs` | **`scroll: f32`** | hover sets the cursor | **ADOPTED** (#445), and the **only user of `RowBand::Inset`**. `ROW_H` 20 → notch **10 px**. `RIGHT_MARGIN` grew 10 → 14 to reserve the bar's gutter |
| `options.rs` | **`scroll: f32`**, **variable** entry heights | hover sets the cursor | **ADOPTED** (#445), the only user of `ListSpec::with_heights`. `DEFAULT_ITEM_HEIGHT` **25** → notch **12 px**, and the gate proves the `heights` table does **not** change it — `scrollRate` is defined against `defaultEntryHeight`, so a page whose entry 0 is a 31 px header still scrolls 12. `Root` reports no list at all |
| `packs.rs` | none | hover sets the cursor | **not applicable** — adopting means *adding* scroll, a feature not a refactor |
| `telemetry.rs` | none | hover sets the cursor | **not applicable** — four fixed controls, no list |
| `world_select.rs` | none | **genuinely separate** (`hovered` + `FocusSet`) | **not a scroll candidate — but it is the hover reference** |

### What converting `options.rs` last established

**The `heights` table must not touch the scroll rate, and that needed its own
assertion.** `ListSpec.row_h` stays `DEFAULT_ITEM_HEIGHT` even when `heights` is
`Some`: it is `defaultEntryHeight`, what `scrollRate` is defined against, and
**not** "the height of the row you are on". The Video page's entry 0 is a header
taller than 25 px, so
`one_wheel_notch_is_half_a_default_entry_and_ignores_the_heights_table` names half
*that* height as a fourth excluded hypothesis alongside 10, 25 and the entry-index
answer — and asserts the premise that entry 0's height really does differ, so the
claim is measured rather than assumed.

**`entry_offset` became absolute, and that fixed a latent inconsistency.** It was
`(first..index).map(entry_height).sum()`, so entry `first` sat at offset 0 and its
own `header_padding_top` was never counted at the top of the window. Summing from
0 is vanilla's own absolute layout, and the scroll is subtracted once in
`list_cell_origin`.

**One gate's *behaviour* had to change rather than its units, and that is the
interesting one.** `the_frame_carries_a_header_label_for_every_visible_header`
asserted that a scrolled-past header is **absent** from `frame.labels` — which was
true only because `settings_frame` emitted the visible window, so absence *was* the
mechanism. Emitting every entry and clipping is the conversion, so the header is
still emitted; what must change is its **position**. Asserting absence would have
been asserting the old implementation. It now asserts the split (`labels` carries
the non-scrolling title, `list_labels` the headers) and that the header's resolved
y moves *above the band's top*, where `render::draw` clips it. The same reasoning
retired the canvas bound in `every_placement_resolves_to_a_rect_on_screen`: a
control below the band now resolves below the canvas on purpose, so the vertical
assertion is bounded by the band and carries an `in_band > 200` premise guard so a
filter that skipped everything cannot pass vacuously. The x bound is unchanged and
still catches the `-1000` sentinel, which is negative in both axes.
| `packs.rs` | none | hover sets the cursor | **not applicable** — adopting means *adding* scroll, a feature not a refactor |
| `telemetry.rs` | none | hover sets the cursor | **not applicable** — four fixed controls, no list |
| `world_select.rs` | none | **genuinely separate** (`hovered` + `FocusSet`) | **not a scroll candidate — but it is the hover reference** |

### Two findings from converting `stats.rs` first (issue #445)

**1. A screen whose rows are free text needed a new primitive, and it is
`MenuFrame::list_labels`.** A pixel-scrolled list routinely has a row half
outside its band, and the only alternative to clipping it is what every
unconverted screen still does — skip any row that does not *wholly* fit, which
is the snap-to-row behaviour this work exists to remove. Rows drawn as `MenuRow`
already got clipping from `render/draw.rs`'s per-row `with_clip`; `stats.rs`'s
rows are `MenuLabel`s (a stat row is not a control — vanilla only narrates it)
and had nowhere to put it. `list_labels` is a second label vector that the draw
clips to the band `frame.list` declares, and is purely additive: `MenuFrame`
derives `Default` and every literal site spreads it.

The clip is **vertical only**, deliberately. A list label is positioned from its
own `Origin`, and on a two-column screen like this one the value column sits on
the far side of the canvas centre; clipping horizontally to `row_w` would crop
it. The band is what must be clipped, and the band is vertical.

**2. `social.rs` was NOT "easy", and the audit above was wrong about it.** It was
the one screen of the five that could not adopt `ListSpec` as the primitive then
stood, and **the primitive was changed first rather than the screen special-cased**.

The blocker, for the record: `ListSpec::row_left` was
`floor(width / 2) - floor(row_w / 2)` — a **centred, fixed-width** row — and
`row_right` (hence the scrollbar's x) derived from it. `social.rs`'s rows are
**full-width and left-anchored**: `name_x` is a flat `NAME_LEFT_INSET` of 4 and
the Hide/Report buttons hang off `width - RIGHT_MARGIN`. No *constant* `row_w`
makes `row_right` land in that right margin at every canvas width.

**That paragraph was prose, and it is now arithmetic.**
`widget::tests::no_constant_row_width_can_express_a_full_width_row` states two
hypotheses and computes both from outside the type: the inset band, and
`Centred { row_w }` for the `row_w` that is *exactly right at 854 px* — the most
favourable constant a tuner could pick, asserted exact at 854 so it is not a straw
man. A centred right edge is `w/2 + row_w/2`, so it moves at **half** the canvas
edge's rate, and the error is predicted as `(854 - w) / 2` rather than merely
asserted non-zero: **107 px at 640, 213 at 1280, 533 at 1920**. Wrong shape, not a
mistuned value.

### `RowBand`, the canvas-relative row edge (issue #445)

`ListSpec`'s `row_w: f32` became `band: RowBand`:

| variant | `row_left` | `row_right` | used by |
|---|---|---|---|
| `Centred { row_w }` | `floor(w/2) - floor(row_w/2)` | `row_left + row_w` | multiplayer 340, accounts 305, key binds 340, language 270, stats 300 |
| `Inset { left, right }` | `left` | `w - right` | **social only** |

`ListSpec::uniform` keeps its exact signature and still builds `Centred`, so the
five centred lists are untouched; `.spanning(left, right)` is the opt-in. `row_w`
became a **method taking the canvas width**, because for `Inset` the row width
genuinely is a function of the canvas — a bare field could only ever have been the
centred answer.

**The gutter rule is the trap `Inset` introduces, and it is gated.**
`AbstractSelectionList` overrides `scrollBarX()` to
`getRowRight() + scrollbarWidth() + 2`, so the bar lives **outside** the row and
nothing clamps it to the canvas: an inset that reserves only the row's own margin
pushes the bar off-screen *silently*, because a rect off the canvas simply does not
draw. The required reservation is `SCROLLBAR_WIDTH + 2 + SCROLLBAR_WIDTH` = **14
px**, and `widget::tests::an_inset_rows_right_gutter_must_reserve_room_for_the_scrollbar`
observes social's existing 10 px **failing** (the bar ended at 858 on an 854
canvas) before pinning 14 as the boundary — exact at 854, and 13 already overflows.
That is why `social::RIGHT_MARGIN` is now `SCROLLBAR_WIDTH + 2 + SCROLLBAR_WIDTH`
rather than a flat 10, and why its Hide/Report buttons moved 4 px left. A centred
list gets that gutter free from the canvas margin either side; a full-width one has
to declare it.

The property no centred `row_w` could have is gated on the screen too:
`social::tests::the_declared_band_tracks_this_screens_own_right_anchored_buttons`
requires `spec.row_right(w) == report_button_x(w) + REPORT_BUTTON_W` at 640, 854,
1280 and 1920, with no tolerance.

Two things fall out and both are decisions, not details:

- ~~**`options.rs` decides whether `ScrollList` grows variable row heights.**~~
  **Decided (#445): it grows them.** `new_variable` landed, so `options.rs`'s
  mixed header/control heights are expressible and the other four screens are the
  degenerate uniform case of the same arithmetic. This was settled *before*
  converting any screen, deliberately — converting the four against a uniform-only
  primitive and then again against a variable one is the one sequencing mistake
  available here.

  ~~**What is still outstanding is the adoption itself**, and the scrollbar draw is
  not generic today.~~ **The hook landed**: `widget::ListSpec` plus
  `MenuNav::active_list` / `scroll_active_list`, and `render/draw.rs` asks the frame
  instead of naming a screen. `server_scroll_list` is deleted. So the sequencing
  warning that used to live here is discharged, and the four conversions now each
  produce real pixels the moment their offset becomes an `f32`.

  ~~The prerequisite that remains is per-screen and is the *field*, not the hook.~~
  **All five conversions have landed.** Each screen positioned its rows as
  `list_top + (row - first) * ROW_H` through an `Origin::*(Placement { row, first })`;
  every one of those placements now carries a `scroll: f32` and subtracts
  `scroll.floor()` once. The uniform shape that repeated five times:

  1. the nav's `first: usize` becomes `scroll: f32` (and `Eq` goes with it — an
     `f32` field cannot derive it, which `options::SettingsNav` had already paid for)
  2. the `Placement`/`Origin` variant carries pixels, and its `checked_sub`
     underflow sentinel disappears — there is nothing left to underflow
  3. the frame producer stops slicing and emits **every** row; `render::draw` clips
  4. free-text row labels move from `labels` to `list_labels`, which is the vector
     that gets the band's clip rect (`MenuRow`s already had one per row)
  5. `scroll_to_cursor`'s hand-rolled `while … { self.first += 1 }` walk becomes
     `ScrollList::scroll_to_entry`, against `MIN_SCALED_HEIGHT` because a keypress
     has no canvas in hand
  6. two `MenuNav` arms — `active_list` and `scroll_active_list` — guarded
     identically, so "the wheel reaches this screen" and "a bar is drawn beside it"
     cannot drift apart

- **`world_select.rs` is the model for the hover half, not the six `self.cursor = row`
  screens.** It is the only screen that already keeps hover and focus apart. If the
  primitive's hover concept is copied from any of the others, a mouse-over will steal
  the keyboard out of a search field.

Net: five thin adoptions, all landed, replacing per-screen window/clamp code with
one declaration each. Both blockers were discharged before any screen was converted
— the variable-height decision (`new_variable`) and the generic hook (`ListSpec`) —
and the third, `social.rs`'s canvas-relative row edge (`RowBand`), was discharged
the same way rather than by special-casing the screen. That ordering was the whole
strategy: **converting a screen against a primitive that is about to change is
converting it twice.**

## Dependencies

`ScrollList` itself needs nothing beyond `core` — pure data and arithmetic. Its
pixels come from `menu::render` (`draw_scrollbar`, `Quads::with_clip`), which resolves
sprite ids through `lodestone_render::GuiAtlas` and draws text through
`crate::hud::VanillaFont`.
